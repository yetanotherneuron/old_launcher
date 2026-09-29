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
mod recovery;
mod setup;
mod style;
#[cfg(test)]
mod testing;
mod transfer;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{
    button, center, column, container, operation, row, scrollable, space, text, tooltip,
};
use iced::{
    Center, Element, Event, Fill, Fit, Function, Subscription, Task, Theme, event, padding, window,
};

use crate::catalog::Catalog;
use crate::core::{Adoption, Config, Installation, Snapshot};
use crate::errors::Errors;

/// Where the launcher's code lives, as `Cargo.toml` says.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// The launcher's icon, rendered from `assets/old_launcher.svg`.
const ICON: &[u8] = include_bytes!("../assets/old_launcher.png");

/// The window's icon, for the title bar, taskbar and window switcher on
/// Windows and X11. Wayland desktops take it from the desktop entry.
pub fn icon() -> Option<iced::window::Icon> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(ICON));
    decoder.set_transformations(png::Transformations::EXPAND);

    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut pixels).ok()?;

    if (frame.color_type, frame.bit_depth) != (png::ColorType::Rgba, png::BitDepth::Eight) {
        return None;
    }
    pixels.truncate(frame.line_size * frame.height as usize);

    iced::window::icon::from_rgba(pixels, frame.width, frame.height).ok()
}

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
    /// The clients dialog, while open.
    clients: Option<clients::State>,
    /// The games started from the launcher or found running, by client
    /// folder, until over.
    games: BTreeMap<PathBuf, Game>,
}

/// A game started from the launcher, or found running. Once started it is
/// set up (Proton downloaded, a prefix made, …) until its `Wow.exe` shows
/// up, and over when that is gone.
struct Game {
    /// Whether its `Wow.exe` was seen, so it is past the setting up.
    started: bool,
    /// Whether the command that started it is still running.
    command: bool,
    /// How many more checks to wait for `Wow.exe` once the command exited
    /// fine without it: Steam or Lutris take the game over and exit.
    patience: u8,
}

impl Game {
    /// Checks, a second apart, that `Wow.exe` gets to show up after the
    /// command that started it exited.
    const PATIENCE: u8 = 60;
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
    /// Open the clients dialog.
    ManageClients,
    /// Start the game.
    Play,
    /// The command that started the game of this client folder exited, with
    /// this exit code if it had one, or failed to start.
    Exited(PathBuf, Result<Option<i32>, String>),
    /// Time to look for the games started, and the selected client's.
    Watch,
    /// The client folders whose game is running.
    Watched(Result<Vec<PathBuf>, String>),
    /// The launcher's window got the focus back.
    Focused,
    /// The selected client folder was checked.
    Opened(PathBuf, Result<Installation, String>),
    /// A client folder was scanned.
    Scanned(PathBuf, Result<Snapshot, String>),
    /// The published catalog was fetched.
    CatalogLoaded(Result<Catalog, String>),
    /// Addons installed without the launcher were recognized as these
    /// catalog entries.
    Adopted(Result<Vec<Adoption>, String>),
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
            clients: None,
            games: BTreeMap::new(),
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
                        // Added from the clients dialog, which shows it next.
                        let read = self.clients.as_mut().map_or_else(Task::none, |clients| {
                            clients.read(&root).map(Message::Clients)
                        });

                        self.config.selected = Some(root);
                        self.selected = Selected::Open {
                            installation,
                            snapshot: None,
                        };
                        self.setup = None;
                        self.recognized.clear();

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
            Message::Clients(message) => {
                let Some(clients) = &mut self.clients else {
                    return Task::none();
                };
                let current = self.selected.path().map(Path::to_path_buf);
                let action = clients.update(
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
                        self.config.remove(&removed);
                        self.selected = Selected::Nothing;

                        let open = match self.config.selected.clone() {
                            Some(path) => {
                                let read =
                                    self.clients.as_mut().map_or_else(Task::none, |clients| {
                                        clients.read(&path).map(Message::Clients)
                                    });
                                Task::batch([read, self.open(path)])
                            }
                            // Nothing left to manage: the first-run form.
                            None => {
                                self.clients = None;
                                self.setup = Some(setup::State::new(false));
                                operation::focus(setup::PATH)
                            }
                        };

                        Task::batch([task, open, self.save()])
                    }
                    Some(clients::Instruction::Close { edited }) => {
                        self.clients = None;

                        if edited {
                            Task::batch([task, self.save()])
                        } else {
                            task
                        }
                    }
                    None => task,
                }
            }
            Message::ManageClients => {
                let mut clients = clients::State::default();
                let read = self
                    .selected
                    .path()
                    .map_or_else(Task::none, |path| clients.read(path).map(Message::Clients));
                self.clients = Some(clients);

                read
            }
            Message::Play => {
                let Selected::Open { installation, .. } = &self.selected else {
                    return Task::none();
                };
                let root = installation.root().to_path_buf();

                if self.games.contains_key(&root) {
                    return Task::none();
                }

                match core::launch::command(installation, &self.config.launch(&root)) {
                    Ok(command) => {
                        self.games.insert(
                            root.clone(),
                            Game {
                                started: false,
                                command: true,
                                patience: Game::PATIENCE,
                            },
                        );
                        Task::perform(play(command), Message::Exited.with(root))
                    }
                    Err(error) => {
                        self.errors
                            .report(format!("Could not start World of Warcraft: {error}"));
                        Task::none()
                    }
                }
            }
            Message::Exited(root, result) => {
                // A fine exit may leave the game running, or about to (see
                // `Game::patience`): the watching tells.
                if result == Ok(Some(0)) {
                    if let Some(game) = self.games.get_mut(&root) {
                        game.command = false;
                    }
                } else {
                    self.games.remove(&root);
                }

                match result {
                    Ok(None | Some(0)) => {}
                    // What `sh` says when it can't find or run the command.
                    Ok(Some(127)) if !cfg!(windows) => self.errors.report(
                        "Could not start World of Warcraft: the command to start it was not \
                         found. Check it under Manage clients.",
                    ),
                    Ok(Some(126)) if !cfg!(windows) => self.errors.report(
                        "Could not start World of Warcraft: the command to start it can't be \
                         run. Check it under Manage clients.",
                    ),
                    Ok(Some(code)) => self
                        .errors
                        .report(format!("World of Warcraft exited with status {code}.")),
                    Err(error) => self
                        .errors
                        .report(format!("Could not start World of Warcraft: {error}")),
                }

                Task::none()
            }
            Message::Watch => {
                // The selected client too, whose game may have been started
                // elsewhere, or before the launcher was.
                let selected = match &self.selected {
                    Selected::Open { installation, .. } => Some(installation.root()),
                    _ => None,
                };
                let mut roots: Vec<PathBuf> = self.games.keys().cloned().collect();
                roots.extend(
                    selected
                        .filter(|root| !self.games.contains_key(*root))
                        .map(Path::to_path_buf),
                );

                if roots.is_empty() {
                    return Task::none();
                }

                Task::perform(
                    blocking(move || Ok(core::launch::running(&roots))),
                    Message::Watched,
                )
            }
            Message::Watched(Ok(running)) => {
                for root in &running {
                    self.games.entry(root.clone()).or_insert(Game {
                        started: true,
                        command: false,
                        patience: 0,
                    });
                }

                self.games.retain(|root, game| {
                    if running.contains(root) {
                        game.started = true;
                        return true;
                    }

                    // Gone once seen: the game is over, even if what started
                    // it lingers (Lutris' window, say).
                    if game.started {
                        return false;
                    }
                    if game.command {
                        return true;
                    }

                    game.patience = game.patience.saturating_sub(1);
                    game.patience > 0
                });

                Task::none()
            }
            // The next look will do.
            Message::Watched(Err(_)) => Task::none(),
            // Back from somewhere else, where a game may have started or a
            // realmlist been edited.
            Message::Focused => Task::batch([
                self.update(Message::Watch),
                self.update(Message::Clients(clients::Message::Focused)),
            ]),
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
                        Task::batch([self.rescan(), self.update(Message::Watch)])
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
                } else if self.clients.is_some() {
                    self.update(Message::Clients(clients::Message::Close))
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

        let game = self.selected.path().and_then(|path| self.games.get(path));

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
            button("Manage clients…")
                .style(button::secondary)
                .on_press(Message::ManageClients),
            button(
                text(match game {
                    None => "Play",
                    Some(Game { started: false, .. }) => "Setting up…",
                    Some(Game { started: true, .. }) => "Game running",
                })
                .font(style::BOLD)
            )
            .padding([6, 26])
            .style(button::primary)
            .on_press_maybe(
                (matches!(self.selected, Selected::Open { .. }) && game.is_none())
                    .then_some(Message::Play)
            ),
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

        let (dialog, on_blur) = match (&self.setup, &self.clients) {
            // Added from the clients dialog, which comes back afterwards.
            (Some(setup), _) => (
                Some(setup.view().map(Message::Setup)),
                Message::Setup(setup::Message::Cancel),
            ),
            (None, Some(clients)) => (
                Some(
                    clients
                        .view(&self.config, self.selected.path(), installation)
                        .map(Message::Clients),
                ),
                Message::Clients(clients::Message::Close),
            ),
            (None, None) if self.details => {
                (Some(self.recognized_view()), Message::CloseRecognized)
            }
            (None, None) => (
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

    /// Keyboard shortcuts (see [`shortcut`]), the window getting the focus
    /// back, and a look for the games every second while there are some.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            keyboard::listen().filter_map(shortcut),
            event::listen_with(|event, _status, _window| {
                matches!(event, Event::Window(window::Event::Focused)).then_some(Message::Focused)
            }),
            if self.games.is_empty() {
                Subscription::none()
            } else {
                iced::time::every(Duration::from_secs(1)).map(|_| Message::Watch)
            },
        ])
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

/// Starts the game with `command` and waits for it to exit: its exit code,
/// if it has one. The game keeps running if the launcher quits first.
async fn play(command: std::process::Command) -> Result<Option<i32>, String> {
    let status = tokio::process::Command::from(command)
        .status()
        .await
        .map_err(|error| describe(&error))?;

    Ok(status.code())
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
                ..Config::default()
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
            clients: None,
            games: BTreeMap::new(),
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
        assert!(app.clients.is_none(), "Escape closes it");
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
        app.config
            .launch
            .insert("/games/wotlk".into(), crate::core::Launch::default());
        let _ = app.update(Message::ManageClients);

        let _ = app.update(Message::Clients(clients::Message::Remove));

        assert!(app.config.installations.is_empty());
        assert!(app.config.launch.is_empty());
        assert!(app.clients.is_none());
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
        let installation = Installation::open(&long).unwrap();
        let mut app = app(&[&long.display().to_string()]);
        app.selected = Selected::Open {
            snapshot: Some(installation.scan().unwrap()),
            installation,
        };

        let mut ui = iced_test::simulator::Simulator::with_size(
            iced::Settings::default(),
            App::MIN_SIZE,
            app.view(),
        );

        for button in ["Play", "Manage clients…", "Import…"] {
            let bounds = ui.find(button).unwrap().bounds();
            assert!(
                bounds.x + bounds.width <= App::MIN_SIZE.width,
                "{button} ends inside the window: {bounds:?}"
            );
        }
    }

    /// An app with the client at `root` open.
    fn playable(installation: Installation) -> App {
        let mut app = app(&[&installation.root().display().to_string()]);
        app.selected = Selected::Open {
            snapshot: None,
            installation,
        };
        app
    }

    /// Whether the Play button, which must say `label`, can be pressed.
    fn play_button(app: &App, label: &str) -> bool {
        let mut ui = simulator(app.view());
        let _ = ui.click(label).unwrap();
        ui.into_messages().count() > 0
    }

    #[test]
    fn a_game_is_set_up_then_running_until_its_wow_exe_is_gone() {
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root().to_path_buf();
        let mut app = playable(installation);

        assert!(play_button(&app, "Play"));
        let _ = app.update(Message::Play);
        let _ = app.update(Message::Watched(Ok(Vec::new())));
        assert!(
            !play_button(&app, "Setting up…"),
            "no Wow.exe yet: Proton being set up, say"
        );

        let _ = app.update(Message::Watched(Ok(vec![root.clone()])));
        assert!(!play_button(&app, "Game running"));

        // wine or umu-run exit with the game, which then goes.
        let _ = app.update(Message::Exited(root.clone(), Ok(Some(0))));
        assert!(!play_button(&app, "Game running"), "until it is gone");
        let _ = app.update(Message::Watched(Ok(Vec::new())));
        assert!(play_button(&app, "Play"));
        assert!(app.errors.messages().is_empty(), "a normal exit");

        let _ = app.update(Message::Play);
        let _ = app.update(Message::Exited(root, Ok(Some(3))));
        assert!(play_button(&app, "Play"), "over at once");
        assert_eq!(
            app.errors.messages(),
            ["World of Warcraft exited with status 3."]
        );
    }

    #[test]
    fn a_game_already_running_is_found_and_followed() {
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root().to_path_buf();
        let mut app = playable(installation);

        // Nothing was started, but the selected client is looked at: when
        // it opens, and when the launcher gets the focus back.
        assert!(app.update(Message::Watch).units() > 0);
        let _ = app.update(Message::Watched(Ok(vec![root])));
        assert!(!play_button(&app, "Game running"));

        let _ = app.update(Message::Watched(Ok(Vec::new())));
        assert!(play_button(&app, "Play"));

        app.selected = Selected::Nothing;
        assert_eq!(app.update(Message::Watch).units(), 0, "nothing to look for");
    }

    #[test]
    fn a_game_handed_to_another_launcher_is_waited_for() {
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root().to_path_buf();
        let mut app = playable(installation);

        // `steam steam://rungameid/…` exits as soon as Steam has the game.
        let _ = app.update(Message::Play);
        let _ = app.update(Message::Exited(root.clone(), Ok(Some(0))));
        for _ in 1..Game::PATIENCE {
            let _ = app.update(Message::Watched(Ok(Vec::new())));
        }
        assert!(!play_button(&app, "Setting up…"));
        let _ = app.update(Message::Watched(Ok(vec![root.clone()])));
        assert!(!play_button(&app, "Game running"));
        let _ = app.update(Message::Watched(Ok(Vec::new())));
        assert!(play_button(&app, "Play"));

        // And when the game never shows up, the launcher stops waiting.
        let _ = app.update(Message::Play);
        let _ = app.update(Message::Exited(root, Ok(Some(0))));
        for _ in 0..Game::PATIENCE {
            let _ = app.update(Message::Watched(Ok(Vec::new())));
        }
        assert!(play_button(&app, "Play"));
    }

    #[cfg(unix)]
    #[test]
    fn playing_waits_for_the_game_and_tells_how_it_ended() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let shell = |line: &str| {
            let mut command = std::process::Command::new("sh");
            command.arg("-c").arg(line);
            command
        };

        assert_eq!(runtime.block_on(play(shell("exit 0"))), Ok(Some(0)));
        assert_eq!(runtime.block_on(play(shell("exit 3"))), Ok(Some(3)));
        assert_eq!(
            runtime.block_on(play(shell("kill -9 $$"))),
            Ok(None),
            "killed, so no exit code"
        );
        assert!(
            runtime
                .block_on(play(std::process::Command::new(
                    "/nonexistent/old_launcher_game"
                )))
                .is_err()
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn a_command_that_cant_start_the_game_is_pointed_at() {
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root().to_path_buf();
        let mut app = app(&[&root.display().to_string()]);
        app.selected = Selected::Open {
            snapshot: None,
            installation,
        };

        app.config.set_launch(
            &root,
            crate::core::Launch {
                env: "umu-run".into(),
                command: String::new(),
            },
        );
        let _ = app.update(Message::Play);
        assert!(app.games.is_empty(), "nothing was started");

        app.config.set_launch(&root, crate::core::Launch::default());
        let _ = app.update(Message::Play);
        let _ = app.update(Message::Exited(root, Ok(Some(127))));

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
