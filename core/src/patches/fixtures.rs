//! Fixtures for the tests of [`order`](super::order) and
//! [`graph`](super::graph): small archives in a client folder, and the
//! bytes of the tables and models they hold.

use std::fs;
use std::io::Cursor;
use std::path::Path;

use crate::mpq::Archive;
use crate::mpq::writer::{Options, Writer};
use crate::patches::Facts;
use crate::patches::facts::Opened;

/// An archive holding `files` (name, bytes), with a listfile naming them.
pub fn archive(files: &[(&str, Vec<u8>)]) -> Writer {
    files
        .iter()
        .fold(Writer::new(), |writer, (name, bytes)| {
            writer.file(name, bytes)
        })
        .with_listfile(Options::default())
}

/// An archive of one file that says nothing.
pub fn tiny() -> Writer {
    archive(&[("x.txt", b"x".to_vec())])
}

/// Writes `archive` to `path` under `root`, creating the folders.
pub fn put(root: &Path, path: &str, archive: &Writer) {
    let target = root.join(path);

    fs::create_dir_all(target.parent().unwrap()).unwrap();
    archive.write(&target);
}

/// What `archive` says, as the scan reads it.
pub fn facts_of(archive: &Writer) -> Facts {
    let opened = Archive::from_reader(Cursor::new(archive.build()), "archive.mpq").unwrap();

    Facts::of(&Opened::new(opened), &[])
}

/// A table of `fields` 4-byte fields: its rows and its string block.
pub fn dbc(fields: usize, rows: &[Vec<u32>], strings: &[u8]) -> Vec<u8> {
    let mut bytes = b"WDBC".to_vec();

    for number in [rows.len(), fields, fields * 4, strings.len()] {
        bytes.extend((number as u32).to_le_bytes());
    }

    for row in rows {
        assert_eq!(row.len(), fields);
        bytes.extend(row.iter().flat_map(|number| number.to_le_bytes()));
    }

    bytes.extend(strings);

    bytes
}

/// A string block holding `strings`, and where each one is.
pub fn block(strings: &[&str]) -> (Vec<u8>, Vec<u32>) {
    let mut block = vec![0];
    let mut at = Vec::new();

    for text in strings {
        at.push(block.len() as u32);
        block.extend(text.as_bytes());
        block.push(0);
    }

    (block, at)
}

/// A row of `fields` zeros, but for the numbers set.
pub fn row(fields: usize, set: &[(usize, u32)]) -> Vec<u32> {
    let mut row = vec![0; fields];

    for &(column, number) in set {
        row[column] = number;
    }

    row
}

/// A `CreatureModelData` of (id, model path).
pub fn model_data(models: &[(u32, &str)]) -> Vec<u8> {
    let (strings, at) = block(&models.iter().map(|(_, path)| *path).collect::<Vec<_>>());
    let rows: Vec<Vec<u32>> = models
        .iter()
        .zip(at)
        .map(|((id, _), at)| row(28, &[(0, *id), (2, at)]))
        .collect();

    dbc(28, &rows, &strings)
}

/// A `CreatureDisplayInfo` of (id, model id, texture).
pub fn display_info(displays: &[(u32, u32, &str)]) -> Vec<u8> {
    let (strings, at) = block(&displays.iter().map(|(_, _, tex)| *tex).collect::<Vec<_>>());
    let rows: Vec<Vec<u32>> = displays
        .iter()
        .zip(at)
        .map(|((id, model, _), at)| row(16, &[(0, *id), (1, *model), (6, at)]))
        .collect();

    dbc(16, &rows, &strings)
}

/// A `GameObjectDisplayInfo` of (id, model path).
pub fn object_displays(objects: &[(u32, &str)]) -> Vec<u8> {
    let (strings, at) = block(&objects.iter().map(|(_, path)| *path).collect::<Vec<_>>());
    let rows: Vec<Vec<u32>> = objects
        .iter()
        .zip(at)
        .map(|((id, _), at)| row(19, &[(0, *id), (1, at)]))
        .collect();

    dbc(19, &rows, &strings)
}

/// A model of version 264 with `skins` skins, no animations and no
/// textures.
pub fn m2(skins: u32) -> Vec<u8> {
    let mut bytes = vec![0; 0x58];

    bytes[..4].copy_from_slice(b"MD20");
    bytes[4..8].copy_from_slice(&264u32.to_le_bytes());
    bytes[0x44..0x48].copy_from_slice(&skins.to_le_bytes());
    bytes[0x20..0x24].copy_from_slice(&0x58u32.to_le_bytes());
    bytes[0x54..0x58].copy_from_slice(&0x58u32.to_le_bytes());

    bytes
}
