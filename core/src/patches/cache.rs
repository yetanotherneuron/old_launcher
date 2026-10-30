//! What the launcher keeps of reading the archives: the [`Facts`] of each,
//! in files of its own under one folder, so an archive is read once until
//! it changes.
//!
//! A client's archives are kept by client (`<client>/<archive>.json`), each
//! stamped with the archive's size and modification time, and with a digest
//! of whatever else its facts depend on (see [`Cache::of_archive`]). The
//! archives of a release are kept apart (`releases/<sha256>.json`), by the
//! checksum the list of mods gives them: the same in every client, so the
//! mods' archives are read once, however many clients have them.
//!
//! The cache doesn't read archives itself: the caller says how to compute
//! the facts of the one it misses. It is only ever an optimisation: a file
//! that doesn't read, or parse, counts as absent, and one that can't be
//! written is logged, never an error.
//!
//! File names are digests, so no two differ only by case, as on Windows
//! they couldn't.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::Facts;
use crate::Error;

/// The folder of the archives that belong to no client.
const RELEASES: &str = "releases";

/// The extension of a file of the cache.
const EXTENSION: &str = "json";

/// The version of what the facts say, written with each file. Files of
/// another version (those from before it was written count as 0) are read
/// again, since what they would say has changed. Raise it whenever the facts
/// of an archive come out different: 1 added the fingerprints of Blizzard's
/// rows and left out the textures of a model that nothing draws.
const FORMAT: u32 = 1;

/// The facts of an archive of a client, with what they were read from:
/// owned when read back, borrowed when written.
#[derive(Serialize, Deserialize)]
struct Archive<F = Facts> {
    /// The version of the facts (see [`FORMAT`]).
    #[serde(default)]
    format: u32,
    /// The archive, relative to the client folder.
    path: String,
    /// Its size, in bytes.
    size: u64,
    /// Its modification time, in nanoseconds since the epoch.
    modified: u64,
    /// The digest the caller gave of what else the facts depend on.
    pool: String,
    facts: F,
}

/// The facts of the archive of a release, owned or borrowed as [`Archive`]'s.
#[derive(Serialize, Deserialize)]
struct Release<F = Facts> {
    /// The version of the facts (see [`FORMAT`]).
    #[serde(default)]
    format: u32,
    /// Its checksum.
    sha256: String,
    facts: F,
}

/// The folder the facts of the archives are kept in.
#[derive(Clone, Debug)]
pub struct Cache {
    dir: PathBuf,
}

impl Cache {
    /// The cache in `dir`, which needn't exist yet.
    pub fn at(dir: impl Into<PathBuf>) -> Cache {
        Cache { dir: dir.into() }
    }

    /// The facts of the archive at `path` (relative to the client folder
    /// `root`), from the cache when its size and modification time match,
    /// and so does `pool`, else from `compute`, and kept. `pool` is a digest
    /// of what else the facts depend on: an archive without a list of its
    /// files is named by the other archives, so the caller digests those
    /// names, and passes `""` when nothing else matters.
    ///
    /// Nothing is kept for an archive whose size and time can't be told.
    pub fn of_archive(
        &self,
        root: &Path,
        path: &str,
        pool: &str,
        compute: impl FnOnce() -> Result<Facts, Error>,
    ) -> Result<Facts, Error> {
        let Some((size, modified)) = stamp(&root.join(path)) else {
            return compute();
        };
        let file = self.archive_file(root, path);

        if let Some(facts) = self.archive(root, path, pool) {
            return Ok(facts);
        }

        let facts = compute()?;
        write(
            &file,
            &Archive {
                format: FORMAT,
                path: path.to_owned(),
                size,
                modified,
                pool: pool.to_owned(),
                facts: &facts,
            },
        );

        Ok(facts)
    }

    /// The facts kept of the archive at `path` of the client at `root`, if
    /// they are there and its stamp and `pool` still match, without
    /// computing any: whether the archive needs reading.
    pub fn archive(&self, root: &Path, path: &str, pool: &str) -> Option<Facts> {
        let (size, modified) = stamp(&root.join(path))?;
        let kept: Archive = read(&self.archive_file(root, path))?;

        (kept.format == FORMAT
            && kept.path == path
            && kept.size == size
            && kept.modified == modified
            && kept.pool == pool)
            .then_some(kept.facts)
    }

    /// The facts of the archive of a release whose checksum is `sha256`,
    /// from the cache or else from `compute`, and kept. A release's archive
    /// is the same in every client, so is what it says.
    pub fn of_release(
        &self,
        sha256: &str,
        compute: impl FnOnce() -> Result<Facts, Error>,
    ) -> Result<Facts, Error> {
        if let Some(facts) = self.release(sha256) {
            return Ok(facts);
        }

        let facts = compute()?;
        if let Some(file) = self.release_file(sha256) {
            write(
                &file,
                &Release {
                    format: FORMAT,
                    sha256: sha256.to_ascii_lowercase(),
                    facts: &facts,
                },
            );
        }

        Ok(facts)
    }

    /// The facts kept of the archive of a release, without computing any:
    /// whether they are known yet.
    pub fn release(&self, sha256: &str) -> Option<Facts> {
        let kept: Release = read(&self.release_file(sha256)?)?;

        (kept.format == FORMAT && kept.sha256.eq_ignore_ascii_case(sha256)).then_some(kept.facts)
    }

    /// Drops what is kept of the client folder at `root`'s archives but
    /// those at `keep` (relative to it): the ones no longer there.
    pub fn sweep(&self, root: &Path, keep: &[String]) {
        let folder = self.client_dir(root);
        let keep: Vec<String> = keep
            .iter()
            .map(|path| format!("{}.{EXTENSION}", digest(path)))
            .collect();
        let Ok(entries) = fs::read_dir(&folder) else {
            return;
        };

        for entry in entries.flatten() {
            let kept = entry
                .file_name()
                .to_str()
                .is_some_and(|name| keep.iter().any(|keep| keep == name));
            if !kept {
                discard(&entry.path());
            }
        }
    }

    /// Drops everything kept of the client folder at `root`: the launcher
    /// no longer has it.
    pub fn forget(&self, root: &Path) {
        let folder = self.client_dir(root);

        if let Err(error) = fs::remove_dir_all(&folder)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("Can't drop {}: {error}", folder.display());
        }
    }

    /// The folder of the client at `root`.
    fn client_dir(&self, root: &Path) -> PathBuf {
        let root = dunce::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());

        self.dir.join(digest(&root.to_string_lossy()))
    }

    /// The file of the archive at `path` of the client at `root`.
    fn archive_file(&self, root: &Path, path: &str) -> PathBuf {
        self.client_dir(root)
            .join(format!("{}.{EXTENSION}", digest(path)))
    }

    /// The file of a release's archive, unless `sha256` isn't a checksum:
    /// it names a file, so it can't be anything else.
    fn release_file(&self, sha256: &str) -> Option<PathBuf> {
        (sha256.len() == 64 && sha256.bytes().all(|byte| byte.is_ascii_hexdigit())).then(|| {
            self.dir
                .join(RELEASES)
                .join(format!("{}.{EXTENSION}", sha256.to_ascii_lowercase()))
        })
    }
}

/// The first 16 hex digits of the SHA-256 of `text`.
fn digest(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The size and modification time (in nanoseconds) of the file at `path`.
fn stamp(path: &Path) -> Option<(u64, u64)> {
    let metadata = fs::metadata(path).ok()?;
    let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;

    Some((metadata.len(), u64::try_from(modified.as_nanos()).ok()?))
}

/// What the file at `path` holds, if it reads and parses.
fn read<T: for<'a> Deserialize<'a>>(path: &Path) -> Option<T> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// Writes `value` to `path`, beside it first and renamed over it once
/// whole, so it is never seen half written. Failing is logged.
fn write<T: Serialize>(path: &Path, value: &T) {
    let written = (|| -> std::io::Result<()> {
        let folder = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(folder)?;

        let mut partial = tempfile::NamedTempFile::new_in(folder)?;
        let mut writer = std::io::BufWriter::new(partial.as_file_mut());
        serde_json::to_writer(&mut writer, value).map_err(std::io::Error::other)?;
        writer.flush()?;
        drop(writer);
        let _ = partial.persist(path).map_err(|error| error.error)?;

        Ok(())
    })();

    if let Err(error) = written {
        log::warn!("Can't keep {}: {error}", path.display());
    }
}

/// Removes the file at `path`, logging a failure.
fn discard(path: &Path) {
    if let Err(error) = fs::remove_file(path)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log::warn!("Can't drop {}: {error}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    /// Facts that tell them apart by one problem.
    fn facts(problem: &str) -> Facts {
        Facts {
            problems: vec![problem.to_owned()],
            ..Facts::default()
        }
    }

    /// A client folder with `Data/patch-x.mpq`.
    fn client() -> (tempfile::TempDir, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("Data")).unwrap();
        fs::write(root.path().join("Data/patch-x.mpq"), "one").unwrap();

        (root, tempfile::tempdir().unwrap())
    }

    #[test]
    fn an_archive_is_read_once_until_its_stamp_changes() {
        let (root, dir) = client();
        let (root, cache) = (root.path(), Cache::at(dir.path()));
        let calls = Cell::new(0);
        let read = |problem: &str| {
            cache
                .of_archive(root, "Data/patch-x.mpq", "", || {
                    calls.set(calls.get() + 1);
                    Ok(facts(problem))
                })
                .unwrap()
        };

        assert_eq!(read("first"), facts("first"));
        assert_eq!(read("second"), facts("first"), "kept");
        assert_eq!(calls.get(), 1);

        // Another size, whatever the time.
        fs::write(root.join("Data/patch-x.mpq"), "three").unwrap();
        assert_eq!(read("third"), facts("third"));
        assert_eq!(read("fourth"), facts("third"));
        assert_eq!(calls.get(), 2);

        // The same size at another time.
        let file = fs::File::options()
            .write(true)
            .open(root.join("Data/patch-x.mpq"))
            .unwrap();
        file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
            .unwrap();
        assert_eq!(read("fifth"), facts("fifth"));
        assert_eq!(calls.get(), 3);

        // Another cache over the same folder finds it.
        let again = Cache::at(dir.path());
        assert_eq!(
            again
                .of_archive(root, "Data/patch-x.mpq", "", || Err(Error::UnknownArchive))
                .unwrap(),
            facts("fifth")
        );
    }

    #[test]
    fn a_failure_to_compute_is_not_kept() {
        let (root, dir) = client();
        let cache = Cache::at(dir.path());

        assert!(
            cache
                .of_archive(root.path(), "Data/patch-x.mpq", "", || Err(
                    Error::UnknownArchive
                ))
                .is_err()
        );
        assert_eq!(
            cache
                .of_archive(root.path(), "Data/patch-x.mpq", "", || Ok(facts("ok")))
                .unwrap(),
            facts("ok")
        );
        assert!(
            cache
                .of_archive(root.path(), "Data/missing.mpq", "", || Ok(facts("none")))
                .is_ok(),
            "an archive that isn't there is computed, not kept"
        );
    }

    #[test]
    fn a_releases_facts_are_kept_by_checksum_whatever_the_path() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::at(dir.path());
        let sum = "AB".repeat(32);

        assert_eq!(cache.release(&sum), None, "not known yet");
        assert_eq!(
            cache.of_release(&sum, || Ok(facts("release"))).unwrap(),
            facts("release")
        );
        assert_eq!(cache.release(&sum), Some(facts("release")));
        assert_eq!(
            cache
                .of_release(&sum.to_ascii_lowercase(), || Err(Error::UnknownArchive))
                .unwrap(),
            facts("release"),
            "in any case, and without computing"
        );
        assert!(
            dir.path()
                .join("releases")
                .join(format!("{}.json", "ab".repeat(32)))
                .is_file()
        );

        // A checksum that is not one names no file.
        assert_eq!(
            cache.of_release("../escape", || Ok(facts("odd"))).unwrap(),
            facts("odd")
        );
        assert_eq!(cache.release("../escape"), None);
        assert!(!dir.path().join("escape.json").exists());
    }

    #[test]
    fn a_cache_file_that_doesnt_read_is_computed_again() {
        let (root, dir) = client();
        let cache = Cache::at(dir.path());
        let archive = || {
            cache
                .of_archive(root.path(), "Data/patch-x.mpq", "", || Ok(facts("fresh")))
                .unwrap()
        };
        let sum = "cd".repeat(32);

        let _ = archive();
        let _ = cache.of_release(&sum, || Ok(facts("fresh"))).unwrap();
        let file = cache.archive_file(root.path(), "Data/patch-x.mpq");
        assert!(file.is_file());

        for broken in [&b"{ not json"[..], b"", b"{}", b"\xff\xfe"] {
            fs::write(&file, broken).unwrap();
            fs::write(cache.release_file(&sum).unwrap(), broken).unwrap();

            assert_eq!(archive(), facts("fresh"));
            assert_eq!(cache.release(&sum), None);
            assert_eq!(
                cache.of_release(&sum, || Ok(facts("again"))).unwrap(),
                facts("again")
            );
            assert_eq!(cache.release(&sum), Some(facts("again")), "written anew");
        }

        // A folder where a file goes: not kept, not an error.
        fs::remove_file(&file).unwrap();
        fs::create_dir(&file).unwrap();
        assert_eq!(archive(), facts("fresh"));
    }

    #[test]
    fn facts_stripped_in_the_old_format_are_read_again() {
        let (root, dir) = client();
        let (root, cache) = (root.path(), Cache::at(dir.path()));
        let sum = "12".repeat(32);
        let _ = cache
            .of_archive(root, "Data/patch-x.mpq", "", || Ok(facts("old")))
            .unwrap();
        let _ = cache.of_release(&sum, || Ok(facts("old"))).unwrap();

        // Files written before the format was told lack it.
        for file in [
            cache.archive_file(root, "Data/patch-x.mpq"),
            cache.release_file(&sum).unwrap(),
        ] {
            let mut kept: serde_json::Value =
                serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
            assert_eq!(kept["format"], FORMAT);
            kept.as_object_mut().unwrap().remove("format");
            fs::write(&file, serde_json::to_vec(&kept).unwrap()).unwrap();
        }

        assert_eq!(cache.archive(root, "Data/patch-x.mpq", ""), None);
        assert_eq!(cache.release(&sum), None);
        assert_eq!(
            cache
                .of_archive(root, "Data/patch-x.mpq", "", || Ok(facts("new")))
                .unwrap(),
            facts("new")
        );
        assert_eq!(
            cache.of_release(&sum, || Ok(facts("new"))).unwrap(),
            facts("new")
        );
        assert_eq!(
            cache.archive(root, "Data/patch-x.mpq", ""),
            Some(facts("new"))
        );
    }

    #[test]
    fn a_different_name_pool_reads_an_archive_again() {
        let (root, dir) = client();
        let cache = Cache::at(dir.path());
        let calls = Cell::new(0);
        let read = |pool: &str| {
            cache
                .of_archive(root.path(), "Data/patch-x.mpq", pool, || {
                    calls.set(calls.get() + 1);
                    Ok(facts(pool))
                })
                .unwrap()
        };

        assert_eq!(read("a"), facts("a"));
        assert_eq!(read("a"), facts("a"));
        assert_eq!(calls.get(), 1);
        assert_eq!(read("b"), facts("b"));
        assert_eq!(calls.get(), 2);
        assert_eq!(read("b"), facts("b"));
        assert_eq!(calls.get(), 2);
    }

    #[test]
    fn sweeping_keeps_the_archives_present_and_forgetting_removes_the_client() {
        let (root, dir) = client();
        let (root, cache) = (root.path(), Cache::at(dir.path()));
        fs::write(root.join("Data/patch-y.mpq"), "two").unwrap();
        let other = tempfile::tempdir().unwrap();
        fs::create_dir(other.path().join("Data")).unwrap();
        fs::write(other.path().join("Data/patch-x.mpq"), "other").unwrap();
        let sum = "ef".repeat(32);

        for path in ["Data/patch-x.mpq", "Data/patch-y.mpq"] {
            let _ = cache
                .of_archive(root, path, "", || Ok(facts(path)))
                .unwrap();
        }
        let _ = cache
            .of_archive(other.path(), "Data/patch-x.mpq", "", || Ok(facts("other")))
            .unwrap();
        let _ = cache.of_release(&sum, || Ok(facts("release"))).unwrap();

        cache.sweep(root, &["Data/patch-y.mpq".to_owned()]);
        let kept = |path: &str| cache.archive_file(root, path).is_file();
        assert!(!kept("Data/patch-x.mpq"), "no longer there");
        assert!(kept("Data/patch-y.mpq"));
        assert!(
            cache
                .archive_file(other.path(), "Data/patch-x.mpq")
                .is_file()
        );
        assert!(cache.release(&sum).is_some());

        cache.forget(root);
        assert!(!cache.client_dir(root).exists());
        assert!(
            cache
                .archive_file(other.path(), "Data/patch-x.mpq")
                .is_file()
        );
        assert!(cache.release(&sum).is_some());

        // Neither fails over what isn't there.
        cache.forget(root);
        cache.sweep(root, &[]);
        Cache::at(dir.path().join("none")).sweep(root, &[]);
    }

    #[test]
    fn what_is_kept_can_be_asked_for_without_computing_any() {
        let (root, dir) = client();
        let (root, cache) = (root.path(), Cache::at(dir.path()));

        assert_eq!(cache.archive(root, "Data/patch-x.mpq", ""), None);
        let _ = cache
            .of_archive(root, "Data/patch-x.mpq", "pool", || Ok(facts("kept")))
            .unwrap();

        assert_eq!(
            cache.archive(root, "Data/patch-x.mpq", "pool"),
            Some(facts("kept"))
        );
        assert_eq!(
            cache.archive(root, "Data/patch-x.mpq", ""),
            None,
            "the pool"
        );
        assert_eq!(cache.archive(root, "Data/none.mpq", "pool"), None);
    }
}
