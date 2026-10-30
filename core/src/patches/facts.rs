//! What one archive says: its files, the columns of its tables that name
//! other files, and what each of its models needs.
//!
//! [`Facts`] are what the launcher caches locally for each archive, not what
//! resolves them: a table's needs depend on other tables (the textures of
//! `CreatureDisplayInfo` are looked up in the folder of the model that
//! `CreatureModelData` names, which may come from another archive), so the
//! cache keeps what the archive *says*, and the resolution happens when the
//! archives are put together.
//!
//! The columns are the game's, by fixed index, never found by looking at
//! the values: a small integer is as good a string offset as any (a value of
//! 1 points at the first string). Nothing here trusts the bytes: a table or
//! model that doesn't add up is noted in [`Facts::problems`] and the rest of
//! the archive is read on.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::btree_map::Entry as Slot;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File as DiskFile;
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Error;
use crate::mpq::{Archive, Key, key};

/// The file that names an archive's files, when it has one.
const LISTFILE: &str = "(listfile)";

/// The version of model the game loads, and the only one.
const M2_VERSION: u32 = 264;

/// How many skins, sequences or textures a model may claim before it is
/// taken for garbage.
const MAX_SKINS: u32 = 256;
const MAX_SEQUENCES: u32 = 0x1_0000;
const MAX_TEXTURES: u32 = 4096;
const MAX_LOOKUPS: u32 = 0x1_0000;
const MAX_BATCHES: u32 = 0x1_0000;
const MAX_EMITTERS: u32 = 0x1000;

/// A sequence flag: its animation is inside the model, not in an `.anim`.
/// (`0x10` is the game's own mark for an animation it has loaded.)
const SEQUENCE_INLINE: u32 = 0x20;
const SEQUENCE_LOADED: u32 = 0x10;
/// A sequence flag: it is another sequence's animation.
const SEQUENCE_ALIAS: u32 = 0x40;

/// A file of an archive, as its hash table lists it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    /// Its two name hashes.
    pub key: Key,
    /// Its size once unpacked.
    pub size: u32,
    /// Whether it only marks the file deleted for the archives below.
    pub deleted: bool,
}

/// An archive, as far as its facts are concerned. The real reader implements
/// it; the tests feed it a map of names to bytes.
pub trait Source {
    /// Every file the archive's hash table lists.
    fn entries(&self) -> Vec<Entry>;
    /// The file called `name` (in any case, with either slash), whole.
    fn read(&self, name: &str) -> io::Result<Vec<u8>>;
    /// `len` bytes of the file from `offset`, fewer if it ends first.
    fn read_range(&self, name: &str, offset: u32, len: u32) -> io::Result<Vec<u8>>;
}

/// A real archive as a [`Source`]. The archive reads with `&mut self`, and
/// a source with `&self`, so it sits in a `RefCell`: nothing here reads
/// two files at once, and a nested read is an error, not a panic.
pub struct Opened<R = DiskFile>(RefCell<Archive<R>>);

impl Opened<DiskFile> {
    /// Opens the archive at `path`.
    pub fn open(path: &Path) -> Result<Self, Error> {
        Archive::open(path).map(Self::new)
    }
}

impl<R> Opened<R> {
    /// `archive` as a source.
    pub fn new(archive: Archive<R>) -> Self {
        Self(RefCell::new(archive))
    }

    /// The archive again.
    pub fn into_inner(self) -> Archive<R> {
        self.0.into_inner()
    }
}

impl<R: Read + Seek> Opened<R> {
    /// The names the archive's `(listfile)` gives, if it has one.
    pub fn listfile(&self) -> Result<Option<Vec<String>>, Error> {
        match self.0.try_borrow_mut() {
            Ok(mut archive) => archive.listfile(),
            Err(error) => Err(Error::Io {
                path: PathBuf::new(),
                source: io::Error::other(error),
            }),
        }
    }
}

impl<R: Read + Seek> Source for Opened<R> {
    fn entries(&self) -> Vec<Entry> {
        let Ok(archive) = self.0.try_borrow() else {
            return Vec::new();
        };

        archive
            .entries()
            .map(|entry| Entry {
                key: entry.key,
                size: entry.size,
                deleted: entry.is_delete_marker(),
            })
            .collect()
    }

    fn read(&self, name: &str) -> io::Result<Vec<u8>> {
        let mut archive = self.0.try_borrow_mut().map_err(io::Error::other)?;

        archive
            .read(name)
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }

    fn read_range(&self, name: &str, offset: u32, len: u32) -> io::Result<Vec<u8>> {
        let mut archive = self.0.try_borrow_mut().map_err(io::Error::other)?;
        let from = u64::from(offset);

        archive
            .read_range(name, from..from + u64::from(len))
            .map_err(io::Error::other)?
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
    }
}

/// What a file is for, by its extension.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// The game's tables (`.dbc`).
    Tables,
    /// Models and what goes with them (`.m2`, `.mdx`, `.mdl`, `.skin`,
    /// `.anim`, `.bone`).
    Models,
    /// Pictures (`.blp`).
    Textures,
    /// Sounds and music (`.wav`, `.mp3`, `.ogg`).
    Sounds,
    /// The interface's code and layouts (`.lua`, `.xml`, `.toc`).
    Interface,
    /// Maps (`.adt`, `.wdt`, `.wdl`, `.wmo`).
    World,
    /// Anything else, and what has no name.
    Other,
}

impl Kind {
    /// The kind of the file called `name`.
    pub fn of(name: &str) -> Kind {
        let file = name.rsplit(['\\', '/']).next().unwrap_or(name);
        let Some((_, extension)) = file.rsplit_once('.') else {
            return Kind::Other;
        };
        match extension.to_ascii_lowercase().as_str() {
            "dbc" => Kind::Tables,
            "m2" | "mdx" | "mdl" | "skin" | "anim" | "bone" => Kind::Models,
            "blp" => Kind::Textures,
            "wav" | "mp3" | "ogg" => Kind::Sounds,
            "lua" | "xml" | "toc" => Kind::Interface,
            "adt" | "wdt" | "wdl" | "wmo" => Kind::World,
            _ => Kind::Other,
        }
    }
}

/// A file of an archive, as the cache keeps it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct File {
    /// What it is, as far as its name says (no name, no kind).
    pub kind: Kind,
    /// Whether it only marks a file deleted for the archives below.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
}

/// What a model needs, or why the game won't load it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Model {
    /// The keys of its skins, of the animations it doesn't hold inside and
    /// of its textures with a name, sorted.
    Needs(Vec<Key>),
    /// A model the game won't load (a version other than 264, or not an
    /// `MD20` at all), with what it is.
    Unloadable {
        /// Its magic or its version, as words.
        unloadable: String,
    },
}

/// A row of `CreatureDisplayInfo`.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct CreatureDisplay {
    /// The id in `CreatureModelData` of its model.
    pub model: u32,
    /// Its three texture variations, empty when it has none: names without
    /// folder or extension, in the folder of the model.
    pub textures: [String; 3],
}

/// A row of `ItemDisplayInfo`: names without folder, some without extension.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ItemDisplay {
    /// The two models.
    pub models: [String; 2],
    /// The textures of the two models.
    pub model_textures: [String; 2],
    /// The two icons, in `Interface\Icons`.
    pub icons: [String; 2],
    /// The textures of the eight regions of the body, each with its suffix
    /// (`_U`, `_M`, `_F`) to find.
    pub components: [String; 8],
}

/// The columns of the six tables that name files, by row id. Rows that name
/// nothing are left out, and so are tables the archive doesn't hold.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Tables {
    /// `CreatureModelData`: the path of the model.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub creature_model_data: BTreeMap<u32, String>,
    /// `CreatureDisplayInfo`: the model and the textures.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub creature_display_info: BTreeMap<u32, CreatureDisplay>,
    /// `CreatureDisplayInfoExtra`: the name of the baked texture, in
    /// `Textures\BakedNpcTextures`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub creature_display_info_extra: BTreeMap<u32, String>,
    /// `ItemDisplayInfo`: models, textures, icons and components.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub item_display_info: BTreeMap<u32, ItemDisplay>,
    /// `GameObjectDisplayInfo`: the path of the model.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub game_object_display_info: BTreeMap<u32, String>,
    /// `SpellVisualEffectName`: the path of the model.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub spell_visual_effect_name: BTreeMap<u32, String>,
}

impl CreatureDisplay {
    /// The fingerprint of the row: its model and textures.
    fn print(&self) -> Key {
        let model = self.model.to_string();

        print(std::iter::once(model.as_str()).chain(self.textures.iter().map(String::as_str)))
    }
}

impl ItemDisplay {
    /// The fingerprint of the row: all its names.
    fn print(&self) -> Key {
        print(
            self.models
                .iter()
                .chain(&self.model_textures)
                .chain(&self.icons)
                .chain(&self.components)
                .map(String::as_str),
        )
    }
}

/// The fingerprint of a row: its path columns joined, hashed as a file name
/// is, so case and the direction of slashes don't matter.
fn print<'a>(columns: impl Iterator<Item = &'a str>) -> Key {
    key(&columns.collect::<Vec<_>>().join("\u{1}"))
}

/// One fingerprint per row of the five tables whose rows the graph resolves
/// (all but `CreatureModelData`, which is kept whole), by row id. Blizzard's
/// archives keep these in place of their rows: a custom table that copies a
/// row unchanged only repeats what the base game ships, dead references
/// included, and the graph tells such a row by its fingerprint.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Prints {
    /// `CreatureDisplayInfo`: model id and textures.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub creature_display_info: BTreeMap<u32, Key>,
    /// `CreatureDisplayInfoExtra`: the baked texture.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub creature_display_info_extra: BTreeMap<u32, Key>,
    /// `ItemDisplayInfo`: all its names.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub item_display_info: BTreeMap<u32, Key>,
    /// `GameObjectDisplayInfo`: the path of the model.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub game_object_display_info: BTreeMap<u32, Key>,
    /// `SpellVisualEffectName`: the path of the model.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub spell_visual_effect_name: BTreeMap<u32, Key>,
}

impl Prints {
    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.creature_display_info.is_empty()
            && self.creature_display_info_extra.is_empty()
            && self.item_display_info.is_empty()
            && self.game_object_display_info.is_empty()
            && self.spell_visual_effect_name.is_empty()
    }

    /// Adds the fingerprint of every row of `tables`.
    pub fn add(&mut self, tables: &Tables) {
        let paths = |rows: &BTreeMap<u32, String>| -> BTreeMap<u32, Key> {
            rows.iter().map(|(id, path)| (*id, key(path))).collect()
        };

        self.creature_display_info.extend(
            tables
                .creature_display_info
                .iter()
                .map(|(id, row)| (*id, row.print())),
        );
        self.creature_display_info_extra
            .extend(paths(&tables.creature_display_info_extra));
        self.item_display_info.extend(
            tables
                .item_display_info
                .iter()
                .map(|(id, row)| (*id, row.print())),
        );
        self.game_object_display_info
            .extend(paths(&tables.game_object_display_info));
        self.spell_visual_effect_name
            .extend(paths(&tables.spell_visual_effect_name));
    }
}

impl CreatureDisplay {
    /// Whether `prints` has this row, unchanged, under `id`.
    pub fn is_in(&self, prints: &Prints, id: u32) -> bool {
        prints.creature_display_info.get(&id) == Some(&self.print())
    }
}

impl ItemDisplay {
    /// Whether `prints` has this row, unchanged, under `id`.
    pub fn is_in(&self, prints: &Prints, id: u32) -> bool {
        prints.item_display_info.get(&id) == Some(&self.print())
    }
}

/// Everything one archive says, which the launcher caches locally: what the
/// archive holds, and what its tables and models reference. Nothing of it
/// depends on any other archive.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Facts {
    /// Every file in the archive's hash table, named or not.
    pub files: BTreeMap<Key, File>,
    /// The names of those it could name: from the archive's own listfile,
    /// from names known elsewhere, and from the paths its tables give.
    pub names: BTreeMap<Key, String>,
    /// The columns of its tables that name files.
    #[serde(default)]
    pub tables: Tables,
    /// The fingerprints of the rows it had before the launcher dropped them
    /// (see [`Prints`]); only Blizzard's archives have any.
    #[serde(default, skip_serializing_if = "Prints::is_empty")]
    pub prints: Prints,
    /// Each of its models (`.m2`) with a name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<Key, Model>,
    /// What couldn't be read or didn't add up, in words, naming the file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

impl Facts {
    /// The fingerprints of the rows of its tables: those it kept, and those
    /// of any rows it still holds (an archive that was never stripped).
    pub fn stock(&self) -> Cow<'_, Prints> {
        let mut rows = Prints::default();
        rows.add(&self.tables);

        if rows.is_empty() {
            return Cow::Borrowed(&self.prints);
        }

        let mut all = self.prints.clone();
        all.add(&self.tables);

        Cow::Owned(all)
    }

    /// What `source` says. `known` are names from elsewhere (the other
    /// archives' listfiles) to try on the files `source` lists without
    /// naming, since models can only be read by name.
    pub fn of(source: &impl Source, known: &[String]) -> Facts {
        let mut facts = Facts::default();
        let entries: BTreeMap<Key, Entry> = source
            .entries()
            .into_iter()
            .map(|entry| (entry.key, entry))
            .collect();
        let there = |name: &str| entries.get(&key(name)).is_some_and(|entry| !entry.deleted);

        let mut listed = Vec::new();
        if there(LISTFILE) {
            match source.read(LISTFILE) {
                Ok(bytes) => listed.extend(listfile_names(&bytes)),
                Err(error) => facts
                    .problems
                    .push(format!("{LISTFILE}: can't be read: {error}")),
            }
        }

        facts.tables = Tables::read(source, &there, &mut facts.problems);
        let tables = &facts.tables;
        let models: Vec<String> = tables
            .creature_model_data
            .values()
            .chain(tables.game_object_display_info.values())
            .chain(tables.spell_visual_effect_name.values())
            .map(|path| model_name(path))
            .collect();

        // `known` is borrowed, not copied: it is every other archive's names.
        // Of the names a key has, the first in byte order is the one kept.
        for name in known.iter().chain(&listed).chain(&models) {
            let key = key(name);
            if !entries.contains_key(&key) {
                continue;
            }
            match facts.names.entry(key) {
                Slot::Vacant(vacant) => {
                    vacant.insert(name.clone());
                }
                Slot::Occupied(mut occupied) if name < occupied.get() => {
                    occupied.insert(name.clone());
                }
                Slot::Occupied(_) => {}
            }
        }
        for (key, entry) in &entries {
            let kind = facts
                .names
                .get(key)
                .map_or(Kind::Other, |name| Kind::of(name));
            let deleted = entry.deleted;
            facts.files.insert(*key, File { kind, deleted });
        }

        for (key, name) in &facts.names {
            let model = name
                .len()
                .checked_sub(3)
                .and_then(|from| name.as_bytes().get(from..))
                .is_some_and(|tail| tail.eq_ignore_ascii_case(b".m2"));
            let size = entries
                .get(key)
                .filter(|entry| !entry.deleted)
                .map(|e| e.size);
            let (true, Some(size)) = (model, size) else {
                continue;
            };
            match read_model(source, &there, name, size, &mut facts.problems) {
                Ok(model) => {
                    facts.models.insert(*key, model);
                }
                Err(problem) => facts.problems.push(format!("{name}: {problem}")),
            }
        }
        facts
    }
}

/// `path` as the game loads a model: `.mdx` and `.mdl` mean `.m2`.
pub fn model_name(path: &str) -> String {
    match path.rsplit_once('.') {
        Some((stem, extension))
            if !extension.contains(['\\', '/'])
                && (extension.eq_ignore_ascii_case("mdx")
                    || extension.eq_ignore_ascii_case("mdl")) =>
        {
            format!("{stem}.m2")
        }
        _ => path.to_string(),
    }
}

/// The names in a listfile: one per line, or separated by `;`. Those that
/// aren't UTF-8 are left out, since a name is hashed as the bytes it has.
fn listfile_names(bytes: &[u8]) -> impl Iterator<Item = String> {
    bytes
        .split(|byte| matches!(byte, b'\n' | b'\r' | b';'))
        .filter_map(|line| std::str::from_utf8(line).ok())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

/// A little-endian number from `bytes`, if they reach that far.
fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_le_bytes(bytes.get(at..end)?.try_into().ok()?))
}

/// A little-endian number from `bytes`, if they reach that far.
fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    let end = at.checked_add(2)?;
    Some(u16::from_le_bytes(bytes.get(at..end)?.try_into().ok()?))
}

/// A table of the game, whose records are all 4-byte fields and whose
/// header adds up.
struct Dbc<'a> {
    records: &'a [u8],
    size: usize,
    count: usize,
    strings: &'a [u8],
}

impl<'a> Dbc<'a> {
    /// The table in `bytes`, which should have `fields` fields. `Ok(None)`
    /// for one with byte fields (five tables of the game have them), whose
    /// columns aren't read.
    fn parse(bytes: &'a [u8], fields: usize) -> Result<Option<Dbc<'a>>, String> {
        let header = |at| u32_at(bytes, at).ok_or_else(|| "has no header".to_string());
        if bytes.get(..4) != Some(b"WDBC") {
            return Err("isn't a table".into());
        }
        let (count, have, size, strings) = (header(4)?, header(8)?, header(12)?, header(16)?);
        let total = 20 + u64::from(count) * u64::from(size) + u64::from(strings);
        let len = bytes.len() as u64;
        if total != len {
            return Err(format!("is {len} bytes, its header says {total}"));
        }
        if u64::from(size) != u64::from(have) * 4 {
            return Ok(None);
        }
        if usize::try_from(have).ok() != Some(fields) {
            return Err(format!("has {have} fields, not {fields}"));
        }
        let (count, size) = (to_usize(count)?, to_usize(size)?);
        let end = 20 + count * size;
        match (bytes.get(20..end), bytes.get(end..)) {
            (Some(records), Some(strings)) => Ok(Some(Dbc {
                records,
                size,
                count,
                strings,
            })),
            _ => Err("has no records".into()),
        }
    }

    /// The number in a field of a record.
    fn word(&self, row: usize, column: usize) -> u32 {
        (row * self.size)
            .checked_add(column * 4)
            .and_then(|at| u32_at(self.records, at))
            .unwrap_or(0)
    }

    /// The string a field of a record points at, empty for none. A pointer
    /// outside the strings counts in `bad` and reads as empty.
    fn text(&self, row: usize, column: usize, bad: &mut usize) -> String {
        let at = self.word(row, column);
        if at == 0 {
            return String::new();
        }
        let from = usize::try_from(at)
            .ok()
            .and_then(|at| self.strings.get(at..));
        let Some(from) = from else {
            *bad += 1;
            return String::new();
        };
        let text = from.split(|&byte| byte == 0).next().unwrap_or(from);
        String::from_utf8_lossy(text).into_owned()
    }
}

fn to_usize(number: u32) -> Result<usize, String> {
    usize::try_from(number).map_err(|_| "is too large".to_string())
}

/// What `row` makes of a record, by the record's index, with the id (the
/// first field) it is kept under.
type Row<T> = fn(&Dbc<'_>, usize, &mut usize) -> Option<T>;

/// The rows of the table `file` that `row` keeps, by id. A table that can't
/// be read or doesn't add up is a problem and gives nothing.
fn rows<T>(
    source: &impl Source,
    there: &impl Fn(&str) -> bool,
    file: &str,
    fields: usize,
    problems: &mut Vec<String>,
    row: Row<T>,
) -> BTreeMap<u32, T> {
    let mut kept = BTreeMap::new();
    if !there(file) {
        return kept;
    }
    let bytes = match source.read(file) {
        Ok(bytes) => bytes,
        Err(error) => {
            problems.push(format!("{file}: can't be read: {error}"));
            return kept;
        }
    };
    let dbc = match Dbc::parse(&bytes, fields) {
        Ok(Some(dbc)) => dbc,
        Ok(None) => return kept,
        Err(problem) => {
            problems.push(format!("{file} {problem}"));
            return kept;
        }
    };
    let mut bad = 0;
    for index in 0..dbc.count {
        if let Some(value) = row(&dbc, index, &mut bad) {
            kept.insert(dbc.word(index, 0), value);
        }
    }
    if bad > 0 {
        problems.push(format!("{file}: {bad} names point outside its strings"));
    }
    kept
}

/// A string column, kept if it isn't empty.
fn named(dbc: &Dbc, row: usize, column: usize, bad: &mut usize) -> Option<String> {
    Some(dbc.text(row, column, bad)).filter(|text| !text.is_empty())
}

impl Tables {
    fn read(
        source: &impl Source,
        there: &impl Fn(&str) -> bool,
        problems: &mut Vec<String>,
    ) -> Tables {
        Tables {
            creature_model_data: rows(
                source,
                there,
                "DBFilesClient\\CreatureModelData.dbc",
                28,
                problems,
                |dbc, row, bad| named(dbc, row, 2, bad),
            ),
            creature_display_info: rows(
                source,
                there,
                "DBFilesClient\\CreatureDisplayInfo.dbc",
                16,
                problems,
                |dbc, row, bad| {
                    Some(CreatureDisplay {
                        model: dbc.word(row, 1),
                        textures: [6, 7, 8].map(|column| dbc.text(row, column, bad)),
                    })
                },
            ),
            creature_display_info_extra: rows(
                source,
                there,
                "DBFilesClient\\CreatureDisplayInfoExtra.dbc",
                21,
                problems,
                |dbc, row, bad| named(dbc, row, 20, bad),
            ),
            item_display_info: rows(
                source,
                there,
                "DBFilesClient\\ItemDisplayInfo.dbc",
                25,
                problems,
                |dbc, row, bad| {
                    let item = ItemDisplay {
                        models: [1, 2].map(|column| dbc.text(row, column, bad)),
                        model_textures: [3, 4].map(|column| dbc.text(row, column, bad)),
                        icons: [5, 6].map(|column| dbc.text(row, column, bad)),
                        components: [15, 16, 17, 18, 19, 20, 21, 22]
                            .map(|column| dbc.text(row, column, bad)),
                    };
                    let names = [
                        &item.models[..],
                        &item.model_textures,
                        &item.icons,
                        &item.components,
                    ];
                    let empty = names.iter().all(|group| group.iter().all(String::is_empty));
                    (!empty).then_some(item)
                },
            ),
            game_object_display_info: rows(
                source,
                there,
                "DBFilesClient\\GameObjectDisplayInfo.dbc",
                19,
                problems,
                |dbc, row, bad| named(dbc, row, 1, bad),
            ),
            spell_visual_effect_name: rows(
                source,
                there,
                "DBFilesClient\\SpellVisualEffectName.dbc",
                7,
                problems,
                |dbc, row, bad| named(dbc, row, 2, bad),
            ),
        }
    }
}

/// `len` bytes of `name` from `offset`, as words when they can't be read.
fn ask(source: &impl Source, name: &str, offset: u32, len: u32) -> Result<Vec<u8>, String> {
    source
        .read_range(name, offset, len)
        .map_err(|error| format!("can't be read: {error}"))
}

/// Which textures of the model `name` something draws: those a batch of one
/// of its skins in the archive picks through the texture lookup, and those
/// its particle emitters and ribbons use. `None` when none of its skins is
/// in the archive (they are all kept then).
fn drawn(
    source: &impl Source,
    there: &impl Fn(&str) -> bool,
    name: &str,
    skins: u32,
) -> Result<Option<BTreeSet<usize>>, String> {
    const CUT: &str = "its header is cut short";
    let stem = name.get(..name.len().saturating_sub(3)).unwrap_or(name);
    let skins: Vec<String> = (0..skins)
        .map(|skin| format!("{stem}{skin:02}.skin"))
        .filter(|skin| there(skin))
        .collect();
    if skins.is_empty() {
        return Ok(None);
    }

    let head = ask(source, name, 0x80, 0xB0)?;
    let word = |at: usize| u32_at(&head, at - 0x80).ok_or(CUT);
    let array = |at: usize, each: u32, most: u32, what: &str| -> Result<Vec<u8>, String> {
        let (count, from) = (word(at)?, word(at + 4)?);
        let end = u64::from(count) * u64::from(each) + u64::from(from);
        if count > most || end > u64::from(u32::MAX) {
            return Err(format!("claims {count} {what} that don't fit in it"));
        }
        let bytes = ask(source, name, from, count * each)?;
        if bytes.len() as u64 != u64::from(count) * u64::from(each) {
            return Err(format!("claims {count} {what} that don't fit in it"));
        }
        Ok(bytes)
    };

    let lookup = array(0x80, 2, MAX_LOOKUPS, "texture lookups")?;
    let mut used = BTreeSet::new();
    for skin in &skins {
        let head = ask(source, skin, 0, 0x2C)?;
        if head.get(..4) != Some(b"SKIN") {
            return Err(format!("{skin} isn't a skin"));
        }
        let (count, from) = (
            u32_at(&head, 0x24).ok_or("a skin's header is cut short")?,
            u32_at(&head, 0x28).ok_or("a skin's header is cut short")?,
        );
        if count > MAX_BATCHES {
            return Err(format!("{skin} claims {count} batches"));
        }
        let batches = ask(source, skin, from, count * 24)?;
        if batches.len() as u64 != u64::from(count) * 24 {
            return Err(format!(
                "{skin} claims {count} batches that don't fit in it"
            ));
        }
        for batch in batches.as_chunks::<24>().0 {
            let (textures, first) = (u16_at(batch, 0x0E), u16_at(batch, 0x10));
            let (Some(textures), Some(first)) = (textures, first) else {
                continue;
            };
            for at in usize::from(first)..usize::from(first) + usize::from(textures) {
                if let Some(texture) = u16_at(&lookup, at * 2) {
                    used.insert(usize::from(texture));
                }
            }
        }
    }

    let emitters = array(0x128, 0x1DC, MAX_EMITTERS, "particle emitters")?;
    for emitter in emitters.as_chunks::<0x1DC>().0 {
        used.extend(u16_at(emitter, 0x16).map(usize::from));
    }

    let ribbons = array(0x120, 0xB0, MAX_EMITTERS, "ribbons")?;
    for ribbon in ribbons.as_chunks::<0xB0>().0 {
        let (count, from) = (
            u32_at(ribbon, 0x14).ok_or(CUT)?,
            u32_at(ribbon, 0x18).ok_or(CUT)?,
        );
        if count > MAX_TEXTURES {
            return Err(format!("claims {count} ribbon textures"));
        }
        let list = ask(source, name, from, count * 2)?;
        if list.len() as u64 != u64::from(count) * 2 {
            return Err(format!(
                "claims {count} ribbon textures that don't fit in it"
            ));
        }
        used.extend(
            list.as_chunks::<2>()
                .0
                .iter()
                .map(|pair| usize::from(u16::from_le_bytes(*pair))),
        );
    }

    Ok(Some(used))
}

/// What the model `name`, of `size` bytes, needs. A model that can still be
/// loaded but whose textures can't be told apart (a bad offset in its header
/// or skins) keeps them all, and `problems` says why.
fn read_model(
    source: &impl Source,
    there: &impl Fn(&str) -> bool,
    name: &str,
    size: u32,
    problems: &mut Vec<String>,
) -> Result<Model, String> {
    const CUT: &str = "its header is cut short";
    let stem = name.get(..name.len().saturating_sub(3)).unwrap_or(name);
    let head = ask(source, name, 0, 0x58)?;
    let magic = head.get(..4).ok_or(CUT)?;
    if magic != b"MD20" {
        let magic = String::from_utf8_lossy(magic).into_owned();
        return Ok(Model::Unloadable { unloadable: magic });
    }
    let version = u32_at(&head, 4).ok_or(CUT)?;
    if version != M2_VERSION {
        return Ok(Model::Unloadable {
            unloadable: format!("version {version}"),
        });
    }
    let word = |at| u32_at(&head, at).ok_or(CUT);
    let (skins, sequences, textures) = (
        word(0x44)?,
        (word(0x1C)?, word(0x20)?),
        (word(0x50)?, word(0x54)?),
    );
    let inside = |count: u32, at: u32, each: u32, most: u32, what: &str| {
        let end = u64::from(count) * u64::from(each) + u64::from(at);
        if count > most || end > u64::from(size) {
            Err(format!("claims {count} {what} that don't fit in it"))
        } else {
            Ok(())
        }
    };

    let mut needs = BTreeSet::new();
    if skins > MAX_SKINS {
        return Err(format!("claims {skins} skins"));
    }
    for skin in 0..skins {
        needs.insert(key(&format!("{stem}{skin:02}.skin")));
    }

    let (count, at) = sequences;
    inside(count, at, 0x40, MAX_SEQUENCES, "sequences")?;
    let table = ask(source, name, at, count * 0x40)?;
    let sequence = |index: usize| {
        let at = index.checked_mul(0x40)?;
        Some((
            u16_at(&table, at)?,
            u16_at(&table, at + 2)?,
            u32_at(&table, at + 0xC)?,
            u16_at(&table, at + 0x3E)?,
        ))
    };
    for index in 0..to_usize(count)? {
        let (mut id, mut variation, flags, mut alias) =
            sequence(index).ok_or("its sequences are cut short")?;
        if flags & (SEQUENCE_INLINE | SEQUENCE_LOADED) != 0 {
            continue;
        }
        let mut flags = flags;
        // Aliases follow to a sequence that isn't one, and use its animation.
        for _ in 0..64 {
            if flags & SEQUENCE_ALIAS == 0 {
                break;
            }
            let Some(next) = sequence(usize::from(alias)) else {
                break;
            };
            (id, variation, flags, alias) = next;
        }
        needs.insert(key(&format!("{stem}{id:04}-{variation:02}.anim")));
    }

    let (count, at) = textures;
    inside(count, at, 16, MAX_TEXTURES, "textures")?;
    let table = ask(source, name, at, count * 16)?;
    // Of the textures with a name, only those something draws, when the
    // skins are there to say.
    let drawn = drawn(source, there, name, skins).unwrap_or_else(|problem| {
        problems.push(format!("{name}: {problem}"));
        None
    });
    for index in 0..to_usize(count)? {
        if drawn.as_ref().is_some_and(|drawn| !drawn.contains(&index)) {
            continue;
        }
        let word = |field: usize| {
            u32_at(&table, index * 16 + field * 4).ok_or("its textures are cut short")
        };
        let (kind, length, from) = (word(0)?, word(2)?, word(3)?);
        if kind != 0 || length <= 1 {
            continue;
        }
        inside(1, from, length, 1024, "texture names")?;
        let text = ask(source, name, from, length)?;
        let text = text.split(|&byte| byte == 0).next().unwrap_or(&text);
        if !text.is_empty() {
            needs.insert(key(&String::from_utf8_lossy(text)));
        }
    }
    Ok(Model::Needs(needs.into_iter().collect()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An archive of files in memory, each named, whatever its case.
    struct Memory(BTreeMap<String, Vec<u8>>);

    impl Memory {
        fn of(files: &[(&str, Vec<u8>)]) -> Memory {
            Memory(
                files
                    .iter()
                    .map(|(name, bytes)| (name.to_string(), bytes.clone()))
                    .collect(),
            )
        }

        /// With a listfile naming every file.
        fn listed(files: &[(&str, Vec<u8>)]) -> Memory {
            let mut memory = Memory::of(files);
            let list: Vec<&str> = files.iter().map(|(name, _)| *name).collect();
            memory
                .0
                .insert(LISTFILE.into(), list.join("\r\n").into_bytes());
            memory
        }

        fn find(&self, name: &str) -> io::Result<&Vec<u8>> {
            let wanted = key(name);
            self.0
                .iter()
                .find(|(have, _)| key(have) == wanted)
                .map(|(_, bytes)| bytes)
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
        }
    }

    impl Source for Memory {
        fn entries(&self) -> Vec<Entry> {
            let entry = |(name, bytes): (&String, &Vec<u8>)| Entry {
                key: key(name),
                size: bytes.len() as u32,
                deleted: false,
            };
            self.0.iter().map(entry).collect()
        }

        fn read(&self, name: &str) -> io::Result<Vec<u8>> {
            self.find(name).cloned()
        }

        fn read_range(&self, name: &str, offset: u32, len: u32) -> io::Result<Vec<u8>> {
            let bytes = self.find(name)?;
            let from = (offset as usize).min(bytes.len());
            let to = from.saturating_add(len as usize).min(bytes.len());
            Ok(bytes[from..to].to_vec())
        }
    }

    /// A table of `fields` 4-byte fields: its rows (numbers, which for
    /// string columns are offsets) and its string block.
    fn dbc(fields: usize, rows: &[Vec<u32>], strings: &[u8]) -> Vec<u8> {
        let mut bytes = b"WDBC".to_vec();
        for number in [rows.len(), fields, fields * 4, strings.len()] {
            bytes.extend((number as u32).to_le_bytes());
        }
        for row in rows {
            assert_eq!(row.len(), fields);
            bytes.extend(row.iter().flat_map(|number| number.to_le_bytes()));
        }
        bytes.extend(strings);
        bytes
    }

    /// A string block holding `strings`, and where each one is.
    fn block(strings: &[&str]) -> (Vec<u8>, Vec<u32>) {
        let mut block = vec![0];
        let mut at = Vec::new();
        for text in strings {
            at.push(block.len() as u32);
            block.extend(text.as_bytes());
            block.push(0);
        }
        (block, at)
    }

    /// A row of `fields` zeros, but for the numbers set.
    fn row(fields: usize, set: &[(usize, u32)]) -> Vec<u32> {
        let mut row = vec![0; fields];
        for &(column, number) in set {
            row[column] = number;
        }
        row
    }

    /// A model of the given version, with the sequences (id, variation,
    /// flags, alias) and the textures (type, name) given and `skins` skins.
    fn m2(
        version: u32,
        skins: u32,
        sequences: &[(u16, u16, u32, u16)],
        textures: &[(u32, &str)],
    ) -> Vec<u8> {
        let mut bytes = vec![0; 0x58];
        bytes[..4].copy_from_slice(b"MD20");
        let mut put =
            |at: usize, number: u32| bytes[at..at + 4].copy_from_slice(&number.to_le_bytes());
        put(4, version);
        put(0x44, skins);
        put(0x1C, sequences.len() as u32);
        put(0x20, 0x58);
        put(0x50, textures.len() as u32);
        put(0x54, 0x58 + sequences.len() as u32 * 0x40);
        for &(id, variation, flags, alias) in sequences {
            let mut sequence = vec![0; 0x40];
            sequence[..2].copy_from_slice(&id.to_le_bytes());
            sequence[2..4].copy_from_slice(&variation.to_le_bytes());
            sequence[0xC..0x10].copy_from_slice(&flags.to_le_bytes());
            sequence[0x3E..].copy_from_slice(&alias.to_le_bytes());
            bytes.extend(sequence);
        }
        let mut names = Vec::new();
        let names_at = bytes.len() + textures.len() * 16;
        for &(kind, name) in textures {
            let length = if name.is_empty() { 0 } else { name.len() + 1 };
            for number in [kind as usize, 0, length, names_at + names.len()] {
                bytes.extend((number as u32).to_le_bytes());
            }
            if !name.is_empty() {
                names.extend(name.as_bytes());
                names.push(0);
            }
        }
        bytes.extend(names);
        bytes
    }

    #[test]
    fn a_table_yields_the_paths_of_its_fixed_columns() {
        let (strings, at) = block(&[
            "Creature\\Foo\\Foo.mdx",
            "FooSkin",
            "FooBake.blp",
            "Head_Foo",
            "Foo_Tex",
            "INV_Foo",
            "TorsoFoo",
            "World\\Foo.mdl",
            "Spell\\Bar.mdx",
        ]);
        let (path, skin, bake, head, tex, icon, torso, object, spell) = (
            at[0], at[1], at[2], at[3], at[4], at[5], at[6], at[7], at[8],
        );
        let memory = Memory::of(&[
            (
                "DBFilesClient\\CreatureModelData.dbc",
                dbc(
                    28,
                    &[row(28, &[(0, 1331), (2, path)]), row(28, &[(0, 2)])],
                    &strings,
                ),
            ),
            (
                "DBFilesClient\\CreatureDisplayInfo.dbc",
                dbc(16, &[row(16, &[(0, 7), (1, 1331), (6, skin)])], &strings),
            ),
            (
                "DBFilesClient\\CreatureDisplayInfoExtra.dbc",
                dbc(21, &[row(21, &[(0, 9), (20, bake)])], &strings),
            ),
            (
                "DBFilesClient\\ItemDisplayInfo.dbc",
                dbc(
                    25,
                    &[row(
                        25,
                        &[
                            (0, 4),
                            (1, head),
                            (3, tex),
                            (5, icon),
                            (6, icon),
                            (16, torso),
                        ],
                    )],
                    &strings,
                ),
            ),
            (
                "DBFilesClient\\GameObjectDisplayInfo.dbc",
                dbc(19, &[row(19, &[(0, 3), (1, object)])], &strings),
            ),
            (
                "DBFilesClient\\SpellVisualEffectName.dbc",
                dbc(7, &[row(7, &[(0, 5), (1, 1), (2, spell)])], &strings),
            ),
        ]);
        let facts = Facts::of(&memory, &[]);
        assert_eq!(facts.problems, Vec::<String>::new());
        let tables = facts.tables;
        assert_eq!(
            tables.creature_model_data,
            BTreeMap::from([(1331, "Creature\\Foo\\Foo.mdx".to_string())])
        );
        assert_eq!(tables.creature_display_info[&7].model, 1331);
        assert_eq!(
            tables.creature_display_info[&7].textures,
            ["FooSkin", "", ""]
        );
        assert_eq!(tables.creature_display_info_extra[&9], "FooBake.blp");
        let item = &tables.item_display_info[&4];
        assert_eq!(item.models, ["Head_Foo", ""]);
        assert_eq!(item.model_textures, ["Foo_Tex", ""]);
        assert_eq!(item.icons, ["INV_Foo", "INV_Foo"]);
        assert_eq!(item.components[1], "TorsoFoo");
        assert_eq!(tables.game_object_display_info[&3], "World\\Foo.mdl");
        assert_eq!(tables.spell_visual_effect_name[&5], "Spell\\Bar.mdx");
    }

    #[test]
    fn a_small_integer_is_never_taken_for_a_string() {
        // Every number that isn't in a name column is 1 or 2, which point
        // at the first strings of the block.
        let (strings, _) = block(&["Trap", "Another"]);
        let ones = |fields: usize| {
            row(
                fields,
                &(0..fields).map(|column| (column, 1)).collect::<Vec<_>>(),
            )
        };
        let mut cdi = ones(16);
        for column in [6, 7, 8] {
            cdi[column] = 0;
        }
        let memory = Memory::of(&[
            (
                "DBFilesClient\\CreatureDisplayInfo.dbc",
                dbc(16, &[cdi], &strings),
            ),
            (
                "DBFilesClient\\CreatureDisplayInfoExtra.dbc",
                dbc(
                    21,
                    &[row(21, &(0..20).map(|c| (c, 1)).collect::<Vec<_>>())],
                    &strings,
                ),
            ),
            (
                "DBFilesClient\\ItemDisplayInfo.dbc",
                dbc(
                    25,
                    &[row(
                        25,
                        &[
                            (0, 1),
                            (9, 1),
                            (10, 2),
                            (11, 1),
                            (12, 1),
                            (13, 1),
                            (14, 1),
                            (23, 1),
                            (24, 1),
                        ],
                    )],
                    &strings,
                ),
            ),
        ]);
        let facts = Facts::of(&memory, &[]);
        assert_eq!(facts.problems, Vec::<String>::new());
        assert_eq!(
            facts.tables.creature_display_info[&1].textures,
            ["", "", ""]
        );
        assert_eq!(facts.tables.creature_display_info[&1].model, 1);
        assert!(facts.tables.creature_display_info_extra.is_empty());
        assert!(facts.tables.item_display_info.is_empty());
        let json = serde_json::to_string(&facts).unwrap();
        assert!(
            !json.contains("Trap") && !json.contains("Another"),
            "{json}"
        );
    }

    #[test]
    fn a_model_names_its_skins_anims_and_textures() {
        let sequences = [
            (0, 0, 0, 0),
            // Its animation is inside the model.
            (4, 1, SEQUENCE_INLINE, 0),
            // An alias of the one after it.
            (5, 0, SEQUENCE_ALIAS, 3),
            (6, 2, 0, 0),
            // A chain of three, ending in a sequence of its own.
            (7, 0, SEQUENCE_ALIAS, 5),
            (8, 0, SEQUENCE_ALIAS, 6),
            (9, 3, 0, 0),
        ];
        let textures = [(0, "Creature/Foo/Foo.BLP"), (11, ""), (0, "")];
        let model = m2(264, 3, &sequences, &textures);
        let memory = Memory::listed(&[("Creature\\Foo\\Foo.m2", model.clone())]);
        let facts = Facts::of(&memory, &[]);
        assert_eq!(facts.problems, Vec::<String>::new());
        let mut want = vec![
            key("Creature\\Foo\\Foo00.skin"),
            key("Creature\\Foo\\Foo01.skin"),
            key("Creature\\Foo\\Foo02.skin"),
            key("Creature\\Foo\\Foo0000-00.anim"),
            key("Creature\\Foo\\Foo0006-02.anim"),
            key("Creature\\Foo\\Foo0009-03.anim"),
            key("Creature\\Foo\\Foo.blp"),
        ];
        want.sort();
        assert_eq!(
            facts.models[&key("Creature\\Foo\\Foo.m2")],
            Model::Needs(want)
        );
        assert_eq!(facts.models.len(), 1);

        // A model of another version, and one that isn't an MD20, are noted.
        let memory = Memory::listed(&[
            ("Cata.m2", m2(272, 1, &[], &[])),
            ("Chunked.m2", [b"MD21".as_slice(), &model[4..]].concat()),
        ]);
        let facts = Facts::of(&memory, &[]);
        assert_eq!(facts.problems, Vec::<String>::new());
        let unloadable = |what: &str| Model::Unloadable {
            unloadable: what.into(),
        };
        assert_eq!(facts.models[&key("Cata.m2")], unloadable("version 272"));
        assert_eq!(facts.models[&key("Chunked.m2")], unloadable("MD21"));
    }

    /// A model of version 264 with one skin and `textures` named, whose
    /// texture lookup is `lookup`, with a particle emitter for each of
    /// `emitters` (the texture it uses) and a ribbon for each of `ribbons`
    /// (the textures it uses).
    fn drawing_m2(
        textures: &[&str],
        lookup: &[i16],
        emitters: &[u16],
        ribbons: &[&[u16]],
    ) -> Vec<u8> {
        let mut bytes = vec![0; 0x130];
        bytes[..4].copy_from_slice(b"MD20");
        let mut at = 0x130;
        let put = |bytes: &mut Vec<u8>, field: usize, count: usize, from: usize| {
            bytes[field..field + 4].copy_from_slice(&(count as u32).to_le_bytes());
            bytes[field + 4..field + 8].copy_from_slice(&(from as u32).to_le_bytes());
        };
        bytes[4..8].copy_from_slice(&264u32.to_le_bytes());
        bytes[0x44..0x48].copy_from_slice(&1u32.to_le_bytes());

        put(&mut bytes, 0x50, textures.len(), at);
        let mut names = at + textures.len() * 16;
        for name in textures {
            for number in [0, 0, name.len() + 1, names] {
                bytes.extend((number as u32).to_le_bytes());
            }
            names += name.len() + 1;
        }
        for name in textures {
            bytes.extend(name.as_bytes());
            bytes.push(0);
        }
        at = bytes.len();

        put(&mut bytes, 0x80, lookup.len(), at);
        bytes.extend(lookup.iter().flat_map(|number| number.to_le_bytes()));
        at = bytes.len();

        put(&mut bytes, 0x128, emitters.len(), at);
        for texture in emitters {
            let mut emitter = vec![0; 0x1DC];
            emitter[0x16..0x18].copy_from_slice(&texture.to_le_bytes());
            bytes.extend(emitter);
        }
        at = bytes.len();

        put(&mut bytes, 0x120, ribbons.len(), at);
        let mut lists = at + ribbons.len() * 0xB0;
        for list in ribbons {
            let mut ribbon = vec![0; 0xB0];
            ribbon[0x14..0x18].copy_from_slice(&(list.len() as u32).to_le_bytes());
            ribbon[0x18..0x1C].copy_from_slice(&(lists as u32).to_le_bytes());
            bytes.extend(ribbon);
            lists += list.len() * 2;
        }
        for list in ribbons {
            bytes.extend(list.iter().flat_map(|number| number.to_le_bytes()));
        }

        bytes
    }

    /// A skin with the batches (textures used, first lookup index).
    fn skin(batches: &[(u16, u16)]) -> Vec<u8> {
        let mut bytes = vec![0; 0x2C];
        bytes[..4].copy_from_slice(b"SKIN");
        bytes[0x24..0x28].copy_from_slice(&(batches.len() as u32).to_le_bytes());
        bytes[0x28..0x2C].copy_from_slice(&0x2Cu32.to_le_bytes());
        for &(count, first) in batches {
            let mut batch = vec![0; 24];
            batch[0x0E..0x10].copy_from_slice(&count.to_le_bytes());
            batch[0x10..0x12].copy_from_slice(&first.to_le_bytes());
            bytes.extend(batch);
        }

        bytes
    }

    #[test]
    fn a_model_needs_only_the_textures_its_skins_particles_and_ribbons_draw() {
        let textures = ["A.blp", "B.blp", "C.blp", "D.blp", "E.blp", "F.blp"];
        // Lookup 0 and 1 name textures 1 and 0; 2 names texture 2. The skin
        // draws lookups 0 and 2, an emitter texture 3, a ribbon texture 4.
        let model = drawing_m2(&textures, &[1, 0, 2], &[3], &[&[4]]);
        let memory = Memory::listed(&[("Foo.m2", model), ("Foo00.skin", skin(&[(1, 0), (1, 2)]))]);
        let facts = Facts::of(&memory, &[]);
        assert_eq!(facts.problems, Vec::<String>::new());
        let mut want = vec![
            key("Foo00.skin"),
            key("B.blp"),
            key("C.blp"),
            key("D.blp"),
            key("E.blp"),
        ];
        want.sort();
        assert_eq!(facts.models[&key("Foo.m2")], Model::Needs(want));
    }

    #[test]
    fn a_model_whose_skins_are_elsewhere_keeps_every_named_texture() {
        let textures = ["A.blp", "B.blp"];
        let model = drawing_m2(&textures, &[1], &[], &[]);
        let every = |skin: Option<Vec<u8>>| {
            let mut files = vec![("Foo.m2", model.clone())];
            files.extend(skin.map(|skin| ("Foo00.skin", skin)));
            let facts = Facts::of(&Memory::listed(&files), &[]);
            let Model::Needs(needs) = &facts.models[&key("Foo.m2")] else {
                panic!("{facts:?}");
            };
            (needs.contains(&key("A.blp")), facts.problems)
        };

        assert_eq!(every(None), (true, Vec::new()));
        assert_eq!(every(Some(skin(&[(1, 0)]))), (false, Vec::new()));

        // A skin that lies, or isn't one, is a problem, not a panic; the
        // model keeps all its textures.
        let mut lies = skin(&[(1, 0)]);
        lies[0x24..0x28].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut far = skin(&[(1, 0)]);
        far[0x28..0x2C].copy_from_slice(&u32::MAX.to_le_bytes());
        for bad in [lies, far, b"nope".to_vec(), Vec::new()] {
            let (kept, problems) = every(Some(bad));
            assert!(kept);
            assert_eq!(problems.len(), 1, "{problems:?}");
        }

        // So does a model whose lookup, emitters or ribbons don't add up.
        for field in [0x80, 0x120, 0x128] {
            let mut lies = drawing_m2(&textures, &[1], &[0], &[&[0]]);
            lies[field..field + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            let mut far = lies.clone();
            far[field..field + 4].copy_from_slice(&1u32.to_le_bytes());
            far[field + 4..field + 8].copy_from_slice(&u32::MAX.to_le_bytes());
            for bad in [lies, far] {
                let memory = Memory::listed(&[("Foo.m2", bad), ("Foo00.skin", skin(&[(1, 0)]))]);
                let facts = Facts::of(&memory, &[]);
                assert_eq!(facts.problems.len(), 1, "{field:#x}: {:?}", facts.problems);
                let Model::Needs(needs) = &facts.models[&key("Foo.m2")] else {
                    panic!("{facts:?}");
                };
                assert!(needs.contains(&key("A.blp")) && needs.contains(&key("B.blp")));
            }
        }
    }

    #[test]
    fn mdx_and_mdl_names_mean_m2() {
        assert_eq!(
            model_name("Creature\\Foo\\Foo.mdx"),
            "Creature\\Foo\\Foo.m2"
        );
        assert_eq!(
            model_name("Creature\\Foo\\Foo.MDL"),
            "Creature\\Foo\\Foo.m2"
        );
        assert_eq!(model_name("World\\Foo.wmo"), "World\\Foo.wmo");
        assert_eq!(model_name("Foo.m2"), "Foo.m2");
        assert_eq!(model_name("Foo.mdx\\Bar"), "Foo.mdx\\Bar");
        assert_eq!(model_name("Foo"), "Foo");

        // An archive without a listfile has its model named by the table
        // that calls it `.mdx`.
        let (strings, at) = block(&["Creature\\Foo\\Foo.mdx"]);
        let memory = Memory::of(&[
            (
                "DBFilesClient\\CreatureModelData.dbc",
                dbc(28, &[row(28, &[(0, 1), (2, at[0])])], &strings),
            ),
            ("Creature\\Foo\\Foo.m2", m2(264, 1, &[], &[])),
        ]);
        let facts = Facts::of(&memory, &[]);
        assert_eq!(
            facts.names[&key("Creature\\Foo\\Foo.m2")],
            "Creature\\Foo\\Foo.m2"
        );
        assert_eq!(
            facts.models[&key("Creature\\Foo\\Foo.m2")],
            Model::Needs(vec![key("Creature\\Foo\\Foo00.skin")])
        );
    }

    #[test]
    fn facts_round_trip_through_json() {
        let (strings, at) = block(&["Creature\\Foo\\Foo.mdx", "Skin"]);
        let memory = Memory::listed(&[
            (
                "DBFilesClient\\CreatureModelData.dbc",
                dbc(28, &[row(28, &[(0, 1), (2, at[0])])], &strings),
            ),
            (
                "DBFilesClient\\CreatureDisplayInfo.dbc",
                dbc(16, &[row(16, &[(0, 2), (1, 1), (6, at[1])])], &strings),
            ),
            (
                "Creature\\Foo\\Foo.m2",
                m2(264, 2, &[(1, 0, 0, 0)], &[(0, "Foo.blp")]),
            ),
            ("Cata.m2", m2(272, 1, &[], &[])),
            ("Interface\\Icons\\Foo.blp", b"x".to_vec()),
            ("Sound\\Foo.wav", b"x".to_vec()),
            ("Interface\\FrameXML\\Foo.lua", b"x".to_vec()),
            ("World\\Maps\\Foo.wdt", b"x".to_vec()),
            ("readme", b"x".to_vec()),
        ]);
        let mut facts = Facts::of(&memory, &[]);
        facts.problems.push("a problem".into());
        let json = serde_json::to_string(&facts).unwrap();
        assert!(json.contains(&format!("\"{}\"", key("Cata.m2"))), "{json}");
        assert_eq!(serde_json::from_str::<Facts>(&json).unwrap(), facts);
        let kinds: BTreeSet<Kind> = facts.files.values().map(|file| file.kind).collect();
        assert_eq!(
            kinds,
            BTreeSet::from([
                Kind::Tables,
                Kind::Models,
                Kind::Textures,
                Kind::Sounds,
                Kind::Interface,
                Kind::World,
                Kind::Other,
            ])
        );
    }

    #[test]
    fn a_table_with_byte_fields_is_left_alone() {
        // Four fields, but a record of five bytes.
        let mut bytes = b"WDBC".to_vec();
        for number in [2u32, 4, 5, 1] {
            bytes.extend(number.to_le_bytes());
        }
        bytes.extend([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 0]);
        let memory = Memory::of(&[("DBFilesClient\\CreatureModelData.dbc", bytes)]);
        let facts = Facts::of(&memory, &[]);
        assert_eq!(facts.problems, Vec::<String>::new());
        assert_eq!(facts.tables, Tables::default());
    }

    #[test]
    fn truncated_tables_and_models_are_problems_not_panics() {
        let (strings, at) = block(&["Creature\\Foo\\Foo.mdx"]);
        let table = dbc(28, &[row(28, &[(0, 1), (2, at[0])])], &strings);
        let model = m2(
            264,
            2,
            &[(1, 0, 0, 0), (2, 0, SEQUENCE_ALIAS, 9)],
            &[(0, "Foo.blp")],
        );
        let mut lies = model.clone();
        // A skin count, sequences and textures that go beyond the file.
        lies[0x44..0x48].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut far = model.clone();
        far[0x20..0x24].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut many = model.clone();
        many[0x50..0x54].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut header = table.clone();
        header[4..8].copy_from_slice(&u32::MAX.to_le_bytes());

        for cut in 0..table.len() {
            let memory = Memory::listed(&[(
                "DBFilesClient\\CreatureModelData.dbc",
                table[..cut].to_vec(),
            )]);
            let facts = Facts::of(&memory, &[]);
            assert_eq!(
                facts.problems.len(),
                1,
                "cut at {cut}: {:?}",
                facts.problems
            );
            assert!(facts.tables.creature_model_data.is_empty());
        }
        for cut in 0..model.len() {
            let memory = Memory::listed(&[("Foo.m2", model[..cut].to_vec())]);
            Facts::of(&memory, &[]);
        }
        for bytes in [lies, far, many] {
            let memory = Memory::listed(&[("Foo.m2", bytes)]);
            let facts = Facts::of(&memory, &[]);
            assert_eq!(facts.problems.len(), 1, "{:?}", facts.problems);
            assert!(facts.models.is_empty());
        }
        let memory = Memory::listed(&[("DBFilesClient\\CreatureModelData.dbc", header)]);
        assert_eq!(Facts::of(&memory, &[]).problems.len(), 1);

        // A name outside the string block is a problem, the table kept.
        let wild = dbc(28, &[row(28, &[(0, 1), (2, 999)])], &strings);
        let memory = Memory::of(&[("DBFilesClient\\CreatureModelData.dbc", wild)]);
        let facts = Facts::of(&memory, &[]);
        assert_eq!(facts.problems.len(), 1);
        assert!(facts.tables.creature_model_data.is_empty());
    }
}
