//! The launcher's mods: changes to a client outside `Interface/AddOns`, such
//! as a DLL the game loads beside `Wow.exe` or a patched archive in `Data`,
//! each with the files its releases put in the client folder.
//!
//! Like the catalog of addons, the list is generated offline by the
//! `indexer` tool, which hashes every file of every release, and is published
//! as one JSON document. Which release a client holds is recorded in a marker
//! in the notes beside `Wow.exe`, written when the launcher installs a
//! release or recognizes one already there.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::io_at;
use crate::installation::{STAGING_PREFIX, config_setting};
use crate::notes::Notes;
use crate::{Error, Installation, archive};

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
    /// archive holds them at the same paths, in [`Release::inside`].
    pub files: Vec<(String, String)>,
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
    /// The schema version this crate reads and writes.
    pub const VERSION: u32 = 1;

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
}

impl Mod {
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

/// Whether the client folder at `root` holds `game_mod`: its marker, or one
/// of its releases' files with that release's contents, or several of them
/// whatever their contents. A lone file that matches no release may be
/// another mod's: `version.dll` is a common name. What it writes as it
/// runs doesn't count: a log outlives it.
fn present(root: &Path, game_mod: &Mod) -> bool {
    if game_mod.marked(root) {
        return true;
    }

    let there: Vec<&str> = game_mod
        .paths()
        .into_iter()
        .filter(|path| root.join(path).is_file())
        .collect();

    match there.as_slice() {
        [] => false,
        [path] => {
            fs::read(root.join(path)).is_ok_and(|bytes| game_mod.released(path, &sha256(&bytes)))
        }
        _ => true,
    }
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

        match fs::read(root.join(path)) {
            Err(_) => missing.push(path.clone()),
            Ok(bytes) if sha256(&bytes) != *sum => changed.push(path.clone()),
            Ok(_) => {}
        }
    }

    (missing, changed)
}

/// What the client folder at `root` holds of `game_mod`, if anything (see
/// [`present`]): checked against the release its marker names, or else
/// [`Problem::Unchecked`].
pub fn status(root: &Path, game_mod: &Mod) -> Option<Status> {
    if !present(root, game_mod) {
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
                .filter(|creates| !marker.set_up || !root.join(creates).exists())
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
/// with the contents of none of `game_mod`'s releases, and at a path that
/// isn't the mod's own there. Its own are the files of the release its
/// marker names, or all its paths when it is there without one (a repair
/// takes those for its own files, broken); with nothing of it there, it has
/// none.
pub fn foreign(root: &Path, game_mod: &Mod, release: &Release) -> Vec<String> {
    let own: BTreeSet<&str> = if present(root, game_mod) {
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
            fs::read(root.join(path)).is_ok_and(|bytes| !game_mod.released(path, &sha256(&bytes)))
        })
        .cloned()
        .collect()
}

/// Checks what the client folder at `root` holds of `game_mod` against its
/// releases, when no marker says which it is: the newest whose files are
/// all there as released is recorded in its marker, set up if what its
/// setup writes is there. Then tells what the folder holds, which is
/// [`Problem::Unknown`] when it matches none: a copy installed by hand,
/// with the version its files say and the DLLs it has beside `Wow.exe`.
pub fn check(root: &Path, game_mod: &Mod) -> Result<Option<Status>, Error> {
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
                    .all(|step| root.join(&step.creates).exists());
                game_mod.mark(root, release, set_up)?;
            }
            None => {
                return Ok(status(root, game_mod).map(|_| {
                    let mut natives: Vec<String> = game_mod
                        .releases
                        .iter()
                        .flat_map(Release::natives)
                        .filter(|dll| root.join(format!("{dll}.dll")).is_file())
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

    Ok(status(root, game_mod))
}

/// The version the files of `game_mod` in the client folder at `root` say
/// they are, if they say: after the text its [`Mod::version_in`] names, or
/// else in the version resource of the first of its files that has one.
fn manual_version(root: &Path, game_mod: &Mod) -> Option<String> {
    if let Some(place) = &game_mod.version_in {
        let bytes = fs::read(root.join(&place.file)).ok()?;
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
        .filter_map(|path| fs::read(root.join(path)).ok())
        .find_map(|bytes| crate::version::product_version(&bytes))
}

/// Installs `release` of `game_mod`, downloaded as `download`, into
/// `installation`'s folder. Every file is checked against the release first,
/// so nothing changes unless all of them are right. Then what was there of
/// the mod goes but the player's files (see [`uninstall`]), the release's
/// files go in place but the player's ones already there, the marker records
/// the release, and `WTF/Config.wtf` gets what the mod needs. Its setup isn't
/// run: until [`finish_setup`] says it ran to the end, the marker says it
/// didn't, when there is one. Files of something else where the release
/// puts its own stay, and so does everything else: see [`foreign`].
pub fn install(
    installation: &Installation,
    game_mod: &Mod,
    release: &Release,
    download: &[u8],
) -> Result<(), Error> {
    let root = installation.root();

    let foreign = foreign(root, game_mod, release);
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
            let file = from.join(path);
            if let Some(parent) = file.parent() {
                fs::create_dir_all(parent).map_err(io_at(parent))?;
            }
            fs::write(&file, download).map_err(io_at(&file))?;
        }
        (None, _) => return Err(Error::UnknownArchive),
    }

    for (path, sum) in &release.files {
        let bytes = fs::read(from.join(path)).map_err(|_| Error::NotInRelease(path.clone()))?;
        if sha256(&bytes) != *sum {
            return Err(Error::Checksum(path.clone()));
        }
    }

    // Nothing left over of what was there, another release's or a broken
    // install's.
    uninstall(installation, game_mod, true)?;

    for (path, _) in &release.files {
        let target = root.join(path);
        if game_mod.is_setting(path) && target.exists() {
            continue;
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(io_at(parent))?;
        }
        fs::rename(from.join(path), &target).map_err(io_at(&target))?;
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
        let file = root.join(path);
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
        remove(&root.join(path))?;
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
            Mods::parse(br#"{ "version": 2, "mods": [] }"#),
            Err(Error::ModsVersion(2))
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
            status(installation.root(), &game_mod)
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
            check(root, &game_mod).unwrap().unwrap().problems,
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
            check(root, &game_mod).unwrap().unwrap().problems,
            [unknown(None)]
        );
        fs::write(root.join("Glow.dll"), b"MZ \0Game: \0Glow: v3.1-rc\0\0 ").unwrap();
        assert_eq!(
            check(root, &game_mod).unwrap().unwrap().problems,
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
            check(root, &game_mod).unwrap(),
            Some(Status {
                release: Some("1.0".to_owned()),
                problems: vec![Problem::NotSetUp(vec!["glow.bin".to_owned()])],
            }),
            "the player's settings are theirs to change"
        );
        assert_eq!(game_mod.marker(root).unwrap().version, "1.0");

        fs::write(root.join("glow.bin"), "").unwrap();
        assert_eq!(
            status(root, &game_mod).unwrap().problems,
            [Problem::NotSetUp(vec!["glow.bin".to_owned()])],
            "made after the check, it may be half written"
        );

        // Checked with what its setup writes there, it counts as set up.
        game_mod.unmark(root).unwrap();
        assert_eq!(check(root, &game_mod).unwrap().unwrap().problems, []);
        assert!(game_mod.marker(root).unwrap().set_up);

        fs::write(root.join("Glow.dll"), "patched").unwrap();
        fs::remove_file(root.join("Glow/old.txt")).unwrap();
        assert_eq!(
            status(root, &game_mod).unwrap().problems,
            [
                Problem::Missing(vec!["Glow/old.txt".to_owned()]),
                Problem::Changed(vec!["Glow.dll".to_owned()]),
            ]
        );

        // Without the marker, it matches no release.
        game_mod.unmark(root).unwrap();
        assert_eq!(
            check(root, &game_mod).unwrap().unwrap().problems,
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
            status(root, &game_mod).unwrap().problems,
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

        install(&installation, &game_mod, &game_mod.releases[1], &old_zip).unwrap();
        fs::write(root.join("Glow.ini"), "Enable=0\n").unwrap();
        fs::write(root.join("Glow.log"), "ran").unwrap();
        install(&installation, &game_mod, &game_mod.releases[0], &new_zip).unwrap();

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
            status(root, &game_mod).unwrap().problems,
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

        install(&installation, &game_mod, newest, &new_zip).unwrap();
        fs::write(root.join("glow.bin"), "half").unwrap();
        assert_eq!(
            status(root, &game_mod).unwrap().problems,
            not_set_up,
            "cut short, the setup may have left it half written"
        );

        finish_setup(&installation, &game_mod, newest).unwrap();
        assert_eq!(status(root, &game_mod).unwrap().problems, []);

        fs::remove_file(root.join("glow.bin")).unwrap();
        assert_eq!(status(root, &game_mod).unwrap().problems, not_set_up);

        // Markers from before the launcher recorded it count as not set up.
        game_mod.unmark(root).unwrap();
        fs::write(
            game_mod.old_marker_path(root),
            "version = \"2.0\"\nurl = \"https://example.com/Glow-2.0.zip\"\n",
        )
        .unwrap();
        fs::write(root.join("glow.bin"), "whole").unwrap();
        assert_eq!(status(root, &game_mod).unwrap().problems, not_set_up);
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
                inside: Some("dxvk-1.0/x32".to_owned()),
                setup: Vec::new(),
            }],
            settings: Vec::new(),
            writes: Vec::new(),
            config: BTreeMap::new(),
            platforms: vec!["windows".to_owned()],
            version_in: None,
        };

        install(&installation, &vulkan, &vulkan.releases[0], &archive).unwrap();
        assert_eq!(read(&installation, "d3d9.dll"), "32-bit");
        assert!(!root.join("dxvk-1.0").exists(), "the rest stays out");
        assert_eq!(status(root, &vulkan).unwrap().problems, []);

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

        assert_eq!(foreign(root, &game_mod, newest), ["version.dll"]);
        assert!(matches!(
            install(&installation, &game_mod, newest, &new_zip),
            Err(Error::Foreign(files)) if files == ["version.dll"]
        ));
        assert_eq!(read(&installation, "version.dll"), "another mod's loader");
        assert!(!root.join("Glow.dll").exists(), "nothing changed");

        // There already, broken: a repair takes its files for its own.
        fs::write(root.join("Glow.dll"), "patched").unwrap();
        assert!(status(root, &game_mod).is_some());
        assert_eq!(foreign(root, &game_mod, newest), [] as [String; 0]);
        install(&installation, &game_mod, newest, &new_zip).unwrap();
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

        let installed = install(&installation, &game_mod, &game_mod.releases[0], &tampered);

        assert!(matches!(&installed, Err(Error::Checksum(file)) if file == "Glow.dll"));
        assert_eq!(status(installation.root(), &game_mod), None);
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
                inside: None,
                setup: Vec::new(),
            }],
            settings: Vec::new(),
            writes: Vec::new(),
            config: BTreeMap::new(),
            platforms: Vec::new(),
            version_in: None,
        };
        let (_root, installation) = crate::testing::client(&["Data/enUS/locale-enUS.MPQ"]);

        install(&installation, &patch, &patch.releases[0], b"MPQ\x1a").unwrap();
        assert_eq!(read(&installation, "Data/patch-4.MPQ"), "MPQ\x1a");
        assert_eq!(status(installation.root(), &patch).unwrap().problems, []);

        uninstall(&installation, &patch, false).unwrap();
        assert!(!installation.root().join("Data/patch-4.MPQ").exists());
        assert!(installation.root().join("Data/enUS").is_dir(), "not empty");
    }

    #[test]
    fn uninstalling_keeps_the_players_files_unless_asked_and_another_mods() {
        let (game_mod, new_zip, _) = glow();
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root();

        install(&installation, &game_mod, &game_mod.releases[0], &new_zip).unwrap();
        fs::write(root.join("glow.bin"), "").unwrap();
        uninstall(&installation, &game_mod, true).unwrap();
        assert!(!root.join("Glow").exists(), "an empty folder goes too");
        assert!(!root.join("glow.bin").exists());
        assert_eq!(read(&installation, "Glow.ini"), "Enable=1\n");
        assert_eq!(status(root, &game_mod), None);

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

    #[test]
    fn the_config_it_needs_is_a_problem_only_when_set_otherwise() {
        let (game_mod, new_zip, _) = glow();
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root();
        install(&installation, &game_mod, &game_mod.releases[0], &new_zip).unwrap();
        fs::write(root.join("glow.bin"), "").unwrap();
        finish_setup(&installation, &game_mod, &game_mod.releases[0]).unwrap();
        assert_eq!(status(root, &game_mod).unwrap().problems, []);

        installation.set("gxApi", "D3D9Ex").unwrap();
        assert_eq!(
            status(root, &game_mod).unwrap().problems,
            [Problem::Config(vec![(
                "gxApi".to_owned(),
                "D3D9Ex".to_owned(),
                "d3d9".to_owned()
            )])]
        );
    }
}
