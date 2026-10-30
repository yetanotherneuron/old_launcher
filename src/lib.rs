//! Old Launcher: starts WoW 3.3.5a clients and manages their addons.
//!
//! The app is a thin shell over [`core`]: it remembers which client folders
//! the user added and how each one starts, lists the launcher's catalog of
//! addons alongside the client's, and runs every filesystem change on a
//! blocking thread.

pub use old_launcher_core as core;

mod action;
mod activity;
mod addons;
mod backdrop;
mod catalog;
mod clients;
pub mod discord;
mod download;
mod errors;
mod game;
mod github;
mod gitlab;
mod patches;
mod recognize;
mod recovery;
#[cfg(test)]
mod screenshots;
mod settings;
mod setup;
mod style;
#[cfg(test)]
mod testing;
mod torrent;
mod transfer;
mod update;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iced::keyboard::{self, Key, key::Named};
use iced::widget::{
    button, center, column, container, operation, row, scrollable, space, text, tooltip,
};
use iced::{
    Center, Element, Event, Fill, Function, Subscription, Task, Theme, Widget, event, window,
};

use crate::catalog::Catalog;
use crate::core::mods::Mods;
use crate::core::{Config, Installation, Locale, Snapshot};
use crate::errors::Errors;

/// Where the launcher's code lives, as `Cargo.toml` says.
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");

/// How long a refresh leaves the catalog alone after fetching it.
const REFRESH_PAUSE: Duration = Duration::from_secs(10);

/// Why the add-client form refuses a torrent while a client downloads, or
/// is checked for updates: one at a time.
const ONE_DOWNLOAD: &str =
    "Wait for the download or check under way to finish, or stop it, before starting another.";

/// The launcher's icon, rendered from `assets/old_launcher.svg` and decoded
/// when building.
const ICON: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon.rgba"));
include!(concat!(env!("OUT_DIR"), "/icon_size.rs"));

/// The window's icon, for the title bar, taskbar and window switcher on
/// Windows and X11. Wayland desktops take it from the desktop entry.
pub fn icon() -> Option<iced::window::Icon> {
    iced::window::icon::from_rgba(ICON.to_vec(), ICON_WIDTH, ICON_HEIGHT).ok()
}

/// The fonts the launcher brings, for the window to load before drawing
/// (see [`style::fonts`]).
pub fn fonts() -> [&'static [u8]; 2] {
    style::fonts()
}

/// The launcher.
pub struct App {
    config_path: PathBuf,
    config: Config,
    /// What downloads go through, or why there is nothing to download with.
    client: Result<reqwest::Client, String>,
    /// The same, but following no redirects by itself, for downloads signed
    /// in to GitHub to follow them and sign only GitHub's hosts.
    direct: Option<reqwest::Client>,
    /// How downloads from GitHub sign in, if they do.
    github: github::Login,
    settings: settings::State,
    catalog: Catalog,
    refresh: catalog::Refresh,
    /// When the catalog was last fetched, so a refresh doesn't fetch it
    /// again right away.
    refreshed: Option<Instant>,
    /// The tag the server gave the catalog shown, to ask whether it changed.
    tag: Option<String>,
    /// The same for the list of mods.
    mods_tag: Option<String>,
    /// Where the catalogs fetched last are kept for the next start.
    cache: catalog::Cache,
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
    /// Clients downloaded with BitTorrent, and their updates.
    download: download::State,
    /// The games started, and the selected client's when found running.
    game: game::State,
    /// What the selected client's archives say, looked at in the background.
    patches: patches::State,
    /// A newer launcher version than this one, when published.
    latest: Option<String>,
    /// Whether quitting was asked for while something it would cut short
    /// is under way, so the launcher asks first (see [`App::under_way`]).
    quitting: bool,
    /// Whether the window has the focus.
    focused: bool,
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
    /// A client downloading, or checked for updates.
    Download(download::Message),
    /// An interaction with the proposal to recognize addons.
    Recognize(recognize::Message),
    /// Something happened to a game.
    Game(game::Message),
    /// The look at a client's patches got somewhere, or ended.
    Patches(patches::Message),
    /// An interaction with the settings dialog.
    Settings(settings::Message),
    /// Open the settings dialog.
    OpenSettings,
    /// The GitHub CLI answered with its login, or why there is none.
    SignedIn(github::Login),
    /// Open the clients dialog.
    ManageClients,
    /// Start the game.
    Play,
    /// The launcher's window got the focus, or lost it.
    Focused(bool),
    /// The selected client folder was checked.
    Opened(PathBuf, Result<Installation, String>),
    /// A client folder was scanned.
    Scanned(PathBuf, Result<Snapshot, String>),
    /// The published catalog was fetched.
    CatalogLoaded(Result<catalog::Fetched<Catalog>, String>),
    /// The published list of mods was fetched.
    ModsLoaded(Result<catalog::Fetched<Mods>, String>),
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
    /// A page, folder or file couldn't be opened: why.
    Unopened(String),
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
    /// Ctrl+Q was pressed, or the window asked to close.
    Quit,
    /// Quit even though something is under way.
    QuitAnyway,
    /// Don't quit after all.
    KeepOpen,
}

impl App {
    /// The smallest window everything still fits in: the toolbar's search
    /// field and dropdowns side by side, and a card's source picker, links
    /// and buttons beside its name and badges.
    pub const MIN_SIZE: iced::Size = iced::Size::new(1200.0, 600.0);

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

        let client = http_client().build().map_err(|error| describe(&error));
        if let Err(error) = &client {
            errors.report(addons::unavailable(error));
        }
        // Without it, downloads stay anonymous.
        let direct = http_client()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok();

        // The catalog fetched last time shows right away, and a fresh one is
        // fetched in a moment. Another catalog, when asked for, comes alone.
        let cache = catalog::Cache::default_location();
        let (mut catalog, tag) =
            if catalog::Location::configured::<Catalog>().is_published::<Catalog>() {
                cache.read().unwrap_or_default()
            } else {
                (Catalog::default(), None)
            };
        let (mods, mods_tag) = if catalog::Location::configured::<Mods>().is_published::<Mods>() {
            cache.read().unwrap_or_default()
        } else {
            (Mods::default(), None)
        };
        catalog.prefer(&config.sources);
        if catalog.entries.is_empty() {
            log::info!("No catalog fetched yet");
        } else {
            log::info!(
                "Showing the catalog fetched last time: {}",
                counted(catalog.entries.len(), "addon")
            );
        }

        let mut app = Self {
            config_path,
            config,
            client,
            direct,
            github: github::Login::Anonymous,
            settings: settings::State::default(),
            catalog,
            refresh: catalog::Refresh::Idle,
            refreshed: None,
            tag,
            mods_tag,
            cache,
            selected: Selected::Nothing,
            addons: addons::State::new(),
            setup: None,
            recovery,
            errors,
            notice: None,
            recognize: recognize::State::default(),
            transfer: transfer::State::default(),
            clients: clients::State::default(),
            download: download::State::new(download::store()),
            game: game::State::default(),
            patches: patches::State::new(patches::cache()),
            latest: None,
            quitting: false,
            focused: true,
        };
        app.clients.keep_mod_downloads_in(clients::mods::cache());
        // No client is open yet to look at with it.
        app.download.mods(mods.clone());
        let _ = app.clients.list_mods(mods, None);

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
            self.sign_in(),
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
                        Task::batch([task, self.add(installation)])
                    }
                    Some(setup::Instruction::Torrent { source, into }) => {
                        // One at a time: the form says so, and keeps what
                        // was typed for when it is.
                        if self.download.is_running() {
                            setup.refuse(ONE_DOWNLOAD.to_owned());
                            return task;
                        }
                        // The first-run form stays under the download's
                        // dialog until there is a client.
                        if !self.config.installations.is_empty() {
                            self.setup = None;
                        }
                        let start = self.download.start(
                            download::Request::New { source, into },
                            &self.config,
                            self.client.as_ref().map_err(String::as_str),
                        );

                        Task::batch([task, start.map(Message::Download)])
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
                        self.catalog.prefer(&self.config.sources);
                        Task::batch([task, self.start()])
                    }
                    Some(recovery::Instruction::Overwrite) => {
                        self.recovery = None;
                        self.config = Config::default();
                        self.catalog.prefer(&self.config.sources);
                        Task::batch([task, self.save(), self.start()])
                    }
                    Some(recovery::Instruction::Quit) => iced::exit(),
                    None => task,
                }
            }
            Message::Addons(message) => {
                let playing = self.playing();
                let (installation, snapshot) = self.selected.open();
                let action = self.addons.update(
                    message,
                    &self.catalog,
                    self.client.as_ref().map_err(String::as_str),
                    installation,
                    snapshot,
                    playing,
                    &mut self.errors,
                );
                let task = action.task.map(Message::Addons);

                match action.instruction {
                    Some(addons::Instruction::Changed) => Task::batch([task, self.rescan()]),
                    Some(addons::Instruction::Reload) => {
                        Task::batch([task, self.refresh_catalog()])
                    }
                    Some(addons::Instruction::Prefer { key, source }) => {
                        log::info!(
                            "Installing {} from {} from now on",
                            key.as_str(),
                            source.as_deref().unwrap_or("the newest source")
                        );
                        self.config.set_source(key.as_str(), source);
                        self.catalog.prefer(&self.config.sources);
                        // Once the list is drawn in its new order, its card
                        // is brought back into view if that moved it out.
                        Task::batch([
                            task,
                            self.save(),
                            Task::done(Message::Addons(addons::Message::Follow(key))),
                        ])
                    }
                    Some(addons::Instruction::Installed { folders, from }) => {
                        let from = from.file_name().map_or_else(
                            || from.display().to_string(),
                            |name| name.to_string_lossy().into_owned(),
                        );
                        self.notice =
                            Some(format!("Installed {} from {from}.", folders.join(", ")));
                        Task::batch([task, self.rescan()])
                    }
                    None => task,
                }
            }
            Message::Clients(message) => {
                // The mods' banners show, and their work ends, with the
                // dialog closed too; and an editor that couldn't open says so.
                if !self.clients.is_open()
                    && !matches!(
                        message,
                        clients::Message::Mods(_) | clients::Message::Unopened(_)
                    )
                {
                    return Task::none();
                }
                let current = self.selected.path().map(Path::to_path_buf);
                let playing = self.playing();
                let (installation, _) = self.selected.open();
                let action = self.clients.update(
                    message,
                    &mut self.config,
                    current.as_deref(),
                    installation,
                    playing,
                    self.client.as_ref().map_err(String::as_str),
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
                    Some(clients::Instruction::Check(root)) => {
                        let start = self.download.start(
                            download::Request::Check(root),
                            &self.config,
                            self.client.as_ref().map_err(String::as_str),
                        );

                        Task::batch([task, start.map(Message::Download)])
                    }
                    Some(clients::Instruction::Languages {
                        root,
                        locales,
                        locale,
                    }) => {
                        let start = self.download.start(
                            download::Request::Languages {
                                root,
                                locales,
                                locale,
                            },
                            &self.config,
                            self.client.as_ref().map_err(String::as_str),
                        );

                        Task::batch([task, start.map(Message::Download)])
                    }
                    Some(clients::Instruction::Adopt { root, source }) => {
                        let start = self.download.start(
                            download::Request::Adopt { root, source },
                            &self.config,
                            self.client.as_ref().map_err(String::as_str),
                        );

                        Task::batch([task, start.map(Message::Download)])
                    }
                    Some(clients::Instruction::Restore(root, torrent)) => {
                        if !self.config.installations.contains(&root) {
                            return task;
                        }
                        let _ = self.config.torrents.insert(root, torrent);
                        Task::batch([task, self.save()])
                    }
                    Some(clients::Instruction::Remove) => {
                        let Some(removed) = current else {
                            return task;
                        };
                        log::info!("Forgetting the client at {}", removed.display());
                        let stopped = self.download.forget(&removed).map(Message::Download);
                        self.config.remove(&removed);
                        self.selected = Selected::Nothing;
                        self.recognize.clear();
                        self.patches.forget(&removed);
                        self.sync_patches();

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

                        Task::batch([task, stopped, open, self.save()])
                    }
                    Some(clients::Instruction::Switched(switched)) => {
                        if let Selected::Open { installation, .. } = &mut self.selected
                            && installation.root() == switched.root()
                        {
                            log::info!(
                                "{} plays in {} from now on",
                                switched.root().display(),
                                switched.locale().map_or("no language", Locale::code)
                            );
                            *installation = switched;
                            // The language decides which archives load.
                            return Task::batch([task, self.look_at_patches()]);
                        }
                        task
                    }
                    Some(clients::Instruction::Close { edited: true }) => {
                        log::info!("The launch settings changed");
                        Task::batch([task, self.save()])
                    }
                    Some(clients::Instruction::Close { edited: false }) | None => task,
                }
            }
            Message::Download(message) => {
                let playing = self
                    .download
                    .root()
                    .is_some_and(|root| self.game.phase(root).is_some());
                let action = self.download.update(
                    message,
                    &self.config,
                    self.client.as_ref().map_err(String::as_str),
                    playing,
                    &mut self.errors,
                );
                let task = action.task.map(Message::Download);

                match action.instruction {
                    Some(download::Instruction::Pending(torrent)) => {
                        self.config
                            .downloads
                            .retain(|pending| pending.info_hash != torrent.info_hash);
                        self.config.downloads.push(torrent);
                        Task::batch([task, self.save()])
                    }
                    Some(download::Instruction::Discarded(info_hash)) => {
                        self.config
                            .downloads
                            .retain(|pending| pending.info_hash != info_hash);
                        Task::batch([task, self.save()])
                    }
                    Some(download::Instruction::Added {
                        installation,
                        torrent,
                        notice,
                    }) => {
                        self.config
                            .downloads
                            .retain(|pending| pending.info_hash != torrent.info_hash);
                        let _ = self
                            .config
                            .torrents
                            .insert(installation.root().to_path_buf(), torrent);
                        self.notice = Some(notice);
                        Task::batch([task, self.add(installation)])
                    }
                    // Unless the client was removed meanwhile.
                    Some(
                        download::Instruction::Saved(root, _)
                        | download::Instruction::Repaired { root, .. }
                        | download::Instruction::LanguageAdded { root, .. }
                        | download::Instruction::LanguageDeleted(root),
                    ) if !self.config.installations.contains(&root) => task,
                    Some(download::Instruction::Saved(root, torrent)) => {
                        let _ = self.config.torrents.insert(root, torrent);
                        Task::batch([task, self.save()])
                    }
                    Some(download::Instruction::Repaired { root, notice }) => {
                        self.notice = Some(notice);
                        Task::batch([task, self.files_changed(&root), self.save()])
                    }
                    Some(download::Instruction::LanguageAdded { root, notice }) => {
                        self.notice = Some(notice);
                        // The mods in it want their files in the language
                        // too.
                        self.clients.complete_mods(&root);
                        Task::batch([task, self.files_changed(&root)])
                    }
                    Some(download::Instruction::LanguageDeleted(root)) => {
                        Task::batch([task, self.files_changed(&root)])
                    }
                    None => task,
                }
            }
            Message::ManageClients => self
                .clients
                .open(self.selected.path())
                .map(Message::Clients),
            Message::Settings(message) => {
                let action = self.settings.update(message);
                let task = action.task.map(Message::Settings);

                match action.instruction {
                    Some(settings::Instruction::UseCli(use_cli)) => {
                        log::info!(
                            "{} the GitHub CLI's login",
                            if use_cli { "Using" } else { "Not using" }
                        );
                        self.config.github_cli = use_cli;
                        Task::batch([task, self.save(), self.sign_in()])
                    }
                    Some(settings::Instruction::ShowOnDiscord(show)) => {
                        log::info!(
                            "{} the game on Discord from the next one on",
                            if show { "Showing" } else { "Not showing" }
                        );
                        self.config.discord = show;
                        Task::batch([task, self.save()])
                    }
                    Some(settings::Instruction::OpenLink(url)) => {
                        log::info!("Opening {url}");
                        Task::batch([task, opening(url, None).map(Message::Unopened)])
                    }
                    None => task,
                }
            }
            // The GitHub CLI may have logged in or out since it was asked.
            Message::OpenSettings => {
                self.settings.open();
                self.sign_in()
            }
            Message::SignedIn(login) => {
                // Unless the box was unticked meanwhile.
                if self.config.github_cli
                    && matches!(self.github, github::Login::Asking | github::Login::Cli(_))
                {
                    self.signed_in(login);
                }
                Task::none()
            }
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
                if self.clients.holds_back(installation.root())
                    || self.download.holds_back(installation.root())
                    || self.patches.holds_back(installation.root())
                {
                    return Task::none();
                }
                let launch = self.config.launch(installation.root());
                let natives = self.clients.natives(installation.root());

                self.game
                    .play(
                        installation,
                        &launch,
                        &natives,
                        self.config.discord,
                        &mut self.errors,
                    )
                    .map(Message::Game)
            }
            Message::Game(message) => self
                .game
                .update(message, &mut self.errors)
                .map(Message::Game),
            Message::Patches(message) => {
                let task = self
                    .patches
                    .update(message, &mut self.errors)
                    .map(Message::Patches);
                self.sync_patches();

                task
            }
            // Back from somewhere else, where a game may have started. The
            // window reports the focus more than once per return, so only
            // the change from lost to gained counts.
            Message::Focused(focused) => {
                let regained = focused && !self.focused;
                self.focused = focused;
                if !regained {
                    return Task::none();
                }
                log::trace!("The window got the focus back");
                let (installation, _) = self.selected.open();
                let selected = installation.map(Installation::root);
                // Archives may have come or gone meanwhile.
                let patches = installation.map_or_else(Task::none, |installation| {
                    self.patches
                        .regained(
                            installation.root(),
                            installation.locale().unwrap_or(Locale::EnUs),
                        )
                        .map(Message::Patches)
                });

                Task::batch([self.game.watch(selected).map(Message::Game), patches])
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
                        Task::batch([self.rescan(), self.look_at_patches(), watch])
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
                        self.addons.rescanned();
                        self.propose();

                        // What the folder holds changed with its addons.
                        self.clients
                            .changed(&root, Some(&root))
                            .map(Message::Clients)
                    }
                    Err(error) => {
                        self.errors
                            .report(format!("Could not read the client: {error}"));
                        // The cards must not stay off for good.
                        self.addons.rescanned();
                        Task::none()
                    }
                }
            }
            Message::CatalogLoaded(result) => {
                self.refresh = catalog::Refresh::Idle;

                match result {
                    Ok(catalog::Fetched::Unchanged) => {
                        log::info!("The catalog is unchanged");
                    }
                    Ok(catalog::Fetched::Changed { mut catalog, tag }) => {
                        log::info!(
                            "Refreshed the catalog: {}",
                            counted(catalog.entries.len(), "addon")
                        );
                        catalog.prefer(&self.config.sources);
                        self.catalog = catalog;
                        self.tag = tag;
                        self.propose();
                    }
                    Err(error) => {
                        self.errors.report(if self.catalog.entries.is_empty() {
                            format!("Could not fetch the list of addons: {error}")
                        } else {
                            format!(
                                "Could not refresh the list of addons, showing the last one \
                                 fetched: {error}"
                            )
                        });
                    }
                }

                Task::none()
            }
            Message::ModsLoaded(result) => match result {
                Ok(catalog::Fetched::Unchanged) => {
                    log::info!("The list of mods is unchanged");
                    Task::none()
                }
                Ok(catalog::Fetched::Changed { catalog: mods, tag }) => {
                    log::info!(
                        "Refreshed the list of mods: {}",
                        counted(mods.mods.len(), "mod")
                    );
                    self.mods_tag = tag;
                    self.download.mods(mods.clone());
                    self.clients
                        .list_mods(mods, self.selected.path())
                        .map(Message::Clients)
                }
                // Said where the mods are: the clients dialog.
                Err(error) => {
                    log::warn!("Could not fetch the list of mods: {error}");
                    self.clients.mods_unfetched(error);
                    Task::none()
                }
            },
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
                if let Err(error) = std::fs::create_dir_all(&addons) {
                    self.errors
                        .report(format!("Could not open {}: {error}", addons.display()));
                    return Task::none();
                }

                opening(addons, None).map(Message::Unopened)
            }
            Message::Refresh => {
                log::info!("Refreshing on request");
                Task::batch([
                    self.rescan(),
                    self.look_at_patches(),
                    self.refresh_catalog(),
                ])
            }
            Message::OpenRepository => {
                log::info!("Opening {REPOSITORY} in the browser");
                opening(REPOSITORY, None).map(Message::Unopened)
            }
            Message::OpenPage => {
                log::info!("Opening {} in the browser", update::PAGE);
                opening(update::PAGE, None).map(Message::Unopened)
            }
            Message::Unopened(error) => {
                self.errors.report(error);
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
                let playing = self.playing();
                let (installation, snapshot) = self.selected.open();
                let action = self.transfer.update(
                    message,
                    &self.catalog,
                    installation,
                    snapshot,
                    playing,
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
                if self.quitting {
                    self.update(Message::KeepOpen)
                } else if self.download.is_open() {
                    self.update(Message::Download(download::Message::Hide))
                } else if self.setup.is_some() {
                    self.update(Message::Setup(setup::Message::Cancel))
                } else if self.settings.is_open() {
                    self.update(Message::Settings(settings::Message::Close))
                } else if self.clients.is_open() {
                    self.update(Message::Clients(clients::Message::Close))
                } else if self.recognize.details_open() {
                    self.update(Message::Recognize(recognize::Message::Close))
                } else if self.transfer.view(self.playing()).is_some() {
                    self.update(Message::Transfer(transfer::Message::Cancel))
                } else {
                    self.update(Message::Addons(addons::Message::Cancel))
                }
            }
            Message::FocusSearch => operation::focus(addons::SEARCH),
            Message::Quit => {
                if self.under_way().is_empty() {
                    self.download.abandon();
                    return iced::exit();
                }
                self.quitting = true;
                Task::none()
            }
            Message::QuitAnyway => {
                log::info!("Quitting with {} under way", self.under_way().join(", "));
                self.download.abandon();
                iced::exit()
            }
            Message::KeepOpen => {
                self.quitting = false;
                Task::none()
            }
        }
    }

    /// What the launcher is doing to the clients' files that quitting would
    /// cut short, one line each, for people.
    fn under_way(&self) -> Vec<String> {
        self.addons
            .under_way()
            .into_iter()
            .chain(self.clients.under_way())
            .chain(self.transfer.under_way())
            .chain(self.download.under_way())
            .collect()
    }

    /// Draws the launcher.
    pub fn view(&self) -> Element<'_, Message> {
        if let Some(recovery) = &self.recovery {
            return column![
                self.errors.view(Message::DismissError),
                center(scrollable(recovery.view().map(Message::Recovery)).style(style::scroll)),
            ]
            .spacing(14)
            .padding(20)
            .boxed();
        }

        if self.config.installations.is_empty()
            && let Some(setup) = &self.setup
        {
            let (dialog, on_blur) = self
                .foremost()
                .map_or((None, Message::KeepOpen), |(dialog, on_blur)| {
                    (Some(dialog), on_blur)
                });
            // A download hidden from here goes on behind its banner.
            let downloading = self
                .download
                .banner(&self.config)
                .map(|banner| banner.map(Message::Download));

            return style::modal(
                column![
                    downloading,
                    center(style::ornamented(setup.view().map(Message::Setup)))
                ]
                .spacing(14)
                .padding(20),
                dialog,
                on_blur,
            )
            .boxed();
        }

        let title = button(style::heading("Old Launcher", 24.0))
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
                    .style(style::quiet)
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
                    .style(style::tooltip_box),
                tooltip::Position::Bottom,
            ),
            version,
            newer,
            space::horizontal(),
            style::icon_button(
                settings::OPEN,
                &style::COG,
                32.0,
                style::secondary,
                "Settings",
                Some(Message::OpenSettings),
            ),
            button("Manage clients…")
                .style(style::secondary)
                .on_press(Message::ManageClients),
            button(
                text(match game {
                    None => "Play",
                    Some(game::Phase::SettingUp) => "Setting up…",
                    Some(game::Phase::Running) => "Game running",
                })
                .font(style::TITLE)
                .size(18),
            )
            .padding([6, 26])
            .style(style::primary)
            .on_press_maybe(
                (matches!(self.selected, Selected::Open { .. })
                    && game.is_none()
                    && !self.selected.path().is_some_and(|root| {
                        self.clients.holds_back(root)
                            || self.download.holds_back(root)
                            || self.patches.holds_back(root)
                    }))
                .then_some(Message::Play),
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

        let client = match &self.selected {
            Selected::Nothing => text("No client selected.").style(text::secondary).boxed(),
            Selected::Opening(path) => text(format!("Opening {}…", path.display()))
                .style(text::secondary)
                .boxed(),
            Selected::Failed(path) => text(format!("{} is not a usable client.", path.display()))
                .style(text::danger)
                .boxed(),
            Selected::Open {
                installation,
                snapshot,
            } => row![
                style::badge(installation.version().to_string(), style::Tone::Primary),
                installation
                    .locale()
                    .map(|locale| style::badge(locale.code(), style::Tone::Primary)),
                text(installation.root().display().to_string())
                    .size(13)
                    .style(text::secondary)
                    .wrapping(text::Wrapping::None)
                    .ellipsis(text::Ellipsis::Middle)
                    .width(Fill),
                // Until the first scan, the cards can't say what is installed.
                snapshot
                    .is_none()
                    .then(|| text("Reading the addons…").size(13).style(text::secondary)),
                style::tip(
                    match self.refresh {
                        catalog::Refresh::Idle => button("Refresh")
                            .style(style::quiet)
                            .on_press(Message::Refresh),
                        catalog::Refresh::Running => button("Refreshing…").style(style::quiet),
                    },
                    "Shortcut: F5",
                ),
                button("Open AddOns folder")
                    .style(style::quiet)
                    .on_press(Message::OpenAddOns),
                style::tip(
                    button("Export…").style(style::quiet).on_press_maybe(
                        (snapshot_ready && !self.transfer.busy())
                            .then_some(Message::Transfer(transfer::Message::Export)),
                    ),
                    "Saves the list of addons and their saved settings to a zip file, to \
                     import on another computer. The files in it are named after your \
                     account and characters.",
                ),
                style::tip(
                    button("Import…").style(style::quiet).on_press_maybe(
                        (snapshot_ready && !self.transfer.busy())
                            .then_some(Message::Transfer(transfer::Message::Import)),
                    ),
                    "Installs the addons listed in an export and restores their saved \
                     settings, after showing what it will do.",
                ),
            ]
            .spacing(8)
            .align_y(Center)
            .boxed(),
        };

        let error = self.errors.view(Message::DismissError);

        let notice = self.notice.as_deref().map(|notice| {
            container(
                row![
                    text(notice).width(Fill),
                    button("Dismiss")
                        .style(style::quiet)
                        .on_press(Message::DismissNotice),
                ]
                .spacing(8)
                .align_y(Center),
            )
            .padding([6, 12])
            .style(style::banner(style::Tone::Success))
        });

        let downloading = self
            .download
            .banner(&self.config)
            .map(|banner| banner.map(Message::Download));

        let looking = self
            .selected
            .path()
            .and_then(|root| self.patches.banner(root))
            .map(|banner| banner.map(Message::Patches));

        let graphics = self
            .clients
            .banner(self.selected.path(), game.is_some())
            .map(|banner| banner.map(Message::Clients));

        let proposal = self
            .recognize
            .banner()
            .map(|banner| banner.map(Message::Recognize));

        let (installation, snapshot) = self.selected.open();

        let body = self
            .addons
            .view(
                &self.catalog,
                installation,
                snapshot,
                self.refresh,
                game.is_some(),
            )
            .map(Message::Addons);

        // The addons are always the second child, whatever the banners above
        // do, so they keep their widget state (the scroll position) when the
        // banners come and go. A keyed column can't be trusted with this:
        // iced's keyed diff mistakes a child inserted before the last one for
        // one appended after it, and hands out the wrong widget states.
        let content = column![
            column![
                header,
                client,
                error,
                notice,
                downloading,
                looking,
                graphics,
                proposal
            ]
            .spacing(14),
            container(body).height(Fill),
        ]
        .spacing(14)
        .padding(20);

        let (dialog, on_blur) = if let Some((dialog, on_blur)) = self.foremost() {
            (Some(dialog), on_blur)
        } else if let Some(setup) = &self.setup {
            // Added from the clients dialog, which comes back afterwards.
            (
                Some(setup.view().map(Message::Setup).boxed()),
                Message::Setup(setup::Message::Cancel),
            )
        } else if self.settings.is_open() {
            (
                Some(
                    self.settings
                        .view(&self.config, &self.github)
                        .map(Message::Settings)
                        .boxed(),
                ),
                Message::Settings(settings::Message::Close),
            )
        } else if self.clients.is_open() {
            (
                Some(
                    self.clients
                        .view(
                            &self.config,
                            self.selected.path(),
                            installation,
                            game.is_some(),
                            self.download.is_running(),
                            self.selected
                                .path()
                                .and_then(|root| self.download.integrity(root)),
                        )
                        .map(Message::Clients)
                        .boxed(),
                ),
                Message::Clients(clients::Message::Close),
            )
        } else if self.recognize.details_open() {
            (
                self.recognize
                    .dialog(&self.catalog)
                    .map(|dialog| dialog.map(Message::Recognize).boxed()),
                Message::Recognize(recognize::Message::Close),
            )
        } else {
            (
                self.transfer
                    .view(game.is_some())
                    .map(|dialog| dialog.map(Message::Transfer).boxed()),
                Message::Transfer(transfer::Message::Cancel),
            )
        };

        style::modal(content, dialog, on_blur).boxed()
    }

    /// The dialog over every other, if any, and what clicking beside it
    /// sends: whether to quit, over whatever dialog the work goes on in, or
    /// how a download goes, which the work goes on without.
    fn foremost(&self) -> Option<(Element<'_, Message>, Message)> {
        if self.quitting {
            return Some((quit_dialog(self.under_way()).boxed(), Message::KeepOpen));
        }
        let playing = self
            .download
            .root()
            .is_some_and(|root| self.game.phase(root).is_some());

        self.download.dialog(playing).map(|dialog| {
            (
                dialog.map(Message::Download).boxed(),
                Message::Download(download::Message::Hide),
            )
        })
    }

    /// Keyboard shortcuts (see [`shortcut`]), the window asking to close or
    /// getting the focus back, the games being followed, and the downloads.
    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            keyboard::listen().filter_map(shortcut),
            // Closing the window quits as Ctrl+Q does (see `main`).
            window::close_requests().map(|_| Message::Quit),
            event::listen_with(|event, _status, _window| match event {
                Event::Window(window::Event::Focused) => Some(Message::Focused(true)),
                Event::Window(window::Event::Unfocused) => Some(Message::Focused(false)),
                _ => None,
            }),
            self.game.subscription().map(Message::Game),
            self.download.subscription().map(Message::Download),
        ])
    }

    /// Fetches the catalog, unless it was fetched in the last
    /// [`REFRESH_PAUSE`].
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

        let location = catalog::Location::configured::<Catalog>();
        let mods = catalog::Location::configured::<Mods>();
        // The tags and the copies kept belong to the published ones only.
        let (tag, cache) = if location.is_published::<Catalog>() {
            (self.tag.clone(), Some(self.cache.clone()))
        } else {
            (None, None)
        };
        let (mods_tag, mods_cache) = if mods.is_published::<Mods>() {
            (self.mods_tag.clone(), Some(self.cache.clone()))
        } else {
            (None, None)
        };

        log::info!(
            "Checking whether the catalog changed at {location}, and the list of mods at {mods}"
        );
        self.refresh = catalog::Refresh::Running;
        self.refreshed = Some(Instant::now());

        Task::batch([
            Task::perform(
                location.fetch(self.client.clone(), tag, cache),
                Message::CatalogLoaded,
            ),
            Task::perform(
                mods.fetch(self.client.clone(), mods_tag, mods_cache),
                Message::ModsLoaded,
            ),
        ])
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

    /// Adds `installation`, which opened, and shows it.
    fn add(&mut self, installation: Installation) -> Task<Message> {
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

        Task::batch([read, self.rescan(), self.look_at_patches(), self.save()])
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

    /// The files of the client at `root` changed: what the clients dialog
    /// says of it is read again, and its addons, if it is shown.
    fn files_changed(&mut self, root: &Path) -> Task<Message> {
        let current = self.selected.path().map(Path::to_path_buf);
        let changed = self
            .clients
            .changed(root, current.as_deref())
            .map(Message::Clients);
        let (rescan, patches) = if current.as_deref() == Some(root) {
            (self.rescan(), self.look_at_patches())
        } else {
            (Task::none(), Task::none())
        };

        Task::batch([changed, rescan, patches])
    }

    /// Looks at the selected client's patches again, in the background.
    fn look_at_patches(&mut self) -> Task<Message> {
        let Selected::Open { installation, .. } = &self.selected else {
            return Task::none();
        };
        let root = installation.root().to_path_buf();
        let locale = installation.locale().unwrap_or(Locale::EnUs);
        let task = self.patches.look(&root, locale).map(Message::Patches);
        self.sync_patches();

        task
    }

    /// Tells the mods which clients' patches are being looked at.
    fn sync_patches(&mut self) {
        self.clients.waiting_for_patches(self.patches.waiting());
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

    /// Whether the selected client's game is being set up or running, so its
    /// settings files are the game's to write.
    fn playing(&self) -> bool {
        self.selected
            .path()
            .and_then(|path| self.game.phase(path))
            .is_some()
    }

    /// Finds how downloads from GitHub sign in: with a token in the
    /// environment, or else with the GitHub CLI's login when the settings
    /// allow it, which takes asking the CLI in the background.
    fn sign_in(&mut self) -> Task<Message> {
        if let Some(login) = github::from_environment(|variable| std::env::var(variable).ok()) {
            self.signed_in(login);
            Task::none()
        } else if self.config.github_cli {
            // A login the CLI gave before stays in use until it answers again.
            if !matches!(self.github, github::Login::Cli(_)) {
                self.signed_in(github::Login::Asking);
            }
            Task::perform(github::ask_cli(), Message::SignedIn)
        } else {
            self.signed_in(github::Login::Anonymous);
            Task::none()
        }
    }

    /// Downloads started from now on sign in to GitHub with `login`.
    fn signed_in(&mut self, login: github::Login) {
        if login != self.github {
            // The status names where a token comes from, never the token.
            log::info!("Downloads from GitHub: {}", login.status());
        }

        let signed = login
            .token()
            .cloned()
            .zip(self.direct.clone())
            .map(|(token, client)| github::Signed::new(client, token));
        self.clients.sign_in(signed.clone());
        self.addons.sign_in(signed);
        self.github = login;
    }
}

/// The dialog asking whether to quit while the `under_way` work isn't
/// finished. Should it all finish meanwhile, it says so.
fn quit_dialog<'a>(under_way: Vec<String>) -> impl Widget<Message> + 'a {
    let finished = under_way.is_empty();
    let explanation = if finished {
        "Everything is finished now."
    } else {
        "Quitting stops this halfway. An addon cut short may need installing again, a mod \
         repairing before the game starts, and a download resuming."
    };

    container(
        column![
            style::heading("Quit now?", 20.0),
            (!finished).then(|| {
                container(column(under_way.into_iter().map(|line| text(line).size(13))).spacing(2))
                    .padding(10)
                    .width(Fill)
                    .style(style::inset)
            }),
            text(explanation).style(text::secondary),
            row![
                space::horizontal(),
                button("Keep the launcher open")
                    .style(style::quiet)
                    .on_press(Message::KeepOpen),
                button(if finished { "Quit" } else { "Quit anyway" })
                    .style(if finished {
                        style::primary
                    } else {
                        style::danger
                    })
                    .on_press(Message::QuitAnyway),
            ]
            .spacing(8),
        ]
        .spacing(14),
    )
    .padding(24)
    .width(Fill.max(560))
    .style(style::card)
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

/// How every HTTP client of the launcher starts. A connection that never
/// comes, or a server that stops sending, ends the request rather than
/// hanging the catalog refresh or a download forever. A slow but flowing
/// download is fine.
fn http_client() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .user_agent(concat!("old_launcher/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
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

/// Opens `target`, a web page, folder or file, with the program the system
/// opens it with, or with `program` (Notepad, say). Finding one that takes
/// it takes a while (`open` gives each one it tries a quarter of a second
/// to fail), so it happens away from the window: waiting on it froze the
/// window, and what was typed or moved meanwhile came all at once, more than
/// iced's subscriptions take. Only a failure comes back, for the banner.
fn opening(target: impl Into<OsString>, program: Option<&'static str>) -> Task<String> {
    let target = target.into();

    Task::perform(
        async move {
            let shown = target.to_string_lossy().into_owned();
            let opened = tokio::task::spawn_blocking(move || match program {
                Some(program) => open::with_detached(&target, program),
                None => open::that_detached(&target),
            })
            .await;

            match opened {
                Ok(Ok(())) => None,
                Ok(Err(error)) => Some(format!("Could not open {shown}: {error}")),
                Err(error) => Some(format!("Could not open {shown}: {error}")),
            }
        },
        std::convert::identity,
    )
    .and_then(Task::done)
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
    use iced_test::{selector, simulator};

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
            direct: None,
            github: github::Login::Anonymous,
            settings: settings::State::default(),
            catalog: Catalog::default(),
            refresh: catalog::Refresh::Idle,
            refreshed: None,
            tag: None,
            mods_tag: None,
            cache: catalog::Cache::at("cache".into()),
            selected: Selected::Nothing,
            addons: addons::State::new(),
            setup: None,
            recovery: None,
            errors: Errors::default(),
            notice: None,
            recognize: recognize::State::default(),
            transfer: transfer::State::default(),
            clients: clients::State::default(),
            download: download::State::new(PathBuf::from("torrents")),
            game: game::State::default(),
            patches: patches::State::new(core::patches::Cache::at("mpq")),
            latest: None,
            quitting: false,
            focused: true,
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

    /// The mods of `app`'s client at `root` were looked at: none there.
    fn mods_looked_at(app: &mut App, root: &Path) {
        let _ = app.update(Message::Clients(clients::Message::Mods(
            clients::mods::Message::Found(root.to_path_buf(), clients::mods::Seen::default()),
        )));
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
    fn quitting_asks_first_while_something_it_would_cut_short_is_under_way() {
        let (_root, installation) = core::testing::client(&["Interface/AddOns/ACP/ACP.toc"]);
        let mut app = opened(installation);

        let _ = app.update(Message::Quit);
        assert!(!app.quitting, "nothing under way: it just quits");

        let _ = app.update(Message::Addons(addons::Message::Ask(
            addons::Confirm::Uninstall {
                title: "ACP".into(),
                folders: vec!["ACP".into()],
                data: Vec::new(),
                delete_data: false,
                required_by: Vec::new(),
                optionally_used_by: Vec::new(),
            },
        )));
        let _ = app.update(Message::Addons(addons::Message::Confirm));
        let _ = app.update(Message::Quit);
        assert!(app.quitting);

        let messages: Vec<Message> = {
            let mut ui = simulator(app.view());
            for wanted in ["Quit now?", "Uninstalling ACP: 1 folder", "Quit anyway"] {
                assert!(ui.find(wanted).is_ok(), "{wanted}");
            }
            let _ = ui.click("Keep the launcher open").unwrap();
            ui.into_messages().collect()
        };
        assert!(matches!(messages[..], [Message::KeepOpen]));
        let _ = app.update(Message::KeepOpen);
        assert!(!app.quitting);

        // Finished while asking: it says so.
        let _ = app.update(Message::Quit);
        let _ = app.update(Message::Addons(addons::Message::Done(Ok(()))));
        let mut ui = simulator(app.view());
        assert!(ui.find("Everything is finished now.").is_ok());
        assert!(ui.find("Quit").is_ok());
        drop(ui);

        let _ = app.update(Message::Escape);
        assert!(!app.quitting, "Escape keeps it open too");
    }

    #[test]
    fn a_client_from_a_torrent_has_its_files_checked_and_one_downloading_resumed() {
        let (_root, installation) = core::testing::client(&[]);
        let root = installation.root().to_path_buf();
        let mut app = opened(installation);
        mods_looked_at(&mut app, &root);
        let torrent = |name: &str, info_hash: &str, folder: &Path| core::config::ClientTorrent {
            name: name.into(),
            info_hash: info_hash.into(),
            source: format!("magnet:?xt=urn:btih:{info_hash}"),
            folder: folder.to_path_buf(),
            languages: Vec::new(),
            made: false,
        };
        let _ = app
            .config
            .torrents
            .insert(root.clone(), torrent("WotLK", "c0ffee", &root));
        app.config
            .downloads
            .push(torrent("HD", "decaf", Path::new("/games/HD")));

        let messages: Vec<Message> = {
            let mut ui = simulator(app.view());
            assert!(ui.find("HD is not fully downloaded yet.").is_ok());
            let _ = ui.click("Resume download").unwrap();
            let _ = ui.click("Play").unwrap();
            ui.into_messages().collect()
        };
        assert!(
            matches!(
                messages.as_slice(),
                [
                    Message::Download(download::Message::Resume(hash)),
                    Message::Play
                ] if hash == "decaf"
            ),
            "a client downloading is not one yet: the others play"
        );

        let _ = app.update(Message::ManageClients);
        let messages: Vec<Message> = {
            let mut ui = simulator(app.view());
            let _ = ui.click("Check").unwrap();
            ui.into_messages().collect()
        };
        for message in messages {
            let _ = app.update(message);
        }

        assert!(app.download.is_open());
        let mut ui = simulator(app.view());
        assert!(ui.find("The files of WotLK").is_ok());
        assert!(ui.find("Finding the torrent…").is_ok());
    }

    #[test]
    fn a_client_gets_its_torrent_back_from_its_folder_or_is_given_one() {
        let (_root, installation) = core::testing::client(&[]);
        let root = installation.root().to_path_buf();
        let mut app = opened(installation);
        let _ = app.update(Message::ManageClients);

        // Forgotten and added back: its folder still notes its torrent.
        let torrent = core::config::ClientTorrent {
            name: "WotLK".into(),
            info_hash: "0123456789abcdef0123456789abcdef01234567".into(),
            source: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
            folder: root.clone(),
            languages: Vec::new(),
            made: false,
        };
        let details = core::Details {
            torrent: Some(torrent.clone()),
            ..core::Details::read(&root).unwrap()
        };
        let _ = app.update(Message::Clients(clients::Message::Read(
            root.clone(),
            Ok(details),
        )));
        assert_eq!(app.config.torrents.get(&root), Some(&torrent));

        app.config.torrents.clear();
        for message in [
            clients::Message::SetSource,
            clients::Message::SourceChanged("magnet:?xt=urn:btih:c0ffee".into()),
            clients::Message::SubmitSource,
        ] {
            let _ = app.update(Message::Clients(message));
        }
        assert!(app.download.is_open());
        let mut ui = simulator(app.view());
        assert!(ui.find("Finding the torrent…").is_ok());
    }

    #[test]
    fn a_torrent_from_the_first_run_form_downloads_over_it_or_behind_a_banner() {
        let mut app = app(&[]);
        app.setup = Some(setup::State::new(false));
        for message in [
            setup::Message::Mode(setup::Mode::Torrent),
            setup::Message::TorrentChanged("magnet:?xt=urn:btih:c0ffee".into()),
            setup::Message::IntoChanged("/games".into()),
            setup::Message::Submit,
        ] {
            let _ = app.update(Message::Setup(message));
        }
        assert!(
            app.setup.is_some(),
            "the form stays until there is a client"
        );
        let mut ui = simulator(app.view());
        assert!(ui.find("Finding the torrent…").is_ok());
        drop(ui);

        let _ = app.update(Message::Escape);
        let mut ui = simulator(app.view());
        for line in ["Add a WoW 3.3.5a client", "Looking at the torrent…", "Show"] {
            assert!(ui.find(line).is_ok(), "{line}");
        }
        drop(ui);

        // Another torrent waits for this one.
        let _ = app.update(Message::Setup(setup::Message::Submit));
        let mut ui = simulator(app.view());
        assert!(ui.find(ONE_DOWNLOAD).is_ok());
    }

    #[test]
    fn a_refresh_fetches_the_catalog_again_only_after_a_pause() {
        let mut app = app(&[]);
        app.refresh = catalog::Refresh::Idle;

        // Just fetched: left alone.
        app.refreshed = Some(Instant::now());
        let _ = app.update(Message::Refresh);
        assert_eq!(app.refresh, catalog::Refresh::Idle);

        // Long enough ago: fetched again.
        app.refreshed = Instant::now().checked_sub(REFRESH_PAUSE);
        let _ = app.update(Message::Refresh);
        assert_eq!(app.refresh, catalog::Refresh::Running);
    }

    /// A catalog listing ACP from two sources, the newest first.
    fn two_sources() -> Catalog {
        Catalog::parse(
            br#"{ "version": 2,
                  "sources": [
                    { "id": "test", "name": "Test Source", "homepage": "https://example.com" },
                    { "id": "other", "name": "Other Source", "homepage": "https://example.org" }
                  ],
                  "entries": [{ "name": "ACP", "downloads": [
                    { "source": "test", "name": "ACP", "url": "https://example.com/ACP.zip",
                      "folders": [{ "name": "ACP" }] },
                    { "source": "other", "name": "ACP", "url": "https://example.org/ACP.zip",
                      "folders": [{ "name": "ACP" }] }
                  ] }] }"#,
        )
        .unwrap()
    }

    #[test]
    fn a_source_picked_for_an_addon_is_saved_and_survives_a_refresh() {
        let mut app = app(&[]);
        app.catalog = two_sources();
        let key = addons::Key::of(&app.catalog.entries[0]);
        let first = |app: &App| app.catalog.entries[0].preferred().unwrap().source.clone();

        let _ = app.update(Message::Addons(addons::Message::SourceSelected(
            key.clone(),
            addons::Choice::Source("other".into()),
        )));
        assert_eq!(
            app.config.sources.get("acp").map(String::as_str),
            Some("other")
        );
        assert_eq!(first(&app), "other");

        let _ = app.update(Message::CatalogLoaded(Ok(catalog::Fetched::Changed {
            catalog: two_sources(),
            tag: None,
        })));
        assert_eq!(first(&app), "other", "kept across refreshes");

        let _ = app.update(Message::Addons(addons::Message::SourceSelected(
            key,
            addons::Choice::Auto,
        )));
        assert!(app.config.sources.is_empty());
        assert_eq!(first(&app), "test");
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
        assert!(ui.find("Reading the addons…").is_ok());
        let _ = ui.click("Export…");
        assert_eq!(ui.into_messages().count(), 0, "still reading the client");

        app.selected = Selected::Open {
            snapshot: Some(snapshot),
            installation,
        };
        let mut ui = simulator(app.view());
        assert!(ui.find("Reading the addons…").is_err());
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

    #[test]
    fn the_refresh_button_names_its_shortcut_on_hover() {
        let (_root, installation) = crate::testing::client(&[]);
        let app = opened(installation);

        let mut ui = simulator(app.view());
        let button = ui.find("Refresh").unwrap().bounds().center();
        ui.point_at(button);
        let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: button,
        })]);

        assert!(ui.find("Shortcut: F5").is_ok());
    }

    #[test]
    fn the_cog_says_settings_on_hover_and_opens_them() {
        let (_root, installation) = crate::testing::client(&[]);
        let mut app = opened(installation);

        let mut ui = simulator(app.view());
        let cog = ui
            .find(selector::id(settings::OPEN))
            .unwrap()
            .bounds()
            .center();
        ui.point_at(cog);
        let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: cog,
        })]);
        assert!(ui.find("Settings").is_ok(), "the hint");

        let _ = ui.click(selector::id(settings::OPEN)).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            let _ = app.update(message);
        }

        assert!(app.settings.is_open());
        let mut ui = simulator(app.view());
        assert!(ui.find(settings::USE_CLI).is_ok());
    }

    #[test]
    fn focus_gained_twice_watches_once() {
        let (_root, installation) = crate::testing::client(&[]);
        let mut app = opened(installation);

        assert_eq!(
            app.update(Message::Focused(true)).units(),
            0,
            "it had the focus already"
        );
        assert_eq!(app.update(Message::Focused(false)).units(), 0);
        assert!(app.update(Message::Focused(true)).units() > 0, "regained");
        assert_eq!(
            app.update(Message::Focused(true)).units(),
            0,
            "the second report of the same return"
        );
    }

    #[test]
    fn a_language_switched_in_the_clients_dialog_shows_in_the_header() {
        let (_root, installation) =
            crate::testing::client(&["Data/enUS/realmlist.wtf", "Data/esES/realmlist.wtf"]);
        let mut app = opened(installation.clone());
        let _ = app.update(Message::ManageClients);
        let mut switched = installation;
        switched.set_locale(Locale::EsEs).unwrap();

        let _ = app.update(Message::Clients(clients::Message::Switched(
            Locale::EsEs,
            Ok(switched),
        )));
        let _ = app.update(Message::Escape);

        let mut ui = simulator(app.view());
        assert!(ui.find("esES").is_ok());
        assert!(ui.find("enUS").is_err());
    }

    #[test]
    fn ticking_the_cli_box_is_saved_and_signs_in_with_it() {
        let mut app = app(&["/games/wotlk"]);

        let _ = app.update(Message::Settings(settings::Message::UseCli(true)));

        assert!(app.config.github_cli);
        // A token in the environment, as CI may have, comes first.
        assert!(
            matches!(
                app.github,
                github::Login::Asking | github::Login::Environment { .. }
            ),
            "{:?}",
            app.github
        );

        let _ = app.update(Message::Settings(settings::Message::UseCli(false)));

        assert!(!app.config.github_cli);
        assert!(
            matches!(
                app.github,
                github::Login::Anonymous | github::Login::Environment { .. }
            ),
            "{:?}",
            app.github
        );
    }

    #[test]
    fn ticking_the_discord_box_is_saved() {
        let mut app = app(&["/games/wotlk"]);
        assert!(!app.config.discord, "off until ticked");

        let _ = app.update(Message::Settings(settings::Message::ShowOnDiscord(true)));
        assert!(app.config.discord);

        let _ = app.update(Message::Settings(settings::Message::ShowOnDiscord(false)));
        assert!(!app.config.discord);
    }

    #[test]
    fn the_clis_answer_counts_only_while_asked_for() {
        let mut app = app(&["/games/wotlk"]);
        app.config.github_cli = false;

        let _ = app.update(Message::SignedIn(github::Login::CliMissing));
        assert_eq!(app.github, github::Login::Anonymous, "unticked meanwhile");

        app.config.github_cli = true;
        app.github = github::Login::Asking;
        let _ = app.update(Message::SignedIn(github::Login::CliMissing));
        assert_eq!(app.github, github::Login::CliMissing);

        // Asked again while signed in, as when the settings open.
        app.github = github::Login::Cli(github::Token::new("secret"));
        let _ = app.update(Message::SignedIn(github::Login::CliMissing));
        assert_eq!(app.github, github::Login::CliMissing, "logged out since");
    }

    #[test]
    fn the_cli_login_stays_in_use_while_the_cli_is_asked_again() {
        let mut app = app(&["/games/wotlk"]);
        app.config.github_cli = true;
        app.github = github::Login::Cli(github::Token::new("secret"));

        let _ = app.update(Message::OpenSettings);

        // A token in the environment, as CI may have, comes first.
        assert!(
            matches!(
                app.github,
                github::Login::Cli(_) | github::Login::Environment { .. }
            ),
            "{:?}",
            app.github
        );
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
        mods_looked_at(&mut app, &root);

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

    #[test]
    fn the_game_waits_for_the_launcher_to_check_a_mod() {
        let (_root, installation) = crate::testing::client(&["CoAVolFog.dll"]);
        let root = installation.root().to_path_buf();
        let mut app = opened(installation);
        let json = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/index/mods.json")).unwrap();
        let _ = app.update(Message::ModsLoaded(Ok(catalog::Fetched::Changed {
            catalog: Mods::parse(&json).unwrap(),
            tag: None,
        })));
        let found = |release: Option<&str>, problems| {
            Message::Clients(clients::Message::Mods(clients::mods::Message::Found(
                root.clone(),
                clients::mods::Seen {
                    held: std::collections::BTreeMap::from([(
                        "lumenveil".to_owned(),
                        crate::core::mods::Status {
                            release: release.map(str::to_owned),
                            problems,
                        },
                    )]),
                    foreign: std::collections::BTreeMap::new(),
                    unfit: Default::default(),
                    excluded: Default::default(),
                    lacking: Default::default(),
                },
            )))
        };

        // Not looked at yet: the game waits.
        assert!(!play_button(&app, "Play"));
        let _ = app.update(Message::Play);
        assert!(app.game.phase(&root).is_none(), "nothing was started");

        // Found needing a repair: it waits still.
        let _ = app.update(found(
            Some("0.1.1"),
            vec![crate::core::mods::Problem::Missing(vec![
                "version.dll".to_owned(),
            ])],
        ));
        let mut ui = simulator(app.view());
        assert!(
            ui.find("The launcher needs to repair “Lumenveil” before starting the game. Missing: version.dll.")
                .is_ok()
        );
        drop(ui);
        assert!(!play_button(&app, "Play"));

        // Found as released: Wine loads its DLLs.
        let _ = app.update(found(Some("0.1.1"), Vec::new()));
        assert!(play_button(&app, "Play"));
        assert_eq!(app.clients.natives(&root), ["CoAVolFog", "version"]);

        // Installed by hand: the player's, with whichever DLLs it has.
        let _ = app.update(found(
            None,
            vec![crate::core::mods::Problem::Unknown {
                version: Some("0.2.0".to_owned()),
                natives: vec!["CoAVolFog".to_owned()],
            }],
        ));
        assert!(play_button(&app, "Play"));
        assert_eq!(app.clients.natives(&root), ["CoAVolFog"]);
    }

    /// A launcher that opens `installation` as it does at start (its patches
    /// are looked at, but the task doing it isn't run), keeping what it reads
    /// of the archives in `cache`, with the list of mods.
    fn opening(installation: Installation, cache: &Path) -> App {
        let root = installation.root().to_path_buf();
        let mut app = app(&[&root.display().to_string()]);
        app.patches = patches::State::new(core::patches::Cache::at(cache));
        app.selected = Selected::Opening(root.clone());
        let json = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/index/mods.json")).unwrap();
        let _ = app.update(Message::ModsLoaded(Ok(catalog::Fetched::Changed {
            catalog: Mods::parse(&json).unwrap(),
            tag: None,
        })));
        let _ = app.update(Message::Opened(root, Ok(installation)));

        app
    }

    /// The look at `root`'s patches ended, finding what is there and keeping
    /// what it read in `cache`.
    fn looked_at_patches(app: &mut App, root: &Path, cache: &Path) {
        let scan = patches::look(
            root,
            Locale::EnUs,
            &core::patches::Cache::at(cache),
            &|_| {},
        );
        let _ = app.update(Message::Patches(patches::Message::Looked(
            root.to_path_buf(),
            scan.map(std::sync::Arc::new),
        )));
    }

    #[test]
    fn play_and_the_mods_buttons_wait_for_the_patches_to_be_looked_at() {
        let (_root, installation) = crate::testing::client(&["Data/patch-a.mpq"]);
        let root = installation.root().to_path_buf();
        let folder = tempfile::tempdir().unwrap();
        let mut app = opening(installation, folder.path());
        mods_looked_at(&mut app, &root);
        let _ = app.config.torrents.insert(
            root.clone(),
            core::config::ClientTorrent {
                name: "WotLK".into(),
                info_hash: "c0ffee".into(),
                source: "magnet:?xt=urn:btih:c0ffee".into(),
                folder: root.clone(),
                languages: Vec::new(),
                made: false,
            },
        );
        let pressed = |app: &App, label: &str| {
            let mut ui = simulator(app.view());
            let _ = ui.click(label).unwrap();
            ui.into_messages().count() > 0
        };

        // From the moment it opens, nothing the patches could change goes on.
        assert!(app.patches.holds_back(&root));
        let _ = app.update(Message::Play);
        assert!(app.game.phase(&root).is_none(), "nothing was started");
        assert!(!play_button(&app, "Play"));
        let _ = app.update(Message::ManageClients);
        let mut ui = simulator(app.view());
        assert!(ui.find("Looking…").is_ok(), "the mods' rows");
        drop(ui);
        assert!(!pressed(&app, "Check"));

        // The look ends: they are free.
        looked_at_patches(&mut app, &root, folder.path());
        assert!(!app.patches.holds_back(&root));
        assert!(app.patches.graph(&root).is_some());
        let mut ui = simulator(app.view());
        assert!(ui.find("Looking…").is_err());
        assert!(ui.find("Not installed.").is_ok());
        drop(ui);
        assert!(pressed(&app, "Check"));
        let _ = app.update(Message::Escape);
        assert!(play_button(&app, "Play"));
    }

    #[test]
    fn a_scan_that_fails_releases_play_and_reports() {
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root().to_path_buf();
        std::fs::write(root.join("Data"), b"a file, not a folder").unwrap();
        let folder = tempfile::tempdir().unwrap();
        let mut app = opening(installation, folder.path());
        mods_looked_at(&mut app, &root);
        assert!(!play_button(&app, "Play"));

        let scan = patches::look(
            &root,
            Locale::EnUs,
            &core::patches::Cache::at(folder.path()),
            &|_| {},
        );
        assert!(scan.is_err());
        let _ = app.update(Message::Patches(patches::Message::Looked(
            root.clone(),
            scan.map(std::sync::Arc::new),
        )));

        assert!(!app.patches.holds_back(&root));
        assert!(app.patches.graph(&root).is_none());
        assert!(play_button(&app, "Play"), "never locked out");
        let [error] = app.errors.messages() else {
            panic!("{:?}", app.errors.messages());
        };
        assert!(
            error.starts_with("Could not look at the patches of "),
            "{error}"
        );
    }

    #[test]
    fn the_banner_appears_with_the_first_progress_after_two_seconds_and_not_before() {
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root().to_path_buf();
        let folder = tempfile::tempdir().unwrap();
        let mut app = opening(installation, folder.path());
        let progress = |seconds: f32, index| {
            Message::Patches(patches::Message::Progressed(
                root.clone(),
                patches::Progress {
                    elapsed: Duration::from_secs_f32(seconds),
                    archive: "Data/patch-H.mpq".to_owned(),
                    index,
                    total: 35,
                    step: patches::Step::Files(200),
                },
            ))
        };
        let banner = "Looking at this client's patches: Data/patch-H.mpq, 12 of 35…";

        let _ = app.update(progress(1.5, 11));
        assert!(simulator(app.view()).find(banner).is_err());
        let _ = app.update(progress(2.5, 12));
        let mut ui = simulator(app.view());
        assert!(ui.find(banner).is_ok());
        assert!(ui.find("Play").is_ok(), "and Play waits all along");
        drop(ui);
        assert!(!play_button(&app, "Play"));

        looked_at_patches(&mut app, &root, folder.path());
        assert!(simulator(app.view()).find(banner).is_err());

        // A look that ends sooner shows nothing.
        let _ = app.update(Message::Refresh);
        let _ = app.update(progress(0.5, 1));
        assert!(simulator(app.view()).find(banner).is_err());
        assert!(app.patches.holds_back(&root));
    }

    #[test]
    fn regaining_the_focus_looks_again_only_when_data_changed() {
        let (_root, installation) = crate::testing::client(&["Data/patch-a.mpq"]);
        let root = installation.root().to_path_buf();
        let folder = tempfile::tempdir().unwrap();
        let mut app = opening(installation, folder.path());
        mods_looked_at(&mut app, &root);
        let listed = || {
            Message::Patches(patches::Message::Listed(
                root.clone(),
                patches::listing(&root, Locale::EnUs).map_err(|error| describe(&error)),
            ))
        };

        // While the first look goes on, the focus changes nothing: it will
        // see what is there.
        let _ = app.update(Message::Focused(false));
        assert_eq!(
            app.update(Message::Focused(true)).units(),
            1,
            "only the games are watched"
        );
        looked_at_patches(&mut app, &root, folder.path());

        let _ = app.update(Message::Focused(false));
        assert_eq!(
            app.update(Message::Focused(true)).units(),
            2,
            "the games, and the Data folders listed"
        );
        assert_eq!(app.update(listed()).units(), 0, "nothing changed");
        assert!(!app.patches.holds_back(&root));

        // An archive came.
        std::fs::write(root.join("Data/patch-b.mpq"), b"x").unwrap();
        assert!(app.update(listed()).units() > 0, "it looks again");
        assert!(app.patches.holds_back(&root));

        // Something else in Data changed: no.
        looked_at_patches(&mut app, &root, folder.path());
        std::fs::write(root.join("Data/notes.txt"), b"x").unwrap();
        assert_eq!(app.update(listed()).units(), 0);
        assert!(!app.patches.holds_back(&root));
    }

    #[test]
    fn removing_a_client_forgets_its_cache() {
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root().to_path_buf();
        let folder = tempfile::tempdir().unwrap();
        let mut app = opening(installation, folder.path());
        mods_looked_at(&mut app, &root);
        std::fs::create_dir_all(root.join("Data")).unwrap();
        crate::core::mpq::writer::Writer::new()
            .file("a.txt", b"a")
            .write(&root.join("Data/patch-a.mpq"));

        // What the look found is kept.
        let cache = core::patches::Cache::at(folder.path());
        assert!(patches::look(&root, Locale::EnUs, &cache, &|_| {}).is_ok());
        let kept = || std::fs::read_dir(folder.path()).unwrap().count();
        assert_eq!(kept(), 1, "a folder for the client");
        let _ = app.update(Message::ManageClients);
        looked_at_patches(&mut app, &root, folder.path());

        let _ = app.update(Message::Clients(clients::Message::Remove));
        let _ = app.update(Message::Clients(clients::Message::ConfirmRemove));

        assert!(app.config.installations.is_empty());
        assert_eq!(kept(), 0, "and gone with the client");
        assert!(app.patches.graph(&root).is_none());

        // A look that ends after it was removed drops what it wrote.
        assert!(patches::look(&root, Locale::EnUs, &cache, &|_| {}).is_ok());
        assert_eq!(kept(), 1);
        let _ = app.update(Message::Patches(patches::Message::Looked(
            root.clone(),
            Err("too late".to_owned()),
        )));
        assert_eq!(kept(), 0);
    }

    #[cfg(not(windows))]
    #[test]
    fn a_command_that_cant_start_the_game_is_pointed_at() {
        let (_root, installation) = crate::testing::client(&[]);
        let root = installation.root().to_path_buf();
        let mut app = opened(installation);
        mods_looked_at(&mut app, &root);

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
                "ACP was installed without the launcher and is available here too. \
                 Recognize it, so it shows as installed and can be updated from here?"
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
