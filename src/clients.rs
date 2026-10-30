//! The clients dialog: which client the launcher shows, what its folder
//! holds, the torrent it comes from, which its files are checked against,
//! outside Windows how the game starts, and its mods.

pub mod mods;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use iced::widget::{
    Column, Row, TextInput, button, checkbox, column, container, operation, pick_list, row,
    scrollable, space, stack, text, text_input,
};
use iced::{Center, Element, Fill, Font, Function, Task, Theme, Widget, padding};

use crate::action::Action;
use crate::core::config::ClientTorrent;
use crate::core::launch;
use crate::core::mods::Mods;
use crate::core::{Config, Details, Installation, Locale, Named, Realmlist, Server};
use crate::errors::Errors;
use crate::torrent::Source;
use crate::{blocking, game, github, size, style};

/// The id of the button that adds a client.
pub const ADD: &str = "clients-add";

/// The id of the button that removes the client shown.
pub const REMOVE: &str = "clients-remove";

/// The id of the button that opens the realmlist in an editor.
pub const EDIT_REALMLIST: &str = "clients-edit-realmlist";

/// The id of the button that reads the realmlist again.
pub const REFRESH_REALMLIST: &str = "clients-refresh-realmlist";

/// The id of the environment variables field.
pub const ENV: &str = "clients-env";

/// The id of the command field.
pub const COMMAND: &str = "clients-command";

/// The id of the field giving the client its torrent.
pub const SOURCE: &str = "clients-source";

/// The warning under a realmlist that sets no server: empty, or without a
/// `set realmlist` line.
const NO_SERVER: &str = "The game can't connect without a server: the file needs a line such as \
                         set realmlist logon.example.com";

/// The warning under a realmlist with a line that is only a host name, which
/// the game skips; `{host}` is that name.
const BARE_HOST: &str = "The game skips “{host}” in realmlist.wtf: a line sets the server only as \
                         set realmlist {host}.";

/// The warning under a client that has no realmlist file at all.
const NO_REALMLIST: &str = "The game can't connect without a realmlist.wtf in the folder of its \
                            language in Data, with a line such as set realmlist logon.example.com";

/// The width and height of the buttons beside the client picker.
const PICKER_BUTTON: f32 = 38.0;

/// The dialog's state, kept between openings: what a client folder holds
/// takes a moment to read, and is remembered until the client changes.
#[derive(Default)]
pub struct State {
    /// Whether the dialog is open.
    open: bool,
    /// What each client folder read so far holds, or why it couldn't be
    /// read; `None` while it is being read for the first time.
    details: BTreeMap<PathBuf, Option<Result<Details, String>>>,
    /// The client folders that changed since they were read: they are read
    /// again when next shown, showing the old reading until the new one.
    stale: BTreeSet<PathBuf>,
    /// Whether how a client starts was changed, so the config needs saving.
    edited: bool,
    /// Whether the client shown is about to be removed, pending a yes.
    removing: bool,
    /// The torrent being given to the client shown, as typed so far,
    /// while the field for it shows.
    source: Option<String>,
    /// Whether a `.torrent` file is being chosen for it.
    choosing: bool,
    /// The mods, with their section and banners.
    mods: mods::State,
}

/// Interactions with the dialog.
#[derive(Debug, Clone)]
pub enum Message {
    /// A client was chosen in the picker.
    Select(PathBuf),
    /// A client folder was read.
    Read(PathBuf, Result<Details, String>),
    /// The pen beside the realmlist was pressed.
    EditRealmlist(PathBuf),
    /// The realmlist couldn't be opened in an editor: why.
    Unopened(String),
    /// The button beside the realmlist that reads it again was pressed.
    RereadRealmlist(PathBuf),
    /// The realmlist of a client folder was read again.
    Reread(PathBuf, Result<Option<Realmlist>, String>),
    /// A language was picked for the client shown.
    Locale(Locale),
    /// The client shown was switched to this language, or couldn't be.
    Switched(Locale, Result<Installation, String>),
    /// The box letting the game use 4 GB of memory was ticked or unticked.
    LargeAddressAware(bool),
    /// `Wow.exe` of a client folder was changed to let the game use 4 GB of
    /// memory, or 2, or couldn't be.
    Patched(PathBuf, bool, Result<(), String>),
    /// The environment variables were edited.
    Env(String),
    /// The command was edited.
    Command(String),
    /// "Check" was pressed beside the file integrity (a client without a
    /// torrent has its patches checked).
    Check,
    /// "Manage…" was pressed beside the languages.
    Languages,
    /// "Set…" or "Change…" was pressed beside the torrent: the field for it
    /// shows.
    SetSource,
    /// The torrent given was edited.
    SourceChanged(String),
    /// "Choose file…" beside it was pressed.
    ChooseSource,
    /// A `.torrent` file was chosen for it, or none.
    ChoseSource(Option<PathBuf>),
    /// "Set" was pressed beside it, or Enter in it.
    SubmitSource,
    /// "Cancel" was pressed beside it.
    CancelSource,
    /// The "+" beside the picker was pressed.
    Add,
    /// The trash can beside the picker was pressed: ask first.
    Remove,
    /// Removing the client shown was confirmed.
    ConfirmRemove,
    /// Removing the client shown was thought better of.
    CancelRemove,
    /// The dialog was closed.
    Close,
    /// Something happened in the mods' section or banners, or their work
    /// ended, which may be while the dialog is closed.
    Mods(mods::Message),
}

/// What the dialog asks of its parent.
pub enum Instruction {
    /// Show the client at this folder.
    Select(PathBuf),
    /// Open the form that adds a client.
    Add,
    /// Check the client at this folder: its patches, and its files against
    /// its torrent if it has one.
    Check(PathBuf),
    /// Check the client at `root` against the torrent of `source` from now
    /// on, and now.
    Adopt { root: PathBuf, source: Source },
    /// Show the languages of the client at `root`, which has `locales` and
    /// whose game plays in `locale`, to download others or delete them.
    Languages {
        root: PathBuf,
        locales: Vec<Locale>,
        locale: Option<Locale>,
    },
    /// The client at this folder comes from this torrent, as its folder
    /// notes, which the launcher didn't know (the client was forgotten and
    /// added back): keep it.
    Restore(PathBuf, ClientTorrent),
    /// Forget the client shown.
    Remove,
    /// The client shown plays in another language from now on: this is it,
    /// switched.
    Switched(Installation),
    /// The dialog closed.
    Close {
        /// Whether how a client starts was changed, so the config needs
        /// saving.
        edited: bool,
    },
    /// A mod was installed in the client at this folder, removed or
    /// repaired: its patches changed with it.
    Changed(PathBuf),
}

impl State {
    /// Opens the dialog on the `current` client, reading its folder if it
    /// wasn't yet or changed since.
    pub fn open(&mut self, current: Option<&Path>) -> Task<Message> {
        self.open = true;
        self.removing = false;
        self.source = None;

        current.map_or_else(Task::none, |root| self.read(root))
    }

    /// Whether the dialog is open.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Downloads from GitHub sign in with `github` from now on.
    pub fn sign_in(&mut self, github: Option<github::Signed>) {
        self.mods.sign_in(github);
    }

    /// The mods' downloads are kept in `folder` from now on (see
    /// [`mods::cache`]).
    pub fn keep_mod_downloads_in(&mut self, folder: PathBuf) {
        self.mods.keep_downloads_in(folder);
    }

    /// What the mods read of the archives of their releases is kept in
    /// `cache` from now on.
    pub fn keep_archive_facts_in(&mut self, cache: crate::core::patches::Cache) {
        self.mods.keep_facts_in(cache);
    }

    /// The patches of the client folder at `root` were looked at and found
    /// `scan`, if the look is over, the client playing in `locale`: what
    /// installing a mod does to them is worked out.
    pub fn patched(
        &mut self,
        root: &Path,
        scan: Option<std::sync::Arc<crate::patches::Scan>>,
        locale: Locale,
    ) -> Task<Message> {
        self.mods.patched(root, scan, locale).map(Message::Mods)
    }

    /// The patches of the client folders at `roots` are being looked at, and
    /// no others: the mods and the file check there wait for it.
    pub fn waiting_for_patches(&mut self, roots: BTreeSet<PathBuf>) {
        self.mods.waiting_for_patches(roots);
    }

    /// Takes `list` as the list of mods, and looks at the `current` client
    /// again with it.
    pub fn list_mods(&mut self, list: Mods, current: Option<&Path>) -> Task<Message> {
        self.mods.list(list, current).map(Message::Mods)
    }

    /// Notes why the list of mods couldn't be fetched, which the dialog says
    /// while there is none.
    pub fn mods_unfetched(&mut self, error: String) {
        self.mods.unfetched(error);
    }

    /// Closes the dialog without a word to the parent: there is nothing
    /// left to manage.
    pub fn close(&mut self) {
        self.open = false;
        self.removing = false;
        self.source = None;
    }

    /// Notes that the client folder at `root` was just read, or changed
    /// (addons came or went), so what it holds is read again: now, if the
    /// dialog shows it (`current`), or when it next does. Its mods are
    /// looked for at once, for the banners above the addons.
    pub fn changed(&mut self, root: &Path, current: Option<&Path>) -> Task<Message> {
        let _ = self.stale.insert(root.to_path_buf());

        let read = if self.open && current == Some(root) {
            self.read(root)
        } else {
            Task::none()
        };

        Task::batch([read, self.mods.find(root).map(Message::Mods)])
    }

    /// Repairs the mods in the client at `root` that need it once it is
    /// next looked at: a language came, which they want their files in.
    pub fn complete_mods(&mut self, root: &Path) {
        self.mods.complete(root);
    }

    /// Whether starting the game of the client at `root` waits for the
    /// launcher to check or repair a mod in it, as its banner says.
    pub fn holds_back(&self, root: &Path) -> bool {
        self.mods.holds_back(root)
    }

    /// What is being done to a mod that quitting would cut short (see
    /// [`mods::State::under_way`]).
    pub fn under_way(&self) -> Option<String> {
        self.mods.under_way()
    }

    /// The DLLs the mods in the client at `root` put beside `Wow.exe`, which
    /// Wine has to be told to load from there (see [`mods::State::natives`]).
    pub fn natives(&self, root: &Path) -> Vec<String> {
        self.mods.natives(root)
    }

    /// The banners above the addons about the mods in the `current` client,
    /// when there is something to say; `playing` says whether its game runs.
    pub fn banner(&self, current: Option<&Path>, playing: bool) -> Option<impl Widget<Message>> {
        self.mods
            .banner(current?, playing)
            .map(|banner| banner.map(Message::Mods))
    }

    /// Reads what the client folder at `root` holds, unless it was read
    /// already and hasn't changed since.
    pub fn read(&mut self, root: &Path) -> Task<Message> {
        if self.details.contains_key(root) && !self.stale.remove(root) {
            return Task::none();
        }
        let _ = self.details.entry(root.to_path_buf()).or_insert(None);

        let root = root.to_path_buf();
        log::info!("Reading what {} holds", root.display());

        Task::perform(
            blocking({
                let root = root.clone();
                move || Details::read(&root)
            }),
            Message::Read.with(root),
        )
    }

    /// Handles an interaction; how the `current` client starts is edited
    /// right in `config`, its language is switched in `installation` (that
    /// client, once it opened) unless its game runs (`playing`), downloads
    /// use `client`, and failures go to `errors`.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        message: Message,
        config: &mut Config,
        current: Option<&Path>,
        installation: Option<&Installation>,
        playing: bool,
        client: Result<&reqwest::Client, &str>,
        errors: &mut Errors,
    ) -> Action<Instruction, Message> {
        match message {
            Message::Select(path) => {
                self.removing = false;
                self.source = None;
                let read = self.read(&path);
                Action::new(Instruction::Select(path), read)
            }
            Message::Read(path, result) => {
                match &result {
                    Ok(details) => log::info!(
                        "{} holds {} in {}, and connects to {}",
                        path.display(),
                        size(details.size),
                        match details.locales.as_slice() {
                            [] => "no language".to_owned(),
                            locales => Locale::codes(locales),
                        },
                        details
                            .realmlist
                            .as_ref()
                            .and_then(|realmlist| realmlist.server.as_ref())
                            .map_or("no realmlist", |server| server.host.as_str())
                    ),
                    Err(error) => log::warn!("Could not read {}: {error}", path.display()),
                }
                let mods = match &result {
                    Ok(_) => self.mods.shown(&path).map(Message::Mods),
                    Err(_) => Task::none(),
                };
                let restore = match &result {
                    Ok(Details {
                        torrent: Some(torrent),
                        ..
                    }) if !config.torrents.contains_key(&path) => {
                        log::info!(
                            "{} comes from the torrent {}, as its folder notes",
                            path.display(),
                            torrent.name
                        );
                        Some(Instruction::Restore(path.clone(), torrent.clone()))
                    }
                    _ => None,
                };
                let _ = self.details.insert(path, Some(result));

                Action {
                    instruction: restore,
                    task: mods,
                }
            }
            Message::EditRealmlist(path) => {
                log::info!("Opening {} in an editor", path.display());

                // Windows has nothing set to open .wtf files with.
                let program = cfg!(windows).then_some("notepad");
                Action::task(crate::opening(path, program).map(Message::Unopened))
            }
            Message::Unopened(error) => {
                errors.report(error);
                Action::none()
            }
            Message::RereadRealmlist(root) => Action::task(reread(root)),
            Message::Reread(path, Ok(realmlist)) => {
                if let Some(Some(Ok(details))) = self.details.get_mut(&path) {
                    details.realmlist = realmlist;
                }
                Action::none()
            }
            // The last reading stays: it was only a refresh.
            Message::Reread(_, Err(_)) => Action::none(),
            Message::Locale(locale) => {
                let Some(mut installation) = installation
                    .filter(|installation| installation.locale() != Some(locale))
                    .filter(|_| !playing)
                    .cloned()
                else {
                    return Action::none();
                };
                log::info!("Switching {} to {locale}", installation.root().display());

                Action::task(Task::perform(
                    blocking(move || installation.set_locale(locale).map(|()| installation)),
                    Message::Switched.with(locale),
                ))
            }
            Message::Switched(_, Ok(installation)) => {
                let root = installation.root().to_path_buf();
                if let Some(Some(Ok(details))) = self.details.get_mut(&root) {
                    details.locale = installation.locale();
                }

                // The realmlist is the new language's.
                Action::new(Instruction::Switched(installation), reread(root))
            }
            Message::Switched(locale, Err(error)) => {
                errors.report(format!(
                    "Could not switch the language to {locale}: {error}"
                ));
                Action::none()
            }
            Message::LargeAddressAware(on) => {
                let Some(installation) = installation.filter(|_| !playing).cloned() else {
                    return Action::none();
                };
                let root = installation.root().to_path_buf();
                log::info!(
                    "Letting the game in {} use {} of memory",
                    root.display(),
                    if on { "4 GB" } else { "2 GB" }
                );

                Action::task(Task::perform(
                    blocking(move || installation.set_large_address_aware(on)),
                    move |result| Message::Patched(root, on, result),
                ))
            }
            Message::Patched(root, on, Ok(())) => {
                if let Some(Some(Ok(details))) = self.details.get_mut(&root) {
                    details.large_address_aware = Some(on);
                }
                Action::none()
            }
            Message::Patched(_, _, Err(error)) => {
                errors.report(format!("Could not change Wow.exe: {error}"));
                Action::none()
            }
            Message::Env(env) => {
                if let Some(root) = current {
                    let mut launch = config.launch(root);
                    launch.env = env;
                    config.set_launch(root, launch);
                    self.edited = true;
                }
                Action::none()
            }
            Message::Command(command) => {
                if let Some(root) = current {
                    let mut launch = config.launch(root);
                    launch.command = command;
                    config.set_launch(root, launch);
                    self.edited = true;
                }
                Action::none()
            }
            Message::Check => match current {
                Some(root) => Action::instruction(Instruction::Check(root.to_path_buf())),
                None => Action::none(),
            },
            Message::Languages => {
                let Some((root, Some(Some(Ok(details))))) =
                    current.map(|root| (root, self.details.get(root)))
                else {
                    return Action::none();
                };
                Action::instruction(Instruction::Languages {
                    root: root.to_path_buf(),
                    locales: details.locales.clone(),
                    locale: details.locale,
                })
            }
            Message::SetSource => {
                if current.is_none() {
                    return Action::none();
                }
                self.source = Some(String::new());
                Action::task(operation::focus(SOURCE))
            }
            Message::SourceChanged(typed) => {
                if let Some(source) = &mut self.source {
                    *source = typed;
                }
                Action::none()
            }
            Message::ChooseSource => {
                self.choosing = true;
                Action::task(Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Select the client's .torrent file")
                            .add_filter("Torrent", &["torrent"])
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    Message::ChoseSource,
                ))
            }
            Message::ChoseSource(file) => {
                self.choosing = false;
                if let (Some(source), Some(file)) = (&mut self.source, file) {
                    *source = file.display().to_string();
                }
                Action::none()
            }
            Message::SubmitSource => {
                let (Some(root), Some(typed)) = (
                    current,
                    self.source
                        .as_deref()
                        .filter(|typed| !typed.trim().is_empty()),
                ) else {
                    return Action::none();
                };
                let source = Source::parse(typed);
                self.source = None;
                Action::instruction(Instruction::Adopt {
                    root: root.to_path_buf(),
                    source,
                })
            }
            Message::CancelSource => {
                self.source = None;
                Action::none()
            }
            Message::Add => {
                self.removing = false;
                Action::instruction(Instruction::Add)
            }
            Message::Remove => {
                self.removing = current.is_some();
                Action::none()
            }
            Message::ConfirmRemove => {
                self.removing = false;
                Action::instruction(Instruction::Remove)
            }
            Message::CancelRemove => {
                self.removing = false;
                Action::none()
            }
            Message::Close => {
                self.close();
                Action::instruction(Instruction::Close {
                    edited: std::mem::take(&mut self.edited),
                })
            }
            Message::Mods(message) => {
                let action = self.mods.update(
                    message,
                    current,
                    installation,
                    config,
                    playing,
                    client,
                    errors,
                );
                let task = action.task.map(Message::Mods);

                match action.instruction {
                    Some(mods::Instruction::Changed(root)) => {
                        let read = self.changed(&root, current);
                        Action {
                            instruction: Some(Instruction::Changed(root)),
                            task: Task::batch([task, read]),
                        }
                    }
                    None => Action::task(task),
                }
            }
        }
    }

    /// The dialog, showing the `current` client of `config`; `installation`
    /// is that client, once it opened, `playing` whether its game runs,
    /// `downloading` whether a client is being downloaded, checked, or
    /// given languages, and `integrity` what is known of its files.
    #[allow(clippy::too_many_arguments)]
    pub fn view<'a>(
        &'a self,
        config: &'a Config,
        current: Option<&'a Path>,
        installation: Option<&'a Installation>,
        playing: bool,
        downloading: bool,
        integrity: Option<String>,
    ) -> impl Widget<Message> + 'a {
        let title = row![
            style::heading("Clients", 20.0),
            space::horizontal(),
            button("Close").style(style::quiet).on_press(Message::Close),
        ]
        .align_y(Center);

        // The built-in arrow is drawn over the end of an ellipsized label,
        // so the label keeps clear of the right edge and the same arrow
        // glyph is layered on top instead.
        let picker = row![
            stack![
                pick_list(
                    current.map(Path::to_path_buf),
                    config.installations.as_slice(),
                    |path: &PathBuf| path.display().to_string(),
                )
                .style(style::picker)
                .menu_style(style::choices)
                .placeholder("Select a client")
                .on_select(Message::Select)
                .ellipsis(text::Ellipsis::Middle)
                .handle(pick_list::Handle::None)
                .padding(padding::all(8).left(10).right(32))
                .width(Fill),
                container(text('\u{e800}').font(Font::new("Iced-Icons")))
                    .align_right(Fill)
                    .center_y(Fill)
                    .padding(padding::right(10)),
            ]
            .width(Fill),
            style::icon_button(
                ADD,
                &style::PLUS,
                PICKER_BUTTON,
                style::secondary,
                "Add a client",
                Some(Message::Add),
            ),
            style::icon_button(
                REMOVE,
                &style::TRASH,
                PICKER_BUTTON,
                style::danger,
                "Remove this client from the launcher. Its folder is left as it is.",
                current.filter(|_| !self.removing).map(|_| Message::Remove),
            ),
        ]
        .spacing(8)
        .align_y(Center);

        let removal = current.filter(|_| self.removing).map(|root| {
            container(
                row![
                    text(format!(
                        "Remove {} from the launcher? Its folder is left as it is.",
                        root.display()
                    ))
                    .width(Fill),
                    button("Cancel")
                        .style(style::quiet)
                        .on_press(Message::CancelRemove),
                    button("Remove")
                        .style(style::danger)
                        .on_press(Message::ConfirmRemove),
                ]
                .spacing(8)
                .align_y(Center),
            )
            .padding([6, 12])
            .style(style::banner(style::Tone::Danger))
        });

        let about = current.map(|root| {
            let torrent = config.torrents.get(root);
            // What the folder holds, or why it isn't known (yet).
            let details: Element<'a, Message> = match self.details.get(root) {
                Some(Some(Ok(details))) => column![
                    languages(details, playing, torrent.is_some(), downloading),
                    match &details.realmlist {
                        Some(realmlist) => fact(
                            "Realmlist",
                            match &realmlist.server {
                                Some(Server {
                                    host,
                                    from: Named::Realmlist,
                                }) => host.clone(),
                                Some(Server {
                                    host,
                                    from: Named::Settings,
                                }) => format!("{host} (from the game's settings)"),
                                None => "Not set".to_owned(),
                            },
                        )
                        .extend(realmlist.exists.then(|| {
                            style::icon_button(
                                EDIT_REALMLIST,
                                &style::PEN,
                                26.0,
                                style::quiet,
                                "Edit realmlist.wtf",
                                Some(Message::EditRealmlist(realmlist.path.clone())),
                            )
                            .boxed()
                        }))
                        .push(
                            style::icon_button(
                                REFRESH_REALMLIST,
                                &style::REFRESH,
                                26.0,
                                style::quiet,
                                "Read realmlist.wtf again",
                                Some(Message::RereadRealmlist(root.to_path_buf())),
                            )
                            .boxed(),
                        )
                        .align_y(Center),
                        None => fact("Realmlist", "No realmlist.wtf".to_owned()),
                    },
                    // Without a server to connect to, the game goes nowhere.
                    Some(warnings(details.realmlist.as_ref()))
                        .filter(|warnings| !warnings.is_empty())
                        .map(|warnings| {
                            column(warnings.into_iter().map(|warning| {
                                text(warning)
                                    .size(13)
                                    .style(text::warning)
                                    .wrapping(text::Wrapping::WordOrGlyph)
                            }))
                            .spacing(6)
                        }),
                    fact("Size", size(details.size)),
                    details
                        .large_address_aware
                        .map(|on| memory(on, playing).align_y(Center)),
                ]
                .spacing(6)
                .boxed(),
                Some(Some(Err(error))) => text(format!("Could not read the folder: {error}"))
                    .size(14)
                    .style(text::danger)
                    .boxed(),
                Some(None) | None => text("Reading the folder…")
                    .size(14)
                    .style(text::secondary)
                    .boxed(),
            };

            container(
                column![
                    labelled(
                        "Folder",
                        text(root.display().to_string())
                            .size(14)
                            .wrapping(text::Wrapping::WordOrGlyph)
                            .selectable(true),
                    ),
                    installation
                        .map(|installation| fact("Version", installation.version().to_string())),
                    self.torrent(torrent, downloading),
                    integrity_of(
                        downloading,
                        self.mods.is_waiting_for_patches(root),
                        integrity,
                    ),
                    details,
                ]
                .spacing(6),
            )
            .padding(12)
            .width(Fill)
            .style(style::inset)
        });

        // Windows starts Wow.exe itself.
        let launch = current.filter(|_| !cfg!(windows)).map(|root| {
            let launch = config.launch.get(root);
            let env = launch.map_or("", |launch| launch.env.as_str());
            let command = launch.map_or("", |launch| launch.command.as_str());
            // Left empty, the fields hold what the launcher sets instead.
            let defaults = launch::Defaults {
                natives: self.mods.natives(root),
                ..launch::Defaults::of(root)
            };
            let set = defaults.env().unwrap_or_default();

            // What the launcher sets, written out whole: a long path doesn't
            // fit in the field.
            let line = |line: String| {
                text(line)
                    .size(13)
                    .style(text::secondary)
                    .wrapping(text::Wrapping::WordOrGlyph)
                    .boxed()
            };
            let added = column(
                std::iter::once(line(
                    "Automatically added environment variables (define again to override):"
                        .to_owned(),
                ))
                .chain(set.iter().map(|variable| line(format!("- {variable}")))),
            )
            .spacing(2);

            column![
                style::heading("Starting the game", 16.0),
                column![
                    field(
                        "Environment variables",
                        text_input(set.join(" "), env)
                            .id(ENV)
                            .style(style::field)
                            .on_input(Message::Env),
                        launch.and_then(|launch| launch.env_problem()),
                    ),
                    added,
                ]
                .spacing(6),
                field(
                    "Command",
                    text_input(defaults.command, command)
                        .id(COMMAND)
                        .style(style::field)
                        .on_input(Message::Command),
                    launch.and_then(|launch| launch.command_problem()),
                ),
            ]
            .spacing(10)
        });

        // Nothing to say of a folder that can't be read.
        let mods = current
            .filter(|root| !matches!(self.details.get(*root), Some(Some(Err(_)))))
            .map(|root| self.mods.view(root, playing).boxed().map(Message::Mods));

        // A short window scrolls what is under the title, so Close stays in
        // sight.
        let body = scrollable(column![picker, removal, about, launch, mods].spacing(14))
            .spacing(10)
            .style(style::scroll);

        container(column![title, body].spacing(14))
            .padding(24)
            .width(Fill.max(760))
            .style(style::card)
    }

    /// The torrent the client shown comes from, if it has one, which its
    /// files are checked against; and the field giving it one, while it
    /// shows. Not while a client is being downloaded, checked, or given
    /// languages (`downloading`).
    fn torrent(
        &self,
        torrent: Option<&ClientTorrent>,
        downloading: bool,
    ) -> Column<Element<'_, Message>> {
        let set = (self.source.is_none() && !downloading).then_some(Message::SetSource);

        let fact = match torrent {
            Some(torrent) => {
                fact("Torrent", torrent.name.clone()).push(small("Change…", style::quiet, set))
            }
            None => {
                fact("Torrent", "Not set".to_owned()).push(small("Set…", style::secondary, set))
            }
        };

        let field = self.source.as_ref().map(|source| {
            let submit =
                (!source.trim().is_empty() && !downloading).then_some(Message::SubmitSource);

            column![
                row![
                    text_input("magnet:?xt=… or https://…/client.torrent", source)
                        .id(SOURCE)
                        .style(style::field)
                        .on_input(Message::SourceChanged)
                        .on_submit_maybe(submit.clone())
                        .size(14)
                        .padding(8),
                    button(text("Choose file…").size(14))
                        .style(style::secondary)
                        .padding(8)
                        .on_press_maybe((!self.choosing).then_some(Message::ChooseSource)),
                ]
                .spacing(8),
                text(
                    "A magnet link, the address of a .torrent file, or the file: the torrent \
                     the client was downloaded with. The launcher checks the client's files \
                     against it, repairs those missing or changed, and downloads its other \
                     languages."
                )
                .size(13)
                .style(text::secondary),
                row![
                    space::horizontal(),
                    button("Cancel")
                        .style(style::quiet)
                        .on_press(Message::CancelSource),
                    button("Set").style(style::primary).on_press_maybe(submit),
                ]
                .spacing(8),
            ]
            .spacing(8)
            .padding(padding::left(112))
        });

        column![fact.align_y(Center), field].spacing(8)
    }
}

/// A small button after a fact, pressed for `message`.
fn small(
    label: &str,
    style: fn(&Theme, button::Status) -> button::Style,
    message: Option<Message>,
) -> Element<'_, Message> {
    button(text(label).size(13))
        .style(style)
        .padding([2, 10])
        .on_press_maybe(message)
        .boxed()
}

/// What is wrong with the realmlist of a client, if anything: no file, a
/// file that names no server, or a line the game skips. Nothing rewrites the
/// file.
fn warnings(realmlist: Option<&Realmlist>) -> Vec<String> {
    let Some(realmlist) = realmlist else {
        return vec![NO_REALMLIST.to_owned()];
    };
    let mut warnings = Vec::new();

    if !realmlist.exists && realmlist.server.is_none() {
        warnings.push(NO_REALMLIST.to_owned());
    }
    if realmlist.exists && realmlist.server.is_none() && realmlist.bare.is_none() {
        warnings.push(NO_SERVER.to_owned());
    }
    if let Some(host) = &realmlist.bare {
        warnings.push(BARE_HOST.replace("{host}", host));
    }

    warnings
}

/// Reads the realmlist of the client at `root` again.
fn reread(root: PathBuf) -> Task<Message> {
    log::debug!("Reading the realmlist of {} again", root.display());

    Task::perform(
        blocking({
            let root = root.clone();
            move || Ok(Realmlist::of(&root))
        }),
        Message::Reread.with(root),
    )
}

/// The languages the client has data for. With two or more, the one it
/// plays in is picked among them, but not while its game runs (`playing`).
fn languages(
    details: &Details,
    playing: bool,
    torrent: bool,
    downloading: bool,
) -> Row<Element<'_, Message>> {
    let row = match details.locales.as_slice() {
        [] => fact("Languages", "None found".to_owned()),
        [locale] => fact("Languages", locale.to_string()),
        locales => {
            let picker = pick_list(details.locale, locales, Locale::to_string)
                .style(style::picker)
                .menu_style(style::choices)
                .text_size(14)
                .padding([2, 10]);

            if playing {
                labelled("Language", style::tip(picker, game::KEEPS_CONFIG))
            } else {
                labelled("Language", picker.on_select(Message::Locale))
            }
        }
    };

    // Others to download from its torrent, or its own to delete.
    let manageable = torrent || details.locales.len() > 1;
    if manageable {
        row.push(small(
            "Manage…",
            style::secondary,
            (!downloading).then_some(Message::Languages),
        ))
    } else {
        row
    }
    .align_y(Center)
}

/// What is known of the files of the client shown, `integrity`, beside the
/// button checking them: every client's patches, and its files against its
/// torrent when it has one. While a client is being downloaded, checked, or
/// given languages (`downloading`), or the client's patches are being looked
/// at (`looking`), it waits.
fn integrity_of<'a>(
    downloading: bool,
    looking: bool,
    integrity: Option<String>,
) -> Row<Element<'a, Message>> {
    let check = button(text("Check").size(13))
        .style(style::secondary)
        .padding([2, 10]);
    let check: Element<'a, Message> = if looking {
        style::tip(check, crate::patches::LOOKING).boxed()
    } else {
        check
            .on_press_maybe((!downloading).then_some(Message::Check))
            .boxed()
    };

    row![
        text("File integrity")
            .size(14)
            .width(100)
            .style(text::secondary),
        check,
        integrity.map(|said| text(said).size(14)),
    ]
    .spacing(12)
    .align_y(Center)
}

/// Whether the game may use 4 GB of memory (`on`), as `Wow.exe` says, which
/// is changed there, but not while the game runs (`playing`).
fn memory<'a>(on: bool, playing: bool) -> Row<Element<'a, Message>> {
    let check = checkbox(on)
        .style(style::check)
        .label("Up to 4 GB (Large Address Aware)")
        .text_size(14);

    labelled(
        "Memory",
        if playing {
            style::tip(check, game::KEEPS_EXE)
        } else {
            style::tip(
                check.on_toggle(Message::LargeAddressAware),
                "The game is a 32-bit program, given 2 GB of memory unless Wow.exe asks for 4: HD \
                 textures and long view distances need them. Ticking it sets that flag in \
                 Wow.exe, and unticking it clears it.",
            )
        },
    )
}

/// A labelled fact about the client, to which more can be pushed after the
/// value.
fn fact(label: &str, value: String) -> Row<Element<'_, Message>> {
    labelled(
        label,
        text(value).size(14).wrapping(text::Wrapping::WordOrGlyph),
    )
}

/// `value`, labelled as a fact about the client.
fn labelled<'a>(label: &'a str, value: impl Widget<Message> + 'a) -> Row<Element<'a, Message>> {
    row![
        text(label).size(14).width(100).style(text::secondary),
        value
    ]
    .spacing(12)
}

/// A labelled text field, with what is wrong with its contents under it.
fn field<'a>(
    label: &'a str,
    input: TextInput<'a, Message>,
    problem: Option<String>,
) -> impl Widget<Message> + 'a {
    column![
        text(label).size(14),
        input.size(14).padding(8),
        problem.map(|problem| text(problem).size(13).style(text::danger)),
    ]
    .spacing(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_test::{selector, simulator};

    const WOTLK: &str = "/games/wotlk";

    fn config() -> Config {
        Config {
            installations: vec![WOTLK.into(), "/games/warmane".into()],
            selected: Some(WOTLK.into()),
            ..Config::default()
        }
    }

    fn details(realmlist: Option<Realmlist>) -> Details {
        Details {
            size: 18_576_000_000,
            locales: vec![Locale::EnUs],
            locale: Some(Locale::EnUs),
            realmlist,
            large_address_aware: Some(false),
            torrent: None,
        }
    }

    fn realmlist(server: Option<&str>) -> Realmlist {
        Realmlist {
            path: PathBuf::from(WOTLK).join("Data/enUS/realmlist.wtf"),
            exists: true,
            server: server.map(|host| Server {
                host: host.to_owned(),
                from: Named::Realmlist,
            }),
            bare: None,
        }
    }

    fn update(
        state: &mut State,
        config: &mut Config,
        message: Message,
    ) -> Action<Instruction, Message> {
        state.update(
            message,
            config,
            Some(Path::new(WOTLK)),
            None,
            false,
            Err("offline"),
            &mut Errors::default(),
        )
    }

    /// An open dialog on the wotlk client, whose folder was read.
    fn opened(read: Option<Details>) -> (State, Config) {
        let mut state = State::default();
        let mut config = config();
        let _ = state.open(Some(Path::new(WOTLK)));

        if let Some(details) = read {
            let _ = update(
                &mut state,
                &mut config,
                Message::Read(WOTLK.into(), Ok(details)),
            );
        }

        (state, config)
    }

    #[test]
    fn the_dialog_says_what_the_client_holds() {
        let (mut state, mut config) = opened(None);
        let root = Path::new(WOTLK);

        let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));
        assert!(ui.find("Reading the folder…").is_ok());
        drop(ui);

        let _ = update(
            &mut state,
            &mut config,
            Message::Read(
                root.into(),
                Ok(details(Some(realmlist(Some("logon.example.com"))))),
            ),
        );

        let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));
        for line in [WOTLK, "Languages", "enUS", "logon.example.com", "17.3 GB"] {
            assert!(ui.find(line).is_ok(), "{line}");
        }
    }

    #[test]
    fn the_game_is_given_4_gb_of_memory_in_wow_exe_on_request_but_not_while_it_runs() {
        let (mut state, mut config) = opened(Some(details(None)));
        let root = Path::new(WOTLK);
        let label = "Up to 4 GB (Large Address Aware)";

        let messages: Vec<Message> = {
            let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));
            assert!(ui.find("Memory").is_ok());
            let _ = ui.click(label).unwrap();
            ui.into_messages().collect()
        };
        assert!(matches!(messages[..], [Message::LargeAddressAware(true)]));

        let messages: Vec<Message> = {
            let mut ui = simulator(state.view(&config, Some(root), None, true, false, None));
            let _ = ui.click(label).unwrap();
            ui.into_messages().collect()
        };
        assert!(messages.is_empty(), "not while the game runs");

        let _ = update(
            &mut state,
            &mut config,
            Message::Patched(root.into(), true, Ok(())),
        );
        assert!(matches!(
            state.details.get(root),
            Some(Some(Ok(Details {
                large_address_aware: Some(true),
                ..
            })))
        ));

        let mut errors = Errors::default();
        let _ = state.update(
            Message::Patched(root.into(), false, Err("denied".to_owned())),
            &mut config,
            Some(root),
            None,
            false,
            Err("offline"),
            &mut errors,
        );
        assert_eq!(errors.messages(), ["Could not change Wow.exe: denied"]);
    }

    #[test]
    fn a_window_too_short_for_the_dialog_scrolls_it_below_the_title() {
        let (state, config) = opened(Some(details(Some(realmlist(None)))));
        let root = Path::new(WOTLK);

        let mut ui = iced_test::simulator::Simulator::with_size(
            iced::Settings::default(),
            crate::App::MIN_SIZE,
            iced::widget::center(state.view(&config, Some(root), None, false, false, None)),
        );
        let title = ui.find("Clients").unwrap().bounds();
        assert!(title.y >= 0.0, "the title is in the window: {title:?}");
        let last = ui
            .find("Servers may forbid modified clients, and their anti-cheat may detect it.")
            .unwrap()
            .bounds();
        assert!(last.height > 0.0, "the last line isn't squeezed: {last:?}");
        assert_eq!(crate::testing::offsets(&mut ui), [0.0]);
    }

    /// A client at a temporary folder with data for enUS and esES, each with
    /// its realmlist, open and shown in the dialog.
    fn bilingual() -> (tempfile::TempDir, Installation, State, Config) {
        let (folder, installation) = crate::testing::client_with(&[
            ("Data/enUS/realmlist.wtf", "set realmlist us.example.com"),
            ("Data/esES/realmlist.wtf", "set realmlist es.example.com"),
        ]);
        let root = installation.root();
        let mut config = Config {
            installations: vec![root.to_path_buf()],
            selected: Some(root.to_path_buf()),
            ..Config::default()
        };
        let mut state = State::default();
        let _ = state.open(Some(root));
        let _ = state.update(
            Message::Read(root.to_path_buf(), Ok(Details::read(root).unwrap())),
            &mut config,
            Some(root),
            Some(&installation),
            false,
            Err("offline"),
            &mut Errors::default(),
        );

        (folder, installation, state, config)
    }

    #[test]
    fn a_language_picked_is_written_and_the_realmlist_follows() {
        let (_folder, installation, mut state, mut config) = bilingual();
        let root = installation.root();
        let mut errors = Errors::default();

        let mut ui =
            simulator(state.view(&config, Some(root), Some(&installation), false, false, None));
        assert!(ui.find("Language").is_ok());
        assert!(ui.find("us.example.com").is_ok());
        let _ = ui.click("enUS").unwrap();
        let _ = ui.click("esES").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            matches!(messages.as_slice(), [Message::Locale(Locale::EsEs)]),
            "{messages:?}"
        );

        let switch = state.update(
            Message::Locale(Locale::EsEs),
            &mut config,
            Some(root),
            Some(&installation),
            false,
            Err("offline"),
            &mut errors,
        );
        assert!(switch.instruction.is_none());
        assert!(switch.task.units() > 0, "written in the background");

        // What the task does.
        let mut switched = installation.clone();
        switched.set_locale(Locale::EsEs).unwrap();
        let done = state.update(
            Message::Switched(Locale::EsEs, Ok(switched)),
            &mut config,
            Some(root),
            Some(&installation),
            false,
            Err("offline"),
            &mut errors,
        );
        assert!(matches!(
            done.instruction,
            Some(Instruction::Switched(switched)) if switched.locale() == Some(Locale::EsEs)
        ));
        assert!(done.task.units() > 0, "the realmlist is read again");
        let _ = state.update(
            Message::Reread(root.to_path_buf(), Ok(Realmlist::of(root))),
            &mut config,
            Some(root),
            Some(&installation),
            false,
            Err("offline"),
            &mut errors,
        );

        let mut ui =
            simulator(state.view(&config, Some(root), Some(&installation), false, false, None));
        assert!(ui.find("esES").is_ok());
        assert!(ui.find("es.example.com").is_ok());
        assert!(errors.messages().is_empty(), "{:?}", errors.messages());
    }

    #[test]
    fn picking_the_language_played_in_writes_nothing() {
        let (_folder, installation, mut state, mut config) = bilingual();

        let action = state.update(
            Message::Locale(Locale::EnUs),
            &mut config,
            Some(installation.root()),
            Some(&installation),
            false,
            Err("offline"),
            &mut Errors::default(),
        );

        assert_eq!(action.task.units(), 0);
        assert!(!installation.root().join("WTF").exists());
    }

    #[test]
    fn a_language_is_not_picked_while_the_game_runs() {
        let (_folder, installation, state, config) = bilingual();
        let root = installation.root();

        let mut ui =
            simulator(state.view(&config, Some(root), Some(&installation), true, false, None));
        let picker = ui.find("enUS").unwrap().bounds().center();
        ui.point_at(picker);
        let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: picker,
        })]);
        assert!(ui.find(game::KEEPS_CONFIG).is_ok());
        let _ = ui.click("enUS").unwrap();
        assert!(ui.find("esES").is_err(), "the list doesn't open");
        assert_eq!(ui.into_messages().count(), 0);
    }

    #[test]
    fn a_failed_switch_goes_to_the_banner() {
        let (_folder, installation, mut state, mut config) = bilingual();
        let mut errors = Errors::default();

        let action = state.update(
            Message::Switched(
                Locale::EsEs,
                Err("/games/wotlk/WTF/Config.wtf: Permission denied".into()),
            ),
            &mut config,
            Some(installation.root()),
            Some(&installation),
            false,
            Err("offline"),
            &mut errors,
        );

        assert!(action.instruction.is_none());
        assert_eq!(
            errors.messages(),
            [
                "Could not switch the language to esES: /games/wotlk/WTF/Config.wtf: Permission denied"
            ]
        );
    }

    #[test]
    fn what_a_folder_holds_is_read_once_until_it_changes() {
        let (mut state, mut config) = opened(Some(details(None)));
        let root = Path::new(WOTLK);
        // The look at its mods under way is done.
        let looked_at = |state: &mut State, config: &mut Config| {
            let _ = update(
                state,
                config,
                Message::Mods(mods::Message::Found(root.into(), mods::Seen::default())),
            );
        };

        state.close();
        assert_eq!(state.open(Some(root)).units(), 0, "remembered");
        looked_at(&mut state, &mut config);

        // Addons came or went while the dialog was closed: only its mods are
        // looked for at once, for the banners above the addons.
        state.close();
        assert_eq!(state.changed(root, None).units(), 1, "read when next shown");
        assert!(state.open(Some(root)).units() > 0);
        assert!(
            matches!(state.details.get(root), Some(Some(Ok(_)))),
            "the old reading shows meanwhile"
        );
        looked_at(&mut state, &mut config);

        // And while it is open and showing that client.
        assert_eq!(state.changed(root, Some(root)).units(), 2);
        assert_eq!(
            state
                .changed(Path::new("/games/warmane"), Some(root))
                .units(),
            1,
            "another client: when next shown"
        );
    }

    #[test]
    fn mods_found_in_a_client_read_say_above_the_addons_what_they_need() {
        let (mut state, mut config) = opened(None);
        let root = Path::new(WOTLK);
        let json = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/index/mods.json")).unwrap();
        let looking = state.list_mods(Mods::parse(&json).unwrap(), Some(root));
        assert!(looking.units() > 0, "the client shown is looked at again");
        assert!(
            state.banner(Some(root), false).is_none(),
            "not looked at yet"
        );

        let seen = mods::Seen {
            held: BTreeMap::from([(
                "lumenveil".to_owned(),
                crate::core::mods::Status {
                    release: Some("0.1.1".to_owned()),
                    problems: vec![crate::core::mods::Problem::Missing(vec![
                        "CoAVolFog.dll".to_owned(),
                    ])],
                },
            )]),
            foreign: BTreeMap::new(),
            unfit: Default::default(),
            excluded: BTreeMap::new(),
            lacking: BTreeMap::new(),
        };
        let _ = update(
            &mut state,
            &mut config,
            Message::Mods(mods::Message::Found(root.into(), seen)),
        );
        state.close();

        let mut ui = simulator(state.banner(Some(root), false).expect("a banner"));
        assert!(
            ui.find(
                "The launcher needs to repair “Lumenveil” before starting the game. Missing: \
                 CoAVolFog.dll."
            )
            .is_ok()
        );
        assert!(
            state
                .banner(Some(Path::new("/games/warmane")), false)
                .is_none(),
            "about the client shown only"
        );
    }

    #[test]
    fn the_realmlist_row_says_where_the_server_comes_from() {
        let view = |realmlist| {
            let (state, config) = opened(Some(details(Some(realmlist))));
            let root = Path::new(WOTLK);
            let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));

            (
                ui.find("logon.example.com").is_ok(),
                ui.find("logon.example.com (from the game's settings)")
                    .is_ok(),
                ui.find(BARE_HOST.replace("{host}", "logon.example.com"))
                    .is_ok(),
                ui.find(NO_SERVER).is_ok(),
            )
        };
        let settings = Realmlist {
            server: Some(Server {
                host: "logon.example.com".to_owned(),
                from: Named::Settings,
            }),
            ..realmlist(None)
        };
        let bare = Realmlist {
            bare: Some("logon.example.com".to_owned()),
            ..realmlist(None)
        };

        assert_eq!(
            view(realmlist(Some("logon.example.com"))),
            (true, false, false, false)
        );
        assert_eq!(view(settings), (false, true, false, false));
        assert_eq!(
            view(bare),
            (false, false, true, false),
            "told apart from a file with nothing in it"
        );
        assert_eq!(view(realmlist(None)), (false, false, false, true));
    }

    #[test]
    fn a_realmlist_is_read_again_when_asked() {
        let (mut state, mut config) = opened(Some(details(Some(realmlist(None)))));
        let root = Path::new(WOTLK);

        let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));
        assert!(ui.find("Not set").is_ok());
        assert!(ui.find(NO_SERVER).is_ok(), "warned");
        let _ = ui.click(selector::id(EDIT_REALMLIST)).unwrap();
        // Not handled here: that would open an editor.
        assert!(matches!(
            ui.into_messages().next(),
            Some(Message::EditRealmlist(path)) if path == realmlist(None).path
        ));

        let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));
        let _ = ui.click(selector::id(REFRESH_REALMLIST)).unwrap();
        let message = ui.into_messages().next().unwrap();
        assert!(matches!(&message, Message::RereadRealmlist(path) if path == root));
        let refresh = update(&mut state, &mut config, message);
        assert!(refresh.task.units() > 0, "read again");
        let _ = update(
            &mut state,
            &mut config,
            Message::Reread(root.into(), Ok(Some(realmlist(Some("logon.example.com"))))),
        );

        let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));
        assert!(ui.find("logon.example.com").is_ok());
        assert!(ui.find(NO_SERVER).is_err(), "nothing to warn about");
    }

    #[test]
    fn a_client_without_a_realmlist_offers_nothing_to_edit_and_is_warned() {
        let (state, config) = opened(Some(details(None)));

        let mut ui =
            simulator(state.view(&config, Some(Path::new(WOTLK)), None, false, false, None));

        assert!(ui.find("No realmlist.wtf").is_ok());
        assert!(ui.find(NO_REALMLIST).is_ok());
        assert!(ui.find(selector::id(EDIT_REALMLIST)).is_err());
    }

    #[test]
    fn picking_a_client_shows_it_and_reads_its_folder() {
        let (mut state, mut config) = opened(None);
        let root = Path::new(WOTLK);

        let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));
        let _ = ui.click(WOTLK).unwrap();
        let _ = ui.click("/games/warmane").unwrap();
        let message = ui.into_messages().next().unwrap();

        let action = update(&mut state, &mut config, message);

        assert!(matches!(
            action.instruction,
            Some(Instruction::Select(path)) if path == Path::new("/games/warmane")
        ));
        assert!(state.details.contains_key(Path::new("/games/warmane")));
    }

    #[test]
    fn removing_a_client_is_asked_about_first() {
        let (mut state, mut config) = opened(None);
        let root = Path::new(WOTLK);

        let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));
        let _ = ui.click(selector::id(REMOVE)).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            let action = update(&mut state, &mut config, message);
            assert!(action.instruction.is_none(), "not yet");
        }

        let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));
        assert!(
            ui.find("Remove /games/wotlk from the launcher? Its folder is left as it is.")
                .is_ok()
        );
        let _ = ui.click("Cancel").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            let _ = update(&mut state, &mut config, message);
        }
        assert!(!state.removing);

        let _ = update(&mut state, &mut config, Message::Remove);
        let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));
        let _ = ui.click("Remove").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        let instructions: Vec<Instruction> = messages
            .into_iter()
            .filter_map(|message| update(&mut state, &mut config, message).instruction)
            .collect();

        assert!(matches!(instructions.as_slice(), [Instruction::Remove]));
    }

    #[test]
    fn adding_and_closing_are_left_to_the_launcher() {
        let (mut state, mut config) = opened(None);

        let mut ui =
            simulator(state.view(&config, Some(Path::new(WOTLK)), None, false, false, None));
        let _ = ui.click(selector::id(ADD)).unwrap();
        let _ = ui.click("Close").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();

        let instructions: Vec<Instruction> = messages
            .into_iter()
            .filter_map(|message| update(&mut state, &mut config, message).instruction)
            .collect();

        assert!(matches!(
            instructions.as_slice(),
            [Instruction::Add, Instruction::Close { edited: false }]
        ));
        assert!(!state.is_open());
    }

    /// What clicking `label` in the dialog sends, the dialog showing the
    /// wotlk client, maybe while a client is `downloading`.
    fn clicked(state: &State, config: &Config, label: &str, downloading: bool) -> Vec<Message> {
        let mut ui = simulator(state.view(
            config,
            Some(Path::new(WOTLK)),
            None,
            false,
            downloading,
            None,
        ));
        let _ = ui.click(label).unwrap();
        ui.into_messages().collect()
    }

    fn torrent() -> ClientTorrent {
        ClientTorrent {
            name: "WotLK".into(),
            info_hash: "0123456789abcdef0123456789abcdef01234567".into(),
            source: "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567".into(),
            folder: WOTLK.into(),
            languages: Vec::new(),
            made: false,
        }
    }

    #[test]
    fn a_client_is_given_its_torrent() {
        let (mut state, mut config) = opened(Some(details(None)));
        let mut ui =
            simulator(state.view(&config, Some(Path::new(WOTLK)), None, false, false, None));
        assert!(ui.find("Not set").is_ok());
        assert!(ui.find("File integrity").is_ok());
        assert!(
            ui.find("Manage…").is_err(),
            "one language, nowhere to get others"
        );
        drop(ui);
        assert!(
            clicked(&state, &config, "Set…", true).is_empty(),
            "not while a client downloads"
        );

        for message in clicked(&state, &config, "Set…", false) {
            let action = update(&mut state, &mut config, message);
            assert!(action.task.units() > 0, "the field takes the keys");
        }
        assert!(clicked(&state, &config, "Set", false).is_empty(), "empty");
        let messages: Vec<Message> = {
            let mut ui =
                simulator(state.view(&config, Some(Path::new(WOTLK)), None, false, false, None));
            let _ = ui.click(selector::id(SOURCE)).unwrap();
            let _ = ui.typewrite(" magnet:?xt=urn:btih:abc ");
            ui.into_messages().collect()
        };
        for message in messages {
            let _ = update(&mut state, &mut config, message);
        }
        let instructions: Vec<Instruction> = clicked(&state, &config, "Set", false)
            .into_iter()
            .filter_map(|message| update(&mut state, &mut config, message).instruction)
            .collect();
        assert!(
            matches!(
                instructions.as_slice(),
                [Instruction::Adopt { root, source: Source::Magnet(link) }]
                    if root == Path::new(WOTLK) && link == "magnet:?xt=urn:btih:abc"
            ),
            "set"
        );
        assert!(state.source.is_none(), "the field goes");

        // A file chosen goes in the field, which can be left too.
        let _ = update(&mut state, &mut config, Message::SetSource);
        let choosing = update(&mut state, &mut config, Message::ChooseSource);
        assert!(choosing.task.units() > 0);
        let _ = update(
            &mut state,
            &mut config,
            Message::ChoseSource(Some("/downloads/client.torrent".into())),
        );
        assert_eq!(state.source.as_deref(), Some("/downloads/client.torrent"));
        for message in clicked(&state, &config, "Cancel", false) {
            let _ = update(&mut state, &mut config, message);
        }
        assert!(state.source.is_none());
    }

    #[test]
    fn the_check_is_offered_to_a_client_without_a_torrent() {
        let (mut state, mut config) = opened(Some(details(None)));
        assert!(!config.torrents.contains_key(Path::new(WOTLK)));

        let instructions: Vec<Instruction> = clicked(&state, &config, "Check", false)
            .into_iter()
            .filter_map(|message| update(&mut state, &mut config, message).instruction)
            .collect();
        assert!(matches!(
            instructions.as_slice(),
            [Instruction::Check(root)] if root == Path::new(WOTLK)
        ));

        // Not while a client downloads, is checked or given languages.
        assert!(clicked(&state, &config, "Check", true).is_empty());

        // What a check says shows beside it.
        let said = "Nothing is wrong with its patches.";
        let mut ui = simulator(state.view(
            &config,
            Some(Path::new(WOTLK)),
            None,
            false,
            false,
            Some(said.to_owned()),
        ));
        assert!(ui.find(said).is_ok());
    }

    #[test]
    fn the_check_waits_for_the_patches_to_be_looked_at() {
        let (mut state, mut config) = opened(Some(details(None)));

        state
            .mods
            .waiting_for_patches(BTreeSet::from([PathBuf::from(WOTLK)]));
        assert!(
            clicked(&state, &config, "Check", false).is_empty(),
            "looking at the patches"
        );

        state.mods.waiting_for_patches(BTreeSet::new());
        let instructions: Vec<Instruction> = clicked(&state, &config, "Check", false)
            .into_iter()
            .filter_map(|message| update(&mut state, &mut config, message).instruction)
            .collect();
        assert!(matches!(instructions.as_slice(), [Instruction::Check(_)]));
    }

    #[test]
    fn a_client_with_a_torrent_has_its_files_checked_or_is_given_another() {
        let (mut state, mut config) = opened(Some(details(None)));
        let _ = config.torrents.insert(WOTLK.into(), torrent());

        let mut ui = simulator(state.view(
            &config,
            Some(Path::new(WOTLK)),
            None,
            false,
            false,
            Some("Every file is intact.".to_owned()),
        ));
        for wanted in [
            "Torrent",
            "WotLK",
            "File integrity",
            "Every file is intact.",
        ] {
            assert!(ui.find(wanted).is_ok(), "{wanted}");
        }
        drop(ui);
        let instructions: Vec<Instruction> = clicked(&state, &config, "Check", false)
            .into_iter()
            .filter_map(|message| update(&mut state, &mut config, message).instruction)
            .collect();
        assert!(matches!(
            instructions.as_slice(),
            [Instruction::Check(root)] if root == Path::new(WOTLK)
        ));
        assert!(clicked(&state, &config, "Check", true).is_empty());

        // Its languages: others from its torrent, or its own to delete.
        let instructions: Vec<Instruction> = clicked(&state, &config, "Manage…", false)
            .into_iter()
            .filter_map(|message| update(&mut state, &mut config, message).instruction)
            .collect();
        assert!(matches!(
            instructions.as_slice(),
            [Instruction::Languages { root, locales, locale }]
                if root == Path::new(WOTLK) && locales == &[Locale::EnUs] && *locale == Some(Locale::EnUs)
        ));
        assert!(clicked(&state, &config, "Manage…", true).is_empty());

        for message in clicked(&state, &config, "Change…", false) {
            let _ = update(&mut state, &mut config, message);
        }
        assert_eq!(state.source.as_deref(), Some(""));
    }

    #[test]
    fn a_client_added_back_has_its_torrent_again_as_its_folder_notes() {
        let (mut state, mut config) = opened(None);
        let read = Details {
            torrent: Some(torrent()),
            ..details(None)
        };

        let action = update(
            &mut state,
            &mut config,
            Message::Read(WOTLK.into(), Ok(read.clone())),
        );
        assert!(matches!(
            action.instruction,
            Some(Instruction::Restore(root, restored))
                if root == Path::new(WOTLK) && restored == torrent()
        ));

        let _ = config.torrents.insert(WOTLK.into(), torrent());
        let action = update(
            &mut state,
            &mut config,
            Message::Read(WOTLK.into(), Ok(read)),
        );
        assert!(action.instruction.is_none(), "known already");
    }

    #[cfg(not(windows))]
    #[test]
    fn the_variables_the_launcher_adds_are_written_out_under_the_field() {
        let (mut state, mut config) = opened(Some(details(None)));
        let root = Path::new(WOTLK);
        let heading = "Automatically added environment variables (define again to override):";
        let prefix = format!(
            "- {}",
            launch::Defaults::of(Path::new(WOTLK)).env().unwrap()[0]
        );
        let prefix = prefix.as_str();

        let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));
        for line in [heading, prefix] {
            assert!(ui.find(line).is_ok(), "{line}");
        }
        drop(ui);

        // Lumenveil, as released: its DLLs load from the game's folder.
        let json = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/index/mods.json")).unwrap();
        let list = Mods::parse(&json).unwrap();
        let release = list.mods.iter().find(|listed| listed.id == "lumenveil");
        let release = release.and_then(|listed| listed.newest()).unwrap();
        let _ = state.list_mods(list.clone(), Some(root));
        let seen = mods::Seen {
            held: BTreeMap::from([(
                "lumenveil".to_owned(),
                crate::core::mods::Status {
                    release: Some(release.version.clone()),
                    problems: Vec::new(),
                },
            )]),
            foreign: BTreeMap::new(),
            unfit: Default::default(),
            excluded: BTreeMap::new(),
            lacking: BTreeMap::new(),
        };
        let _ = update(
            &mut state,
            &mut config,
            Message::Mods(mods::Message::Found(root.into(), seen)),
        );
        let natives: Vec<String> = release
            .natives()
            .iter()
            .map(|dll| format!("{dll}=n,b"))
            .collect();

        let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));
        for line in [
            heading,
            prefix,
            &format!("- WINEDLLOVERRIDES='{}'", natives.join(";")),
        ] {
            assert!(ui.find(line).is_ok(), "{line}");
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn how_each_client_starts_is_typed_in() {
        use crate::core::Launch;

        let (mut state, mut config) = opened(None);
        let root = Path::new(WOTLK);

        let prefix = launch::Defaults::of(root).env().unwrap().remove(0);
        let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));
        for default in [prefix.as_str(), launch::default_command()] {
            assert!(ui.find(default).is_ok(), "what is set instead: {default}");
        }
        let _ = ui.click(selector::id(ENV)).unwrap();
        let _ = ui.typewrite("WINEPREFIX=~/wow umu-run");
        let _ = ui.click(selector::id(COMMAND)).unwrap();
        let _ = ui.typewrite("umu-run %command%");
        let messages: Vec<Message> = ui.into_messages().collect();

        for message in messages {
            let _ = update(&mut state, &mut config, message);
        }

        assert_eq!(
            config.launch(root),
            Launch {
                env: "WINEPREFIX=~/wow umu-run".into(),
                command: "umu-run %command%".into(),
            }
        );
        assert_eq!(
            config.launch(Path::new("/games/warmane")),
            Launch::default()
        );
        let mut ui = simulator(state.view(&config, Some(root), None, false, false, None));
        assert!(ui.find("umu-run is not a NAME=value pair.").is_ok());
        drop(ui);

        let closed = update(&mut state, &mut config, Message::Close);
        assert!(matches!(
            closed.instruction,
            Some(Instruction::Close { edited: true })
        ));
    }
}
