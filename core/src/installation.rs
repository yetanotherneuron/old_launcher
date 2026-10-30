//! A WoW 3.3.5a client directory, and every change the launcher makes to it.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use crate::config::ClientTorrent;
use crate::error::io_at;
use crate::{
    Addon, Adoption, Error, Locale, Marker, SavedVariables, Snapshot, Toc, Version, archive,
    layout, torrent,
};

/// A validated 3.3.5a client directory.
///
/// Every path inside it is resolved case-insensitively, since clients copied
/// from Windows onto case-sensitive filesystems keep arbitrary casing
/// (`Interface/Addons`, `WTF/account`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installation {
    root: PathBuf,
    executable: PathBuf,
    version: Version,
    locale: Option<Locale>,
}

/// What a client folder holds, for people; see [`Details::read`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Details {
    /// Every file in the folder, in bytes.
    pub size: u64,
    /// The languages the client has data for, sorted.
    pub locales: Vec<Locale>,
    /// The one it plays in; see [`Installation::locale`].
    pub locale: Option<Locale>,
    /// Which server the client connects to, when it has a `realmlist.wtf`.
    pub realmlist: Option<Realmlist>,
    /// Whether `Wow.exe` may use 4 GB of memory, not just 2; see
    /// [`Installation::set_large_address_aware`]. `None` when its headers
    /// can't be read.
    pub large_address_aware: Option<bool>,
    /// The torrent the folder notes it comes from, if any; see
    /// [`torrent::origin`].
    pub torrent: Option<ClientTorrent>,
}

/// The server a client connects to and where the game gets it from, the way
/// the game reads it; see [`Realmlist::of`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Realmlist {
    /// The `realmlist.wtf` the game runs, or the one of the language's
    /// folder when there is none.
    pub path: PathBuf,
    /// Whether that file exists.
    pub exists: bool,
    /// The server the game connects to, if anything sets one.
    pub server: Option<Server>,
    /// A line of the file that is just a host name, which the game skips
    /// because only `set realmlist <host>` sets the server.
    pub bare: Option<String>,
}

/// A server and the file that sets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    /// The host name.
    pub host: String,
    /// Where the game gets it from.
    pub from: Named,
}

/// The file a server is named in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Named {
    /// The `realmlist.wtf` the game runs.
    Realmlist,
    /// The game's settings, `WTF/Config.wtf`.
    Settings,
}

impl Installation {
    /// Opens the client at `root`, checking that it is a 3.3.5a client.
    ///
    /// The path is canonicalized, so the same client reached through different
    /// spellings (`..`, trailing slashes) is recognized as the same. On
    /// Windows that is done without the `\\?\` prefix `std` adds, which its
    /// shell can't open and nobody wants to read.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, Error> {
        let root = root.into();
        let root = match dunce::canonicalize(&root) {
            Ok(root) => root,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(Error::NotAClient(root));
            }
            Err(error) => return Err(io_at(&root)(error)),
        };
        let executable = child(&root, "Wow.exe");

        if !executable.is_file() {
            return Err(Error::NotAClient(root));
        }

        let bytes = fs::read(&executable).map_err(io_at(&executable))?;
        let version = Version::from_executable(&bytes)
            .ok_or_else(|| Error::UnknownVersion(executable.clone()))?;

        if !version.is_wotlk() {
            return Err(Error::UnsupportedVersion(version));
        }

        let locale = locale(&root);

        Ok(Self {
            root,
            executable,
            version,
            locale,
        })
    }

    /// The client directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `Wow.exe`, as it is spelled on disk.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// The client version read from `Wow.exe`.
    pub fn version(&self) -> Version {
        self.version
    }

    /// The language the client plays in, as its game picks it: the one
    /// `WTF/Config.wtf` sets, or `enUS`, when installed, else the next
    /// installed in the game's own order.
    pub fn locale(&self) -> Option<Locale> {
        self.locale
    }

    /// The languages the client has data for (see [`Details::locales`]),
    /// read from its folder now.
    pub fn locales(&self) -> Vec<Locale> {
        locales(&self.root)
    }

    /// Deletes the data of `locale`, one of the client's languages: its
    /// whole `Data` folder, with the realmlist and the mods' copies in it.
    /// Not the language the game plays in, as it picks it now.
    pub fn remove_locale(&self, locale: Locale) -> Result<(), Error> {
        if !locales(&self.root).contains(&locale) {
            return Err(Error::NoLocale(locale));
        }
        if self::locale(&self.root) == Some(locale) {
            return Err(Error::PlayedLocale(locale));
        }

        let folder = locale.folder(&self.root);
        fs::remove_dir_all(&folder).map_err(io_at(&folder))
    }

    /// Makes the client play in `locale`, one it has data for, from its next
    /// start, as [`Installation::set`] sets it.
    pub fn set_locale(&mut self, locale: Locale) -> Result<(), Error> {
        if !locales(&self.root).contains(&locale) {
            return Err(Error::NoLocale(locale));
        }

        self.set("locale", locale.code())?;

        self.locale = Some(locale);
        Ok(())
    }

    /// What `WTF/Config.wtf` sets `name` to (`gxApi`, …), the way the client
    /// reads it, if anything.
    pub fn setting(&self, name: &str) -> Option<String> {
        config_setting(&self.root, name)
    }

    /// Sets `name` to `value` from the client's next start: `WTF/Config.wtf`
    /// gets one `SET` line for it, in place of any it had, and is made if
    /// missing. Every other line stays byte for byte, and the file keeps its
    /// permissions.
    ///
    /// The file is written beside itself and renamed over, so a crash
    /// mid-write never leaves the client's settings truncated.
    pub fn set(&self, name: &str, value: &str) -> Result<(), Error> {
        let wtf = self.wtf_directory();
        fs::create_dir_all(&wtf).map_err(io_at(&wtf))?;
        let path = child(&wtf, "Config.wtf");
        let config = match fs::read(&path) {
            Ok(config) => config,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(io_at(&path)(error)),
        };

        write_over(&path, &with_setting(&config, name, value))
    }

    /// Lets the game use 4 GB of memory instead of 2 GB (`on`), or takes
    /// that back: the Large Address Aware flag of `Wow.exe`'s headers, which
    /// a 32-bit program needs to be given more than 2 GB on 64-bit Windows,
    /// and under Wine. HD textures and long view distances need it. Nothing
    /// else in the file changes, and like `Config.wtf` it is written beside
    /// itself and renamed over. Not while the game runs: Windows keeps the
    /// file of a running program.
    pub fn set_large_address_aware(&self, on: bool) -> Result<(), Error> {
        set_large_address_aware(&self.executable, on)
    }

    /// `Interface/AddOns`, whether or not it exists yet.
    pub fn addons_directory(&self) -> PathBuf {
        child(&child(&self.root, "Interface"), "AddOns")
    }

    /// `WTF`, whether or not it exists yet.
    pub fn wtf_directory(&self) -> PathBuf {
        child(&self.root, "WTF")
    }

    /// `Data`, whatever its case: where the game's archives are.
    pub fn data_directory(&self) -> PathBuf {
        child(&self.root, "Data")
    }

    /// Reads the installed addons and their data files. Staging folders an
    /// interrupted install left behind, in `Interface/AddOns` and beside
    /// `Wow.exe` for mods, are removed first (see [`sweep_staging`]).
    pub fn scan(&self) -> Result<Snapshot, Error> {
        let addons = self.addons_directory();
        sweep_staging(&addons);
        sweep_staging(&self.root);

        Ok(Snapshot {
            addons: Addon::scan(&addons)?,
            saved_variables: SavedVariables::scan(&self.wtf_directory()),
        })
    }

    /// Installs the addons in `archive` and returns their folder names.
    ///
    /// The archive is extracted to a staging folder inside `Interface/AddOns`
    /// (so moving out of it is a rename on the same filesystem), then every
    /// folder with a `.toc` moves into place under the name its `.toc`
    /// declares, replacing any folder already there. The `replacing` folders
    /// (a previous install of the same entry) are removed first, so folders a
    /// new version dropped do not linger. Each installed folder gets `marker`.
    pub fn install(
        &self,
        archive: &[u8],
        marker: &Marker,
        replacing: &[String],
    ) -> Result<Vec<String>, Error> {
        let staging = self.staging()?;

        archive::extract(archive, staging.path())?;

        self.place(staging.path(), |_| true, Some(marker), replacing)
    }

    /// What [`Installation::import`] would install from `path`, an archive
    /// or a folder the user picked, without changing anything: its addon
    /// folders, each under the name its `.toc` declares, wherever it sits.
    pub fn preview(&self, path: &Path) -> Result<Vec<Found>, Error> {
        if path.is_dir() {
            self.outside_addons(path)?;

            return found(path);
        }

        let temporary = std::env::temp_dir();
        let staging = tempfile::tempdir().map_err(io_at(&temporary))?;

        archive::extract(&fs::read(path).map_err(io_at(path))?, staging.path())?;

        found(staging.path())
    }

    /// Installs the addon folders named `chosen` (as [`Installation::preview`]
    /// names them) from `path`, an archive or a folder the user picked, as
    /// installed by hand: without a marker. As [`Installation::install`]
    /// does, each goes into place under its `.toc` name, wherever it sits,
    /// replacing any folder already there. Only a folder's chosen addon
    /// folders are copied, not the rest of it (a repository's `.git`, its
    /// sources). Returns their folder names.
    pub fn import(&self, path: &Path, chosen: &[String]) -> Result<Vec<String>, Error> {
        let staging = self.staging()?;
        let is_chosen = |name: &str| {
            chosen
                .iter()
                .any(|chosen| chosen.eq_ignore_ascii_case(name))
        };

        if path.is_dir() {
            self.outside_addons(path)?;

            let folders = layout::find(path)?;

            for folder in folders.iter().filter(|folder| is_chosen(&folder.name)) {
                // Its modules are copied on their own, when chosen.
                let modules: Vec<&Path> = folders
                    .iter()
                    .map(|other| other.path.as_path())
                    .filter(|other| *other != folder.path && other.starts_with(&folder.path))
                    .collect();

                copy_folder(
                    &folder.path,
                    &staging.path().join(plain(&folder.name)?),
                    &modules,
                )?;
            }
        } else {
            archive::extract(&fs::read(path).map_err(io_at(path))?, staging.path())?;
        }

        // What isn't placed goes with the staging folder.
        self.place(staging.path(), is_chosen, None, &[])
    }

    /// A staging folder inside `Interface/AddOns`, so moving out of it is a
    /// rename on the same filesystem. It goes when dropped.
    fn staging(&self) -> Result<tempfile::TempDir, Error> {
        let addons = self.addons_directory();
        fs::create_dir_all(&addons).map_err(io_at(&addons))?;

        tempfile::Builder::new()
            .prefix(STAGING_PREFIX)
            .tempdir_in(&addons)
            .map_err(io_at(&addons))
    }

    /// Moves the addon folders in `staging` whose names are `chosen` into
    /// `Interface/AddOns`, under the name their `.toc` declares, replacing
    /// any folder already there, once the `replacing` folders are gone. Each
    /// gets `marker`, when there is one. A module left out is deleted from
    /// inside its addon, where the client wouldn't load it anyway. Returns
    /// their folder names.
    ///
    /// What is replaced moves aside, into a staging folder deleted at the
    /// end: an install cut short leaves no folder half deleted, and the
    /// staging folder is swept later (see [`sweep_staging`]).
    fn place(
        &self,
        staging: &Path,
        chosen: impl Fn(&str) -> bool,
        marker: Option<&Marker>,
        replacing: &[String],
    ) -> Result<Vec<String>, Error> {
        let addons = self.addons_directory();
        let (folders, left_out): (Vec<_>, Vec<_>) = layout::find(staging)?
            .into_iter()
            .partition(|folder| chosen(&folder.name));

        if folders.is_empty() {
            return Err(Error::NoAddon);
        }

        let aside = self.staging()?;
        for name in replacing {
            set_aside(&addons.join(plain(name)?), aside.path())?;
        }

        for folder in &left_out {
            if folders
                .iter()
                .any(|chosen| folder.path.starts_with(&chosen.path))
            {
                fs::remove_dir_all(&folder.path).map_err(io_at(&folder.path))?;
            }
        }

        let mut installed = Vec::with_capacity(folders.len());

        // Modules come after their addon: they move out of it first.
        for folder in folders.into_iter().rev() {
            let target = addons.join(plain(&folder.name)?);

            set_aside(&target, aside.path())?;
            fs::rename(&folder.path, &target).map_err(io_at(&target))?;
            if let Some(marker) = marker {
                marker.write(&target)?;
            }

            installed.push(folder.name);
        }

        installed.reverse();

        Ok(installed)
    }

    /// Refuses `path` when it is in `Interface/AddOns`: its addons are
    /// installed already, and importing them again would only copy them onto
    /// themselves, dropping the markers of those installed from the list.
    fn outside_addons(&self, path: &Path) -> Result<(), Error> {
        let addons = self.addons_directory();
        let inside = match (dunce::canonicalize(path), dunce::canonicalize(&addons)) {
            (Ok(path), Ok(addons)) => path.starts_with(addons),
            _ => false,
        };

        if inside {
            Err(Error::InAddOns(path.to_path_buf()))
        } else {
            Ok(())
        }
    }

    /// Fixes a folder the client cannot load because the addon inside is
    /// nested (`Foo-master/Foo/Foo.toc`, `Foo/Foo/Foo.toc`) or misnamed
    /// (`Foo-master/Foo.toc`). Every addon folder found in it moves up into
    /// `Interface/AddOns` under its `.toc` name, like an install would place
    /// it, and whatever else the folder held is deleted. Returns the new
    /// folder names.
    pub fn repair(&self, folder: &str) -> Result<Vec<String>, Error> {
        let addons = self.addons_directory();
        let broken = addons.join(plain(folder)?);

        // Moving the folder aside first lets a nested addon take its name.
        let staging = self.staging()?;
        let aside = staging.path().join(folder);
        // Where the folders it replaces go, as an install's (see `place`).
        let replaced = self.staging()?;

        fs::rename(&broken, &aside).map_err(io_at(&broken))?;

        let repaired = layout::find(staging.path()).and_then(|folders| {
            if folders.is_empty() {
                return Err(Error::NoAddon);
            }

            let mut repaired = Vec::with_capacity(folders.len());

            // Modules come after their addon: they move out of it first.
            for folder in folders.into_iter().rev() {
                let target = addons.join(plain(&folder.name)?);

                set_aside(&target, replaced.path())?;
                fs::rename(&folder.path, &target).map_err(io_at(&target))?;

                repaired.push(folder.name);
            }

            repaired.reverse();

            Ok(repaired)
        });

        // On failure, whatever was not moved out goes back where it was
        // instead of being deleted with the staging folder.
        if repaired.is_err() && aside.exists() {
            fs::rename(&aside, &broken).map_err(io_at(&broken))?;
        }

        repaired
    }

    /// Writes `adoption`'s marker into its folders, as if the launcher had
    /// installed them, and returns how many it marked. Folders that are gone
    /// or already have a marker are left alone, since the addons may have
    /// changed since the scan.
    pub fn adopt(&self, adoption: &Adoption) -> Result<usize, Error> {
        let addons = self.addons_directory();
        let mut marked = 0;

        for name in &adoption.folders {
            let folder = addons.join(plain(name)?);

            if folder.is_dir() && Marker::read(&folder).is_none() {
                adoption.marker.write(&folder)?;
                marked += 1;
            }
        }

        Ok(marked)
    }

    /// Deletes the named folders from `Interface/AddOns`. Folders that are
    /// already gone are skipped; symlinked folders lose only the link.
    pub fn uninstall(&self, folders: &[String]) -> Result<(), Error> {
        let addons = self.addons_directory();

        for name in folders {
            remove_folder(&addons.join(plain(name)?))?;
        }

        Ok(())
    }

    /// Deletes SavedVariables `files`, which must lie inside `WTF`. Files that
    /// are already gone are skipped.
    pub fn clean(&self, files: &[PathBuf]) -> Result<(), Error> {
        let wtf = self.wtf_directory();

        for file in files {
            let inside = file.starts_with(&wtf)
                && file
                    .components()
                    .all(|component| !matches!(component, Component::ParentDir));

            if !inside {
                return Err(Error::OutsideWtf(file.clone()));
            }

            match fs::remove_file(file) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    return Err(io_at(file)(error));
                }
                _ => {}
            }
        }

        Ok(())
    }
}

impl Details {
    /// Reads the client folder at `root`. Every file in it is measured, which
    /// takes a moment on a whole client; what can't be read counts as empty.
    pub fn read(root: &Path) -> Result<Self, Error> {
        Ok(Self {
            size: size(root)?,
            locales: locales(root),
            locale: locale(root),
            realmlist: Realmlist::of(root),
            large_address_aware: fs::read(child(root, "Wow.exe"))
                .ok()
                .and_then(|bytes| large_address_aware(&bytes)),
            torrent: torrent::origin(root),
        })
    }
}

impl Realmlist {
    /// The server of the client at `root`, found the way the game does: it
    /// runs the first of `Data/<language>/realmlist.wtf`,
    /// `WTF/data/<language>/realmlist.wtf`, `realmlist.wtf` in the client
    /// folder and `WTF/realmlist.wtf` that exists (even an empty one, with no
    /// fallback), and the last `set realmlist` in it wins. When the file sets
    /// none, the `SET realmList` of `WTF/Config.wtf` stays in effect.
    ///
    /// `None` only when the language the client plays in is unknown.
    pub fn of(root: &Path) -> Option<Self> {
        let locale = locale(root)?;
        let code = locale.code();
        let data = locale.folder(root);
        let wtf = child(root, "WTF");
        let candidates = [
            child(&data, "realmlist.wtf"),
            child(&child(&child(&wtf, "data"), code), "realmlist.wtf"),
            child(root, "realmlist.wtf"),
            child(&wtf, "realmlist.wtf"),
        ];
        let found = candidates.iter().find(|path| path.is_file());
        let (server, bare) = found.map_or((None, None), |path| realmlist_lines(path));
        let server = server
            .map(|host| (host, Named::Realmlist))
            .or_else(|| config_setting(root, "realmlist").map(|host| (host, Named::Settings)))
            .map(|(host, from)| Server { host, from });

        Some(Self {
            exists: found.is_some(),
            path: found.unwrap_or(&candidates[0]).clone(),
            server,
            bare,
        })
    }
}

/// What the `realmlist.wtf` at `path` sets as the server, the last
/// `set realmlist` winning, and its last line that is only a host name, which
/// the game skips.
fn realmlist_lines(path: &Path) -> (Option<String>, Option<String>) {
    let Ok(bytes) = fs::read(path) else {
        return (None, None);
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut server = None;
    let mut bare = None;

    for line in text.trim_start_matches('\u{feff}').lines() {
        if let Some((name, value)) = set_line(line) {
            if name.eq_ignore_ascii_case("realmlist") && !value.is_empty() {
                server = Some(value);
            }
            continue;
        }

        let mut words = line.split_whitespace();
        if let (Some(word), None) = (words.next(), words.next())
            && word.contains('.')
            && !word.starts_with('#')
            && !word.starts_with("//")
        {
            bare = Some(word.to_owned());
        }
    }

    (server, bare)
}

/// The flag of a Windows program's headers (`IMAGE_FILE_LARGE_ADDRESS_AWARE`)
/// that lets a 32-bit program use 4 GB of memory instead of 2 GB.
pub(crate) const LARGE_ADDRESS_AWARE: u16 = 0x0020;

/// Where the characteristics of the Windows program in `bytes` are, if it
/// is one: in its file header, after the `PE\0\0` its DOS header points to,
/// and the machine, section count, time stamp, symbols and optional header
/// size (18 bytes).
pub(crate) fn characteristics(bytes: &[u8]) -> Option<usize> {
    let header = u32::from_le_bytes(bytes.get(0x3C..0x40)?.try_into().ok()?) as usize;
    let offset = header.checked_add(4 + 18)?;

    (bytes.starts_with(b"MZ")
        && bytes.get(header..header.checked_add(4)?)? == b"PE\0\0"
        && bytes.len() >= offset + 2)
        .then_some(offset)
}

/// Whether the Windows program in `bytes` may use 4 GB of memory, if it is
/// one.
pub(crate) fn large_address_aware(bytes: &[u8]) -> Option<bool> {
    let offset = characteristics(bytes)?;
    let flags = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);

    Some(flags & LARGE_ADDRESS_AWARE != 0)
}

/// Sets or clears the Large Address Aware flag of the Windows program at
/// `path`; see [`Installation::set_large_address_aware`].
pub(crate) fn set_large_address_aware(path: &Path, on: bool) -> Result<(), Error> {
    let mut bytes = fs::read(path).map_err(io_at(path))?;
    let offset = characteristics(&bytes).ok_or_else(|| Error::NoHeaders(path.to_path_buf()))?;

    let flags = u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
    let wanted = if on {
        flags | LARGE_ADDRESS_AWARE
    } else {
        flags & !LARGE_ADDRESS_AWARE
    };
    if wanted == flags {
        return Ok(());
    }
    bytes[offset..offset + 2].copy_from_slice(&wanted.to_le_bytes());

    write_over(path, &bytes)
}

/// Writes `bytes` over the file at `path`, beside it first and renamed
/// over, so a crash mid-write never leaves it truncated. A file it replaces
/// keeps its permissions.
fn write_over(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let permissions = fs::metadata(path)
        .ok()
        .map(|metadata| metadata.permissions());
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);

    fs::write(&partial, bytes).map_err(io_at(&partial))?;
    if let Some(permissions) = permissions {
        fs::set_permissions(&partial, permissions).map_err(io_at(&partial))?;
    }
    fs::rename(&partial, path).map_err(io_at(path))
}

/// The language the client at `root` plays in, as its game picks it. In a
/// whole client (with `Data/common.MPQ`), the first installed (see
/// [`Locale::installed`]) in the game's order ([`Locale::GAME_ORDER`]),
/// looking from the one `WTF/Config.wtf` sets onward and around; from
/// `enUS` when it sets none, or one the game doesn't know. The game then
/// writes it into `Config.wtf`. Otherwise, or with none installed: the one
/// `Config.wtf` sets when the client has data for it, or else the first it
/// has data for.
pub(crate) fn locale(root: &Path) -> Option<Locale> {
    let set = setting(&child(&child(root, "WTF"), "Config.wtf"), "locale")
        .as_deref()
        .and_then(Locale::parse);

    if child(&child(root, "Data"), "common.MPQ").is_file() {
        let start = set.unwrap_or(Locale::EnUs);
        let from = Locale::GAME_ORDER
            .iter()
            .position(|locale| *locale == start)
            .unwrap_or_default();
        let installed = Locale::GAME_ORDER
            .into_iter()
            .cycle()
            .skip(from)
            .take(Locale::GAME_ORDER.len())
            .find(|locale| locale.installed(root));

        if installed.is_some() {
            return installed;
        }
    }

    let locales = locales(root);
    set.filter(|set| locales.contains(set))
        .or_else(|| locales.into_iter().next())
}

/// The languages the client at `root` has data for: its `Data` folders of a
/// language, whatever their case, sorted.
pub(crate) fn locales(root: &Path) -> Vec<Locale> {
    let Ok(entries) = fs::read_dir(child(root, "Data")) else {
        return Vec::new();
    };

    let mut locales: Vec<Locale> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| Locale::parse(&entry.file_name().to_string_lossy()))
        .collect();
    locales.sort();
    locales.dedup();

    locales
}

/// What `WTF/Config.wtf` of the client at `root` sets `name` to, if
/// anything; see [`Installation::setting`].
pub(crate) fn config_setting(root: &Path, name: &str) -> Option<String> {
    setting(&child(&child(root, "WTF"), "Config.wtf"), name)
}

/// What the `.wtf` file at `path` sets `name` to, the way the client reads
/// it: every line setting it in turn, so the last one wins.
fn setting(path: &Path, name: &str) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);

    text.trim_start_matches('\u{feff}')
        .lines()
        .rev()
        .filter_map(set_line)
        .find_map(|(key, value)| {
            (key.eq_ignore_ascii_case(name) && !value.is_empty()).then_some(value)
        })
}

/// What a `.wtf` line sets, and to what: `set <name> <value>`, in any case,
/// the value maybe quoted. Like the game, only a line that starts with `set`
/// and a space counts: one with spaces or tabs before it is ignored.
fn set_line(line: &str) -> Option<(&str, String)> {
    let line = line.trim_end_matches(['\r', '\n']);
    if !line.get(..4)?.eq_ignore_ascii_case("set ") {
        return None;
    }
    let mut words = line.get(4..)?.split_whitespace();
    let name = words.next()?;
    let value = words.collect::<Vec<_>>().join(" ");

    Some((name, value.trim_matches('"').to_owned()))
}

/// The `.wtf` file `file` with `name` set to `value` by one line, where the
/// first line setting it was, or else at the end, and no other line setting
/// it. The rest stays byte for byte, a byte order mark included, and the
/// new line ends the way the file's lines do.
fn with_setting(file: &[u8], name: &str, value: &str) -> Vec<u8> {
    const BOM: &[u8] = "\u{feff}".as_bytes();

    let ending: &[u8] = if file.windows(2).any(|pair| pair == b"\r\n") {
        b"\r\n"
    } else {
        b"\n"
    };
    let setting = [format!("SET {name} \"{value}\"").as_bytes(), ending].concat();
    let body = file.strip_prefix(BOM);
    let mut written = body.map_or_else(Vec::new, |_| BOM.to_vec());
    let mut set = false;

    for line in body.unwrap_or(file).split_inclusive(|&byte| byte == b'\n') {
        let sets = set_line(&String::from_utf8_lossy(line))
            .is_some_and(|(key, _)| key.eq_ignore_ascii_case(name));

        match (sets, set) {
            (false, _) => written.extend_from_slice(line),
            (true, false) => written.extend_from_slice(&setting),
            (true, true) => {}
        }
        set |= sets;
    }

    if !set {
        if !written.is_empty() && !written.ends_with(b"\n") && written != BOM {
            written.extend_from_slice(ending);
        }
        written.extend_from_slice(&setting);
    }

    written
}

/// The bytes in every file under `root`, symlinks not followed. Folders
/// that can't be read inside it count as empty.
fn size(root: &Path) -> Result<u64, Error> {
    let mut size = 0;
    let mut pending = vec![root.to_path_buf()];
    let mut first = true;

    while let Some(folder) = pending.pop() {
        let entries = match fs::read_dir(&folder) {
            Ok(entries) => entries,
            Err(error) if first => return Err(io_at(&folder)(error)),
            Err(_) => continue,
        };
        first = false;

        for entry in entries.filter_map(Result::ok) {
            // Doesn't follow symlinks, so a link to a folder isn't walked.
            let Ok(metadata) = entry.metadata() else {
                continue;
            };

            if metadata.is_dir() {
                pending.push(entry.path());
            } else {
                size += metadata.len();
            }
        }
    }

    Ok(size)
}

/// The prefix of the staging folders installs and repairs extract into,
/// inside `Interface/AddOns`, and mod installs beside `Wow.exe`.
pub(crate) const STAGING_PREFIX: &str = ".old_launcher-";

/// How long a staging folder gets to be in use before it counts as left
/// behind by an install that never finished (the launcher was closed
/// halfway through, say).
const STAGING_AGE: Duration = Duration::from_secs(60 * 60);

/// Deletes the staging folders in `folder` that an interrupted install left
/// behind: those untouched for [`STAGING_AGE`], so one in use is left alone.
/// Only folders: files beside `Wow.exe` share the prefix (0.9.0's mod
/// markers, a file being moved in). What can't be read or removed is left
/// for next time.
fn sweep_staging(folder: &Path) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };

    for entry in entries.filter_map(Result::ok) {
        let stale = entry
            .file_name()
            .to_string_lossy()
            .starts_with(STAGING_PREFIX)
            && entry.file_type().is_ok_and(|kind| kind.is_dir())
            && entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .and_then(|modified| modified.elapsed().map_err(io::Error::other))
                .is_ok_and(|age| age >= STAGING_AGE);

        if stale {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Whether the file system finds a name whatever its case, as Windows' does
/// (and macOS' by default): there, a name that exists may be spelled
/// otherwise on disk.
const CASE_BLIND: bool = cfg!(any(windows, target_os = "macos"));

/// The entry of `directory` named `name` ignoring case, as it is spelled
/// there (`name` itself first, where case tells names apart), or
/// `directory/name` when there is none.
pub(crate) fn child(directory: &Path, name: &str) -> PathBuf {
    let exact = directory.join(name);

    if !CASE_BLIND && exact.exists() {
        return exact;
    }

    fs::read_dir(directory)
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(Result::ok)
                .find(|entry| {
                    entry
                        .file_name()
                        .to_string_lossy()
                        .eq_ignore_ascii_case(name)
                })
                .map(|entry| entry.path())
        })
        .unwrap_or(exact)
}

/// Checks that an addon folder name is one plain path component, so joining
/// it to `Interface/AddOns` can never point anywhere else.
fn plain(name: &str) -> Result<&str, Error> {
    let mut components = Path::new(name).components();
    let is_plain = matches!(components.next(), Some(Component::Normal(_)))
        && components.next().is_none()
        && !name.contains(['/', '\\', '\0']);

    if is_plain {
        Ok(name)
    } else {
        Err(Error::InvalidName(name.to_owned()))
    }
}

/// An addon folder in an archive or folder the user picked to install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// The name it gets in `Interface/AddOns`: its `.toc` file's stem.
    pub name: String,
    /// Where it is in the archive or folder, with `/` between folders: the
    /// same as `name` unless it moves or is renamed on the way in.
    pub from: String,
    /// Its `.toc`.
    pub toc: Toc,
}

/// The addon folders in `root`, as installing them would place them, with
/// their `.toc` read.
fn found(root: &Path) -> Result<Vec<Found>, Error> {
    let folders = layout::find(root)?;

    if folders.is_empty() {
        return Err(Error::NoAddon);
    }

    folders
        .into_iter()
        .map(|folder| {
            let toc = Toc::parse(&fs::read(&folder.toc).map_err(io_at(&folder.toc))?);
            let from = match folder.path.strip_prefix(root) {
                // The folder picked is the addon folder itself.
                Ok(relative) if relative.as_os_str().is_empty() => root
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                Ok(relative) => relative
                    .components()
                    .map(|component| component.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
                Err(_) => folder.path.display().to_string(),
            };

            Ok(Found {
                name: folder.name,
                from,
                toc,
            })
        })
        .collect()
}

/// Copies the folder `from`, and everything in it but the folders `leaving`,
/// to `to`.
fn copy_folder(from: &Path, to: &Path, leaving: &[&Path]) -> Result<(), Error> {
    fs::create_dir_all(to).map_err(io_at(to))?;

    for entry in fs::read_dir(from).map_err(io_at(from))? {
        let source = entry.map_err(io_at(from))?.path();
        let target = to.join(source.file_name().unwrap_or_default());

        if leaving.contains(&source.as_path()) {
            continue;
        }

        if source.is_dir() {
            copy_folder(&source, &target, leaving)?;
        } else {
            let _ = fs::copy(&source, &target).map_err(io_at(&source))?;
        }
    }

    Ok(())
}

/// Moves the addon folder at `path`, or just the link if it is a symlink,
/// into the staging folder `aside`, when there is one: renamed, it is never
/// left half deleted.
fn set_aside(path: &Path, aside: &Path) -> Result<(), Error> {
    let Some(name) = path.file_name() else {
        return Ok(());
    };
    if fs::symlink_metadata(path).is_err() {
        return Ok(());
    }

    fs::rename(path, aside.join(name)).map_err(io_at(path))
}

/// Removes an addon folder, or just the link if it is a symlink.
fn remove_folder(path: &Path) -> Result<(), Error> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };

    let removal = if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };

    removal.map_err(io_at(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::zip_of;
    use crate::testing::{WOTLK, fake_executable};

    fn client() -> (tempfile::TempDir, Installation) {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("Wow.exe"), fake_executable(WOTLK)).unwrap();
        fs::create_dir_all(root.path().join("Data/esES")).unwrap();
        fs::create_dir_all(root.path().join("Interface/AddOns")).unwrap();

        let installation = Installation::open(root.path()).unwrap();

        (root, installation)
    }

    fn marker(entry: &str) -> Marker {
        Marker {
            source: "test".into(),
            entry: entry.into(),
            url: format!("https://example.com/{entry}.zip"),
        }
    }

    fn folders(installation: &Installation) -> Vec<String> {
        installation
            .scan()
            .unwrap()
            .addons
            .into_iter()
            .map(|addon| addon.name)
            .collect()
    }

    #[test]
    fn a_wotlk_client_opens_with_its_version_and_locale() {
        let (_root, installation) = client();

        assert_eq!(installation.version(), WOTLK);
        assert_eq!(installation.locale(), Some(Locale::EsEs));
    }

    #[test]
    fn details_tell_the_size_languages_and_server() {
        let (root, installation) = client();
        fs::create_dir_all(root.path().join("Data/enUS")).unwrap();
        fs::write(root.path().join("Data/enUS/realmlist.wtf"), "x").unwrap();
        fs::write(
            root.path().join("Data/esES/realmlist.wtf"),
            "\u{feff}set patchlist localhost\r\nSET realmList \"logon.example.com\"\r\n",
        )
        .unwrap();

        let details = Details::read(installation.root()).unwrap();

        assert_eq!(details.locales, [Locale::EnUs, Locale::EsEs]);
        assert_eq!(details.locale, Some(Locale::EnUs));
        let size = [
            "Wow.exe",
            "Data/enUS/realmlist.wtf",
            "Data/esES/realmlist.wtf",
        ]
        .iter()
        .map(|file| fs::metadata(root.path().join(file)).unwrap().len())
        .sum::<u64>();
        assert_eq!(details.size, size);
        // Nothing says which language, so the first: its file sets none.
        assert_eq!(
            details.realmlist,
            Some(Realmlist {
                path: installation.root().join("Data/enUS/realmlist.wtf"),
                exists: true,
                server: None,
                bare: None,
            })
        );
        assert_eq!(details.large_address_aware, Some(false), "as shipped");
    }

    #[test]
    fn the_game_is_given_4_gb_of_memory_and_back_with_one_flag_of_wow_exe() {
        let (root, installation) = client();
        let exe = root.path().join("Wow.exe");
        let shipped = fs::read(&exe).unwrap();
        let aware = || {
            Details::read(installation.root())
                .unwrap()
                .large_address_aware
        };

        installation.set_large_address_aware(true).unwrap();
        assert_eq!(aware(), Some(true));
        let changed: Vec<usize> = fs::read(&exe)
            .unwrap()
            .iter()
            .zip(&shipped)
            .enumerate()
            .filter(|(_, (now, before))| now != before)
            .map(|(offset, _)| offset)
            .collect();
        assert_eq!(changed, [0x126], "the characteristics' low byte alone");
        installation.set_large_address_aware(true).unwrap();
        assert_eq!(aware(), Some(true), "set already: nothing to do");

        installation.set_large_address_aware(false).unwrap();
        assert_eq!(fs::read(&exe).unwrap(), shipped, "byte for byte");
        assert!(!root.path().join("Wow.exe.partial").exists());

        // Its version found, but no headers around it.
        fs::write(&exe, &shipped[0x400..]).unwrap();
        assert_eq!(aware(), None);
        assert!(matches!(
            installation.set_large_address_aware(true),
            Err(Error::NoHeaders(_))
        ));
    }

    #[test]
    fn the_language_played_in_has_the_realmlist_that_counts() {
        let (root, _) = client();
        fs::create_dir_all(root.path().join("Data/enUS")).unwrap();
        fs::write(
            root.path().join("Data/esES/realmlist.wtf"),
            "\u{feff}set realmlist old.example.com\r\nset patchlist localhost\r\n\
             SET realmList \"logon.example.com\"\r\nset realmlistbn \"\"\r\n\
             #set realmlist commented.example.com\r\n",
        )
        .unwrap();
        fs::create_dir_all(root.path().join("WTF")).unwrap();
        fs::write(
            root.path().join("WTF/Config.wtf"),
            "SET locale \"deDE\"\nSET gxResolution \"1920x1080\"\nSET locale \"esES\"\n",
        )
        .unwrap();

        let installation = Installation::open(root.path()).unwrap();

        assert_eq!(installation.locale(), Some(Locale::EsEs));
        assert_eq!(
            Realmlist::of(installation.root()),
            Some(Realmlist {
                path: installation.root().join("Data/esES/realmlist.wtf"),
                exists: true,
                server: Some(Server {
                    host: "logon.example.com".into(),
                    from: Named::Realmlist,
                }),
                bare: None,
            }),
            "the last line setting it"
        );

        // A language the client has no data for doesn't count: the game
        // plays in the first it has, and that one's folder has no
        // realmlist.wtf.
        fs::write(root.path().join("WTF/Config.wtf"), "SET locale \"deDE\"\n").unwrap();
        assert_eq!(
            Installation::open(root.path()).unwrap().locale(),
            Some(Locale::EnUs)
        );
        let none = Realmlist::of(installation.root()).unwrap();
        assert!(!none.exists);
        assert_eq!(none.server, None);
    }

    #[test]
    fn the_realmlist_is_read_the_way_the_game_runs_it() {
        let (root, _) = client();
        let data = root.path().join("Data/esES/realmlist.wtf");
        let config = root.path().join("WTF/Config.wtf");
        let host = |from| {
            Realmlist::of(root.path())
                .and_then(|realmlist| realmlist.server)
                .map(|server| (server.host, server.from == from))
        };

        // Nothing anywhere.
        let nothing = Realmlist::of(root.path()).unwrap();
        assert_eq!(nothing.server, None);
        assert!(!nothing.exists);
        assert_eq!(nothing.path, data);

        // The last line wins, whatever the case and the line endings.
        fs::write(&data, "set realmlist a\r\nSET realmlist b").unwrap();
        assert_eq!(host(Named::Realmlist), Some(("b".to_owned(), true)));

        // A line with a space before it is not run; the settings stay.
        fs::create_dir_all(root.path().join("WTF")).unwrap();
        fs::write(&config, "SET locale \"esES\"\nSET realmList \"d\"\n").unwrap();
        fs::write(&data, " set realmlist c\nlogon.example.com").unwrap();
        assert_eq!(host(Named::Settings), Some(("d".to_owned(), true)));
        let realmlist = Realmlist::of(root.path()).unwrap();
        assert!(realmlist.exists);
        assert_eq!(realmlist.bare.as_deref(), Some("logon.example.com"));

        // A comment is not a host.
        fs::write(&data, "#logon.example.com\n//other.example.com").unwrap();
        assert_eq!(Realmlist::of(root.path()).unwrap().bare, None);

        // An empty file is run, so the others are not looked at.
        fs::write(&data, "").unwrap();
        fs::write(root.path().join("realmlist.wtf"), "set realmlist root").unwrap();
        let empty = Realmlist::of(root.path()).unwrap();
        assert_eq!(empty.path, data);
        assert_eq!(empty.server.map(|server| server.host), Some("d".into()));

        // Without the language's file, the one in the client folder.
        fs::remove_file(&data).unwrap();
        let root_file = Realmlist::of(root.path()).unwrap();
        assert!(root_file.exists);
        assert_eq!(root_file.path, root.path().join("realmlist.wtf"));
        assert_eq!(
            root_file.server,
            Some(Server {
                host: "root".into(),
                from: Named::Realmlist
            })
        );

        // A client with lowercase folders, as the HD one.
        fs::remove_file(root.path().join("realmlist.wtf")).unwrap();
        fs::rename(root.path().join("Data/esES"), root.path().join("Data/eses")).unwrap();
        fs::write(
            root.path().join("Data/eses/realmlist.wtf"),
            "set realmlist hd",
        )
        .unwrap();
        assert_eq!(host(Named::Realmlist), Some(("hd".to_owned(), true)));
    }

    #[test]
    fn a_line_with_spaces_before_set_sets_nothing() {
        let (root, installation) = client();
        fs::create_dir_all(root.path().join("WTF")).unwrap();
        fs::write(
            root.path().join("WTF/Config.wtf"),
            "SET gxApi \"d3d9\"\n SET gxApi \"opengl\"\n\tSET gxApi \"d3d11\"\r\n",
        )
        .unwrap();

        assert_eq!(installation.setting("gxApi").as_deref(), Some("d3d9"));
    }

    #[test]
    fn choosing_a_language_rewrites_only_its_line_in_config_wtf() {
        let (root, _) = client();
        fs::create_dir_all(root.path().join("Data/enUS")).unwrap();
        fs::write(
            root.path().join("Data/esES/realmlist.wtf"),
            "set realmlist es.example.com",
        )
        .unwrap();
        fs::create_dir_all(root.path().join("wtf")).unwrap();
        fs::write(
            root.path().join("wtf/config.wtf"),
            "\u{feff}SET gxResolution \"1920x1080\"\r\nSET locale \"enUS\"\r\n\
             SET realmName \"Icecrown\"\r\nset LOCALE enGB\r\nSET accountName \"PLAYER\"",
        )
        .unwrap();
        let mut installation = Installation::open(root.path()).unwrap();
        assert_eq!(
            installation.locale(),
            Some(Locale::EnUs),
            "enGB has no data"
        );

        installation.set_locale(Locale::EsEs).unwrap();

        assert_eq!(
            fs::read_to_string(root.path().join("wtf/config.wtf")).unwrap(),
            "\u{feff}SET gxResolution \"1920x1080\"\r\nSET locale \"esES\"\r\n\
             SET realmName \"Icecrown\"\r\nSET accountName \"PLAYER\"",
            "in place of the first, the others gone, the file spelled as it was"
        );
        assert_eq!(installation.locale(), Some(Locale::EsEs));
        assert_eq!(
            Installation::open(root.path()).unwrap().locale(),
            Some(Locale::EsEs)
        );
        assert_eq!(
            Realmlist::of(installation.root())
                .and_then(|realmlist| realmlist.server)
                .map(|server| server.host),
            Some("es.example.com".into()),
            "the realmlist follows"
        );
        assert_eq!(
            fs::read_dir(root.path().join("wtf")).unwrap().count(),
            1,
            "nothing left beside it"
        );
    }

    #[test]
    fn a_language_is_set_in_a_config_wtf_made_or_extended_for_it() {
        let (root, mut installation) = client();

        installation.set_locale(Locale::EsEs).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("WTF/Config.wtf")).unwrap(),
            "SET locale \"esES\"\n"
        );

        fs::write(root.path().join("WTF/Config.wtf"), "SET gxWindow \"1\"").unwrap();
        installation.set_locale(Locale::EsEs).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("WTF/Config.wtf")).unwrap(),
            "SET gxWindow \"1\"\nSET locale \"esES\"\n",
            "after the last line, ended first"
        );
    }

    #[cfg(unix)]
    #[test]
    fn choosing_a_language_keeps_the_permissions_of_config_wtf() {
        use std::os::unix::fs::PermissionsExt;

        let (root, mut installation) = client();
        let path = root.path().join("WTF/Config.wtf");
        fs::create_dir_all(root.path().join("WTF")).unwrap();
        fs::write(&path, "SET locale \"enUS\"\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o777)).unwrap();

        installation.set_locale(Locale::EsEs).unwrap();

        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o777
        );
    }

    #[test]
    fn the_language_is_the_one_the_game_picks() {
        let picked = |files: &[&str], config: Option<&str>| {
            let mut files: Vec<(String, String)> = files
                .iter()
                .map(|file| ((*file).to_owned(), String::new()))
                .collect();
            if let Some(locale) = config {
                files.push((
                    "WTF/Config.wtf".to_owned(),
                    format!("SET locale \"{locale}\"\n"),
                ));
            }
            let files: Vec<(&str, &str)> = files
                .iter()
                .map(|(path, contents)| (path.as_str(), contents.as_str()))
                .collect();
            let (_root, installation) = crate::testing::client_with(&files);

            installation.locale()
        };
        let whole = |locales: &[&str]| -> Vec<String> {
            locales
                .iter()
                .map(|code| format!("Data/{code}/locale-{code}.MPQ"))
                .chain(["Data/common.MPQ".to_owned()])
                .collect()
        };
        fn files(paths: &[String]) -> Vec<&str> {
            paths.iter().map(String::as_str).collect()
        }

        let both = whole(&["deDE", "enUS"]);
        assert_eq!(picked(&files(&both), None), Some(Locale::EnUs));
        let no_english = whole(&["deDE", "enGB"]);
        assert_eq!(
            picked(&files(&no_english), None),
            Some(Locale::DeDe),
            "from enUS onward, and around"
        );
        let two = whole(&["enUS", "ruRU"]);
        assert_eq!(
            picked(&files(&two), Some("frFR")),
            Some(Locale::RuRu),
            "set without data: the next installed"
        );
        let brazilian = whole(&["enUS", "ptBR"]);
        assert_eq!(
            picked(&files(&brazilian), Some("ptBR")),
            Some(Locale::EnUs),
            "one the game doesn't know"
        );
        let only_brazilian = whole(&["ptBR"]);
        assert_eq!(
            picked(&files(&only_brazilian), Some("ptBR")),
            None,
            "no folder of a language the game knows"
        );
        let lowercase = whole(&["enus"]);
        assert_eq!(
            picked(&files(&lowercase), None),
            Some(Locale::EnUs),
            "found whatever its case"
        );

        // Not a whole client: the first it has data for.
        let partial = ["Data/deDE/locale-deDE.MPQ", "Data/enUS/locale-enUS.MPQ"];
        assert_eq!(picked(&partial, None), Some(Locale::DeDe));
    }

    #[test]
    fn a_language_without_data_is_refused() {
        let (root, mut installation) = client();
        fs::create_dir_all(root.path().join("WTF")).unwrap();
        fs::write(root.path().join("WTF/Config.wtf"), "SET locale \"esES\"\n").unwrap();

        assert!(matches!(
            installation.set_locale(Locale::DeDe),
            Err(Error::NoLocale(Locale::DeDe))
        ));
        assert_eq!(
            fs::read_to_string(root.path().join("WTF/Config.wtf")).unwrap(),
            "SET locale \"esES\"\n"
        );
        assert_eq!(installation.locale(), Some(Locale::EsEs));
    }

    #[test]
    fn a_language_is_deleted_whole_but_the_one_the_game_plays_in() {
        let (root, installation) = client();
        fs::create_dir_all(root.path().join("Data/frFR")).unwrap();
        fs::write(root.path().join("Data/frFR/realmlist.wtf"), "").unwrap();
        fs::create_dir_all(root.path().join("WTF")).unwrap();
        fs::write(root.path().join("WTF/Config.wtf"), "SET locale \"esES\"\n").unwrap();

        assert!(matches!(
            installation.remove_locale(Locale::EsEs),
            Err(Error::PlayedLocale(Locale::EsEs))
        ));
        assert!(matches!(
            installation.remove_locale(Locale::DeDe),
            Err(Error::NoLocale(_))
        ));

        installation.remove_locale(Locale::FrFr).unwrap();
        assert!(!root.path().join("Data/frFR").exists());
        assert_eq!(installation.locales(), [Locale::EsEs]);
    }

    #[test]
    fn details_of_a_missing_folder_are_an_error() {
        let (root, installation) = client();
        drop(root);

        assert!(matches!(
            Details::read(installation.root()),
            Err(Error::Io { .. })
        ));
    }

    #[test]
    fn roots_are_canonicalized() {
        let (root, installation) = client();
        let detour = root.path().join("Data/../.");

        assert_eq!(Installation::open(detour).unwrap(), installation);
    }

    #[test]
    fn directories_without_wow_exe_are_not_clients() {
        let root = tempfile::tempdir().unwrap();

        assert!(matches!(
            Installation::open(root.path()),
            Err(Error::NotAClient(_))
        ));
    }

    #[test]
    fn other_client_versions_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let vanilla = Version {
            major: 1,
            minor: 12,
            patch: 1,
            build: 5875,
        };
        fs::write(root.path().join("WoW.EXE"), fake_executable(vanilla)).unwrap();

        assert!(matches!(
            Installation::open(root.path()),
            Err(Error::UnsupportedVersion(version)) if version == vanilla
        ));
    }

    #[test]
    fn paths_resolve_whatever_their_casing() {
        let (root, _) = client();
        fs::rename(
            root.path().join("Interface/AddOns"),
            root.path().join("Interface/Addons"),
        )
        .unwrap();
        fs::create_dir(root.path().join("wtf")).unwrap();

        let installation = Installation::open(root.path()).unwrap();

        // Canonicalized on both sides: a folder is reachable under any
        // spelling where the file system ignores case, and the launcher
        // need not pick the one on disk there.
        let existing = |relative: &str| dunce::canonicalize(root.path().join(relative)).unwrap();
        assert_eq!(
            dunce::canonicalize(installation.addons_directory()).unwrap(),
            existing("Interface/Addons")
        );
        assert_eq!(
            dunce::canonicalize(installation.wtf_directory()).unwrap(),
            existing("wtf")
        );
    }

    #[test]
    fn installing_a_branch_snapshot_places_each_addon_with_a_marker() {
        let (_root, installation) = client();
        let archive = zip_of(&[
            ("Skada-main/Skada/Skada.toc", "## Title: Skada"),
            ("Skada-main/SkadaImprovement/SkadaImprovement.toc", ""),
            ("Skada-main/README.md", ""),
        ]);

        let installed = installation
            .install(&archive, &marker("Skada"), &[])
            .unwrap();

        assert_eq!(installed, ["Skada", "SkadaImprovement"]);
        let snapshot = installation.scan().unwrap();
        assert_eq!(folders(&installation), ["Skada", "SkadaImprovement"]);
        assert!(
            snapshot
                .addons
                .iter()
                .all(|addon| addon.marker == Some(marker("Skada")))
        );
    }

    #[test]
    fn scanning_sweeps_staging_folders_left_behind_but_not_ones_in_use() {
        let (_root, installation) = client();
        let addons = installation.addons_directory();
        let root = installation.root();
        let abandoned = addons.join(format!("{STAGING_PREFIX}abandoned"));
        let in_use = addons.join(format!("{STAGING_PREFIX}in-use"));
        let abandoned_by_a_mod = root.join(format!("{STAGING_PREFIX}abandoned"));
        let mod_marker = root.join(format!("{STAGING_PREFIX}glow.toml"));
        fs::create_dir(&abandoned).unwrap();
        fs::create_dir(&in_use).unwrap();
        fs::create_dir(&abandoned_by_a_mod).unwrap();
        fs::write(&mod_marker, "version = \"1.0\"").unwrap();

        // Windows opens a folder only with FILE_FLAG_BACKUP_SEMANTICS, and
        // sets its times only through FILE_WRITE_ATTRIBUTES access.
        let mut options = fs::OpenOptions::new();
        options.read(true);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.custom_flags(0x0200_0000).access_mode(0x100);
        }
        for left in [&abandoned, &abandoned_by_a_mod, &mod_marker] {
            options
                .open(left)
                .unwrap()
                .set_modified(std::time::SystemTime::now() - 2 * STAGING_AGE)
                .unwrap();
        }

        let snapshot = installation.scan().unwrap();

        assert!(!abandoned.exists());
        assert!(in_use.exists());
        assert!(snapshot.addons.is_empty(), "staging folders are not addons");
        assert!(!abandoned_by_a_mod.exists(), "beside Wow.exe too");
        assert!(mod_marker.exists(), "a mod's marker isn't a staging folder");
    }

    #[test]
    fn installing_leaves_no_staging_folder_behind() {
        let (_root, installation) = client();
        let archive = zip_of(&[("Foo-master/Foo.toc", "")]);

        installation.install(&archive, &marker("Foo"), &[]).unwrap();

        let entries: Vec<_> = fs::read_dir(installation.addons_directory())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, ["Foo"]);
    }

    #[test]
    fn reinstalling_drops_folders_the_new_version_no_longer_ships() {
        let (_root, installation) = client();
        let old = zip_of(&[("A/A.toc", ""), ("A_Old/A_Old.toc", "")]);
        let new = zip_of(&[("A/A.toc", "## Version: 2")]);

        let previous = installation.install(&old, &marker("A"), &[]).unwrap();
        installation.install(&new, &marker("A"), &previous).unwrap();

        assert_eq!(folders(&installation), ["A"]);
    }

    #[test]
    fn installing_over_a_manual_copy_replaces_it() {
        let (_root, installation) = client();
        let manual = installation.addons_directory().join("ACP");
        fs::create_dir_all(&manual).unwrap();
        fs::write(manual.join("stale.lua"), "").unwrap();

        installation
            .install(&zip_of(&[("ACP/ACP.toc", "")]), &marker("ACP"), &[])
            .unwrap();

        assert!(!manual.join("stale.lua").exists());
        assert!(manual.join("ACP.toc").exists());
    }

    /// A repository holding two addons a few folders down, as a branch
    /// snapshot of it is: `kekator-master/addon/…`.
    fn kekator() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "kekator-master/addon/Kekator/Kekator.toc",
                "## Title: Kekator\n## Version: 0.3\n## Interface: 30300\n",
            ),
            ("kekator-master/addon/Kekator/Kekator.lua", ""),
            (
                "kekator-master/addon/Kekator_NPC/Kekator_NPC.toc",
                "## Dependencies: Kekator\n",
            ),
            ("kekator-master/README.md", ""),
        ]
    }

    #[test]
    fn a_preview_says_what_an_archive_would_install_and_from_where() {
        let (_root, installation) = client();
        let picked = tempfile::tempdir().unwrap();
        let archive = picked.path().join("kekator-master.zip");
        fs::write(&archive, zip_of(&kekator())).unwrap();

        let found = installation.preview(&archive).unwrap();

        assert_eq!(
            found
                .iter()
                .map(|found| (found.name.as_str(), found.from.as_str()))
                .collect::<Vec<_>>(),
            [
                ("Kekator", "kekator-master/addon/Kekator"),
                ("Kekator_NPC", "kekator-master/addon/Kekator_NPC"),
            ]
        );
        assert_eq!(found[0].toc.version.as_deref(), Some("0.3"));
        assert_eq!(found[1].toc.dependencies, ["Kekator"]);
        assert!(folders(&installation).is_empty(), "nothing installed yet");
    }

    #[test]
    fn an_imported_archive_is_installed_as_by_hand() {
        let (_root, installation) = client();
        let picked = tempfile::tempdir().unwrap();
        let archive = picked.path().join("kekator-master.zip");
        fs::write(&archive, zip_of(&kekator())).unwrap();

        let installed = installation
            .import(&archive, &["Kekator".into(), "Kekator_NPC".into()])
            .unwrap();

        assert_eq!(installed, ["Kekator", "Kekator_NPC"]);
        assert_eq!(folders(&installation), ["Kekator", "Kekator_NPC"]);
        assert!(
            installation
                .scan()
                .unwrap()
                .addons
                .iter()
                .all(|addon| addon.marker.is_none()),
            "no marker: installed manually"
        );
        assert!(
            installation
                .addons_directory()
                .join("Kekator/Kekator.lua")
                .exists()
        );
    }

    #[test]
    fn an_imported_folder_gives_its_addons_and_nothing_else() {
        let (_root, installation) = client();
        let picked = tempfile::tempdir().unwrap();
        for (file, contents) in kekator() {
            let path = picked.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        fs::create_dir_all(picked.path().join("kekator-master/.git")).unwrap();
        fs::write(picked.path().join("kekator-master/.git/HEAD"), "").unwrap();
        let repository = picked.path().join("kekator-master");

        assert_eq!(installation.preview(&repository).unwrap().len(), 2);
        let installed = installation
            .import(&repository, &["Kekator".into(), "Kekator_NPC".into()])
            .unwrap();

        assert_eq!(installed, ["Kekator", "Kekator_NPC"]);
        let entries: Vec<_> = fs::read_dir(installation.addons_directory())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries.len(),
            2,
            "no README, no .git, no staging: {entries:?}"
        );
        assert!(
            installation
                .addons_directory()
                .join("Kekator/Kekator.lua")
                .exists()
        );
        assert!(
            repository.join("addon/Kekator/Kekator.lua").exists(),
            "copied, not moved"
        );
    }

    #[test]
    fn a_misnamed_folder_goes_in_under_its_toc_name() {
        let (_root, installation) = client();
        let picked = tempfile::tempdir().unwrap();
        let folder = picked.path().join("Foo-master");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("Foo.toc"), "").unwrap();

        let found = installation.preview(&folder).unwrap();
        assert_eq!(
            (found[0].name.as_str(), found[0].from.as_str()),
            ("Foo", "Foo-master")
        );

        installation.import(&folder, &["Foo".into()]).unwrap();
        assert_eq!(folders(&installation), ["Foo"]);
    }

    #[test]
    fn only_the_chosen_folders_are_imported() {
        let picked = tempfile::tempdir().unwrap();
        let archive = picked.path().join("kekator-master.zip");
        fs::write(&archive, zip_of(&kekator())).unwrap();
        for (file, contents) in kekator() {
            let path = picked.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        for path in [archive.clone(), picked.path().join("kekator-master")] {
            let (_root, installation) = client();

            let installed = installation.import(&path, &["kekator".into()]).unwrap();

            assert_eq!(installed, ["Kekator"], "{}", path.display());
            assert_eq!(folders(&installation), ["Kekator"], "{}", path.display());
        }

        // An archive whose root is the addon folder, left out.
        let (_root, installation) = client();
        let root = picked.path().join("root.zip");
        fs::write(&root, zip_of(&[("Root.toc", "")])).unwrap();
        assert!(matches!(
            installation.import(&root, &[]),
            Err(Error::NoAddon)
        ));
        assert!(
            fs::read_dir(installation.addons_directory())
                .unwrap()
                .next()
                .is_none(),
            "nothing installed, no staging folder left"
        );
    }

    #[test]
    fn a_folder_in_addons_is_not_imported_again() {
        let (_root, installation) = client();
        installation
            .install(&zip_of(&[("Foo/Foo.toc", "")]), &marker("Foo"), &[])
            .unwrap();
        let foo = installation.addons_directory().join("Foo");

        for folder in [foo.clone(), installation.addons_directory()] {
            assert!(matches!(
                installation.preview(&folder),
                Err(Error::InAddOns(_))
            ));
            assert!(matches!(
                installation.import(&folder, &["Foo".into()]),
                Err(Error::InAddOns(_))
            ));
        }
        assert_eq!(Marker::read(&foo), Some(marker("Foo")), "left as it was");
    }

    #[test]
    fn a_pick_without_an_addon_installs_nothing() {
        let (_root, installation) = client();
        let picked = tempfile::tempdir().unwrap();
        fs::write(picked.path().join("readme.txt"), "").unwrap();
        let archive = picked.path().join("readme.zip");
        fs::write(&archive, zip_of(&[("readme.txt", "")])).unwrap();

        for path in [picked.path(), archive.as_path()] {
            assert!(matches!(installation.preview(path), Err(Error::NoAddon)));
            assert!(matches!(
                installation.import(path, &["readme".into()]),
                Err(Error::NoAddon)
            ));
        }
        assert!(
            fs::read_dir(installation.addons_directory())
                .unwrap()
                .next()
                .is_none(),
            "no staging folder left either"
        );
    }

    /// A repository whose root is its addon, with its modules inside it, as
    /// a branch snapshot of it is: `Overachiever-master/Overachiever_Tabs/…`.
    fn overachiever() -> Vec<(&'static str, &'static str)> {
        vec![
            (
                "Overachiever-master/Overachiever.toc",
                "## Interface: 30300\n",
            ),
            ("Overachiever-master/Overachiever.lua", ""),
            ("Overachiever-master/libs/LibStub/LibStub.toc", ""),
            (
                "Overachiever-master/Overachiever_Tabs/Overachiever_Tabs.toc",
                "## Dependencies: Overachiever\n",
            ),
            (
                "Overachiever-master/Overachiever_Trade/Overachiever_Trade.toc",
                "## Dependencies: Overachiever\n",
            ),
        ]
    }

    #[test]
    fn modules_kept_inside_their_addon_go_in_beside_it() {
        let (_root, installation) = client();

        let installed = installation
            .install(&zip_of(&overachiever()), &marker("Overachiever"), &[])
            .unwrap();

        let all = ["Overachiever", "Overachiever_Tabs", "Overachiever_Trade"];
        assert_eq!(installed, all);
        assert_eq!(folders(&installation), all);
        let addons = installation.addons_directory();
        assert!(!addons.join("Overachiever/Overachiever_Tabs").exists());
        assert!(
            addons
                .join("Overachiever/libs/LibStub/LibStub.toc")
                .exists(),
            "its libraries stay inside it"
        );
        assert!(
            installation
                .scan()
                .unwrap()
                .addons
                .iter()
                .all(|addon| addon.marker == Some(marker("Overachiever")))
        );
    }

    #[test]
    fn a_module_left_out_is_not_installed_inside_its_addon_either() {
        let picked = tempfile::tempdir().unwrap();
        let archive = picked.path().join("Overachiever-master.zip");
        fs::write(&archive, zip_of(&overachiever())).unwrap();
        for (file, contents) in overachiever() {
            let path = picked.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        for path in [archive, picked.path().join("Overachiever-master")] {
            let (_root, installation) = client();

            let installed = installation
                .import(&path, &["Overachiever".into(), "Overachiever_Trade".into()])
                .unwrap();

            let addons = installation.addons_directory();
            assert_eq!(installed, ["Overachiever", "Overachiever_Trade"]);
            assert_eq!(folders(&installation), installed, "{}", path.display());
            for module in ["Overachiever_Tabs", "Overachiever_Trade"] {
                assert!(
                    !addons.join("Overachiever").join(module).exists(),
                    "{module} from {}",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn archives_without_addons_change_nothing() {
        let (_root, installation) = client();
        installation
            .install(&zip_of(&[("A/A.toc", "")]), &marker("A"), &[])
            .unwrap();

        let result = installation.install(
            &zip_of(&[("README.md", "")]),
            &marker("A"),
            &["A".to_owned()],
        );

        assert!(matches!(result, Err(Error::NoAddon)));
        assert_eq!(folders(&installation), ["A"]);
    }

    fn touch(installation: &Installation, files: &[&str]) {
        for file in files {
            let path = installation.addons_directory().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
    }

    #[test]
    fn repairing_moves_nested_addons_up() {
        let (_root, installation) = client();
        touch(
            &installation,
            &[
                "Bartender4-master/Bartender4/Bartender4.toc",
                "Bartender4-master/Bartender4/Bartender4.lua",
                "Bartender4-master/README.md",
            ],
        );

        let repaired = installation.repair("Bartender4-master").unwrap();

        assert_eq!(repaired, ["Bartender4"]);
        assert_eq!(folders(&installation), ["Bartender4"]);
        assert!(
            installation
                .addons_directory()
                .join("Bartender4/Bartender4.lua")
                .exists()
        );
    }

    #[test]
    fn repairing_moves_modules_out_of_their_addon() {
        let (_root, installation) = client();
        let addons = installation.addons_directory();
        for (file, contents) in overachiever() {
            let path = addons.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }

        let repaired = installation.repair("Overachiever-master").unwrap();

        let all = ["Overachiever", "Overachiever_Tabs", "Overachiever_Trade"];
        assert_eq!(repaired, all);
        assert_eq!(folders(&installation), all);
        assert!(addons.join("Overachiever/Overachiever.lua").exists());
    }

    #[test]
    fn repairing_a_doubled_folder_keeps_its_name() {
        let (_root, installation) = client();
        touch(&installation, &["Foo/Foo/Foo.toc"]);

        installation.repair("Foo").unwrap();

        assert!(installation.addons_directory().join("Foo/Foo.toc").exists());
        assert!(!installation.addons_directory().join("Foo/Foo").exists());
    }

    #[test]
    fn repairing_renames_a_misnamed_folder() {
        let (_root, installation) = client();
        touch(&installation, &["GearScore-main/GearScore.toc"]);

        installation.repair("GearScore-main").unwrap();

        assert_eq!(folders(&installation), ["GearScore"]);
    }

    #[test]
    fn repairing_a_folder_without_addons_leaves_it_alone() {
        let (_root, installation) = client();
        touch(&installation, &["Junk/readme.txt"]);

        assert!(matches!(installation.repair("Junk"), Err(Error::NoAddon)));
        assert!(
            installation
                .addons_directory()
                .join("Junk/readme.txt")
                .exists()
        );
        assert_eq!(folders(&installation), ["Junk"]);
    }

    #[test]
    fn adopting_marks_only_unmarked_folders_that_exist() {
        let (_root, installation) = client();
        touch(&installation, &["ACP/ACP.toc", "Skada/Skada.toc"]);
        let other = marker("Other");
        other
            .write(&installation.addons_directory().join("Skada"))
            .unwrap();

        let marked = installation
            .adopt(&Adoption {
                name: "ACP".into(),
                marker: marker("ACP"),
                folders: vec!["ACP".into(), "Skada".into(), "Gone".into()],
            })
            .unwrap();

        assert_eq!(marked, 1);
        let snapshot = installation.scan().unwrap();
        assert_eq!(snapshot.addons[0].marker, Some(marker("ACP")));
        assert_eq!(snapshot.addons[1].marker, Some(other));
        assert!(!installation.addons_directory().join("Gone").exists());
    }

    #[test]
    fn uninstalling_removes_only_the_named_folders() {
        let (_root, installation) = client();
        installation
            .install(
                &zip_of(&[("A/A.toc", ""), ("B/B.toc", "")]),
                &marker("AB"),
                &[],
            )
            .unwrap();

        installation
            .uninstall(&["A".to_owned(), "Missing".to_owned()])
            .unwrap();

        assert_eq!(folders(&installation), ["B"]);
    }

    #[test]
    fn uninstalling_rejects_names_that_escape_addons() {
        let (_root, installation) = client();

        for name in ["..", "../WTF", "", "A/B"] {
            assert!(matches!(
                installation.uninstall(&[name.to_owned()]),
                Err(Error::InvalidName(_))
            ));
        }
    }

    #[test]
    fn cleaning_deletes_listed_saved_variables() {
        let (_root, installation) = client();
        let file = installation
            .root()
            .join("WTF/Account/ME/SavedVariables/A.lua");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "A = {}").unwrap();
        let listed = installation
            .scan()
            .unwrap()
            .saved_variables
            .of("A")
            .to_vec();

        installation.clean(&listed).unwrap();

        assert!(!file.exists());
        assert_eq!(
            installation.scan().unwrap().saved_variables.of("A").len(),
            0
        );
    }

    #[test]
    fn cleaning_refuses_files_outside_wtf() {
        let (root, installation) = client();
        let outside = root.path().join("Wow.exe");
        let sneaky = root.path().join("WTF/../Wow.exe");

        for file in [outside.clone(), sneaky] {
            assert!(matches!(
                installation.clean(&[file]),
                Err(Error::OutsideWtf(_))
            ));
        }
        assert!(outside.exists());
    }

    #[cfg(unix)]
    #[test]
    fn uninstalling_a_symlinked_addon_keeps_its_target() {
        let (root, installation) = client();
        let shared = root.path().join("shared/A");
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("A.toc"), "").unwrap();
        std::os::unix::fs::symlink(&shared, installation.addons_directory().join("A")).unwrap();

        installation.uninstall(&["A".to_owned()]).unwrap();

        assert!(shared.join("A.toc").exists());
        assert_eq!(folders(&installation), [] as [String; 0]);
    }

    #[cfg(unix)]
    #[test]
    fn installing_over_a_symlinked_addon_replaces_only_the_link() {
        let (root, installation) = client();
        let shared = root.path().join("shared/A");
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("A.toc"), "## Version: 1").unwrap();
        let link = installation.addons_directory().join("A");
        std::os::unix::fs::symlink(&shared, &link).unwrap();

        let archive = zip_of(&[("A/A.toc", "## Version: 2")]);
        installation
            .install(&archive, &marker("A"), &["A".to_owned()])
            .unwrap();

        assert_eq!(
            fs::read_to_string(shared.join("A.toc")).unwrap(),
            "## Version: 1",
            "set aside, then deleted as a link"
        );
        assert!(!link.is_symlink());
        assert_eq!(
            fs::read_to_string(link.join("A.toc")).unwrap(),
            "## Version: 2"
        );
        assert_eq!(folders(&installation), ["A"], "no staging folder left");
    }
}
