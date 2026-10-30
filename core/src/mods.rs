//! The launcher's mods: changes to a client outside `Interface/AddOns`, such
//! as a DLL the game loads beside `Wow.exe` or a patched archive in `Data`,
//! each with the files its releases put in the client folder.
//!
//! Like the catalog of addons, the list is generated offline by the
//! `indexer` tool, which hashes every file of every release, and is published
//! as one JSON document. Which release a client holds is recorded in a marker
//! in the notes beside `Wow.exe`, written when the launcher installs a
//! release or recognizes one already there, by its files. A copy that
//! matches no release, with no marker, isn't known to be the mod's:
//! installing over it backs it up. The marker also notes the size and
//! modification time of each file found as released, so a look at the client
//! reads only the files that changed since; [`verify`] reads them all.
//!
//! A path in the list may name a language, as [`LOCALE`]: a mod's files can
//! go in the folder of `Data` of each language the client has, so it plays
//! whichever its game is switched to, and a language deleted takes its
//! copies with it, leaving the mod whole in the others. Paths are found in
//! a client folder whatever their case, as the game finds them. Some mods
//! are alternatives to each other, editions of one thing, of which a client
//! holds one at most: installing one takes the place of the other.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::io_at;
use crate::installation::{STAGING_PREFIX, child, config_setting};
use crate::notes::{Notes, Stamp};
use crate::patches::{Cache, Change, Facts, Opened};
use crate::{Error, Installation, Locale, archive};

/// What stands for a language (`enUS`, `deDE`, …) in the paths of the list
/// of mods: a mod's file at such a path goes in every language the client
/// has, and what a mod requires or excludes is looked for in the one its
/// game plays in.
pub const LOCALE: &str = "{locale}";

/// A published list of mods.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mods {
    /// The schema version; see [`Mods::VERSION`].
    pub version: u32,
    /// Every mod.
    pub mods: Vec<Mod>,
}

/// A mod: what it is, where it comes from, and its releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mod {
    /// What identifies it, and names its marker: lowercase letters, digits
    /// and `-`.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What it does.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Where it comes from.
    pub source: Source,
    /// Its releases, the newest first.
    pub releases: Vec<Release>,
    /// The player's files: its settings and what they save in the game,
    /// some of which its releases bring. Never checked, kept by a reinstall,
    /// and removed only when the player asks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<String>,
    /// Files the mod writes as it runs, which removing it removes too.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub writes: Vec<String>,
    /// What `WTF/Config.wtf` must set for the mod to work (`gxApi` to
    /// `d3d9`), which installing it sets.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, String>,
    /// The systems it is for, as Rust names them (`windows`, `linux`,
    /// `macos`): every system when there are none. DXVK is for Windows
    /// only, since Proton and the like bring their own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
    /// Files a client must have for the mod to suit it: one without them
    /// all isn't offered it. An edition made for an HD model pack requires
    /// the pack's archives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// Files of the clients the mod isn't made for: one with all of them
    /// isn't offered it, and one with some is, those being in conflict with
    /// it (see [`excluded`]). An edition made for clients without an HD
    /// model pack excludes the pack's language archive, which only that
    /// pack has.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excludes: Vec<String>,
    /// Files it can't be beside, whoever's: those a client has are in
    /// conflict with it, and installing it backs them up (see
    /// [`excluded`]), but it is offered all the same. Patch Y's HD edition
    /// with the original spell visuals can't be beside the new ones'
    /// archives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<String>,
    /// The mods it is an alternative to, by id: editions of one thing, of
    /// which a client holds one at most. Installing it takes the place of
    /// the one there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternatives: Vec<String>,
    /// Where its files say their version, when not in their version
    /// resource (see [`crate::version::product_version`]): DXVK's DLLs
    /// carry Windows' own version there, to pass for its, and say theirs
    /// after `DXVK: ` in the file. Read of a copy installed by hand that
    /// matches no release (see [`Problem::Unknown`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version_in: Option<VersionIn>,
}

/// Where a mod's files say their version; see [`Mod::version_in`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionIn {
    /// The file, by path relative to the client folder.
    pub file: String,
    /// The text the version follows in it, up to a NUL byte: a log line's
    /// start, say.
    pub after: String,
}

/// Where a mod comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    /// Who publishes it, and where ("Lumenveil on GitHub").
    pub name: String,
    /// Its page there.
    pub page: String,
}

/// A release of a mod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// Its version, as its source names it.
    pub version: String,
    /// Where to download it: an archive of its files, or its one file
    /// itself.
    pub url: String,
    /// When it came out (`YYYY-MM-DD`), if its source says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// What changed in it, if its source says somewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changes: Option<String>,
    /// Every file it puts in the client folder, by path relative to it with
    /// `/` between folders, with its SHA-256 in lowercase hexadecimal. An
    /// archive holds them at the same paths, in [`Release::inside`], but
    /// those [`Release::from`] names.
    pub files: Vec<(String, String)>,
    /// The file of its archive each of its files is, by the file's path,
    /// when not at the same path: one file of the archive may go to several
    /// places (`patch-y.mpq` to `Data/patch-y.mpq` and
    /// `Data/{locale}/patch-{locale}-Y.MPQ`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub from: BTreeMap<String, String>,
    /// The folder of its archive its files are in, when not the top:
    /// `dxvk-3.1.1/x32` holds DXVK's. The rest of the archive is left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inside: Option<String>,
    /// What runs once its files are in place, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<Step>,
    /// Its files when they differ by language, by language, each with its
    /// SHA-256: their paths name the language as [`LOCALE`], and so does
    /// `url`, which then downloads one language's files at a time (see
    /// [`Release::downloads`]); `files` lists none. A client gets those of
    /// every language it has, and those of another, as chosen, where it
    /// doesn't come in one (see [`lacking`]). Languages the game doesn't
    /// know are left out.
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        deserialize_with = "crate::locale::lenient::map"
    )]
    pub languages: BTreeMap<Locale, Vec<(String, String)>>,
}

/// A program of a release, run once its files are in place, the way the game
/// starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    /// The program: one of the release's files.
    pub run: String,
    /// Its arguments; `{locale}` stands for the language the client plays
    /// in.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arguments: Vec<String>,
    /// The file it writes, which tells whether it ran.
    pub creates: String,
}

/// Which release of a mod a client folder holds, as the launcher recorded
/// it when it installed or recognized it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Marker {
    /// The release's version.
    pub version: String,
    /// Where it was downloaded from, or would have been.
    pub url: String,
    /// Whether its setup ran to the end. A setup cut short (the launcher
    /// killed halfway) can leave what it writes half written, which only
    /// this tells.
    #[serde(default)]
    pub set_up: bool,
    /// The language of the release put in the client's languages it
    /// doesn't come in, when the player chose one (see [`lacking`]).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "crate::locale::lenient::option"
    )]
    pub instead: Option<Locale>,
    /// The release's files, by path, as they were when last found as
    /// released: a look reads only those that no longer are (see [`check`]),
    /// and [`verify`] reads them all. A marker written before this has none.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) files: BTreeMap<String, Stamp>,
}

/// What a client folder holds of a mod, and what keeps it from being as
/// released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The release it holds, as its marker says, when one does.
    pub release: Option<String>,
    /// What keeps it from being that release, as released, if anything.
    pub problems: Vec<Problem>,
}

/// Something that keeps a mod from being as released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// No marker says which release it is: its files weren't checked
    /// against its releases (see [`check`]).
    Unchecked,
    /// Its files match none of its releases: a copy installed by hand, the
    /// player's to keep.
    Unknown {
        /// The version its files say they are, if they say (see
        /// [`Mod::version_in`]).
        version: Option<String>,
        /// Its DLLs beside `Wow.exe`, of any release, by name without
        /// `.dll`: the ones Wine is told to load from there.
        natives: Vec<String>,
    },
    /// The release its marker names is no longer listed.
    Unlisted(String),
    /// Files of its release that are missing, by path.
    Missing(Vec<String>),
    /// Files that differ from its release's, by path.
    Changed(Vec<String>),
    /// What its setup writes, missing, or all of it when its marker doesn't
    /// say the setup ran to the end.
    NotSetUp(Vec<String>),
    /// What `WTF/Config.wtf` sets otherwise than the mod needs: the
    /// setting, its value, and the value needed.
    Config(Vec<(String, String, String)>),
}

impl Mods {
    /// The schema version this crate reads and writes: 2 added the
    /// language in paths, files placed from another file of the archive,
    /// the files a client must have or not, alternatives, and releases
    /// whose files differ by language.
    pub const VERSION: u32 = 2;

    /// Reads a published list, refusing schema versions this build does not
    /// know. Releases naming a path outside the client folder, or running a
    /// program they don't bring, are dropped, and so are mods with such
    /// files, without a valid id, or without releases.
    pub fn parse(json: &[u8]) -> Result<Self, Error> {
        let mut mods: Self = serde_json::from_slice(json).map_err(Error::Mods)?;

        if mods.version != Self::VERSION {
            return Err(Error::ModsVersion(mods.version));
        }

        mods.mods.retain_mut(|game_mod| {
            game_mod.releases.retain(|release| {
                let every = || {
                    release
                        .files
                        .iter()
                        .chain(release.languages.values().flatten())
                };

                every().all(|(path, _)| safe(path))
                    && release
                        .from
                        .iter()
                        .all(|(to, from)| safe(from) && every().any(|(path, _)| path == to))
                    && (release.languages.is_empty()
                        || (release.files.is_empty() && release.url.contains(LOCALE)))
                    // Its languages' files, but those the game doesn't know.
                    && every().next().is_some()
                    && release.inside.as_deref().is_none_or(safe)
                    && release.setup.iter().all(|step| {
                        safe(&step.creates)
                            && release.files.iter().any(|(path, _)| *path == step.run)
                    })
            });

            valid(&game_mod.id)
                && game_mod
                    .settings
                    .iter()
                    .chain(&game_mod.writes)
                    .chain(&game_mod.requires)
                    .chain(&game_mod.excludes)
                    .chain(&game_mod.conflicts)
                    .all(|path| safe(path))
                && game_mod
                    .version_in
                    .as_ref()
                    .is_none_or(|place| safe(&place.file) && !place.after.is_empty())
                && !game_mod.releases.is_empty()
        });

        Ok(mods)
    }

    /// The list as it is published: sorted, so regenerating it only changes
    /// what actually changed, and pretty-printed for readable diffs. The
    /// order of each mod's releases is kept: the newest first.
    pub fn to_json(&self) -> Result<String, Error> {
        let mut mods = self.clone();

        mods.mods.sort_by(|a, b| a.id.cmp(&b.id));
        for release in mods
            .mods
            .iter_mut()
            .flat_map(|game_mod| &mut game_mod.releases)
        {
            release.files.sort();
        }

        let mut json = serde_json::to_string_pretty(&mods).map_err(Error::Mods)?;
        json.push('\n');

        Ok(json)
    }

    /// The mods of the list that `game_mod` is an alternative to, or that
    /// are one to it: a client holds one of them at most.
    pub fn alternatives(&self, game_mod: &Mod) -> Vec<&Mod> {
        self.mods
            .iter()
            .filter(|other| {
                other.id != game_mod.id
                    && (game_mod.alternatives.contains(&other.id)
                        || other.alternatives.contains(&game_mod.id))
            })
            .collect()
    }

    /// The files the mods installed in the client folder at `root` own, by
    /// path within it, in lowercase (a torrent may spell it `data/enus/…`),
    /// with the id of the mod: those of the release its marker names, as
    /// placed there (see [`Mod::placed`]), and its settings and what it
    /// writes. A marker naming a release no longer listed owns every path
    /// of the mod. A mod without a marker owns nothing: its files are as
    /// much the player's as any.
    pub fn owned(&self, root: &Path) -> BTreeMap<String, String> {
        let mut owned = BTreeMap::new();

        for game_mod in &self.mods {
            let Some(marker) = game_mod.marker(root) else {
                continue;
            };
            let Some(placed) = game_mod.placed_in(root) else {
                continue;
            };
            let paths: BTreeSet<&str> = match placed.release(&marker.version) {
                Some(release) => release
                    .files
                    .iter()
                    .map(|(path, _)| path.as_str())
                    .chain(placed.settings.iter().map(String::as_str))
                    .chain(placed.written())
                    .collect(),
                None => placed.claimed(),
            };

            for path in paths {
                let _ = owned
                    .entry(path.to_ascii_lowercase())
                    .or_insert_with(|| game_mod.id.clone());
            }
        }

        owned
    }
}

/// `path` in a client that plays in `locale`, [`LOCALE`] in it standing for
/// it: nothing when it names the language and `locale` is unknown.
fn place(path: &str, locale: Option<Locale>) -> Option<String> {
    if path.contains(LOCALE) {
        Some(path.replace(LOCALE, locale?.code()))
    } else {
        Some(path.to_owned())
    }
}

/// Every one of `paths` in a client that plays in `locale` (see [`place`]).
fn place_all(paths: &[String], locale: Option<Locale>) -> Option<Vec<String>> {
    paths.iter().map(|path| place(path, locale)).collect()
}

/// `path` in a client that has `locales`, by language: [`LOCALE`] in it
/// standing for each in turn, or the path alone when it names none. Nothing
/// when it names one and the client has none.
fn spread(path: &str, locales: &[Locale]) -> Option<Vec<(Option<Locale>, String)>> {
    if !path.contains(LOCALE) {
        return Some(vec![(None, path.to_owned())]);
    }

    (!locales.is_empty()).then(|| {
        locales
            .iter()
            .map(|locale| (Some(*locale), path.replace(LOCALE, locale.code())))
            .collect()
    })
}

/// Every one of `paths` in a client that has `locales` (see [`spread`]).
fn spread_all(paths: &[String], locales: &[Locale]) -> Option<Vec<String>> {
    let mut spread_out = Vec::new();
    for path in paths {
        spread_out.extend(spread(path, locales)?.into_iter().map(|(_, path)| path));
    }

    Some(spread_out)
}

/// The language the game of the client folder at `root` plays in, and every
/// one it has, that one among them.
fn languages(root: &Path) -> (Option<Locale>, Vec<Locale>) {
    let locale = crate::installation::locale(root);
    let mut locales = crate::installation::locales(root);
    if let Some(locale) = locale
        && !locales.contains(&locale)
    {
        locales.push(locale);
    }

    (locale, locales)
}

impl Mod {
    /// The mod as it goes in a client that plays in `locale` and has
    /// `locales`, its paths naming those languages (see [`LOCALE`]): its
    /// files in each of them, what it requires or excludes in the one its
    /// game plays in, what it can't be beside in any. Releases whose files
    /// differ by language put those of `instead` in the client's languages
    /// they don't come in (see [`Release::placed`]). Nothing when its paths
    /// name a language and the client has none.
    pub fn placed(
        &self,
        locale: Option<Locale>,
        locales: &[Locale],
        instead: Option<Locale>,
    ) -> Option<Self> {
        Some(Self {
            releases: self
                .releases
                .iter()
                .map(|release| release.placed(locale, locales, instead))
                .collect::<Option<_>>()?,
            settings: spread_all(&self.settings, locales)?,
            writes: spread_all(&self.writes, locales)?,
            requires: place_all(&self.requires, locale)?,
            excludes: place_all(&self.excludes, locale)?,
            conflicts: spread_all(&self.conflicts, locales)?,
            version_in: match &self.version_in {
                Some(place_of) => Some(VersionIn {
                    file: place(&place_of.file, locale)?,
                    after: place_of.after.clone(),
                }),
                None => None,
            },
            ..self.clone()
        })
    }

    /// The mod as it goes in the client folder at `root` (see
    /// [`Mod::placed`]), in the language its marker says, if any, where the
    /// client's own isn't among its releases'.
    fn placed_in(&self, root: &Path) -> Option<Self> {
        let (locale, locales) = languages(root);
        let instead = self.marker(root).and_then(|marker| marker.instead);

        self.placed(locale, &locales, instead)
    }

    /// Its release of `version`.
    pub fn release(&self, version: &str) -> Option<&Release> {
        self.releases
            .iter()
            .find(|release| release.version == version)
    }

    /// Its newest release.
    pub fn newest(&self) -> Option<&Release> {
        self.releases.first()
    }

    /// Whether it is for the system the launcher runs on.
    pub fn is_for_this_system(&self) -> bool {
        self.platforms.is_empty()
            || self
                .platforms
                .iter()
                .any(|platform| platform == std::env::consts::OS)
    }

    /// Whether the file at `path` is the player's.
    fn is_setting(&self, path: &str) -> bool {
        self.settings.iter().any(|setting| setting == path)
    }

    /// Every path its releases put files at, but the player's files.
    fn paths(&self) -> BTreeSet<&str> {
        self.releases
            .iter()
            .flat_map(|release| &release.files)
            .map(|(path, _)| path.as_str())
            .filter(|path| !self.is_setting(path))
            .collect()
    }

    /// Whether any release has `sum` for the file at `path`.
    fn released(&self, path: &str, sum: &str) -> bool {
        self.releases
            .iter()
            .flat_map(|release| &release.files)
            .any(|(released, released_sum)| released == path && released_sum == sum)
    }

    /// Every path it may put in a client folder, in any release: its files,
    /// the player's, and what it and its setups write.
    fn claimed(&self) -> BTreeSet<&str> {
        self.releases
            .iter()
            .flat_map(|release| &release.files)
            .map(|(path, _)| path.as_str())
            .chain(self.settings.iter().map(String::as_str))
            .chain(self.written())
            .collect()
    }

    /// What it and its setups write, in any release.
    fn written(&self) -> BTreeSet<&str> {
        self.writes
            .iter()
            .map(String::as_str)
            .chain(
                self.releases
                    .iter()
                    .flat_map(|release| &release.setup)
                    .map(|step| step.creates.as_str()),
            )
            .collect()
    }

    /// Where 0.9.0 kept its marker in the client folder at `root`, a file
    /// of its own: one there moves into the folder's notes when read.
    fn old_marker_path(&self, root: &Path) -> PathBuf {
        root.join(format!(".old_launcher-{}.toml", self.id))
    }

    /// Its marker in the client folder at `root`, as the folder notes it,
    /// if there is a valid one.
    pub fn marker(&self, root: &Path) -> Option<Marker> {
        if let Some(marker) = Notes::read(root).mods.remove(&self.id) {
            return Some(marker);
        }

        let old = self.old_marker_path(root);
        let marker: Marker = toml::from_str(&fs::read_to_string(&old).ok()?).ok()?;
        // Noted with the rest, it goes; noting it may fail, and does again.
        let moved = Notes::change(root, |notes| {
            let _ = notes.mods.insert(self.id.clone(), marker.clone());
        });
        if moved.is_ok() {
            let _ = fs::remove_file(&old);
        }

        Some(marker)
    }

    /// Whether the client folder at `root` has a marker of it, valid or not.
    fn marked(&self, root: &Path) -> bool {
        Notes::read(root).mods.contains_key(&self.id) || self.old_marker_path(root).is_file()
    }

    /// Notes in the client folder at `root` that it holds `release`, and
    /// whether its setup ran to the end; `files` are the stamps of those
    /// found as released.
    fn mark(
        &self,
        root: &Path,
        release: &Release,
        set_up: bool,
        instead: Option<Locale>,
        files: BTreeMap<String, Stamp>,
    ) -> Result<(), Error> {
        let marker = Marker {
            version: release.version.clone(),
            url: release.url.clone(),
            set_up,
            instead,
            files,
        };
        Notes::change(root, |notes| {
            let _ = notes.mods.insert(self.id.clone(), marker);
        })?;

        remove(&self.old_marker_path(root))
    }

    /// Notes in the client folder at `root` that the files of the release
    /// `marker` names were found as released with these `files` stamps,
    /// unless they are the ones noted, or another release was marked since.
    fn stamp(&self, root: &Path, marker: &Marker, files: BTreeMap<String, Stamp>) {
        if marker.files == files {
            return;
        }

        let noted = Notes::change(root, |notes| {
            if let Some(held) = notes.mods.get_mut(&self.id)
                && held.version == marker.version
                && held.url == marker.url
            {
                held.files = files;
            }
        });
        if let Err(error) = noted {
            log::warn!("Could not note the files of {}: {error}", self.name);
        }
    }

    /// Forgets its marker in the client folder at `root`.
    fn unmark(&self, root: &Path) -> Result<(), Error> {
        Notes::change(root, |notes| {
            let _ = notes.mods.remove(&self.id);
        })?;

        remove(&self.old_marker_path(root))
    }
}

impl Release {
    /// The release as it goes in a client that plays in `locale` and has
    /// `locales` (see [`Mod::placed`]): its files in each language, each
    /// from its archive's file of that language; when its files differ by
    /// language, those of each of the client's languages, or of `instead`
    /// for those it doesn't come in; its setup in the one the game plays
    /// in.
    pub fn placed(
        &self,
        locale: Option<Locale>,
        locales: &[Locale],
        instead: Option<Locale>,
    ) -> Option<Self> {
        let mut files = Vec::new();
        for (path, sum) in &self.files {
            files.extend(
                spread(path, locales)?
                    .into_iter()
                    .map(|(_, path)| (path, sum.clone())),
            );
        }
        let mut from = BTreeMap::new();
        for (to, member) in &self.from {
            for (language, to) in spread(to, locales)? {
                let member = place(member, language.or(locale))?;
                let _ = from.insert(to, member);
            }
        }
        // In each of the client's languages, the files of that language, or
        // of `instead`, by the language of the release they are of: what
        // the download of that language holds.
        let mut languages: BTreeMap<Locale, Vec<(String, String)>> = BTreeMap::new();
        for there in locales {
            let Some(language) = [Some(*there), instead]
                .into_iter()
                .flatten()
                .find(|language| self.languages.contains_key(language))
            else {
                continue;
            };
            let own: Vec<(String, String)> = self.languages[&language]
                .iter()
                .map(|(path, sum)| (path.replace(LOCALE, there.code()), sum.clone()))
                .collect();
            files.extend(own.iter().cloned());
            languages.entry(language).or_default().extend(own);
        }

        Some(Self {
            files,
            from,
            languages,
            setup: self
                .setup
                .iter()
                .map(|step| {
                    Some(Step {
                        run: place(&step.run, locale)?,
                        creates: place(&step.creates, locale)?,
                        arguments: step.arguments.clone(),
                    })
                })
                .collect::<Option<_>>()?,
            ..self.clone()
        })
    }

    /// The file of its archive that is its file at `path`.
    fn source<'a>(&'a self, path: &'a str) -> &'a str {
        self.from.get(path).map_or(path, String::as_str)
    }

    /// What downloading it, as it goes in a client (see
    /// [`Release::placed`]), takes: by address, the files each download
    /// holds. One download of all its files, or one per language when they
    /// differ by language (see [`Release::languages`]).
    pub fn downloads(&self) -> Vec<(String, Vec<String>)> {
        let paths =
            |files: &[(String, String)]| files.iter().map(|(path, _)| path.clone()).collect();

        if self.languages.is_empty() {
            return vec![(self.url.clone(), paths(&self.files))];
        }

        self.languages
            .iter()
            .map(|(language, files)| (self.url.replace(LOCALE, language.code()), paths(files)))
            .collect()
    }

    /// Its DLLs at the client folder's root, by name without `.dll`: the
    /// ones `Wow.exe` may load from its folder in place of Windows' own.
    pub fn natives(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|(path, _)| !path.contains('/'))
            .filter_map(|(path, _)| {
                let (stem, extension) = path.split_at_checked(path.len().checked_sub(4)?)?;
                extension
                    .eq_ignore_ascii_case(".dll")
                    .then(|| stem.to_owned())
            })
            .collect()
    }
}

/// The SHA-256 of `bytes`, in lowercase hexadecimal.
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Lists the files of `archive`, by path with `/` between folders, with
/// their SHA-256, sorted by path.
pub fn inspect(archive: &[u8]) -> Result<Vec<(String, String)>, Error> {
    let temporary = std::env::temp_dir();
    let staging = tempfile::tempdir().map_err(io_at(&temporary))?;
    archive::extract(archive, staging.path())?;

    let mut files = Vec::new();
    walk(staging.path(), staging.path(), &mut files)?;
    files.sort();

    Ok(files)
}

/// Adds every file under `directory`, by path relative to `root`, with its
/// SHA-256, to `files`.
fn walk(root: &Path, directory: &Path, files: &mut Vec<(String, String)>) -> Result<(), Error> {
    for entry in fs::read_dir(directory).map_err(io_at(directory))? {
        let path = entry.map_err(io_at(directory))?.path();

        if path.is_dir() {
            walk(root, &path, files)?;
        } else {
            let bytes = fs::read(&path).map_err(io_at(&path))?;
            let relative = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");

            files.push((relative, sha256(&bytes)));
        }
    }

    Ok(())
}

/// Where `path` is in the client folder at `root`: each folder and the file
/// found whatever their case, as the game finds them, or as named when
/// there is none.
fn at(root: &Path, path: &str) -> PathBuf {
    path.split('/')
        .fold(root.to_path_buf(), |folder, name| child(&folder, name))
}

/// `path` as it is spelled in the client folder at `root` (see [`at`]),
/// with `/` between folders: `Data/patch-Y.mpq` for `Data/patch-y.mpq`.
pub fn spelled(root: &Path, path: &str) -> String {
    at(root, path).strip_prefix(root).map_or_else(
        |_| path.to_owned(),
        |there| {
            there
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        },
    )
}

/// `mods` as they go in the client folder at `root` (see [`Mod::placed`]),
/// but those that can't.
fn placed_all(mods: &[&Mod], root: &Path) -> Vec<Mod> {
    mods.iter()
        .filter_map(|game_mod| game_mod.placed_in(root))
        .collect()
}

/// Whether the client folder at `root` holds `game_mod`: its marker, or one
/// of its releases' files with that release's contents, or several of them
/// whatever their contents. A lone file that matches no release may be
/// another mod's: `version.dll` is a common name. What it writes as it
/// runs doesn't count: a log outlives it. Neither do the paths it shares
/// with its `alternatives`, which tell that one of them is there, not which.
fn present(root: &Path, game_mod: &Mod, alternatives: &[&Mod]) -> bool {
    if game_mod.marked(root) {
        return true;
    }

    let shared: BTreeSet<String> = alternatives
        .iter()
        .flat_map(|other| other.paths())
        .map(str::to_ascii_lowercase)
        .collect();
    let there: Vec<&str> = game_mod
        .paths()
        .into_iter()
        .filter(|path| !shared.contains(&path.to_ascii_lowercase()))
        .filter(|path| at(root, path).is_file())
        .collect();

    match there.as_slice() {
        [] => false,
        [path] => {
            fs::read(at(root, path)).is_ok_and(|bytes| game_mod.released(path, &sha256(&bytes)))
        }
        _ => true,
    }
}

/// Whether the file at `path` in the client folder at `root`, of contents
/// `sum`, is `game_mod`'s: one of the release its marker names, or with the
/// contents one of its releases has there.
fn owns(root: &Path, game_mod: &Mod, path: &str, sum: &str) -> bool {
    game_mod.released(path, sum)
        || game_mod
            .marker(root)
            .and_then(|marker| game_mod.release(&marker.version))
            .is_some_and(|release| release.files.iter().any(|(file, _)| file == path))
}

/// How a look at a client folder goes about its files.
struct Looking<'a> {
    /// Whether to read every file, whatever its stamp, instead of only
    /// those that no longer have the one noted.
    thorough: bool,
    /// Whether to note the stamps of what it finds as released.
    note: bool,
    /// Hears how many bytes each file read was.
    read: &'a mut dyn FnMut(u64),
}

/// What reading the files of a release found.
#[derive(Default)]
struct Comparison {
    /// Those that are missing.
    missing: Vec<String>,
    /// Those that differ.
    changed: Vec<String>,
    /// The stamps of those found as released.
    stamps: BTreeMap<String, Stamp>,
}

impl Comparison {
    /// Whether every file is there as released.
    fn whole(&self) -> bool {
        self.missing.is_empty() && self.changed.is_empty()
    }
}

/// The files of `release` in the client folder at `root` that are missing,
/// and those that differ, but the player's; and the stamps of those found
/// as released. A file whose stamp is the one in `trusted` is as released
/// and not read.
fn compare(
    root: &Path,
    game_mod: &Mod,
    release: &Release,
    trusted: &BTreeMap<String, Stamp>,
    read: &mut dyn FnMut(u64),
) -> Comparison {
    let mut found = Comparison::default();

    for (path, sum) in &release.files {
        if game_mod.is_setting(path) {
            continue;
        }

        let there = at(root, path);
        // Taken before reading: a file written meanwhile is read again.
        let stamp = fs::metadata(&there)
            .ok()
            .filter(fs::Metadata::is_file)
            .as_ref()
            .and_then(Stamp::of);
        if let Some(stamp) = stamp
            && trusted.get(path) == Some(&stamp)
        {
            let _ = found.stamps.insert(path.clone(), stamp);
            continue;
        }

        match fs::read(&there) {
            Err(_) => found.missing.push(path.clone()),
            Ok(bytes) => {
                read(bytes.len() as u64);
                if sha256(&bytes) != *sum {
                    found.changed.push(path.clone());
                } else if let Some(stamp) = stamp {
                    let _ = found.stamps.insert(path.clone(), stamp);
                }
            }
        }
    }

    found
}

/// What the client folder at `root` holds of `game_mod`, if anything (see
/// [`present`], which leaves out the paths it shares with its
/// `alternatives`): checked against the release its marker names, or else
/// [`Problem::Unchecked`].
pub fn status(root: &Path, game_mod: &Mod, alternatives: &[&Mod]) -> Option<Status> {
    let game_mod = game_mod.placed_in(root)?;
    let alternatives = placed_all(alternatives, root);

    status_of(
        root,
        &game_mod,
        &alternatives.iter().collect::<Vec<_>>(),
        &mut Looking {
            thorough: false,
            note: false,
            read: &mut |_| {},
        },
    )
}

/// [`status`], of `game_mod` and its `alternatives` placed in the client
/// folder at `root`, `looking` as it says.
fn status_of(
    root: &Path,
    game_mod: &Mod,
    alternatives: &[&Mod],
    looking: &mut Looking,
) -> Option<Status> {
    if !present(root, game_mod, alternatives) {
        return None;
    }

    let Some(marker) = game_mod.marker(root) else {
        return Some(Status {
            release: None,
            problems: vec![Problem::Unchecked],
        });
    };

    let mut problems = Vec::new();

    match game_mod.release(&marker.version) {
        None => problems.push(Problem::Unlisted(marker.version.clone())),
        Some(release) => {
            let no_stamps = BTreeMap::new();
            let found = compare(
                root,
                game_mod,
                release,
                if looking.thorough {
                    &no_stamps
                } else {
                    &marker.files
                },
                looking.read,
            );
            if looking.note {
                game_mod.stamp(root, &marker, found.stamps);
            }
            if !found.missing.is_empty() {
                problems.push(Problem::Missing(found.missing));
            }
            if !found.changed.is_empty() {
                problems.push(Problem::Changed(found.changed));
            }

            let not_set_up: Vec<String> = release
                .setup
                .iter()
                .map(|step| step.creates.clone())
                .filter(|creates| !marker.set_up || !at(root, creates).exists())
                .collect();
            if !not_set_up.is_empty() {
                problems.push(Problem::NotSetUp(not_set_up));
            }
        }
    }

    let config: Vec<(String, String, String)> = game_mod
        .config
        .iter()
        .filter_map(|(name, needed)| {
            let set = config_setting(root, name)?;
            (!set.eq_ignore_ascii_case(needed)).then(|| (name.clone(), set, needed.clone()))
        })
        .collect();
    if !config.is_empty() {
        problems.push(Problem::Config(config));
    }

    Some(Status {
        release: Some(marker.version),
        problems,
    })
}

/// The paths both `a` and `b` may put in a client folder, as `a` spells
/// them: the two can't both be in one. Windows, and Wine, find a file
/// whatever the case of its name, so case doesn't count.
pub fn conflicts(a: &Mod, b: &Mod) -> Vec<String> {
    let theirs: BTreeSet<String> = b
        .claimed()
        .into_iter()
        .map(str::to_ascii_lowercase)
        .collect();

    a.claimed()
        .into_iter()
        .filter(|path| theirs.contains(&path.to_ascii_lowercase()))
        .map(str::to_owned)
        .collect()
}

/// The files of `release` in the client folder at `root` that belong to
/// something else, as spelled there, which installing it would replace (or
/// back up: see [`install`]): not the player's,
/// with the contents of none of `game_mod`'s releases, at a path that isn't
/// the mod's own there, and none of its `alternatives`', which installing it
/// takes the place of. Its own are the files of the release its marker
/// names, or all its paths when that release is no longer listed (a repair
/// takes those for its own files, broken); without a marker it has none: a
/// copy that matches no release isn't known to be the mod's.
pub fn foreign(
    root: &Path,
    game_mod: &Mod,
    release: &Release,
    alternatives: &[&Mod],
) -> Vec<String> {
    let (locale, locales) = languages(root);
    // Where it doesn't come in the client's language, it goes in another.
    let instead = game_mod
        .marker(root)
        .and_then(|marker| marker.instead)
        .or_else(|| fallback(game_mod));
    let (Some(game_mod), Some(release)) = (
        game_mod.placed(locale, &locales, instead),
        release.placed(locale, &locales, instead),
    ) else {
        return Vec::new();
    };
    let alternatives = placed_all(alternatives, root);

    foreign_of(
        root,
        &game_mod,
        &release,
        &alternatives.iter().collect::<Vec<_>>(),
    )
}

/// [`foreign`], of `game_mod`, `release` and the `alternatives` placed in
/// the client folder at `root`.
fn foreign_of(
    root: &Path,
    game_mod: &Mod,
    release: &Release,
    alternatives: &[&Mod],
) -> Vec<String> {
    let own: BTreeSet<&str> = match game_mod.marker(root) {
        Some(marker) => match game_mod.release(&marker.version) {
            Some(held) => held.files.iter().map(|(path, _)| path.as_str()).collect(),
            None => game_mod.paths(),
        },
        None => BTreeSet::new(),
    };

    release
        .files
        .iter()
        .map(|(path, _)| path)
        .filter(|path| !game_mod.is_setting(path) && !own.contains(path.as_str()))
        .filter(|path| {
            fs::read(at(root, path)).is_ok_and(|bytes| {
                let sum = sha256(&bytes);
                !game_mod.released(path, &sum)
                    && !alternatives
                        .iter()
                        .any(|other| owns(root, other, path, &sum))
            })
        })
        .map(|path| spelled(root, path))
        .collect()
}

/// Checks what the client folder at `root` holds of `game_mod` against its
/// releases, when no marker says which it is: the newest whose files are
/// all there as released is recorded in its marker, set up if what its
/// setup writes is there. Not when an `alternative` is marked, or has as many
/// files or more there, whole: editions that share files are told apart by
/// the one with the most, and when they tie, neither is known. Then tells
/// what the folder holds (see [`status`], which leaves out what it shares
/// with its `alternatives`), which is
/// [`Problem::Unknown`] when it matches none: a copy installed by hand,
/// with the version its files say and the DLLs it has beside `Wow.exe`.
///
/// The files of the release its marker names are read only if they no longer
/// have the size and modification time noted when they were last found as
/// released, which this notes anew: [`verify`] reads them all.
pub fn check(root: &Path, game_mod: &Mod, alternatives: &[&Mod]) -> Result<Option<Status>, Error> {
    look_at(
        root,
        game_mod,
        alternatives,
        &mut Looking {
            thorough: false,
            note: true,
            read: &mut |_| {},
        },
    )
}

/// [`check`], but reading every file of the release the marker names,
/// whatever its stamp: a byte changed since is found, and the stamps are
/// noted anew. `read` hears how many bytes each file read was.
pub fn verify(
    root: &Path,
    game_mod: &Mod,
    alternatives: &[&Mod],
    read: &mut dyn FnMut(u64),
) -> Result<Option<Status>, Error> {
    look_at(
        root,
        game_mod,
        alternatives,
        &mut Looking {
            thorough: true,
            note: true,
            read,
        },
    )
}

/// How many bytes the files of the release `game_mod`'s marker names in the
/// client folder at `root` take there: what [`verify`] reads, give or take
/// what it doesn't find. Only looks at the folder, reading nothing.
pub fn weight(root: &Path, game_mod: &Mod) -> u64 {
    let Some(game_mod) = game_mod.placed_in(root) else {
        return 0;
    };
    let Some(release) = game_mod
        .marker(root)
        .and_then(|marker| game_mod.release(&marker.version).cloned())
    else {
        return 0;
    };

    release
        .files
        .iter()
        .filter(|(path, _)| !game_mod.is_setting(path))
        .filter_map(|(path, _)| fs::metadata(at(root, path)).ok())
        .filter(fs::Metadata::is_file)
        .map(|metadata| metadata.len())
        .sum()
}

/// [`check`] of `game_mod` and its `alternatives`, `looking` as it says.
fn look_at(
    root: &Path,
    game_mod: &Mod,
    alternatives: &[&Mod],
    looking: &mut Looking,
) -> Result<Option<Status>, Error> {
    let Some(game_mod) = game_mod.placed_in(root) else {
        return Ok(None);
    };
    let game_mod = &game_mod;
    let alternatives = placed_all(alternatives, root);
    let alternatives: Vec<&Mod> = alternatives.iter().collect();

    if game_mod.marker(root).is_none() {
        let whole = |game_mod: &Mod, release: &Release| {
            compare(root, game_mod, release, &BTreeMap::new(), &mut |_| {})
        };
        let size = |game_mod: &Mod, release: &Release| {
            release
                .files
                .iter()
                .filter(|(path, _)| !game_mod.is_setting(path))
                .count()
        };
        // Nothing is recognized beside an edition marked, which is not
        // worth reading the files for.
        let recognized = if alternatives.iter().any(|other| other.marked(root)) {
            None
        } else {
            game_mod.releases.iter().find_map(|release| {
                let found = whole(game_mod, release);
                found.whole().then_some((release, found.stamps))
            })
        };
        // Editions of one thing that share files: the one with the most
        // files there is the one there, and when they tie, neither is known.
        let taken = recognized.as_ref().is_some_and(|(release, _)| {
            let wanted = size(game_mod, release);
            alternatives.iter().any(|other| {
                other
                    .releases
                    .iter()
                    .filter(|held| size(other, held) >= wanted)
                    .any(|held| whole(other, held).whole())
            })
        });

        match recognized.filter(|_| !taken) {
            Some((release, stamps)) => {
                let set_up = release
                    .setup
                    .iter()
                    .all(|step| at(root, &step.creates).exists());
                game_mod.mark(root, release, set_up, None, stamps)?;
                // Every file was just read.
                looking.thorough = false;
            }
            None => {
                return Ok(status_of(root, game_mod, &alternatives, looking).map(|_| {
                    let mut natives: Vec<String> = game_mod
                        .releases
                        .iter()
                        .flat_map(Release::natives)
                        .filter(|dll| at(root, &format!("{dll}.dll")).is_file())
                        .collect();
                    natives.sort_unstable();
                    natives.dedup();

                    Status {
                        release: None,
                        problems: vec![Problem::Unknown {
                            version: manual_version(root, game_mod),
                            natives,
                        }],
                    }
                }));
            }
        }
    }

    Ok(status_of(root, game_mod, &alternatives, looking))
}

/// The suffix of a file backed up: the game doesn't load it.
pub const BACKUP: &str = ".backup";

/// Backs up the files at `paths` in `installation`'s folder: each renamed
/// to end in [`BACKUP`], which the game doesn't load, so renaming it back
/// restores it. A backup made before stays: the new one ends in `.backup-1`
/// then, or `.backup-2`, and so on.
fn back_up(installation: &Installation, paths: &[String]) -> Result<(), Error> {
    for path in paths {
        let file = at(installation.root(), path);
        let folder = file.parent().unwrap_or(installation.root());
        let name = file.file_name().unwrap_or_default().to_string_lossy();

        // One backed up before, whatever its case, as the game finds it.
        let mut backup = child(folder, &format!("{name}{BACKUP}"));
        let mut tries = 0;
        while backup.exists() {
            tries += 1;
            backup = child(folder, &format!("{name}{BACKUP}-{tries}"));
        }

        fs::rename(&file, &backup).map_err(io_at(&file))?;
    }

    Ok(())
}

/// Whether `game_mod` suits the client folder at `root`: it has every file
/// the mod requires, and not every file it excludes, whatever their case.
/// One with some of those is offered it, with them in conflict (see
/// [`excluded`]). A mod whose paths name a language suits none without
/// language data. One that doesn't come in the client's languages suits it
/// still, in another (see [`lacking`]).
pub fn fits(root: &Path, game_mod: &Mod) -> bool {
    let Some(game_mod) = game_mod.placed_in(root) else {
        return false;
    };

    game_mod
        .requires
        .iter()
        .all(|path| at(root, path).is_file())
        && (game_mod.excludes.is_empty()
            || !game_mod.excludes.iter().all(|path| at(root, path).exists()))
}

/// The files in the client folder at `root`, as spelled there, in conflict
/// with `game_mod`, whose `alternatives` it takes the place of: those it
/// excludes, when the folder has some but not all (it suits the mod, see
/// [`fits`], but the mod isn't made for them: half an HD model pack is no
/// pack, nor no pack), and those it can't be beside, but its alternatives'.
/// Installing it over them backs them up (see [`install`]).
pub fn excluded(root: &Path, game_mod: &Mod, alternatives: &[&Mod]) -> Vec<String> {
    let Some(game_mod) = game_mod.placed_in(root) else {
        return Vec::new();
    };
    let alternatives = placed_all(alternatives, root);
    let there = |paths: &[String]| -> Vec<String> {
        paths
            .iter()
            .filter(|path| at(root, path).is_file())
            .cloned()
            .collect()
    };

    let mut excluded = there(&game_mod.excludes);
    if excluded.len() == game_mod.excludes.len() {
        excluded.clear();
    }
    let beside = there(&game_mod.conflicts).into_iter().filter(|path| {
        fs::read(at(root, path)).is_ok_and(|bytes| {
            let sum = sha256(&bytes);
            !alternatives
                .iter()
                .any(|other| owns(root, other, path, &sum))
        })
    });

    let mut conflicting: Vec<String> = excluded
        .into_iter()
        .chain(beside)
        .map(|path| spelled(root, &path))
        .collect();
    conflicting.dedup();

    conflicting
}

/// The client's languages, in the folder at `root`, that the newest
/// release of `game_mod` doesn't come in, when its files differ by
/// language: those get the files of another (see [`Mod::placed`]), which
/// the player chooses, or [`fallback`].
pub fn lacking(root: &Path, game_mod: &Mod) -> Vec<Locale> {
    let Some(newest) = game_mod
        .newest()
        .filter(|newest| !newest.languages.is_empty())
    else {
        return Vec::new();
    };
    let (_, locales) = languages(root);

    locales
        .into_iter()
        .filter(|locale| !newest.languages.contains_key(locale))
        .collect()
}

/// The languages `game_mod`'s newest release comes in, when its files
/// differ by language.
pub fn editions(game_mod: &Mod) -> Vec<Locale> {
    game_mod
        .newest()
        .map(|newest| newest.languages.keys().copied().collect())
        .unwrap_or_default()
}

/// The language of `game_mod`'s newest release that goes in the client's
/// languages it doesn't come in, unless the player chooses another:
/// English (`enUS`) if it comes in it, or else the first.
pub fn fallback(game_mod: &Mod) -> Option<Locale> {
    let editions = editions(game_mod);

    editions
        .contains(&Locale::EnUs)
        .then_some(Locale::EnUs)
        .or_else(|| editions.first().copied())
}

/// The version the files of `game_mod` in the client folder at `root` say
/// they are, if they say: after the text its [`Mod::version_in`] names, or
/// else in the version resource of the first of its files that has one.
fn manual_version(root: &Path, game_mod: &Mod) -> Option<String> {
    if let Some(place) = &game_mod.version_in {
        let bytes = fs::read(at(root, &place.file)).ok()?;
        let after = place.after.as_bytes();
        let at = bytes
            .windows(after.len())
            .position(|window| window == after)?
            + after.len();
        let version: String = bytes
            .get(at..)?
            .iter()
            .take_while(|byte| **byte != 0)
            .take(32)
            .map(|byte| char::from(*byte))
            .collect();

        return (!version.is_empty() && version.chars().all(|c| c.is_ascii_graphic()))
            .then_some(version);
    }

    game_mod
        .paths()
        .into_iter()
        .filter_map(|path| fs::read(at(root, path)).ok())
        .find_map(|bytes| crate::version::product_version(&bytes))
}

/// Installs `release` of `game_mod`, downloaded whole as `download`, into
/// `installation`'s folder (see [`install_downloaded`]).
pub fn install(
    installation: &Installation,
    game_mod: &Mod,
    release: &Release,
    download: &[u8],
    alternatives: &[&Mod],
    over: bool,
) -> Result<(), Error> {
    let downloads = BTreeMap::from([(release.url.clone(), download.to_vec())]);

    install_downloaded(
        installation,
        game_mod,
        release,
        &downloads,
        alternatives,
        over,
        None,
    )
}

/// Installs `release` of `game_mod`, its `downloads` by address (see
/// [`Release::downloads`]), into `installation`'s folder. Every file is
/// checked against the release first, so nothing changes unless all of
/// them are right. Then its `alternatives` there go, and what was there of
/// the mod, but the player's files (see [`uninstall`]); the release's files
/// go in place but the player's ones already there, the marker records the
/// release, and `WTF/Config.wtf` gets what the mod needs. Its setup isn't
/// run: until [`finish_setup`] says it ran to the end, the marker says it
/// didn't, when there is one. Files of something else where the release
/// puts its own (see [`foreign`]) keep it out, unless it goes `over` them:
/// then they are backed up (see [`back_up`]), and so are those in conflict
/// with it (see [`excluded`]). Everything else stays. Where its files
/// differ by language, the client's languages it doesn't come in get those
/// of `instead`, or of the language its marker names, or none.
#[allow(clippy::too_many_arguments)]
pub fn install_downloaded(
    installation: &Installation,
    game_mod: &Mod,
    release: &Release,
    downloads: &BTreeMap<String, Vec<u8>>,
    alternatives: &[&Mod],
    over: bool,
    instead: Option<Locale>,
) -> Result<(), Error> {
    let root = installation.root();
    let unmade = if over {
        excluded(root, game_mod, alternatives)
    } else {
        Vec::new()
    };
    let instead = instead_in(root, game_mod, instead);
    let (locale, locales) = languages(root);
    let (Some(game_mod), Some(release)) = (
        game_mod.placed(locale, &locales, instead),
        release.placed(locale, &locales, instead),
    ) else {
        return Err(Error::NoLanguage);
    };
    let (game_mod, release) = (&game_mod, &release);
    let alternatives = placed_all(alternatives, root);
    let alternatives: Vec<&Mod> = alternatives.iter().collect();

    let plan = Plan::of(root, game_mod, release, &alternatives, unmade);
    if !plan.foreign.is_empty() && !over {
        return Err(Error::Foreign(plan.foreign));
    }

    // Beside the files it replaces, so they move in by renaming. Left behind
    // by an install cut short, it is swept later (see `Installation::scan`).
    let staging = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(root)
        .map_err(io_at(root))?;
    // Where each file of the release is in it, each download in a folder of
    // its own.
    let mut staged: BTreeMap<&str, PathBuf> = BTreeMap::new();
    for (index, (url, paths)) in release.downloads().into_iter().enumerate() {
        let download = downloads
            .get(&url)
            .ok_or_else(|| Error::Undownloaded(url.clone()))?;
        let folder = staging.path().join(index.to_string());
        let from = folder.join(release.inside.as_deref().unwrap_or(""));

        fs::create_dir_all(&folder).map_err(io_at(&folder))?;
        // Not an archive: one file, each of its paths a copy of it (a
        // language's patch in another's folder too).
        let single = match archive::Format::detect(download) {
            Some(_) => {
                archive::extract(download, &folder)?;
                None
            }
            None if !paths.is_empty() => {
                let file = folder.join("download");
                fs::write(&file, download).map_err(io_at(&file))?;
                Some(file)
            }
            None => return Err(Error::UnknownArchive),
        };

        for (path, _) in &release.files {
            if paths.contains(path) {
                let file = single
                    .clone()
                    .unwrap_or_else(|| from.join(release.source(path)));
                let _ = staged.insert(path, file);
            }
        }
    }

    for (path, sum) in &release.files {
        let source = release.source(path);
        let bytes = staged
            .get(path.as_str())
            .and_then(|file| fs::read(file).ok())
            .ok_or_else(|| Error::NotInRelease(source.to_owned()))?;
        if sha256(&bytes) != *sum {
            return Err(Error::Checksum(path.clone()));
        }
    }

    // What is something else's is backed up, then what it takes the place
    // of goes, then whatever was there of it, another release's or a broken
    // install's.
    back_up(installation, &plan.backups)?;
    for (other, paths) in &plan.removals {
        remove_all(installation, other, paths)?;
    }

    // A file of the archive that goes to several places is copied to each
    // but the last, where it moves.
    let mut uses: BTreeMap<&Path, usize> = BTreeMap::new();
    for file in staged.values() {
        *uses.entry(file).or_default() += 1;
    }

    for (path, _) in &release.files {
        let Some(file) = staged.get(path.as_str()) else {
            continue;
        };
        let left = uses.get_mut(file.as_path()).map_or(0, |left| {
            *left -= 1;
            *left
        });
        let target = at(root, path);
        if game_mod.is_setting(path) && target.exists() {
            continue;
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(io_at(parent))?;
        }
        let placed = if left > 0 {
            fs::copy(file, &target).map(drop)
        } else {
            fs::rename(file, &target)
        };
        placed.map_err(io_at(&target))?;
    }
    // Every file was just checked against the release: noted as found, the
    // next look reads none of them.
    let mut stamps = BTreeMap::new();
    for (path, _) in &release.files {
        if game_mod.is_setting(path) {
            continue;
        }
        if let Ok(metadata) = fs::metadata(at(root, path))
            && metadata.is_file()
            && let Some(stamp) = Stamp::of(&metadata)
        {
            let _ = stamps.insert(path.clone(), stamp);
        }
    }
    game_mod.mark(root, release, release.setup.is_empty(), instead, stamps)?;

    for (name, value) in &game_mod.config {
        if config_setting(root, name).is_none_or(|set| !set.eq_ignore_ascii_case(value)) {
            installation.set(name, value)?;
        }
    }

    Ok(())
}

/// Records in `installation`'s folder that the setup of `release`, the
/// release of `game_mod` there, ran to the end.
pub fn finish_setup(
    installation: &Installation,
    game_mod: &Mod,
    release: &Release,
) -> Result<(), Error> {
    let root = installation.root();
    let marker = game_mod.marker(root);
    let instead = marker.as_ref().and_then(|marker| marker.instead);
    // The files of the same release are as they were found.
    let files = marker
        .filter(|marker| marker.version == release.version)
        .map(|marker| marker.files)
        .unwrap_or_default();

    game_mod.mark(root, release, true, instead, files)
}

/// Removes `game_mod` from `installation`'s folder: the files of the release
/// its marker names, the files of any release that hold that release's
/// contents, what the mod and its setups write, its marker, and the
/// player's files too unless `keep_settings`. A file of another content
/// with no marker to say it's the mod's stays: it may be another mod's.
/// Folders left empty go.
pub fn uninstall(
    installation: &Installation,
    game_mod: &Mod,
    keep_settings: bool,
) -> Result<(), Error> {
    let root = installation.root();

    match game_mod.placed_in(root) {
        Some(game_mod) => uninstall_of(installation, &game_mod, keep_settings),
        // Where its files go is unknown: only its marker can go.
        None => game_mod.unmark(root),
    }
}

/// [`uninstall`], of `game_mod` placed in `installation`'s folder.
fn uninstall_of(
    installation: &Installation,
    game_mod: &Mod,
    keep_settings: bool,
) -> Result<(), Error> {
    let paths = removal_of(installation.root(), game_mod, keep_settings);

    remove_all(installation, game_mod, &paths)
}

/// The paths, as `game_mod` spells them, that [`uninstall_of`] removes from
/// the client folder at `root` (there or not): its files of the release its
/// marker names or with the contents of one, and what it and its setups
/// write, and the player's files too unless `keep_settings`.
fn removal_of(root: &Path, game_mod: &Mod, keep_settings: bool) -> Vec<String> {
    let marked: BTreeSet<&str> = game_mod
        .marker(root)
        .and_then(|marker| game_mod.release(&marker.version))
        .map(|release| {
            release
                .files
                .iter()
                .map(|(path, _)| path.as_str())
                .filter(|path| !game_mod.is_setting(path))
                .collect()
        })
        .unwrap_or_default();

    let ours = game_mod.paths().into_iter().filter(|path| {
        marked.contains(path)
            || fs::read(at(root, path)).is_ok_and(|bytes| game_mod.released(path, &sha256(&bytes)))
    });
    let settings = game_mod
        .settings
        .iter()
        .map(String::as_str)
        .filter(|_| !keep_settings);

    ours.chain(game_mod.written())
        .chain(settings)
        .map(str::to_owned)
        .collect()
}

/// Removes the `paths` of `game_mod` (see [`removal_of`]) from
/// `installation`'s folder, and its marker, and the folders left empty.
fn remove_all(installation: &Installation, game_mod: &Mod, paths: &[String]) -> Result<(), Error> {
    let root = installation.root();

    for path in paths {
        remove(&at(root, path))?;
    }
    game_mod.unmark(root)?;

    for path in paths {
        prune(root, path);
    }

    Ok(())
}

/// What installing a release does to the files already in a client folder,
/// listed before it does anything: [`install_downloaded`] follows it, and
/// [`change`] tells it.
struct Plan<'a> {
    /// The files of something else where the release puts its own (see
    /// [`foreign_of`]).
    foreign: Vec<String>,
    /// Every file it backs up: those, and those in conflict with it.
    backups: Vec<String>,
    /// Each mod it takes the place of (its alternatives there, then the mod
    /// itself) with the paths it removes of it (see [`removal_of`]).
    removals: Vec<(&'a Mod, Vec<String>)>,
}

impl<'a> Plan<'a> {
    /// The plan to install `release` of `game_mod` over the client folder at
    /// `root`, where `unmade` are the files in conflict ([`excluded`]),
    /// everything placed (see [`Mod::placed`]).
    fn of(
        root: &Path,
        game_mod: &'a Mod,
        release: &Release,
        alternatives: &[&'a Mod],
        unmade: Vec<String>,
    ) -> Self {
        let foreign = foreign_of(root, game_mod, release, alternatives);
        let backups = [foreign.clone(), unmade].concat();
        let removals = alternatives
            .iter()
            .copied()
            .filter(|other| present(root, other, &[]))
            .chain([game_mod])
            .map(|other| (other, removal_of(root, other, true)))
            .collect();

        Self {
            foreign,
            backups,
            removals,
        }
    }
}

/// What installing `release` of `game_mod` over the client folder at `root`,
/// with its `alternatives`, does to the game's files, going over whatever
/// is in the way (see [`install_downloaded`], which does exactly this): the
/// files placed, and those gone, which are the ones it removes, as spelled
/// on disk, and the ones it backs up. The player's files stay. Nothing
/// where [`install_downloaded`] fails for lack of a language. The languages
/// the client lacks get those of `instead`, as there.
pub fn change(
    root: &Path,
    game_mod: &Mod,
    release: &Release,
    alternatives: &[&Mod],
    instead: Option<Locale>,
) -> Change {
    let unmade = excluded(root, game_mod, alternatives);
    let instead = instead_in(root, game_mod, instead);
    let (locale, locales) = languages(root);
    let (Some(game_mod), Some(release)) = (
        game_mod.placed(locale, &locales, instead),
        release.placed(locale, &locales, instead),
    ) else {
        return Change::default();
    };
    let alternatives = placed_all(alternatives, root);
    let alternatives: Vec<&Mod> = alternatives.iter().collect();
    let plan = Plan::of(root, &game_mod, &release, &alternatives, unmade);

    let mut removes: Vec<String> = Vec::new();
    let removed = plan
        .removals
        .iter()
        .flat_map(|(_, paths)| paths)
        .filter(|path| at(root, path).exists())
        .map(|path| spelled(root, path));
    for path in plan.backups.iter().cloned().chain(removed) {
        if !removes.contains(&path) {
            removes.push(path);
        }
    }

    // A file in the way is gone by then, so the new one has the release's
    // spelling of its name, in the folders as they are.
    let adds = release
        .files
        .iter()
        .filter(|(path, _)| !(game_mod.is_setting(path) && at(root, path).exists()))
        .map(|(path, sum)| {
            let there = spelled(root, path);
            let spelled = match (removes.contains(&there), path.rsplit_once('/')) {
                (true, Some((_, name))) => match there.rsplit_once('/') {
                    Some((folder, _)) => format!("{folder}/{name}"),
                    None => name.to_owned(),
                },
                (true, None) => path.clone(),
                (false, _) => there,
            };

            (spelled, sum.clone())
        })
        .collect();

    Change { adds, removes }
}

/// Whether `path` names an archive of the game's (`.mpq`, in any case).
pub fn is_archive(path: &str) -> bool {
    path.len() > 4
        && path
            .get(path.len() - 4..)
            .is_some_and(|end| end.eq_ignore_ascii_case(".mpq"))
}

/// The language `release` is installed with: `instead`, or the one the
/// mod's marker names, when it stands in for one of the client's languages.
fn instead_in(root: &Path, game_mod: &Mod, instead: Option<Locale>) -> Option<Locale> {
    instead
        .or_else(|| game_mod.marker(root).and_then(|marker| marker.instead))
        // Noted only when it stands in for one of the client's languages.
        .filter(|_| !lacking(root, game_mod).is_empty())
}

/// Reads the archives (`.mpq`) of `release` of `game_mod` as installing it
/// in the client folder at `root` would place them, from its `downloads` by
/// address (see [`Release::downloads`]), and returns what each says (see
/// [`Facts`]) by SHA-256, keeping it in `cache` so a release's archive is
/// read once whatever the client. Every archive is checked against the
/// release first, as [`install_downloaded`] does. One that can't be read
/// provides and needs nothing, with a line in the log. An archive of the
/// downloads is unpacked beside `scratch`, the launcher's own folder, or in
/// the system's when there is none, and nothing is touched in the client.
pub fn read_archives(
    root: &Path,
    game_mod: &Mod,
    release: &Release,
    downloads: &BTreeMap<String, Vec<u8>>,
    instead: Option<Locale>,
    scratch: Option<&Path>,
    cache: &Cache,
) -> Result<BTreeMap<String, Facts>, Error> {
    let instead = instead_in(root, game_mod, instead);
    let (locale, locales) = languages(root);
    let release = release
        .placed(locale, &locales, instead)
        .ok_or(Error::NoLanguage)?;
    let mut read = BTreeMap::new();
    if !release.files.iter().any(|(path, _)| is_archive(path)) {
        return Ok(read);
    }

    let staging = match scratch {
        Some(folder) => {
            fs::create_dir_all(folder).map_err(io_at(folder))?;
            tempfile::Builder::new()
                .prefix(STAGING_PREFIX)
                .tempdir_in(folder)
                .map_err(io_at(folder))?
        }
        None => tempfile::Builder::new()
            .prefix(STAGING_PREFIX)
            .tempdir()
            .map_err(io_at(&std::env::temp_dir()))?,
    };

    for (index, (url, paths)) in release.downloads().into_iter().enumerate() {
        let wanted: Vec<&(String, String)> = release
            .files
            .iter()
            .filter(|(path, _)| is_archive(path) && paths.contains(path))
            .collect();
        if wanted.is_empty() {
            continue;
        }
        let download = downloads.get(&url).ok_or(Error::Undownloaded(url))?;
        let folder = staging.path().join(index.to_string());

        // Not an archive: the one file, each of its paths a copy of it.
        let unpacked = match archive::Format::detect(download) {
            Some(_) => {
                fs::create_dir_all(&folder).map_err(io_at(&folder))?;
                archive::extract(download, &folder)?;
                Some(folder.join(release.inside.as_deref().unwrap_or("")))
            }
            None if !paths.is_empty() => None,
            None => return Err(Error::UnknownArchive),
        };

        for (path, sum) in wanted {
            if read.contains_key(sum) {
                continue;
            }
            let facts = match &unpacked {
                Some(from) => {
                    let file = from.join(release.source(path));
                    let mut hash = Sha256::new();
                    let mut opened = fs::File::open(&file)
                        .map_err(|_| Error::NotInRelease(release.source(path).to_owned()))?;
                    let _ = io::copy(&mut opened, &mut hash).map_err(io_at(&file))?;
                    if format!("{:x}", hash.finalize()) != *sum {
                        return Err(Error::Checksum(path.clone()));
                    }

                    cache.of_release(sum, || Ok(facts_of(&file, Opened::open(&file), path)))?
                }
                None => {
                    if sha256(download) != *sum {
                        return Err(Error::Checksum(path.clone()));
                    }
                    cache.of_release(sum, || {
                        let opened = crate::mpq::Archive::from_reader(
                            io::Cursor::new(download.as_slice()),
                            path.as_str(),
                        )
                        .map(Opened::new);

                        Ok(facts_of(Path::new(path), opened, path))
                    })?
                }
            };

            let _ = read.insert(sum.clone(), facts);
        }
    }

    Ok(read)
}

/// What the archive `opened`, at `file`, says: nothing if it can't be read,
/// with a line in the log.
fn facts_of<R: io::Read + io::Seek>(
    file: &Path,
    opened: Result<Opened<R>, Error>,
    path: &str,
) -> Facts {
    match opened {
        Ok(opened) => Facts::of(&opened, &[]),
        Err(error) => {
            log::debug!("{path} ({}) can't be read: {error}", file.display());

            Facts {
                problems: vec![format!("can't be read: {error}")],
                ..Facts::default()
            }
        }
    }
}

/// The archives (`.mpq`) of the release `game_mod` is installed with in the
/// client folder at `root`, that are there: path as spelled on disk, and the
/// SHA-256 of the release's. Nothing when its marker names no listed release.
pub fn installed_archives(root: &Path, game_mod: &Mod) -> Vec<(String, String)> {
    let Some(marker) = game_mod.marker(root) else {
        return Vec::new();
    };
    let Some(placed) = game_mod.placed_in(root) else {
        return Vec::new();
    };
    let Some(release) = placed.release(&marker.version) else {
        return Vec::new();
    };

    release
        .files
        .iter()
        .filter(|(path, _)| is_archive(path) && at(root, path).is_file())
        .map(|(path, sum)| (spelled(root, path), sum.clone()))
        .collect()
}

/// Removes the file or folder at `path`, if there is one.
fn remove(path: &Path) -> Result<(), Error> {
    let removed = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };

    match removed {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(io_at(path)(error)),
        _ => Ok(()),
    }
}

/// Removes the folders of `path`, relative to `root`, that are left empty,
/// from the innermost out.
fn prune(root: &Path, path: &str) {
    let mut folder = Path::new(path).parent();

    while let Some(relative) = folder.filter(|relative| !relative.as_os_str().is_empty()) {
        // Fails, and stops, at the first that isn't empty.
        if fs::remove_dir(root.join(relative)).is_err() {
            break;
        }
        folder = relative.parent();
    }
}

/// Whether `id` can name a marker: lowercase letters, digits and `-`.
fn valid(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Whether `path` stays inside the client folder: relative, with `/`
/// between plain folder names.
fn safe(path: &str) -> bool {
    !path.is_empty()
        && !path.contains(['\\', ':'])
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::*;

    /// A zip of `files`, with `\` between folders as Lumenveil's have.
    fn zip(files: &[(&str, &str)]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, contents) in files {
            writer
                .start_file(
                    path.replace('/', "\\"),
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
            writer.write_all(contents.as_bytes()).unwrap();
        }

        writer.finish().unwrap().into_inner()
    }

    /// A release of `version` with `files`, and its zip.
    fn release(version: &str, files: &[(&str, &str)]) -> (Release, Vec<u8>) {
        let release = Release {
            version: version.to_owned(),
            url: format!("https://example.com/Glow-{version}.zip"),
            updated: None,
            changes: None,
            files: files
                .iter()
                .map(|(path, contents)| ((*path).to_owned(), sha256(contents.as_bytes())))
                .collect(),
            from: BTreeMap::new(),
            inside: None,
            setup: vec![Step {
                run: "Glow/setup.exe".to_owned(),
                arguments: vec!["--locale".to_owned(), "{locale}".to_owned()],
                creates: "glow.bin".to_owned(),
            }],
            languages: BTreeMap::new(),
        };

        (release, zip(files))
    }

    /// Glow, a mod of two releases, the newest first.
    fn glow() -> (Mod, Vec<u8>, Vec<u8>) {
        let (new, new_zip) = release(
            "2.0",
            &[
                ("version.dll", "loader 2"),
                ("Glow.dll", "renderer 2"),
                ("Glow.ini", "Enable=1\n"),
                ("Glow/setup.exe", "setup"),
            ],
        );
        let (old, old_zip) = release(
            "1.0",
            &[
                ("version.dll", "loader 1"),
                ("Glow.dll", "renderer 1"),
                ("Glow.ini", "Enable=1\n"),
                ("Glow/setup.exe", "setup"),
                ("Glow/old.txt", "gone in 2.0"),
            ],
        );

        let game_mod = Mod {
            id: "glow".to_owned(),
            name: "Glow".to_owned(),
            description: String::new(),
            source: Source {
                name: "Glow on GitHub".to_owned(),
                page: "https://example.com/glow".to_owned(),
            },
            releases: vec![new, old],
            settings: vec!["Glow.ini".to_owned(), "Glow-spots".to_owned()],
            writes: vec!["Glow.log".to_owned()],
            config: BTreeMap::from([("gxApi".to_owned(), "d3d9".to_owned())]),
            platforms: Vec::new(),
            requires: Vec::new(),
            excludes: Vec::new(),
            conflicts: Vec::new(),
            alternatives: Vec::new(),
            version_in: None,
        };

        (game_mod, new_zip, old_zip)
    }

    fn read(installation: &Installation, path: &str) -> String {
        fs::read_to_string(installation.root().join(path)).unwrap()
    }

    #[test]
    fn a_published_list_reads_back_and_drops_what_it_cant_trust() {
        let (game_mod, _, _) = glow();
        let mut unsafe_release = game_mod.releases[1].clone();
        unsafe_release
            .files
            .push(("../Wow.exe".to_owned(), "0".repeat(64)));
        let mut foreign_run = game_mod.releases[1].clone();
        foreign_run.setup[0].run = "C:/Windows/notepad.exe".to_owned();
        let mut broken = game_mod.clone();
        broken.releases = vec![game_mod.releases[0].clone(), unsafe_release, foreign_run];
        let mut nameless = game_mod.clone();
        nameless.id = "Glow 2".to_owned();

        let mods = Mods {
            version: Mods::VERSION,
            mods: vec![broken, nameless],
        };
        let json = mods.to_json().unwrap();
        let published = Mods::parse(json.as_bytes()).unwrap();

        assert_eq!(published.mods.len(), 1, "the id can't name a marker");
        let mut newest = game_mod.releases[0].clone();
        newest.files.sort();
        assert_eq!(published.mods[0].releases, [newest]);
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value["mods"][0]["releases"][0]["files"][0],
            serde_json::json!(["Glow.dll", sha256(b"renderer 2")]),
            "files are path and checksum pairs, sorted"
        );
        assert!(matches!(
            Mods::parse(br#"{ "version": 1, "mods": [] }"#),
            Err(Error::ModsVersion(1))
        ));
        assert!(matches!(
            Mods::parse(br#"{ "version": 3, "mods": [] }"#),
            Err(Error::ModsVersion(3))
        ));
    }

    #[test]
    fn an_archive_lists_its_files_with_their_checksums() {
        let files = inspect(&zip(&[
            ("Glow/setup.exe", "setup"),
            ("Glow.dll", "renderer"),
        ]))
        .unwrap();

        assert_eq!(
            files,
            [
                ("Glow.dll".to_owned(), sha256(b"renderer")),
                ("Glow/setup.exe".to_owned(), sha256(b"setup")),
            ]
        );
    }

    #[test]
    fn a_mod_is_there_by_its_marker_or_its_files() {
        let (game_mod, _, _) = glow();
        let is_there = |files: &[(&str, &str)]| {
            let (_root, installation) = crate::testing::client_with(files);
            status(installation.root(), &game_mod, &[])
        };

        assert_eq!(is_there(&[]), None);
        assert_eq!(is_there(&[("Glow.ini", "mine")]), None, "the player's");
        assert_eq!(
            is_there(&[("version.dll", "another mod")]),
            None,
            "a common name"
        );
        assert_eq!(is_there(&[("Glow.log", "")]), None, "a log outlives it");

        let unchecked = Some(Status {
            release: None,
            problems: vec![Problem::Unchecked],
        });
        assert_eq!(is_there(&[("version.dll", "loader 1")]), unchecked);
        assert_eq!(
            is_there(&[("Glow.dll", "?"), ("version.dll", "?")]),
            unchecked
        );
    }

    #[test]
    fn a_copy_installed_by_hand_says_its_version_and_its_dlls() {
        let (mut game_mod, _, _) = glow();
        let utf16 =
            |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(u16::to_le_bytes).collect() };
        let mut resource = b"MZ renderer 3.0 ".to_vec();
        resource.extend(utf16("ProductVersion\0"));
        resource.extend([0, 0]);
        resource.extend(utf16("3.0\0"));
        let (_root, installation) = crate::testing::client_with(&[
            ("version.dll", "loader 3.0"),
            ("Glow.dll", "MZ renderer 3.0"),
        ]);
        let root = installation.root();
        fs::write(root.join("Glow.dll"), &resource).unwrap();
        let unknown = |version: Option<&str>| Problem::Unknown {
            version: version.map(str::to_owned),
            natives: vec!["Glow".to_owned(), "version".to_owned()],
        };

        // Its version resource says.
        assert_eq!(
            check(root, &game_mod, &[]).unwrap().unwrap().problems,
            [unknown(Some("3.0"))]
        );
        assert_eq!(game_mod.marker(root), None, "nothing to record");

        // Where the list says, when it does: a DLL passing for Windows' own
        // says Windows' version in its resource.
        game_mod.version_in = Some(VersionIn {
            file: "Glow.dll".to_owned(),
            after: "Glow: v".to_owned(),
        });
        assert_eq!(
            check(root, &game_mod, &[]).unwrap().unwrap().problems,
            [unknown(None)]
        );
        fs::write(root.join("Glow.dll"), b"MZ \0Game: \0Glow: v3.1-rc\0\0 ").unwrap();
        assert_eq!(
            check(root, &game_mod, &[]).unwrap().unwrap().problems,
            [unknown(Some("3.1-rc"))]
        );
    }

    #[test]
    fn checking_recognizes_the_release_there_and_records_it() {
        let (game_mod, _, _) = glow();
        let (_root, installation) = crate::testing::client_with(&[
            ("version.dll", "loader 1"),
            ("Glow.dll", "renderer 1"),
            ("Glow.ini", "Enable=0\n"),
            ("Glow/setup.exe", "setup"),
            ("Glow/old.txt", "gone in 2.0"),
        ]);
        let root = installation.root();

        assert_eq!(
            check(root, &game_mod, &[]).unwrap(),
            Some(Status {
                release: Some("1.0".to_owned()),
                problems: vec![Problem::NotSetUp(vec!["glow.bin".to_owned()])],
            }),
            "the player's settings are theirs to change"
        );
        assert_eq!(game_mod.marker(root).unwrap().version, "1.0");

        fs::write(root.join("glow.bin"), "").unwrap();
        assert_eq!(
            status(root, &game_mod, &[]).unwrap().problems,
            [Problem::NotSetUp(vec!["glow.bin".to_owned()])],
            "made after the check, it may be half written"
        );

        // Checked with what its setup writes there, it counts as set up.
        game_mod.unmark(root).unwrap();
        assert_eq!(check(root, &game_mod, &[]).unwrap().unwrap().problems, []);
        assert!(game_mod.marker(root).unwrap().set_up);

        fs::write(root.join("Glow.dll"), "patched").unwrap();
        fs::remove_file(root.join("Glow/old.txt")).unwrap();
        assert_eq!(
            status(root, &game_mod, &[]).unwrap().problems,
            [
                Problem::Missing(vec!["Glow/old.txt".to_owned()]),
                Problem::Changed(vec!["Glow.dll".to_owned()]),
            ]
        );

        // Without the marker, it matches no release.
        game_mod.unmark(root).unwrap();
        assert_eq!(
            check(root, &game_mod, &[]).unwrap().unwrap().problems,
            [Problem::Unknown {
                version: None,
                natives: vec!["Glow".to_owned(), "version".to_owned()],
            }]
        );
        assert_eq!(game_mod.marker(root), None);
    }

    /// Writes `contents` at `path`, whose modification time stays the same.
    fn rewrite(path: &Path, contents: &str) {
        let modified = fs::metadata(path).unwrap().modified().unwrap();
        fs::write(path, contents).unwrap();
        set_modified(path, modified);
    }

    /// Makes `path` last modified at `time`.
    fn set_modified(path: &Path, time: std::time::SystemTime) {
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(time)
            .unwrap();
    }

    /// Glow 2.0, as installed and set up in a new client folder.
    fn glowing() -> (tempfile::TempDir, Installation, Mod) {
        let (game_mod, _, _) = glow();
        let (root, installation) = crate::testing::client_with(&[
            ("version.dll", "loader 2"),
            ("Glow.dll", "renderer 2"),
            ("Glow/setup.exe", "setup"),
            ("glow.bin", ""),
        ]);

        (root, installation, game_mod)
    }

    #[test]
    fn a_file_found_as_released_is_not_hashed_again_until_its_stamp_changes() {
        let (_root, installation, game_mod) = glowing();
        let root = installation.root();
        let dll = root.join("Glow.dll");

        assert_eq!(check(root, &game_mod, &[]).unwrap().unwrap().problems, []);
        let marker = game_mod.marker(root).unwrap();
        assert_eq!(
            marker.files.keys().map(String::as_str).collect::<Vec<_>>(),
            ["Glow.dll", "Glow/setup.exe", "version.dll"],
            "the files found as released, but the player's"
        );

        // A byte flipped under the same size and time is not looked for.
        rewrite(&dll, "renderer 3");
        assert_eq!(check(root, &game_mod, &[]).unwrap().unwrap().problems, []);
        assert_eq!(status(root, &game_mod, &[]).unwrap().problems, []);
        assert_eq!(game_mod.marker(root).unwrap(), marker, "nothing to note");

        // Once its time changes, it is read.
        let modified = fs::metadata(&dll).unwrap().modified().unwrap();
        set_modified(&dll, modified + std::time::Duration::from_secs(2));
        let changed = vec![Problem::Changed(vec!["Glow.dll".to_owned()])];
        assert_eq!(
            status(root, &game_mod, &[]).unwrap().problems,
            changed,
            "a look that notes nothing reads it too"
        );
        assert_eq!(
            check(root, &game_mod, &[]).unwrap().unwrap().problems,
            changed
        );
        assert!(
            !game_mod
                .marker(root)
                .unwrap()
                .files
                .contains_key("Glow.dll"),
            "a file that differs is not noted"
        );

        // Its size changing is enough.
        rewrite(&dll, "renderer 2");
        assert_eq!(check(root, &game_mod, &[]).unwrap().unwrap().problems, []);
        assert!(
            game_mod
                .marker(root)
                .unwrap()
                .files
                .contains_key("Glow.dll")
        );

        fs::remove_file(&dll).unwrap();
        assert_eq!(
            check(root, &game_mod, &[]).unwrap().unwrap().problems,
            [Problem::Missing(vec!["Glow.dll".to_owned()])],
            "a missing file is missing, trusted or not"
        );
    }

    #[test]
    fn an_install_notes_its_files_so_the_next_look_reads_nothing() {
        let (game_mod, new_zip, _) = glow();
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root();
        install(
            &installation,
            &game_mod,
            &game_mod.releases[0],
            &new_zip,
            &[],
            false,
        )
        .unwrap();

        let marker = game_mod.marker(root).unwrap();
        assert_eq!(
            marker.files.keys().map(String::as_str).collect::<Vec<_>>(),
            ["Glow.dll", "Glow/setup.exe", "version.dll"],
            "the files it placed, but the player's"
        );

        // A byte flipped under the same size and time is not looked for:
        // nothing is read.
        rewrite(&root.join("Glow.dll"), "renderer 3");
        let mut read = 0;
        let placed = game_mod.placed_in(root).unwrap();
        let _ = status_of(
            root,
            &placed,
            &[],
            &mut Looking {
                thorough: false,
                note: false,
                read: &mut |bytes| read += bytes,
            },
        );
        assert_eq!(read, 0);
        assert_eq!(game_mod.marker(root).unwrap(), marker, "nothing to note");
    }

    #[test]
    fn a_marker_without_stamps_is_read_as_before() {
        let (_root, installation, game_mod) = glowing();
        let root = installation.root();
        // What 1.1.2 noted.
        fs::write(
            root.join(crate::notes::FILE),
            "[mods.glow]\nversion = \"2.0\"\nurl = \"https://example.com/Glow-2.0.zip\"\n\
             set_up = true\n",
        )
        .unwrap();

        let marker = game_mod.marker(root).unwrap();
        assert_eq!(marker.version, "2.0");
        assert!(marker.files.is_empty());

        rewrite(&root.join("Glow.dll"), "patched!!!");
        assert_eq!(
            status(root, &game_mod, &[]).unwrap().problems,
            [Problem::Changed(vec!["Glow.dll".to_owned()])],
            "every file is read"
        );
        rewrite(&root.join("Glow.dll"), "renderer 2");
        assert_eq!(check(root, &game_mod, &[]).unwrap().unwrap().problems, []);
        assert_eq!(game_mod.marker(root).unwrap().files.len(), 3, "now noted");
        assert!(game_mod.marker(root).unwrap().set_up);
    }

    #[test]
    fn verifying_hashes_everything_and_renews_the_stamps() {
        let (_root, installation, game_mod) = glowing();
        let root = installation.root();
        let dll = root.join("Glow.dll");
        let total = ("loader 2".len() + "renderer 2".len() + "setup".len()) as u64;

        assert_eq!(check(root, &game_mod, &[]).unwrap().unwrap().problems, []);
        assert_eq!(weight(root, &game_mod), total);

        let mut read = 0;
        let held = verify(root, &game_mod, &[], &mut |bytes| read += bytes);
        assert_eq!(held.unwrap().unwrap().problems, []);
        assert_eq!(read, total, "every file, though all are stamped");

        // A byte flipped under the same size and time: only verifying finds it.
        rewrite(&dll, "renderer 3");
        assert_eq!(check(root, &game_mod, &[]).unwrap().unwrap().problems, []);
        let mut read = 0;
        let held = verify(root, &game_mod, &[], &mut |bytes| read += bytes);
        assert_eq!(
            held.unwrap().unwrap().problems,
            [Problem::Changed(vec!["Glow.dll".to_owned()])]
        );
        assert_eq!(read, total);
        assert!(
            !game_mod
                .marker(root)
                .unwrap()
                .files
                .contains_key("Glow.dll")
        );
        assert_eq!(
            check(root, &game_mod, &[]).unwrap().unwrap().problems,
            [Problem::Changed(vec!["Glow.dll".to_owned()])],
            "so a look finds it now"
        );

        // What is as released is stamped as it is now.
        rewrite(&dll, "renderer 2");
        let version = root.join("version.dll");
        let modified = fs::metadata(&version).unwrap().modified().unwrap();
        set_modified(&version, modified + std::time::Duration::from_secs(2));
        let held = verify(root, &game_mod, &[], &mut |_| {});
        assert_eq!(held.unwrap().unwrap().problems, []);
        let files = game_mod.marker(root).unwrap().files;
        assert_eq!(files.len(), 3);
        assert_eq!(
            files["version.dll"],
            Stamp::of(&fs::metadata(&version).unwrap()).unwrap()
        );
    }

    #[test]
    fn a_marker_naming_a_release_no_longer_listed_says_so() {
        let (game_mod, _, _) = glow();
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root();
        fs::write(
            game_mod.old_marker_path(root),
            "version = \"0.9\"\nurl = \"https://example.com/Glow-0.9.zip\"\n",
        )
        .unwrap();

        assert_eq!(
            status(root, &game_mod, &[]).unwrap().problems,
            [Problem::Unlisted("0.9".to_owned())]
        );
        assert!(
            !game_mod.old_marker_path(root).exists(),
            "0.9.0's marker moved into the notes"
        );
        assert_eq!(game_mod.marker(root).unwrap().version, "0.9");
    }

    #[test]
    fn installing_replaces_what_was_there_but_the_players_files() {
        let (game_mod, new_zip, old_zip) = glow();
        let (_root, installation) = crate::testing::client_with(&[
            ("WTF/Config.wtf", "SET gxApi \"d3d9ex\"\n"),
            ("Glow-spots/1.txt", "a spot"),
        ]);
        let root = installation.root();

        install(
            &installation,
            &game_mod,
            &game_mod.releases[1],
            &old_zip,
            &[],
            false,
        )
        .unwrap();
        fs::write(root.join("Glow.ini"), "Enable=0\n").unwrap();
        fs::write(root.join("Glow.log"), "ran").unwrap();
        install(
            &installation,
            &game_mod,
            &game_mod.releases[0],
            &new_zip,
            &[],
            false,
        )
        .unwrap();

        assert_eq!(read(&installation, "Glow.dll"), "renderer 2");
        assert!(!root.join("Glow/old.txt").exists(), "1.0's alone");
        assert!(!root.join("Glow.log").exists());
        assert_eq!(
            read(&installation, "Glow.ini"),
            "Enable=0\n",
            "the player's"
        );
        assert_eq!(read(&installation, "Glow-spots/1.txt"), "a spot");
        assert_eq!(installation.setting("gxApi").as_deref(), Some("d3d9"));
        assert_eq!(game_mod.marker(root).unwrap().version, "2.0");
        assert_eq!(
            status(root, &game_mod, &[]).unwrap().problems,
            [Problem::NotSetUp(vec!["glow.bin".to_owned()])],
            "the setup is left to run"
        );
        let staging = fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.is_dir()
                    && path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with(".old_")
            })
            .count();
        assert_eq!(staging, 0, "no staging folder left behind");
    }

    #[test]
    fn a_setup_counts_once_it_ran_to_the_end_and_while_what_it_wrote_is_there() {
        let (game_mod, new_zip, _) = glow();
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root();
        let newest = &game_mod.releases[0];
        let not_set_up = [Problem::NotSetUp(vec!["glow.bin".to_owned()])];

        install(&installation, &game_mod, newest, &new_zip, &[], false).unwrap();
        fs::write(root.join("glow.bin"), "half").unwrap();
        assert_eq!(
            status(root, &game_mod, &[]).unwrap().problems,
            not_set_up,
            "cut short, the setup may have left it half written"
        );

        finish_setup(&installation, &game_mod, newest).unwrap();
        assert_eq!(status(root, &game_mod, &[]).unwrap().problems, []);

        fs::remove_file(root.join("glow.bin")).unwrap();
        assert_eq!(status(root, &game_mod, &[]).unwrap().problems, not_set_up);

        // Markers from before the launcher recorded it count as not set up.
        game_mod.unmark(root).unwrap();
        fs::write(
            game_mod.old_marker_path(root),
            "version = \"2.0\"\nurl = \"https://example.com/Glow-2.0.zip\"\n",
        )
        .unwrap();
        fs::write(root.join("glow.bin"), "whole").unwrap();
        assert_eq!(status(root, &game_mod, &[]).unwrap().problems, not_set_up);
    }

    #[test]
    fn a_release_can_take_its_files_from_a_folder_of_its_archive() {
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root();
        let archive = zip(&[
            ("dxvk-1.0/x32/d3d9.dll", "32-bit"),
            ("dxvk-1.0/x64/d3d9.dll", "64-bit"),
        ]);
        let vulkan = Mod {
            id: "vulkan".to_owned(),
            name: "Vulkan".to_owned(),
            description: String::new(),
            source: Source {
                name: "Somewhere".to_owned(),
                page: "https://example.com".to_owned(),
            },
            releases: vec![Release {
                version: "1.0".to_owned(),
                url: "https://example.com/dxvk-1.0.tar.gz".to_owned(),
                updated: None,
                changes: None,
                files: vec![("d3d9.dll".to_owned(), sha256(b"32-bit"))],
                from: BTreeMap::new(),
                inside: Some("dxvk-1.0/x32".to_owned()),
                setup: Vec::new(),
                languages: BTreeMap::new(),
            }],
            settings: Vec::new(),
            writes: Vec::new(),
            config: BTreeMap::new(),
            platforms: vec!["windows".to_owned()],
            requires: Vec::new(),
            excludes: Vec::new(),
            conflicts: Vec::new(),
            alternatives: Vec::new(),
            version_in: None,
        };

        install(
            &installation,
            &vulkan,
            &vulkan.releases[0],
            &archive,
            &[],
            false,
        )
        .unwrap();
        assert_eq!(read(&installation, "d3d9.dll"), "32-bit");
        assert!(!root.join("dxvk-1.0").exists(), "the rest stays out");
        assert_eq!(status(root, &vulkan, &[]).unwrap().problems, []);

        assert_eq!(vulkan.is_for_this_system(), cfg!(windows));
        let mut anywhere = vulkan.clone();
        anywhere.platforms.clear();
        assert!(anywhere.is_for_this_system());

        // A folder outside the archive is no folder of it.
        let mut escaping = vulkan.clone();
        escaping.releases[0].inside = Some("../x32".to_owned());
        let json = Mods {
            version: Mods::VERSION,
            mods: vec![escaping],
        }
        .to_json()
        .unwrap();
        assert!(Mods::parse(json.as_bytes()).unwrap().mods.is_empty());
    }

    #[test]
    fn a_file_of_something_else_where_it_goes_keeps_it_out() {
        let (game_mod, new_zip, _) = glow();
        let newest = &game_mod.releases[0];
        let (_root, installation) =
            crate::testing::client_with(&[("version.dll", "another mod's loader")]);
        let root = installation.root();

        assert_eq!(foreign(root, &game_mod, newest, &[]), ["version.dll"]);
        assert!(matches!(
            install(&installation, &game_mod, newest, &new_zip, &[], false),
            Err(Error::Foreign(files)) if files == ["version.dll"]
        ));
        assert_eq!(read(&installation, "version.dll"), "another mod's loader");
        assert!(!root.join("Glow.dll").exists(), "nothing changed");

        // There already, matching no release and with no marker: not known
        // to be its own, so installing over it needs to back it up.
        fs::write(root.join("Glow.dll"), "patched").unwrap();
        assert!(status(root, &game_mod, &[]).is_some());
        assert_eq!(
            foreign(root, &game_mod, newest, &[]),
            ["version.dll", "Glow.dll"]
        );
        assert!(matches!(
            install(&installation, &game_mod, newest, &new_zip, &[], false),
            Err(Error::Foreign(files)) if files == ["version.dll", "Glow.dll"]
        ));
        install(&installation, &game_mod, newest, &new_zip, &[], true).unwrap();
        assert_eq!(read(&installation, "Glow.dll.backup"), "patched");
        assert_eq!(
            read(&installation, "version.dll.backup"),
            "another mod's loader"
        );
        assert_eq!(read(&installation, "version.dll"), "loader 2");
        assert_eq!(
            status(root, &game_mod, &[]).unwrap().release.as_deref(),
            Some("2.0")
        );
    }

    #[test]
    fn an_unknown_copy_is_backed_up_but_a_marked_release_is_replaced() {
        let (game_mod, new_zip, _) = glow();
        let newest = &game_mod.releases[0];

        // Files at its paths that match no release, with no marker: the
        // player's, or another mod's.
        let (_root, installation) = crate::testing::client_with(&[
            ("Glow.dll", "a patched one"),
            ("version.dll", "a loader"),
        ]);
        let root = installation.root();
        assert_eq!(game_mod.marker(root), None);
        install(&installation, &game_mod, newest, &new_zip, &[], true).unwrap();
        assert_eq!(read(&installation, "Glow.dll.backup"), "a patched one");
        assert_eq!(read(&installation, "version.dll.backup"), "a loader");
        assert_eq!(read(&installation, "version.dll"), "loader 2");

        // Marked, one file changed: it is the mod's, repaired without a backup.
        fs::write(root.join("version.dll"), "changed since").unwrap();
        install(&installation, &game_mod, newest, &new_zip, &[], false).unwrap();
        assert_eq!(read(&installation, "version.dll"), "loader 2");
        assert!(!root.join("version.dll.backup-1").exists());
        assert_eq!(
            status(root, &game_mod, &[]).unwrap().release.as_deref(),
            Some("2.0")
        );
    }

    #[test]
    fn an_edition_installed_by_hand_is_adopted_as_the_edition_it_is() {
        // Both editions have the same Y: only the new one's spells tell.
        let (new, _) = pax("pax-new", "Y same", true);
        let (old, _) = pax("pax-old", "Y same", false);
        let place = |game_mod: &Mod, root: &Path| {
            for (path, _) in &game_mod.placed_in(root).unwrap().releases[0].files {
                let file = at(root, path);
                fs::create_dir_all(file.parent().unwrap()).unwrap();
                let contents = if path.ends_with("-S.MPQ") {
                    "tables"
                } else if path.ends_with("patch-s.mpq") {
                    "spells"
                } else {
                    "Y same"
                };
                fs::write(file, contents).unwrap();
            }
        };

        // The new edition's files, in either order of asking.
        let (_root, first) =
            crate::testing::client(&["Data/enUS/locale-enUS.MPQ", "Data/patch-f.mpq"]);
        place(&new, first.root());
        assert_eq!(check(first.root(), &old, &[&new]).unwrap(), None);
        assert_eq!(old.marker(first.root()), None, "not the old edition");
        assert!(check(first.root(), &new, &[&old]).unwrap().is_some());
        assert_eq!(
            new.marker(first.root()).map(|marker| marker.version),
            Some("3.1.0".to_owned())
        );

        let (_root, second) =
            crate::testing::client(&["Data/enUS/locale-enUS.MPQ", "Data/patch-f.mpq"]);
        place(&new, second.root());
        assert!(check(second.root(), &new, &[&old]).unwrap().is_some());
        assert_eq!(check(second.root(), &old, &[&new]).unwrap(), None);
        assert_eq!(old.marker(second.root()), None);

        // The old edition's files alone are the old edition.
        let (_root, third) =
            crate::testing::client(&["Data/enUS/locale-enUS.MPQ", "Data/patch-f.mpq"]);
        place(&old, third.root());
        assert!(check(third.root(), &old, &[&new]).unwrap().is_some());
        assert_eq!(new.marker(third.root()), None);
    }

    #[test]
    fn installed_over_a_file_of_something_else_it_backs_that_up() {
        let (game_mod, new_zip, _) = glow();
        let newest = &game_mod.releases[0];
        let (_root, installation) =
            crate::testing::client_with(&[("VERSION.dll", "another mod's loader")]);
        let root = installation.root();

        install(&installation, &game_mod, newest, &new_zip, &[], true).unwrap();
        assert_eq!(
            read(&installation, "VERSION.dll.backup"),
            "another mod's loader",
            "renamed, whatever its case"
        );
        assert_eq!(read(&installation, "version.dll"), "loader 2");
        assert_eq!(
            status(root, &game_mod, &[]).unwrap().release.as_deref(),
            Some("2.0")
        );

        // A backup made before stays: the next is numbered.
        for (loader, backup) in [
            ("a third loader", "version.dll.backup-1"),
            ("a fourth loader", "version.dll.backup-2"),
        ] {
            uninstall(&installation, &game_mod, false).unwrap();
            fs::write(root.join("version.dll"), loader).unwrap();
            install(&installation, &game_mod, newest, &new_zip, &[], true).unwrap();
            assert_eq!(read(&installation, backup), loader);
        }
        assert_eq!(
            read(&installation, "VERSION.dll.backup"),
            "another mod's loader"
        );
        assert_eq!(read(&installation, "version.dll"), "loader 2");
    }

    #[test]
    fn mods_sharing_a_path_conflict_whatever_its_case() {
        let (game_mod, _, _) = glow();
        let mut other = game_mod.clone();
        other.id = "other".to_owned();
        other.settings.clear();
        other.writes = vec!["Other.log".to_owned()];
        for release in &mut other.releases {
            release.setup.clear();
            release.files = vec![("VERSION.dll".to_owned(), "0".repeat(64))];
        }

        assert_eq!(conflicts(&game_mod, &other), ["version.dll"]);
        assert_eq!(conflicts(&other, &game_mod), ["VERSION.dll"]);

        other.releases[0].files[0].0 = "d3d9.dll".to_owned();
        other.releases[1].files[0].0 = "d3d9.dll".to_owned();
        assert_eq!(conflicts(&game_mod, &other), [] as [String; 0]);
    }

    #[test]
    fn a_download_that_doesnt_match_its_release_installs_nothing() {
        let (game_mod, _, _) = glow();
        let (_root, installation) = crate::testing::client(&[]);
        let tampered = zip(&[
            ("version.dll", "loader 2"),
            ("Glow.dll", "something else"),
            ("Glow.ini", "Enable=1\n"),
            ("Glow/setup.exe", "setup"),
        ]);

        let installed = install(
            &installation,
            &game_mod,
            &game_mod.releases[0],
            &tampered,
            &[],
            false,
        );

        assert!(matches!(&installed, Err(Error::Checksum(file)) if file == "Glow.dll"));
        assert_eq!(status(installation.root(), &game_mod, &[]), None);
    }

    #[test]
    fn a_release_of_one_file_downloads_as_that_file() {
        let patch = Mod {
            id: "patch-4".to_owned(),
            name: "Patch 4".to_owned(),
            description: String::new(),
            source: Source {
                name: "Somewhere".to_owned(),
                page: "https://example.com".to_owned(),
            },
            releases: vec![Release {
                version: "1".to_owned(),
                url: "https://example.com/patch-4.MPQ".to_owned(),
                updated: None,
                changes: None,
                files: vec![("Data/patch-4.MPQ".to_owned(), sha256(b"MPQ\x1a"))],
                from: BTreeMap::new(),
                inside: None,
                setup: Vec::new(),
                languages: BTreeMap::new(),
            }],
            settings: Vec::new(),
            writes: Vec::new(),
            config: BTreeMap::new(),
            platforms: Vec::new(),
            requires: Vec::new(),
            excludes: Vec::new(),
            conflicts: Vec::new(),
            alternatives: Vec::new(),
            version_in: None,
        };
        let (_root, installation) = crate::testing::client(&["Data/enUS/locale-enUS.MPQ"]);

        install(
            &installation,
            &patch,
            &patch.releases[0],
            b"MPQ\x1a",
            &[],
            false,
        )
        .unwrap();
        assert_eq!(read(&installation, "Data/patch-4.MPQ"), "MPQ\x1a");
        assert_eq!(
            status(installation.root(), &patch, &[]).unwrap().problems,
            []
        );

        uninstall(&installation, &patch, false).unwrap();
        assert!(!installation.root().join("Data/patch-4.MPQ").exists());
        assert!(installation.root().join("Data/enUS").is_dir(), "not empty");
    }

    #[test]
    fn uninstalling_keeps_the_players_files_unless_asked_and_another_mods() {
        let (game_mod, new_zip, _) = glow();
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root();

        install(
            &installation,
            &game_mod,
            &game_mod.releases[0],
            &new_zip,
            &[],
            false,
        )
        .unwrap();
        fs::write(root.join("glow.bin"), "").unwrap();
        uninstall(&installation, &game_mod, true).unwrap();
        assert!(!root.join("Glow").exists(), "an empty folder goes too");
        assert!(!root.join("glow.bin").exists());
        assert_eq!(read(&installation, "Glow.ini"), "Enable=1\n");
        assert_eq!(status(root, &game_mod, &[]), None);

        uninstall(&installation, &game_mod, false).unwrap();
        assert!(!root.join("Glow.ini").exists());

        // Unmarked, a file of another content may be another mod's.
        fs::write(root.join("version.dll"), "another mod").unwrap();
        fs::write(root.join("Glow.dll"), "renderer 1").unwrap();
        uninstall(&installation, &game_mod, false).unwrap();
        assert_eq!(read(&installation, "version.dll"), "another mod");
        assert!(!root.join("Glow.dll").exists());
    }

    #[test]
    fn its_dlls_beside_wow_exe_are_the_ones_wine_loads_from_there() {
        let (game_mod, _, _) = glow();

        assert_eq!(game_mod.releases[0].natives(), ["version", "Glow"]);

        // A name of other letters is no DLL either, wherever its bytes end.
        let mut release = game_mod.releases[0].clone();
        release.files.push(("ñ.dl".to_owned(), String::new()));
        release.files.push(("ñ".to_owned(), String::new()));
        assert_eq!(release.natives(), ["version", "Glow"]);
    }

    /// An edition of Pax, a patch of the client's archives: `patch-y.mpq`,
    /// of `y`, in `Data` and in the folder of the client's language, and with
    /// `spells` the new spells' pair too. It is for clients with the HD
    /// models' `patch-f.mpq`, and the other edition is its alternative.
    fn pax(id: &str, y: &str, spells: bool) -> (Mod, Vec<u8>) {
        let mut members = vec![("patch-y.mpq", y)];
        let mut placed = vec![
            ("Data/patch-y.mpq", "patch-y.mpq", y),
            ("Data/{locale}/patch-{locale}-Y.MPQ", "patch-y.mpq", y),
        ];
        if spells {
            members.extend([("patch-s.mpq", "spells"), ("patch-LOCALE-S.MPQ", "tables")]);
            placed.extend([
                ("Data/patch-s.mpq", "patch-s.mpq", "spells"),
                (
                    "Data/{locale}/patch-{locale}-S.MPQ",
                    "patch-LOCALE-S.MPQ",
                    "tables",
                ),
            ]);
        }

        let release = Release {
            version: "3.1.0".to_owned(),
            url: format!("https://example.com/{id}.zip"),
            updated: None,
            changes: None,
            files: placed
                .iter()
                .map(|(path, _, contents)| ((*path).to_owned(), sha256(contents.as_bytes())))
                .collect(),
            from: placed
                .iter()
                .map(|(path, member, _)| ((*path).to_owned(), (*member).to_owned()))
                .collect(),
            inside: None,
            setup: Vec::new(),
            languages: BTreeMap::new(),
        };
        let game_mod = Mod {
            id: id.to_owned(),
            name: id.to_owned(),
            description: String::new(),
            source: Source {
                name: "Pax on GitHub".to_owned(),
                page: "https://example.com/pax".to_owned(),
            },
            releases: vec![release],
            settings: Vec::new(),
            writes: Vec::new(),
            config: BTreeMap::new(),
            platforms: Vec::new(),
            requires: vec!["Data/patch-f.mpq".to_owned()],
            excludes: Vec::new(),
            conflicts: Vec::new(),
            alternatives: ["pax-new", "pax-old"]
                .into_iter()
                .filter(|other| *other != id)
                .map(str::to_owned)
                .collect(),
            version_in: None,
        };

        (game_mod, zip(&members))
    }

    #[test]
    fn paths_can_name_every_language_the_client_has() {
        let (pax, archive) = pax("pax-new", "Y new", true);
        let (_root, installation) =
            crate::testing::client(&["Data/enUS/locale-enUS.MPQ", "Data/deDE/locale-deDE.MPQ"]);
        let root = installation.root();

        install(&installation, &pax, &pax.releases[0], &archive, &[], false).unwrap();
        assert_eq!(read(&installation, "Data/patch-y.mpq"), "Y new");
        for locale in ["enUS", "deDE"] {
            assert_eq!(
                read(
                    &installation,
                    &format!("Data/{locale}/patch-{locale}-Y.MPQ")
                ),
                "Y new",
                "one file of the archive, in every language"
            );
            assert_eq!(
                read(
                    &installation,
                    &format!("Data/{locale}/patch-{locale}-S.MPQ")
                ),
                "tables"
            );
        }
        assert_eq!(status(root, &pax, &[]).unwrap().problems, []);

        pax.unmark(root).unwrap();
        assert_eq!(
            check(root, &pax, &[]).unwrap().unwrap().release.as_deref(),
            Some("3.1.0"),
            "recognized"
        );

        // A language deleted takes its copies along: the rest is whole.
        fs::remove_dir_all(root.join("Data/deDE")).unwrap();
        assert_eq!(status(root, &pax, &[]).unwrap().problems, []);
        // One added lacks them.
        fs::create_dir_all(root.join("Data/frFR")).unwrap();
        fs::write(root.join("Data/frFR/locale-frFR.MPQ"), "").unwrap();
        let Some(Problem::Missing(missing)) = status(root, &pax, &[]).unwrap().problems.pop()
        else {
            panic!("its files in frFR are missing");
        };
        assert_eq!(
            missing.iter().filter(|path| path.contains("frFR")).count(),
            2
        );
        assert_eq!(missing.len(), 2);

        uninstall(&installation, &pax, false).unwrap();
        assert!(!root.join("Data/patch-y.mpq").exists());
        assert!(!root.join("Data/enUS/patch-enUS-Y.MPQ").exists());
        assert!(root.join("Data/enUS/locale-enUS.MPQ").exists());

        // Each language's copy is from the archive's file of that language.
        let placed = pax
            .placed(Some(Locale::DeDe), &[Locale::DeDe], None)
            .unwrap();
        assert!(
            placed.releases[0]
                .files
                .iter()
                .any(|(path, _)| path == "Data/deDE/patch-deDE-Y.MPQ")
        );
        assert_eq!(
            placed.releases[0].source("Data/deDE/patch-deDE-Y.MPQ"),
            "patch-y.mpq"
        );

        // A client without language data has no folder for it.
        assert_eq!(pax.placed(None, &[], None), None);
        let (_root, bare) = crate::testing::client(&[]);
        assert!(matches!(
            install(&bare, &pax, &pax.releases[0], &archive, &[], false),
            Err(Error::NoLanguage)
        ));
        assert_eq!(check(bare.root(), &pax, &[]).unwrap(), None);
        let (glow, _, _) = glow();
        assert_eq!(
            glow.placed(None, &[], None),
            Some(glow.clone()),
            "it names none"
        );
    }

    #[test]
    fn a_mod_suits_clients_with_what_it_requires_and_without_what_it_excludes() {
        let (hd, _) = pax("pax-new", "Y new", true);
        let mut original = hd.clone();
        original.excludes = std::mem::take(&mut original.requires);

        let (_root, plain) = crate::testing::client(&["Data/enUS/locale-enUS.MPQ"]);
        assert!(!fits(plain.root(), &hd));
        assert!(fits(plain.root(), &original));

        // Whatever the case of its name, as the game finds it.
        let (_root, models) =
            crate::testing::client(&["Data/enUS/locale-enUS.MPQ", "Data/PATCH-F.MPQ"]);
        assert!(fits(models.root(), &hd));
        assert!(!fits(models.root(), &original));
        assert_eq!(excluded(models.root(), &original, &[]), [] as [String; 0]);
    }

    #[test]
    fn some_of_what_a_mod_excludes_is_in_conflict_with_it() {
        let (hd, archive) = pax("pax-new", "Y new", true);
        let mut hd = hd;
        hd.requires
            .push("Data/{locale}/patch-{locale}-F.MPQ".to_owned());
        let mut original = hd.clone();
        original.excludes = std::mem::take(&mut original.requires);
        let (_root, installation) =
            crate::testing::client(&["Data/enUS/locale-enUS.MPQ", "Data/PATCH-F.MPQ"]);
        let root = installation.root();

        // Half the pack: not the client one needs, nor the one the other
        // isn't made for.
        assert!(!fits(root, &hd));
        assert!(fits(root, &original));
        assert_eq!(
            excluded(root, &original, &[]),
            ["Data/PATCH-F.MPQ"],
            "as spelled there"
        );
        assert_eq!(excluded(root, &hd, &[]), [] as [String; 0]);

        // Installed over it, it backs it up; repaired, it leaves it.
        let release = &original.releases[0];
        install(&installation, &original, release, &archive, &[], false).unwrap();
        assert!(root.join("Data/PATCH-F.MPQ").is_file(), "not over it");
        install(&installation, &original, release, &archive, &[], true).unwrap();
        assert!(root.join("Data/PATCH-F.MPQ.backup").is_file());
        assert!(!root.join("Data/PATCH-F.MPQ").exists());
        assert_eq!(excluded(root, &original, &[]), [] as [String; 0]);
        fs::rename(
            root.join("Data/PATCH-F.MPQ.backup"),
            root.join("Data/PATCH-F.MPQ"),
        )
        .unwrap();

        // All of it there.
        fs::write(root.join("Data/enUS/patch-enUS-F.MPQ"), "").unwrap();
        assert!(fits(root, &hd));
        assert!(!fits(root, &original));
        assert_eq!(excluded(root, &original, &[]), [] as [String; 0]);
    }

    #[test]
    fn a_file_named_like_half_of_a_pack_is_left_alone_when_the_other_half_identifies_it() {
        let (mut original, archive) = pax("pax-old", "Y old", false);
        original.requires.clear();
        original.excludes = vec!["Data/{locale}/patch-{locale}-F.MPQ".to_owned()];
        let (_root, installation) =
            crate::testing::client(&["Data/enUS/locale-enUS.MPQ", "Data/patch-F.mpq"]);
        let root = installation.root();

        // Another pack's `patch-F.mpq`: the client suits it, nothing is in
        // conflict, and installing it leaves that file be.
        assert!(fits(root, &original));
        assert_eq!(excluded(root, &original, &[]), [] as [String; 0]);
        install(
            &installation,
            &original,
            &original.releases[0],
            &archive,
            &[],
            true,
        )
        .unwrap();
        assert!(root.join("Data/patch-F.mpq").is_file());
        assert!(!root.join("Data/patch-F.mpq.backup").exists());

        // With the pack's language archive, it is the pack.
        fs::write(root.join("Data/enUS/patch-enUS-F.MPQ"), "").unwrap();
        assert!(!fits(root, &original));
    }

    #[test]
    fn installing_an_alternative_takes_the_place_of_the_one_there() {
        let (new, new_zip) = pax("pax-new", "Y new", true);
        let (old, old_zip) = pax("pax-old", "Y old", false);
        let (_root, installation) =
            crate::testing::client(&["Data/enUS/locale-enUS.MPQ", "Data/patch-f.mpq"]);
        let root = installation.root();

        install(
            &installation,
            &new,
            &new.releases[0],
            &new_zip,
            &[&old],
            false,
        )
        .unwrap();
        assert_eq!(
            check(root, &old, &[&new]).unwrap(),
            None,
            "the files they share are the other's"
        );
        assert!(check(root, &new, &[&old]).unwrap().is_some());
        assert_eq!(
            foreign(root, &old, &old.releases[0], &[&new]),
            [] as [String; 0],
            "taken the place of"
        );

        install(
            &installation,
            &old,
            &old.releases[0],
            &old_zip,
            &[&new],
            false,
        )
        .unwrap();
        assert_eq!(read(&installation, "Data/enUS/patch-enUS-Y.MPQ"), "Y old");
        assert!(!root.join("Data/patch-s.mpq").exists(), "the other's alone");
        assert!(!root.join("Data/enUS/patch-enUS-S.MPQ").exists());
        assert_eq!(new.marker(root), None);
        assert_eq!(status(root, &new, &[&old]), None);
        assert_eq!(status(root, &old, &[&new]).unwrap().problems, []);
    }

    /// Every file of the client folder at `root` that a mod can change, by
    /// path, with its checksum: not the launcher's notes, nor the game's
    /// settings.
    fn contents(root: &Path) -> BTreeMap<String, String> {
        fn walk(root: &Path, folder: &Path, files: &mut BTreeMap<String, String>) {
            for entry in fs::read_dir(folder).unwrap().map(Result::unwrap) {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with(".old_") || name == "WTF" {
                    continue;
                }
                if path.is_dir() {
                    walk(root, &path, files);
                } else {
                    let relative = path.strip_prefix(root).unwrap();
                    let _ = files.insert(
                        relative.to_string_lossy().replace('\\', "/"),
                        sha256(&fs::read(&path).unwrap()),
                    );
                }
            }
        }

        let mut files = BTreeMap::new();
        walk(root, root, &mut files);
        files
    }

    /// Installs `release` over everything, as [`change`] said it would, and
    /// checks that the folder changed by exactly that. Gives the change.
    fn install_as_changed(
        installation: &Installation,
        game_mod: &Mod,
        release: &Release,
        download: &[u8],
        alternatives: &[&Mod],
    ) -> Change {
        let root = installation.root();
        let before = contents(root);
        let changed = change(root, game_mod, release, alternatives, None);

        install(
            installation,
            game_mod,
            release,
            download,
            alternatives,
            true,
        )
        .unwrap();

        // The backups of before stay, and so aren't counted.
        let mut expected: BTreeMap<_, _> = before
            .iter()
            .filter(|(path, _)| !path.contains(BACKUP))
            .map(|(path, sum)| (path.clone(), sum.clone()))
            .collect();
        for path in &changed.removes {
            assert!(expected.remove(path).is_some(), "{path} wasn't there");
        }
        for (path, sum) in &changed.adds {
            let _ = expected.insert(path.clone(), sum.clone());
        }
        let (backups, after): (BTreeMap<_, _>, BTreeMap<_, _>) = contents(root)
            .into_iter()
            .partition(|(path, _)| path.contains(BACKUP));
        assert_eq!(after, expected);
        for (path, sum) in backups
            .into_iter()
            .filter(|(path, _)| !before.contains_key(path))
        {
            let original = path.split(BACKUP).next().unwrap();
            assert_eq!(before.get(original), Some(&sum), "{path} backs up a file");
            assert!(changed.removes.iter().any(|gone| gone == original));
        }

        changed
    }

    #[test]
    fn a_change_lists_what_an_install_backs_up_removes_and_places() {
        let (game_mod, new_zip, old_zip) = glow();
        let (_root, installation) = crate::testing::client_with(&[
            ("WTF/Config.wtf", "SET gxApi \"d3d9ex\"\n"),
            ("Glow.dll", "somebody else's"),
            ("Glow-spots/1.txt", "a spot"),
        ]);
        let root = installation.root();
        let sorted = |mut list: Vec<String>| {
            list.sort();
            list
        };
        let paths = |change: &Change| -> Vec<String> {
            sorted(change.adds.iter().map(|(path, _)| path.clone()).collect())
        };

        let first = install_as_changed(
            &installation,
            &game_mod,
            &game_mod.releases[1],
            &old_zip,
            &[],
        );
        assert_eq!(first.removes, ["Glow.dll"], "backed up");
        assert_eq!(
            paths(&first),
            [
                "Glow.dll",
                "Glow.ini",
                "Glow/old.txt",
                "Glow/setup.exe",
                "version.dll"
            ]
        );
        assert!(root.join("Glow.dll.backup").is_file());

        fs::write(root.join("Glow.ini"), "Enable=0\n").unwrap();
        fs::write(root.join("Glow.log"), "ran").unwrap();
        let second = install_as_changed(
            &installation,
            &game_mod,
            &game_mod.releases[0],
            &new_zip,
            &[],
        );
        assert_eq!(
            sorted(second.removes.clone()),
            [
                "Glow.dll",
                "Glow.log",
                "Glow/old.txt",
                "Glow/setup.exe",
                "version.dll"
            ],
            "1.0's and what it wrote, but the player's files"
        );
        assert_eq!(
            paths(&second),
            ["Glow.dll", "Glow/setup.exe", "version.dll"],
            "the player's Glow.ini stays"
        );
        assert!(
            second
                .adds
                .contains(&("Glow.dll".to_owned(), sha256(b"renderer 2")))
        );

        // An alternative's files go too, and a file of another case is
        // listed as it is there.
        let (new, new_zip) = pax("pax-new", "Y new", true);
        let (old, old_zip) = pax("pax-old", "Y old", false);
        let (_root, installation) = crate::testing::client_with(&[
            ("Data/enUS/locale-enUS.MPQ", ""),
            ("Data/patch-f.mpq", ""),
            ("Data/patch-Y.MPQ", "somebody else's"),
        ]);

        let first = install_as_changed(&installation, &new, &new.releases[0], &new_zip, &[&old]);
        assert_eq!(first.removes, ["Data/patch-Y.MPQ"], "its case, as there");
        assert_eq!(first.adds.len(), 4);

        let second = install_as_changed(&installation, &old, &old.releases[0], &old_zip, &[&new]);
        let mut removes = second.removes.clone();
        removes.sort();
        assert_eq!(
            removes,
            [
                "Data/enUS/patch-enUS-S.MPQ",
                "Data/enUS/patch-enUS-Y.MPQ",
                "Data/patch-s.mpq",
                "Data/patch-y.mpq"
            ]
        );
        assert_eq!(second.adds.len(), 2);
    }

    #[test]
    fn a_file_of_something_else_keeps_an_alternative_out_whatever_its_case() {
        let (new, new_zip) = pax("pax-new", "Y new", true);
        let (old, _) = pax("pax-old", "Y old", false);
        let (_root, installation) = crate::testing::client_with(&[
            ("Data/enUS/locale-enUS.MPQ", ""),
            ("Data/enUS/patch-enus-y.mpq", "another patch"),
        ]);

        assert!(matches!(
            install(&installation, &new, &new.releases[0], &new_zip, &[&old], false),
            Err(Error::Foreign(files)) if files == ["Data/enUS/patch-enus-y.mpq"]
        ));
        assert_eq!(
            read(&installation, "Data/enUS/patch-enus-y.mpq"),
            "another patch"
        );
        assert!(!installation.root().join("Data/patch-y.mpq").exists());
    }

    #[test]
    fn files_placed_from_another_of_the_archive_must_be_its_files() {
        let (pax, _) = pax("pax-new", "Y new", true);
        let list = |game_mod: Mod| {
            let json = Mods {
                version: Mods::VERSION,
                mods: vec![game_mod],
            }
            .to_json()
            .unwrap();
            Mods::parse(json.as_bytes()).unwrap().mods
        };

        let mut sorted = pax.clone();
        sorted.releases[0].files.sort();
        assert_eq!(list(pax.clone()), [sorted], "read back as written");

        let mut stray = pax.clone();
        let _ = stray.releases[0]
            .from
            .insert("Data/patch-z.mpq".to_owned(), "patch-y.mpq".to_owned());
        assert_eq!(list(stray), [], "a file it doesn't list");

        let mut escaping = pax;
        escaping.requires.push("../patch-f.mpq".to_owned());
        assert_eq!(list(escaping), []);
    }

    #[test]
    fn a_release_in_some_languages_goes_in_another_where_it_lacks_one() {
        let (mut maps, _) = pax("maps", "maps", false);
        maps.requires.clear();
        maps.alternatives.clear();
        let release = &mut maps.releases[0];
        release.url = "https://example.com/patch-{locale}-M.MPQ".to_owned();
        release.from.clear();
        release.languages = [Locale::DeDe, Locale::EnUs]
            .into_iter()
            .map(|language| {
                let path = "Data/{locale}/patch-{locale}-M.MPQ".to_owned();
                (language, vec![(path, sha256(language.code().as_bytes()))])
            })
            .collect();
        release.files.clear();

        let (_root, english) =
            crate::testing::client(&["Data/enUS/locale-enUS.MPQ", "Data/esES/locale-esES.MPQ"]);
        assert!(fits(english.root(), &maps));
        let placed = maps.releases[0]
            .placed(Some(Locale::EnUs), &[Locale::EnUs, Locale::EsEs], None)
            .unwrap();
        assert_eq!(
            placed.downloads(),
            [(
                "https://example.com/patch-enUS-M.MPQ".to_owned(),
                vec!["Data/enUS/patch-enUS-M.MPQ".to_owned()]
            )],
            "its languages the client has"
        );

        // Not in its language: it goes there in another, as chosen.
        let (_root, spanish) = crate::testing::client(&["Data/esES/locale-esES.MPQ"]);
        assert!(fits(spanish.root(), &maps));
        assert_eq!(lacking(spanish.root(), &maps), [Locale::EsEs]);
        assert_eq!(fallback(&maps), Some(Locale::EnUs));
        assert_eq!(editions(&maps), [Locale::DeDe, Locale::EnUs]);
        let placed = maps.releases[0]
            .placed(Some(Locale::EsEs), &[Locale::EsEs], Some(Locale::DeDe))
            .unwrap();
        assert_eq!(
            placed.downloads(),
            [(
                "https://example.com/patch-deDE-M.MPQ".to_owned(),
                vec!["Data/esES/patch-esES-M.MPQ".to_owned()]
            )],
        );
        assert_eq!(
            placed.files,
            [("Data/esES/patch-esES-M.MPQ".to_owned(), sha256(b"deDE"))]
        );
        assert_eq!(lacking(english.root(), &maps), [Locale::EsEs]);

        // Read back as written, but releases that can't say where each
        // language's files are.
        let list = |game_mod: Mod| {
            let json = Mods {
                version: Mods::VERSION,
                mods: vec![game_mod],
            }
            .to_json()
            .unwrap();
            Mods::parse(json.as_bytes()).unwrap().mods
        };
        assert_eq!(list(maps.clone()), [maps.clone()]);
        let mut one_address = maps.clone();
        one_address.releases[0].url = "https://example.com/maps.zip".to_owned();
        assert_eq!(list(one_address), []);

        // Languages the game doesn't know are left out, and a release in
        // those alone with them.
        let json = Mods {
            version: Mods::VERSION,
            mods: vec![maps],
        }
        .to_json()
        .unwrap();
        let read = |json: &str| Mods::parse(json.as_bytes()).unwrap().mods;
        let italian = read(&json.replace("\"deDE\"", "\"itIT\""));
        assert_eq!(editions(&italian[0]), [Locale::EnUs]);
        assert_eq!(
            read(
                &json
                    .replace("\"deDE\"", "\"itIT\"")
                    .replace("\"enUS\"", "\"ptBR\"")
            ),
            []
        );
    }

    #[test]
    fn a_change_follows_the_language_the_player_picked() {
        let (mut maps, _) = pax("maps", "maps", false);
        maps.requires.clear();
        maps.alternatives.clear();
        let release = &mut maps.releases[0];
        release.url = "https://example.com/patch-{locale}-M.MPQ".to_owned();
        release.from.clear();
        release.languages = [Locale::DeDe, Locale::EnUs]
            .into_iter()
            .map(|language| {
                let path = "Data/{locale}/patch-{locale}-M.MPQ".to_owned();
                (language, vec![(path, sha256(language.code().as_bytes()))])
            })
            .collect();
        release.files.clear();
        let (_root, spanish) = crate::testing::client(&["Data/esES/locale-esES.MPQ"]);
        let root = spanish.root();
        let adds = |instead| change(root, &maps, &maps.releases[0], &[], instead).adds;
        let there = "Data/esES/patch-esES-M.MPQ".to_owned();

        assert_eq!(adds(Some(Locale::DeDe)), [(there.clone(), sha256(b"deDE"))]);
        assert_eq!(adds(Some(Locale::EnUs)), [(there, sha256(b"enUS"))]);
    }

    #[test]
    fn the_config_it_needs_is_a_problem_only_when_set_otherwise() {
        let (game_mod, new_zip, _) = glow();
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root();
        install(
            &installation,
            &game_mod,
            &game_mod.releases[0],
            &new_zip,
            &[],
            false,
        )
        .unwrap();
        fs::write(root.join("glow.bin"), "").unwrap();
        finish_setup(&installation, &game_mod, &game_mod.releases[0]).unwrap();
        assert_eq!(status(root, &game_mod, &[]).unwrap().problems, []);

        installation.set("gxApi", "D3D9Ex").unwrap();
        assert_eq!(
            status(root, &game_mod, &[]).unwrap().problems,
            [Problem::Config(vec![(
                "gxApi".to_owned(),
                "D3D9Ex".to_owned(),
                "d3d9".to_owned()
            )])]
        );
    }

    #[test]
    fn installed_mods_files_are_owned_by_them() {
        let (mut game_mod, _, _) = glow();
        game_mod.releases[0].files.push((
            "Data/{locale}/patch-{locale}-M.MPQ".to_owned(),
            sha256(b"maps"),
        ));
        let list = Mods {
            version: Mods::VERSION,
            mods: vec![game_mod.clone()],
        };
        let (_root, installation) = crate::testing::client(&["Data/enUS/a.MPQ", "Data/esES/a.MPQ"]);
        let root = installation.root();

        assert!(list.owned(root).is_empty(), "no marker: not owned");

        game_mod
            .mark(root, &game_mod.releases[0], true, None, BTreeMap::new())
            .unwrap();
        let expected: BTreeMap<String, String> = [
            "version.dll",
            "glow.dll",
            "glow.ini",
            "glow/setup.exe",
            "glow-spots",
            "glow.log",
            "glow.bin",
            "data/enus/patch-enus-m.mpq",
            "data/eses/patch-eses-m.mpq",
        ]
        .into_iter()
        .map(|path| (path.to_owned(), "glow".to_owned()))
        .collect();
        assert_eq!(list.owned(root), expected, "the release's, as placed here");

        // The older release the marker names has no such file.
        game_mod
            .mark(root, &game_mod.releases[1], true, None, BTreeMap::new())
            .unwrap();
        let owned = list.owned(root);
        assert!(owned.contains_key("glow/old.txt"));
        assert!(!owned.contains_key("data/enus/patch-enus-m.mpq"));

        // Naming a release no longer listed, it owns every path.
        let mut elsewhere = game_mod.releases[0].clone();
        elsewhere.version = "0.1".to_owned();
        game_mod
            .mark(root, &elsewhere, true, None, BTreeMap::new())
            .unwrap();
        let owned = list.owned(root);
        assert_eq!(owned.len(), expected.len() + 1, "and the older's");
        assert!(owned.contains_key("glow/old.txt"));
        assert!(owned.contains_key("data/eses/patch-eses-m.mpq"));

        // Marked mods that aren't listed own nothing.
        assert!(Mods::default().owned(root).is_empty());
    }
}
