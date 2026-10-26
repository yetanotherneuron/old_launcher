//! Fixtures for tests, here and in the crates using this one (with the
//! `testing` feature): a fake 3.3.5a client in a temporary directory.

// Fixtures fail like tests do, by panicking, even when built as a feature.
#![allow(clippy::panic)]

use std::fs;
use std::path::Path;

use crate::{Installation, Version};

/// The 3.3.5a client version.
pub const WOTLK: Version = Version {
    major: 3,
    minor: 3,
    patch: 5,
    build: 12340,
};

/// The headers of 3.3.5a's `Wow.exe` (its first KB, up to where `.text`
/// starts), as the client ships them: not Large Address Aware.
const HEADERS: &[u8] = include_bytes!("testing/Wow.exe-headers");

/// A fake executable: the headers of 3.3.5a's `Wow.exe`, and a version
/// resource that says `version`, as [`Version::from_executable`] reads it.
pub fn fake_executable(version: Version) -> Vec<u8> {
    let mut bytes = HEADERS.to_vec();
    bytes.extend(0xFEEF_04BDu32.to_le_bytes());
    bytes.extend(0x0001_0000u32.to_le_bytes());
    bytes.extend((u32::from(version.major) << 16 | u32::from(version.minor)).to_le_bytes());
    bytes.extend((u32::from(version.patch) << 16 | u32::from(version.build)).to_le_bytes());
    bytes.extend([0; 32]);
    bytes
}

/// A 3.3.5a client in a temporary directory: a `Wow.exe` whose version
/// resource says 3.3.5.12340, plus empty `files` (relative paths).
pub fn client(files: &[&str]) -> (tempfile::TempDir, Installation) {
    let with_contents: Vec<(&str, &str)> = files.iter().map(|file| (*file, "")).collect();

    client_with(&with_contents)
}

/// A 3.3.5a client in a temporary directory: a `Wow.exe` whose version
/// resource says 3.3.5.12340, plus `files` (relative path, contents).
pub fn client_with(files: &[(&str, &str)]) -> (tempfile::TempDir, Installation) {
    let root = tempfile::tempdir().unwrap_or_else(|error| panic!("temporary directory: {error}"));
    write(root.path(), "Wow.exe", &fake_executable(WOTLK));

    for (file, contents) in files {
        write(root.path(), file, contents.as_bytes());
    }

    let installation = Installation::open(root.path())
        .unwrap_or_else(|error| panic!("{}: {error}", root.path().display()));

    (root, installation)
}

/// A `.torrent` named `name` of `files` (path with `/` between folders,
/// contents), in that order, in pieces of `piece_length` bytes.
pub fn torrent_of(name: &str, files: &[(&str, &[u8])], piece_length: usize) -> Vec<u8> {
    torrent_naming(name, files, piece_length, &[])
}

/// [`torrent_of`], naming the `trackers` that find its peers.
pub fn torrent_naming(
    name: &str,
    files: &[(&str, &[u8])],
    piece_length: usize,
    trackers: &[&str],
) -> Vec<u8> {
    use sha1::{Digest, Sha1};

    fn string(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend(format!("{}:", bytes.len()).as_bytes());
        out.extend(bytes);
    }

    let stream: Vec<u8> = files
        .iter()
        .flat_map(|(_, contents)| contents.iter().copied())
        .collect();
    let pieces: Vec<u8> = stream
        .chunks(piece_length)
        .flat_map(|piece| Sha1::digest(piece).to_vec())
        .collect();

    // Bencoded, with the keys of each dictionary sorted.
    let mut info = b"d".to_vec();
    string(&mut info, b"files");
    info.push(b'l');
    for (path, contents) in files {
        info.push(b'd');
        string(&mut info, b"length");
        info.extend(format!("i{}e", contents.len()).as_bytes());
        string(&mut info, b"path");
        info.push(b'l');
        for part in path.split('/') {
            string(&mut info, part.as_bytes());
        }
        info.extend(b"ee");
    }
    info.push(b'e');
    string(&mut info, b"name");
    string(&mut info, name.as_bytes());
    string(&mut info, b"piece length");
    info.extend(format!("i{piece_length}e").as_bytes());
    string(&mut info, b"pieces");
    string(&mut info, &pieces);
    info.push(b'e');

    let mut torrent = b"d".to_vec();
    if let Some(first) = trackers.first() {
        string(&mut torrent, b"announce");
        string(&mut torrent, first.as_bytes());
        string(&mut torrent, b"announce-list");
        torrent.push(b'l');
        for tracker in trackers {
            torrent.push(b'l');
            string(&mut torrent, tracker.as_bytes());
            torrent.push(b'e');
        }
        torrent.push(b'e');
    }
    string(&mut torrent, b"info");
    torrent.extend(info);
    torrent.push(b'e');
    torrent
}

/// Writes `contents` to `file` under `root`, creating its folders.
fn write(root: &Path, file: &str, contents: &[u8]) {
    let path = root.join(file);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|error| panic!("{}: {error}", parent.display()));
    }
    fs::write(&path, contents).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}
