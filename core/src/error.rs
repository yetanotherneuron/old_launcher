//! The error type shared by every operation in this crate.

use std::io;
use std::path::{Path, PathBuf};

use crate::Version;

/// Everything that can go wrong while inspecting or modifying an installation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A filesystem operation failed on `path`.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or directory being accessed.
        path: PathBuf,
        /// The underlying OS error.
        source: io::Error,
    },

    /// The directory has no `Wow.exe`.
    #[error("no Wow.exe found in {}", .0.display())]
    NotAClient(PathBuf),

    /// `Wow.exe` exists but carries no readable version resource.
    #[error("could not read the client version from {}", .0.display())]
    UnknownVersion(PathBuf),

    /// An executable without the headers of a Windows program, so nothing
    /// in them can be changed.
    #[error("{} has no Windows program headers", .0.display())]
    NoHeaders(PathBuf),

    /// The client is not 3.3.5a.
    #[error("client version {0} is not supported; only 3.3.5a is")]
    UnsupportedVersion(Version),

    /// The downloaded file is not a zip, 7z or rar archive.
    #[error("unrecognized archive format")]
    UnknownArchive,

    /// The zip archive is corrupt or uses an unsupported feature.
    #[error("zip: {0}")]
    Zip(#[from] zip::result::ZipError),

    /// The 7z archive is corrupt or uses an unsupported feature.
    #[error("7z: {0}")]
    SevenZip(#[from] sevenz_rust2::Error),

    /// The rar archive is corrupt or uses an unsupported feature.
    #[error("rar: {0}")]
    Rar(#[from] rars::Error),

    /// An archive entry would be written outside the extraction directory.
    #[error("archive entry escapes the extraction directory: {0}")]
    UnsafeEntry(String),

    /// The archive or folder holds no folder with a `.toc` file.
    #[error("no addon in it: no folder holds a .toc file")]
    NoAddon,

    /// A folder to install from that is in `Interface/AddOns` already.
    #[error("{} is in Interface/AddOns already", .0.display())]
    InAddOns(PathBuf),

    /// A file of a mod's release that doesn't match the checksum the list of
    /// mods gives for it.
    #[error("{0} isn't the file its release has")]
    Checksum(String),

    /// A file of a mod's release that its download lacks.
    #[error("{0} is missing from the download")]
    NotInRelease(String),

    /// Bytes that aren't a torrent the launcher can use, and why.
    #[error("not a usable torrent: {0}")]
    Torrent(String),

    /// Files in the client folder where a mod's release puts its own, which
    /// aren't the mod's: another mod's, say. Installing would replace them.
    #[error(
        "{} in the client folder {} to something else",
        .0.join(", "),
        if .0.len() == 1 { "belongs" } else { "belong" }
    )]
    Foreign(Vec<String>),

    /// An addon folder name that is not a single, plain path component.
    #[error("invalid addon folder name: {0:?}")]
    InvalidName(String),

    /// A language the client has no `Data` folder for.
    #[error("the client has no Data/{0} folder")]
    NoLocale(String),

    /// A SavedVariables path handed to `clean` lies outside the installation.
    #[error("refusing to delete {}: not inside the installation's WTF folder", .0.display())]
    OutsideWtf(PathBuf),

    /// A catalog could not be parsed.
    #[error("invalid catalog: {0}")]
    Catalog(#[from] serde_json::Error),

    /// A catalog uses a schema version this build does not know.
    #[error(
        "the catalog uses format version {0}, which this version of the launcher does not support"
    )]
    CatalogVersion(u32),

    /// A list of mods could not be parsed.
    #[error("invalid list of mods: {0}")]
    Mods(serde_json::Error),

    /// A list of mods uses a schema version this build does not know.
    #[error(
        "the list of mods uses format version {0}, which this version of the launcher does not \
         support"
    )]
    ModsVersion(u32),

    /// The config file is not valid TOML for [`Config`](crate::Config).
    #[error("invalid config file: {0}")]
    ConfigRead(#[from] toml::de::Error),

    /// The config could not be serialized.
    #[error("could not serialize config: {0}")]
    ConfigWrite(#[from] toml::ser::Error),

    /// A zip file without a readable list of addons, so not an export.
    #[error("{} is not an export of the launcher: {reason}", path.display())]
    NotAnExport {
        /// The file.
        path: PathBuf,
        /// What is wrong with its list of addons.
        reason: String,
    },

    /// An export uses a format version this build does not know.
    #[error(
        "the export uses format version {0}, which this version of the launcher does not support"
    )]
    ExportVersion(u32),

    /// A client's launch settings can't be run.
    #[error("invalid {setting}: {problem}")]
    Launch {
        /// Which setting: `environment variables`, `command`, …
        setting: &'static str,
        /// What is wrong with it, as a sentence.
        problem: String,
    },
}

/// Wraps an [`io::Error`] with the path it happened on.
pub(crate) fn io_at(path: &Path) -> impl FnOnce(io::Error) -> Error + '_ {
    move |source| Error::Io {
        path: path.to_path_buf(),
        source,
    }
}
