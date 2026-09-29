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

use std::fs;
use std::path::{Path, PathBuf};

use crate::core::library::{Order, Status};
use crate::core::transfer::{Import, Listed, Manifest, Plan};
use crate::core::{Details, Installation, Launch};
use crate::tests::app;
use crate::{App, Message, Selected, addons, catalog, clients, style, transfer};

#[test]
#[ignore = "renders the README screenshots into $SCREENSHOT_OUT"]
fn screenshots() {
    let root = PathBuf::from(std::env::var("SCREENSHOT_CLIENT").unwrap());
    let out = std::env::var("SCREENSHOT_OUT").unwrap();
    let catalog = catalog::embedded().unwrap();
    let entry = |name: &str| {
        catalog
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap_or_else(|| panic!("{name}"))
            .clone()
    };
    let write = |path: &Path, text: &str| {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };

    // A 3.3.5a client: Wow.exe's version, its data (sparse, so it weighs
    // what a real one does without taking the space), addons installed from
    // the catalog (one at an older version), one by hand, one misplaced
    // folder.
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
    ] {
        write(&root.join(file), "");
        fs::File::options()
            .write(true)
            .open(root.join(file))
            .unwrap()
            .set_len(megabytes * 1024 * 1024)
            .unwrap();
    }
    write(
        &root.join("Data/enUS/realmlist.wtf"),
        "set realmlist logon.example.com\nset patchlist localhost\n",
    );
    let addons = root.join("Interface/AddOns");

    for name in [
        "Bartender4",
        "Details",
        "Questie",
        "WeakAuras-WotLK",
        "Omen Thread Meter",
        "PitBull Unit Frames 4.0",
    ] {
        let entry = entry(name);
        let download = &entry.downloads[0];
        for folder in &download.folders {
            let version = match name {
                // Behind the catalog, so its card offers an update.
                "Questie" => "9.8.0-335".to_owned(),
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

    let shoot = |app: &App, name: &str| {
        let mut ui = iced_test::simulator::Simulator::with_size(
            iced::Settings::default(),
            (1100.0, 780.0),
            app.view(),
        );
        let _ = ui
            .snapshot(&style::theme())
            .unwrap()
            .matches_image(format!("{out}/{name}"))
            .unwrap();
    };

    let _ = app.update(Message::Addons(addons::Message::OrderSelected(
        Order::Updated,
    )));
    shoot(&app, "addons");

    let _ = app.update(Message::Addons(addons::Message::OrderSelected(Order::Name)));
    let _ = app.update(Message::Addons(addons::Message::StatusSelected(
        Status::Installed,
    )));
    shoot(&app, "installed");

    let _ = app.update(Message::ManageClients);
    let details = Details::read(installation.root());
    let _ = app.update(Message::Clients(clients::Message::Read(
        installation.root().to_path_buf(),
        details.map_err(|error| error.to_string()),
    )));
    shoot(&app, "clients");
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
        install: [
            "DBM Deadly Boss Mods Warmane",
            "AtlasLoot Enhanced",
            "Postal+",
            "Recount",
            "Grid2",
        ]
        .into_iter()
        .map(entry)
        .collect(),
        present: vec![listed("Bartender4"), listed("Details"), listed("Questie")],
        unavailable: Vec::new(),
        manual: vec![listed("MyTweaks")],
        replaced: vec![PathBuf::from(
            "WTF/Account/PLAYER/SavedVariables/Bartender4.lua",
        )],
    };
    let _ = app.update(Message::Addons(addons::Message::StatusSelected(
        Status::All,
    )));
    let _ = app.update(Message::Transfer(transfer::Message::Read(Ok((
        import, plan,
    )))));
    shoot(&app, "import");
}
