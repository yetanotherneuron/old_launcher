//! The launcher's mods: changes to a client outside `Interface/AddOns`, such
//! as a DLL the game loads beside `Wow.exe` or a patched archive in `Data`,
//! each with the files its releases put in the client folder.
//!
//! Like the catalog of addons, the list is generated offline by the
//! `indexer` tool, which hashes every file of every release, and is published
//! as one JSON document. Which release a client holds is recorded in a marker
//! in the notes beside `Wow.exe`, written when the launcher installs a
//! release or recognizes one already there.
//!
//! A path in the list may name the language the client plays in, as
//! [`LOCALE`]: a mod's files can go in that language's folder of `Data`.
//! Paths are found in a client folder whatever their case, as the game
//! finds them. Some mods are alternatives to each other, editions of one
//! thing, of which a client holds one at most: installing one takes the
//! place of the other.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::io_at;
use crate::installation::{STAGING_PREFIX, child, config_setting};
use crate::notes::Notes;
use crate::{Error, Installation, archive};

/// What stands for the language a client plays in (`enUS`, `deDE`, …) in
/// the paths of the list of mods.
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
    /// isn't offered it. An edition made for an HD model pack requires the
    /// pack's archives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<String>,
    /// Files a client must not have for the mod to suit it: one with any of
    /// them isn't offered it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excludes: Vec<String>,
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
    /// the files a client must have or not, and alternatives.
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
                release.files.iter().all(|(path, _)| safe(path))
                    && release.from.iter().all(|(to, from)| {
                        safe(from) && release.files.iter().any(|(path, _)| path == to)
                    })
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
}

/// `path` in a client that plays in `locale`, [`LOCALE`] in it standing for
/// it: nothing when it names the language and `locale` is unknown.
fn place(path: &str, locale: Option<&str>) -> Option<String> {
    if path.contains(LOCALE) {
        Some(path.replace(LOCALE, locale?))
    } else {
        Some(path.to_owned())
    }
}

/// Every one of `paths` in a client that plays in `locale` (see [`place`]).
fn place_all(paths: &[String], locale: Option<&str>) -> Option<Vec<String>> {
    paths.iter().map(|path| place(path, locale)).collect()
}

impl Mod {
    /// The mod as it goes in a client that plays in `locale`, its paths
    /// naming that language (see [`LOCALE`]): nothing when they name it and
    /// `locale` is unknown.
    pub fn placed(&self, locale: Option<&str>) -> Option<Self> {
        Some(Self {
            releases: self
                .releases
                .iter()
                .map(|release| release.placed(locale))
                .collect::<Option<_>>()?,
            settings: place_all(&self.settings, locale)?,
            writes: place_all(&self.writes, locale)?,
            requires: place_all(&self.requires, locale)?,
            excludes: place_all(&self.excludes, locale)?,
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
    /// whether its setup ran to the end.
    fn mark(&self, root: &Path, release: &Release, set_up: bool) -> Result<(), Error> {
        let marker = Marker {
            version: release.version.clone(),
            url: release.url.clone(),
            set_up,
        };
        Notes::change(root, |notes| {
            let _ = notes.mods.insert(self.id.clone(), marker);
        })?;

        remove(&self.old_marker_path(root))
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
    /// The release as it goes in a client that plays in `locale` (see
    /// [`Mod::placed`]).
    pub fn placed(&self, locale: Option<&str>) -> Option<Self> {
        Some(Self {
            files: self
                .files
                .iter()
                .map(|(path, sum)| Some((place(path, locale)?, sum.clone())))
                .collect::<Option<_>>()?,
            from: self
                .from
                .iter()
                .map(|(to, from)| Some((place(to, locale)?, place(from, locale)?)))
                .collect::<Option<_>>()?,
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

/// `mods` as they go in a client that plays in `locale` (see
/// [`Mod::placed`]), but those that can't.
fn placed_all(mods: &[&Mod], locale: Option<&str>) -> Vec<Mod> {
    mods.iter()
        .filter_map(|game_mod| game_mod.placed(locale))
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

/// The files of `release` in the client folder at `root` that are missing,
/// and those that differ, but the player's.
fn compare(root: &Path, game_mod: &Mod, release: &Release) -> (Vec<String>, Vec<String>) {
    let mut missing = Vec::new();
    let mut changed = Vec::new();

    for (path, sum) in &release.files {
        if game_mod.is_setting(path) {
            continue;
        }

        match fs::read(at(root, path)) {
            Err(_) => missing.push(path.clone()),
            Ok(bytes) if sha256(&bytes) != *sum => changed.push(path.clone()),
            Ok(_) => {}
        }
    }

    (missing, changed)
}

/// What the client folder at `root` holds of `game_mod`, if anything (see
/// [`present`], which leaves out the paths it shares with its
/// `alternatives`): checked against the release its marker names, or else
/// [`Problem::Unchecked`].
pub fn status(root: &Path, game_mod: &Mod, alternatives: &[&Mod]) -> Option<Status> {
    let locale = crate::installation::locale(root);
    let game_mod = game_mod.placed(locale.as_deref())?;
    let alternatives = placed_all(alternatives, locale.as_deref());

    status_of(root, &game_mod, &alternatives.iter().collect::<Vec<_>>())
}

/// [`status`], of `game_mod` and its `alternatives` placed in the client
/// folder at `root`.
fn status_of(root: &Path, game_mod: &Mod, alternatives: &[&Mod]) -> Option<Status> {
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
            let (missing, changed) = compare(root, game_mod, release);
            if !missing.is_empty() {
                problems.push(Problem::Missing(missing));
            }
            if !changed.is_empty() {
                problems.push(Problem::Changed(changed));
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
/// something else, which installing it would replace: not the player's,
/// with the contents of none of `game_mod`'s releases, at a path that isn't
/// the mod's own there, and none of its `alternatives`', which installing it
/// takes the place of. Its own are the files of the release its marker
/// names, or all its paths when it is there without one (a repair takes
/// those for its own files, broken); with nothing of it there, it has none.
pub fn foreign(
    root: &Path,
    game_mod: &Mod,
    release: &Release,
    alternatives: &[&Mod],
) -> Vec<String> {
    let locale = crate::installation::locale(root);
    let (Some(game_mod), Some(release)) = (
        game_mod.placed(locale.as_deref()),
        release.placed(locale.as_deref()),
    ) else {
        return Vec::new();
    };
    let alternatives = placed_all(alternatives, locale.as_deref());

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
    let own: BTreeSet<&str> = if present(root, game_mod, alternatives) {
        match game_mod
            .marker(root)
            .and_then(|marker| game_mod.release(&marker.version))
        {
            Some(held) => held.files.iter().map(|(path, _)| path.as_str()).collect(),
            None => game_mod.paths(),
        }
    } else {
        BTreeSet::new()
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
        .cloned()
        .collect()
}

/// Checks what the client folder at `root` holds of `game_mod` against its
/// releases, when no marker says which it is: the newest whose files are
/// all there as released is recorded in its marker, set up if what its
/// setup writes is there. Then tells what the folder holds (see [`status`],
/// which leaves out what it shares with its `alternatives`), which is
/// [`Problem::Unknown`] when it matches none: a copy installed by hand,
/// with the version its files say and the DLLs it has beside `Wow.exe`.
pub fn check(root: &Path, game_mod: &Mod, alternatives: &[&Mod]) -> Result<Option<Status>, Error> {
    let locale = crate::installation::locale(root);
    let Some(game_mod) = game_mod.placed(locale.as_deref()) else {
        return Ok(None);
    };
    let game_mod = &game_mod;
    let alternatives = placed_all(alternatives, locale.as_deref());
    let alternatives: Vec<&Mod> = alternatives.iter().collect();

    if game_mod.marker(root).is_none() {
        let recognized = game_mod.releases.iter().find(|release| {
            let (missing, changed) = compare(root, game_mod, release);
            missing.is_empty() && changed.is_empty()
        });

        match recognized {
            Some(release) => {
                let set_up = release
                    .setup
                    .iter()
                    .all(|step| at(root, &step.creates).exists());
                game_mod.mark(root, release, set_up)?;
            }
            None => {
                return Ok(status_of(root, game_mod, &alternatives).map(|_| {
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

    Ok(status_of(root, game_mod, &alternatives))
}

/// The files `game_mod` requires that the client folder at `root` has, and
/// those it lacks, when it has some but not all: what the mod needs is
/// there only in part, so it doesn't suit the client, and neither does an
/// alternative that excludes those files.
pub fn partly(root: &Path, game_mod: &Mod) -> Option<(Vec<String>, Vec<String>)> {
    let game_mod = game_mod.placed(crate::installation::locale(root).as_deref())?;
    let (present, missing): (Vec<String>, Vec<String>) = game_mod
        .requires
        .iter()
        .cloned()
        .partition(|path| at(root, path).is_file());

    (!present.is_empty() && !missing.is_empty()).then_some((present, missing))
}

/// The suffix of a file set aside: the game doesn't load it.
pub const ASIDE: &str = ".disabled";

/// Sets the files at `paths` in `installation`'s folder aside: each renamed
/// to end in [`ASIDE`], which the game doesn't load, so renaming it back
/// undoes it. Nothing is renamed when a file set aside before is in the way
/// of any of them.
pub fn set_aside(installation: &Installation, paths: &[String]) -> Result<(), Error> {
    let moves: Vec<(PathBuf, PathBuf)> = paths
        .iter()
        .map(|path| {
            let file = at(installation.root(), path);
            let name = file.file_name().unwrap_or_default().to_string_lossy();
            // One set aside before, whatever its case, as the game finds it.
            let aside = child(
                file.parent().unwrap_or(installation.root()),
                &format!("{name}{ASIDE}"),
            );

            (file, aside)
        })
        .collect();

    if let Some((_, aside)) = moves.iter().find(|(_, aside)| aside.exists()) {
        return Err(io_at(aside)(io::ErrorKind::AlreadyExists.into()));
    }
    for (file, aside) in &moves {
        fs::rename(file, aside).map_err(io_at(file))?;
    }

    Ok(())
}

/// Whether `game_mod` suits the client folder at `root`: it has every file
/// the mod requires and none it excludes, whatever their case. A mod whose
/// paths name the client's language suits none without language data.
pub fn fits(root: &Path, game_mod: &Mod) -> bool {
    let Some(game_mod) = game_mod.placed(crate::installation::locale(root).as_deref()) else {
        return false;
    };

    game_mod
        .requires
        .iter()
        .all(|path| at(root, path).is_file())
        && !game_mod.excludes.iter().any(|path| at(root, path).exists())
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

/// Installs `release` of `game_mod`, downloaded as `download`, into
/// `installation`'s folder. Every file is checked against the release first,
/// so nothing changes unless all of them are right. Then its `alternatives`
/// there go, and what was there of the mod, but the player's files (see
/// [`uninstall`]); the release's files go in place but the player's ones
/// already there, the marker records the release, and `WTF/Config.wtf` gets
/// what the mod needs. Its setup isn't run: until [`finish_setup`] says it
/// ran to the end, the marker says it didn't, when there is one. Files of
/// something else where the release puts its own stay, and so does
/// everything else: see [`foreign`].
pub fn install(
    installation: &Installation,
    game_mod: &Mod,
    release: &Release,
    download: &[u8],
    alternatives: &[&Mod],
) -> Result<(), Error> {
    let root = installation.root();
    let locale = crate::installation::locale(root);
    let (Some(game_mod), Some(release)) = (
        game_mod.placed(locale.as_deref()),
        release.placed(locale.as_deref()),
    ) else {
        return Err(Error::NoLanguage);
    };
    let (game_mod, release) = (&game_mod, &release);
    let alternatives = placed_all(alternatives, locale.as_deref());
    let alternatives: Vec<&Mod> = alternatives.iter().collect();

    let foreign = foreign_of(root, game_mod, release, &alternatives);
    if !foreign.is_empty() {
        return Err(Error::Foreign(foreign));
    }

    // Beside the files it replaces, so they move in by renaming. Left behind
    // by an install cut short, it is swept later (see `Installation::scan`).
    let staging = tempfile::Builder::new()
        .prefix(STAGING_PREFIX)
        .tempdir_in(root)
        .map_err(io_at(root))?;
    // Where the release's files are in it.
    let from = staging.path().join(release.inside.as_deref().unwrap_or(""));

    match (archive::Format::detect(download), release.files.as_slice()) {
        (Some(_), _) => {
            archive::extract(download, staging.path())?;
        }
        // Not an archive: the release's one file itself.
        (None, [(path, _)]) => {
            let file = from.join(release.source(path));
            if let Some(parent) = file.parent() {
                fs::create_dir_all(parent).map_err(io_at(parent))?;
            }
            fs::write(&file, download).map_err(io_at(&file))?;
        }
        (None, _) => return Err(Error::UnknownArchive),
    }

    for (path, sum) in &release.files {
        let source = release.source(path);
        let bytes =
            fs::read(from.join(source)).map_err(|_| Error::NotInRelease(source.to_owned()))?;
        if sha256(&bytes) != *sum {
            return Err(Error::Checksum(path.clone()));
        }
    }

    // What it takes the place of goes, then whatever was there of it,
    // another release's or a broken install's.
    for other in alternatives
        .iter()
        .filter(|other| present(root, other, &[]))
    {
        uninstall_of(installation, other, true)?;
    }
    uninstall_of(installation, game_mod, true)?;

    // A file of the archive that goes to several places is copied to each
    // but the last, where it moves.
    let mut uses: BTreeMap<&str, usize> = BTreeMap::new();
    for (path, _) in &release.files {
        *uses.entry(release.source(path)).or_default() += 1;
    }

    for (path, _) in &release.files {
        let source = release.source(path);
        let left = uses.get_mut(source).map_or(0, |left| {
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
        let staged = from.join(source);
        let placed = if left > 0 {
            fs::copy(&staged, &target).map(drop)
        } else {
            fs::rename(&staged, &target)
        };
        placed.map_err(io_at(&target))?;
    }
    game_mod.mark(root, release, release.setup.is_empty())?;

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
    game_mod.mark(installation.root(), release, true)
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

    match game_mod.placed(crate::installation::locale(root).as_deref()) {
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
    let root = installation.root();
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

    let mut removed: Vec<&str> = Vec::new();

    for path in game_mod.paths() {
        let file = at(root, path);
        let ours = marked.contains(path)
            || fs::read(&file).is_ok_and(|bytes| game_mod.released(path, &sha256(&bytes)));

        if ours {
            remove(&file)?;
            removed.push(path);
        }
    }

    let settings = game_mod
        .settings
        .iter()
        .map(String::as_str)
        .filter(|_| !keep_settings);
    for path in game_mod.written().into_iter().chain(settings) {
        remove(&at(root, path))?;
        removed.push(path);
    }
    game_mod.unmark(root)?;

    for path in removed {
        prune(root, path);
    }

    Ok(())
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

        install(&installation, &game_mod, newest, &new_zip, &[]).unwrap();
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
            }],
            settings: Vec::new(),
            writes: Vec::new(),
            config: BTreeMap::new(),
            platforms: vec!["windows".to_owned()],
            requires: Vec::new(),
            excludes: Vec::new(),
            alternatives: Vec::new(),
            version_in: None,
        };

        install(&installation, &vulkan, &vulkan.releases[0], &archive, &[]).unwrap();
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
            install(&installation, &game_mod, newest, &new_zip, &[]),
            Err(Error::Foreign(files)) if files == ["version.dll"]
        ));
        assert_eq!(read(&installation, "version.dll"), "another mod's loader");
        assert!(!root.join("Glow.dll").exists(), "nothing changed");

        // There already, broken: a repair takes its files for its own.
        fs::write(root.join("Glow.dll"), "patched").unwrap();
        assert!(status(root, &game_mod, &[]).is_some());
        assert_eq!(foreign(root, &game_mod, newest, &[]), [] as [String; 0]);
        install(&installation, &game_mod, newest, &new_zip, &[]).unwrap();
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
            }],
            settings: Vec::new(),
            writes: Vec::new(),
            config: BTreeMap::new(),
            platforms: Vec::new(),
            requires: Vec::new(),
            excludes: Vec::new(),
            alternatives: Vec::new(),
            version_in: None,
        };
        let (_root, installation) = crate::testing::client(&["Data/enUS/locale-enUS.MPQ"]);

        install(&installation, &patch, &patch.releases[0], b"MPQ\x1a", &[]).unwrap();
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
    fn paths_can_name_the_language_the_client_plays_in() {
        let (pax, archive) = pax("pax-new", "Y new", true);
        let (_root, installation) = crate::testing::client(&["Data/enUS/locale-enUS.MPQ"]);
        let root = installation.root();

        install(&installation, &pax, &pax.releases[0], &archive, &[]).unwrap();
        assert_eq!(read(&installation, "Data/patch-y.mpq"), "Y new");
        assert_eq!(
            read(&installation, "Data/enUS/patch-enUS-Y.MPQ"),
            "Y new",
            "one file of the archive, in two places"
        );
        assert_eq!(read(&installation, "Data/enUS/patch-enUS-S.MPQ"), "tables");
        assert_eq!(status(root, &pax, &[]).unwrap().problems, []);

        pax.unmark(root).unwrap();
        assert_eq!(
            check(root, &pax, &[]).unwrap().unwrap().release.as_deref(),
            Some("3.1.0"),
            "recognized"
        );

        uninstall(&installation, &pax, false).unwrap();
        assert!(!root.join("Data/patch-y.mpq").exists());
        assert!(!root.join("Data/enUS/patch-enUS-Y.MPQ").exists());
        assert!(root.join("Data/enUS/locale-enUS.MPQ").exists());

        // In another language, its own folder.
        let placed = pax.placed(Some("deDE")).unwrap();
        assert_eq!(placed.releases[0].files[1].0, "Data/deDE/patch-deDE-Y.MPQ");
        assert_eq!(
            placed.releases[0].source("Data/deDE/patch-deDE-Y.MPQ"),
            "patch-y.mpq"
        );

        // A client without language data has no folder for it.
        assert_eq!(pax.placed(None), None);
        let (_root, bare) = crate::testing::client(&[]);
        assert!(matches!(
            install(&bare, &pax, &pax.releases[0], &archive, &[]),
            Err(Error::NoLanguage)
        ));
        assert_eq!(check(bare.root(), &pax, &[]).unwrap(), None);
        let (glow, _, _) = glow();
        assert_eq!(glow.placed(None), Some(glow.clone()), "it names none");
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
    }

    #[test]
    fn what_a_mod_requires_there_only_in_part_can_be_set_aside() {
        let (hd, _) = pax("pax-new", "Y new", true);
        let mut hd = hd;
        hd.requires
            .push("Data/{locale}/patch-{locale}-F.MPQ".to_owned());
        let (_root, installation) =
            crate::testing::client(&["Data/enUS/locale-enUS.MPQ", "Data/PATCH-F.MPQ"]);
        let root = installation.root();

        assert_eq!(
            partly(root, &hd),
            Some((
                vec!["Data/patch-f.mpq".to_owned()],
                vec!["Data/enUS/patch-enUS-F.MPQ".to_owned()]
            ))
        );
        assert!(!fits(root, &hd));

        set_aside(&installation, &["Data/patch-f.mpq".to_owned()]).unwrap();
        assert!(root.join("Data/PATCH-F.MPQ.disabled").is_file());
        assert!(!root.join("Data/PATCH-F.MPQ").exists());
        assert_eq!(partly(root, &hd), None, "none of it there now");

        // One set aside before stays, and so does the file in its way.
        fs::write(root.join("Data/patch-f.mpq"), "again").unwrap();
        assert!(matches!(
            set_aside(&installation, &["Data/PATCH-F.MPQ".to_owned()]),
            Err(Error::Io { .. })
        ));
        assert_eq!(read(&installation, "Data/patch-f.mpq"), "again");

        // All of it there: nothing to set aside.
        fs::write(root.join("Data/enUS/patch-enUS-F.MPQ"), "").unwrap();
        assert_eq!(partly(root, &hd), None);
        assert!(fits(root, &hd));
    }

    #[test]
    fn installing_an_alternative_takes_the_place_of_the_one_there() {
        let (new, new_zip) = pax("pax-new", "Y new", true);
        let (old, old_zip) = pax("pax-old", "Y old", false);
        let (_root, installation) =
            crate::testing::client(&["Data/enUS/locale-enUS.MPQ", "Data/patch-f.mpq"]);
        let root = installation.root();

        install(&installation, &new, &new.releases[0], &new_zip, &[&old]).unwrap();
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

        install(&installation, &old, &old.releases[0], &old_zip, &[&new]).unwrap();
        assert_eq!(read(&installation, "Data/enUS/patch-enUS-Y.MPQ"), "Y old");
        assert!(!root.join("Data/patch-s.mpq").exists(), "the other's alone");
        assert!(!root.join("Data/enUS/patch-enUS-S.MPQ").exists());
        assert_eq!(new.marker(root), None);
        assert_eq!(status(root, &new, &[&old]), None);
        assert_eq!(status(root, &old, &[&new]).unwrap().problems, []);
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
            install(&installation, &new, &new.releases[0], &new_zip, &[&old]),
            Err(Error::Foreign(files)) if files == ["Data/enUS/patch-enUS-Y.MPQ"]
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
}
