//! Lumenveil, a graphics mod for the 3.3.5a client: volumetric fog, lamp
//! light, light shafts and water (<https://github.com/lumenveil/Lumenveil>).
//!
//! `Wow.exe` loads it as the `version.dll` in its own folder, which loads
//! the renderer, `CoAVolFog.dll`, beside it. A release is a zip of those,
//! the player's settings, a `Lumenveil` folder with the setup tool, and two
//! scripts for Windows; the setup tool then writes where the world's lamps
//! and water are, read from the client's own archives. Each release also
//! publishes a `SHA256SUMS.txt` naming every file of the zip, which tells
//! what an install holds and whether a file changed since.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use sha2::{Digest, Sha256};

use crate::error::io_at;
use crate::installation::config_setting;
use crate::{Error, Installation, Version, archive};

/// The loader `Wow.exe` loads from its folder in place of Windows' own, as
/// Wine needs telling to (see [`crate::launch::command`]).
pub const LOADER: &str = "version.dll";

/// The renderer the loader loads.
pub const RENDERER: &str = "CoAVolFog.dll";

/// The player's settings, which an update leaves alone.
pub const SETTINGS: &str = "CoAVolFog.ini";

/// The look of each zone, which the player can tune in the game too.
pub const REGIONS: &str = "CoAVolFog-regions.ini";

/// The setup tool, by its path in the zip.
pub const TOOL: &str = "Lumenveil/lamp_extract.exe";

/// Where the world's lamps are, as the setup tool writes it.
pub const LAMPS: &str = "lampdata.bin";

/// Where the world's lakes and sea are, as the setup tool writes it.
pub const WATER: &str = "liquiddata.bin";

/// What the mod found and did, written at every start of the game.
pub const LOG: &str = "CoAVolFog.log";

/// The folder of the setup tool, the notices and the setup's logs.
const FOLDER: &str = "Lumenveil";

/// The checksums of the release installed, kept so its files can be checked
/// without asking GitHub (see [`kept`]).
const KEPT: &str = "Lumenveil/SHA256SUMS.txt";

/// The spots the player saved in the game: a list, and their screenshots.
const SPOTS: [&str; 2] = ["CoAVolFog-spots.txt", "CoAVolFog-spots"];

/// Every file an install leaves in the client folder but the player's:
/// the release's own and what the mod and its setup write.
const FILES: [&str; 8] = [
    LOADER,
    RENDERER,
    REGIONS,
    LAMPS,
    WATER,
    LOG,
    "Lumenveil-Setup.cmd",
    "Lumenveil-Uninstall.cmd",
];

/// A release's `SHA256SUMS.txt`: the SHA-256 of each of its files, by the
/// file's path in the zip with `/` between folders, the zip's own included.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sums(BTreeMap<String, String>);

impl Sums {
    /// Reads `SHA256SUMS.txt` as `sha256sum` writes it: per line, a
    /// checksum, a space, a space or `*`, and a path. Other lines are
    /// skipped.
    pub fn parse(text: &str) -> Self {
        Self(
            text.lines()
                .filter_map(|line| {
                    let (sum, path) = line.trim_end().split_once(' ')?;
                    let path = path.strip_prefix([' ', '*']).unwrap_or(path);
                    let hex = sum.len() == 64 && sum.bytes().all(|byte| byte.is_ascii_hexdigit());

                    (hex && !path.is_empty())
                        .then(|| (path.replace('\\', "/"), sum.to_ascii_lowercase()))
                })
                .collect(),
        )
    }

    /// The release's zip, by name, and its checksum, if the sums give it.
    pub fn archive(&self) -> Option<(&str, &str)> {
        self.0
            .iter()
            .find(|(path, _)| path.ends_with(".zip"))
            .map(|(path, sum)| (path.as_str(), sum.as_str()))
    }

    /// The files of the zip, by path, and their checksums.
    pub fn files(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0
            .iter()
            .filter(|(path, _)| !path.ends_with(".zip"))
            .map(|(path, sum)| (path.as_str(), sum.as_str()))
    }

    /// The checksum of the file at `path` in the zip.
    pub fn of(&self, path: &str) -> Option<&str> {
        self.0.get(path).map(String::as_str)
    }

    /// The sums as `sha256sum` writes them, which [`Sums::parse`] reads.
    fn text(&self) -> String {
        self.0
            .iter()
            .map(|(path, sum)| format!("{sum}  {path}\n"))
            .collect()
    }
}

/// The checksums of the release installed in the client folder at `root`,
/// if the launcher kept them: when it installed it ([`install`]), or found
/// out its version's ([`keep`]).
pub fn kept(root: &Path) -> Option<Sums> {
    fs::read_to_string(root.join(KEPT))
        .ok()
        .map(|text| Sums::parse(&text))
}

/// Keeps `sums`, those of the release installed in the client folder at
/// `root`, for [`kept`].
pub fn keep(root: &Path, sums: &Sums) -> Result<(), Error> {
    if kept(root).as_ref() == Some(sums) {
        return Ok(());
    }

    let path = root.join(KEPT);
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder).map_err(io_at(folder))?;
    }
    fs::write(&path, sums.text()).map_err(io_at(&path))
}

/// The SHA-256 of `bytes`, in lowercase hexadecimal.
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// What a client folder holds of Lumenveil, and what keeps it from working.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// The version installed, as its renderer says (`0.1.1`): releases
    /// before 0.1.1 don't say.
    pub version: Option<String>,
    /// Whether the game loaded it since it was installed: it writes its log
    /// at every start, and an install removes the old one.
    pub loaded: bool,
    /// What keeps it from working, if anything.
    pub problems: Vec<Problem>,
}

/// Something that keeps Lumenveil from working in a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// A `version.dll` that isn't Lumenveil's: another mod's, which
    /// Lumenveil can't load beside, and which installing it replaces.
    Foreign,
    /// Files of the release that are missing, by path in the zip.
    Missing(Vec<String>),
    /// Files that differ from the release's, by path in the zip; the
    /// settings are the player's to change, so they never count.
    Changed(Vec<String>),
    /// Its files weren't checked against its release, whose checksums
    /// weren't kept (see [`kept`]): only that its main files are there.
    Unchecked,
    /// The setup tool hasn't written where the lamps are: they give no
    /// light.
    NoLamps,
    /// The setup tool hasn't written where the water is: the fog takes all
    /// ground for land.
    NoWater,
    /// The client runs Direct3D 9Ex (`SET gxApi "d3d9ex"`), which Lumenveil
    /// doesn't draw on.
    Direct3D9Ex,
    /// Its log says the client isn't the 12340 build, or that something
    /// changed it: Lumenveil stays off.
    WrongClient,
    /// Its log says the graphics card or its driver refused its shaders.
    Shaders,
}

impl Problem {
    /// Whether repairing fixes it: the release put back in place of what
    /// was there, Direct3D 9 set ([`install`]), and the setup run again
    /// ([`setup`]).
    pub fn repairable(&self) -> bool {
        matches!(
            self,
            Self::Missing(_) | Self::Changed(_) | Self::NoLamps | Self::NoWater | Self::Direct3D9Ex
        )
    }
}

/// Whether the launcher has the game load Lumenveil from the client folder
/// at `root`: it is there with nothing to repair, its files checked against
/// the checksums [`kept`] of its release. What its log reports doesn't
/// count: no repair fixes that, and only running it again tells whether it
/// still holds.
pub fn working(root: &Path) -> bool {
    status(root, kept(root).as_ref()).is_some_and(|status| {
        !status.problems.iter().any(|problem| {
            matches!(problem, Problem::Foreign | Problem::Unchecked) || problem.repairable()
        })
    })
}

/// What the client folder at `root` holds of Lumenveil, if any file of it,
/// with its files checked against `sums`, the installed release's, when
/// known; when not, only that the loader, the renderer and the setup tool
/// are there, and that the rest is [`Problem::Unchecked`].
pub fn status(root: &Path, sums: Option<&Sums>) -> Option<Status> {
    let renderer = fs::read(root.join(RENDERER)).ok();
    let loader = fs::read(root.join(LOADER)).ok();

    // Another mod's loader has the same name: alone, it is Lumenveil's
    // only if its release says so.
    let ours = traces(root)
        || loader
            .as_deref()
            .is_some_and(|loader| sums.and_then(|sums| sums.of(LOADER)) == Some(&*sha256(loader)));
    if !ours && loader.is_none() {
        return None;
    }

    let log = fs::read(root.join(LOG))
        .ok()
        .map(|log| String::from_utf8_lossy(&log).to_lowercase());

    if !ours {
        return Some(Status {
            version: None,
            loaded: false,
            problems: vec![Problem::Foreign],
        });
    }

    let mut problems = Vec::new();
    let (mut missing, mut changed) = (Vec::new(), Vec::new());

    match sums {
        Some(sums) => {
            for (path, sum) in sums.files() {
                match fs::read(root.join(path)) {
                    Err(_) => missing.push(path.to_owned()),
                    Ok(bytes) if path != SETTINGS && path != REGIONS && sha256(&bytes) != sum => {
                        changed.push(path.to_owned());
                    }
                    Ok(_) => {}
                }
            }
        }
        None => missing.extend(
            [LOADER, RENDERER, TOOL]
                .into_iter()
                .filter(|path| !root.join(path).is_file())
                .map(str::to_owned),
        ),
    }

    if !missing.is_empty() {
        problems.push(Problem::Missing(missing));
    }
    if !changed.is_empty() {
        problems.push(Problem::Changed(changed));
    }
    if sums.is_none() {
        problems.push(Problem::Unchecked);
    }
    if !root.join(LAMPS).is_file() {
        problems.push(Problem::NoLamps);
    }
    if !root.join(WATER).is_file() {
        problems.push(Problem::NoWater);
    }
    if config_setting(root, "gxApi").is_some_and(|api| api.eq_ignore_ascii_case("d3d9ex")) {
        problems.push(Problem::Direct3D9Ex);
    }
    if let Some(log) = &log {
        if log.contains("hooks not installed") || log.contains("differs from the 12340 client") {
            problems.push(Problem::WrongClient);
        }
        if log.contains("shader creation failed") {
            problems.push(Problem::Shaders);
        }
    }

    Some(Status {
        version: renderer
            .as_deref()
            .and_then(Version::from_executable)
            .map(label),
        loaded: log.is_some(),
        problems,
    })
}

/// Whether the client folder at `root` holds any file of Lumenveil but its
/// loader: its own, or what it and its setup write. The player's settings
/// and saved spots don't count: uninstalling keeps them on purpose.
fn traces(root: &Path) -> bool {
    FILES
        .iter()
        .filter(|file| **file != LOADER)
        .chain(&[FOLDER])
        .any(|file| root.join(file).exists())
}

/// A version as Lumenveil tags its releases (`0.1.1`), without the build
/// number its files carry when that is 0.
fn label(version: Version) -> String {
    let Version {
        major,
        minor,
        patch,
        build,
    } = version;

    if build == 0 {
        format!("{major}.{minor}.{patch}")
    } else {
        format!("{major}.{minor}.{patch}.{build}")
    }
}

/// Installs the release whose zip is `archive` into `installation`'s folder,
/// checked against the release's `sums` first, so nothing changes unless
/// all of it is right. What was there of Lumenveil goes first but the
/// player's settings and saved spots (see [`uninstall`]), then each file the
/// sums name goes in place, but [`SETTINGS`] when the player has some, and
/// the sums are kept (see [`kept`]). A client drawing with Direct3D 9Ex is
/// set to Direct3D 9, the only one Lumenveil draws on. The setup isn't run
/// (see [`setup`]).
pub fn install(installation: &Installation, archive: &[u8], sums: &Sums) -> Result<(), Error> {
    let root = installation.root();

    if let Some((name, sum)) = sums.archive()
        && sha256(archive) != sum
    {
        return Err(Error::Checksum(name.to_owned()));
    }

    // Beside the files it replaces, so they move in by renaming.
    let staging = tempfile::Builder::new()
        .prefix(".old_launcher-")
        .tempdir_in(root)
        .map_err(io_at(root))?;
    archive::extract(archive, staging.path())?;

    for (path, sum) in sums.files() {
        let bytes = fs::read(staging.path().join(path))
            .map_err(|_| Error::NotInRelease(path.to_owned()))?;
        if sha256(&bytes) != sum {
            return Err(Error::Checksum(path.to_owned()));
        }
    }

    // Nothing left over of what was there, an older release's or a broken
    // install's.
    uninstall(installation, true)?;

    for (path, _) in sums.files() {
        let target = root.join(path);
        if path == SETTINGS && target.is_file() {
            continue;
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(io_at(parent))?;
        }
        fs::rename(staging.path().join(path), &target).map_err(io_at(&target))?;
    }
    keep(root, sums)?;

    if config_setting(root, "gxApi").is_some_and(|api| api.eq_ignore_ascii_case("d3d9ex")) {
        installation.set("gxApi", "d3d9")?;
    }

    Ok(())
}

/// The setup's two runs of [`TOOL`], for a client playing in `locale`: the
/// arguments, with paths relative to the client folder it runs in, and
/// where in it its output goes. The first finds where the lamps are, the
/// second where the water is, as `Lumenveil-Setup.cmd` runs them.
pub fn setup(locale: &str) -> [(Vec<String>, &'static str); 2] {
    let arguments = |words: &[&str]| -> Vec<String> {
        words
            .iter()
            .copied()
            .chain(["--locale", locale, "--background"])
            .map(str::to_owned)
            .collect()
    };

    [
        (
            arguments(&["--data", "Data", "--out", LAMPS]),
            "Lumenveil/setup-lamps.log",
        ),
        (
            arguments(&[
                "--data",
                "Data",
                "--liquids",
                WATER,
                "--maps",
                "0,1,530,571",
            ]),
            "Lumenveil/setup-water.log",
        ),
    ]
}

/// Removes Lumenveil from `installation`'s folder: its files, what the mod
/// and its setup wrote, and also the player's settings and saved spots
/// unless `keep_settings`. What isn't there is skipped, and so is a
/// `version.dll` without Lumenveil's renderer beside it: another mod's.
pub fn uninstall(installation: &Installation, keep_settings: bool) -> Result<(), Error> {
    let root = installation.root();
    let ours = root.join(RENDERER).is_file();

    for file in FILES {
        if file != LOADER || ours {
            remove(&root.join(file))?;
        }
    }
    remove(&root.join(FOLDER))?;

    if !keep_settings {
        remove(&root.join(SETTINGS))?;
        for spot in SPOTS {
            remove(&root.join(spot))?;
        }
    }

    Ok(())
}

/// Removes the file or folder at `path`, if there is one.
fn remove(path: &Path) -> Result<(), Error> {
    let removed = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };

    match removed {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(io_at(path)(error)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::*;
    use crate::testing::{client, fake_executable};

    const VERSION: Version = Version {
        major: 0,
        minor: 1,
        patch: 1,
        build: 0,
    };

    /// A release: its zip, as Lumenveil's are, with `\` between folders, and
    /// its sums.
    fn release() -> (Vec<u8>, Sums) {
        let files: Vec<(&str, Vec<u8>)> = vec![
            ("version.dll", b"loader".to_vec()),
            ("CoAVolFog.dll", fake_executable(VERSION)),
            ("CoAVolFog.ini", b"Enable=1\n".to_vec()),
            ("CoAVolFog-regions.ini", b"[1]\n".to_vec()),
            ("Lumenveil-Setup.cmd", b"@echo off\n".to_vec()),
            ("Lumenveil\\lamp_extract.exe", b"tool".to_vec()),
            ("Lumenveil\\licenses\\GPL-2.0.txt", b"GPL".to_vec()),
        ];

        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (path, bytes) in &files {
            writer
                .start_file(*path, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(bytes).unwrap();
        }
        let archive = writer.finish().unwrap().into_inner();

        let mut text: String = files
            .iter()
            .map(|(path, bytes)| format!("{}  {}\n", sha256(bytes), path.replace('\\', "/")))
            .collect();
        text.push_str(&format!("{}  Lumenveil-0.1.1.zip\n", sha256(&archive)));

        (archive, Sums::parse(&text))
    }

    fn read(installation: &Installation, path: &str) -> String {
        fs::read_to_string(installation.root().join(path)).unwrap()
    }

    #[test]
    fn sums_read_as_sha256sum_writes_them() {
        let sums = Sums::parse(&format!(
            "{a}  CoAVolFog.dll\n{b} *Lumenveil\\lamp_extract.exe\r\nnot a checksum  x\n{c}  L-0.1.1.zip\n",
            a = "A".repeat(64),
            b = "b".repeat(64),
            c = "c".repeat(64),
        ));

        assert_eq!(sums.of("CoAVolFog.dll"), Some(&*"a".repeat(64)));
        assert_eq!(
            sums.of("Lumenveil/lamp_extract.exe"),
            Some(&*"b".repeat(64))
        );
        assert_eq!(sums.archive(), Some(("L-0.1.1.zip", &*"c".repeat(64))));
        assert_eq!(sums.files().count(), 2, "the zip isn't one of its files");
    }

    #[test]
    fn a_folder_without_lumenveil_has_no_status() {
        let (_root, installation) = client(&[]);

        assert_eq!(status(installation.root(), None), None);
    }

    #[test]
    fn an_install_puts_every_file_in_place_and_says_what_is_left_to_do() {
        let (_root, installation) = client(&[]);
        let (archive, sums) = release();

        install(&installation, &archive, &sums).unwrap();

        assert_eq!(read(&installation, "Lumenveil/lamp_extract.exe"), "tool");
        assert_eq!(read(&installation, "Lumenveil/licenses/GPL-2.0.txt"), "GPL");
        assert_eq!(
            status(installation.root(), Some(&sums)),
            Some(Status {
                version: Some("0.1.1".to_owned()),
                loaded: false,
                problems: vec![Problem::NoLamps, Problem::NoWater],
            }),
            "the setup is still to run, and the game to start"
        );
        let leftovers: Vec<_> = fs::read_dir(installation.root())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".old_launcher-"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn the_game_loads_it_only_while_nothing_in_it_needs_repairing() {
        let (_root, installation) = client(&[]);
        let root = installation.root();
        let (archive, sums) = release();
        install(&installation, &archive, &sums).unwrap();
        assert_eq!(kept(root).as_ref(), Some(&sums), "checked offline since");
        assert!(!working(root), "the setup is still to run");

        for data in [LAMPS, WATER] {
            fs::write(root.join(data), "").unwrap();
        }
        assert!(working(root));

        // What only running it again tells doesn't keep it off.
        fs::write(root.join(LOG), "hooks not installed").unwrap();
        assert!(working(root));

        // An install never checked against its release, until it is.
        fs::remove_file(root.join(KEPT)).unwrap();
        assert!(!working(root), "unchecked");
        keep(root, &sums).unwrap();
        assert!(working(root));

        fs::write(root.join(RENDERER), "changed").unwrap();
        assert!(!working(root), "not as released");
        fs::remove_file(root.join(RENDERER)).unwrap();
        assert!(!working(root), "missing");

        uninstall(&installation, false).unwrap();
        assert_eq!(kept(root), None);
        fs::write(root.join(LOADER), "another mod").unwrap();
        assert!(!working(root));
    }

    #[test]
    fn an_update_keeps_the_players_settings_and_forgets_the_old_log() {
        let (_root, installation) = client(&[]);
        let (archive, sums) = release();
        install(&installation, &archive, &sums).unwrap();
        for (path, contents) in [
            (SETTINGS, "Enable=0\n"),
            (REGIONS, "[1] tuned\n"),
            (LOG, "hooks not installed"),
        ] {
            fs::write(installation.root().join(path), contents).unwrap();
        }
        assert!(status(installation.root(), Some(&sums)).unwrap().loaded);

        install(&installation, &archive, &sums).unwrap();

        assert_eq!(read(&installation, SETTINGS), "Enable=0\n");
        assert_eq!(read(&installation, REGIONS), "[1]\n");
        let status = status(installation.root(), Some(&sums)).unwrap();
        assert!(!status.loaded);
        assert!(!status.problems.contains(&Problem::WrongClient));
    }

    #[test]
    fn a_release_that_doesnt_match_its_sums_installs_nothing() {
        let (archive, sums) = release();

        let mut tampered = archive.clone();
        if let Some(byte) = tampered.last_mut() {
            *byte ^= 1;
        }
        let without_archive_sum = Sums::parse(
            &sums
                .files()
                .map(|(path, sum)| {
                    let sum = if path == RENDERER {
                        "0".repeat(64)
                    } else {
                        sum.to_owned()
                    };
                    format!("{sum}  {path}\n")
                })
                .collect::<String>(),
        );

        for (archive, sums, culprit) in [
            (&tampered, &sums, "Lumenveil-0.1.1.zip"),
            (&archive, &without_archive_sum, RENDERER),
        ] {
            let (_root, installation) = client(&[]);

            let installed = install(&installation, archive, sums);

            assert!(
                matches!(&installed, Err(Error::Checksum(file)) if file == culprit),
                "{installed:?}"
            );
            assert_eq!(status(installation.root(), None), None, "nothing installed");
        }
    }

    #[test]
    fn missing_and_changed_files_are_told_apart_from_the_players_settings() {
        let (_root, installation) = client(&[]);
        let (archive, sums) = release();
        install(&installation, &archive, &sums).unwrap();
        fs::remove_file(installation.root().join("Lumenveil/lamp_extract.exe")).unwrap();
        fs::write(installation.root().join(LOADER), "patched").unwrap();
        fs::write(installation.root().join(SETTINGS), "Density=2\n").unwrap();
        fs::write(installation.root().join(LAMPS), "").unwrap();
        fs::write(installation.root().join(WATER), "").unwrap();

        assert_eq!(
            status(installation.root(), Some(&sums)).unwrap().problems,
            [
                Problem::Missing(vec!["Lumenveil/lamp_extract.exe".to_owned()]),
                Problem::Changed(vec![LOADER.to_owned()]),
            ]
        );
        assert_eq!(
            status(installation.root(), None).unwrap().problems,
            [Problem::Missing(vec![TOOL.to_owned()]), Problem::Unchecked],
            "without the sums, only what it can't run without"
        );
    }

    #[test]
    fn another_mods_version_dll_is_not_lumenveil() {
        let (_root, installation) = client(&["version.dll"]);
        let (_, sums) = release();

        for sums in [None, Some(&sums)] {
            assert_eq!(
                status(installation.root(), sums),
                Some(Status {
                    version: None,
                    loaded: false,
                    problems: vec![Problem::Foreign],
                })
            );
        }

        uninstall(&installation, false).unwrap();
        assert!(
            installation.root().join(LOADER).exists(),
            "not ours to remove"
        );
    }

    #[test]
    fn its_log_and_the_clients_settings_tell_what_keeps_it_off() {
        let (_root, installation) = client(&[]);
        let (archive, sums) = release();
        install(&installation, &archive, &sums).unwrap();
        fs::write(installation.root().join(LAMPS), "").unwrap();
        fs::write(installation.root().join(WATER), "").unwrap();
        fs::write(
            installation.root().join(LOG),
            "Wow.exe differs from the 12340 client: hooks not installed\n\
             Shader creation failed; fog disabled for this device\n",
        )
        .unwrap();
        fs::create_dir_all(installation.root().join("WTF")).unwrap();
        fs::write(
            installation.root().join("WTF/Config.wtf"),
            "SET gxApi \"D3D9Ex\"\n",
        )
        .unwrap();

        let problems = status(installation.root(), Some(&sums)).unwrap().problems;

        assert_eq!(
            problems,
            [Problem::Direct3D9Ex, Problem::WrongClient, Problem::Shaders]
        );
        assert!(problems[0].repairable() && !problems[1].repairable());

        // Repairing installs it again, which forgets the log too.
        install(&installation, &archive, &sums).unwrap();
        assert_eq!(installation.setting("gxApi").as_deref(), Some("d3d9"));
        assert!(!installation.root().join(LOG).exists());
    }

    #[test]
    fn any_of_its_files_left_is_lumenveil_which_reinstalling_clears_away() {
        let (_root, installation) = client(&[SETTINGS]);
        let root = installation.root();
        assert_eq!(
            status(root, None),
            None,
            "the player's, kept on uninstalling"
        );

        for file in ["Lumenveil/stale.txt", LAMPS] {
            fs::create_dir_all(root.join("Lumenveil")).unwrap();
            fs::write(root.join(file), "").unwrap();
        }

        let left = status(root, None).unwrap();
        assert_eq!(
            left.problems,
            [
                Problem::Missing(vec![
                    LOADER.to_owned(),
                    RENDERER.to_owned(),
                    TOOL.to_owned()
                ]),
                Problem::Unchecked,
                Problem::NoWater,
            ]
        );
        assert_eq!(left.version, None);

        let (archive, sums) = release();
        install(&installation, &archive, &sums).unwrap();
        assert!(!root.join("Lumenveil/stale.txt").exists());
        assert!(!root.join(LAMPS).exists(), "the setup writes it again");
        assert_eq!(read(&installation, SETTINGS), "", "the player's");
    }

    #[test]
    fn the_setup_runs_as_its_script_does() {
        let [(lamps, lamps_log), (water, water_log)] = setup("esES");

        assert_eq!(
            lamps.join(" "),
            "--data Data --out lampdata.bin --locale esES --background"
        );
        assert_eq!(lamps_log, "Lumenveil/setup-lamps.log");
        assert_eq!(
            water.join(" "),
            "--data Data --liquids liquiddata.bin --maps 0,1,530,571 --locale esES --background"
        );
        assert_eq!(water_log, "Lumenveil/setup-water.log");
    }

    #[test]
    fn uninstalling_keeps_the_players_settings_unless_asked() {
        let (archive, sums) = release();

        for keep in [true, false] {
            let (_root, installation) = client(&["WTF/Config.wtf"]);
            install(&installation, &archive, &sums).unwrap();
            for written in [
                LAMPS,
                WATER,
                LOG,
                "CoAVolFog-spots.txt",
                "Lumenveil/setup-lamps.log",
            ] {
                fs::write(installation.root().join(written), "").unwrap();
            }
            fs::create_dir_all(installation.root().join("CoAVolFog-spots")).unwrap();

            uninstall(&installation, keep).unwrap();

            let mut left: Vec<_> = fs::read_dir(installation.root())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            left.sort();
            let expected: &[&str] = if keep {
                &[
                    "CoAVolFog-spots",
                    "CoAVolFog-spots.txt",
                    SETTINGS,
                    "WTF",
                    "Wow.exe",
                ]
            } else {
                &["WTF", "Wow.exe"]
            };
            assert_eq!(left, expected);
            assert_eq!(status(installation.root(), None), None);
        }
    }
}
