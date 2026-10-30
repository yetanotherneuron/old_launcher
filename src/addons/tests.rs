use std::collections::BTreeMap;
use std::fs;
use std::sync::LazyLock;

use iced::widget::Id;
use iced_test::simulator;

use super::*;
use crate::core::{Addon, Bundled, Marker, Toc};
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

/// The launcher's own client: GitHub's API refuses requests without its
/// user agent.
fn client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    crate::http_client().build().unwrap()
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
        false,
    ))
}

/// [`open`], while the client's game runs.
fn playing<'a>(
    state: &'a State,
    installation: &'a Installation,
    snapshot: &'a Snapshot,
) -> simulator::Simulator<'a, Message> {
    simulator(state.view(
        &EMPTY,
        Some(installation),
        Some(snapshot),
        catalog::Refresh::Idle,
        true,
    ))
}

/// Applies `message` with no client open, and returns what it reported.
fn apply(state: &mut State, catalog: &Catalog, message: Message) -> Errors {
    let mut errors = Errors::default();
    let _ = state.update(
        message,
        catalog,
        Ok(&client()),
        None,
        None,
        false,
        &mut errors,
    );

    errors
}

/// What `installation` holds, as the launcher sees it once its game's
/// archives are looked at, here holding no addon.
fn known(installation: &Installation) -> Snapshot {
    Snapshot {
        bundled: Bundled::none(),
        ..installation.scan().unwrap()
    }
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

        let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle, false));
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

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle, false));
    let _ = ui.click("Install");
    assert_eq!(ui.into_messages().count(), 0);

    let mut ui = simulator(state.view(
        &catalog,
        Some(&installation),
        Some(&snapshot),
        catalog::Refresh::Idle,
        false,
    ));
    let _ = ui.click("Install").unwrap();
    assert!(matches!(
        ui.into_messages().next(),
        Some(Message::Install(Key(key))) if key == "acp"
    ));
}

#[test]
fn a_finished_install_stays_disabled_until_the_client_is_read_again() {
    let catalog = catalog();
    let (_root, installation) = testing::client(&[]);
    let snapshot = installation.scan().unwrap();
    let mut state = State::new();
    let key = Key::of(&catalog.entries[0]);

    let _ = state.update(
        Message::Finished(key.clone(), Ok(vec!["Acp".into()])),
        &catalog,
        Ok(&client()),
        Some(&installation),
        None,
        false,
        &mut Errors::default(),
    );

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    let _ = ui.click("Install");
    assert_eq!(ui.into_messages().count(), 0);

    state.rescanned();

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    let _ = ui.click("Install").unwrap();
    assert!(matches!(
        ui.into_messages().next(),
        Some(Message::Install(Key(key))) if key == "acp"
    ));
}

#[test]
fn without_a_catalog_the_list_says_what_is_going_on() {
    let state = State::new();
    let catalog = Catalog::default();

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Running, false));
    assert!(ui.find("Fetching the list of addons…").is_ok());
    drop(ui);

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle, false));
    assert!(
        ui.find("No addons listed yet: check the connection and try again.")
            .is_ok()
    );
    let _ = ui.click("Try again").unwrap();
    assert!(matches!(ui.into_messages().next(), Some(Message::Reload)));
}

#[test]
fn cards_show_every_source_the_date_folders_and_foreign_interfaces() {
    let catalog = catalog();
    let state = State::new();

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle, false));

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
fn an_addon_changed_since_it_was_installed_says_so_on_hover() {
    let catalog = catalog();
    let (_root, installation) = client_with_skada(&catalog);
    let state = State::new();
    let snapshot = installation.scan().unwrap();
    assert!(
        open(&state, &catalog, &installation, &snapshot)
            .find("Modified")
            .is_err(),
        "as installed"
    );

    // A file edited, and one added, by hand.
    let skada = installation.addons_directory().join("Skada");
    std::fs::write(
        skada.join("Skada.toc"),
        "## Title: Skada\n## Version: dev\n",
    )
    .unwrap();
    std::fs::write(skada.join("Extra.lua"), "").unwrap();
    let snapshot = installation.scan().unwrap();
    let changed = snapshot
        .addons
        .iter()
        .find(|addon| addon.name == "Skada")
        .map(|addon| addon.changed.clone());
    assert_eq!(
        changed.as_deref(),
        Some(&["Extra.lua".to_owned(), "Skada.toc".to_owned()][..])
    );

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    let badge = ui.find("Modified").unwrap().bounds().center();
    ui.point_at(badge);
    let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
        position: badge,
    })]);
    assert!(
        ui.find(
            "Changed since it was installed: Skada/Extra.lua, Skada/Skada.toc. Reinstall puts \
             the installed version back."
        )
        .is_ok()
    );
    assert!(ui.find("Reinstall").is_ok());
}

#[test]
fn addons_made_for_other_versions_explain_the_risk_on_hover() {
    let catalog = catalog();
    let state = State::new();

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle, false));
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

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle, false));
    assert!(ui.find("GitHub ↗").is_err(), "the same page as the pill");
    assert!(ui.find("Curseforge ↗").is_ok());
    let _ = ui.click("Other Source ↗").unwrap();
    assert!(matches!(
        ui.into_messages().next(),
        Some(Message::OpenLink(url)) if url == "https://example.org/skada"
    ));

    // A source without a page per addon links to its homepage.
    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle, false));
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
            changed: Vec::new(),
        }],
        ..Snapshot::default()
    };

    let mut ui = simulator(state.view(
        &catalog,
        None,
        Some(&snapshot),
        catalog::Refresh::Idle,
        false,
    ));

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

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle, false));
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

    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle, false));

    assert!(ui.find("Select a client to see its addons.").is_ok());
}

#[test]
fn recently_updated_addons_can_come_first() {
    let catalog = catalog();
    let mut state = State::new();
    let order = |state: &State| {
        let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle, false));
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
    let snapshot = known(&installation);
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
        false,
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
            false,
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
    assert!(state.working.is_some() && state.confirm.is_none());
    assert_eq!(state.under_way(), ["Uninstalling ACP: 1 folder"]);

    let done = update(&mut state, Message::Done(Ok(())));
    assert!(matches!(done.instruction, Some(Instruction::Changed)));
    assert!(state.working.is_none());
    assert!(state.under_way().is_empty());
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
        false,
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
        false,
        &mut errors,
    );
    assert!(matches!(finished.instruction, Some(Instruction::Changed)));
    state.rescanned();

    // Stopping a download is asked for; the job stays until it says it
    // stopped, and nothing is reported then.
    let _ = state.update(
        Message::Install(key.clone()),
        &catalog,
        Ok(&client),
        Some(&installation),
        None,
        false,
        &mut errors,
    );
    let _ = state.update(
        Message::Stop(key.clone()),
        &catalog,
        Ok(&client),
        Some(&installation),
        None,
        false,
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
        false,
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
        false,
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
    let snapshot = known(&installation);
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

    // Until the game's archives are known, the data may be an addon's of
    // theirs.
    let unknown = installation.scan().unwrap();
    let mut ui = open(&state, &catalog, &installation, &unknown);
    assert!(ui.find("Leftover data (0)").is_ok());
    assert!(ui.find("Recount").is_err());
    assert!(
        ui.find(
            "Leftover data shows once this client's patches are looked at: some addons come \
             inside them."
        )
        .is_ok()
    );
    drop(ui);

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
fn saved_settings_are_left_alone_while_the_game_runs() {
    let (_root, installation) = testing::client(&[
        "Interface/AddOns/Skada/Skada.toc",
        "WTF/Account/ME/SavedVariables/Skada.lua",
        "WTF/Account/ME/SavedVariables/Recount.lua",
    ]);
    let snapshot = known(&installation);
    let mut state = State::new();

    // Clean data is off, and says why on hover.
    let mut ui = playing(&state, &installation, &snapshot);
    let button = ui.find("Clean data").unwrap().bounds().center();
    ui.point_at(button);
    let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
        position: button,
    })]);
    assert!(
        ui.find(format!(
            "Saved settings in 1 file, shared by the whole account. {}",
            game::KEEPS_SETTINGS
        ))
        .is_ok()
    );
    let _ = ui.click("Clean data").unwrap();
    assert_eq!(ui.into_messages().count(), 0);

    // Uninstalling still works, keeping the saved settings.
    let mut ui = playing(&state, &installation, &snapshot);
    let _ = ui.click(Id::from(card::uninstall_id("Skada"))).unwrap();
    for message in ui.into_messages() {
        apply(&mut state, &EMPTY, message);
    }
    let mut ui = playing(&state, &installation, &snapshot);
    assert!(ui.find(game::KEEPS_SETTINGS).is_ok());
    let _ = ui.click("Also delete its saved settings (1 file)").unwrap();
    assert_eq!(ui.into_messages().count(), 0, "the box is off");
    let _ = state.update(
        Message::Cancel,
        &EMPTY,
        Ok(&client()),
        Some(&installation),
        Some(&snapshot),
        true,
        &mut Errors::default(),
    );

    // Neither leftover data goes.
    state.filter.status = Status::LeftoverData;
    let mut ui = playing(&state, &installation, &snapshot);
    assert!(ui.find("Recount").is_ok());
    let _ = ui.click("Delete all").unwrap();
    let _ = ui.click("Delete").unwrap();
    assert_eq!(ui.into_messages().count(), 0);

    // A deletion asked before the game started waits for it to end.
    state.confirm = Some(Confirm::Clean {
        title: "Recount".into(),
        files: snapshot.orphaned_data()[0].1.to_vec(),
    });
    let action = state.update(
        Message::Confirm,
        &EMPTY,
        Ok(&client()),
        Some(&installation),
        Some(&snapshot),
        true,
        &mut Errors::default(),
    );
    assert_eq!(action.task.units(), 0);
    assert!(state.confirm.is_some(), "still asking");
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
        false,
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
        false,
    ));
    assert!(ui.find("No longer available").is_ok());
    let _ = ui.click("NoM0Re ↗").unwrap();

    assert!(matches!(
        ui.into_messages().next(),
        Some(Message::OpenLink(url)) if url == "https://github.com/NoM0Re/WoW-3.3.5a-Addons"
    ));
}

#[test]
fn manual_installs_link_to_the_pages_their_toc_names() {
    let (_root, installation) = testing::client_with(&[(
        "Interface/AddOns/Raven/Raven.toc",
        "## Title: Raven\n## X-Website: http://www.example.org/raven/\n\
         ## X-Repository: https://github.com/example/Raven\n## X-Email: someone@example.org\n",
    )]);
    let snapshot = installation.scan().unwrap();
    let state = State::new();

    let mut ui = open(&state, &EMPTY, &installation, &snapshot);
    assert!(ui.find("Installed manually").is_ok());
    assert!(ui.find("github.com/example/Raven ↗").is_ok());
    let _ = ui.click("example.org/raven ↗").unwrap();

    assert!(matches!(
        ui.into_messages().next(),
        Some(Message::OpenLink(url)) if url == "http://www.example.org/raven/"
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
            false,
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
        false,
    ));
    let mut ui = ui.rebuild(plain.view(
        &EMPTY,
        Some(&installation),
        Some(&snapshot),
        catalog::Refresh::Idle,
        false,
    ));

    assert_eq!(offsets(&mut ui), scrolled);
}

#[test]
fn a_card_out_of_view_is_found_to_scroll_to() {
    use iced::advanced::widget::operation::black_box;

    // More entries than fit in the window.
    let entries: Vec<String> = (0..30)
        .map(|index| {
            format!(
                r#"{{ "name": "Addon{index:02}", "description": "One of many.",
                      "downloads": [{{ "source": "test", "name": "Addon{index:02}",
                                      "url": "https://example.com/Addon{index:02}.zip",
                                      "folders": [{{ "name": "Addon{index:02}" }}] }}] }}"#
            )
        })
        .collect();
    let catalog = Catalog::parse(
        format!(
            r#"{{ "version": 2,
                  "sources": [{{ "id": "test", "name": "Test Source", "homepage": "https://example.com" }}],
                  "entries": [{}] }}"#,
            entries.join(",")
        )
        .as_bytes(),
    )
    .unwrap();
    let state = State::new();
    let mut ui = simulator::Simulator::with_size(
        iced::Settings::default(),
        (1000.0, 500.0),
        state.view(&catalog, None, None, catalog::Refresh::Idle, false),
    );
    let locate = |ui: &mut simulator::Simulator<'_, Message>, name: &str| -> Option<f32> {
        let entry = catalog
            .entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap();
        let mut locate = Locate::new(card::id(&Key::of(entry)));
        // The list says how far it is scrolled as last drawn.
        ui.draw(&style::theme());
        ui.operate(&mut black_box(&mut locate));
        match locate.finish() {
            Outcome::Some(top) => top,
            _ => panic!("no outcome"),
        }
    };

    assert_eq!(locate(&mut ui, "Addon00"), None, "in view");
    let last = locate(&mut ui, "Addon29").expect("out of view");
    assert!(last > 500.0, "{last}");

    // Scrolled there, it is in view, and the first card isn't.
    ui.point_at(iced::Point::new(500.0, 300.0));
    let _ = ui.scroll(iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -last });
    assert_eq!(locate(&mut ui, "Addon29"), None);
    let first = locate(&mut ui, "Addon00").expect("scrolled past");
    assert!(first < 100.0, "{first}");
    assert_eq!(
        Locate::new(card::id(&Key::of(&catalog.entries[0]))).top(),
        None,
        "nothing found yet, nothing to scroll to"
    );
}

#[test]
fn a_failed_repair_keeps_the_list_scrolled_where_it_was() {
    let mut tocs: Vec<String> = (0..30)
        .map(|index| format!("Interface/AddOns/Addon{index:02}/Addon{index:02}.toc"))
        .collect();
    tocs.push("Interface/AddOns/Foo-master/Foo/Foo.toc".into());
    let (_root, installation) =
        testing::client(&tocs.iter().map(String::as_str).collect::<Vec<_>>());
    let snapshot = known(&installation);

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
    working.working = Some("Repairing Foo-master".into());
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
            false,
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
    let snapshot = known(&installation);
    assert_eq!(
        snapshot.addons,
        [Addon {
            name: "Foo-master".into(),
            toc: None,
            marker: None,
            changed: Vec::new(),
        }]
    );
    let mut state = State::new();
    let catalog = Catalog::default();

    // Until the game's archives are known, it may be a folder of one of
    // theirs: it isn't listed, let alone broken.
    let unknown = installation.scan().unwrap();
    let mut ui = open(&state, &catalog, &installation, &unknown);
    assert!(ui.find("Foo-master").is_err());
    assert!(ui.find("Repair").is_err());
    drop(ui);

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
    drop(ui);

    // Which says nothing loads fine before the archives are known.
    let mut ui = open(&state, &catalog, &installation, &unknown);
    assert!(ui.find("Needs attention (0)").is_ok());
    assert!(
        ui.find(
            "What needs attention shows once this client's patches are looked at: some addons \
             come inside them."
        )
        .is_ok()
    );
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
        changes: None,
        links: Vec::new(),
        folders: Vec::new(),
    };

    (source.to_owned(), download)
}

#[tokio::test]
async fn a_download_installs_and_marks_the_folders_with_its_source() {
    let server = serve(vec![("/acp.zip", Some(acp_zip()))]).await;
    let (_root, installation) = testing::client(&[]);
    let (source, download) = download("Working", format!("{server}/acp.zip"));
    let marker = download.marker();

    let folders = install(
        client(),
        None,
        source,
        download,
        installation.clone(),
        Vec::new(),
        Arc::default(),
    )
    .await
    .unwrap();

    assert_eq!(folders, ["ACP"]);
    assert_eq!(installation.scan().unwrap().addons[0].marker, Some(marker));
}

/// A failed download or install names its source and installs nothing;
/// no other source is tried in its place.
#[tokio::test]
async fn a_failed_download_names_its_source_and_installs_nothing() {
    let server = serve(vec![("/html.zip", Some(b"<html>".to_vec()))]).await;
    let (_root, installation) = testing::client(&[]);
    let attempt = |source: &str, file: &str| {
        let (source, download) = download(source, format!("{server}/{file}"));

        install(
            client(),
            None,
            source,
            download,
            installation.clone(),
            Vec::new(),
            Arc::default(),
        )
    };

    let missing = attempt("Missing", "gone.zip").await.unwrap_err();
    let page = attempt("Page", "html.zip").await.unwrap_err();

    let Failure::Failed(reason) = &missing else {
        panic!("{missing}");
    };
    assert!(
        reason.starts_with("Missing: ") && reason.contains("404"),
        "{missing}"
    );
    assert_eq!(
        page,
        Failure::Failed("Page: unrecognized archive format".into())
    );
    assert!(installation.scan().unwrap().addons.is_empty());
}

/// Signed in to GitHub, an install downloads through the GitHub module's
/// own redirect handling, and ends the same.
#[tokio::test]
async fn a_signed_in_download_installs_like_any_other() {
    let server = serve(vec![("/acp.zip", Some(acp_zip()))]).await;
    let (_root, installation) = testing::client(&[]);
    let (source, download) = download("GitHub", format!("{server}/acp.zip"));
    let direct = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let signed = github::Signed::sending_to(direct, "secret", std::slice::from_ref(&server));

    let folders = install(
        client(),
        Some(signed),
        source,
        download,
        installation.clone(),
        Vec::new(),
        Arc::default(),
    )
    .await
    .unwrap();

    assert_eq!(folders, ["ACP"]);
}

#[tokio::test]
async fn a_stopped_download_installs_nothing() {
    let server = serve(vec![("/acp.zip", Some(acp_zip()))]).await;
    let (_root, installation) = testing::client(&[]);
    let (source, download) = download("First", format!("{server}/acp.zip"));
    let cancel = Arc::new(AtomicBool::new(true));

    let error = install(
        client(),
        None,
        source,
        download,
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
    let _ = ui.click("Update to 2.0").unwrap();
    for message in ui.into_messages() {
        let _ = state.update(
            message,
            &catalog,
            Ok(&client()),
            Some(&installation),
            Some(&snapshot),
            false,
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
        None,
        "Test Source".to_owned(),
        entry.downloads[0].clone(),
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
        false,
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

/// Version 1.0 is installed and the catalog offers 2.0: the card says both,
/// the button says which it brings and, on hover, where from and what is
/// tried when that fails, the badge says the same versions and source, and
/// the source's history of changes is a click away. Another entry, not
/// installed, says the catalog's version and nothing else.
#[test]
fn an_update_says_both_versions_and_links_to_what_changed() {
    let catalog = Catalog::parse(
        br#"{ "version": 2,
              "sources": [
                { "id": "test", "name": "Test Source", "homepage": "https://example.com" },
                { "id": "other", "name": "Other Source", "homepage": "https://example.org" }
              ],
              "entries": [
                { "name": "Addon Control Panel",
                  "downloads": [
                    { "source": "test", "name": "ACP", "url": "https://example.com/ACP.zip",
                      "updated": "2026-05-06", "changes": "https://example.com/ACP/commits",
                      "folders": [{ "name": "ACP", "version": "2.0" }] },
                    { "source": "other", "name": "ACP", "url": "https://example.org/ACP.zip",
                      "folders": [{ "name": "ACP", "version": "2.0" }] }
                  ] },
                { "name": "Omen",
                  "downloads": [{ "source": "test", "name": "Omen", "url": "https://example.com/Omen.zip",
                                  "changes": "https://example.com/Omen/commits",
                                  "folders": [{ "name": "Omen", "version": "3.1" }] }] }
              ] }"#,
    )
    .unwrap();
    let (_root, installation) = testing::client_with(&[(
        "Interface/AddOns/ACP/ACP.toc",
        "## Title: ACP\n## Version: 1.0\n",
    )]);
    catalog.entries[0].downloads[0]
        .marker()
        .write(&installation.addons_directory().join("ACP"))
        .unwrap();
    let snapshot = installation.scan().unwrap();
    let state = State::new();

    // Without a client, both are catalog versions, and nothing has changed.
    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle, false));
    assert!(ui.find("Version 2.0").is_ok());
    assert!(ui.find("Version 3.1").is_ok());
    assert!(ui.find("Changes ↗").is_err());
    drop(ui);

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Installed 1.0 · New version 2.0 available").is_ok());
    assert!(ui.find("Update to 2.0").is_ok());
    assert!(
        ui.find("Version 3.1").is_ok(),
        "not installed: the catalog's"
    );
    assert!(ui.find("Version 2.0").is_err(), "said as an update instead");
    let badge = ui.find("Update available").unwrap().bounds().center();
    ui.point_at(badge);
    let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
        position: badge,
    })]);
    assert!(
        ui.find("Installed 1.0; 2.0 is available from Test Source, updated 2026-05-06.")
            .is_ok()
    );
    // The button says where it downloads from, and what is tried after.
    let button = ui.find("Update to 2.0").unwrap().bounds().center();
    ui.point_at(button);
    let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
        position: button,
    })]);
    assert!(ui.find("From Test Source, updated 2026-05-06.").is_ok());
    let _ = ui.click("Changes ↗").unwrap();

    assert!(matches!(
        ui.into_messages().next(),
        Some(Message::OpenLink(url)) if url == "https://example.com/ACP/commits"
    ));
}

/// ACP comes from two sources, the newest first, and the older one is
/// installed. The card offers to pick a source; the pick is reported to
/// remember, and once applied it is what installs and what updates are
/// measured against.
#[test]
fn a_source_can_be_picked_per_addon() {
    let mut catalog = Catalog::parse(
        br#"{ "version": 2,
              "sources": [
                { "id": "test", "name": "Test Source", "homepage": "https://example.com" },
                { "id": "other", "name": "Other Source", "homepage": "https://example.org" }
              ],
              "entries": [{ "name": "Addon Control Panel",
                            "downloads": [
                              { "source": "test", "name": "ACP", "url": "https://example.com/ACP.zip",
                                "updated": "2026-05-06", "folders": [{ "name": "ACP", "version": "2.0" }] },
                              { "source": "other", "name": "ACP", "url": "https://example.org/ACP.zip",
                                "updated": "2020-01-01", "folders": [{ "name": "ACP", "version": "1.0" }] }
                            ] }] }"#,
    )
    .unwrap();
    let (_root, installation) = testing::client_with(&[(
        "Interface/AddOns/ACP/ACP.toc",
        "## Title: ACP\n## Version: 1.0\n",
    )]);
    catalog.entries[0].downloads[1]
        .marker()
        .write(&installation.addons_directory().join("ACP"))
        .unwrap();
    let snapshot = installation.scan().unwrap();
    let mut state = State::new();
    let mut errors = Errors::default();

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Installed 1.0 · New version 2.0 available").is_ok());
    let _ = ui.click("Auto (Test Source)").unwrap();
    let _ = ui.click("Other Source").unwrap();
    let messages: Vec<Message> = ui.into_messages().collect();
    let instructions: Vec<Instruction> = messages
        .into_iter()
        .filter_map(|message| {
            state
                .update(
                    message,
                    &catalog,
                    Ok(&client()),
                    Some(&installation),
                    Some(&snapshot),
                    false,
                    &mut errors,
                )
                .instruction
        })
        .collect();
    assert!(matches!(
        instructions.as_slice(),
        [Instruction::Prefer { key, source: Some(source) }] if key.as_str() == "acp" && source == "other"
    ));

    // What the launcher does with the pick.
    catalog.prefer(&BTreeMap::from([("acp".to_owned(), "other".to_owned())]));

    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Version 1.0").is_ok(), "current, from that source");
    assert!(ui.find("Update available").is_err());
    assert!(ui.find("Updated 2020-01-01").is_ok());
    assert!(
        ui.find("Test Source ↗").is_ok(),
        "every source still linked"
    );
    let button = ui.find("Reinstall").unwrap().bounds().center();
    ui.point_at(button);
    let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
        position: button,
    })]);
    assert!(ui.find("From Other Source, updated 2020-01-01.").is_ok());
    let _ = ui.click("Reinstall").unwrap();
    for message in ui.into_messages() {
        let _ = state.update(
            message,
            &catalog,
            Ok(&client()),
            Some(&installation),
            Some(&snapshot),
            false,
            &mut errors,
        );
    }

    let job = &state.jobs[&Key::of(&catalog.entries[0])];
    assert!(
        matches!(&job.stage, Stage::Downloading { source, .. } if source == "Other Source"),
        "installs from the source picked first"
    );
}

/// At the smallest window, a card with everything its heading can hold (a
/// long name, two badges, the source picker, the changes link and the
/// three buttons) keeps the name and the buttons on one line.
#[test]
fn a_full_heading_fits_on_one_line_at_the_smallest_window() {
    let catalog = Catalog::parse(
        br#"{ "version": 2,
              "sources": [
                { "id": "test", "name": "Test Source", "homepage": "https://example.com" },
                { "id": "other", "name": "Bunny67 on GitHub", "homepage": "https://example.org" }
              ],
              "entries": [{ "name": "PitBull Unit Frames 4.0",
                            "downloads": [
                              { "source": "other", "name": "PitBull4", "url": "https://example.org/PitBull4.zip",
                                "updated": "2026-05-06", "changes": "https://example.org/PitBull4/commits",
                                "folders": [{ "name": "PitBull4", "version": "v4.2.0-beta12" }] },
                              { "source": "test", "name": "PitBull4", "url": "https://example.com/PitBull4.zip",
                                "folders": [{ "name": "PitBull4", "version": "v4.1.0" }] }
                            ] }] }"#,
    )
    .unwrap();
    let (_root, installation) = testing::client_with(&[(
        "Interface/AddOns/PitBull4/PitBull4.toc",
        "## Title: PitBull4\n## Version: v4.1.0\n",
    )]);
    catalog.entries[0].downloads[1]
        .marker()
        .write(&installation.addons_directory().join("PitBull4"))
        .unwrap();
    let mut app = crate::tests::opened(installation);
    app.catalog = catalog;

    let mut ui = simulator::Simulator::with_size(
        iced::Settings::default(),
        crate::App::MIN_SIZE,
        app.view(),
    );
    let name = ui.find("PitBull Unit Frames 4.0").unwrap().bounds();
    let update = ui.find("Update to v4.2.0-beta12").unwrap().bounds();
    let trash = ui
        .find(Id::from(card::uninstall_id("PitBull Unit Frames 4.0")))
        .unwrap()
        .bounds();

    assert!(
        (name.center_y() - update.center_y()).abs() < 4.0,
        "the name and the buttons share a line: {name:?} against {update:?}"
    );
    assert!(
        trash.x + trash.width <= crate::App::MIN_SIZE.width,
        "the buttons end inside the window: {trash:?}"
    );
}

/// Version 3.0 is installed and the catalog only has 2.0: no update, and
/// the card says the installed one is ahead.
#[test]
fn an_installed_version_ahead_of_the_catalog_says_so() {
    let catalog = Catalog::parse(
        br#"{ "version": 2,
              "sources": [{ "id": "test", "name": "Test Source", "homepage": "https://example.com" }],
              "entries": [{ "name": "Addon Control Panel",
                            "downloads": [{ "source": "test", "name": "ACP", "url": "https://example.com/ACP.zip",
                                            "changes": "https://example.com/ACP/commits",
                                            "folders": [{ "name": "ACP", "version": "2.0" }] }] }] }"#,
    )
    .unwrap();
    let (_root, installation) = testing::client_with(&[(
        "Interface/AddOns/ACP/ACP.toc",
        "## Title: ACP\n## Version: 3.0\n",
    )]);
    catalog.entries[0].downloads[0]
        .marker()
        .write(&installation.addons_directory().join("ACP"))
        .unwrap();
    let snapshot = installation.scan().unwrap();
    let state = State::new();

    let mut ui = open(&state, &catalog, &installation, &snapshot);

    assert!(
        ui.find("Installed 3.0 · Your version is newer than the 2.0 available to download")
            .is_ok()
    );
    assert!(ui.find("Update available").is_err());
    assert!(ui.find("Changes ↗").is_err(), "nothing to update to");
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

    // A repository zip, a branch snapshot, a release archive and a bare
    // repository link, plus one from each other kind of source: WoWInterface's
    // a RAR archive.
    for (source, name) in [
        ("nom0re", "Addon Control Panel"),
        ("nom0re", "Skada vs. Details!"),
        ("nom0re", "Storyline"),
        ("nom0re", "CartoMapper"),
        ("rising-gods", "Quartz"),
        ("locus313", "Omen"),
        ("github-bkader", "FixGroups-WoTLK"),
        ("wowinterface", "1337 Achievement Helper"),
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
            None,
            source.to_owned(),
            download.clone(),
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

/// Applies `message` with `installation` open as `snapshot` saw it, and
/// returns what it asked of the launcher and what it reported.
fn apply_to(
    state: &mut State,
    catalog: &Catalog,
    installation: &Installation,
    snapshot: &Snapshot,
    message: Message,
) -> (Option<Instruction>, Errors) {
    let mut errors = Errors::default();
    let action = state.update(
        message,
        catalog,
        Ok(&client()),
        Some(installation),
        Some(snapshot),
        false,
        &mut errors,
    );

    (action.instruction, errors)
}

#[test]
fn the_manual_installs_offer_to_install_an_external_addon() {
    let catalog = catalog();
    let mut state = State::new();
    let (_root, installation) = client_with_skada(&catalog);
    let snapshot = installation.scan().unwrap();

    assert!(
        open(&state, &catalog, &installation, &snapshot)
            .find("Got an external addon?")
            .is_err(),
        "only among the manual installs"
    );

    apply(
        &mut state,
        &catalog,
        Message::OriginSelected(Origin::Manual),
    );
    let mut ui = open(&state, &catalog, &installation, &snapshot);
    assert!(ui.find("Got an external addon?").is_ok());
    assert!(ui.find("Manual").is_ok(), "above the manual installs");
    let _ = ui.click("Install…").unwrap();
    let messages: Vec<Message> = ui.into_messages().collect();
    assert!(
        matches!(messages[..], [Message::OpenExternal]),
        "{messages:?}"
    );

    // Without a client open, there is nowhere to install to.
    let mut ui = simulator(state.view(&catalog, None, None, catalog::Refresh::Idle, false));
    assert!(ui.find("Got an external addon?").is_ok());
    let _ = ui.click("Install…").unwrap();
    assert_eq!(ui.into_messages().count(), 0);
}

#[test]
fn an_external_addon_is_shown_before_it_installs() {
    let catalog = catalog();
    let mut state = State::new();
    let (_root, installation) = client_with_skada(&catalog);
    let snapshot = known(&installation);
    let path = PathBuf::from("/downloads/skada-master.zip");
    let update =
        |state: &mut State, message| apply_to(state, &catalog, &installation, &snapshot, message);
    // Only the manual installs behind the dialog, so its buttons are the
    // only ones of their names.
    apply(
        &mut state,
        &catalog,
        Message::OriginSelected(Origin::Manual),
    );

    let shows = |state: &State, line: &str| {
        open(state, &catalog, &installation, &snapshot)
            .find(line)
            .is_ok()
    };
    let clicked = |state: &State, button: &str| -> Vec<Message> {
        let mut ui = open(state, &catalog, &installation, &snapshot);
        let _ = ui.click(button).unwrap();
        ui.into_messages().collect()
    };

    let _ = update(&mut state, Message::OpenExternal);
    assert!(shows(&state, "Install an addon"));
    assert!(clicked(&state, "Install").is_empty(), "nothing picked yet");
    for (button, expected) in [
        ("Choose a file…", "PickArchive"),
        ("Choose a folder…", "PickFolder"),
    ] {
        let messages = clicked(&state, button);
        assert_eq!(format!("{messages:?}"), format!("[{expected}]"));
    }

    let _ = update(&mut state, Message::Picked(Some(path.clone())));
    assert!(shows(&state, "Reading…"));
    assert!(shows(&state, path.display().to_string().as_str()));

    let _ = update(
        &mut state,
        Message::Previewed(PathBuf::from("/downloads/other.zip"), Err("late".into())),
    );
    assert!(
        shows(&state, "Reading…"),
        "another pick's reading is ignored"
    );

    let found = vec![
        Found {
            name: "Skada".into(),
            from: "skada-master/Skada".into(),
            toc: Toc {
                version: Some("2.0".into()),
                interface: Some(Toc::WOTLK_INTERFACE),
                ..Toc::default()
            },
        },
        Found {
            name: "Fresh".into(),
            from: "skada-master/Fresh".into(),
            toc: Toc {
                title: Some("Fresh Meat".into()),
                interface: Some(40000),
                dependencies: vec!["Missing".into(), "skada".into()],
                ..Toc::default()
            },
        },
    ];
    let _ = update(&mut state, Message::Previewed(path.clone(), Ok(found)));
    let replaces =
        "Replaces Skada, installed from Other Source: it won't update from there anymore.";
    for line in [
        "Choose the folders to install into Interface/AddOns:",
        "2.0 · from skada-master/Skada · already installed from Other Source",
        "Fresh Meat · from skada-master/Fresh",
        "Fresh was made for Cataclysm: it might not work on 3.3.5a.",
        "Fresh requires Missing, which won't be installed.",
    ] {
        assert!(shows(&state, line), "{line}");
    }
    assert!(
        !shows(&state, replaces),
        "Skada starts left out: the list's version stays"
    );
    assert!(
        !shows(&state, "Fresh requires skada, which won't be installed."),
        "Skada is installed"
    );
    assert!(
        open(
            &state,
            &catalog,
            &installation,
            &installation.scan().unwrap()
        )
        .find("Fresh requires Missing, which won't be installed.")
        .is_err(),
        "the game's archives may hold it, until they are known"
    );

    let messages = clicked(&state, "Skada");
    assert!(
        matches!(&messages[..], [Message::Choose(name, true)] if name == "Skada"),
        "{messages:?}"
    );
    let _ = update(&mut state, Message::Choose("Skada".into(), true));
    assert!(shows(&state, replaces), "chosen, so it warns");

    let _ = update(&mut state, Message::Choose("Fresh".into(), false));
    assert!(
        !shows(&state, "Fresh requires Missing, which won't be installed."),
        "left out, so no warning"
    );
    let _ = update(&mut state, Message::Choose("Skada".into(), false));
    assert!(
        clicked(&state, "Install").is_empty(),
        "nothing chosen, nothing to install"
    );

    let _ = update(&mut state, Message::Choose("Fresh".into(), true));
    let messages = clicked(&state, "Install");
    assert!(
        matches!(messages[..], [Message::InstallPicked]),
        "{messages:?}"
    );

    let _ = update(&mut state, Message::InstallPicked);
    let _ = update(&mut state, Message::Cancel);
    assert!(
        shows(&state, "Installing…"),
        "an install runs to its end in view"
    );

    let (instruction, errors) = update(
        &mut state,
        Message::InstalledPicked(path.clone(), Ok(vec!["Fresh".into()])),
    );
    assert!(matches!(
        instruction,
        Some(Instruction::Installed { folders, from }) if folders == ["Fresh"] && from == path
    ));
    assert!(errors.messages().is_empty());
    assert!(!shows(&state, "Install an addon"), "closed");
}

#[test]
fn an_external_pick_without_an_addon_says_why_and_a_failure_is_reported() {
    let catalog = catalog();
    let mut state = State::new();
    let (_root, installation) = client_with_skada(&catalog);
    let snapshot = installation.scan().unwrap();
    let path = PathBuf::from("/downloads/junk.zip");
    let update =
        |state: &mut State, message| apply_to(state, &catalog, &installation, &snapshot, message);
    apply(
        &mut state,
        &catalog,
        Message::OriginSelected(Origin::Manual),
    );

    let _ = update(&mut state, Message::OpenExternal);
    let _ = update(&mut state, Message::Picked(Some(path.clone())));
    let _ = update(
        &mut state,
        Message::Previewed(
            path.clone(),
            Err("no addon in it: no folder holds a .toc file".into()),
        ),
    );
    {
        let mut ui = open(&state, &catalog, &installation, &snapshot);
        assert!(
            ui.find("No addon in it: no folder holds a .toc file")
                .is_ok(),
            "as a sentence of its own"
        );
        let _ = ui.click("Install").unwrap();
        assert_eq!(ui.into_messages().count(), 0, "nothing to install");
    }

    let _ = update(&mut state, Message::Cancel);
    assert!(
        open(&state, &catalog, &installation, &snapshot)
            .find("Install an addon")
            .is_err(),
        "closed"
    );

    let fresh = Found {
        name: "Fresh".into(),
        from: "Fresh".into(),
        toc: Toc::default(),
    };
    let _ = update(&mut state, Message::OpenExternal);
    let _ = update(&mut state, Message::Picked(Some(path.clone())));
    let _ = update(
        &mut state,
        Message::Previewed(path.clone(), Ok(vec![fresh])),
    );
    let _ = update(&mut state, Message::InstallPicked);
    let (instruction, errors) = update(
        &mut state,
        Message::InstalledPicked(path.clone(), Err("disk full".into())),
    );

    assert!(matches!(instruction, Some(Instruction::Changed)));
    assert_eq!(
        errors.messages(),
        [format!(
            "Could not install from {}: disk full",
            path.display()
        )]
    );
}
