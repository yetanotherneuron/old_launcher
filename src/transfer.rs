//! Exporting a client's addons and saved settings to a zip file, and
//! importing one into another client.

use std::path::PathBuf;

use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Fill, Fit, Task, Widget};

use crate::action::Action;
use crate::catalog::{Catalog, Entry};
use crate::core::transfer::{self, Import, Listed, Plan, Summary};
use crate::core::{Installation, Snapshot};
use crate::errors::Errors;
use crate::{blocking, game, style};

/// The export and import state.
#[derive(Default)]
pub struct State {
    stage: Stage,
}

#[derive(Default)]
enum Stage {
    #[default]
    Idle,
    /// A file dialog is open, or a file is being written or read.
    Busy,
    /// An export was read: what importing it would do, until the user
    /// confirms or cancels.
    Reviewing { import: Import, plan: Plan },
    /// The export's data files are being written; these entries come next.
    Restoring { install: Vec<Entry> },
}

/// Interactions with exports and imports.
#[derive(Debug, Clone)]
pub enum Message {
    /// "Export…" was pressed.
    Export,
    /// The save dialog closed.
    ExportTo(Option<PathBuf>),
    /// The export was written.
    Exported(PathBuf, Result<Summary, String>),
    /// "Import…" was pressed.
    Import,
    /// The open dialog closed.
    ImportFrom(Option<PathBuf>),
    /// The export was read and compared with the client.
    Read(Result<(Import, Plan), String>),
    /// Go ahead with the import.
    Confirm,
    /// Close the import dialog without importing.
    Cancel,
    /// The export's data files were written, this many.
    Restored(Result<usize, String>),
}

/// What exports and imports ask of the parent.
pub enum Instruction {
    /// An export was written.
    Exported {
        /// Where.
        path: PathBuf,
        /// What it holds.
        summary: Summary,
    },
    /// An export's data files were restored; its addons are to be installed.
    Imported {
        /// How many data files were written.
        restored: usize,
        /// The catalog entries to install.
        install: Vec<Entry>,
    },
}

impl State {
    /// Whether an export or import is under way, so another can't start.
    pub fn busy(&self) -> bool {
        !matches!(self.stage, Stage::Idle)
    }

    /// What an import is doing to the client's files that quitting would
    /// cut short, for people: restoring its saved settings. The addons it
    /// installs next are the addons screen's.
    pub fn under_way(&self) -> Option<String> {
        matches!(self.stage, Stage::Restoring { .. }).then(|| "Importing saved settings".to_owned())
    }

    /// Handles an interaction with the client `installation`, as `snapshot`
    /// saw it; imports install from `catalog`, and restore no saved settings
    /// while its game runs (`playing`). Failures go to `errors`.
    pub fn update(
        &mut self,
        message: Message,
        catalog: &Catalog,
        installation: Option<&Installation>,
        snapshot: Option<&Snapshot>,
        playing: bool,
        errors: &mut Errors,
    ) -> Action<Instruction, Message> {
        match message {
            Message::Export => {
                if self.busy() {
                    return Action::none();
                }
                self.stage = Stage::Busy;

                Action::task(Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Export the addons and their saved settings")
                            .set_file_name("old_launcher-addons.zip")
                            .add_filter("Zip archive", &["zip"])
                            .save_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    Message::ExportTo,
                ))
            }
            Message::ExportTo(path) => {
                let (Some(path), Some(installation), Some(snapshot)) =
                    (path, installation, snapshot)
                else {
                    self.stage = Stage::Idle;
                    return Action::none();
                };
                let installation = installation.clone();
                let snapshot = snapshot.clone();
                log::info!("Exporting to {}", path.display());

                Action::task(Task::perform(
                    blocking({
                        let path = path.clone();
                        move || transfer::export(&installation, &snapshot, &path)
                    }),
                    move |result| Message::Exported(path.clone(), result),
                ))
            }
            Message::Exported(path, result) => {
                self.stage = Stage::Idle;

                match result {
                    Ok(summary) => {
                        log::info!(
                            "Exported {} addons and {} saved settings files",
                            summary.addons,
                            summary.files
                        );
                        Action::instruction(Instruction::Exported { path, summary })
                    }
                    Err(error) => {
                        errors.report(format!("Could not export to {}: {error}", path.display()));
                        Action::none()
                    }
                }
            }
            Message::Import => {
                if self.busy() {
                    return Action::none();
                }
                self.stage = Stage::Busy;

                Action::task(Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Import addons and saved settings")
                            .add_filter("Zip archive", &["zip"])
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    Message::ImportFrom,
                ))
            }
            Message::ImportFrom(path) => {
                let (Some(path), Some(installation), Some(snapshot)) =
                    (path, installation, snapshot)
                else {
                    self.stage = Stage::Idle;
                    return Action::none();
                };
                let catalog = catalog.clone();
                let installation = installation.clone();
                let snapshot = snapshot.clone();
                log::info!("Reading the export at {}", path.display());

                Action::task(Task::perform(
                    blocking(move || {
                        let import = Import::read(&path)?;
                        let plan = import.plan(&catalog, &installation, &snapshot);

                        Ok((import, plan))
                    }),
                    Message::Read,
                ))
            }
            Message::Read(Ok((import, plan))) => {
                log::info!(
                    "The export lists {} addons: {} to install, {} present, {} not in the \
                     catalog, {} installed by hand; {} saved settings files, {} replacing",
                    import.manifest.addons.len(),
                    plan.install.len(),
                    plan.present.len(),
                    plan.unavailable.len(),
                    plan.manual.len(),
                    import.files.len(),
                    plan.replaced.len()
                );
                self.stage = Stage::Reviewing { import, plan };
                Action::none()
            }
            Message::Read(Err(error)) => {
                self.stage = Stage::Idle;
                errors.report(format!("Could not import: {error}"));
                Action::none()
            }
            Message::Confirm => {
                let (Stage::Reviewing { import, .. }, Some(installation)) =
                    (&self.stage, installation)
                else {
                    return Action::none();
                };
                if playing && !import.files.is_empty() {
                    return Action::none();
                }
                let Stage::Reviewing { import, plan } = std::mem::take(&mut self.stage) else {
                    return Action::none();
                };
                let installation = installation.clone();
                log::info!(
                    "Restoring {} saved settings files from {}",
                    import.files.len(),
                    import.path.display()
                );

                self.stage = Stage::Restoring {
                    install: plan.install,
                };

                Action::task(Task::perform(
                    blocking(move || import.restore(&installation)),
                    Message::Restored,
                ))
            }
            Message::Cancel => {
                if matches!(self.stage, Stage::Reviewing { .. }) {
                    self.stage = Stage::Idle;
                }
                Action::none()
            }
            Message::Restored(result) => {
                let Stage::Restoring { install } = std::mem::take(&mut self.stage) else {
                    return Action::none();
                };

                match result {
                    Ok(restored) => {
                        log::info!(
                            "Restored {restored} saved settings files; {} addons to install",
                            install.len()
                        );
                        Action::instruction(Instruction::Imported { restored, install })
                    }
                    Err(error) => {
                        errors.report(format!(
                            "Could not restore the saved settings, so nothing was installed \
                             either: {error}"
                        ));
                        Action::none()
                    }
                }
            }
        }
    }

    /// The import dialog, while an export is being reviewed. An export with
    /// saved settings can't be imported while the client's game runs
    /// (`playing`).
    pub fn view(&self, playing: bool) -> Option<impl Widget<Message> + '_> {
        let Stage::Reviewing { import, plan } = &self.stage else {
            return None;
        };
        let blocked = playing && !import.files.is_empty();

        let file = import.path.file_name().map_or_else(
            || import.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );

        let install: Vec<String> = plan
            .install
            .iter()
            .map(|entry| entry.name.clone())
            .collect();

        let sections = column![
            section("To install", &install, "Nothing to install.", None),
            section(
                "Already here, left as they are",
                &titles(&plan.present),
                "",
                Some(text::secondary),
            ),
            section(
                "No longer available, so not installed",
                &titles(&plan.unavailable),
                "",
                Some(text::warning),
            ),
            section(
                "Installed by hand on the other computer: install these yourself",
                &titles(&plan.manual),
                "",
                Some(text::warning),
            ),
            files("Saved settings already here, replaced", &plan.replaced),
        ]
        .spacing(12);

        let files = import.files.len();
        let restores = if files == 0 {
            "It holds no saved settings.".to_owned()
        } else {
            format!(
                "Restores {files} saved settings {}.",
                if files == 1 { "file" } else { "files" }
            )
        };
        let replaces = match plan.replaced.len() {
            0 => None,
            1 => Some(
                "1 of them replaces a file already here, and the settings it holds.".to_owned(),
            ),
            replaced => Some(format!(
                "{replaced} of them replace files already here, and the settings they hold."
            )),
        }
        .map(|replaces| text(replaces).style(text::warning));

        Some(
            container(
                column![
                    style::heading(format!("Import {file}?"), 20.0),
                    container(scrollable(sections).height(Fit.max(320)).width(Fill))
                        .padding(10)
                        .width(Fill)
                        .style(container::rounded_box),
                    column![
                        text(restores),
                        replaces,
                        blocked.then(|| text(game::KEEPS_SETTINGS).style(text::warning)),
                    ]
                    .spacing(4),
                    row![
                        space::horizontal(),
                        button("Cancel")
                            .style(button::text)
                            .on_press(Message::Cancel),
                        button("Import")
                            .style(button::primary)
                            .on_press_maybe((!blocked).then_some(Message::Confirm)),
                    ]
                    .spacing(8),
                ]
                .spacing(14),
            )
            .padding(24)
            .width(Fill.max(620))
            .style(style::card),
        )
    }
}

/// The names addons are listed under.
fn titles(listed: &[Listed]) -> Vec<String> {
    listed.iter().map(|listed| listed.title.clone()).collect()
}

/// A titled list of `names`, with `empty` in their place when there are
/// none; left out altogether when there are none and `empty` is empty.
fn section<'a, Message: 'a>(
    title: &'a str,
    names: &[String],
    empty: &'a str,
    tone: Option<fn(&iced::Theme) -> text::Style>,
) -> Option<impl Widget<Message> + use<'a, Message>> {
    if names.is_empty() && empty.is_empty() {
        return None;
    }

    let body = if names.is_empty() {
        empty.to_owned()
    } else {
        names.join(", ")
    };
    let mut body = text(body).size(13);
    if let Some(tone) = tone {
        body = body.style(tone);
    }

    Some(column![heading(title, names.len()), body].spacing(4))
}

/// A titled list of data `files`, one per line, relative to the client
/// folder; left out when there are none.
fn files<'a, Message: 'a>(
    title: &'a str,
    files: &[PathBuf],
) -> Option<impl Widget<Message> + use<'a, Message>> {
    if files.is_empty() {
        return None;
    }

    let lines = column(files.iter().map(|file| {
        // With the slashes the other dialogs use, on Windows too.
        let path = file
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");

        text(path).size(13).style(text::warning)
    }))
    .spacing(2);

    Some(column![heading(title, files.len()), lines].spacing(4))
}

/// A section's title, with how many items it lists.
fn heading<'a>(title: &str, count: usize) -> iced::widget::Text<'a> {
    style::heading(format!("{title} ({count})"), 14.0)
}

#[cfg(test)]
mod tests {
    use iced_test::simulator;

    use super::*;
    use crate::core::Marker;
    use crate::core::catalog::Download;
    use crate::core::transfer::Manifest;
    use crate::testing;

    fn listed(title: &str, marker: bool) -> Listed {
        Listed {
            title: title.into(),
            folders: vec![title.into()],
            marker: marker.then(|| Marker {
                source: "test".into(),
                entry: title.into(),
                url: format!("https://example.com/{title}.zip"),
            }),
        }
    }

    fn entry(name: &str) -> Entry {
        Entry {
            name: name.into(),
            description: String::new(),
            image: None,
            preferred: None,
            downloads: vec![Download {
                source: "test".into(),
                name: name.into(),
                url: format!("https://example.com/{name}.zip"),
                updated: None,
                page: None,
                changes: None,
                links: Vec::new(),
                folders: Vec::new(),
            }],
        }
    }

    /// An export of Skada (to install), Omen (already there) and a
    /// hand-installed addon, with two settings files, one replacing.
    fn reviewed() -> (Import, Plan) {
        let import = Import {
            path: PathBuf::from("/exports/old_launcher-addons.zip"),
            manifest: Manifest {
                version: Manifest::VERSION,
                addons: vec![
                    listed("Skada", true),
                    listed("Omen", true),
                    listed("Mine", false),
                ],
            },
            files: vec![
                PathBuf::from("WTF/Account/ME/SavedVariables/Skada.lua"),
                PathBuf::from("WTF/Account/ME/SavedVariables/Omen.lua"),
            ],
        };
        let plan = Plan {
            install: vec![entry("Skada")],
            present: vec![listed("Omen", true)],
            unavailable: Vec::new(),
            manual: vec![listed("Mine", false)],
            replaced: vec![PathBuf::from("WTF/Account/ME/SavedVariables/Omen.lua")],
        };

        (import, plan)
    }

    fn update(state: &mut State, message: Message) -> Action<Instruction, Message> {
        update_while(state, message, false)
    }

    /// [`update`], with the client's game running or not (`playing`).
    fn update_while(
        state: &mut State,
        message: Message,
        playing: bool,
    ) -> Action<Instruction, Message> {
        let (_root, installation) = testing::client(&[]);
        let snapshot = installation.scan().unwrap();

        state.update(
            message,
            &Catalog::default(),
            Some(&installation),
            Some(&snapshot),
            playing,
            &mut Errors::default(),
        )
    }

    #[test]
    fn the_import_dialog_says_everything_it_will_do() {
        let mut state = State::default();
        let _ = update(&mut state, Message::Read(Ok(reviewed())));

        let dialog = state.view(false).expect("the dialog is open");
        let mut ui = simulator(dialog);

        for line in [
            "Import old_launcher-addons.zip?",
            "To install (1)",
            "Skada",
            "Already here, left as they are (1)",
            "Installed by hand on the other computer: install these yourself (1)",
            "Mine",
            "Restores 2 saved settings files.",
            "Saved settings already here, replaced (1)",
            "WTF/Account/ME/SavedVariables/Omen.lua",
            "1 of them replaces a file already here, and the settings it holds.",
        ] {
            assert!(ui.find(line).is_ok(), "{line}");
        }
        assert!(
            ui.find("No longer available, so not installed (0)")
                .is_err(),
            "empty sections are left out"
        );
    }

    #[test]
    fn every_saved_settings_file_an_import_replaces_is_listed() {
        let (import, mut plan) = reviewed();
        plan.replaced = vec![
            PathBuf::from("WTF/Account/ME/SavedVariables/Omen.lua"),
            [
                "WTF",
                "Account",
                "ME",
                "Icecrown",
                "Arthas",
                "SavedVariables",
                "Skada.lua",
            ]
            .iter()
            .collect(),
        ];
        let mut state = State::default();
        let _ = update(&mut state, Message::Read(Ok((import, plan))));

        let dialog = state.view(false).expect("the dialog is open");
        let mut ui = simulator(dialog);

        for line in [
            "Saved settings already here, replaced (2)",
            "WTF/Account/ME/SavedVariables/Omen.lua",
            "WTF/Account/ME/Icecrown/Arthas/SavedVariables/Skada.lua",
            "2 of them replace files already here, and the settings they hold.",
        ] {
            assert!(ui.find(line).is_ok(), "{line}");
        }
    }

    #[test]
    fn an_import_replacing_nothing_lists_nothing_replaced() {
        let (import, mut plan) = reviewed();
        plan.replaced.clear();
        let mut state = State::default();
        let _ = update(&mut state, Message::Read(Ok((import, plan))));

        let dialog = state.view(false).expect("the dialog is open");
        let mut ui = simulator(dialog);

        assert!(
            ui.find("Saved settings already here, replaced (0)")
                .is_err()
        );
        assert!(ui.find("Restores 2 saved settings files.").is_ok());
    }

    #[test]
    fn confirming_restores_the_settings_then_installs() {
        let mut state = State::default();
        let _ = update(&mut state, Message::Read(Ok(reviewed())));

        let confirmed = update(&mut state, Message::Confirm);
        assert!(confirmed.instruction.is_none(), "restoring first");
        assert!(state.busy() && state.view(false).is_none());

        let restored = update(&mut state, Message::Restored(Ok(2)));

        assert!(matches!(
            restored.instruction,
            Some(Instruction::Imported { restored: 2, install }) if install == [entry("Skada")]
        ));
        assert!(!state.busy());
    }

    #[test]
    fn saved_settings_are_not_imported_while_the_game_runs() {
        let mut state = State::default();
        let _ = update(&mut state, Message::Read(Ok(reviewed())));

        let mut ui = simulator(state.view(true).expect("the dialog is open"));
        assert!(ui.find(game::KEEPS_SETTINGS).is_ok());
        let _ = ui.click("Import").unwrap();
        assert_eq!(ui.into_messages().count(), 0, "the button is off");

        let confirmed = update_while(&mut state, Message::Confirm, true);
        assert_eq!(confirmed.task.units(), 0);
        assert!(state.view(true).is_some(), "still reviewing");

        // An export without saved settings only installs addons.
        let (mut import, plan) = reviewed();
        import.files.clear();
        let mut state = State::default();
        let _ = update(&mut state, Message::Read(Ok((import, plan))));

        let mut ui = simulator(state.view(true).expect("the dialog is open"));
        assert!(ui.find(game::KEEPS_SETTINGS).is_err());
        let _ = ui.click("Import").unwrap();
        assert_eq!(ui.into_messages().count(), 1);
    }

    #[test]
    fn a_failed_restore_installs_nothing_and_says_why() {
        let (_root, installation) = testing::client(&[]);
        let snapshot = installation.scan().unwrap();
        let mut state = State::default();
        let mut errors = Errors::default();
        let mut update = |state: &mut State, message| {
            state.update(
                message,
                &Catalog::default(),
                Some(&installation),
                Some(&snapshot),
                false,
                &mut errors,
            )
        };

        let _ = update(&mut state, Message::Read(Ok(reviewed())));
        let _ = update(&mut state, Message::Confirm);
        let failed = update(&mut state, Message::Restored(Err("disk full".into())));

        assert!(failed.instruction.is_none());
        assert_eq!(
            errors.messages(),
            ["Could not restore the saved settings, so nothing was installed either: disk full"]
        );
    }
}
