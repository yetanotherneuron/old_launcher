//! Old Launcher: starts WoW 3.3.5a clients and manages their addons.
//!
//! The app is a thin shell over [`core`]: it remembers which client folders
//! the user added and how each one starts, lists the launcher's catalog of
//! addons alongside the client's, and runs every filesystem change on a
//! blocking thread.

pub use old_launcher_core as core;

mod action;
mod addons;
mod catalog;
mod clients;
mod errors;
mod game;
mod recognize;
mod recovery;
#[cfg(test)]
mod screenshots;
mod setup;
mod style;
#[cfg(test)]
mod testing;
mod transfer;
mod update;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{
    button, center, column, container, operation, row, scrollable, space, text, tooltip,
};
use iced::{Center, Element, Event, Fill, Function, Subscription, Task, Theme, event, window};

use crate::catalog::Catalog;
use crate::core::{Config, Installation, Snapshot};
use crate::errors::Errors;

/// Where the launcher's code lives, as `Cargo.toml` says.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// How long a refresh leaves the catalog alone after fetching it.
const REFRESH_PAUSE: Duration = Duration::from_secs(10);

/// The launcher's icon, rendered from `assets/old_launcher.svg` and decoded
/// when building.
const ICON: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon.rgba"));
include!(concat!(env!("OUT_DIR"), "/icon_size.rs"));

/// The window's icon, for the title bar, taskbar and window switcher on
/// Windows and X11. Wayland desktops take it from the desktop entry.
pub fn icon() -> Option<iced::window::Icon> {
    iced::window::icon::from_rgba(ICON.to_vec(), ICON_WIDTH, ICON_HEIGHT).ok()
}

/// The launcher.
pub struct App {
    config_path: PathBuf,
    config: Config,
    /// What downloads go through, or why there is nothing to download with.
    client: Result<reqwest::Client, String>,
    catalog: Catalog,
    refresh: catalog::Refresh,
    /// When the catalog was last fetched, so a refresh doesn't fetch it
    /// again right away.
    refreshed: Option<Instant>,
    selected: Selected,
    addons: addons::State,
    setup: Option<setup::State>,
    /// The config couldn't be read: nothing else happens until the user
    /// decides what to do about it.
    recovery: Option<recovery::State>,
    errors: Errors,
    /// Something that went well and is worth saying, until dismissed.
    notice: Option<String>,
    /// Addons of the selected client installed without the launcher that
    /// match the catalog, proposed for recognition.
    recognize: recognize::State,
    transfer: transfer::State,
    clients: clients::State,
    /// The games started, and the selected client's when found running.
    game: game::State,
    /// A newer launcher version than this one, when published.
    latest: Option<String>,
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

impl Selected {
    /// The client folder, whether or not it opened.
    fn path(&self) -> Option<&Path> {
        match self {
            Self::Nothing => None,
            Self::Opening(path) | Self::Failed(path) => Some(path),
            Self::Open { installation, .. } => Some(installation.root()),
        }
    }

    /// The client, once it opened, and what was last scanned in it.
    fn open(&self) -> (Option<&Installation>, Option<&Snapshot>) {
        match self {
            Self::Open {
                installation,
                snapshot,
            } => (Some(installation), snapshot.as_ref()),
            _ => (None, None),
        }
    }
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
    /// An interaction with the clients dialog.
    Clients(clients::Message),
    /// An interaction with the proposal to recognize addons.
    Recognize(recognize::Message),
    /// Something happened to a game.
    Game(game::Message),
    /// Open the clients dialog.
    ManageClients,
    /// Start the game.
    Play,
    /// The launcher's window got the focus back.
    Focused,
    /// The selected client folder was checked.
    Opened(PathBuf, Result<Installation, String>),
    /// A client folder was scanned.
    Scanned(PathBuf, Result<Snapshot, String>),
    /// The published catalog was fetched.
    CatalogLoaded(Result<Catalog, String>),
    /// Whether a newer launcher is published was checked.
    UpdateChecked(Option<String>),
    /// Open `Interface/AddOns` in the file manager.
    OpenAddOns,
    /// Scan the selected client again.
    Refresh,
    /// Open the launcher's repository in the browser.
    OpenRepository,
    /// Open the launcher's page, with the downloads, in the browser.
    OpenPage,
    /// The config was written.
    Saved(Result<(), String>),
    /// Hide the error banner.
    DismissError,
    /// Hide the notice banner.
    DismissNotice,
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

        log::info!(
            "Old Launcher {} starting, with the settings at {}",
            update::VERSION,
            config_path.display()
        );

        let mut errors = Errors::default();

        let (config, recovery) = match Config::load(&config_path) {
            Ok(config) => {
                log::info!(
                    "The settings list {}",
                    counted(config.installations.len(), "client")
                );
                (config, None)
            }
            Err(error) => {
                log::warn!("The settings can't be read: {}", describe(&error));
                (
                    Config::default(),
                    Some(recovery::State::new(config_path.clone(), &error)),
                )
            }
        };

        // A connection that never comes, or a server that stops sending,
        // ends the request rather than hanging the catalog refresh or a
        // download forever. A slow but flowing download is fine.
        let client = reqwest::Client::builder()
            .user_agent(concat!("old_launcher/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
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
            refreshed: None,
            selected: Selected::Nothing,
            addons: addons::State::new(),
            setup: None,
            recovery,
            errors,
            notice: None,
            recognize: recognize::State::default(),
            transfer: transfer::State::default(),
            clients: clients::State::default(),
            game: game::State::default(),
            latest: None,
        };

        let start = if app.recovery.is_some() {
            Task::none()
        } else {
            app.start()
        };

        (app, start)
    }

    /// Gets going with a readable config: opens the last client (or asks
    /// for one), fetches the published catalog, and looks for a newer
    /// launcher.
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
                log::info!("No client yet: asking for one");
                self.setup = Some(setup::State::new(false));
                operation::focus(setup::PATH)
            }
        };

        Task::batch([
            self.refresh_catalog(),
            open,
            Task::perform(update::check(self.client.clone()), Message::UpdateChecked),
        ])
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
                        // Added from the clients dialog, which shows it next.
                        let read = if self.clients.is_open() {
                            self.clients.read(&root).map(Message::Clients)
                        } else {
                            Task::none()
                        };

                        self.config.selected = Some(root);
                        self.selected = Selected::Open {
                            installation,
                            snapshot: None,
                        };
                        self.setup = None;
                        self.recognize.clear();

                        Task::batch([task, read, self.rescan(), self.save()])
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
                let (installation, snapshot) = self.selected.open();
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
            Message::Clients(message) => {
                if !self.clients.is_open() {
                    return Task::none();
                }
                let current = self.selected.path().map(Path::to_path_buf);
                let action = self.clients.update(
                    message,
                    &mut self.config,
                    current.as_deref(),
                    &mut self.errors,
                );
                let task = action.task.map(Message::Clients);

                match action.instruction {
                    Some(clients::Instruction::Select(path)) => {
                        self.config.selected = Some(path.clone());
                        Task::batch([task, self.open(path), self.save()])
                    }
                    Some(clients::Instruction::Add) => {
                        self.setup = Some(setup::State::new(true));
                        Task::batch([task, operation::focus(setup::PATH)])
                    }
                    Some(clients::Instruction::Remove) => {
                        let Some(removed) = current else {
                            return task;
                        };
                        log::info!("Forgetting the client at {}", removed.display());
                        self.config.remove(&removed);
                        self.selected = Selected::Nothing;
                        self.recognize.clear();

                        let open = match self.config.selected.clone() {
                            Some(path) => {
                                let read = self.clients.read(&path).map(Message::Clients);
                                Task::batch([read, self.open(path)])
                            }
                            // Nothing left to manage: the first-run form.
                            None => {
                                self.clients.close();
                                self.setup = Some(setup::State::new(false));
                                operation::focus(setup::PATH)
                            }
                        };

                        Task::batch([task, open, self.save()])
                    }
                    Some(clients::Instruction::Close { edited: true }) => {
                        log::info!("The launch settings changed");
                        Task::batch([task, self.save()])
                    }
                    Some(clients::Instruction::Close { edited: false }) | None => task,
                }
            }
            Message::ManageClients => self
                .clients
                .open(self.selected.path())
                .map(Message::Clients),
            Message::Recognize(message) => {
                let (installation, _) = self.selected.open();
                let action = self
                    .recognize
                    .update(message, installation, &mut self.errors);
                let task = action.task.map(Message::Recognize);

                match action.instruction {
                    Some(recognize::Instruction::Recognized(count)) => {
                        self.notice = Some(format!(
                            "Recognized {}: now listed as installed.",
                            counted(count, "addon")
                        ));
                        Task::batch([task, self.rescan()])
                    }
                    None => task,
                }
            }
            Message::Play => {
                let Selected::Open { installation, .. } = &self.selected else {
                    return Task::none();
                };
                let launch = self.config.launch(installation.root());

                self.game
                    .play(installation, &launch, &mut self.errors)
                    .map(Message::Game)
            }
            Message::Game(message) => {
                let (installation, _) = self.selected.open();
                let selected = installation.map(Installation::root);

                self.game
                    .update(message, selected, &mut self.errors)
                    .map(Message::Game)
            }
            // Back from somewhere else, where a game may have started or a
            // realmlist been edited.
            Message::Focused => {
                log::trace!("The window got the focus back");
                let (installation, _) = self.selected.open();
                let selected = installation.map(Installation::root);

                Task::batch([
                    self.game.watch(selected).map(Message::Game),
                    self.update(Message::Clients(clients::Message::Focused)),
                ])
            }
            Message::Opened(path, result) => {
                if !matches!(&self.selected, Selected::Opening(opening) if *opening == path) {
                    return Task::none();
                }

                match result {
                    Ok(installation) => {
                        log::info!(
                            "Opened {}: version {}, {}",
                            installation.root().display(),
                            installation.version(),
                            installation
                                .locale()
                                .map_or("no language folder".to_owned(), |locale| {
                                    format!("language {locale}")
                                })
                        );
                        let watch = self
                            .game
                            .watch(Some(installation.root()))
                            .map(Message::Game);
                        self.selected = Selected::Open {
                            installation,
                            snapshot: None,
                        };
                        Task::batch([self.rescan(), watch])
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
                        log::info!(
                            "Scanned {}: {} in Interface/AddOns, {} with saved settings",
                            root.display(),
                            counted(scanned.addons.len(), "folder"),
                            counted(scanned.saved_variables.iter().count(), "addon")
                        );
                        *snapshot = Some(scanned);
                        self.propose();

                        // What the folder holds changed with its addons.
                        self.clients
                            .changed(&root, Some(&root))
                            .map(Message::Clients)
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
                        log::info!("Refreshed the catalog: {} addons", catalog.entries.len());
                        self.catalog = catalog;
                        self.propose();
                    }
                    Err(error) => {
                        self.errors.report(format!(
                            "Could not refresh the catalog, showing the one built into the \
                             launcher: {error}"
                        ));
                    }
                }

                Task::none()
            }
            Message::UpdateChecked(latest) => {
                self.latest = latest;
                Task::none()
            }
            Message::OpenAddOns => {
                let Selected::Open { installation, .. } = &self.selected else {
                    return Task::none();
                };
                let addons = installation.addons_directory();
                log::info!("Opening {} in the file manager", addons.display());

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
            Message::Refresh => {
                log::info!("Refreshing on request");
                Task::batch([self.rescan(), self.refresh_catalog()])
            }
            Message::OpenRepository => {
                log::info!("Opening {REPOSITORY} in the browser");
                if let Err(error) = open::that_detached(REPOSITORY) {
                    self.errors
                        .report(format!("Could not open {REPOSITORY}: {error}"));
                }

                Task::none()
            }
            Message::OpenPage => {
                log::info!("Opening {} in the browser", update::PAGE);
                if let Err(error) = open::that_detached(update::PAGE) {
                    self.errors
                        .report(format!("Could not open {}: {error}", update::PAGE));
                }

                Task::none()
            }
            Message::Saved(result) => {
                match result {
                    Ok(()) => log::debug!("Saved the settings"),
                    Err(error) => self
                        .errors
                        .report(format!("Could not save the settings: {error}")),
                }

                Task::none()
            }
            Message::DismissError => {
                self.errors.clear();
                Task::none()
            }
            Message::DismissNotice => {
                self.notice = None;
                Task::none()
            }
            Message::Transfer(message) => {
                let (installation, snapshot) = self.selected.open();
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
                } else if self.clients.is_open() {
                    self.update(Message::Clients(clients::Message::Close))
                } else if self.recognize.details_open() {
                    self.update(Message::Recognize(recognize::Message::Close))
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

        let version = text(format!("v{}", update::VERSION))
            .size(13)
            .style(text::secondary);

        let newer = self.latest.as_deref().map(|latest| {
            style::tip(
                button(text(format!("Update to {latest} ↗")).size(13))
                    .style(button::text)
                    .padding([2, 6])
                    .on_press(Message::OpenPage),
                "A newer launcher is published. This opens its page, where the downloads \
                 are.",
            )
        });

        let game = self.selected.path().and_then(|path| self.game.phase(path));

        let header = row![
            tooltip(
                title,
                container(text(REPOSITORY).size(12))
                    .padding([4, 8])
                    .style(container::rounded_box),
                tooltip::Position::Bottom,
            ),
            version,
        ]
        .push(newer)
        .push(space::horizontal())
        .push(
            button("Manage clients…")
                .style(button::secondary)
                .on_press(Message::ManageClients),
        )
        .push(
            button(
                text(match game {
                    None => "Play",
                    Some(game::Phase::SettingUp) => "Setting up…",
                    Some(game::Phase::Running) => "Game running",
                })
                .font(style::BOLD),
            )
            .padding([6, 26])
            .style(button::primary)
            .on_press_maybe(
                (matches!(self.selected, Selected::Open { .. }) && game.is_none())
                    .then_some(Message::Play),
            ),
        )
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
                 import on another computer. The files in it are named after your \
                 account and characters.",
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

        let proposal = self
            .recognize
            .banner()
            .map(|banner| banner.map(Message::Recognize));

        let (installation, snapshot) = self.selected.open();

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
                .push(proposal)
                .spacing(14),
            container(body).height(Fill),
        ]
        .spacing(14)
        .padding(20);

        let (dialog, on_blur) = if let Some(setup) = &self.setup {
            // Added from the clients dialog, which comes back afterwards.
            (
                Some(setup.view().map(Message::Setup)),
                Message::Setup(setup::Message::Cancel),
            )
        } else if self.clients.is_open() {
            (
                Some(
                    self.clients
                        .view(&self.config, self.selected.path(), installation)
                        .map(Message::Clients),
                ),
                Message::Clients(clients::Message::Close),
            )
        } else if self.recognize.details_open() {
            (
                self.recognize
                    .dialog(&self.catalog)
                    .map(|dialog| dialog.map(Message::Recognize)),
                Message::Recognize(recognize::Message::Close),
            )
        } else {
            (
                self.transfer
                    .view()
                    .map(|dialog| dialog.map(Message::Transfer)),
                Message::Transfer(transfer::Message::Cancel),
            )
        };

        style::modal(content, dialog, on_blur)
    }

    /// Keyboard shortcuts (see [`shortcut`]), the window getting the focus
    /// back, and the games being followed.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            keyboard::listen().filter_map(shortcut),
            event::listen_with(|event, _status, _window| {
                matches!(event, Event::Window(window::Event::Focused)).then_some(Message::Focused)
            }),
            self.game.subscription().map(Message::Game),
        ])
    }

    /// Fetches the published catalog, if there is one to fetch and it
    /// wasn't fetched in the last [`REFRESH_PAUSE`].
    fn refresh_catalog(&mut self) -> Task<Message> {
        if self.refresh == catalog::Refresh::Running {
            return Task::none();
        }
        if let Some(since) = self
            .refreshed
            .map(|at| at.elapsed())
            .filter(|since| *since < REFRESH_PAUSE)
        {
            log::debug!("The catalog was fetched {since:?} ago: not again yet");
            return Task::none();
        }

        let Some(location) = catalog::Location::configured() else {
            log::info!(
                "No catalog location is configured: showing the {} addons built in",
                self.catalog.entries.len()
            );
            self.refresh = catalog::Refresh::Unavailable;
            return Task::none();
        };

        log::info!("Refreshing the catalog from {location}");
        self.refresh = catalog::Refresh::Running;
        self.refreshed = Some(Instant::now());

        Task::perform(location.load(self.client.clone()), Message::CatalogLoaded)
    }

    /// Proposes recognizing the selected client's addons that were installed
    /// without the launcher but match a catalog entry exactly.
    fn propose(&mut self) {
        if let Selected::Open {
            snapshot: Some(snapshot),
            ..
        } = &self.selected
        {
            self.recognize.propose(snapshot, &self.catalog.entries);
        }
    }

    /// Starts checking the client at `path`.
    fn open(&mut self, path: PathBuf) -> Task<Message> {
        log::info!("Opening the client at {}", path.display());
        self.selected = Selected::Opening(path.clone());
        self.recognize.clear();

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
        log::debug!("Saving the settings to {}", path.display());

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

/// A byte count for people: `850 KB`, `12.4 MB`, `17.3 GB`.
fn size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * MB;

    if bytes as f64 >= GB {
        format!("{:.1} GB", bytes as f64 / GB)
    } else if bytes as f64 >= MB {
        format!("{:.1} MB", bytes as f64 / MB)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
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

    /// A launcher listing `installations`, with nothing open.
    pub(crate) fn app(installations: &[&str]) -> App {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let installations: Vec<PathBuf> = installations.iter().map(PathBuf::from).collect();

        App {
            config_path: PathBuf::from("config.toml"),
            config: Config {
                selected: installations.first().cloned(),
                installations,
                ..Config::default()
            },
            client: Ok(reqwest::Client::new()),
            catalog: Catalog::default(),
            refresh: catalog::Refresh::Unavailable,
            refreshed: None,
            selected: Selected::Nothing,
            addons: addons::State::new(),
            setup: None,
            recovery: None,
            errors: Errors::default(),
            notice: None,
            recognize: recognize::State::default(),
            transfer: transfer::State::default(),
            clients: clients::State::default(),
            game: game::State::default(),
            latest: None,
        }
    }

    /// A launcher with `installation` open and scanned.
    pub(crate) fn opened(installation: Installation) -> App {
        let mut app = app(&[&installation.root().display().to_string()]);
        app.selected = Selected::Open {
            snapshot: installation.scan().ok(),
            installation,
        };
        app
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

    /// A catalog with one entry per name, each installing the folder of
    /// that name from the source `test`.
    fn catalog(names: &[&str]) -> Catalog {
        let entries: Vec<String> = names
            .iter()
            .map(|name| {
                format!(
                    r#"{{ "name": "{name}", "downloads": [{{ "source": "test", "name": "{name}",
                         "url": "https://example.com/{name}.zip", "folders": [{{ "name": "{name}" }}] }}] }}"#
                )
            })
            .collect();
        let json = format!(
            r#"{{ "version": 2,
                  "sources": [{{ "id": "test", "name": "Test", "homepage": "https://example.com" }}],
                  "entries": [{}] }}"#,
            entries.join(", ")
        );

        Catalog::parse(json.as_bytes()).unwrap()
    }

    #[test]
    fn an_unreadable_config_stops_everything_until_the_user_decides() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let mut app = unreadable(&path, "installations = [\"/games/wotlk\"");

        let mut ui = simulator(app.view());
        assert!(ui.find("The settings file can't be read").is_ok());
        assert!(ui.find("Manage clients…").is_err(), "nothing else is shown");
        drop(ui);

        // Overwriting starts afresh: no clients, so the add-client form.
        let _ = app.update(Message::Recovery(recovery::Message::Overwrite));
        assert!(app.recovery.is_none());
        assert_eq!(app.config, Config::default());
        let mut ui = simulator(app.view());
        assert!(ui.find("Add a WoW 3.3.5a client").is_ok());
    }

    #[test]
    fn the_window_icon_decodes() {
        assert!(icon().is_some());
        assert_eq!((ICON_WIDTH, ICON_HEIGHT), (256, 256));
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
    fn a_refresh_fetches_the_catalog_again_only_after_a_pause() {
        let mut app = app(&[]);
        app.refresh = catalog::Refresh::Idle;

        // Just fetched: left alone.
        app.refreshed = Some(Instant::now());
        let _ = app.update(Message::Refresh);
        assert_eq!(app.refresh, catalog::Refresh::Idle);

        // Long enough ago: fetched again, from wherever this build gets its
        // catalog, if anywhere.
        app.refreshed = Instant::now().checked_sub(REFRESH_PAUSE);
        let _ = app.update(Message::Refresh);
        assert_ne!(app.refresh, catalog::Refresh::Idle);
    }

    #[test]
    fn the_title_links_to_the_repository_and_shows_the_version() {
        let app = app(&[]);

        let mut ui = simulator(app.view());
        assert!(ui.find(format!("v{}", update::VERSION)).is_ok());
        assert!(ui.find("Update to").is_err(), "nothing newer is known");
        let _ = ui.click("Old Launcher").unwrap();

        assert!(matches!(
            ui.into_messages().next(),
            Some(Message::OpenRepository)
        ));
        assert_eq!(REPOSITORY, "https://gitlab.com/juxuanu/old_launcher");
    }

    #[test]
    fn a_newer_version_is_offered_next_to_the_version() {
        let mut app = app(&[]);
        let _ = app.update(Message::UpdateChecked(Some("9.0.0".into())));

        let mut ui = simulator(app.view());
        let _ = ui.click("Update to 9.0.0 ↗").unwrap();

        assert!(matches!(ui.into_messages().next(), Some(Message::OpenPage)));
    }

    #[test]
    fn an_import_restores_the_settings_then_installs_its_addons() {
        use crate::core::transfer::{Import, Manifest, Plan};

        let (_root, installation) = crate::testing::client(&[]);
        let mut app = opened(installation);
        app.catalog = catalog(&["ACP"]);
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
    fn clients_are_managed_in_a_dialog() {
        let mut app = app(&["/games/wotlk", "/games/warmane"]);
        app.selected = Selected::Opening("/games/wotlk".into());

        let mut ui = simulator(app.view());
        assert!(
            ui.find("/games/warmane").is_err(),
            "no picker in the header"
        );
        let _ = ui.click("Manage clients…").unwrap();
        for message in ui.into_messages() {
            let _ = app.update(message);
        }

        let mut ui = simulator(app.view());
        let _ = ui.click("/games/wotlk").unwrap();
        assert!(ui.find("/games/warmane").is_ok(), "every client is listed");
        assert!(ui.find(iced_test::selector::id(clients::ADD)).is_ok());
        drop(ui);

        let _ = app.update(Message::Escape);
        assert!(!app.clients.is_open(), "Escape closes it");
    }

    #[test]
    fn adding_a_client_from_the_dialog_comes_back_to_it() {
        let (_root, installation) = crate::testing::client(&[]);
        let mut app = app(&["/games/wotlk"]);
        app.selected = Selected::Failed("/games/wotlk".into());
        let _ = app.update(Message::ManageClients);

        let _ = app.update(Message::Clients(clients::Message::Add));
        let mut ui = simulator(app.view());
        assert!(ui.find("Add a WoW 3.3.5a client").is_ok());
        drop(ui);

        let _ = app.update(Message::Setup(setup::Message::Checked(Ok(
            installation.clone()
        ))));

        assert!(app.setup.is_none());
        let mut ui = simulator(app.view());
        assert!(ui.find("Clients").is_ok(), "the dialog again");
        assert!(
            ui.find(installation.root().display().to_string().as_str())
                .is_ok()
        );
    }

    #[test]
    fn removing_the_last_client_asks_for_another() {
        let mut app = app(&["/games/wotlk"]);
        app.selected = Selected::Failed("/games/wotlk".into());
        let _ = app
            .config
            .launch
            .insert("/games/wotlk".into(), crate::core::Launch::default());
        let _ = app.update(Message::ManageClients);

        let _ = app.update(Message::Clients(clients::Message::Remove));
        assert_eq!(app.config.installations.len(), 1, "asked first");
        let _ = app.update(Message::Clients(clients::Message::ConfirmRemove));

        assert!(app.config.installations.is_empty());
        assert!(app.config.launch.is_empty());
        assert!(!app.clients.is_open());
        let mut ui = simulator(app.view());
        assert!(ui.find("Add a WoW 3.3.5a client").is_ok());
        assert!(ui.find("Cancel").is_err(), "there is nothing to go back to");
    }

    #[test]
    fn a_long_client_path_is_shortened_to_fit() {
        let (root, _) = crate::testing::client(&[]);
        let long = root
            .path()
            .join("World of Warcraft ".repeat(5).trim_end())
            .join("3.3.5a ".repeat(12).trim_end());
        std::fs::create_dir_all(&long).unwrap();
        std::fs::copy(root.path().join("Wow.exe"), long.join("Wow.exe")).unwrap();
        let mut app = opened(Installation::open(&long).unwrap());
        let _ = app.update(Message::UpdateChecked(Some("9.0.0".into())));

        let mut ui = iced_test::simulator::Simulator::with_size(
            iced::Settings::default(),
            App::MIN_SIZE,
            app.view(),
        );

        for button in ["Play", "Manage clients…", "Import…", "Update to 9.0.0 ↗"] {
            let bounds = ui.find(button).unwrap().bounds();
            assert!(
                bounds.x + bounds.width <= App::MIN_SIZE.width,
                "{button} ends inside the window: {bounds:?}"
            );
        }
    }

    /// Whether the Play button, which must say `label`, can be pressed.
    fn play_button(app: &App, label: &str) -> bool {
        let mut ui = simulator(app.view());
        let _ = ui.click(label).unwrap();
        ui.into_messages().count() > 0
    }

    #[test]
    fn the_play_button_follows_the_game() {
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root().to_path_buf();
        let mut app = opened(installation);

        assert!(play_button(&app, "Play"));
        let _ = app.update(Message::Play);
        let label = if cfg!(windows) {
            "Game running"
        } else {
            "Setting up…"
        };
        assert!(!play_button(&app, label), "started, so not again");

        let _ = app.update(Message::Game(game::Message::Watched(Ok(vec![
            root.clone(),
        ]))));
        assert!(!play_button(&app, "Game running"));

        let _ = app.update(Message::Game(game::Message::Exited(
            root.clone(),
            Ok(Some(0)),
        )));
        let _ = app.update(Message::Game(game::Message::Watched(Ok(Vec::new()))));
        assert!(play_button(&app, "Play"));
        assert!(app.errors.messages().is_empty(), "a normal exit");
    }

    #[cfg(not(windows))]
    #[test]
    fn a_command_that_cant_start_the_game_is_pointed_at() {
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root().to_path_buf();
        let mut app = opened(installation);

        app.config.set_launch(
            &root,
            crate::core::Launch {
                env: "umu-run".into(),
                command: String::new(),
            },
        );
        let _ = app.update(Message::Play);
        assert!(app.game.phase(&root).is_none(), "nothing was started");

        app.config.set_launch(&root, crate::core::Launch::default());
        let _ = app.update(Message::Play);
        let _ = app.update(Message::Game(game::Message::Exited(root, Ok(Some(127)))));

        assert_eq!(
            app.errors.messages(),
            [
                "Could not start World of Warcraft: invalid environment variables: umu-run is \
                 not a NAME=value pair.",
                "Could not start World of Warcraft: the command to start it was not found. \
                 Check it under Manage clients.",
            ]
        );
    }

    #[test]
    fn a_client_finishing_to_open_redraws_cleanly() {
        let (_root, installation) = crate::testing::client(&[]);
        let path = installation.root().display().to_string();

        let mut opening = app(&[&path]);
        opening.selected = Selected::Opening(installation.root().to_path_buf());
        let open = opened(installation);

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
        let snapshot = installation.scan().unwrap();
        let catalog = catalog(&["Addon07"]);
        let open = |error: Option<&str>, proposed: bool| {
            let mut app = opened(installation.clone());
            if let Some(error) = error {
                app.errors.report(error);
            }
            if proposed {
                app.recognize.propose(&snapshot, &catalog.entries);
            }
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
    fn recognizing_addons_is_proposed_after_a_scan_and_done_on_request() {
        let (_root, installation) = crate::testing::client(&["Interface/AddOns/ACP/ACP.toc"]);
        let root = installation.root().to_path_buf();
        let mut app = opened(installation.clone());
        app.catalog = catalog(&["ACP"]);

        let _ = app.update(Message::Scanned(
            root.clone(),
            Ok(installation.scan().unwrap()),
        ));

        let mut ui = simulator(app.view());
        assert!(
            ui.find(
                "ACP was installed without the launcher and matches the catalog. Recognize \
                 it, so it shows as installed and can be updated from here?"
            )
            .is_ok()
        );
        assert!(ui.find("Installed manually").is_ok(), "not yet recognized");
        let _ = ui.click("Details").unwrap();
        for message in ui.into_messages() {
            let _ = app.update(message);
        }

        let mut ui = simulator(app.view());
        assert!(ui.find("Addons to recognize").is_ok());
        drop(ui);
        let _ = app.update(Message::Escape);
        assert!(!app.recognize.details_open(), "Escape closes the list");

        // Recognizing writes the markers (in a task not run here) and
        // reports; the client is then scanned again.
        let adopted = core::Adoption::plan(&installation.scan().unwrap(), &app.catalog.entries);
        let rescan = app.update(Message::Recognize(recognize::Message::Adopted(Ok(adopted))));

        assert!(rescan.units() > 0);
        assert_eq!(
            app.notice.as_deref(),
            Some("Recognized 1 addon: now listed as installed.")
        );
        let mut ui = simulator(app.view());
        assert!(ui.find("Recognize").is_err(), "nothing left to propose");
    }
}
