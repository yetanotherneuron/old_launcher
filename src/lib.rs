//! Old Launcher: an addon manager for WoW 3.3.5a clients.
//!
//! The app is a thin shell over [`core`]: it remembers which client folders
//! the user added, lists the launcher's catalog of addons alongside the
//! client's, and runs every filesystem change on a blocking thread.

pub use old_launcher_core as core;

mod action;
mod addons;
mod catalog;
mod errors;
mod recovery;
mod setup;
mod style;
#[cfg(test)]
mod testing;
mod transfer;

use std::path::PathBuf;

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{
    button, center, column, container, operation, pick_list, row, scrollable, space, stack, text,
    tooltip,
};
use iced::{Center, Element, Fill, Fit, Font, Function, Subscription, Task, Theme, padding};

use crate::catalog::Catalog;
use crate::core::{Adoption, Config, Installation, Snapshot};
use crate::errors::Errors;

/// Where the launcher's code lives, as `Cargo.toml` says.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// The launcher.
pub struct App {
    config_path: PathBuf,
    config: Config,
    /// What downloads go through, or why there is nothing to download with.
    client: Result<reqwest::Client, String>,
    catalog: Catalog,
    refresh: catalog::Refresh,
    selected: Selected,
    addons: addons::State,
    setup: Option<setup::State>,
    /// The config couldn't be read: nothing else happens until the user
    /// decides what to do about it.
    recovery: Option<recovery::State>,
    errors: Errors,
    /// Something that went well and is worth saying, until dismissed.
    notice: Option<String>,
    /// Addons of the selected client that were installed without the
    /// launcher and have just been recognized as catalog entries.
    recognized: Vec<Adoption>,
    /// Whether the list of recognized addons is open.
    details: bool,
    transfer: transfer::State,
}

/// The state of the client folder being shown.
enum Selected {
    Nothing,
    Opening(PathBuf),
    /// The folder is not a usable client; the banner says why.
    Failed(PathBuf),
    Open {
        installation: Installation,
        /// `None` until the first scan finishes; later scans keep showing the
        /// previous snapshot until theirs arrives.
        snapshot: Option<Snapshot>,
    },
}

/// Everything that can happen in the launcher.
#[derive(Debug, Clone)]
pub enum Message {
    /// An interaction with the add-client form.
    Setup(setup::Message),
    /// An interaction with the screen for an unreadable config.
    Recovery(recovery::Message),
    /// An interaction with the addons.
    Addons(addons::Message),
    /// An export or import.
    Transfer(transfer::Message),
    /// A client folder was chosen in the picker.
    Select(PathBuf),
    /// The selected client folder was checked.
    Opened(PathBuf, Result<Installation, String>),
    /// A client folder was scanned.
    Scanned(PathBuf, Result<Snapshot, String>),
    /// The published catalog was fetched.
    CatalogLoaded(Result<Catalog, String>),
    /// Addons installed without the launcher were recognized as these
    /// catalog entries.
    Adopted(Result<Vec<Adoption>, String>),
    /// Show the add-client form.
    AddClient,
    /// Forget the selected client folder (its files are left alone).
    RemoveClient,
    /// Open `Interface/AddOns` in the file manager.
    OpenAddOns,
    /// Scan the selected client again.
    Refresh,
    /// Open the launcher's repository in the browser.
    OpenRepository,
    /// The config was written.
    Saved(Result<(), String>),
    /// Hide the error banner.
    DismissError,
    /// Hide the notice banner.
    DismissNotice,
    /// List the recognized addons.
    ShowRecognized,
    /// Close the list of recognized addons.
    CloseRecognized,
    /// Hide the banner about recognized addons.
    DismissRecognized,
    /// Escape was pressed.
    Escape,
    /// Ctrl+F was pressed.
    FocusSearch,
    /// Ctrl+Q was pressed.
    Quit,
}

impl App {
    /// The smallest window everything still fits in: the toolbar's search
    /// field and dropdowns side by side, and a card's buttons beside its name.
    pub const MIN_SIZE: iced::Size = iced::Size::new(900.0, 600.0);

    /// Starts the launcher: loads the config, opens the last client, and
    /// shows the built-in catalog while fetching the published one. A config
    /// that can't be read stops everything at the recovery screen instead.
    ///
    /// The config lives at `$OLD_LAUNCHER_CONFIG` when set, and in the user's
    /// config directory otherwise.
    pub fn new() -> (Self, Task<Message>) {
        // Fails only when a provider is already installed, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();

        let config_path = std::env::var_os("OLD_LAUNCHER_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                dirs::config_dir()
                    .unwrap_or_default()
                    .join("old_launcher")
                    .join("config.toml")
            });

        let mut errors = Errors::default();

        let (config, recovery) = match Config::load(&config_path) {
            Ok(config) => (config, None),
            Err(error) => (
                Config::default(),
                Some(recovery::State::new(config_path.clone(), &error)),
            ),
        };

        let client = reqwest::Client::builder()
            .user_agent(concat!("old_launcher/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| describe(&error));
        if let Err(error) = &client {
            errors.report(addons::unavailable(error));
        }

        // Only a broken build has a catalog that doesn't parse; the published
        // one may still load.
        let catalog = catalog::embedded().unwrap_or_else(|error| {
            errors.report(format!("The built-in catalog is unreadable: {error}"));
            Catalog::default()
        });

        let mut app = Self {
            config_path,
            config,
            client,
            catalog,
            refresh: catalog::Refresh::Unavailable,
            selected: Selected::Nothing,
            addons: addons::State::new(),
            setup: None,
            recovery,
            errors,
            notice: None,
            recognized: Vec::new(),
            details: false,
            transfer: transfer::State::default(),
        };

        let start = if app.recovery.is_some() {
            Task::none()
        } else {
            app.start()
        };

        (app, start)
    }

    /// Gets going with a readable config: opens the last client (or asks
    /// for one) and fetches the published catalog.
    fn start(&mut self) -> Task<Message> {
        let last = self
            .config
            .selected
            .clone()
            .filter(|path| self.config.installations.contains(path))
            .or_else(|| self.config.installations.first().cloned());

        let open = match last {
            Some(path) => self.open(path),
            None => {
                self.setup = Some(setup::State::new(false));
                operation::focus(setup::PATH)
            }
        };

        Task::batch([self.refresh_catalog(), open])
    }

    /// The window title.
    pub fn title(&self) -> String {
        "Old Launcher".to_owned()
    }

    /// The launcher's theme.
    pub fn theme(&self) -> Theme {
        style::theme()
    }

    /// Applies a message.
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Setup(message) => {
                let Some(setup) = &mut self.setup else {
                    return Task::none();
                };
                let action = setup.update(message);
                let task = action.task.map(Message::Setup);

                match action.instruction {
                    Some(setup::Instruction::Added(installation)) => {
                        let root = installation.root().to_path_buf();

                        if !self.config.installations.contains(&root) {
                            self.config.installations.push(root.clone());
                        }
                        self.config.selected = Some(root);
                        self.selected = Selected::Open {
                            installation,
                            snapshot: None,
                        };
                        self.setup = None;
                        self.recognized.clear();

                        Task::batch([task, self.rescan(), self.save()])
                    }
                    Some(setup::Instruction::Cancelled) => {
                        self.setup = None;
                        task
                    }
                    None => task,
                }
            }
            Message::Recovery(message) => {
                let Some(recovery) = &mut self.recovery else {
                    return Task::none();
                };
                let action = recovery.update(message, &mut self.errors);
                let task = action.task.map(Message::Recovery);

                match action.instruction {
                    Some(recovery::Instruction::Loaded(config)) => {
                        self.recovery = None;
                        self.config = config;
                        Task::batch([task, self.start()])
                    }
                    Some(recovery::Instruction::Overwrite) => {
                        self.recovery = None;
                        self.config = Config::default();
                        Task::batch([task, self.save(), self.start()])
                    }
                    Some(recovery::Instruction::Quit) => iced::exit(),
                    None => task,
                }
            }
            Message::Addons(message) => {
                let (installation, snapshot) = match &self.selected {
                    Selected::Open {
                        installation,
                        snapshot,
                    } => (Some(installation), snapshot.as_ref()),
                    _ => (None, None),
                };
                let action = self.addons.update(
                    message,
                    &self.catalog,
                    self.client.as_ref().map_err(String::as_str),
                    installation,
                    snapshot,
                    &mut self.errors,
                );
                let task = action.task.map(Message::Addons);

                match action.instruction {
                    Some(addons::Instruction::Changed) => Task::batch([task, self.rescan()]),
                    Some(addons::Instruction::Reload) => {
                        Task::batch([task, self.refresh_catalog()])
                    }
                    None => task,
                }
            }
            Message::Select(path) => {
                self.config.selected = Some(path.clone());
                Task::batch([self.open(path), self.save()])
            }
            Message::Opened(path, result) => {
                if !matches!(&self.selected, Selected::Opening(opening) if *opening == path) {
                    return Task::none();
                }

                match result {
                    Ok(installation) => {
                        self.selected = Selected::Open {
                            installation,
                            snapshot: None,
                        };
                        self.rescan()
                    }
                    Err(error) => {
                        self.errors
                            .report(format!("Could not open {}: {error}", path.display()));
                        self.selected = Selected::Failed(path);
                        Task::none()
                    }
                }
            }
            Message::Scanned(root, result) => {
                let Selected::Open {
                    installation,
                    snapshot,
                } = &mut self.selected
                else {
                    return Task::none();
                };
                if installation.root() != root {
                    return Task::none();
                }

                match result {
                    Ok(scanned) => {
                        *snapshot = Some(scanned);
                        self.adopt()
                    }
                    Err(error) => {
                        self.errors
                            .report(format!("Could not read the client: {error}"));
                        Task::none()
                    }
                }
            }
            Message::CatalogLoaded(result) => {
                self.refresh = catalog::Refresh::Idle;

                match result {
                    Ok(catalog) => {
                        self.catalog = catalog;
                        self.adopt()
                    }
                    Err(error) => {
                        self.errors.report(format!(
                            "Could not refresh the catalog, showing the one built into the \
                             launcher: {error}"
                        ));
                        Task::none()
                    }
                }
            }
            Message::Adopted(Ok(adopted)) => {
                if adopted.is_empty() {
                    return Task::none();
                }

                self.recognized.extend(adopted);
                self.recognized
                    .sort_by_cached_key(|adoption| adoption.name.to_lowercase());
                self.rescan()
            }
            Message::Adopted(Err(error)) => {
                self.errors
                    .report(format!("Could not mark recognized addons: {error}"));
                Task::none()
            }
            Message::AddClient => {
                self.setup = Some(setup::State::new(true));
                operation::focus(setup::PATH)
            }
            Message::RemoveClient => {
                let removed = match &self.selected {
                    Selected::Opening(path) | Selected::Failed(path) => path.clone(),
                    Selected::Open { installation, .. } => installation.root().to_path_buf(),
                    Selected::Nothing => return Task::none(),
                };

                self.config.installations.retain(|path| *path != removed);
                self.config.selected = self.config.installations.first().cloned();
                self.selected = Selected::Nothing;

                let open = match self.config.selected.clone() {
                    Some(path) => self.open(path),
                    None => {
                        self.setup = Some(setup::State::new(false));
                        Task::none()
                    }
                };

                Task::batch([open, self.save()])
            }
            Message::OpenAddOns => {
                let Selected::Open { installation, .. } = &self.selected else {
                    return Task::none();
                };
                let addons = installation.addons_directory();

                // A fresh client has no AddOns folder until the first install.
                let opened = std::fs::create_dir_all(&addons)
                    .map_err(|error| error.to_string())
                    .and_then(|()| open::that_detached(&addons).map_err(|error| error.to_string()));

                if let Err(error) = opened {
                    self.errors
                        .report(format!("Could not open {}: {error}", addons.display()));
                }

                Task::none()
            }
            Message::Refresh => self.rescan(),
            Message::OpenRepository => {
                if let Err(error) = open::that_detached(REPOSITORY) {
                    self.errors
                        .report(format!("Could not open {REPOSITORY}: {error}"));
                }

                Task::none()
            }
            Message::Saved(result) => {
                if let Err(error) = result {
                    self.errors
                        .report(format!("Could not save the settings: {error}"));
                }

                Task::none()
            }
            Message::DismissError => {
                self.errors.clear();
                Task::none()
            }
            Message::ShowRecognized => {
                self.details = true;
                Task::none()
            }
            Message::CloseRecognized => {
                self.details = false;
                Task::none()
            }
            Message::DismissRecognized => {
                self.recognized.clear();
                self.details = false;
                Task::none()
            }
            Message::DismissNotice => {
                self.notice = None;
                Task::none()
            }
            Message::Transfer(message) => {
                let (installation, snapshot) = match &self.selected {
                    Selected::Open {
                        installation,
                        snapshot,
                    } => (Some(installation), snapshot.as_ref()),
                    _ => (None, None),
                };
                let action = self.transfer.update(
                    message,
                    &self.catalog,
                    installation,
                    snapshot,
                    &mut self.errors,
                );
                let task = action.task.map(Message::Transfer);

                match action.instruction {
                    Some(transfer::Instruction::Exported { path, summary }) => {
                        self.notice = Some(format!(
                            "Exported {} and {} to {}.",
                            counted(summary.addons, "addon"),
                            counted(summary.files, "saved settings file"),
                            path.display()
                        ));
                        task
                    }
                    Some(transfer::Instruction::Imported { restored, install }) => {
                        let restored = counted(restored, "saved settings file");
                        self.notice = Some(if install.is_empty() {
                            format!("Restored {restored}.")
                        } else {
                            format!(
                                "Restored {restored}, and installing {}: each card shows its \
                                 progress.",
                                counted(install.len(), "addon")
                            )
                        });

                        let installs: Vec<Task<Message>> = install
                            .iter()
                            .map(|entry| {
                                self.update(Message::Addons(addons::Message::Install(
                                    addons::Key::of(entry),
                                )))
                            })
                            .collect();

                        Task::batch([task, self.rescan()].into_iter().chain(installs))
                    }
                    None => task,
                }
            }
            Message::Escape => {
                if self.setup.is_some() {
                    self.update(Message::Setup(setup::Message::Cancel))
                } else if self.details {
                    self.update(Message::CloseRecognized)
                } else if self.transfer.view().is_some() {
                    self.update(Message::Transfer(transfer::Message::Cancel))
                } else {
                    self.update(Message::Addons(addons::Message::Cancel))
                }
            }
            Message::FocusSearch => operation::focus(addons::SEARCH),
            Message::Quit => iced::exit(),
        }
    }

    /// Draws the launcher.
    pub fn view(&self) -> Element<'_, Message> {
        if let Some(recovery) = &self.recovery {
            return column![]
                .push(self.errors.view(Message::DismissError))
                .push(center(scrollable(recovery.view().map(Message::Recovery))))
                .spacing(14)
                .padding(20)
                .into();
        }

        if self.config.installations.is_empty()
            && let Some(setup) = &self.setup
        {
            return center(setup.view().map(Message::Setup)).padding(20).into();
        }

        let current = match &self.selected {
            Selected::Opening(path) | Selected::Failed(path) => Some(path.clone()),
            Selected::Open { installation, .. } => Some(installation.root().to_path_buf()),
            Selected::Nothing => None,
        };

        let title = button(text("Old Launcher").size(24).font(style::BOLD))
            .padding(0)
            .on_press(Message::OpenRepository)
            .style(|theme: &Theme, status| {
                let palette = theme.palette();

                button::Style {
                    text_color: match status {
                        button::Status::Hovered | button::Status::Pressed => {
                            palette.primary.strong.color
                        }
                        button::Status::Active | button::Status::Disabled => {
                            palette.primary.base.color
                        }
                    },
                    ..button::Style::default()
                }
            });

        let header = row![
            tooltip(
                title,
                container(text(REPOSITORY).size(12))
                    .padding([4, 8])
                    .style(container::rounded_box),
                tooltip::Position::Bottom,
            ),
            space::horizontal(),
            // The built-in arrow is drawn over the end of an ellipsized label,
            // so the label keeps clear of the right edge and the same arrow
            // glyph is layered on top instead.
            stack![
                pick_list(
                    current.clone(),
                    self.config.installations.as_slice(),
                    |path: &PathBuf| path.display().to_string(),
                )
                .placeholder("Select a client")
                .on_select(Message::Select)
                .ellipsis(text::Ellipsis::Middle)
                .handle(pick_list::Handle::None)
                .padding(padding::all(5).left(10).right(32))
                .width(Fill),
                container(text('\u{e800}').font(Font::new("Iced-Icons")))
                    .align_right(Fill)
                    .center_y(Fill)
                    .padding(padding::right(10)),
            ]
            .width(Fill.max(460)),
            button("Add client…")
                .style(button::secondary)
                .on_press(Message::AddClient),
            button("Remove")
                .style(button::text)
                .on_press_maybe(current.map(|_| Message::RemoveClient)),
        ]
        .spacing(10)
        .align_y(Center);

        // Exports and imports work from what was last scanned.
        let snapshot_ready = matches!(
            &self.selected,
            Selected::Open {
                snapshot: Some(_),
                ..
            }
        );

        let client: Element<'_, Message> = match &self.selected {
            Selected::Nothing => text("No client selected.").style(text::secondary).into(),
            Selected::Opening(path) => text(format!("Opening {}…", path.display()))
                .style(text::secondary)
                .into(),
            Selected::Failed(path) => text(format!("{} is not a usable client.", path.display()))
                .style(text::danger)
                .into(),
            Selected::Open { installation, .. } => row![style::badge(
                installation.version().to_string(),
                style::Tone::Primary
            )]
            .push(
                installation
                    .locale()
                    .map(|locale| style::badge(locale, style::Tone::Primary)),
            )
            .push(
                text(installation.root().display().to_string())
                    .size(13)
                    .style(text::secondary)
                    .wrapping(text::Wrapping::None)
                    .ellipsis(text::Ellipsis::Middle)
                    .width(Fill),
            )
            .push(
                button("Refresh")
                    .style(button::text)
                    .on_press(Message::Refresh),
            )
            .push(
                button("Open AddOns folder")
                    .style(button::text)
                    .on_press(Message::OpenAddOns),
            )
            .push(style::tip(
                button("Export…").style(button::text).on_press_maybe(
                    (snapshot_ready && !self.transfer.busy())
                        .then_some(Message::Transfer(transfer::Message::Export)),
                ),
                "Saves the list of addons and their saved settings to a zip file, to \
                 import on another computer.",
            ))
            .push(style::tip(
                button("Import…").style(button::text).on_press_maybe(
                    (snapshot_ready && !self.transfer.busy())
                        .then_some(Message::Transfer(transfer::Message::Import)),
                ),
                "Installs the addons listed in an export and restores their saved \
                 settings, after showing what it will do.",
            ))
            .spacing(8)
            .align_y(Center)
            .into(),
        };

        let error = self.errors.view(Message::DismissError);

        let notice = self.notice.as_deref().map(|notice| {
            container(
                row![
                    text(notice).width(Fill),
                    button("Dismiss")
                        .style(button::text)
                        .on_press(Message::DismissNotice),
                ]
                .spacing(8)
                .align_y(Center),
            )
            .padding([6, 12])
            .style(style::banner(style::Tone::Success))
        });

        // Adoption only adds a marker file to folders that were already
        // there, so there is nothing to do about it; the banner just says so.
        let recognized = (!self.recognized.is_empty()).then(|| {
            let notice = match self.recognized.as_slice() {
                [one] => format!(
                    "{} was already installed and matches the catalog, so it now shows as \
                     installed. Nothing else changed.",
                    one.name
                ),
                all => format!(
                    "{} addons already installed match the catalog, so they now show as \
                     installed. Nothing else changed.",
                    all.len()
                ),
            };

            container(
                row![
                    text(notice).width(Fill),
                    button("Details")
                        .style(button::secondary)
                        .on_press(Message::ShowRecognized),
                    button("Dismiss")
                        .style(button::text)
                        .on_press(Message::DismissRecognized),
                ]
                .spacing(8)
                .align_y(Center),
            )
            .padding([6, 12])
            .style(style::banner(style::Tone::Success))
        });

        let (installation, snapshot) = match &self.selected {
            Selected::Open {
                installation,
                snapshot,
            } => (Some(installation), snapshot.as_ref()),
            _ => (None, None),
        };

        let body = self
            .addons
            .view(&self.catalog, installation, snapshot, self.refresh)
            .map(Message::Addons);

        // The addons are always the second child, whatever the banners above
        // do, so they keep their widget state (the scroll position) when the
        // banners come and go. A keyed column can't be trusted with this:
        // iced's keyed diff mistakes a child inserted before the last one for
        // one appended after it, and hands out the wrong widget states.
        let content = column![
            column![header, client]
                .push(error)
                .push(notice)
                .push(recognized)
                .spacing(14),
            container(body).height(Fill),
        ]
        .spacing(14)
        .padding(20);

        let (dialog, on_blur) = match &self.setup {
            Some(setup) => (
                Some(setup.view().map(Message::Setup)),
                Message::Setup(setup::Message::Cancel),
            ),
            None if self.details => (Some(self.recognized_view()), Message::CloseRecognized),
            None => (
                self.transfer
                    .view()
                    .map(|dialog| dialog.map(Message::Transfer)),
                Message::Transfer(transfer::Message::Cancel),
            ),
        };

        style::modal(content, dialog, on_blur)
    }

    /// Every recognized addon, with the source it is now recorded as
    /// installed from and its folders.
    fn recognized_view(&self) -> Element<'_, Message> {
        let addons = self.recognized.iter().map(|adoption| {
            let source = self
                .catalog
                .source(&adoption.marker.source)
                .map_or(adoption.marker.source.as_str(), |source| {
                    source.name.as_str()
                });

            column![
                row![
                    text(&adoption.name).font(style::BOLD).width(Fill),
                    text(source).size(13).style(text::secondary),
                ]
                .spacing(8),
                text(format!("Folders: {}", adoption.folders.join(", ")))
                    .size(12)
                    .style(text::secondary),
            ]
            .spacing(2)
            .into()
        });

        container(
            column![
                text("Recognized addons").size(20).font(style::BOLD),
                text(
                    "These addons were in Interface/AddOns before the launcher, and their \
                     folders match a catalog entry exactly. They are now listed as installed \
                     from that entry, so they can be reinstalled and uninstalled like any \
                     other. Their files were left as they were: each folder only got a small \
                     .old_launcher.toml file recording where it comes from."
                )
                .style(text::secondary),
                container(
                    scrollable(column(addons).spacing(10).padding(padding::right(14)))
                        .height(Fit.max(360))
                        .width(Fill)
                )
                .padding(10)
                .width(Fill)
                .style(container::rounded_box),
                row![
                    space::horizontal(),
                    button("Close")
                        .style(button::primary)
                        .on_press(Message::CloseRecognized),
                ],
            ]
            .spacing(14),
        )
        .padding(24)
        .width(Fill.max(620))
        .style(style::card)
        .into()
    }

    /// Keyboard shortcuts; see [`shortcut`].
    pub fn subscription(&self) -> Subscription<Message> {
        keyboard::listen().filter_map(shortcut)
    }

    /// Fetches the published catalog, if there is one to fetch.
    fn refresh_catalog(&mut self) -> Task<Message> {
        let Some(location) = catalog::Location::configured() else {
            self.refresh = catalog::Refresh::Unavailable;
            return Task::none();
        };

        self.refresh = catalog::Refresh::Running;

        Task::perform(location.load(self.client.clone()), Message::CatalogLoaded)
    }

    /// Marks the selected client's addons that were installed without the
    /// launcher but match a catalog entry exactly.
    fn adopt(&self) -> Task<Message> {
        let Selected::Open {
            installation,
            snapshot: Some(snapshot),
        } = &self.selected
        else {
            return Task::none();
        };

        let adoptions = Adoption::plan(snapshot, &self.catalog.entries);

        if adoptions.is_empty() {
            return Task::none();
        }

        let installation = installation.clone();

        Task::perform(
            blocking(move || {
                let mut adopted = Vec::new();

                for adoption in adoptions {
                    if installation.adopt(&adoption)? > 0 {
                        adopted.push(adoption);
                    }
                }

                Ok(adopted)
            }),
            Message::Adopted,
        )
    }

    /// Starts checking the client at `path`.
    fn open(&mut self, path: PathBuf) -> Task<Message> {
        self.selected = Selected::Opening(path.clone());
        self.recognized.clear();
        self.details = false;

        Task::perform(
            blocking({
                let path = path.clone();
                move || Installation::open(path)
            }),
            Message::Opened.with(path),
        )
    }

    /// Reads the selected client's addons and data again.
    fn rescan(&self) -> Task<Message> {
        let Selected::Open { installation, .. } = &self.selected else {
            return Task::none();
        };
        let installation = installation.clone();
        let root = installation.root().to_path_buf();

        Task::perform(
            blocking(move || installation.scan()),
            Message::Scanned.with(root),
        )
    }

    /// Writes the config.
    fn save(&self) -> Task<Message> {
        let config = self.config.clone();
        let path = self.config_path.clone();

        Task::perform(blocking(move || config.save(&path)), Message::Saved)
    }
}

/// What a key press does: Escape closes dialogs, F5 rescans, Ctrl+F
/// searches and Ctrl+Q quits (Cmd on macOS).
fn shortcut(event: keyboard::Event) -> Option<Message> {
    let keyboard::Event::KeyPressed { key, modifiers, .. } = event else {
        return None;
    };

    match key.as_ref() {
        Key::Named(Named::Escape) => Some(Message::Escape),
        Key::Named(Named::F5) => Some(Message::Refresh),
        Key::Character("f") if modifiers.command() => Some(Message::FocusSearch),
        Key::Character("q") if modifiers.command() => Some(Message::Quit),
        _ => None,
    }
}

/// `count` of `noun`, in the plural when it isn't one: `1 addon`, `3 addons`.
fn counted(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("1 {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// Runs filesystem work on a blocking thread, so the UI never waits on disk.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, core::Error> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| describe(&error))
}

/// An error and its chain of causes, for people.
fn describe(error: &dyn std::error::Error) -> String {
    let mut description = error.to_string();
    let mut source = error.source();

    while let Some(cause) = source {
        let message = cause.to_string();

        // Some errors repeat their source in their own message.
        if !description.contains(&message) {
            description.push_str(": ");
            description.push_str(&message);
        }
        source = cause.source();
    }

    description
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_test::simulator;

    fn app(installations: &[&str]) -> App {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let installations: Vec<PathBuf> = installations.iter().map(PathBuf::from).collect();

        App {
            config_path: PathBuf::from("config.toml"),
            config: Config {
                selected: installations.first().cloned(),
                installations,
            },
            client: Ok(reqwest::Client::new()),
            catalog: Catalog::default(),
            refresh: catalog::Refresh::Unavailable,
            selected: Selected::Nothing,
            addons: addons::State::new(),
            setup: None,
            recovery: None,
            errors: Errors::default(),
            notice: None,
            recognized: Vec::new(),
            details: false,
            transfer: transfer::State::default(),
        }
    }

    /// The app as it starts with an unreadable config at `path`, holding
    /// `text`.
    fn unreadable(path: &std::path::Path, text: &str) -> App {
        std::fs::write(path, text).unwrap();
        let error = Config::load(path).unwrap_err();

        App {
            config_path: path.to_path_buf(),
            recovery: Some(recovery::State::new(path.to_path_buf(), &error)),
            ..app(&[])
        }
    }

    #[test]
    fn an_unreadable_config_stops_everything_until_the_user_decides() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let mut app = unreadable(&path, "installations = [\"/games/wotlk\"");

        let mut ui = simulator(app.view());
        assert!(ui.find("The settings file can't be read").is_ok());
        assert!(ui.find("Add client…").is_err(), "nothing else is shown");
        drop(ui);

        // Overwriting starts afresh: no clients, so the add-client form.
        let _ = app.update(Message::Recovery(recovery::Message::Overwrite));
        assert!(app.recovery.is_none());
        assert_eq!(app.config, Config::default());
        let mut ui = simulator(app.view());
        assert!(ui.find("Add a WoW 3.3.5a client").is_ok());
    }

    #[test]
    fn ctrl_q_quits_and_a_plain_q_does_not() {
        let press = |modifiers| keyboard::Event::KeyPressed {
            key: Key::Character("q".into()),
            modified_key: Key::Character("q".into()),
            physical_key: keyboard::key::Physical::Code(keyboard::key::Code::KeyQ),
            location: keyboard::Location::Standard,
            modifiers,
            text: None,
            repeat: false,
        };

        assert!(matches!(
            shortcut(press(keyboard::Modifiers::COMMAND)),
            Some(Message::Quit)
        ));
        assert!(shortcut(press(keyboard::Modifiers::empty())).is_none());
    }

    #[test]
    fn the_title_links_to_the_repository() {
        let app = app(&[]);

        let mut ui = simulator(app.view());
        let _ = ui.click("Old Launcher").unwrap();

        assert!(matches!(
            ui.into_messages().next(),
            Some(Message::OpenRepository)
        ));
        assert_eq!(REPOSITORY, "https://gitlab.com/juxuanu/old_launcher");
    }

    #[test]
    fn an_import_restores_the_settings_then_installs_its_addons() {
        use crate::core::transfer::{Import, Manifest, Plan};

        let (_root, installation) = crate::testing::client(&[]);
        let mut app = app(&[&installation.root().display().to_string()]);
        app.catalog = Catalog::parse(
            br#"{ "version": 2,
                  "sources": [{ "id": "test", "name": "Test", "homepage": "https://example.com" }],
                  "entries": [{ "name": "Addon Control Panel",
                                "downloads": [{ "source": "test", "name": "ACP",
                                                "url": "https://example.com/ACP.zip",
                                                "folders": [{ "name": "ACP" }] }] }] }"#,
        )
        .unwrap();
        app.selected = Selected::Open {
            snapshot: Some(installation.scan().unwrap()),
            installation,
        };
        let import = Import {
            path: PathBuf::from("/exports/old_launcher-addons.zip"),
            manifest: Manifest {
                version: Manifest::VERSION,
                addons: Vec::new(),
            },
            files: Vec::new(),
        };
        let plan = Plan {
            install: app.catalog.entries.clone(),
            ..Plan::default()
        };

        let mut ui = simulator(app.view());
        assert!(ui.find("Install").is_ok());
        drop(ui);

        for message in [
            transfer::Message::Read(Ok((import, plan))),
            transfer::Message::Confirm,
            transfer::Message::Restored(Ok(1)),
        ] {
            let _ = app.update(Message::Transfer(message));
        }

        assert_eq!(
            app.notice.as_deref(),
            Some(
                "Restored 1 saved settings file, and installing 1 addon: each card shows its \
                 progress."
            )
        );
        let mut ui = simulator(app.view());
        assert!(ui.find("Install").is_err(), "the install is under way");
        assert!(ui.find("Test: 0 KB").is_ok());
    }

    #[test]
    fn exports_and_imports_wait_for_the_client_to_be_read() {
        let (_root, installation) = crate::testing::client(&[]);
        let snapshot = installation.scan().unwrap();
        let mut app = app(&[&installation.root().display().to_string()]);
        app.selected = Selected::Open {
            snapshot: None,
            installation: installation.clone(),
        };

        let mut ui = simulator(app.view());
        let _ = ui.click("Export…");
        assert_eq!(ui.into_messages().count(), 0, "still reading the client");

        app.selected = Selected::Open {
            snapshot: Some(snapshot),
            installation,
        };
        let mut ui = simulator(app.view());
        let _ = ui.click("Export…").unwrap();
        assert!(matches!(
            ui.into_messages().next(),
            Some(Message::Transfer(transfer::Message::Export))
        ));
    }

    #[test]
    fn a_config_fixed_by_hand_is_used_once_read_again() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let mut app = unreadable(&path, "installations = [\"/games/wotlk\"");

        std::fs::write(&path, "installations = [\"/games/wotlk\"]\n").unwrap();
        let _ = app.update(Message::Recovery(recovery::Message::Retry));

        assert!(app.recovery.is_none());
        assert_eq!(app.config.installations, [PathBuf::from("/games/wotlk")]);
        assert!(
            matches!(&app.selected, Selected::Opening(path) if path == &PathBuf::from("/games/wotlk"))
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "installations = [\"/games/wotlk\"]\n",
            "the fixed file is left as the user wrote it"
        );
    }

    #[test]
    fn the_client_picker_lists_client_paths() {
        let mut app = app(&["/games/wotlk", "/games/warmane"]);
        app.selected = Selected::Opening("/games/wotlk".into());

        let mut ui = simulator(app.view());
        let _ = ui.click("/games/wotlk").unwrap();

        assert!(ui.find("/games/warmane").is_ok());
    }

    #[test]
    fn a_client_finishing_to_open_redraws_cleanly() {
        let (_root, installation) = crate::testing::client(&[]);
        let path = installation.root().display().to_string();

        let mut opening = app(&[&path]);
        opening.selected = Selected::Opening(installation.root().to_path_buf());
        let mut open = app(&[&path]);
        open.selected = Selected::Open {
            snapshot: Some(installation.scan().unwrap()),
            installation,
        };

        // What happens right after every launch: the line under the header
        // goes from "Opening …" to the client's badges and buttons.
        let ui = simulator(opening.view());
        let mut ui = ui.rebuild(open.view());

        assert!(ui.find("Open AddOns folder").is_ok());
    }

    #[test]
    fn banners_coming_and_going_keep_the_addons_scrolled_where_they_were() {
        let tocs: Vec<String> = (0..30)
            .map(|index| format!("Interface/AddOns/Addon{index:02}/Addon{index:02}.toc"))
            .collect();
        let (_root, installation) =
            crate::testing::client(&tocs.iter().map(String::as_str).collect::<Vec<_>>());
        let path = installation.root().display().to_string();
        let snapshot = installation.scan().unwrap();
        let open = |error: Option<&str>, recognized: bool| {
            let mut app = app(&[&path]);
            if let Some(error) = error {
                app.errors.report(error);
            }
            if recognized {
                app.recognized = vec![Adoption {
                    name: "Addon07".into(),
                    marker: crate::core::Marker {
                        source: "test".into(),
                        entry: "Addon07".into(),
                        url: "https://example.com/Addon07.zip".into(),
                    },
                    folders: vec!["Addon07".into()],
                }];
            }
            app.selected = Selected::Open {
                installation: installation.clone(),
                snapshot: Some(snapshot.clone()),
            };
            app
        };

        let plain = open(None, false);
        let states = [
            open(Some("Could not save the settings"), false),
            open(Some("Could not save the settings"), true),
            open(None, true),
            open(None, false),
        ];

        let mut ui = iced_test::simulator::Simulator::with_size(
            iced::Settings::default(),
            (1000.0, 700.0),
            plain.view(),
        );
        ui.point_at(iced::Point::new(500.0, 500.0));
        let _ = ui.scroll(iced::mouse::ScrollDelta::Pixels { x: 0.0, y: -600.0 });
        let scrolled = crate::testing::offsets(&mut ui);
        assert!(scrolled[0] > 0.0, "the list scrolled down: {scrolled:?}");

        for (step, app) in states.iter().enumerate() {
            ui = ui.rebuild(app.view());
            assert_eq!(crate::testing::offsets(&mut ui), scrolled, "step {step}");
        }
    }

    #[test]
    fn recognized_addons_are_announced_and_listed_on_request() {
        let (_root, installation) = crate::testing::client(&["Interface/AddOns/ACP/ACP.toc"]);
        let mut app = app(&[&installation.root().display().to_string()]);
        app.catalog = Catalog::parse(
            br#"{ "version": 2,
                  "sources": [{ "id": "nom0re", "name": "NoM0Re", "homepage": "https://example.com" }],
                  "entries": [{ "name": "Addon Control Panel",
                                "downloads": [{ "source": "nom0re", "name": "ACP",
                                                "url": "https://example.com/ACP.zip",
                                                "folders": [{ "name": "ACP" }] }] }] }"#,
        )
        .unwrap();
        let snapshot = installation.scan().unwrap();
        let adoptions = Adoption::plan(&snapshot, &app.catalog.entries);
        app.selected = Selected::Open {
            installation,
            snapshot: Some(snapshot),
        };

        let _ = app.update(Message::Adopted(Ok(adoptions)));

        let mut ui = simulator(app.view());
        assert!(
            ui.find(
                "Addon Control Panel was already installed and matches the catalog, so it now \
                 shows as installed. Nothing else changed."
            )
            .is_ok()
        );
        let _ = ui.click("Details").unwrap();
        for message in ui.into_messages() {
            let _ = app.update(message);
        }

        let mut ui = simulator(app.view());
        assert!(ui.find("Recognized addons").is_ok());
        assert!(ui.find("NoM0Re").is_ok(), "the source it is recorded under");
        drop(ui);

        let _ = app.update(Message::Escape);
        assert!(!app.details, "Escape closes the list");
        assert_eq!(app.recognized.len(), 1, "the banner stays until dismissed");
    }
}
