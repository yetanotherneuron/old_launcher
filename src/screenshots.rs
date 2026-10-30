//! Renders the README's screenshots: a demo client is made at
//! `$SCREENSHOT_CLIENT` and the launcher drawn on it into `$SCREENSHOT_OUT`.
//!
//! ```sh
//! SCREENSHOT_CLIENT=/tmp/demo SCREENSHOT_OUT=/tmp/shots \
//!     cargo test --lib screenshots -- --ignored
//! ```
//!
//! The renderer names each image after itself: the README's come from
//! tiny-skia (`addons-tiny-skia.png`), which draws the same on every machine.
//!
//! Nothing is fetched: what the launcher would download, or work out in the
//! background, is handed to it, as its tests do.

use std::fs;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use iced::Task;
use iced::futures::StreamExt;
use iced_test::core::widget::operation::scrollable::AbsoluteOffset;
use iced_test::core::widget::operation::{self, Animation};
use iced_test::core::widget::{Id, Operation};
use iced_test::core::{Rectangle, Size, Vector};
use iced_test::runtime::Action;
use iced_test::simulator::Simulator;

use crate::core::library::{Order, Status};
use crate::core::mods::{self, Mods};
use crate::core::mpq::writer::{Options, Writer};
use crate::core::transfer::{Import, Listed, Manifest, Plan};
use crate::core::{Details, Installation, Launch, Locale};
use crate::tests::app;
use crate::torrent::Source;
use crate::{App, Message, Selected, addons, catalog, clients, download, patches, style, transfer};

const MB: u64 = 1024 * 1024;

#[test]
#[ignore = "renders the README screenshots into $SCREENSHOT_OUT"]
fn screenshots() {
    let root = PathBuf::from(std::env::var("SCREENSHOT_CLIENT").unwrap());
    let out = std::env::var("SCREENSHOT_OUT").unwrap();
    let catalog = catalog::repository();
    // Picked by the first folder they install, which their archive fixes,
    // not by the name their source gives them, which it may reword (Omen's
    // did): the first in the catalog's order, where several bring it.
    let entry = |first: &str| {
        catalog
            .entries
            .iter()
            .find(|entry| {
                entry
                    .downloads
                    .first()
                    .and_then(|download| download.folders.first())
                    .is_some_and(|folder| folder.name == first)
            })
            .unwrap_or_else(|| panic!("{first}"))
            .clone()
    };
    let write = |path: &Path, text: &str| {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };

    // A 3.3.5a client: Wow.exe's version, its data (archives the launcher
    // can read, grown sparse to what a real one weighs without taking the
    // space), addons installed from the catalog (one at an older version),
    // one by hand, one misplaced folder.
    write(&root.join("Wow.exe"), "");
    fs::write(
        root.join("Wow.exe"),
        crate::core::testing::fake_executable(crate::core::testing::WOTLK),
    )
    .unwrap();
    for (file, megabytes) in [
        ("Data/common.MPQ", 2_690),
        ("Data/common-2.MPQ", 2_010),
        ("Data/expansion.MPQ", 2_030),
        ("Data/lichking.MPQ", 2_480),
        ("Data/patch.MPQ", 1_930),
        ("Data/patch-2.MPQ", 980),
        ("Data/patch-3.MPQ", 650),
        ("Data/enUS/locale-enUS.MPQ", 440),
        ("Data/enUS/expansion-locale-enUS.MPQ", 210),
        ("Data/enUS/lichking-locale-enUS.MPQ", 330),
        ("Data/esES/locale-esES.MPQ", 450),
        ("Data/esES/expansion-locale-esES.MPQ", 215),
        ("Data/esES/lichking-locale-esES.MPQ", 335),
    ] {
        archive(&root.join(file));
        fs::File::options()
            .write(true)
            .open(root.join(file))
            .unwrap()
            .set_len(megabytes * MB)
            .unwrap();
    }
    for locale in ["enUS", "esES"] {
        write(
            &root.join(format!("Data/{locale}/realmlist.wtf")),
            "set realmlist logon.example.com\nset patchlist localhost\n",
        );
    }
    let addons = root.join("Interface/AddOns");

    for first in [
        "Bartender4",
        "Details",
        "Questie-335",
        "WeakAuras",
        "Omen",
        "Prat-3.0",
    ] {
        let entry = entry(first);
        let download = &entry.downloads[0];
        for folder in &download.folders {
            let version = match first {
                // Behind the catalog, so its card offers an update.
                "Questie-335" => "9.8.0-335".to_owned(),
                _ => folder.version.clone().unwrap_or_else(|| "1.0".into()),
            };
            let toc = format!(
                "## Title: {}\n## Interface: {}\n## Version: {version}\n",
                folder.title.clone().unwrap_or_else(|| folder.name.clone()),
                folder.interface.unwrap_or(30300),
            );
            let path = addons.join(&folder.name);
            write(&path.join(format!("{}.toc", folder.name)), &toc);
            download.marker().write(&path).unwrap();
        }
    }
    write(
        &addons.join("MyTweaks/MyTweaks.toc"),
        "## Title: My Tweaks\n## Interface: 30300\n## Notes: Small fixes for my own UI.\n\
         ## Version: 0.3\n## Author: Player\n",
    );
    write(
        &addons.join("Bagnon-master/Bagnon/Bagnon.toc"),
        "## Title: Bagnon\n## Interface: 30300\n",
    );
    for file in [
        "WTF/Account/PLAYER/SavedVariables/Bartender4.lua",
        "WTF/Account/PLAYER/SavedVariables/Details.lua",
        "WTF/Account/PLAYER/SavedVariables/WeakAuras.lua",
        "WTF/Account/PLAYER/Icecrown/Arthas/SavedVariables/Bartender4.lua",
        "WTF/Account/PLAYER/Icecrown/Jaina/SavedVariables/Bartender4.lua",
    ] {
        write(&root.join(file), "");
    }

    let installation = Installation::open(&root).unwrap();
    let snapshot = installation.scan().unwrap();
    let mut app = app(&[
        &installation.root().display().to_string(),
        "/home/player/Games/WoW 3.3.5a (PTR)",
    ]);
    app.config.set_launch(
        installation.root(),
        Launch {
            env: "WINEPREFIX=~/Games/wotlk-prefix PROTONPATH=GE-Proton".into(),
            command: "umu-run %command%".into(),
        },
    );
    app.catalog = catalog.clone();
    app.refresh = catalog::Refresh::Idle;
    app.selected = Selected::Open {
        installation: installation.clone(),
        snapshot: Some(snapshot),
    };
    // Its mods looked at, none there: the game may start.
    let _ = app.update(Message::Clients(clients::Message::Mods(
        clients::mods::Message::Found(
            installation.root().to_path_buf(),
            clients::mods::Seen::default(),
        ),
    )));

    // The scene `name`, with the text `top`, if any, scrolled to the top of
    // the dialog it is in.
    let shoot = |app: &App, name: &str, top: Option<&str>| {
        let mut ui = Simulator::with_size(
            iced::Settings {
                fonts: style::fonts().into_iter().map(Into::into).collect(),
                font: style::FONT,
                ..iced::Settings::default()
            },
            (1200.0, 780.0),
            app.view(),
        );
        if let Some(top) = top {
            scroll_to(&mut ui, top);
        }
        let _ = ui
            .snapshot(&style::theme())
            .unwrap()
            .matches_image(format!("{out}/{name}"))
            .unwrap();
    };

    let _ = app.update(Message::Addons(addons::Message::OrderSelected(
        Order::Updated,
    )));
    shoot(&app, "addons", None);

    let _ = app.update(Message::Addons(addons::Message::OrderSelected(Order::Name)));
    let _ = app.update(Message::Addons(addons::Message::StatusSelected(
        Status::Installed,
    )));
    shoot(&app, "installed", None);

    let _ = app.update(Message::ManageClients);
    let details = Details::read(installation.root());
    let _ = app.update(Message::Clients(clients::Message::Read(
        installation.root().to_path_buf(),
        details.map_err(|error| error.to_string()),
    )));
    shoot(&app, "clients", None);
    let _ = app.update(Message::Escape);

    let listed = |name: &str| Listed {
        title: name.into(),
        folders: vec![name.into()],
        marker: None,
    };
    let import = Import {
        path: PathBuf::from("/home/player/old_launcher-addons.zip"),
        manifest: Manifest {
            version: Manifest::VERSION,
            addons: Vec::new(),
        },
        files: (0..14)
            .map(|index| PathBuf::from(format!("WTF/Account/PLAYER/SavedVariables/{index}.lua")))
            .collect(),
    };
    let plan = Plan {
        // By their first folder too: DBM, AtlasLoot, Postal+, Recount and
        // Grid2.
        install: ["DBM-AQ20", "AtlasLoot", "Postal", "Recount", "Grid2"]
            .into_iter()
            .map(entry)
            .collect(),
        present: vec![listed("Bartender4"), listed("Details"), listed("Questie")],
        unavailable: Vec::new(),
        manual: vec![listed("MyTweaks")],
        replaced: vec![
            PathBuf::from("WTF/Account/PLAYER/SavedVariables/Bartender4.lua"),
            PathBuf::from("WTF/Account/PLAYER/Icecrown/Arthas/SavedVariables/Bartender4.lua"),
        ],
    };
    let _ = app.update(Message::Addons(addons::Message::StatusSelected(
        Status::All,
    )));
    let _ = app.update(Message::Transfer(transfer::Message::Read(Ok((
        import, plan,
    )))));
    shoot(&app, "import", None);
    let _ = app.update(Message::Escape);
    let _ = app.update(Message::Addons(addons::Message::StatusSelected(
        Status::Installed,
    )));

    // The player's own patches, two of them named but for their case, and
    // Dungeon Maps, with room past its archives.
    for file in [
        "Data/patch-4.MPQ",
        "Data/patch-Z.MPQ",
        "Data/patch-z.mpq",
        "Data/enUS/patch-enUS-4.MPQ",
        "Data/enUS/patch-enUS-M.MPQ",
        "Data/esES/patch-esES-M.MPQ",
    ] {
        archive(&root.join(file));
    }
    for file in ["Data/enUS/patch-enUS-M.MPQ", "Data/esES/patch-esES-M.MPQ"] {
        fs::File::options()
            .write(true)
            .open(root.join(file))
            .unwrap()
            .set_len(24 * MB)
            .unwrap();
    }
    // The list of mods, which takes those archives for Dungeon Maps' newest
    // release's.
    let json = fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/index/mods.json")).unwrap();
    let mut list = Mods::parse(&json).unwrap();
    let maps = list
        .mods
        .iter_mut()
        .find(|listed| listed.id == "dungeon-maps")
        .unwrap();
    for (locale, files) in &mut maps.releases[0].languages {
        for (path, sum) in files {
            if let Ok(bytes) = fs::read(root.join(path.replace(mods::LOCALE, locale.code()))) {
                *sum = mods::sha256(&bytes);
            }
        }
    }
    let _ = app.update(Message::ModsLoaded(Ok(catalog::Fetched::Changed {
        catalog: list.clone(),
        tag: None,
    })));

    // The Mods section of the clients dialog, once the client's mods and
    // patches were looked at.
    let _ = app.update(Message::ManageClients);
    let details = Details::read(installation.root());
    let _ = app.update(Message::Clients(clients::Message::Read(
        installation.root().to_path_buf(),
        details.map_err(|error| error.to_string()),
    )));
    let seen = clients::mods::look(installation.root(), &list);
    assert!(
        seen.held
            .get("dungeon-maps")
            .is_some_and(|status| status.problems.is_empty()),
        "{seen:?}"
    );
    let _ = app.update(Message::Clients(clients::Message::Mods(
        clients::mods::Message::Found(installation.root().to_path_buf(), seen),
    )));
    let facts = tempfile::tempdir().unwrap();
    let locale = installation.locale().unwrap_or(Locale::EnUs);
    let _ = app.patches.look(installation.root(), locale);
    let scan = patches::look(
        installation.root(),
        locale,
        &crate::core::patches::Cache::at(facts.path()),
        &|_| {},
    );
    assert!(
        scan.as_ref().is_ok_and(|scan| scan.unreadable.is_empty()),
        "{scan:?}"
    );
    let _ = app.update(Message::Patches(patches::Message::Looked(
        installation.root().to_path_buf(),
        scan.map(Arc::new),
    )));
    shoot(&app, "mods", Some("Mods"));

    // A byte of Dungeon Maps changes past its archive, its size and time
    // staying: a look trusts them, the check reads it.
    let changed = root.join("Data/enUS/patch-enUS-M.MPQ");
    let modified = fs::metadata(&changed).unwrap().modified().unwrap();
    let mut file = fs::File::options().write(true).open(&changed).unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(&[1]).unwrap();
    file.set_modified(modified).unwrap();
    drop(file);

    // A client to download from a torrent, in two of its five languages,
    // once its Wow.exe was read.
    let mut files = vec![("Wow.exe".to_owned(), 7_704_216)];
    files.extend(
        [
            ("common", 2_690),
            ("common-2", 2_010),
            ("expansion", 2_030),
            ("lichking", 2_480),
            ("patch", 1_930),
            ("patch-2", 980),
            ("patch-3", 650),
        ]
        .map(|(name, megabytes)| (format!("Data/{name}.MPQ"), megabytes * MB)),
    );
    for (locale, megabytes) in [
        ("deDE", [1_190, 270, 420]),
        ("enUS", [1_150, 260, 410]),
        ("esES", [1_160, 265, 415]),
        ("frFR", [1_180, 268, 418]),
        ("ruRU", [1_210, 275, 430]),
    ] {
        for (name, megabytes) in ["locale", "expansion-locale", "lichking-locale"]
            .into_iter()
            .zip(megabytes)
        {
            files.push((format!("Data/{locale}/{name}-{locale}.MPQ"), megabytes * MB));
        }
        files.push((format!("Data/{locale}/realmlist.wtf"), 56));
    }
    let given = tempfile::tempdir().unwrap();
    let path = given.path().join("WoW 3.3.5a.torrent");
    fs::write(&path, torrent("WoW 3.3.5a", &files)).unwrap();
    let start = app.download.start(
        download::Request::New {
            source: Source::File(path),
            into: root.parent().unwrap().to_path_buf(),
        },
        &app.config,
        Err("offline"),
    );
    let found = run(start).into_iter().next().unwrap();
    let download::Message::Found(job, Ok(_)) = &found else {
        panic!("{found:?}");
    };
    let job = *job;
    let _ = app.update(Message::Download(found));
    let _ = app.update(Message::Download(download::Message::Identified(
        job,
        Ok(installation.clone()),
    )));
    for language in [Locale::EnUs, Locale::EsEs] {
        let _ = app.update(Message::Download(download::Message::Language(
            language, true,
        )));
    }
    shoot(&app, "download", None);
    let _ = app.update(Message::Download(download::Message::Stop));

    // The client's File integrity check, without a torrent: what the look
    // at its patches found, and its mods' files, read.
    let check = app.update(Message::Clients(clients::Message::Check));
    for message in run(check) {
        assert!(
            matches!(
                message,
                Message::Download(download::Message::Verified(_, Ok(_)))
            ),
            "{message:?}"
        );
        let _ = app.update(message);
    }
    assert!(app.download.is_open() && !app.download.is_running());
    shoot(&app, "check", None);
}

/// Writes an archive the launcher can read at `path`, holding a small file.
fn archive(path: &Path) {
    let name = path.file_stem().unwrap().to_string_lossy();

    fs::create_dir_all(path.parent().unwrap()).unwrap();
    Writer::new()
        .file(&format!("{name}.txt"), name.as_bytes())
        .with_listfile(Options::default())
        .write(path);
}

/// A `.torrent` named `name` of `files` (path with `/` between folders,
/// size), in pieces of 4 MB whose hashes are left empty: nothing is
/// downloaded from it.
fn torrent(name: &str, files: &[(String, u64)]) -> Vec<u8> {
    fn string(out: &mut Vec<u8>, bytes: &[u8]) {
        out.extend(format!("{}:", bytes.len()).as_bytes());
        out.extend(bytes);
    }

    let piece = 4 * MB;
    let total: u64 = files.iter().map(|(_, size)| size).sum();

    // Bencoded, with the keys of each dictionary sorted.
    let mut info = b"d".to_vec();
    string(&mut info, b"files");
    info.push(b'l');
    for (path, size) in files {
        info.push(b'd');
        string(&mut info, b"length");
        info.extend(format!("i{size}e").as_bytes());
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
    info.extend(format!("i{piece}e").as_bytes());
    string(&mut info, b"pieces");
    string(
        &mut info,
        &vec![0; usize::try_from(total.div_ceil(piece) * 20).unwrap()],
    );
    info.push(b'e');

    let mut torrent = b"d".to_vec();
    string(&mut torrent, b"info");
    torrent.extend(info);
    torrent.push(b'e');
    torrent
}

/// What `task` comes back with, run to its end as the launcher would.
fn run<T: Send + 'static>(task: Task<T>) -> Vec<T> {
    let Some(stream) = iced_test::runtime::task::into_stream(task) else {
        return Vec::new();
    };

    tokio::runtime::Runtime::new().unwrap().block_on(
        stream
            .filter_map(|action| {
                std::future::ready(match action {
                    Action::Output(output) => Some(output),
                    _ => None,
                })
            })
            .collect(),
    )
}

/// Scrolls the dialog in `ui` so that its text `label` is at the top, as far
/// as it goes: of the scrollables that hold the text, the dialog's is the
/// narrowest, over the list of addons that spans the window.
fn scroll_to<'a, Message: 'a>(ui: &mut Simulator<'a, Message>, label: &str) {
    struct Holding {
        /// Where the text is laid out, unscrolled.
        text: Rectangle,
        /// The bounds of the scrollables that hold it.
        found: Vec<Rectangle>,
        /// The bounds of the one to scroll, once known.
        scrolled: Option<Rectangle>,
    }

    impl Operation for Holding {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn scrollable(
            &mut self,
            _id: Option<&Id>,
            bounds: Rectangle,
            content: Size,
            _translation: Vector,
            state: &mut dyn operation::Scrollable,
        ) {
            let text = self.text;

            if self.scrolled == Some(bounds) {
                state.scroll_to(
                    AbsoluteOffset {
                        x: None,
                        y: Some(text.y - bounds.y),
                    },
                    Animation::Instant,
                    bounds,
                    content,
                );
            } else if bounds.x <= text.x
                && text.x + text.width <= bounds.x + bounds.width
                && bounds.y <= text.y
                && text.y < bounds.y + content.height
            {
                self.found.push(bounds);
            }
        }
    }

    let mut holding = Holding {
        text: ui.find(label).unwrap().bounds(),
        found: Vec::new(),
        scrolled: None,
    };
    ui.operate(&mut holding);
    holding.scrolled = holding
        .found
        .iter()
        .copied()
        .min_by(|a, b| a.width.total_cmp(&b.width));
    assert!(holding.scrolled.is_some(), "nothing scrolls to {label}");
    ui.operate(&mut holding);
}
