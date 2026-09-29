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

/// A fake executable whose version resource says `version`, as
/// [`Version::from_executable`] reads it.
pub fn fake_executable(version: Version) -> Vec<u8> {
    let mut bytes = b"MZ\x90\x00 not really a PE file ".to_vec();
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

/// Writes `contents` to `file` under `root`, creating its folders.
fn write(root: &Path, file: &str, contents: &[u8]) {
    let path = root.join(file);

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap_or_else(|error| panic!("{}: {error}", parent.display()));
    }
    fs::write(&path, contents).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
}
