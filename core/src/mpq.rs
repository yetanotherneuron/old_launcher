//! A reader of the game's MPQ archives: which files an archive holds, and
//! their bytes, the way the 3.3.5a client reads them.
//!
//! Only what the client's own archives use is supported: headers of format
//! 0 and 1 (anything else is an error naming the archive), 32-bit offsets,
//! files stored, or split in sectors compressed with zlib or bzip2 (a
//! sector whose packed length is its length is raw), single-unit files,
//! and encryption, with or without a key fixed by the file's position.
//! Imploded, patch and sparse files, which no archive of the game has, are
//! errors when read.
//!
//! The reader streams. Opening an archive reads its two tables once (at
//! most [`MAX_ENTRIES`] entries of 16 bytes each); a file is read sector by
//! sector from the file, never whole archives, and [`Archive::read_range`]
//! reads only the sectors a byte range spans. Nothing in an archive can make
//! it panic: lengths are checked against the file's, table indexes are
//! bounded, and what doesn't add up (a compression it doesn't know, sizes
//! that disagree) is an [`Error::Mpq`] naming the archive and the file.
//!
//! The archive keys a file by two 32-bit hashes of its name (see [`Key`]),
//! not by the name, which is why an archive without a `(listfile)` still
//! lists its files: as keys. Names are hashed as bytes, not as text, after
//! the game's own normalisation: ASCII upper case, `/` as `\`. Files are
//! found by name or by key ([`Target`]); the one thing a key can't do is
//! read an encrypted file, whose encryption key comes from its name.
//!
//! The archive's locale and platform fields are ignored, as the game
//! ignores them: the first entry that matches wins.
//!
//! [`writer`] writes small archives for tests.

use std::fmt;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::Error;

#[cfg(any(test, feature = "testing"))]
pub mod writer;

/// The most entries a hash table or block table may have, 16 bytes each. The
/// game's biggest has 524,288.
pub const MAX_ENTRIES: u32 = 1 << 20;

/// The largest single-unit file the reader unpacks, in bytes.
const MAX_SINGLE_UNIT: u32 = 1 << 28;

/// How much of an archive is read at once.
const BUFFER: usize = 16 * 1024;

/// The block index of a hash table entry nothing was ever put in: the end of
/// every probe.
const EMPTY: u32 = 0xFFFF_FFFF;

/// The block index of a hash table entry whose file was removed: probes go
/// past it.
const DELETED: u32 = 0xFFFF_FFFE;

/// The bits of a block's flags.
pub mod flag {
    /// The file is imploded (PKWare), which no archive of the game has.
    pub const IMPLODE: u32 = 0x0000_0100;
    /// The sectors are compressed, each starting with a byte for how.
    pub const COMPRESS: u32 = 0x0000_0200;
    /// The file is encrypted, with a key from its name.
    pub const ENCRYPTED: u32 = 0x0001_0000;
    /// The key of the encrypted file also depends on where it lies and its
    /// size.
    pub const FIX_KEY: u32 = 0x0002_0000;
    /// A patch to a file of an older archive, which the game doesn't have.
    pub const PATCH_FILE: u32 = 0x0010_0000;
    /// The file is one piece instead of sectors.
    pub const SINGLE_UNIT: u32 = 0x0100_0000;
    /// The file is there to hide the copy of an archive below: it has no
    /// bytes.
    pub const DELETE_MARKER: u32 = 0x0200_0000;
    /// The sectors' offset table has one more entry, for their checksums,
    /// which the reader doesn't check.
    pub const SECTOR_CRC: u32 = 0x0400_0000;
    /// The file has holes, which the game doesn't have.
    pub const SPARSE: u32 = 0x2000_0000;
    /// The block holds a file.
    pub const EXISTS: u32 = 0x8000_0000;
}

/// What the reader can't read.
const UNSUPPORTED: u32 = flag::IMPLODE | flag::PATCH_FILE | flag::SPARSE;

/// A file's two name hashes, the way the archive's hash table keys it.
///
/// In JSON, as the hash table's own pair of numbers written as sixteen
/// lowercase hexadecimal digits.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Key(pub u32, pub u32);

impl Key {
    /// The key of `name`, hashed as bytes.
    pub fn of(name: impl AsRef<[u8]>) -> Self {
        let name = name.as_ref();

        Self(hash(name, HashKind::NameA), hash(name, HashKind::NameB))
    }

    /// The key written as sixteen hexadecimal digits, in either case.
    pub fn parse(text: &str) -> Option<Self> {
        if text.len() != 16 || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }

        Some(Self(
            u32::from_str_radix(text.get(..8)?, 16).ok()?,
            u32::from_str_radix(text.get(8..)?, 16).ok()?,
        ))
    }
}

impl fmt::Display for Key {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:08x}{:08x}", self.0, self.1)
    }
}

impl Serialize for Key {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;

        Self::parse(&text).ok_or_else(|| {
            serde::de::Error::custom(format!("{text:?} is not sixteen hexadecimal digits"))
        })
    }
}

/// The key of `name`: its two name hashes, after normalising it.
pub fn key(name: &str) -> Key {
    Key::of(name)
}

/// The four hashes the game computes of a name.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HashKind {
    /// Where the name starts in the hash table.
    Index = 0,
    /// The first of the two hashes that identify the name.
    NameA = 1,
    /// The second.
    NameB = 2,
    /// The key an encrypted file is encrypted with (of its base name).
    FileKey = 3,
}

/// The game's table of pseudo-random numbers, which every hash and every
/// decryption draws from.
static CRYPT: [u32; 0x500] = crypt_table();

const fn crypt_table() -> [u32; 0x500] {
    let mut table = [0; 0x500];
    let mut seed: u32 = 0x0010_0001;
    let mut start = 0;

    while start < 0x100 {
        let mut index = start;
        let mut round = 0;

        while round < 5 {
            seed = (seed * 125 + 3) % 0x2A_AAAB;
            let high = (seed & 0xFFFF) << 16;
            seed = (seed * 125 + 3) % 0x2A_AAAB;
            let low = seed & 0xFFFF;
            table[index] = high | low;
            index += 0x100;
            round += 1;
        }

        start += 1;
    }

    table
}

/// A name's byte as the game hashes it: ASCII upper case, `/` as `\`.
fn normalize_byte(byte: u8) -> u8 {
    match byte {
        b'/' => b'\\',
        other => other.to_ascii_uppercase(),
    }
}

/// `name` the way the game normalises it before hashing: ASCII upper case,
/// `/` as `\`. Bytes outside ASCII are left as they are.
pub fn normalize(name: impl AsRef<[u8]>) -> Vec<u8> {
    name.as_ref().iter().copied().map(normalize_byte).collect()
}

/// The `kind` hash of `name`, normalised on the way.
pub fn hash(name: impl AsRef<[u8]>, kind: HashKind) -> u32 {
    let table = (kind as usize) << 8;
    let (mut first, mut second) = (0x7FED_7FEDu32, 0xEEEE_EEEEu32);

    for &byte in name.as_ref() {
        let byte = normalize_byte(byte);
        let value = CRYPT[table + usize::from(byte)];
        first = value ^ first.wrapping_add(second);
        second = u32::from(byte)
            .wrapping_add(first)
            .wrapping_add(second)
            .wrapping_add(second << 5)
            .wrapping_add(3);
    }

    first
}

/// Decrypts `data` in place with `key`, a word at a time. A last few bytes
/// short of a word are left as they are, as the game leaves them.
fn decrypt(data: &mut [u8], key: u32) {
    let (mut first, mut second) = (key, 0xEEEE_EEEEu32);

    for chunk in data.as_chunks_mut::<4>().0 {
        second = second.wrapping_add(CRYPT[0x400 + (first & 0xFF) as usize]);
        let word = u32::from_le_bytes(*chunk) ^ first.wrapping_add(second);
        first = ((!first) << 21).wrapping_add(0x1111_1111) | (first >> 11);
        second = word
            .wrapping_add(second)
            .wrapping_add(second << 5)
            .wrapping_add(3);
        *chunk = word.to_le_bytes();
    }
}

/// The little-endian number at `at`, or 0 beyond the end.
fn le32(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at.saturating_add(4))
        .and_then(|word| <[u8; 4]>::try_from(word).ok())
        .map_or(0, u32::from_le_bytes)
}

/// The little-endian number at `at`, or 0 beyond the end.
fn le16(bytes: &[u8], at: usize) -> u16 {
    bytes
        .get(at..at.saturating_add(2))
        .and_then(|word| <[u8; 2]>::try_from(word).ok())
        .map_or(0, u16::from_le_bytes)
}

/// A file's name without its folders.
fn base_name(name: &[u8]) -> &[u8] {
    name.rsplit(|byte| matches!(byte, b'\\' | b'/'))
        .next()
        .unwrap_or(name)
}

/// What to look an archive's file up by: its name, or only its [`Key`].
#[derive(Clone, Copy, Debug)]
pub enum Target<'a> {
    /// The two hashes alone, as [`Archive::entries`] lists them.
    Key(Key),
    /// The name, as bytes, in any case and with either slash.
    Name(&'a [u8]),
}

impl From<Key> for Target<'_> {
    fn from(key: Key) -> Self {
        Self::Key(key)
    }
}

impl<'a> From<&'a str> for Target<'a> {
    fn from(name: &'a str) -> Self {
        Self::Name(name.as_bytes())
    }
}

impl<'a> From<&'a String> for Target<'a> {
    fn from(name: &'a String) -> Self {
        Self::Name(name.as_bytes())
    }
}

impl<'a> From<&'a [u8]> for Target<'a> {
    fn from(name: &'a [u8]) -> Self {
        Self::Name(name)
    }
}

/// A file of an archive, as its tables describe it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Entry {
    /// The file's two name hashes.
    pub key: Key,
    /// Its row in the block table.
    pub block: u32,
    /// Where its bytes start, from the start of the archive.
    pub offset: u32,
    /// How many bytes it takes in the archive.
    pub packed: u32,
    /// How many bytes it has once unpacked.
    pub size: u32,
    /// Its [`flag`]s.
    pub flags: u32,
}

impl Entry {
    /// Whether the file only hides the copy of the same name in an archive
    /// below. It has no bytes.
    pub fn is_delete_marker(&self) -> bool {
        self.flags & flag::DELETE_MARKER != 0
    }

    /// Whether the file is encrypted.
    pub fn is_encrypted(&self) -> bool {
        self.flags & flag::ENCRYPTED != 0
    }

    /// Whether its sectors are compressed.
    pub fn is_compressed(&self) -> bool {
        self.flags & flag::COMPRESS != 0
    }

    /// Whether it is one piece instead of sectors.
    pub fn is_single_unit(&self) -> bool {
        self.flags & flag::SINGLE_UNIT != 0
    }
}

/// A row of the hash table, without the locale and platform the game
/// ignores.
#[derive(Clone, Copy)]
struct Slot {
    key: Key,
    block: u32,
}

/// A row of the block table.
#[derive(Clone, Copy)]
struct Block {
    offset: u32,
    packed: u32,
    size: u32,
    flags: u32,
}

/// An MPQ archive, opened for reading.
///
/// Reading takes `&mut self`: files are read from where the archive's reader
/// is moved to.
pub struct Archive<R = File> {
    reader: BufReader<R>,
    /// Where the reader is, when known, so reading on from there moves
    /// nothing.
    at: Option<u64>,
    /// The archive's path, for messages.
    path: PathBuf,
    /// The archive's length, which nothing is read beyond.
    len: u64,
    /// The size of a sector.
    sector: u64,
    slots: Vec<Slot>,
    blocks: Vec<Block>,
    /// The blocks of the hash table's entries by key, once a file has been
    /// looked up by its key alone.
    by_key: Option<Vec<(Key, u32)>>,
}

impl<R> fmt::Debug for Archive<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Archive")
            .field("path", &self.path)
            .field("len", &self.len)
            .field("hash_table", &self.slots.len())
            .field("block_table", &self.blocks.len())
            .finish_non_exhaustive()
    }
}

/// An error of the archive at `path`.
fn broken(path: &Path, reason: impl fmt::Display) -> Error {
    Error::Mpq {
        archive: path.to_path_buf(),
        reason: reason.to_string(),
    }
}

/// An error of reading the archive at `path`: a truncated file is one the
/// archive is broken in, not the disk.
fn io_error(path: &Path, source: io::Error) -> Error {
    if source.kind() == io::ErrorKind::UnexpectedEof {
        broken(path, "is cut short")
    } else {
        Error::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

impl Archive<File> {
    /// Opens the archive at `path`, reading its tables.
    pub fn open(path: &Path) -> Result<Self, Error> {
        let file = File::open(path).map_err(|source| io_error(path, source))?;

        Self::from_reader(file, path)
    }
}

impl<R: Read + Seek> Archive<R> {
    /// Opens an archive from `reader` (its length is where it ends), reading
    /// its tables. `path` only names it in messages.
    pub fn from_reader(reader: R, path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        let mut reader = BufReader::with_capacity(BUFFER, reader);
        let len = reader
            .seek(SeekFrom::End(0))
            .map_err(|source| io_error(&path, source))?;

        if len < 32 {
            return Err(broken(&path, "isn't an MPQ archive"));
        }

        reader
            .seek(SeekFrom::Start(0))
            .map_err(|source| io_error(&path, source))?;
        let mut head = [0; 44];
        let (first, rest) = head.split_at_mut(32);
        reader
            .read_exact(first)
            .map_err(|source| io_error(&path, source))?;

        match first.get(..4) {
            Some(b"MPQ\x1A") => {}
            Some(b"MPQ\x1B") => {
                return Err(broken(
                    &path,
                    "has a user data header, which isn't supported",
                ));
            }
            _ => return Err(broken(&path, "isn't an MPQ archive")),
        }

        let version = le16(first, 12);

        if version > 1 {
            return Err(broken(
                &path,
                format!("has format version {version}; only 0 and 1 are supported"),
            ));
        }

        let header_size = le32(first, 4);

        if header_size < 32 + 12 * u32::from(version) {
            return Err(broken(&path, "has a header too short for its version"));
        }

        if version == 1 {
            reader
                .read_exact(rest)
                .map_err(|source| io_error(&path, source))?;

            if le32(rest, 0) != 0 || le32(rest, 4) != 0 || le32(rest, 8) != 0 {
                return Err(broken(
                    &path,
                    "needs offsets beyond 4 GB, which aren't supported",
                ));
            }
        }

        let shift = le16(first, 14);

        if shift > 12 {
            return Err(broken(
                &path,
                format!("has sectors of 512 << {shift} bytes"),
            ));
        }

        let mut archive = Self {
            reader,
            at: Some(if version == 1 { 44 } else { 32 }),
            path,
            len,
            sector: 512 << shift,
            slots: Vec::new(),
            blocks: Vec::new(),
            by_key: None,
        };
        let hash_count = le32(first, 24);
        let block_count = le32(first, 28);

        if hash_count != 0 && !hash_count.is_power_of_two() {
            return Err(archive.broken(format!(
                "has a hash table of {hash_count} entries, not a power of two"
            )));
        }

        let hashes = archive.table(
            "hash",
            u64::from(le32(first, 16)),
            hash_count,
            "(hash table)",
        )?;
        let blocks = archive.table(
            "block",
            u64::from(le32(first, 20)),
            block_count,
            "(block table)",
        )?;

        archive.slots = hashes
            .as_chunks::<16>()
            .0
            .iter()
            .map(|row| Slot {
                key: Key(le32(row, 0), le32(row, 4)),
                block: le32(row, 12),
            })
            .collect();
        archive.blocks = blocks
            .as_chunks::<16>()
            .0
            .iter()
            .map(|row| Block {
                offset: le32(row, 0),
                packed: le32(row, 4),
                size: le32(row, 8),
                flags: le32(row, 12),
            })
            .collect();

        Ok(archive)
    }

    /// The archive's path, as it was opened.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// An error of this archive.
    fn broken(&self, reason: impl fmt::Display) -> Error {
        broken(&self.path, reason)
    }

    /// An error of one of this archive's files.
    fn bad(&self, what: &str, reason: impl fmt::Display) -> Error {
        broken(&self.path, format!("{what}: {reason}"))
    }

    /// One of the two tables, decrypted: `count` rows of 16 bytes at `at`.
    fn table(&mut self, name: &str, at: u64, count: u32, key_of: &str) -> Result<Vec<u8>, Error> {
        if count > MAX_ENTRIES {
            return Err(self.broken(format!("has a {name} table of {count} entries, too many")));
        }

        let bytes = u64::from(count) * 16;

        if at.saturating_add(bytes) > self.len {
            return Err(self.broken(format!("has a {name} table beyond the end of the file")));
        }

        let mut table = self.read_at(at, bytes)?;
        decrypt(&mut table, hash(key_of, HashKind::FileKey));

        Ok(table)
    }

    /// `len` bytes at `at`.
    fn read_at(&mut self, at: u64, len: u64) -> Result<Vec<u8>, Error> {
        if at.checked_add(len).is_none_or(|end| end > self.len) {
            return Err(self.broken("is cut short"));
        }

        let mut bytes = vec![0; usize::try_from(len).map_err(|_| self.broken("is cut short"))?];

        let moved = match self.at {
            Some(here) if here == at => Ok(()),
            Some(here) => match (i64::try_from(at), i64::try_from(here)) {
                (Ok(at), Ok(here)) => self.reader.seek_relative(at - here),
                _ => self.reader.seek(SeekFrom::Start(at)).map(drop),
            },
            None => self.reader.seek(SeekFrom::Start(at)).map(drop),
        };
        let read = moved.and_then(|()| self.reader.read_exact(&mut bytes));

        match read {
            Ok(()) => {
                self.at = Some(at + len);
                Ok(bytes)
            }
            Err(source) => {
                self.at = None;
                Err(io_error(&self.path, source))
            }
        }
    }

    /// Every file the hash table lists, in its order, delete markers
    /// included. The ones whose block is missing from the block table or
    /// isn't a file are left out. A file listed under two locales appears
    /// twice.
    pub fn entries(&self) -> impl Iterator<Item = Entry> + '_ {
        self.slots
            .iter()
            .filter_map(|slot| self.entry_of(slot.key, slot.block))
    }

    /// The entry of `key` in `block`, if that block holds a file.
    fn entry_of(&self, key: Key, block: u32) -> Option<Entry> {
        if block >= DELETED {
            return None;
        }

        let row = self.blocks.get(usize::try_from(block).ok()?)?;

        (row.flags & flag::EXISTS != 0).then_some(Entry {
            key,
            block,
            offset: row.offset,
            packed: row.packed,
            size: row.size,
            flags: row.flags,
        })
    }

    /// The file `file` names, if the archive has it: a delete marker
    /// included. By name, the hash table is probed from where the name
    /// hashes to, past the entries of removed files, around the end of the
    /// table. By key, the entries are searched for it.
    pub fn entry<'a>(&mut self, file: impl Into<Target<'a>>) -> Option<Entry> {
        match file.into() {
            Target::Name(name) => self.probe(name),
            Target::Key(key) => self.search(key),
        }
    }

    fn probe(&self, name: &[u8]) -> Option<Entry> {
        let count = self.slots.len();
        let key = Key::of(name);
        let mask = count.checked_sub(1)?;
        let start = usize::try_from(hash(name, HashKind::Index)).ok()? & mask;

        (0..count)
            .map(|step| self.slots.get((start + step) & mask))
            .take_while(|slot| slot.is_some_and(|slot| slot.block != EMPTY))
            .flatten()
            .find(|slot| slot.block != DELETED && slot.key == key)
            .and_then(|slot| self.entry_of(key, slot.block))
    }

    fn search(&mut self, key: Key) -> Option<Entry> {
        let index = self.by_key.get_or_insert_with(|| {
            let mut index: Vec<_> = self
                .slots
                .iter()
                .filter(|slot| slot.block < DELETED)
                .map(|slot| (slot.key, slot.block))
                .collect();
            index.sort_by_key(|(key, _)| *key);

            index
        });
        let first = index.partition_point(|(other, _)| *other < key);
        let (found, block) = *index.get(first)?;

        (found == key).then(|| self.entry_of(key, block)).flatten()
    }

    /// Whether the archive has the file `file` names.
    pub fn contains<'a>(&mut self, file: impl Into<Target<'a>>) -> bool {
        self.entry(file).is_some()
    }

    /// The bytes of the file `file` names, or `None` if the archive doesn't
    /// have it, or only has a delete marker (see [`Archive::entry`]).
    pub fn read<'a>(&mut self, file: impl Into<Target<'a>>) -> Result<Option<Vec<u8>>, Error> {
        self.read_range(file, 0..u64::MAX)
    }

    /// Bytes `range` of the file `file` names, or `None` as [`Archive::read`]
    /// says. Only the sectors the range spans are read. A range that goes
    /// beyond the file's end stops there.
    pub fn read_range<'a>(
        &mut self,
        file: impl Into<Target<'a>>,
        range: Range<u64>,
    ) -> Result<Option<Vec<u8>>, Error> {
        let file = file.into();
        let Some(entry) = self.entry(file) else {
            return Ok(None);
        };

        if entry.is_delete_marker() {
            return Ok(None);
        }

        let (what, name) = match file {
            Target::Name(name) => (String::from_utf8_lossy(name).into_owned(), Some(name)),
            Target::Key(key) => (format!("file {key}"), None),
        };

        self.unpack(&entry, name, &what, range).map(Some)
    }

    /// The names the archive's `(listfile)` gives, or `None` if it has none.
    /// A line that isn't valid UTF-8 is left out: its file is still among
    /// the [`entries`](Archive::entries), by its key.
    pub fn listfile(&mut self) -> Result<Option<Vec<String>>, Error> {
        let Some(bytes) = self.read("(listfile)")? else {
            return Ok(None);
        };

        Ok(Some(
            bytes
                .split(|byte| matches!(byte, b'\r' | b'\n' | b';'))
                .map(<[u8]>::trim_ascii)
                .filter_map(|line| str::from_utf8(line).ok())
                .filter(|line| !line.is_empty())
                .map(String::from)
                .collect(),
        ))
    }

    /// Bytes `range` of the file of `entry`, called `what` in messages.
    fn unpack(
        &mut self,
        entry: &Entry,
        name: Option<&[u8]>,
        what: &str,
        range: Range<u64>,
    ) -> Result<Vec<u8>, Error> {
        let size = u64::from(entry.size);
        let start = range.start.min(size);
        let end = range.end.min(size);

        if start >= end {
            return Ok(Vec::new());
        }

        if entry.flags & UNSUPPORTED != 0 {
            return Err(self.bad(what, "is stored in a way the reader doesn't support"));
        }

        let offset = u64::from(entry.offset);
        let packed = u64::from(entry.packed);

        if offset + packed > self.len {
            return Err(self.bad(what, "lies beyond the end of the archive"));
        }

        let compressed = entry.is_compressed();

        if !compressed && packed < size {
            return Err(self.bad(what, "is shorter than its size says"));
        }

        let key = if entry.is_encrypted() {
            let Some(name) = name else {
                return Err(self.bad(what, "is encrypted: read it by its name"));
            };
            let key = hash(base_name(name), HashKind::FileKey);

            Some(if entry.flags & flag::FIX_KEY != 0 {
                key.wrapping_add(entry.offset) ^ entry.size
            } else {
                key
            })
        } else {
            None
        };

        if entry.is_single_unit() {
            return self.single_unit(entry, key, what, start..end);
        }

        let sector = self.sector;
        let first = start / sector;
        let last = (end - 1) / sector;
        // The offsets of the sectors from the file's start, one more than
        // there are sectors: the table is decrypted from its first entry.
        let mut table_from = first;
        let mut offsets = Vec::new();

        if compressed {
            let count = size.div_ceil(sector);

            if (count + 1) * 4 > packed {
                return Err(self.bad(what, "has fewer bytes than its sectors need"));
            }

            if key.is_some() {
                table_from = 0;
            }

            let mut table = self.read_at(offset + table_from * 4, (last + 2 - table_from) * 4)?;

            if let Some(key) = key {
                decrypt(&mut table, key.wrapping_sub(1));
            }

            offsets = table
                .as_chunks::<4>()
                .0
                .iter()
                .map(|word| u64::from(u32::from_le_bytes(*word)))
                .collect();
        }

        let mut out = Vec::with_capacity(usize::try_from(end - start).unwrap_or(0).min(1 << 20));

        for index in first..=last {
            let from = index * sector;
            let want = (size - from).min(sector);
            let (a, b) = if compressed {
                let at = usize::try_from(index - table_from).unwrap_or(usize::MAX);

                match (offsets.get(at), offsets.get(at.saturating_add(1))) {
                    (Some(&a), Some(&b)) => (a, b),
                    _ => return Err(self.bad(what, "has no offset for a sector")),
                }
            } else {
                (from, from + want)
            };

            if a > b || b > packed {
                return Err(self.bad(what, "has sectors that don't add up"));
            }

            let mut data = self.read_at(offset + a, b - a)?;

            if let Some(key) = key {
                decrypt(&mut data, key.wrapping_add(index as u32));
            }

            let sector_bytes = if b - a == want {
                data
            } else if b - a > want || !compressed {
                return Err(self.bad(what, "has sectors that don't add up"));
            } else {
                inflate(&data, usize::try_from(want).unwrap_or(0))
                    .map_err(|reason| self.bad(what, reason))?
            };
            let lo = usize::try_from(start.saturating_sub(from)).unwrap_or(0);
            let hi = usize::try_from((end - from).min(want)).unwrap_or(0);

            out.extend_from_slice(
                sector_bytes
                    .get(lo..hi)
                    .ok_or_else(|| self.bad(what, "has sectors that don't add up"))?,
            );
        }

        Ok(out)
    }

    /// Bytes `range` of a file in one piece.
    fn single_unit(
        &mut self,
        entry: &Entry,
        key: Option<u32>,
        what: &str,
        range: Range<u64>,
    ) -> Result<Vec<u8>, Error> {
        if entry.size > MAX_SINGLE_UNIT {
            return Err(self.bad(what, "is too big for a single piece"));
        }

        let size = usize::try_from(entry.size).unwrap_or(0);
        let mut data = self.read_at(u64::from(entry.offset), u64::from(entry.packed))?;

        if let Some(key) = key {
            decrypt(&mut data, key);
        }

        let whole = if entry.is_compressed() && entry.packed < entry.size {
            inflate(&data, size).map_err(|reason| self.bad(what, reason))?
        } else {
            data
        };
        let start = usize::try_from(range.start).unwrap_or(0);
        let end = usize::try_from(range.end).unwrap_or(0);

        whole
            .get(start..end)
            .map(<[u8]>::to_vec)
            .ok_or_else(|| self.bad(what, "is shorter than its size says"))
    }
}

/// `data` unpacked to `want` bytes: a byte for how it is compressed (`0x02`
/// zlib, `0x10` bzip2), then the compressed bytes.
fn inflate(data: &[u8], want: usize) -> Result<Vec<u8>, String> {
    let Some((&method, packed)) = data.split_first() else {
        return Err("has an empty compressed sector".into());
    };
    // One byte more than wanted, to tell a stream that goes on.
    let limit = want as u64 + 1;
    let mut out = Vec::with_capacity(want.min(1 << 20));
    let read = match method {
        0x02 => flate2::read::ZlibDecoder::new(packed)
            .take(limit)
            .read_to_end(&mut out),
        0x10 => bzip2::read::BzDecoder::new(packed)
            .take(limit)
            .read_to_end(&mut out),
        other => {
            return Err(format!(
                "is compressed with {other:#04x}, which isn't supported"
            ));
        }
    };

    read.map_err(|error| format!("has corrupt compressed data: {error}"))?;

    if out.len() == want {
        Ok(out)
    } else {
        Err(format!(
            "has a sector of {} bytes where {want} are expected",
            out.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::BTreeSet;
    use std::io::Cursor;
    use std::rc::Rc;

    use super::writer::{Encryption, Layout, Options, Writer};
    use super::*;

    /// Bytes that don't compress.
    fn noise(len: usize, seed: u64) -> Vec<u8> {
        let mut state = seed | 1;

        (0..len)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                (state >> 24) as u8
            })
            .collect()
    }

    /// Bytes that compress well, and differ from sector to sector.
    fn text(len: usize) -> Vec<u8> {
        (0..len)
            .flat_map(|line| format!("line {line} of a file that repeats itself\n").into_bytes())
            .take(len)
            .collect()
    }

    fn open(bytes: Vec<u8>) -> Archive<Cursor<Vec<u8>>> {
        Archive::from_reader(Cursor::new(bytes), "test.mpq").unwrap()
    }

    fn with(options: Options) -> Writer {
        Writer::new().file_with("Dir\\File.dat", &text(10_000), options)
    }

    #[test]
    fn names_hash_as_the_game_hashes_them() {
        let name = b"(listfile)";

        assert_eq!(hash(name, HashKind::Index), 0x5F3D_E859);
        assert_eq!(hash(name, HashKind::NameA), 0xFD65_7910);
        assert_eq!(hash(name, HashKind::NameB), 0x4E9B_98A7);
        assert_eq!(hash(name, HashKind::FileKey), 0x2D2F_0A94);
        assert_eq!(key("(listfile)"), Key(0xFD65_7910, 0x4E9B_98A7));

        let icon = key("Interface\\Icons\\INV_Misc_QuestionMark.blp");

        assert_eq!(icon, Key(0x80C4_C06C, 0x1E69_3E27));
        assert_eq!(key("interface/icons/inv_misc_questionmark.blp"), icon);
        assert_eq!(
            hash("interface/icons/inv_misc_questionmark.blp", HashKind::Index),
            0xE57C_0370
        );
        assert_eq!(
            hash("INV_Misc_QuestionMark.blp", HashKind::FileKey),
            0xE293_092B
        );
        // Bytes beyond ASCII are hashed as they are.
        assert_eq!(normalize(b"a/b\xE9"), b"A\\B\xE9");
        assert_ne!(Key::of(b"\xE9"), Key::of(b"\xC9"));
    }

    #[test]
    fn keys_are_written_as_sixteen_hex_digits() {
        let icon = Key(0x80C4_C06C, 0x1E69_3E27);
        let json = serde_json::to_string(&icon).unwrap();

        assert_eq!(json, "\"80c4c06c1e693e27\"");
        assert_eq!(serde_json::from_str::<Key>(&json).unwrap(), icon);
        assert_eq!(
            serde_json::from_str::<Key>("\"0000000100000002\"").unwrap(),
            Key(1, 2)
        );
        assert_eq!(Key(1, 2).to_string(), "0000000100000002");

        for bad in [
            "\"\"",
            "\"80c4c06c\"",
            "\"+0c4c06c1e693e27\"",
            "\"zzzzzzzzzzzzzzzz\"",
        ] {
            assert!(serde_json::from_str::<Key>(bad).is_err(), "{bad}");
        }

        let map = serde_json::to_string(&std::collections::BTreeMap::from([(icon, 1)])).unwrap();

        assert_eq!(map, "{\"80c4c06c1e693e27\":1}");
        assert_eq!(
            serde_json::from_str::<std::collections::BTreeMap<Key, u32>>(&map).unwrap()[&icon],
            1
        );
    }

    #[test]
    fn a_written_archive_reads_back_file_by_file() {
        let many = text(60_000);
        let random = noise(9_000, 7);
        let stored = Options {
            layout: Layout::Stored,
            ..Options::default()
        };
        let single = Options {
            layout: Layout::Single,
            ..Options::default()
        };
        let single_zlib = Options {
            layout: Layout::SingleZlib,
            ..Options::default()
        };
        let bzip2 = Options {
            layout: Layout::Bzip2,
            ..Options::default()
        };
        let checksummed = Options {
            sector_crc: true,
            ..Options::default()
        };

        for version in [0, 1] {
            let bytes = Writer::new()
                .version(version)
                .file("zlib.txt", &many)
                .file("raw\\noise.bin", &random)
                .file_with("stored.bin", &random, stored)
                .file_with("single.bin", &random, single)
                .file_with("single.txt", &many, single_zlib)
                .file_with("bzip2.txt", &many, bzip2)
                .file_with("crc.txt", &many, checksummed)
                .file("short.txt", b"a few bytes")
                .file("empty.txt", b"")
                .build();
            let mut archive = open(bytes);
            let files: [(&str, &[u8]); 9] = [
                ("zlib.txt", &many),
                ("raw\\noise.bin", &random),
                ("stored.bin", &random),
                ("single.bin", &random),
                ("single.txt", &many),
                ("bzip2.txt", &many),
                ("crc.txt", &many),
                ("short.txt", b"a few bytes"),
                ("empty.txt", b""),
            ];

            for (name, contents) in files {
                assert_eq!(
                    archive.read(name).unwrap().as_deref(),
                    Some(contents),
                    "{name}"
                );
                assert_eq!(
                    archive.read(key(name)).unwrap().as_deref(),
                    Some(contents),
                    "{name} by its key"
                );
            }

            // Case and slashes don't matter; absent files are none.
            assert_eq!(archive.read("RAW/NOISE.BIN").unwrap().unwrap(), random);
            assert_eq!(archive.read("nothing.txt").unwrap(), None);
            assert_eq!(archive.read(key("nothing.txt")).unwrap(), None);

            let by_name = |archive: &mut Archive<_>, name| archive.entry(name).unwrap();
            let zlib = by_name(&mut archive, "zlib.txt");

            assert!(zlib.is_compressed() && !zlib.is_single_unit() && !zlib.is_encrypted());
            assert!(zlib.packed < zlib.size);
            assert_eq!(zlib.size as usize, many.len());
            // Sectors that can't shrink are raw: more bytes than the file
            // has, for the offset table.
            let noise = by_name(&mut archive, "raw\\noise.bin");

            assert!(noise.is_compressed() && noise.packed > noise.size);
            assert!(by_name(&mut archive, "single.txt").is_single_unit());
            assert!(!by_name(&mut archive, "stored.bin").is_compressed());
            assert_eq!(by_name(&mut archive, "stored.bin").packed, 9_000);
            assert_eq!(archive.entries().count(), 9);
        }
    }

    #[test]
    fn encrypted_files_decrypt_with_and_without_a_fixed_key() {
        let contents = text(10_000);

        for encryption in [Encryption::Key, Encryption::FixedKey] {
            for layout in [
                Layout::Zlib,
                Layout::Stored,
                Layout::Single,
                Layout::SingleZlib,
                Layout::Bzip2,
            ] {
                for sector_crc in [false, true] {
                    let options = Options {
                        layout,
                        encryption,
                        sector_crc,
                    };
                    // Put the file after others, so its offset isn't small.
                    let bytes = Writer::new()
                        .file("before.txt", &text(5_000))
                        .file_with("Data\\Dir\\File.dat", &contents, options)
                        .with_listfile(Options {
                            encryption,
                            ..Options::default()
                        })
                        .build();
                    let mut archive = open(bytes);
                    let entry = archive.entry("data/dir/file.dat").unwrap();

                    assert!(entry.is_encrypted());
                    assert_eq!(
                        entry.flags & flag::FIX_KEY != 0,
                        encryption == Encryption::FixedKey
                    );
                    assert_eq!(
                        archive.read("Data\\Dir\\File.dat").unwrap().unwrap(),
                        contents,
                        "{encryption:?} {layout:?} {sector_crc}"
                    );
                    assert_eq!(
                        archive
                            .read_range("Data\\Dir\\File.dat", 4_000..9_000)
                            .unwrap()
                            .unwrap(),
                        contents[4_000..9_000],
                        "{encryption:?} {layout:?} {sector_crc}, a range"
                    );
                    // The key is the base name's: the folders don't matter.
                    assert_eq!(archive.read("File.dat").unwrap(), None);
                    assert_eq!(
                        archive.listfile().unwrap().unwrap(),
                        ["before.txt", "Data\\Dir\\File.dat"]
                    );
                }
            }
        }

        // A key alone can't decrypt.
        let mut archive = open(
            with(Options {
                encryption: Encryption::Key,
                ..Options::default()
            })
            .build(),
        );
        let error = archive.read(key("Dir\\File.dat")).unwrap_err().to_string();

        assert!(
            error.contains("test.mpq") && error.contains("encrypted"),
            "{error}"
        );
    }

    #[test]
    fn deleted_entries_are_probed_past_and_a_delete_marker_is_told() {
        // A name that hashes to the last slot of a table of two, so that
        // its file, past the removed entry that is there, wraps around.
        let last = (0..)
            .map(|number| format!("wrap{number}.txt"))
            .find(|name| hash(name, HashKind::Index) & 1 == 1)
            .unwrap();
        let mut archive = open(
            Writer::new()
                .hash_size(2)
                .deleted_slot_at(&last)
                .file(&last, b"wrapped")
                .build(),
        );

        assert_eq!(archive.read(&last).unwrap().unwrap(), b"wrapped");
        assert_eq!(archive.entries().count(), 1);

        // Removed entries ahead of a file, a delete marker among them, and a
        // platform field the game ignores.
        let bytes = Writer::new()
            .platform(0xFF00)
            .hash_size(16)
            .deleted_slot_at("one.txt")
            .deleted_slot_at("one.txt")
            .deleted_slot_at("one.txt")
            .file("one.txt", b"first")
            .delete_marker("gone.txt")
            .file("two.txt", b"second")
            .build();
        let mut archive = open(bytes);

        assert_eq!(archive.read("one.txt").unwrap().unwrap(), b"first");
        assert_eq!(archive.read(key("two.txt")).unwrap().unwrap(), b"second");

        let marker = archive.entry("gone.txt").unwrap();

        assert!(marker.is_delete_marker());
        assert_eq!(marker.size, 0);
        assert_eq!(archive.entry(key("gone.txt")), Some(marker));
        assert_eq!(archive.read("gone.txt").unwrap(), None);
        assert!(archive.contains("gone.txt"));
        assert!(!archive.contains("absent.txt"));
        assert_eq!(archive.entries().filter(Entry::is_delete_marker).count(), 1);
        assert_eq!(archive.entries().count(), 3);
    }

    #[test]
    fn an_archive_without_a_listfile_still_lists_its_keys() {
        let names = ["Interface\\A.blp", "World\\B.m2", "DBFilesClient\\C.dbc"];
        let mut writer = Writer::new();

        for name in names {
            writer = writer.file(name, name.as_bytes());
        }

        let mut archive = open(writer.clone().build());

        assert_eq!(archive.listfile().unwrap(), None);
        assert_eq!(
            archive
                .entries()
                .map(|entry| entry.key)
                .collect::<BTreeSet<_>>(),
            names.iter().map(|name| key(name)).collect()
        );

        let mut archive = open(writer.with_listfile(Options::default()).build());

        assert_eq!(archive.listfile().unwrap().unwrap(), names);
        assert_eq!(archive.entries().count(), 4);
    }

    /// Counts the bytes read from the archive.
    struct Counting {
        inner: Cursor<Vec<u8>>,
        count: Rc<Cell<usize>>,
    }

    impl Read for Counting {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let read = self.inner.read(buffer)?;
            self.count.set(self.count.get() + read);

            Ok(read)
        }
    }

    impl Seek for Counting {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.inner.seek(to)
        }
    }

    #[test]
    fn a_range_reads_only_the_sectors_it_spans() {
        let sector = 4096;
        let contents = noise(100 * sector, 3);

        for encryption in [Encryption::None, Encryption::Key] {
            for layout in [Layout::Zlib, Layout::Stored] {
                let options = Options {
                    layout,
                    encryption,
                    ..Options::default()
                };
                let bytes = Writer::new()
                    .sector_shift(3)
                    .file_with("big.bin", &contents, options)
                    .build();
                let total = bytes.len();
                let count = Rc::new(Cell::new(0));
                let mut archive = Archive::from_reader(
                    Counting {
                        inner: Cursor::new(bytes),
                        count: Rc::clone(&count),
                    },
                    "counted.mpq",
                )
                .unwrap();
                count.set(0);

                let range = 50 * sector as u64 + 100..52 * sector as u64 + 10;
                let part = archive
                    .read_range("big.bin", range.clone())
                    .unwrap()
                    .unwrap();

                assert_eq!(part, contents[range.start as usize..range.end as usize]);

                // Three sectors, their offsets and the buffer's slack: a
                // fifth of the archive at most.
                let read = count.get();

                assert!(
                    read < total / 5,
                    "{encryption:?} {layout:?}: {read} of {total} bytes read"
                );

                count.set(0);
                assert_eq!(archive.read("big.bin").unwrap().unwrap(), contents);
                assert!(count.get() >= total * 9 / 10);
            }
        }
    }

    #[test]
    fn a_range_is_the_same_bytes_of_the_file_whatever_the_layout() {
        let contents = text(20_000);

        for layout in [
            Layout::Zlib,
            Layout::Bzip2,
            Layout::Stored,
            Layout::Single,
            Layout::SingleZlib,
        ] {
            let mut archive = open(
                Writer::new()
                    .sector_shift(1)
                    .file_with(
                        "f",
                        &contents,
                        Options {
                            layout,
                            ..Options::default()
                        },
                    )
                    .build(),
            );

            for range in [
                0..1,
                0..1024,
                1023..1025,
                5..19_999,
                19_999..20_000,
                0..20_000,
            ] {
                assert_eq!(
                    archive.read_range("f", range.clone()).unwrap().unwrap(),
                    contents[range.start as usize..range.end as usize],
                    "{layout:?} {range:?}"
                );
            }

            // Beyond the end stops there; nothing at or past it.
            assert_eq!(
                archive.read_range("f", 19_990..99_999).unwrap().unwrap(),
                contents[19_990..]
            );
            assert_eq!(
                archive.read_range("f", 20_000..20_005).unwrap().unwrap(),
                b""
            );
            assert_eq!(archive.read_range("f", 30..30).unwrap().unwrap(), b"");
            assert_eq!(archive.read_range("absent", 0..5).unwrap(), None);
        }
    }

    /// Opens `bytes` and reads everything it can, whatever that does.
    fn read_everything(bytes: &[u8], names: &[&str]) -> usize {
        let Ok(mut archive) = Archive::from_reader(Cursor::new(bytes), "garbage.mpq") else {
            return 1;
        };
        let mut errors = 0;

        errors += usize::from(archive.listfile().is_err());

        for name in names {
            errors += usize::from(archive.read(*name).is_err());
            errors += usize::from(archive.read_range(*name, 100..5_000).is_err());
        }

        for entry in archive.entries().collect::<Vec<_>>() {
            errors += usize::from(archive.read(entry.key).is_err());
            errors += usize::from(archive.read_range(entry.key, 1..3).is_err());
        }

        errors
    }

    #[test]
    fn garbage_truncation_and_wrong_sizes_are_errors_not_panics() {
        let contents = text(1_300);
        let names = ["a.txt", "b.bin", "c.dat", "d.dat", "e.txt", "f.txt"];
        let bytes = Writer::new()
            .sector_shift(0)
            .file("a.txt", &contents)
            .file("b.bin", &noise(900, 5))
            .file_with(
                "c.dat",
                &contents,
                Options {
                    encryption: Encryption::FixedKey,
                    ..Options::default()
                },
            )
            .file_with(
                "d.dat",
                &contents,
                Options {
                    layout: Layout::SingleZlib,
                    encryption: Encryption::Key,
                    ..Options::default()
                },
            )
            .file_with(
                "e.txt",
                &contents,
                Options {
                    layout: Layout::Bzip2,
                    sector_crc: true,
                    ..Options::default()
                },
            )
            .file_with(
                "f.txt",
                &contents,
                Options {
                    layout: Layout::Stored,
                    ..Options::default()
                },
            )
            .with_listfile(Options::default())
            .build();

        // The two encrypted files can't be read by their keys, twice.
        assert_eq!(read_everything(&bytes, &names), 4);

        // Every prefix: a cut archive is an error, or reads what it has.
        for length in 0..bytes.len() {
            read_everything(bytes.get(..length).unwrap(), &names);
        }

        // Every byte of the header and the tables flipped, and set to the
        // most it can be, and every third of the files'.
        let tables = le32(&bytes, 16) as usize;
        let mut failures = 0;

        for at in (0..bytes.len()).filter(|at| *at < 44 || *at >= tables || at % 3 == 0) {
            let mut garbage = bytes.clone();
            garbage[at] ^= 0xFF;
            failures += read_everything(&garbage, &names);
            garbage[at] = 0xFF;
            failures += read_everything(&garbage, &names);
        }

        assert!(failures > 100, "{failures}");

        // Whole fields of the header at their largest.
        for field in (4..32).step_by(4) {
            let mut garbage = bytes.clone();
            garbage[field..field + 4].copy_from_slice(&u32::MAX.to_le_bytes());
            read_everything(&garbage, &names);
        }
    }

    #[test]
    fn an_unsupported_archive_is_an_error_naming_it() {
        let bytes = Writer::new().file("a.txt", b"a").build();

        for (what, at, value, reason) in [
            ("version", 12, 2u8, "version 2"),
            ("magic", 0, b'X', "isn't an MPQ"),
            ("table size", 24, 3, "power of two"),
            ("sectors", 14, 13, "sectors"),
        ] {
            let mut garbage = bytes.clone();
            garbage[at] = value;
            let error = Archive::from_reader(Cursor::new(garbage), "Data/patch-X.mpq")
                .unwrap_err()
                .to_string();

            assert!(error.starts_with("Data/patch-X.mpq: "), "{what}: {error}");
            assert!(error.contains(reason), "{what}: {error}");
        }

        let error = Archive::from_reader(Cursor::new(vec![0; 10]), "tiny.mpq")
            .unwrap_err()
            .to_string();

        assert!(error.contains("tiny.mpq"), "{error}");

        // A header of 44 bytes with 64-bit offsets set.
        let mut big = Writer::new().version(1).file("a.txt", b"a").build();
        big[32] = 1;

        assert!(
            Archive::from_reader(Cursor::new(big), "big.mpq")
                .unwrap_err()
                .to_string()
                .contains("4 GB")
        );
    }

    #[test]
    fn a_compression_it_does_not_know_is_an_error_naming_the_file() {
        let contents = text(5_000);

        for layout in [Layout::SingleZlib, Layout::Zlib] {
            let mut bytes = Writer::new()
                .file_with(
                    "Dir\\Odd.dat",
                    &contents,
                    Options {
                        layout,
                        ..Options::default()
                    },
                )
                .build();
            let entry = open(bytes.clone()).entry("Dir\\Odd.dat").unwrap();
            // The first sector's method byte follows the offset table.
            let table = if layout == Layout::Zlib {
                (contents.len().div_ceil(4096) + 1) * 4
            } else {
                0
            };
            let at = entry.offset as usize + table;

            assert_eq!(bytes[at], 0x02);
            bytes[at] = 0x80;

            let error = open(bytes).read("Dir\\Odd.dat").unwrap_err().to_string();

            assert!(
                error.contains("test.mpq")
                    && error.contains("Dir\\Odd.dat")
                    && error.contains("0x80"),
                "{layout:?}: {error}"
            );
        }
    }

    /// `cargo test -p old_launcher_core --release mpq::tests::every_archive
    /// -- --ignored --nocapture`, with `OLD_LAUNCHER_TEST_CLIENT` a client
    /// folder, only ever read.
    #[test]
    #[ignore = "reads every archive of the client in $OLD_LAUNCHER_TEST_CLIENT"]
    fn every_archive_of_a_real_client_reads_its_listfile() {
        let client = PathBuf::from(
            std::env::var("OLD_LAUNCHER_TEST_CLIENT")
                .expect("OLD_LAUNCHER_TEST_CLIENT is a client folder"),
        );
        let mut folders = vec![client.join("Data")];

        for entry in std::fs::read_dir(&folders[0]).unwrap() {
            let path = entry.unwrap().path();

            if path.is_dir() {
                folders.push(path);
            }
        }

        let mut archives = Vec::new();

        for folder in folders {
            for entry in std::fs::read_dir(&folder).unwrap() {
                let path = entry.unwrap().path();

                if path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("mpq"))
                {
                    archives.push(path);
                }
            }
        }

        archives.sort();
        assert!(!archives.is_empty(), "no archive in {}", client.display());

        let started = std::time::Instant::now();
        let (mut with_names, mut without, mut read, mut missing, mut unnamed) = (0, 0, 0, 0, 0);

        for path in &archives {
            let opened = std::time::Instant::now();
            let mut archive = Archive::open(path).unwrap_or_else(|error| panic!("{error}"));
            let entries = archive.entries().count();

            match archive.listfile().unwrap_or_else(|error| panic!("{error}")) {
                None => {
                    without += 1;
                    println!("{}: {entries} files, no listfile", path.display());
                }
                Some(names) => {
                    with_names += 1;

                    // Names that don't hash to a file of the archive
                    // (listfiles can name files of other archives).
                    for name in [names.first(), names.last()].into_iter().flatten() {
                        match archive.read(name).unwrap_or_else(|error| panic!("{error}")) {
                            Some(_) => read += 1,
                            None => missing += 1,
                        }
                    }

                    unnamed += entries.saturating_sub(names.len());
                    println!(
                        "{}: {entries} files, {} names, {:?}",
                        path.display(),
                        names.len(),
                        opened.elapsed()
                    );
                }
            }
        }

        println!(
            "{} archives in {:?}: {with_names} with a listfile, {without} without; \
             {read} files read, {missing} named but absent, {unnamed} entries beyond the names",
            archives.len(),
            started.elapsed()
        );
    }
}
