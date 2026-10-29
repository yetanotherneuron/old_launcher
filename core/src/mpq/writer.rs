//! Writes small MPQ archives, for tests. **Test-only**: it is built for the
//! crate's own tests and for those of the crates using it (with the
//! `testing` feature), and the launcher itself never writes an archive.
//!
//! The archives it writes are what the game's look like at a small scale:
//! a header of format 0 (or 1), the files' sectors, the hash table and the
//! block table, both encrypted. A file is stored, in zlib or bzip2 sectors
//! (a sector that doesn't shrink stays raw), or in one piece; it may be
//! encrypted, with or without a key fixed by its position; it may have the
//! extra entry of the sectors' checksums (whose content is zeros, since
//! the reader doesn't check it). Delete markers, removed entries of the hash
//! table that probes must go past, and a `(listfile)` are optional.
//! [`Writer::build`] gives the bytes, which [`Archive::from_reader`] reads
//! from a `Cursor`; [`Writer::write`] puts them in a file.
//!
//! [`Archive::from_reader`]: super::Archive::from_reader

// Fixtures fail like tests do, by panicking, even when built as a feature.
#![allow(clippy::panic)]

use std::io::Write as _;
use std::path::Path;

use super::{CRYPT, HashKind, base_name, flag, hash};

/// How a file's bytes are laid out in the archive.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    /// In sectors, as they are.
    Stored,
    /// In sectors compressed with zlib: the game's usual.
    Zlib,
    /// In sectors compressed with bzip2.
    Bzip2,
    /// In one piece, as it is.
    Single,
    /// In one piece, compressed with zlib if that is smaller.
    SingleZlib,
}

/// Whether a file is encrypted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Encryption {
    /// Not.
    None,
    /// With the key of its name.
    Key,
    /// With the key of its name changed by where the file lies and its size.
    FixedKey,
}

/// How a file is written.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Options {
    /// Its layout.
    pub layout: Layout,
    /// Its encryption.
    pub encryption: Encryption,
    /// Whether the sectors' offset table has the extra entry for their
    /// checksums (and the checksums follow, as zeros). Only for sectors
    /// compressed.
    pub sector_crc: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            layout: Layout::Zlib,
            encryption: Encryption::None,
            sector_crc: false,
        }
    }
}

#[derive(Clone)]
enum Item {
    File {
        name: String,
        data: Vec<u8>,
        options: Options,
    },
    Marker(String),
}

impl Item {
    fn name(&self) -> &str {
        match self {
            Self::File { name, .. } | Self::Marker(name) => name,
        }
    }
}

/// An archive being put together.
#[derive(Clone)]
pub struct Writer {
    version: u16,
    sector_shift: u16,
    hash_size: Option<usize>,
    platform: u16,
    items: Vec<Item>,
    removed: Vec<String>,
    listfile: Option<Options>,
}

impl Default for Writer {
    fn default() -> Self {
        Self::new()
    }
}

impl Writer {
    /// An archive of format 0 with sectors of 4096 bytes, and no files.
    pub fn new() -> Self {
        Self {
            version: 0,
            sector_shift: 3,
            hash_size: None,
            platform: 0,
            items: Vec::new(),
            removed: Vec::new(),
            listfile: None,
        }
    }

    /// Writes a header of this format, 0 or 1.
    pub fn version(mut self, version: u16) -> Self {
        assert!(version <= 1, "the writer writes formats 0 and 1");
        self.version = version;

        self
    }

    /// Makes the sectors 512 << `shift` bytes.
    pub fn sector_shift(mut self, shift: u16) -> Self {
        self.sector_shift = shift;

        self
    }

    /// Makes the hash table this many entries, a power of two. By default
    /// it has twice as many as it needs, so names collide.
    pub fn hash_size(mut self, size: usize) -> Self {
        self.hash_size = Some(size);

        self
    }

    /// Writes this in the platform field of every hash table entry, which
    /// the game ignores.
    pub fn platform(mut self, platform: u16) -> Self {
        self.platform = platform;

        self
    }

    /// Adds a file of zlib sectors, not encrypted.
    pub fn file(self, name: &str, data: &[u8]) -> Self {
        self.file_with(name, data, Options::default())
    }

    /// Adds a file written as `options` say.
    pub fn file_with(mut self, name: &str, data: &[u8], options: Options) -> Self {
        self.items.push(Item::File {
            name: name.into(),
            data: data.to_vec(),
            options,
        });

        self
    }

    /// Adds a delete marker: a file of no bytes that hides the one of its
    /// name in the archives below.
    pub fn delete_marker(mut self, name: &str) -> Self {
        self.items.push(Item::Marker(name.into()));

        self
    }

    /// Puts an entry of a file removed where `name` hashes to, so that a
    /// file of that name, or one that collides with it, is past it. Once
    /// more puts another after it.
    pub fn deleted_slot_at(mut self, name: &str) -> Self {
        self.removed.push(name.into());

        self
    }

    /// Adds a `(listfile)` last, naming every file added, written as
    /// `options` say.
    pub fn with_listfile(mut self, options: Options) -> Self {
        self.listfile = Some(options);

        self
    }

    /// The archive's bytes.
    pub fn build(&self) -> Vec<u8> {
        let header = if self.version == 1 { 44 } else { 32 };
        let mut out = vec![0; header];
        let mut items = self.items.clone();

        if let Some(options) = self.listfile {
            let names: Vec<_> = items.iter().map(Item::name).collect();

            items.push(Item::File {
                name: "(listfile)".into(),
                data: names.join("\r\n").into_bytes(),
                options,
            });
        }

        let mut blocks = Vec::new();

        for item in &items {
            let offset = number(out.len());

            blocks.push(match item {
                Item::Marker(_) => [offset, 0, 0, flag::EXISTS | flag::DELETE_MARKER],
                Item::File {
                    name,
                    data,
                    options,
                } => {
                    let (packed, flags) = self.write_file(&mut out, name, data, *options);

                    [offset, packed, number(data.len()), flags]
                }
            });
        }

        let size = self.hash_size.unwrap_or_else(|| {
            ((items.len() + self.removed.len()) * 2)
                .next_power_of_two()
                .max(4)
        });

        assert!(
            size.is_power_of_two() && size >= items.len() + self.removed.len(),
            "a hash table of {size} entries"
        );

        let mut slots = vec![[u32::MAX; 4]; size];
        let place = |slots: &mut Vec<[u32; 4]>, name: &str, row: [u32; 4]| {
            let mut at = hash(name, HashKind::Index) as usize & (size - 1);

            while slots[at][3] != u32::MAX {
                at = (at + 1) & (size - 1);
            }

            slots[at] = row;
        };

        for name in &self.removed {
            place(
                &mut slots,
                name,
                [u32::MAX, u32::MAX, u32::MAX, u32::MAX - 1],
            );
        }

        for (block, item) in items.iter().enumerate() {
            let name = item.name();

            place(
                &mut slots,
                name,
                [
                    hash(name, HashKind::NameA),
                    hash(name, HashKind::NameB),
                    u32::from(self.platform) << 16,
                    number(block),
                ],
            );
        }

        let mut hashes = slots
            .concat()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        encrypt(&mut hashes, hash("(hash table)", HashKind::FileKey));

        let mut table = blocks
            .concat()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect::<Vec<_>>();
        encrypt(&mut table, hash("(block table)", HashKind::FileKey));

        let hash_at = number(out.len());
        out.extend(&hashes);
        let block_at = number(out.len());
        out.extend(&table);

        let fields = [
            u32::from_le_bytes(*b"MPQ\x1A"),
            number(header),
            number(out.len()),
            u32::from(self.version) | u32::from(self.sector_shift) << 16,
            hash_at,
            block_at,
            number(size),
            number(blocks.len()),
        ];

        for (word, bytes) in fields.iter().zip(out.as_chunks_mut::<4>().0) {
            *bytes = word.to_le_bytes();
        }

        out
    }

    /// Writes the archive to `path`.
    pub fn write(&self, path: &Path) {
        std::fs::write(path, self.build())
            .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }

    /// Appends a file's bytes to `out`, returning how many they take and the
    /// block's flags.
    fn write_file(
        &self,
        out: &mut Vec<u8>,
        name: &str,
        data: &[u8],
        options: Options,
    ) -> (u32, u32) {
        let sector = 512usize << self.sector_shift;
        let mut flags = flag::EXISTS;
        let start = out.len();
        let key = match options.encryption {
            Encryption::None => None,
            Encryption::Key | Encryption::FixedKey => {
                let key = hash(base_name(name.as_bytes()), HashKind::FileKey);
                flags |= flag::ENCRYPTED;

                Some(if options.encryption == Encryption::FixedKey {
                    flags |= flag::FIX_KEY;
                    key.wrapping_add(number(start)) ^ number(data.len())
                } else {
                    key
                })
            }
        };
        let sealed = |bytes: &mut Vec<u8>, key: Option<u32>| {
            if let Some(key) = key {
                encrypt(bytes, key);
            }
        };

        match options.layout {
            Layout::Single | Layout::SingleZlib => {
                flags |= flag::SINGLE_UNIT;

                let mut bytes = data.to_vec();

                if options.layout == Layout::SingleZlib {
                    let packed = compress(0x02, data);

                    if packed.len() < data.len() {
                        bytes = packed;
                        flags |= flag::COMPRESS;
                    }
                }

                sealed(&mut bytes, key);
                out.extend(bytes);
            }
            Layout::Stored => {
                for (index, chunk) in data.chunks(sector).enumerate() {
                    let mut bytes = chunk.to_vec();
                    sealed(&mut bytes, key.map(|key| key.wrapping_add(number(index))));
                    out.extend(bytes);
                }
            }
            Layout::Zlib | Layout::Bzip2 => {
                flags |= flag::COMPRESS;

                let method = if options.layout == Layout::Zlib {
                    0x02
                } else {
                    0x10
                };
                let sectors: Vec<Vec<u8>> = data
                    .chunks(sector)
                    .enumerate()
                    .map(|(index, chunk)| {
                        let mut bytes = compress(method, chunk);

                        if bytes.len() >= chunk.len() {
                            bytes = chunk.to_vec();
                        }

                        sealed(&mut bytes, key.map(|key| key.wrapping_add(number(index))));

                        bytes
                    })
                    .collect();

                if !sectors.is_empty() {
                    let crc = options.sector_crc;

                    if crc {
                        flags |= flag::SECTOR_CRC;
                    }

                    let mut position = (sectors.len() + 1 + usize::from(crc)) * 4;
                    let mut offsets = Vec::new();

                    for bytes in &sectors {
                        offsets.push(number(position));
                        position += bytes.len();
                    }

                    offsets.push(number(position));

                    if crc {
                        offsets.push(number(position + sectors.len() * 4));
                    }

                    let mut table: Vec<u8> =
                        offsets.iter().flat_map(|word| word.to_le_bytes()).collect();
                    sealed(&mut table, key.map(|key| key.wrapping_sub(1)));
                    out.extend(table);

                    for bytes in sectors.iter() {
                        out.extend(bytes);
                    }

                    if crc {
                        out.extend(vec![0; sectors.len() * 4]);
                    }
                }
            }
        }

        (number(out.len() - start), flags)
    }
}

/// `value` as a number of the archive's tables.
fn number(value: usize) -> u32 {
    u32::try_from(value).unwrap_or_else(|_| panic!("{value} doesn't fit an archive's number"))
}

/// `data` compressed: the byte for how, then the stream.
fn compress(method: u8, data: &[u8]) -> Vec<u8> {
    let mut out = vec![method];

    if method == 0x02 {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder
            .write_all(data)
            .unwrap_or_else(|error| panic!("{error}"));
        out.extend(encoder.finish().unwrap_or_else(|error| panic!("{error}")));
    } else {
        let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        encoder
            .write_all(data)
            .unwrap_or_else(|error| panic!("{error}"));
        out.extend(encoder.finish().unwrap_or_else(|error| panic!("{error}")));
    }

    out
}

/// Encrypts `data` in place with `key`, the inverse of the reader's
/// decryption.
fn encrypt(data: &mut [u8], key: u32) {
    let (mut first, mut second) = (key, 0xEEEE_EEEEu32);

    for chunk in data.as_chunks_mut::<4>().0 {
        second = second.wrapping_add(CRYPT[0x400 + (first & 0xFF) as usize]);
        let plain = u32::from_le_bytes(*chunk);
        let word = plain ^ first.wrapping_add(second);
        first = ((!first) << 21).wrapping_add(0x1111_1111) | (first >> 11);
        second = plain
            .wrapping_add(second)
            .wrapping_add(second << 5)
            .wrapping_add(3);
        *chunk = word.to_le_bytes();
    }
}
