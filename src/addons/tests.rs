use std::fs;
use std::sync::LazyLock;

use iced::widget::Id;
use iced_test::simulator;

use super::*;
use crate::core::{Addon, Marker};
use crate::testing::{self, offsets};

const CATALOG: &str = r#"{
    "version": 2,
    "sources": [
        { "id": "test", "name": "Test Source", "homepage": "https://example.com" },
        { "id": "other", "name": "Other Source", "homepage": "https://example.org" }
    ],
    "entries": [
        { "name": "Addon Control Panel", "description": "Toggle addons in game.",
          "downloads": [{ "source": "test", "name": "ACP", "url": "https://example.com/ACP.zip",
                          "folders": [{ "name": "ACP", "interface": 30300 }] }] },
        { "name": "Skada", "description": "Modular damage meter.",
          "downloads": [
            { "source": "other", "name": "Skada!", "url": "https://example.org/Skada.zip",
              "updated": "2026-01-02", "page": "https://example.org/skada",
              "links": [
                { "label": "GitHub", "url": "https://example.org/skada" },
                { "label": "Curseforge", "url": "https://curseforge.example/skada" }
              ],
              "folders": [{ "name": "Skada", "interface": 40000 }, { "name": "SkadaStorage" }] },
            { "source": "test", "name": "Skada", "url": "https://example.com/Skada.zip",
              "updated": "2011-05-06",
              "folders": [{ "name": "Skada" }, { "name": "SkadaStorage" }] }
          ] }
    ]
}"#;

/// A catalog that knows no sources, for views that don't need one.
static EMPTY: LazyLock<Catalog> = LazyLock::new(Catalog::default);

fn catalog() -> Catalog {
    Catalog::parse(CATALOG.as_bytes()).unwrap()
}

fn client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::new()
}

/// The screen of `state`, with `installation` open as `snapshot` saw it.
fn open<'a>(
    state: &'a State,
    catalog: &'a Catalog,
    installation: &'a Installation,
    snapshot: &'a Snapshot,
) -> simulator::Simulator<'a, Message> {
    simulator(state.view(
        catalog,
        Some(installation),
        Some(snapshot),
        catalog::Refresh::Idle,
    ))
}

/// Applies `message` with no client open, and returns what it reported.
fn apply(state: &mut State, catalog: &Catalog, message: Message) -> Errors {
    let mut errors = Errors::default();
    let _ = state.update(message, catalog, Ok(&client()), None, None, &mut errors);

    errors
}

/// A client with Skada installed from [`CATALOG`] and `Manual` installed by
/// hand.
fn client_with_skada(catalog: &Catalog) -> (tempfile::TempDir, Installation) {
    let (root, installation) = testing::client(&[
        "Interface/AddOns/Skada/Skada.toc",
        "Interface/AddOns/SkadaStorage/SkadaStorage.toc",
        "Interface/AddOns/Manual/Manual.toc",
    ]);
    let marker = catalog.entries[1].downloads[0].marker();

    for folder in ["Skada", "SkadaStorage"] {
        marker
            .write(&installation.addons_directory().join(folder))
            .unwrap();
    }

    (root, installation)
}

#[test]
fn searching_narrows_the_list_by_name_description_or_folder() {
    let catalog = catalog();
    let mut state = State::new();

    for query in ["damage", "skadastorage"] {
        apply(&mut state, &catalog, Message::QueryChanged(query.into()));

        let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle));
        assert!(ui.find("Skada").is_ok(), "{query}");
        assert!(ui.find("Addon Control Panel").is_err(), "{query}");
        assert!(ui.find("All addons (1)").is_ok(), "{query}");
    }
}

#[test]
fn installing_needs_an_open_client() {
    let catalog = catalog();
    let state = State::new();
    let (_root, installation) = testing::client(&[]);
    let snapshot = installation.scan().unwrap();

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle));
    let _ = ui.click("Install");
    assert_eq!(ui.into_messages().count(), 0);

    let mut ui = simulator(state.view(
        &catalog,
        Some(&installation),
        Some(&snapshot),
        catalog::Refresh::Idle,
    ));
    let _ = ui.click("Install").unwrap();
    assert!(matches!(
        ui.into_messages().next(),
        Some(Message::Install(Key(folders))) if folders == ["acp"]
    ));
}

#[test]
fn without_a_catalog_the_list_says_what_is_going_on() {
    let state = State::new();
    let catalog = Catalog::default();

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Running));
    assert!(ui.find("Fetching the catalog…").is_ok());
    drop(ui);

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle));
    assert!(
        ui.find("No catalog yet: check the connection and press Reload.")
            .is_ok()
    );
    assert!(ui.find("Reload").is_ok());
}

#[test]
fn cards_show_every_source_the_date_folders_and_foreign_interfaces() {
    let catalog = catalog();
    let state = State::new();

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle));

    assert!(ui.find("Other Source ↗").is_ok());
    assert!(ui.find("Test Source ↗").is_ok());
    assert!(
        ui.find("Updated 2026-01-02").is_ok(),
        "the preferred download's date"
    );
    assert!(ui.find("Folders: Skada, SkadaStorage").is_ok());
    assert!(ui.find("Made for Cataclysm").is_ok());
}

#[test]
fn addons_made_for_other_versions_explain_the_risk_on_hover() {
    let catalog = catalog();
    let state = State::new();

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle));
    assert!(
        ui.find("Made for WotLK 3.3").is_err() && ui.find("Made for ").is_err(),
        "ACP declares 3.3.5a's own interface"
    );
    let badge = ui.find("Made for Cataclysm").unwrap().bounds().center();
    ui.point_at(badge);
    let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
        position: badge,
    })]);

    assert!(
        ui.find(
            "Its .toc declares interface 40000; 3.3.5a's is 30300. The game lists it as out of \
             date and only loads it with \u{201c}Load out of date AddOns\u{201d} ticked on the \
             character selection screen. Even then, it might not work, or only in part."
        )
        .is_ok()
    );
}

#[test]
fn source_pills_link_to_the_addon_page_and_links_are_never_repeated() {
    let catalog = catalog();
    let state = State::new();

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle));
    assert!(ui.find("GitHub ↗").is_err(), "the same page as the pill");
    assert!(ui.find("Curseforge ↗").is_ok());
    let _ = ui.click("Other Source ↗").unwrap();
    assert!(matches!(
        ui.into_messages().next(),
        Some(Message::OpenLink(url)) if url == "https://example.org/skada"
    ));

    // A source without a page per addon links to its homepage.
    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle));
    let _ = ui.click("Test Source ↗").unwrap();
    assert!(matches!(
        ui.into_messages().next(),
        Some(Message::OpenLink(url)) if url == "https://example.com"
    ));
}

#[test]
fn entries_installed_from_any_source_offer_a_reinstall() {
    let catalog = catalog();
    let state = State::new();
    let snapshot = Snapshot {
        addons: vec![Addon {
            name: "Skada".into(),
            toc: None,
            marker: Some(catalog.entries[1].downloads[1].marker()),
        }],
        ..Snapshot::default()
    };

    let mut ui = simulator(state.view(&catalog, None, Some(&snapshot), catalog::Refresh::Idle));

    assert!(ui.find("Reinstall").is_ok());
    assert!(ui.find("Installed").is_ok());
}

#[test]
fn the_show_filter_counts_and_picks_installed_addons() {
    let catalog = catalog();
    let (_root, installation) = client_with_skada(&catalog);
    let snapshot = installation.scan().unwrap();
    let mut state = State::new();

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    let _ = ui.click("All addons (3)").unwrap();
    assert!(ui.find("Not installed (1)").is_ok());
    let _ = ui.click("Installed (2)").unwrap();
    for message in ui.into_messages() {
        apply(&mut state, &catalog, message);
    }

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Installed (2)").is_ok());
    assert!(ui.find("Skada").is_ok());
    assert!(ui.find("Manual").is_ok());
    assert!(ui.find("Addon Control Panel").is_err());
}

#[test]
fn the_source_filter_offers_every_source_and_manual_installs() {
    let catalog = catalog();
    let (_root, installation) = client_with_skada(&catalog);
    let snapshot = installation.scan().unwrap();
    let mut state = State::new();

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    let _ = ui.click("All sources").unwrap();
    assert!(ui.find("Other Source").is_ok());
    assert!(ui.find("Test Source").is_ok());
    drop(ui);

    // The option reads like the badge on hand-installed cards.
    apply(
        &mut state,
        &catalog,
        Message::OriginSelected(Origin::Manual),
    );

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Manual").is_ok());
    assert!(ui.find("Skada").is_err());
    assert!(ui.find("Addon Control Panel").is_err());
}

#[test]
fn an_empty_result_offers_to_clear_the_filters() {
    let catalog = catalog();
    let mut state = State::new();
    apply(
        &mut state,
        &catalog,
        Message::QueryChanged("nothing".into()),
    );
    apply(&mut state, &catalog, Message::OrderSelected(Order::Updated));

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle));
    assert!(ui.find("No addon matches.").is_ok());
    let _ = ui.click("Clear filters").unwrap();
    for message in ui.into_messages() {
        apply(&mut state, &catalog, message);
    }

    assert!(state.query.is_empty());
    assert_eq!(state.filter, Filter::default());
}

#[test]
fn installed_filters_need_a_client() {
    let catalog = catalog();
    let mut state = State::new();
    apply(
        &mut state,
        &catalog,
        Message::StatusSelected(Status::Installed),
    );

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle));

    assert!(ui.find("Select a client to see its addons.").is_ok());
}

#[test]
fn recently_updated_addons_can_come_first() {
    let catalog = catalog();
    let mut state = State::new();
    let order = |state: &State| {
        let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle));
        let skada = ui.find("Skada").unwrap().bounds().y;
        let acp = ui.find("Addon Control Panel").unwrap().bounds().y;

        skada < acp
    };

    assert!(!order(&state), "alphabetical");
    apply(&mut state, &catalog, Message::OrderSelected(Order::Updated));
    assert!(order(&state), "Skada has a date, ACP has none");
}

#[test]
fn uninstalling_asks_before_deleting() {
    let (_root, installation) = testing::client(&["Interface/AddOns/ACP/ACP.toc"]);
    let snapshot = installation.scan().unwrap();
    let mut state = State::new();
    let catalog = Catalog::default();

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    let _ = ui.click(Id::from(card::uninstall_id("ACP"))).unwrap();
    for message in ui.into_messages() {
        apply(&mut state, &EMPTY, message);
    }

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Uninstall ACP?").is_ok());
    assert!(ui.find("Interface/AddOns/ACP").is_ok());
    assert!(installation.addons_directory().join("ACP").exists());
}

#[test]
fn uninstalling_offers_to_delete_the_saved_settings_too() {
    let (_root, installation) = testing::client(&[
        "Interface/AddOns/Skada/Skada.toc",
        "WTF/Account/ME/SavedVariables/Skada.lua",
        "WTF/Account/ME/Icecrown/Arthas/SavedVariables/Skada.lua",
    ]);
    let snapshot = installation.scan().unwrap();
    let catalog = Catalog::default();
    let mut state = State::new();

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    let _ = ui.click(Id::from(card::uninstall_id("Skada"))).unwrap();
    for message in ui.into_messages() {
        apply(&mut state, &catalog, message);
    }

    // Kept unless asked, since the card with "Clean data" goes away.
    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(
        ui.find("They stay in WTF for a reinstall, and show under Leftover data.")
            .is_ok()
    );
    assert!(ui.find("WTF/Account/ME/SavedVariables/Skada.lua").is_err());
    let _ = ui
        .click("Also delete its saved settings (2 files)")
        .unwrap();
    for message in ui.into_messages() {
        apply(&mut state, &catalog, message);
    }

    // Asked: the files join the list of what goes.
    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("WTF/Account/ME/SavedVariables/Skada.lua").is_ok());
    assert!(
        ui.find("WTF/Account/ME/Icecrown/Arthas/SavedVariables/Skada.lua")
            .is_ok()
    );
    drop(ui);

    state.confirm.take().unwrap().run(&installation).unwrap();
    let after = installation.scan().unwrap();
    assert!(after.addons.is_empty());
    assert!(after.saved_variables.iter().next().is_none());
}

#[test]
fn uninstalling_an_addon_without_settings_does_not_mention_them() {
    let (_root, installation) = testing::client(&["Interface/AddOns/Foo-master/Foo/Foo.toc"]);
    let snapshot = installation.scan().unwrap();
    let catalog = Catalog::default();
    let mut state = State::new();

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    let _ = ui
        .click(Id::from(card::uninstall_id("Foo-master")))
        .unwrap();
    for message in ui.into_messages() {
        apply(&mut state, &catalog, message);
    }

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(
        ui.find("These folders will be deleted from Interface/AddOns.")
            .is_ok()
    );
    assert!(
        ui.find("They stay in WTF for a reinstall, and show under Leftover data.")
            .is_err()
    );
}

#[test]
fn the_uninstall_icon_says_what_it_does_on_hover() {
    let (_root, installation) = testing::client(&["Interface/AddOns/ACP/ACP.toc"]);
    let snapshot = installation.scan().unwrap();
    let state = State::new();

    let mut ui = simulator(state.view(
        &EMPTY,
        Some(&installation),
        Some(&snapshot),
        catalog::Refresh::Idle,
    ));
    let icon = ui
        .find(Id::from(card::uninstall_id("ACP")))
        .unwrap()
        .bounds()
        .center();
    ui.point_at(icon);
    let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
        position: icon,
    })]);

    assert!(ui.find("Uninstall").is_ok());
}

#[test]
fn confirming_runs_the_deletion_and_asks_for_a_rescan() {
    let (_root, installation) = testing::client(&[]);
    let mut state = State::new();
    let client = client();
    let mut errors = Errors::default();
    let mut update = |state: &mut State, message| {
        state.update(
            message,
            &EMPTY,
            Ok(&client),
            Some(&installation),
            None,
            &mut errors,
        )
    };

    let _ = update(
        &mut state,
        Message::Ask(Confirm::Uninstall {
            title: "ACP".into(),
            folders: vec!["ACP".into()],
            data: Vec::new(),
            delete_data: false,
            required_by: Vec::new(),
            optionally_used_by: Vec::new(),
        }),
    );

    let confirmed = update(&mut state, Message::Confirm);
    assert!(confirmed.instruction.is_none());
    assert!(state.working && state.confirm.is_none());

    let done = update(&mut state, Message::Done(Ok(())));
    assert!(matches!(done.instruction, Some(Instruction::Changed)));
    assert!(!state.working);
    assert!(errors.messages().is_empty());
}

#[test]
fn failures_go_to_the_banner_saying_what_failed() {
    let catalog = catalog();
    let (_root, installation) = testing::client(&[]);
    let client = client();
    let mut state = State::new();
    let mut errors = Errors::default();
    let key = Key::of(&catalog.entries[0]);

    let _ = state.update(
        Message::Install(key.clone()),
        &catalog,
        Ok(&client),
        Some(&installation),
        None,
        &mut errors,
    );
    let finished = state.update(
        Message::Finished(
            key.clone(),
            Err(Failure::Failed("Test Source: 404 Not Found".into())),
        ),
        &catalog,
        Ok(&client),
        Some(&installation),
        None,
        &mut errors,
    );
    assert!(matches!(finished.instruction, Some(Instruction::Changed)));

    // Stopping a download is asked for; the job stays until it says it
    // stopped, and nothing is reported then.
    let _ = state.update(
        Message::Install(key.clone()),
        &catalog,
        Ok(&client),
        Some(&installation),
        None,
        &mut errors,
    );
    let _ = state.update(
        Message::Stop(key.clone()),
        &catalog,
        Ok(&client),
        Some(&installation),
        None,
        &mut errors,
    );
    assert!(
        state.jobs[&key]
            .cancel
            .load(std::sync::atomic::Ordering::Relaxed)
            && matches!(state.jobs[&key].stage, Stage::Cancelling)
    );
    let stopped = state.update(
        Message::Finished(key.clone(), Err(Failure::Cancelled)),
        &catalog,
        Ok(&client),
        Some(&installation),
        None,
        &mut errors,
    );
    assert!(stopped.instruction.is_none() && !state.jobs.contains_key(&key));

    // Without an HTTP client, installing says why nothing downloads.
    let _ = state.update(
        Message::Install(key),
        &catalog,
        Err("no TLS provider"),
        Some(&installation),
        None,
        &mut errors,
    );

    assert_eq!(
        errors.messages(),
        [
            "Could not install Addon Control Panel: Test Source: 404 Not Found",
            "Downloads are unavailable: no TLS provider",
        ]
    );
}

#[test]
fn leftover_data_of_removed_addons_can_be_deleted() {
    let (_root, installation) = testing::client(&[
        "Interface/AddOns/ACP/ACP.toc",
        "WTF/Account/ME/SavedVariables/ACP.lua",
        "WTF/Account/ME/SavedVariables/Recount.lua",
        "WTF/Account/ME/Realm/Hero/SavedVariables/Recount.lua",
    ]);
    let snapshot = installation.scan().unwrap();
    let mut state = State::new();
    let catalog = Catalog::default();

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Clean data").is_ok());
    assert!(ui.find("Recount").is_err(), "not an addon");
    let _ = ui.click("All addons (1)").unwrap();
    let _ = ui.click("Leftover data (1)").unwrap();
    for message in ui.into_messages() {
        apply(&mut state, &EMPTY, message);
    }

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Recount").is_ok());
    assert!(ui.find("2 files").is_ok());
    let _ = ui.click("Delete all").unwrap();
    assert!(matches!(
        ui.into_messages().next(),
        Some(Message::Ask(Confirm::Clean { files, .. })) if files.len() == 2
    ));
}

#[test]
fn clean_data_explains_whose_settings_it_deletes_on_hover() {
    let (_root, installation) = testing::client(&[
        "Interface/AddOns/Skada/Skada.toc",
        "WTF/Account/ME/SavedVariables/Skada.lua",
        "WTF/Account/ME/SavedVariables/Skada.lua.bak",
        "WTF/Account/ME/Icecrown/Arthas/SavedVariables/Skada.lua",
        "WTF/Account/ME/Icecrown/Jaina/SavedVariables/Skada.lua",
    ]);
    let snapshot = installation.scan().unwrap();
    let state = State::new();

    let mut ui = simulator(state.view(
        &EMPTY,
        Some(&installation),
        Some(&snapshot),
        catalog::Refresh::Idle,
    ));
    let button = ui.find("Clean data").unwrap().bounds().center();
    ui.point_at(button);
    let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
        position: button,
    })]);

    // Counts only: the confirmation lists the files one by one.
    assert!(
        ui.find(
            "Saved settings in 4 files: 2 shared by the whole account, and 2 for 2 characters."
        )
        .is_ok()
    );
}

#[test]
fn the_data_summary_stays_one_sentence_however_many_characters() {
    let files: Vec<PathBuf> = (0..40)
        .map(|index| {
            PathBuf::from(format!(
                "/wow/WTF/Account/ME/Icecrown/Hero{index}/SavedVariables/Skada.lua"
            ))
        })
        .collect();

    assert_eq!(
        confirm::data_summary(&files),
        "Saved settings in 40 files, for 40 characters."
    );
    assert_eq!(
        confirm::data_summary(&[PathBuf::from(
            "/wow/WTF/Account/ME/SavedVariables/Skada.lua"
        )]),
        "Saved settings in 1 file, shared by the whole account."
    );
}

#[test]
fn addons_from_entries_the_catalog_dropped_keep_their_source() {
    let (_root, installation) = testing::client(&["Interface/AddOns/ACP/ACP.toc"]);
    Marker {
        source: "nom0re".into(),
        entry: "Addon Control Panel".into(),
        url: "https://example.com/ACP.zip".into(),
    }
    .write(&installation.addons_directory().join("ACP"))
    .unwrap();
    let snapshot = installation.scan().unwrap();
    let state = State::new();
    let catalog = Catalog {
        version: Catalog::VERSION,
        sources: vec![crate::core::catalog::Source {
            id: "nom0re".into(),
            name: "NoM0Re".into(),
            homepage: "https://github.com/NoM0Re/WoW-3.3.5a-Addons".into(),
        }],
        entries: Vec::new(),
    };

    let mut ui = simulator(state.view(
        &catalog,
        Some(&installation),
        Some(&snapshot),
        catalog::Refresh::Idle,
    ));
    assert!(ui.find("No longer in the catalog").is_ok());
    let _ = ui.click("NoM0Re ↗").unwrap();

    assert!(matches!(
        ui.into_messages().next(),
        Some(Message::OpenLink(url)) if url == "https://github.com/NoM0Re/WoW-3.3.5a-Addons"
    ));
}

#[test]
fn addons_list_what_requires_them() {
    let (_root, installation) = client_with_dependents();
    let snapshot = installation.scan().unwrap();
    let mut state = State::new();
    let catalog = Catalog::default();

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Required by: My Plugin").is_ok());
    assert!(ui.find("Optionally used by: Extras").is_ok());
    drop(ui);

    // Dependents are listed even when a search hides them.
    apply(&mut state, &EMPTY, Message::QueryChanged("ace3".into()));
    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("My Plugin").is_err());
    assert!(ui.find("Required by: My Plugin").is_ok());
}

#[test]
fn uninstalling_warns_about_dependent_addons() {
    let (_root, installation) = client_with_dependents();
    let snapshot = installation.scan().unwrap();
    let mut state = State::new();
    let catalog = Catalog::default();

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    let _ = ui.click(Id::from(card::uninstall_id("Ace3"))).unwrap();
    for message in ui.into_messages() {
        apply(&mut state, &EMPTY, message);
    }

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Uninstall Ace3?").is_ok());
    assert!(
        ui.find("My Plugin requires Ace3 and will not load without it.")
            .is_ok()
    );
    assert!(
        ui.find("Extras optionally uses Ace3; the rest keeps working without it.")
            .is_ok()
    );
}

/// A client with Ace3, a plugin that requires it and one that can use it.
fn client_with_dependents() -> (tempfile::TempDir, Installation) {
    let (root, installation) = testing::client(&["Interface/AddOns/Ace3/Ace3.toc"]);

    for (folder, toc) in [
        ("Plugin", "## Title: My Plugin\n## Dependencies: Ace3\n"),
        ("Extras", "## Title: Extras\n## OptionalDeps: Ace3\n"),
    ] {
        let path = root.path().join("Interface/AddOns").join(folder);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join(format!("{folder}.toc")), toc).unwrap();
    }

    (root, installation)
}

#[test]
fn dialogs_keep_the_list_scrolled_where_it_was() {
    let tocs: Vec<String> = (0..30)
        .map(|index| format!("Interface/AddOns/Addon{index:02}/Addon{index:02}.toc"))
        .collect();
    let (_root, installation) =
        testing::client(&tocs.iter().map(String::as_str).collect::<Vec<_>>());
    let snapshot = installation.scan().unwrap();
    let plain = State::new();
    let mut confirming = State::new();
    apply(
        &mut confirming,
        &EMPTY,
        Message::Ask(Confirm::Repair {
            folder: "Addon05".into(),
        }),
    );
    let mut ui = iced_test::simulator::Simulator::with_size(
        iced::Settings::default(),
        (1000.0, 500.0),
        plain.view(
            &EMPTY,
            Some(&installation),
            Some(&snapshot),
            catalog::Refresh::Idle,
        ),
    );
    ui.point_at(iced::Point::new(500.0, 300.0));
    // Touchpad-style scrolling applies at once, without the wheel's easing.
    let _ = ui.scroll(iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -600.0 });

    let scrolled = offsets(&mut ui);
    assert!(scrolled[0] > 0.0, "the list scrolled down: {scrolled:?}");

    // Open and close the dialog.
    let ui = ui.rebuild(confirming.view(
        &EMPTY,
        Some(&installation),
        Some(&snapshot),
        catalog::Refresh::Idle,
    ));
    let mut ui = ui.rebuild(plain.view(
        &EMPTY,
        Some(&installation),
        Some(&snapshot),
        catalog::Refresh::Idle,
    ));

    assert_eq!(offsets(&mut ui), scrolled);
}

#[test]
fn a_failed_repair_keeps_the_list_scrolled_where_it_was() {
    let mut tocs: Vec<String> = (0..30)
        .map(|index| format!("Interface/AddOns/Addon{index:02}/Addon{index:02}.toc"))
        .collect();
    tocs.push("Interface/AddOns/Foo-master/Foo/Foo.toc".into());
    let (_root, installation) =
        testing::client(&tocs.iter().map(String::as_str).collect::<Vec<_>>());
    let snapshot = installation.scan().unwrap();

    // Every state the screen goes through: asked, confirmed and working,
    // failed (the error goes to the app's banner), and back to normal.
    let plain = State::new();
    let mut asked = State::new();
    apply(
        &mut asked,
        &EMPTY,
        Message::Ask(Confirm::Repair {
            folder: "Foo-master".into(),
        }),
    );
    let mut working = State::new();
    working.working = true;
    let mut failed = State::new();
    let reported = apply(
        &mut failed,
        &EMPTY,
        Message::Done(Err("Could not repair Foo-master: nothing to repair".into())),
    );
    assert_eq!(
        reported.messages(),
        ["Could not repair Foo-master: nothing to repair"]
    );

    let view = |state| {
        State::view(
            state,
            &EMPTY,
            Some(&installation),
            Some(&snapshot),
            catalog::Refresh::Idle,
        )
    };

    let mut ui = iced_test::simulator::Simulator::with_size(
        iced::Settings::default(),
        (1000.0, 500.0),
        view(&plain),
    );
    ui.point_at(iced::Point::new(500.0, 300.0));
    let _ = ui.scroll(iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -600.0 });
    let scrolled = offsets(&mut ui);
    assert!(scrolled[0] > 0.0, "the list scrolled down: {scrolled:?}");

    // The list is the first scrollable; the dialog has one too.
    for (step, state) in [&asked, &working, &failed, &plain].into_iter().enumerate() {
        ui = ui.rebuild(view(state));
        assert_eq!(offsets(&mut ui)[0], scrolled[0], "step {step}");
    }
}

#[test]
fn folders_the_client_cannot_load_offer_a_repair() {
    let (_root, installation) = testing::client(&["Interface/AddOns/Foo-master/Foo/Foo.toc"]);
    let snapshot = installation.scan().unwrap();
    assert_eq!(
        snapshot.addons,
        [Addon {
            name: "Foo-master".into(),
            toc: None,
            marker: None
        }]
    );
    let mut state = State::new();
    let catalog = Catalog::default();

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    let _ = ui.click("Repair").unwrap();
    assert!(matches!(
        ui.into_messages().next(),
        Some(Message::Ask(Confirm::Repair { folder })) if folder == "Foo-master"
    ));

    // It is also what the "Needs attention" filter is for.
    apply(
        &mut state,
        &EMPTY,
        Message::StatusSelected(Status::Attention),
    );
    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Needs attention (1)").is_ok());
    assert!(ui.find("Foo-master").is_ok());
}

/// Serves `files` (path, body) over HTTP on localhost, 404 for anything
/// else or for a `None` body, and returns the base URL.
async fn serve(files: Vec<(&'static str, Option<Vec<u8>>)>) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    // The server lives as long as the test's runtime.
    drop(tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let read = stream.read(&mut request).await.unwrap();
            let request = String::from_utf8_lossy(&request[..read]).into_owned();
            let path = request.split_whitespace().nth(1).unwrap_or("/");

            let response = match files.iter().find(|(file, _)| *file == path) {
                Some((_, Some(body))) => {
                    let mut response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    response.extend(body);
                    response
                }
                _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    .to_vec(),
            };

            stream.write_all(&response).await.unwrap();
        }
    }));

    format!("http://{address}")
}

fn acp_zip() -> Vec<u8> {
    zip_with(&[("ACP/ACP.toc", "## Title: ACP\n")])
}

/// A zip holding `files` (path, contents).
fn zip_with(files: &[(&str, &str)]) -> Vec<u8> {
    use std::io::Write;

    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (path, contents) in files {
        writer
            .start_file(*path, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(contents.as_bytes()).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn download(source: &str, url: String) -> (String, Download) {
    let download = Download {
        source: source.to_lowercase(),
        name: format!("ACP from {source}"),
        url,
        updated: None,
        page: None,
        links: Vec::new(),
        folders: Vec::new(),
    };

    (source.to_owned(), download)
}

#[tokio::test]
async fn failing_downloads_fall_back_to_the_next_source() {
    let server = serve(vec![("/gone.zip", None), ("/acp.zip", Some(acp_zip()))]).await;
    let (_root, installation) = testing::client(&[]);
    let downloads = vec![
        download("Broken", format!("{server}/gone.zip")),
        download("Working", format!("{server}/acp.zip")),
    ];
    let working = downloads[1].1.marker();

    let folders = install(
        client(),
        downloads,
        installation.clone(),
        Vec::new(),
        Arc::default(),
    )
    .await
    .unwrap();

    assert_eq!(folders, ["ACP"]);
    let snapshot = installation.scan().unwrap();
    assert_eq!(
        snapshot.addons[0].marker,
        Some(working),
        "marked as installed from the one that worked"
    );
}

#[tokio::test]
async fn when_every_source_fails_each_reason_is_given() {
    let server = serve(vec![("/html.zip", Some(b"<html>".to_vec()))]).await;
    let (_root, installation) = testing::client(&[]);
    let downloads = vec![
        download("Missing", format!("{server}/gone.zip")),
        download("Page", format!("{server}/html.zip")),
    ];

    let error = install(
        client(),
        downloads,
        installation.clone(),
        Vec::new(),
        Arc::default(),
    )
    .await
    .unwrap_err();

    let Failure::Failed(reasons) = &error else {
        panic!("{error}");
    };
    assert!(
        reasons.contains("Missing: ") && reasons.contains("404"),
        "{error}"
    );
    assert!(
        reasons.contains("Page: unrecognized archive format"),
        "{error}"
    );
    assert!(installation.scan().unwrap().addons.is_empty());
}

#[tokio::test]
async fn a_stopped_download_installs_nothing_and_tries_no_other_source() {
    let server = serve(vec![("/acp.zip", Some(acp_zip()))]).await;
    let (_root, installation) = testing::client(&[]);
    let downloads = vec![
        download("First", format!("{server}/acp.zip")),
        download("Second", format!("{server}/acp.zip")),
    ];
    let cancel = Arc::new(AtomicBool::new(true));

    let error = install(
        client(),
        downloads,
        installation.clone(),
        Vec::new(),
        cancel,
    )
    .await
    .unwrap_err();

    assert_eq!(error, Failure::Cancelled);
    assert!(installation.scan().unwrap().addons.is_empty());
}

/// Version 1.0 of an addon is installed, in two folders; the catalog offers
/// 2.0, which ships one of them. Pressing Update brings the client to 2.0,
/// the folder 2.0 no longer ships included.
#[tokio::test]
async fn an_installed_addon_is_updated_to_the_catalogs_version() {
    let server = serve(vec![(
        "/acp.zip",
        Some(zip_with(&[(
            "ACP/ACP.toc",
            "## Title: ACP\n## Version: 2.0\n",
        )])),
    )])
    .await;
    let catalog = Catalog::parse(
        format!(
            r#"{{ "version": 2,
                  "sources": [{{ "id": "test", "name": "Test Source", "homepage": "https://example.com" }}],
                  "entries": [{{ "name": "Addon Control Panel",
                                "downloads": [{{ "source": "test", "name": "ACP", "url": "{server}/acp.zip",
                                                "folders": [{{ "name": "ACP", "version": "2.0" }}] }}] }}] }}"#
        )
        .as_bytes(),
    )
    .unwrap();
    let entry = &catalog.entries[0];
    let (_root, installation) = testing::client_with(&[
        (
            "Interface/AddOns/ACP/ACP.toc",
            "## Title: ACP\n## Version: 1.0\n",
        ),
        (
            "Interface/AddOns/ACP_Extras/ACP_Extras.toc",
            "## Title: ACP Extras\n## Version: 1.0\n",
        ),
    ]);
    for folder in ["ACP", "ACP_Extras"] {
        entry.downloads[0]
            .marker()
            .write(&installation.addons_directory().join(folder))
            .unwrap();
    }
    let snapshot = installation.scan().unwrap();
    let mut state = State::new();
    let mut errors = Errors::default();

    // Offered as an update, and pressing it starts one.
    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Update available").is_ok());
    let _ = ui.click("Update").unwrap();
    for message in ui.into_messages() {
        let _ = state.update(
            message,
            &catalog,
            Ok(&client()),
            Some(&installation),
            Some(&snapshot),
            &mut errors,
        );
    }
    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Cancel").is_ok(), "downloading");
    drop(ui);

    // What the task it started does: replaces the version installed.
    let replacing = snapshot
        .installed(entry)
        .into_iter()
        .map(|addon| addon.name.clone())
        .collect();
    let folders = install(
        client(),
        vec![("Test Source".to_owned(), entry.downloads[0].clone())],
        installation.clone(),
        replacing,
        Arc::default(),
    )
    .await
    .unwrap();
    assert_eq!(folders, ["ACP"]);
    let action = state.update(
        Message::Finished(Key::of(entry), Ok(folders)),
        &catalog,
        Ok(&client()),
        Some(&installation),
        Some(&snapshot),
        &mut errors,
    );
    assert!(matches!(action.instruction, Some(Instruction::Changed)));
    assert!(errors.messages().is_empty());

    // Scanned again, the client is at the catalog's version.
    let snapshot = installation.scan().unwrap();
    let acp = snapshot
        .addons
        .iter()
        .find(|addon| addon.name == "ACP")
        .unwrap();
    assert_eq!(acp.toc.as_ref().unwrap().version.as_deref(), Some("2.0"));
    assert!(
        !installation.addons_directory().join("ACP_Extras").exists(),
        "the folder 2.0 no longer ships is gone"
    );
    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Update available").is_err());
    assert!(ui.find("Reinstall").is_ok());
}

/// Installs one catalog entry of every archive shape into the client at
/// `$OLD_LAUNCHER_TEST_CLIENT`, checks the result, and uninstalls again.
/// Entries with folders the client already has are skipped, so the client's
/// own addons are never replaced or removed.
#[tokio::test]
#[ignore = "downloads from the network into $OLD_LAUNCHER_TEST_CLIENT"]
async fn catalog_entries_install_into_a_real_client() {
    let root = std::env::var("OLD_LAUNCHER_TEST_CLIENT").expect("OLD_LAUNCHER_TEST_CLIENT is set");
    let installation = Installation::open(root).unwrap();
    let catalog = catalog::repository();

    // A repository zip, a branch snapshot, a rar, a 7z and a bare
    // repository link, plus one from another source.
    for (source, name) in [
        ("nom0re", "Addon Control Panel"),
        ("nom0re", "Skada vs. Details!"),
        ("nom0re", "Gatherer+"),
        ("nom0re", "Storyline"),
        ("nom0re", "CartoMapper"),
        ("rising-gods", "Quartz"),
        ("locus313", "Omen"),
        ("github-bkader", "FixGroups-WoTLK"),
    ] {
        let (entry, download) = catalog
            .entries
            .iter()
            .find_map(|entry| {
                let download = entry
                    .downloads
                    .iter()
                    .find(|download| download.source == source && download.name == name)?;
                Some((entry, download))
            })
            .unwrap_or_else(|| panic!("{source} lists {name}"));

        let addons = installation.addons_directory();
        if download
            .folders
            .iter()
            .any(|folder| addons.join(&folder.name).exists())
        {
            println!("{name}: skipped, the client already has its folders");
            continue;
        }

        let folders = install(
            client(),
            vec![(source.to_owned(), download.clone())],
            installation.clone(),
            Vec::new(),
            Arc::default(),
        )
        .await
        .unwrap_or_else(|error| panic!("{name}: {error}"));

        let snapshot = installation.scan().unwrap();
        let installed = snapshot.installed(entry);
        let mut expected: Vec<&str> = download
            .folders
            .iter()
            .map(|folder| folder.name.as_str())
            .collect();
        let mut got: Vec<&str> = folders.iter().map(String::as_str).collect();
        expected.sort_unstable();
        got.sort_unstable();

        assert_eq!(got, expected, "{name}: installs what the catalog says");
        assert_eq!(
            installed.len(),
            folders.len(),
            "{name}: every folder carries the marker"
        );
        println!("{name}: {folders:?}");

        installation.uninstall(&folders).unwrap();
        assert!(
            folders.iter().all(|folder| !addons.join(folder).exists()),
            "{name}: uninstalled"
        );
    }
}
