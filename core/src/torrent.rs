//! Clients shared through BitTorrent: what a torrent holds, where the client
//! is in it, how a client folder compares with it, and how what was
//! downloaded elsewhere gets into the folder.
//!
//! The comparison only reads the client: it hashes the folder's files piece
//! by piece against the torrent, leaving the player's own files alone
//! (their settings, addons and realmlist, which the torrent may bring too).
//! What it finds as the torrent has it, it notes by length and modification
//! time, so the next comparison reads only the files changed since.
//!
//! Downloads happen in a staging folder outside the client, which the
//! files are moved from once complete: the torrent engine writes the pieces
//! it downloads whole, over every file they share, and makes every file of
//! the torrent it doesn't find. Only the files asked for leave it.
//!
//! A client folder notes the torrent it comes from (see [`keep_origin`]),
//! so a client forgotten by the launcher and added back has it still, and
//! what was found as it has it, beside `Wow.exe` with the rest of what the
//! launcher notes there.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use librqbit_core::torrent_metainfo::torrent_from_bytes;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};

use crate::Error;
use crate::config::ClientTorrent;
use crate::error::io_at;
use crate::installation::{
    LARGE_ADDRESS_AWARE, characteristics, large_address_aware, set_large_address_aware,
};
use crate::notes::Notes;

/// What a file being copied across disks is named after, beside where it
/// goes, until it is whole.
const MOVING: &str = ".old_launcher-moving";

/// A torrent, as the launcher needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Torrent {
    /// Its name: the folder it downloads into is named after it.
    pub name: String,
    /// Its info hash, in lowercase hexadecimal: what identifies it.
    pub info_hash: String,
    /// The trackers it names, which find its peers.
    pub trackers: Vec<String>,
    /// Its files, in order.
    pub files: Vec<File>,
    /// How long its pieces are, but the last.
    pub piece_length: u64,
    /// The SHA-1 of each piece.
    pieces: Vec<[u8; 20]>,
}

/// A file of a torrent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct File {
    /// Its path in the folder the torrent downloads into, with `/` between
    /// folders.
    pub path: String,
    /// Its length in bytes.
    pub length: u64,
    /// Whether it only pads the next file to a piece (BEP 47): it is never
    /// on disk.
    pub padding: bool,
}

/// How a client folder compares with its torrent: the files to download.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Check {
    /// The files the folder lacks, by index in the torrent.
    pub missing: Vec<usize>,
    /// The files whose contents differ, by index.
    pub different: Vec<usize>,
}

/// The files of a torrent last found as it has it, by path, as the client
/// folder notes them.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Verified {
    /// The torrent they were compared with.
    info_hash: String,
    files: BTreeMap<String, Stamp>,
}

/// What tells a file changed: its length, and when it last was.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Stamp {
    length: u64,
    /// In nanoseconds since 1970.
    modified: u64,
}

impl Verified {
    /// What the client of `torrent`, whose files go in `folder`, notes was
    /// found as `torrent` has it.
    fn read(folder: &Path, torrent: &Torrent) -> Self {
        Notes::read(&torrent.root(folder))
            .verified
            .filter(|verified| verified.info_hash == torrent.info_hash)
            .unwrap_or_default()
    }

    /// Notes it in the client of `torrent`, whose files go in `folder`.
    fn write(self, folder: &Path, torrent: &Torrent) -> Result<(), Error> {
        Notes::change(&torrent.root(folder), |notes| notes.verified = Some(self))
    }
}

impl Stamp {
    fn of(metadata: &fs::Metadata) -> Option<Self> {
        let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;

        Some(Self {
            length: metadata.len(),
            modified: u64::try_from(modified.as_nanos()).ok()?,
        })
    }
}

impl Torrent {
    /// Reads a `.torrent` file's bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self, Error> {
        let unusable = |error: &dyn std::fmt::Display| Error::Torrent(error.to_string());
        let parsed = torrent_from_bytes(bytes).map_err(|error| unusable(&error))?;
        let info_hash = parsed.info_hash.as_string();
        let mut trackers: Vec<String> = Vec::new();
        for tracker in parsed.iter_announce() {
            let tracker = String::from_utf8_lossy(tracker.as_ref()).into_owned();
            if !trackers.contains(&tracker) {
                trackers.push(tracker);
            }
        }
        let info = parsed
            .info
            .data
            .validate()
            .map_err(|error| unusable(&error))?;

        let files: Vec<File> = info
            .iter_file_details()
            .map(|details| File {
                path: details.filename.to_vec().join("/"),
                length: details.len,
                padding: details.attrs().padding,
            })
            .collect();
        // Its files are read, set aside and put back by these paths.
        if let Some(file) = files.iter().find(|file| !stays_inside(&file.path)) {
            return Err(Error::Torrent(format!(
                "{} is outside the folder it downloads into",
                file.path
            )));
        }
        let piece_length = u64::from(info.info().piece_length);
        let pieces: Vec<[u8; 20]> = info.info().pieces.as_ref().as_chunks::<20>().0.to_vec();

        let total: u64 = files.iter().map(|file| file.length).sum();
        if piece_length == 0 || pieces.len() as u64 != total.div_ceil(piece_length) {
            return Err(Error::Torrent(
                "its pieces don't add up to its files".to_owned(),
            ));
        }

        Ok(Self {
            name: info
                .name()
                .map_or_else(|| info_hash.clone(), |name| name.into_owned()),
            info_hash,
            trackers,
            files,
            piece_length,
            pieces,
        })
    }

    /// Its magnet link: its info hash, its name and its trackers, which
    /// find it again without its `.torrent` file.
    pub fn magnet(&self) -> String {
        let mut link = format!(
            "magnet:?xt=urn:btih:{}&dn={}",
            self.info_hash,
            percent_encoded(&self.name)
        );
        for tracker in &self.trackers {
            let _ = write!(link, "&tr={}", percent_encoded(tracker));
        }

        link
    }

    /// How many bytes its files hold.
    pub fn size(&self) -> u64 {
        self.files
            .iter()
            .filter(|file| !file.padding)
            .map(|file| file.length)
            .sum()
    }

    /// How many pieces it has.
    pub fn pieces(&self) -> usize {
        self.pieces.len()
    }

    /// How many pieces hold some of the `files`, by index: those a download
    /// of them gets.
    pub fn pieces_of(&self, files: &[usize]) -> usize {
        let mut pieces = BTreeSet::new();
        let mut start = 0;
        for (index, file) in self.files.iter().enumerate() {
            if files.contains(&index) && !file.padding && file.length > 0 {
                let first = start / self.piece_length;
                let last = (start + file.length - 1) / self.piece_length;
                pieces.extend(first..=last);
            }
            start += file.length;
        }

        pieces.len()
    }

    /// Where the client is in it: the folder holding its `Wow.exe` and a
    /// `Data` folder, the shallowest if several do, as a path prefix (`""`
    /// at the top, `World of Warcraft/` below it). `None` when it holds no
    /// client.
    pub fn client(&self) -> Option<String> {
        self.executable()
            .map(|index| folder_of(&self.files[index].path).to_owned())
    }

    /// The client folder, when its files go in `folder`: `folder` itself
    /// when it holds no client.
    pub fn root(&self, folder: &Path) -> PathBuf {
        match self
            .client()
            .as_deref()
            .map(|client| client.trim_end_matches('/'))
        {
            None | Some("") => folder.to_path_buf(),
            Some(client) => folder.join(client),
        }
    }

    /// The index of the client's `Wow.exe`, if it holds a client.
    pub fn executable(&self) -> Option<usize> {
        self.files
            .iter()
            .enumerate()
            .filter(|(_, file)| {
                let name = file.path.rsplit('/').next().unwrap_or_default();
                name.eq_ignore_ascii_case("Wow.exe")
            })
            .filter(|(_, file)| {
                let data = format!("{}data/", folder_of(&file.path)).to_ascii_lowercase();
                self.files
                    .iter()
                    .any(|other| other.path.to_ascii_lowercase().starts_with(&data))
            })
            .min_by_key(|(_, file)| file.path.matches('/').count())
            .map(|(index, _)| index)
    }

    /// The files whose paths, within the client, are the player's (see
    /// [`is_players`]) or the launcher's own (see [`is_launchers`]), by
    /// index: never compared with it, nor replaced from it.
    pub fn untouched(&self) -> BTreeSet<usize> {
        self.within(|path| is_players(path) || is_launchers(path))
    }

    /// The files that are the launcher's own, by index (see
    /// [`is_launchers`]).
    fn launchers(&self) -> BTreeSet<usize> {
        self.within(is_launchers)
    }

    /// The files whose paths within the client `matches`, by index.
    fn within(&self, matches: impl Fn(&str) -> bool) -> BTreeSet<usize> {
        let client = self.client().unwrap_or_default();

        self.files
            .iter()
            .enumerate()
            .filter(|(_, file)| file.path.strip_prefix(&client).is_some_and(&matches))
            .map(|(index, _)| index)
            .collect()
    }
}

impl Check {
    /// Whether the folder is as the torrent has it.
    pub fn is_current(&self) -> bool {
        self.missing.is_empty() && self.different.is_empty()
    }

    /// The files to download, by index, in order.
    pub fn files(&self) -> Vec<usize> {
        let mut files: Vec<usize> = self
            .missing
            .iter()
            .chain(&self.different)
            .copied()
            .collect();
        files.sort_unstable();
        files
    }

    /// How many bytes those files hold.
    pub fn bytes(&self, torrent: &Torrent) -> u64 {
        self.files()
            .into_iter()
            .filter_map(|index| torrent.files.get(index))
            .map(|file| file.length)
            .sum()
    }
}

/// `text`, with every byte but letters, digits and `-._~` written `%XX`, for
/// a link.
fn percent_encoded(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }

    encoded
}

/// What a client folder notes of the torrent it comes from.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Origin {
    name: String,
    info_hash: String,
    /// What the player gave for it (see [`ClientTorrent::source`]).
    source: String,
    /// Where the client is in it, as [`Torrent::client`] says.
    client: String,
}

/// Notes in the client folder at `root` the torrent it comes from,
/// `torrent`, so that it isn't lost with the launcher's settings: a client
/// forgotten and added back has it again (see [`origin`]).
pub fn keep_origin(root: &Path, torrent: &ClientTorrent) -> Result<(), Error> {
    let origin = Origin {
        name: torrent.name.clone(),
        info_hash: torrent.info_hash.clone(),
        source: torrent.source.clone(),
        client: client_in(root, &torrent.folder).unwrap_or_default(),
    };

    Notes::change(root, |notes| notes.torrent = Some(origin))
}

/// The torrent the client folder at `root` notes it comes from, if any (see
/// [`keep_origin`]). The folder may come from anywhere: an info hash that
/// can't be one, which names files, is no origin.
pub fn origin(root: &Path) -> Option<ClientTorrent> {
    let origin = Notes::read(root).torrent?;
    let hash = &origin.info_hash;
    if hash.len() != 40
        || !hash
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }

    Some(ClientTorrent {
        folder: folder_of_client(root, &origin.client),
        name: origin.name,
        info_hash: origin.info_hash,
        source: origin.source,
    })
}

/// Where the client at `root` is in a torrent whose files go in `folder`,
/// as [`Torrent::client`] says it: `None` when `root` isn't in `folder`.
pub fn client_in(root: &Path, folder: &Path) -> Option<String> {
    let parts: Vec<String> = root
        .strip_prefix(folder)
        .ok()?
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();

    Some(if parts.is_empty() {
        String::new()
    } else {
        format!("{}/", parts.join("/"))
    })
}

/// The folder a torrent's files go in, when the client in it, at `client`
/// (as [`Torrent::client`] says), is at `root`.
pub fn folder_of_client(root: &Path, client: &str) -> PathBuf {
    let depth = client.split('/').filter(|part| !part.is_empty()).count();

    root.ancestors()
        .nth(depth)
        .map_or_else(|| root.to_path_buf(), Path::to_path_buf)
}

/// Whether `path`, of a torrent's file, names a file inside the folder the
/// torrent downloads into: no part of it climbs out, or starts elsewhere (a
/// drive, on Windows).
fn stays_inside(path: &str) -> bool {
    path.split('/')
        .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', ':']))
}

/// The folder of `path`, as a prefix: `a/b/` for `a/b/c`, `""` for `c`.
fn folder_of(path: &str) -> &str {
    path.rfind('/').map_or("", |slash| &path[..=slash])
}

/// Whether the file at `path`, within the client folder, is the player's:
/// their settings (`WTF`), addons (`Interface/AddOns`), the server they
/// play on (any `realmlist.wtf`), and what the game writes (`Cache`,
/// `Logs`, `Screenshots`, `Errors`). The launcher never checks those
/// against a torrent, nor replaces them.
pub fn is_players(path: &str) -> bool {
    let path = path.to_ascii_lowercase();
    let first = path.split('/').next().unwrap_or_default();

    matches!(first, "wtf" | "cache" | "logs" | "screenshots" | "errors")
        || path.starts_with("interface/addons/")
        || path.rsplit('/').next() == Some("realmlist.wtf")
}

/// Whether the file at `path`, within the client folder, is the launcher's
/// own: what it notes beside `Wow.exe` (`.old_launcher.toml`, and 0.9.0's
/// markers), which a torrent made of a client folder carries along. What
/// the launcher notes in a folder is about that folder: it is never checked
/// against a torrent, nor taken from one.
pub fn is_launchers(path: &str) -> bool {
    !path.contains('/') && path.starts_with(".old_launcher")
}

/// What a file is to [`check`], from a look at the folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Found {
    /// There at its length: its pieces are read.
    Sized,
    /// There as last found as the torrent has it, unchanged since: read
    /// only for a piece shared with a file that changed.
    Unchanged,
    /// The player's or the launcher's, at its length: read, but blamed for
    /// nothing.
    Untouched,
    /// The player's or the launcher's, of another length or missing: its
    /// pieces can't be compared.
    UntouchedOther,
    /// Padding: zeros, never on disk.
    Padding,
    /// Not there.
    Missing,
    /// There at another length.
    Resized,
}

/// Compares the folder `torrent` downloads into, `folder`, with it, piece
/// by piece, only reading: the files missing, and those whose contents
/// differ. The player's files and the launcher's own (see
/// [`Torrent::untouched`]) are neither: a piece shared with one that changed
/// blames no other file. A piece that differs and is shared by other files
/// blames them all, unless one of them differs anyway: downloading it
/// writes the piece whole. `Wow.exe` with the Large Address Aware flag set
/// otherwise than the torrent's is as released, the flag being the
/// launcher's to set.
///
/// The files found as the torrent has them are noted in `folder`, for the
/// next check to read only those changed since: those whose every piece
/// was read and matched, but for pieces shared with the player's files,
/// which may never match. A piece shared with a file to download can't be
/// read whole until it is: the files sharing it aren't noted, and the next
/// check reads them. `progress` hears how many of the torrent's bytes were
/// gone through, and stops the check by breaking: then there is no result.
pub fn check(
    folder: &Path,
    torrent: &Torrent,
    progress: &mut dyn FnMut(u64) -> ControlFlow<()>,
) -> Result<Option<Check>, Error> {
    let untouched = torrent.untouched();
    let verified = Verified::read(folder, torrent);
    let mut starts = Vec::with_capacity(torrent.files.len());
    let mut total = 0;
    for file in &torrent.files {
        starts.push(total);
        total += file.length;
    }

    let mut found = Vec::with_capacity(torrent.files.len());
    let mut stamps = Vec::with_capacity(torrent.files.len());
    for (index, file) in torrent.files.iter().enumerate() {
        let path = folder.join(&file.path);
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => Some(metadata),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(io_at(&path)(error)),
        };
        let there = metadata.is_some();
        let stamp = metadata
            .filter(|metadata| metadata.is_file() && metadata.len() == file.length)
            .as_ref()
            .and_then(Stamp::of);
        let unchanged = stamp.is_some() && verified.files.get(&file.path) == stamp.as_ref();

        found.push(
            match (file.padding, untouched.contains(&index), there, stamp) {
                (true, ..) => Found::Padding,
                (false, true, _, Some(_)) => Found::Untouched,
                (false, true, _, None) => Found::UntouchedOther,
                (false, false, false, _) => Found::Missing,
                (false, false, true, None) => Found::Resized,
                (false, false, true, Some(_)) if unchanged => Found::Unchanged,
                (false, false, true, Some(_)) => Found::Sized,
            },
        );
        stamps.push(stamp);
    }

    // Where `Wow.exe`'s Large Address Aware flag is, among the torrent's
    // bytes laid end to end.
    let flag = torrent
        .executable()
        .filter(|index| matches!(found[*index], Found::Sized | Found::Unchanged))
        .and_then(|index| {
            let mut header = Vec::new();
            fs::File::open(folder.join(&torrent.files[index].path))
                .and_then(|file| file.take(4096).read_to_end(&mut header))
                .ok()?;
            Some(starts[index] + characteristics(&header)? as u64)
        });

    let mut different = BTreeSet::new();
    // The files of the pieces that differ and that more than one file may
    // be to blame for. Downloading any of them writes such a piece whole,
    // over all of them, so they are blamed at the end, unless one of them
    // is downloaded anyway.
    let mut shared = Vec::new();
    // The files read whose piece shared with a file to download went
    // unread: not found as the torrent has them until it is downloaded.
    let mut unread = BTreeSet::new();
    let mut piece = Vec::new();
    for (index, hash) in torrent.pieces.iter().enumerate() {
        let start = index as u64 * torrent.piece_length;
        let end = (start + torrent.piece_length).min(total);
        let within: Vec<usize> = (0..torrent.files.len())
            .filter(|file| {
                let length = torrent.files[*file].length;
                length > 0 && starts[*file] < end && starts[*file] + length > start
            })
            .collect();

        let readable = within.iter().all(|file| {
            matches!(
                found[*file],
                Found::Sized | Found::Unchanged | Found::Untouched | Found::Padding
            )
        });
        let suspects: Vec<usize> = within
            .iter()
            .copied()
            .filter(|file| found[*file] == Found::Sized && !different.contains(file))
            .collect();
        if !readable || suspects.is_empty() {
            if within
                .iter()
                .any(|file| matches!(found[*file], Found::Missing | Found::Resized))
            {
                unread.extend(suspects);
            }
            if progress(end).is_break() {
                return Ok(None);
            }
            continue;
        }

        piece.clear();
        for file in &within {
            let from = start.max(starts[*file]) - starts[*file];
            let to = end.min(starts[*file] + torrent.files[*file].length) - starts[*file];
            if found[*file] == Found::Padding {
                piece.resize(piece.len() + (to - from) as usize, 0);
                continue;
            }

            let path = folder.join(&torrent.files[*file].path);
            let mut opened = fs::File::open(&path).map_err(io_at(&path))?;
            let _ = opened.seek(SeekFrom::Start(from)).map_err(io_at(&path))?;
            let _ = opened
                .take(to - from)
                .read_to_end(&mut piece)
                .map_err(io_at(&path))?;
        }

        let mut matches = Sha1::digest(&piece)[..] == hash[..];
        if let Some(at) = flag.filter(|at| (start..end).contains(at)) {
            let byte = (at - start) as usize;
            if !matches && let Some(characteristic) = piece.get_mut(byte) {
                *characteristic ^= LARGE_ADDRESS_AWARE.to_le_bytes()[0];
                matches = Sha1::digest(&piece)[..] == hash[..];
            }
        }
        let blames = within.iter().all(|file| found[*file] != Found::Untouched);
        if !matches && blames {
            if let [suspect] = suspects[..] {
                let _ = different.insert(suspect);
            } else {
                shared.push(suspects);
            }
        }

        if progress(end).is_break() {
            return Ok(None);
        }
    }

    for suspects in shared {
        if !suspects.iter().any(|file| different.contains(file)) {
            different.extend(suspects);
        }
    }

    let mut record = Verified {
        info_hash: torrent.info_hash.clone(),
        files: BTreeMap::new(),
    };
    for (index, file) in torrent.files.iter().enumerate() {
        let current = match found[index] {
            Found::Unchanged => true,
            Found::Sized => !different.contains(&index) && !unread.contains(&index),
            _ => false,
        };
        if let (true, Some(stamp)) = (current, stamps[index]) {
            let _ = record.files.insert(file.path.clone(), stamp);
        }
    }
    // It only spares the next check reading what didn't change: without
    // it, that check reads everything again, and finds the same.
    let _ = record.write(folder, torrent);

    Ok(Some(Check {
        missing: (0..found.len())
            .filter(|file| found[*file] == Found::Missing)
            .collect(),
        different: (0..found.len())
            .filter(|file| found[*file] == Found::Resized || different.contains(file))
            .collect(),
    }))
}

/// Notes the files of `torrent` that a download into `folder` just wrote,
/// `downloaded` by index, as the torrent has them: the download checked
/// every piece it wrote. Those a check found so and that are unchanged
/// since stay noted; the next check reads the rest.
pub fn record(folder: &Path, torrent: &Torrent, downloaded: &[usize]) -> Result<(), Error> {
    let untouched = torrent.untouched();
    let earlier = Verified::read(folder, torrent);
    let mut record = Verified {
        info_hash: torrent.info_hash.clone(),
        files: BTreeMap::new(),
    };

    for (index, file) in torrent.files.iter().enumerate() {
        if file.padding || untouched.contains(&index) {
            continue;
        }
        let Some(stamp) = fs::metadata(folder.join(&file.path))
            .ok()
            .filter(|metadata| metadata.is_file() && metadata.len() == file.length)
            .as_ref()
            .and_then(Stamp::of)
        else {
            continue;
        };
        if downloaded.contains(&index) || earlier.files.get(&file.path) == Some(&stamp) {
            let _ = record.files.insert(file.path.clone(), stamp);
        }
    }

    record.write(folder, torrent)
}

/// Readies `staging`, the folder an update downloads into, with copies of
/// the `files` of `torrent` that `folder` has, for the download to fetch
/// only their pieces that differ; those it doesn't have download whole. A
/// file `staging` has already, from an update cut short, stays as it is.
pub fn stage(
    staging: &Path,
    folder: &Path,
    torrent: &Torrent,
    files: &[usize],
) -> Result<(), Error> {
    fs::create_dir_all(staging).map_err(io_at(staging))?;

    for index in files {
        let path = &torrent.files[*index].path;
        let (from, to) = (folder.join(path), staging.join(path));
        if !from.is_file() || to.exists() {
            continue;
        }
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(io_at(parent))?;
        }
        let _ = fs::copy(&from, &to).map_err(io_at(&from))?;
    }

    Ok(())
}

/// Moves the `files` of `torrent` downloaded into `staging` to their places
/// in `folder`, over what is there, then removes `staging` with whatever
/// else the download left in it: the launcher's own files among them (see
/// [`is_launchers`]), which stay there. Within a disk a file is renamed;
/// across disks it is copied first. `moved` hears how many of their bytes
/// are in place. `Wow.exe` keeps the Large Address Aware flag the folder's
/// had.
pub fn move_into(
    staging: &Path,
    folder: &Path,
    torrent: &Torrent,
    files: &[usize],
    moved: &mut dyn FnMut(u64),
) -> Result<(), Error> {
    let executable = torrent.executable();
    let launchers = torrent.launchers();
    let mut done = 0;

    for index in files {
        let file = &torrent.files[*index];
        if file.padding || launchers.contains(index) {
            continue;
        }
        let (from, to) = (staging.join(&file.path), folder.join(&file.path));
        let flag = if executable == Some(*index) {
            fs::read(&to)
                .ok()
                .and_then(|bytes| large_address_aware(&bytes))
        } else {
            None
        };

        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(io_at(parent))?;
        }
        match fs::rename(&from, &to) {
            Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {
                copy_over(&from, &to)?;
                fs::remove_file(&from).map_err(io_at(&from))?;
            }
            renamed => renamed.map_err(io_at(&from))?,
        }
        if let Some(on) = flag {
            set_large_address_aware(&to, on)?;
        }

        done += file.length;
        moved(done);
    }

    fs::remove_dir_all(staging).map_err(io_at(staging))
}

/// Copies the file at `from` over the one at `to`, which is never seen
/// half copied: the copy is made beside it, and renamed over it once whole.
fn copy_over(from: &Path, to: &Path) -> Result<(), Error> {
    let mut beside = to.as_os_str().to_owned();
    beside.push(MOVING);
    let beside = Path::new(&beside);

    let _ = fs::copy(from, beside).map_err(io_at(from))?;
    fs::rename(beside, to).map_err(io_at(to))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Version;
    use crate::testing::{WOTLK, fake_executable, torrent_naming, torrent_of};

    /// A client of a few files, the pieces 16 bytes so most files share
    /// some, with `Wow.exe` first: its flag is in its first piece.
    fn files() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("Wow.exe", fake_executable(WOTLK)),
            ("Data/common.MPQ", b"the world, in many bytes".to_vec()),
            (
                "Data/enUS/realmlist.wtf",
                b"set realmlist logon.example.com".to_vec(),
            ),
            ("Data/enUS/locale-enUS.MPQ", b"words".to_vec()),
            ("WTF/Config.wtf", b"SET locale \"enUS\"".to_vec()),
            (
                "Interface/AddOns/Atlas/Atlas.toc",
                b"## Title: Atlas".to_vec(),
            ),
        ]
    }

    /// The torrent of `files`, and a folder holding them as it has them.
    fn downloaded(files: &[(&str, Vec<u8>)]) -> (tempfile::TempDir, Torrent) {
        let folder = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let path = folder.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        let listed: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(path, contents)| (*path, contents.as_slice()))
            .collect();
        let torrent = Torrent::parse(&torrent_of("WotLK", &listed, 16)).unwrap();

        (folder, torrent)
    }

    fn named(torrent: &Torrent, indices: &[usize]) -> Vec<String> {
        indices
            .iter()
            .map(|index| torrent.files[*index].path.clone())
            .collect()
    }

    /// Checks `folder` through.
    fn checked(folder: &Path, torrent: &Torrent) -> Check {
        check(folder, torrent, &mut |_| ControlFlow::Continue(()))
            .unwrap()
            .unwrap()
    }

    /// Writes `contents` over the file at `path`, keeping when it was
    /// last changed: as if it hadn't been.
    fn sneak(path: &Path, contents: &[u8]) {
        let modified = fs::metadata(path).unwrap().modified().unwrap();
        written(path, contents, modified);
    }

    /// Writes `contents` over the file at `path`, a second after it was
    /// last changed: file times may be coarser than a test is quick.
    fn change(path: &Path, contents: &[u8]) {
        let modified = fs::metadata(path).unwrap().modified().unwrap();
        written(path, contents, modified + std::time::Duration::from_secs(1));
    }

    fn written(path: &Path, contents: &[u8], modified: std::time::SystemTime) {
        fs::write(path, contents).unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    #[test]
    fn a_torrent_says_its_files_and_where_its_client_is() {
        let (_folder, torrent) = downloaded(&files());

        assert_eq!(torrent.name, "WotLK");
        assert_eq!(torrent.info_hash.len(), 40);
        assert_eq!(torrent.files.len(), 6);
        assert_eq!(torrent.client().as_deref(), Some(""));
        assert_eq!(torrent.executable(), Some(0));
        assert_eq!(
            named(
                &torrent,
                &torrent.untouched().into_iter().collect::<Vec<_>>()
            ),
            [
                "Data/enUS/realmlist.wtf",
                "WTF/Config.wtf",
                "Interface/AddOns/Atlas/Atlas.toc",
            ]
        );

        let below: Vec<(&str, &[u8])> = vec![
            ("readme.txt", b"hello"),
            ("World of Warcraft/Data/common.MPQ", b"world"),
            ("World of Warcraft/Wow.exe", b"MZ"),
        ];
        let below = Torrent::parse(&torrent_of("HD", &below, 16)).unwrap();
        assert_eq!(below.client().as_deref(), Some("World of Warcraft/"));

        // 1072 bytes of Wow.exe, then 24, 31, 5, 17 and 15: pieces 0 to 66
        // hold Wow.exe, 67 and 68 common.MPQ, 68 to 70 the realmlist.
        assert_eq!(torrent.pieces(), 73);
        assert_eq!(torrent.pieces_of(&[0]), 67);
        assert_eq!(torrent.pieces_of(&[1, 2]), 4);
        assert_eq!(torrent.pieces_of(&[0, 1, 2, 3, 4, 5]), 73);

        let no_client: Vec<(&str, &[u8])> = vec![("Wow.exe", b"MZ")];
        let no_client = Torrent::parse(&torrent_of("exe", &no_client, 16)).unwrap();
        assert_eq!(no_client.client(), None, "no Data beside it");

        assert!(matches!(
            Torrent::parse(b"<html>not found</html>"),
            Err(Error::Torrent(_))
        ));

        for path in ["../Wow.exe", "Data/../../x", "C:/Windows/x", "a\\..\\..\\b"] {
            let outside: Vec<(&str, &[u8])> = vec![(path, b"MZ")];
            assert!(
                matches!(
                    Torrent::parse(&torrent_of("bad", &outside, 16)),
                    Err(Error::Torrent(_))
                ),
                "{path}"
            );
        }
    }

    #[test]
    fn a_magnet_link_finds_the_torrent_without_its_file() {
        let files: Vec<(&str, &[u8])> = vec![
            ("World of Warcraft/Data/common.MPQ", b"world"),
            ("World of Warcraft/Wow.exe", b"MZ"),
        ];
        let udp = "udp://tracker.example.com:80/announce";
        let https = "https://example.org/announce?key=a b";
        let torrent = Torrent::parse(&torrent_naming(
            "WotLK 3.3.5a",
            &files,
            16,
            &[udp, https, udp],
        ))
        .unwrap();
        assert_eq!(torrent.trackers, [udp, https], "each once");

        let magnet = torrent.magnet();
        assert_eq!(
            magnet,
            format!(
                "magnet:?xt=urn:btih:{}&dn=WotLK%203.3.5a\
                 &tr=udp%3A%2F%2Ftracker.example.com%3A80%2Fannounce\
                 &tr=https%3A%2F%2Fexample.org%2Fannounce%3Fkey%3Da%20b",
                torrent.info_hash
            )
        );
        let parsed = librqbit_core::magnet::Magnet::parse(&magnet).unwrap();
        assert_eq!(parsed.as_id20().unwrap().as_string(), torrent.info_hash);
        assert_eq!(parsed.name.as_deref(), Some("WotLK 3.3.5a"));
        assert_eq!(parsed.trackers, [udp, https]);

        let none = Torrent::parse(&torrent_of("bare", &files, 16)).unwrap();
        assert_eq!(
            none.magnet(),
            format!("magnet:?xt=urn:btih:{}&dn=bare", none.info_hash),
            "found through DHT alone"
        );
    }

    #[test]
    fn a_client_folder_keeps_the_torrent_it_comes_from() {
        let folder = tempfile::tempdir().unwrap();
        let below = folder.path().join("World of Warcraft");
        fs::create_dir(&below).unwrap();
        let hash = "0123456789abcdef0123456789abcdef01234567";

        for (root, client) in [(folder.path(), ""), (below.as_path(), "World of Warcraft/")] {
            assert_eq!(origin(root), None, "none yet");
            assert_eq!(client_in(root, folder.path()).as_deref(), Some(client));
            assert_eq!(folder_of_client(root, client), folder.path());

            let torrent = ClientTorrent {
                name: "WotLK".to_owned(),
                info_hash: hash.to_owned(),
                source: format!("magnet:?xt=urn:btih:{hash}"),
                folder: folder.path().to_path_buf(),
            };
            keep_origin(root, &torrent).unwrap();
            assert_eq!(origin(root), Some(torrent), "{client:?}");
        }
        assert_eq!(client_in(folder.path(), &below), None, "not in it");

        // A hash that can't be one is no origin: it names files.
        Notes::change(&below, |notes| {
            if let Some(origin) = &mut notes.torrent {
                origin.info_hash = "../../elsewhere".to_owned();
            }
        })
        .unwrap();
        assert_eq!(origin(&below), None);
    }

    #[test]
    fn a_folder_as_the_torrent_has_it_is_current() {
        let (folder, torrent) = downloaded(&files());
        let mut read = 0;

        let check = check(folder.path(), &torrent, &mut |bytes| {
            read = bytes;
            ControlFlow::Continue(())
        })
        .unwrap()
        .unwrap();

        assert!(check.is_current(), "{check:?}");
        assert_eq!(read, torrent.size());
    }

    #[test]
    fn a_check_stops_when_asked() {
        let (folder, torrent) = downloaded(&files());
        let mut heard = 0;

        let check = check(folder.path(), &torrent, &mut |_| {
            heard += 1;
            ControlFlow::Break(())
        })
        .unwrap();

        assert_eq!(check, None);
        assert_eq!(heard, 1);
    }

    #[test]
    fn a_later_check_reads_only_the_files_changed_since() {
        let (folder, torrent) = downloaded(&files());
        let root = folder.path();
        assert!(checked(root, &torrent).is_current());

        // Unchanged as far as it can tell: not read again.
        sneak(&root.join("Data/common.MPQ"), b"the world, in MANY bytes");
        assert!(checked(root, &torrent).is_current());

        // Changed: read, and found different.
        change(&root.join("Data/common.MPQ"), b"the world, in MANY bytes");
        let check = checked(root, &torrent);
        assert_eq!(named(&torrent, &check.different), ["Data/common.MPQ"]);

        // Still different the next time, though unchanged since.
        assert_eq!(checked(root, &torrent), check);

        // A record of another torrent is of no use.
        let mut others = files();
        others[1].1 = b"the world, in more bytes".to_vec();
        let (_other, other) = downloaded(&others);
        change(&root.join("Data/common.MPQ"), b"the world, in many bytes");
        assert!(checked(root, &torrent).is_current());
        sneak(&root.join("Data/common.MPQ"), b"the world, in more bytes");
        assert!(checked(root, &other).is_current(), "read, not taken");
        sneak(&root.join("Data/common.MPQ"), b"the world, in MANY bytes");
        let check = checked(root, &torrent);
        assert_eq!(named(&torrent, &check.different), ["Data/common.MPQ"]);
    }

    #[test]
    fn what_a_download_wrote_is_noted_as_the_torrent_has_it() {
        let (folder, torrent) = downloaded(&files());
        let root = folder.path();
        let exe = torrent.executable().unwrap();

        record(root, &torrent, &[exe]).unwrap();
        let rebuilt = Version {
            build: 12341,
            ..WOTLK
        };
        sneak(&root.join("Wow.exe"), &fake_executable(rebuilt));
        sneak(&root.join("Data/common.MPQ"), b"the world, in MANY bytes");

        let check = checked(root, &torrent);
        assert_eq!(
            named(&torrent, &check.different),
            ["Data/common.MPQ"],
            "only what was downloaded is taken as written"
        );
    }

    #[test]
    fn an_update_downloads_beside_the_client_and_moves_in_only_its_files() {
        let (folder, torrent) = downloaded(&files());
        let root = folder.path();
        // Outdated: a game file changed, another missing, and Wow.exe
        // patched, given 4 GB by the player.
        fs::write(root.join("Data/common.MPQ"), b"the world, in MANY bytes").unwrap();
        fs::remove_file(root.join("Data/enUS/locale-enUS.MPQ")).unwrap();
        let mut aware = fs::read(root.join("Wow.exe")).unwrap();
        aware[0x126] |= 0x20;
        let mut patched = aware.clone();
        patched[0x200] ^= 0xFF;
        fs::write(root.join("Wow.exe"), &patched).unwrap();
        // The player's own server.
        let realmlist = root.join("Data/enUS/realmlist.wtf");
        fs::write(&realmlist, b"set realmlist logon.other.org").unwrap();

        let files = checked(root, &torrent).files();
        assert_eq!(
            named(&torrent, &files),
            ["Wow.exe", "Data/common.MPQ", "Data/enUS/locale-enUS.MPQ"]
        );
        let elsewhere = tempfile::tempdir().unwrap();
        let staging = elsewhere.path().join("staging");
        stage(&staging, root, &torrent, &files).unwrap();
        assert_eq!(
            fs::read(staging.join("Data/common.MPQ")).unwrap(),
            b"the world, in MANY bytes",
            "what is there, for its pieces as they are"
        );
        assert!(!staging.join("Data/enUS/locale-enUS.MPQ").exists());

        // The download: whole pieces, over every file they share.
        for (path, contents) in self::files() {
            let path = staging.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        let mut moved = 0;
        move_into(&staging, root, &torrent, &files, &mut |bytes| moved = bytes).unwrap();

        assert!(checked(root, &torrent).is_current());
        assert_eq!(
            fs::read(root.join("Wow.exe")).unwrap(),
            aware,
            "as released, with the player's 4 GB"
        );
        assert_eq!(
            fs::read(&realmlist).unwrap(),
            b"set realmlist logon.other.org"
        );
        assert_eq!(moved, 1072 + 24 + 5);
        assert!(!staging.exists(), "nothing else left");
    }

    /// `cargo test -p old_launcher_core torrent::tests::files_move_across_disks
    /// -- --ignored`, with `OLD_LAUNCHER_TEST_OTHER_DISK` a folder on another
    /// disk than the temporary folder's: a download's files are copied
    /// there, whole, and the staging folder goes.
    #[test]
    #[ignore = "moves files into $OLD_LAUNCHER_TEST_OTHER_DISK, on another disk"]
    fn files_move_across_disks() {
        let other = std::path::PathBuf::from(
            std::env::var("OLD_LAUNCHER_TEST_OTHER_DISK")
                .expect("OLD_LAUNCHER_TEST_OTHER_DISK is a folder on another disk"),
        );
        let (staging, torrent) = downloaded(&files());
        let folder = tempfile::tempdir_in(&other).unwrap();
        let probe = staging.path().join("probe");
        fs::write(&probe, b"?").unwrap();
        assert_eq!(
            fs::rename(&probe, folder.path().join("probe"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::CrossesDevices,
            "the two are on one disk"
        );
        fs::remove_file(&probe).unwrap();

        let all: Vec<usize> = (0..torrent.files.len()).collect();
        let mut moved = 0;
        move_into(
            staging.path(),
            folder.path(),
            &torrent,
            &all,
            &mut |bytes| moved = bytes,
        )
        .unwrap();

        assert!(checked(folder.path(), &torrent).is_current());
        assert_eq!(moved, torrent.size());
        assert!(!staging.path().exists());
        let mut left = Vec::new();
        for entry in walkdir(folder.path()) {
            left.push(entry);
        }
        assert!(
            left.iter().all(|path| !path.ends_with(MOVING)),
            "no copy left half way: {left:?}"
        );
    }

    /// Every file under `folder`, as a path.
    fn walkdir(folder: &Path) -> Vec<String> {
        let mut found = Vec::new();
        for entry in fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                found.extend(walkdir(&path));
            } else {
                found.push(path.display().to_string());
            }
        }
        found
    }

    #[test]
    fn staging_keeps_what_an_update_cut_short_left_and_copies_are_whole() {
        let (folder, torrent) = downloaded(&files());
        let elsewhere = tempfile::tempdir().unwrap();
        let staging = elsewhere.path().join("staging");
        fs::create_dir_all(staging.join("Data")).unwrap();
        fs::write(staging.join("Data/common.MPQ"), b"the world, in so").unwrap();

        stage(&staging, folder.path(), &torrent, &[0, 1]).unwrap();
        assert_eq!(
            fs::read(staging.join("Data/common.MPQ")).unwrap(),
            b"the world, in so",
            "downloaded already"
        );
        assert!(staging.join("Wow.exe").is_file());

        // Across disks, a copy beside the file, renamed over it.
        let to = folder.path().join("Data/common.MPQ");
        copy_over(&staging.join("Data/common.MPQ"), &to).unwrap();
        assert_eq!(fs::read(&to).unwrap(), b"the world, in so");
        assert_eq!(
            fs::read_dir(folder.path().join("Data")).unwrap().count(),
            2,
            "no copy left beside it: {:?}",
            fs::read_dir(folder.path().join("Data"))
                .unwrap()
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn missing_and_changed_files_are_told_apart_and_the_players_left_out() {
        let (folder, torrent) = downloaded(&files());
        let root = folder.path();
        fs::remove_file(root.join("Data/enUS/locale-enUS.MPQ")).unwrap();
        fs::write(root.join("Data/common.MPQ"), b"the world, in MANY bytes").unwrap();
        // The player's: their server, settings and addons.
        fs::write(
            root.join("Data/enUS/realmlist.wtf"),
            b"set realmlist logon.other.org",
        )
        .unwrap();
        fs::write(root.join("WTF/Config.wtf"), b"SET locale \"esES\" and more").unwrap();
        fs::remove_dir_all(root.join("Interface")).unwrap();

        let check = checked(root, &torrent);

        assert_eq!(
            named(&torrent, &check.missing),
            ["Data/enUS/locale-enUS.MPQ"]
        );
        assert_eq!(named(&torrent, &check.different), ["Data/common.MPQ"]);
        assert_eq!(check.bytes(&torrent), 24 + 5);
    }

    #[test]
    fn a_file_sharing_a_piece_with_one_to_download_is_read_again_once_it_is() {
        // Pieces of 16 bytes: 1 holds the end of a and the start of b.
        let files = vec![("a.MPQ", vec![1; 20]), ("b.MPQ", vec![2; 20])];
        let (folder, torrent) = downloaded(&files);
        let (a, b) = (folder.path().join("a.MPQ"), folder.path().join("b.MPQ"));

        fs::remove_file(&b).unwrap();
        let check = checked(folder.path(), &torrent);
        assert_eq!(named(&torrent, &check.missing), ["b.MPQ"]);
        assert!(check.different.is_empty(), "{check:?}");

        // a's end, in the piece b held some of, goes wrong unnoticed, and b
        // is downloaded.
        let mut changed = vec![1; 20];
        changed[18] = 3;
        sneak(&a, &changed);
        fs::write(&b, vec![2; 20]).unwrap();
        record(folder.path(), &torrent, &[1]).unwrap();

        let check = checked(folder.path(), &torrent);
        assert_eq!(
            named(&torrent, &check.different),
            ["a.MPQ"],
            "a's last piece was never read: a wasn't noted as the torrent has it"
        );
    }

    #[test]
    fn the_launchers_own_notes_in_a_torrent_are_neither_compared_nor_taken() {
        let mut files = files();
        files.push((".old_launcher.toml", b"[mods.theirs]\n".to_vec()));
        let (folder, torrent) = downloaded(&files);
        let root = folder.path();
        let notes = root.join(".old_launcher.toml");
        assert_eq!(
            named(
                &torrent,
                &torrent.untouched().into_iter().collect::<Vec<_>>()
            )[3],
            ".old_launcher.toml"
        );

        // The torrent's copy is what the folder noted, as far as the check
        // is concerned: it goes, and is neither missing nor different.
        fs::remove_file(&notes).unwrap();
        assert!(checked(root, &torrent).is_current());
        assert!(notes.is_file(), "what the check noted");
        assert!(!fs::read_to_string(&notes).unwrap().contains("theirs"));

        // Downloaded whole, the torrent's copy stays in the staging folder.
        let elsewhere = tempfile::tempdir().unwrap();
        let staging = elsewhere.path().join("staging");
        for (path, contents) in &files {
            let path = staging.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        let all: Vec<usize> = (0..torrent.files.len()).collect();
        move_into(&staging, root, &torrent, &all, &mut |_| {}).unwrap();
        assert!(!fs::read_to_string(&notes).unwrap().contains("theirs"));
        assert!(!staging.exists());
    }

    #[test]
    fn a_piece_two_files_share_blames_the_one_found_different_elsewhere() {
        // Pieces of 16 bytes: 0 holds a's first 16, 1 the rest of a and
        // the start of b, 2 the rest of b.
        let files = vec![("a.MPQ", vec![1; 20]), ("b.MPQ", vec![2; 20])];
        let (folder, torrent) = downloaded(&files);
        let b = folder.path().join("b.MPQ");

        let mut changed = vec![2; 20];
        changed[0] = 3;
        changed[19] = 3;
        fs::write(&b, &changed).unwrap();
        let check = checked(folder.path(), &torrent);
        assert_eq!(named(&torrent, &check.different), ["b.MPQ"]);

        // Nothing tells which of the two changed: both.
        changed[19] = 2;
        fs::write(&b, &changed).unwrap();
        fs::remove_file(folder.path().join(crate::notes::FILE)).unwrap();
        let check = checked(folder.path(), &torrent);
        assert_eq!(named(&torrent, &check.different), ["a.MPQ", "b.MPQ"]);
    }

    #[test]
    fn wow_exe_given_4_gb_is_as_released_and_any_other_change_is_not() {
        let (folder, torrent) = downloaded(&files());
        let exe = folder.path().join("Wow.exe");
        let shipped = fs::read(&exe).unwrap();

        let mut aware = shipped.clone();
        aware[0x126] |= 0x20;
        fs::write(&exe, &aware).unwrap();
        assert!(checked(folder.path(), &torrent).is_current());

        // Changed after the check noted it: file times may be coarser than
        // a test is quick.
        let mut patched = shipped;
        patched[0x200] ^= 0xFF;
        change(&exe, &patched);
        let check = checked(folder.path(), &torrent);
        assert_eq!(named(&torrent, &check.different), ["Wow.exe"]);
    }

    /// `cargo test -p old_launcher_core --release torrent::tests::a_real_client
    /// -- --ignored --nocapture`, with `OLD_LAUNCHER_TEST_CLIENT_FILES` a client folder,
    /// only ever read: a temporary folder links to its files, a torrent of
    /// 4 MB pieces is made of them, and the folder of links is checked
    /// against it twice, saying how long each check took.
    #[test]
    #[cfg(unix)]
    #[ignore = "reads the client folder in $OLD_LAUNCHER_TEST_CLIENT_FILES whole, three times"]
    fn a_real_client_is_read_whole_once_then_only_what_changed() {
        const PIECE: usize = 4 * 1024 * 1024;

        fn listed(root: &Path, folder: &Path, files: &mut Vec<String>) {
            for entry in fs::read_dir(folder).unwrap() {
                let path = entry.unwrap().path();
                let kind = fs::symlink_metadata(&path).unwrap().file_type();
                if kind.is_dir() {
                    listed(root, &path, files);
                } else if kind.is_file() {
                    let relative = path.strip_prefix(root).unwrap();
                    files.push(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }

        fn string(out: &mut Vec<u8>, bytes: &[u8]) {
            out.extend(bytes.len().to_string().as_bytes());
            out.push(b':');
            out.extend(bytes);
        }

        let client = std::path::PathBuf::from(
            std::env::var("OLD_LAUNCHER_TEST_CLIENT_FILES")
                .expect("OLD_LAUNCHER_TEST_CLIENT_FILES is a client folder to read"),
        );
        let mut files = Vec::new();
        listed(&client, &client, &mut files);
        files.sort();

        let links = tempfile::tempdir().unwrap();
        let started = std::time::Instant::now();
        let (mut pieces, mut piece, mut info_files) = (Vec::new(), Vec::new(), Vec::new());
        let mut total = 0;
        for path in &files {
            let link = links.path().join(path);
            fs::create_dir_all(link.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(client.join(path), &link).unwrap();

            let mut file = fs::File::open(client.join(path)).unwrap();
            let length = file.metadata().unwrap().len();
            let mut buffer = vec![0; PIECE];
            loop {
                let read = file.read(&mut buffer[..PIECE - piece.len()]).unwrap();
                if read == 0 {
                    break;
                }
                piece.extend(&buffer[..read]);
                if piece.len() == PIECE {
                    pieces.extend(Sha1::digest(&piece));
                    piece.clear();
                }
            }
            total += length;

            info_files.extend(format!("d6:lengthi{length}e4:pathl").as_bytes());
            for part in path.split('/') {
                string(&mut info_files, part.as_bytes());
            }
            info_files.extend(b"ee");
        }
        if !piece.is_empty() {
            pieces.extend(Sha1::digest(&piece));
        }
        let mut bytes = b"d4:infod5:filesl".to_vec();
        bytes.extend(info_files);
        bytes.extend(format!("e4:name4:test12:piece lengthi{PIECE}e6:pieces").as_bytes());
        string(&mut bytes, &pieces);
        bytes.extend(b"ee");
        let torrent = Torrent::parse(&bytes).unwrap();
        println!(
            "Made a torrent of {} files, {}, in {:.1?}",
            files.len(),
            total,
            started.elapsed()
        );
        assert_eq!(
            torrent
                .executable()
                .map(|index| &*torrent.files[index].path),
            Some("Wow.exe")
        );

        for pass in ["First", "Second"] {
            let started = std::time::Instant::now();
            let mut read = 0;
            let check = check(links.path(), &torrent, &mut |bytes| {
                read = bytes;
                ControlFlow::Continue(())
            })
            .unwrap()
            .unwrap();
            println!("{pass} check: {:.1?}, {check:?}", started.elapsed());
            assert!(check.is_current());
            assert_eq!(read, total);
        }
    }

    #[test]
    fn the_players_files_are_known_by_their_place() {
        for path in [
            "WTF/Config.wtf",
            "wtf/Account/X/SavedVariables/A.lua",
            "Interface/AddOns/Atlas/Atlas.toc",
            "Data/esES/realmlist.wtf",
            "Screenshots/WoWScrnShot_1.jpg",
            "Cache/WDB/enUS/creaturecache.wdb",
        ] {
            assert!(is_players(path), "{path}");
        }
        for path in ["Wow.exe", "Data/common.MPQ", "Interface/Glues/x.blp"] {
            assert!(!is_players(path), "{path}");
        }

        for path in [".old_launcher.toml", ".old_launcher-lumenveil.toml"] {
            assert!(is_launchers(path), "{path}");
        }
        for path in ["Wow.exe", "Data/.old_launcher.toml", ".download"] {
            assert!(!is_launchers(path), "{path}");
        }
    }
}
