//! Extraction of downloaded addon archives (zip, 7z and rar).

use std::fs;
use std::io::{self, Cursor, Write};
use std::path::{Component, Path, PathBuf};

use crate::Error;
use crate::error::io_at;

/// A supported archive container, recognized by its magic bytes rather than
/// its URL, since download links often carry no file extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// PKZIP.
    Zip,
    /// 7-Zip.
    SevenZip,
    /// RAR, any version from 1.3 to 7.
    Rar,
}

impl Format {
    /// Recognizes the archive format of `bytes`.
    pub fn detect(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
            Some(Self::Zip)
        } else if bytes.starts_with(b"7z\xBC\xAF\x27\x1C") {
            Some(Self::SevenZip)
        } else if bytes.starts_with(b"Rar!\x1A\x07") {
            Some(Self::Rar)
        } else {
            None
        }
    }
}

/// A calendar date: year, month, day.
type Date = (u16, u8, u8);

/// Extracts the archive in `bytes` into the existing directory `destination`,
/// and returns the newest modification date among its files (`YYYY-MM-DD`)
/// when the archive records a plausible one. For a snapshot of a repository
/// that is the date of its last commit.
///
/// Entries that would land outside `destination` (absolute paths, `..`) abort
/// the extraction, and symbolic links are skipped.
pub fn extract(bytes: &[u8], destination: &Path) -> Result<Option<String>, Error> {
    // `sevenz_rust2` refuses any `..` in the joined path, even one that is
    // part of `destination` itself.
    let destination = &fs::canonicalize(destination).map_err(io_at(destination))?;
    let mut newest: Option<Date> = None;

    match Format::detect(bytes).ok_or(Error::UnknownArchive)? {
        Format::Zip => {
            let mut archive = zip::ZipArchive::new(Cursor::new(bytes))?;

            for index in 0..archive.len() {
                let mut file = archive.by_index(index)?;

                if file.is_symlink() {
                    continue;
                }

                newer(
                    &mut newest,
                    file.last_modified()
                        .map(|time| (time.year(), time.month(), time.day())),
                );

                let path = contained(destination, file.name())?;

                if file.is_dir() {
                    fs::create_dir_all(&path).map_err(io_at(&path))?;
                    continue;
                }
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(io_at(parent))?;
                }

                let mut output = fs::File::create(&path).map_err(io_at(&path))?;
                let _ = io::copy(&mut file, &mut output).map_err(io_at(&path))?;
            }
        }
        Format::SevenZip => {
            // `sevenz_rust2` rejects escaping entry names itself.
            sevenz_rust2::decompress_with_extract_fn(
                Cursor::new(bytes),
                destination,
                |entry, reader, path| {
                    if entry.has_last_modified_date {
                        // 100 ns ticks since 1601, to days since 1970.
                        let seconds = u64::from(entry.last_modified_date) / 10_000_000;
                        let days = (seconds as i64 - 11_644_473_600).div_euclid(86_400);
                        newer(&mut newest, Some(civil(days)));
                    }

                    sevenz_rust2::default_entry_extract_fn(entry, reader, path)
                },
            )?;
        }
        Format::Rar => {
            let archive = rars::ArchiveReader::read(bytes)?;

            archive.extract_to(None, |entry| {
                // A DOS timestamp: the date sits in the high 16 bits.
                let time = entry.file_time;
                newer(
                    &mut newest,
                    Some((
                        (time >> 25) as u16 + 1980,
                        ((time >> 21) & 0x0F) as u8,
                        ((time >> 16) & 0x1F) as u8,
                    )),
                );

                let name = entry.name_lossy();
                let path = contained(destination, &name).map_err(|error| {
                    rars::Error::from(io::Error::new(
                        io::ErrorKind::InvalidData,
                        error.to_string(),
                    ))
                })?;

                if entry.is_directory {
                    fs::create_dir_all(&path)?;
                    return Ok(Box::new(io::sink()) as Box<dyn Write>);
                }
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent)?;
                }

                Ok(Box::new(fs::File::create(&path)?) as Box<dyn Write>)
            })?;
        }
    }

    Ok(newest.map(|(year, month, day)| format!("{year:04}-{month:02}-{day:02}")))
}

/// Keeps the newer of `newest` and `date`, ignoring dates that cannot be
/// real: before any 3.3.5a addon existed (unset timestamps read as 1980), or
/// in the future.
fn newer(newest: &mut Option<Date>, date: Option<Date>) {
    let today = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64 / 86_400);

    if let Some(date) = date.filter(|date| date.0 >= 2000 && *date <= civil(today)) {
        *newest = (*newest).max(Some(date));
    }
}

/// The UTC calendar date `seconds` after 1970-01-01, written as the catalog
/// writes dates (`YYYY-MM-DD`).
pub fn date(seconds: i64) -> String {
    let (year, month, day) = civil(seconds.div_euclid(86_400));

    format!("{year:04}-{month:02}-{day:02}")
}

/// The calendar date `days` days after 1970-01-01.
fn civil(days: i64) -> Date {
    // Howard Hinnant's `civil_from_days`.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    (year as u16, month as u8, day as u8)
}

/// Resolves an archive entry name inside `destination`, refusing any name that
/// would escape it. Backslashes count as separators: Windows-made archives use
/// them.
fn contained(destination: &Path, name: &str) -> Result<PathBuf, Error> {
    let relative = PathBuf::from(name.replace('\\', "/"));
    let is_plain = relative
        .components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir));

    if !is_plain || relative.as_os_str().is_empty() {
        return Err(Error::UnsafeEntry(name.to_owned()));
    }

    Ok(destination.join(relative))
}

/// Builds an in-memory zip archive holding `files` (path, contents).
#[cfg(test)]
pub(crate) fn zip_of(files: &[(&str, &str)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));

    for (path, contents) in files {
        writer
            .start_file(*path, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(contents.as_bytes()).unwrap();
    }

    writer.finish().unwrap().into_inner()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_are_detected_by_magic_bytes() {
        assert_eq!(Format::detect(b"PK\x03\x04rest"), Some(Format::Zip));
        assert_eq!(
            Format::detect(b"7z\xBC\xAF\x27\x1Crest"),
            Some(Format::SevenZip)
        );
        assert_eq!(Format::detect(b"Rar!\x1A\x07\x01\x00"), Some(Format::Rar));
        assert_eq!(Format::detect(b"<!DOCTYPE html>"), None);
    }

    #[test]
    fn a_zip_is_extracted_with_its_folders() {
        let destination = tempfile::tempdir().unwrap();
        let archive = zip_of(&[("ACP/ACP.toc", "## Title: ACP"), ("ACP/ACP.lua", "-- code")]);

        // Without timestamps, zip entries read as 1980: no plausible date.
        assert_eq!(extract(&archive, destination.path()).unwrap(), None);

        let toc = fs::read_to_string(destination.path().join("ACP/ACP.toc")).unwrap();
        assert_eq!(toc, "## Title: ACP");
    }

    #[test]
    fn extraction_reports_the_newest_file_date() {
        let destination = tempfile::tempdir().unwrap();
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, (year, month, day)) in [("A/A.toc", (2019, 3, 4)), ("A/A.lua", (2024, 11, 30))] {
            let time = zip::DateTime::from_date_and_time(year, month, day, 12, 0, 0).unwrap();
            writer
                .start_file(
                    path,
                    zip::write::SimpleFileOptions::default().last_modified_time(time),
                )
                .unwrap();
        }
        let archive = writer.finish().unwrap().into_inner();

        assert_eq!(
            extract(&archive, destination.path()).unwrap().as_deref(),
            Some("2024-11-30")
        );
    }

    #[test]
    fn days_since_the_epoch_become_calendar_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(19_000), (2022, 1, 8));
        assert_eq!(civil(11_016), (2000, 2, 29));
    }

    #[test]
    fn seconds_since_the_epoch_become_catalog_dates() {
        assert_eq!(date(0), "1970-01-01");
        // Late on 2010-10-11, UTC: the evening before patch 4.0.1.
        assert_eq!(date(1_286_834_170), "2010-10-11");
        assert_eq!(date(-1), "1969-12-31");
    }

    #[test]
    fn entries_escaping_the_destination_are_rejected() {
        let destination = tempfile::tempdir().unwrap();

        for name in ["../evil.lua", "/etc/evil.lua", "Addon\\..\\..\\evil.lua"] {
            assert!(matches!(
                contained(destination.path(), name),
                Err(Error::UnsafeEntry(_))
            ));
        }
    }

    #[test]
    fn html_error_pages_are_not_archives() {
        let destination = tempfile::tempdir().unwrap();

        assert!(matches!(
            extract(b"<html>rate limited</html>", destination.path()),
            Err(Error::UnknownArchive)
        ));
    }
}
