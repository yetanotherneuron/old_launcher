//! The clients dialog: which client the launcher shows, what its folder
//! holds, outside Windows how the game starts, and its mods.

pub mod mods;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use iced::widget::{
    Row, TextInput, button, checkbox, column, container, pick_list, row, scrollable, space, stack,
    text, text_input,
};
use iced::{Center, Element, Fill, Font, Function, Task, Widget, padding};

use crate::action::Action;
use crate::core::launch::DEFAULT_COMMAND;
use crate::core::mods::Mods;
use crate::core::{Config, Details, Installation, Realmlist};
use crate::errors::Errors;
use crate::{blocking, game, github, size, style};

/// The id of the button that adds a client.
pub const ADD: &str = "clients-add";

/// The id of the button that removes the client shown.
pub const REMOVE: &str = "clients-remove";

/// The id of the button that opens the realmlist in an editor.
pub const EDIT_REALMLIST: &str = "clients-edit-realmlist";

/// The id of the environment variables field.
pub const ENV: &str = "clients-env";

/// The id of the command field.
pub const COMMAND: &str = "clients-command";

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
    /// The launcher's window got the focus back, maybe from the editor.
    Focused,
    /// The realmlist of a client folder was read again.
    Reread(PathBuf, Result<Option<Realmlist>, String>),
    /// A language was picked for the client shown.
    Locale(String),
    /// The client shown was switched to this language, or couldn't be.
    Switched(String, Result<Installation, String>),
    /// The box letting the game use 4 GB of memory was ticked or unticked.
    LargeAddressAware(bool),
    /// `Wow.exe` of a client folder was changed to let the game use 4 GB of
    /// memory, or 2, or couldn't be.
    Patched(PathBuf, bool, Result<(), String>),
    /// The environment variables were edited.
    Env(String),
    /// The command was edited.
    Command(String),
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
}

impl State {
    /// Opens the dialog on the `current` client, reading its folder if it
    /// wasn't yet or changed since.
    pub fn open(&mut self, current: Option<&Path>) -> Task<Message> {
        self.open = true;
        self.removing = false;

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
                            locales => locales.join(", "),
                        },
                        details
                            .realmlist
                            .as_ref()
                            .and_then(|realmlist| realmlist.server.as_deref())
                            .unwrap_or("no realmlist")
                    ),
                    Err(error) => log::warn!("Could not read {}: {error}", path.display()),
                }
                let mods = match &result {
                    Ok(_) => self.mods.shown(&path).map(Message::Mods),
                    Err(_) => Task::none(),
                };
                let _ = self.details.insert(path, Some(result));
                Action::task(mods)
            }
            Message::EditRealmlist(path) => {
                log::info!("Opening {} in an editor", path.display());

                // Windows has nothing set to open .wtf files with.
                let opened = if cfg!(windows) {
                    open::with_detached(&path, "notepad")
                } else {
                    open::that_detached(&path)
                };

                if let Err(error) = opened {
                    errors.report(format!("Could not open {}: {error}", path.display()));
                }

                Action::none()
            }
            Message::Focused => {
                let Some(root) = current.filter(|root| {
                    self.open && matches!(self.details.get(*root), Some(Some(Ok(_))))
                }) else {
                    return Action::none();
                };
                Action::task(reread(root.to_path_buf()))
            }
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
                    .filter(|installation| installation.locale() != Some(locale.as_str()))
                    .filter(|_| !playing)
                    .cloned()
                else {
                    return Action::none();
                };
                log::info!("Switching {} to {locale}", installation.root().display());

                Action::task(Task::perform(
                    blocking({
                        let locale = locale.clone();
                        move || installation.set_locale(&locale).map(|()| installation)
                    }),
                    Message::Switched.with(locale),
                ))
            }
            Message::Switched(_, Ok(installation)) => {
                let root = installation.root().to_path_buf();
                if let Some(Some(Ok(details))) = self.details.get_mut(&root) {
                    details.locale = installation.locale().map(str::to_owned);
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
                        Action::task(Task::batch([task, read]))
                    }
                    None => Action::task(task),
                }
            }
        }
    }

    /// The dialog, showing the `current` client of `config`; `installation`
    /// is that client, once it opened, and `playing` whether its game runs.
    pub fn view<'a>(
        &'a self,
        config: &'a Config,
        current: Option<&'a Path>,
        installation: Option<&'a Installation>,
        playing: bool,
    ) -> impl Widget<Message> + 'a {
        let title = row![
            text("Clients").size(20).font(style::BOLD),
            space::horizontal(),
            button("Close").style(button::text).on_press(Message::Close),
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
                style::PLUS,
                PICKER_BUTTON,
                button::secondary,
                "Add a client",
                Some(Message::Add),
            ),
            style::icon_button(
                REMOVE,
                style::TRASH,
                PICKER_BUTTON,
                button::danger,
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
                        .style(button::text)
                        .on_press(Message::CancelRemove),
                    button("Remove")
                        .style(button::danger)
                        .on_press(Message::ConfirmRemove),
                ]
                .spacing(8)
                .align_y(Center),
            )
            .padding([6, 12])
            .style(style::banner(style::Tone::Danger))
        });

        let about = current.map(|root| {
            // What the folder holds, or why it isn't known (yet).
            let details: Element<'a, Message> = match self.details.get(root) {
                Some(Some(Ok(details))) => column![
                    languages(details, playing),
                    match &details.realmlist {
                        Some(realmlist) => fact(
                            "Realmlist",
                            realmlist
                                .server
                                .clone()
                                .unwrap_or_else(|| "Not set".to_owned()),
                        )
                        .push(
                            style::icon_button(
                                EDIT_REALMLIST,
                                style::PEN,
                                26.0,
                                button::text,
                                "Edit realmlist.wtf",
                                Some(Message::EditRealmlist(realmlist.path.clone())),
                            )
                            .boxed(),
                        )
                        .align_y(Center),
                        None => fact("Realmlist", "No realmlist.wtf".to_owned()),
                    },
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
                    fact("Folder", root.display().to_string()),
                    installation
                        .map(|installation| fact("Version", installation.version().to_string())),
                    details,
                ]
                .spacing(6),
            )
            .padding(12)
            .width(Fill)
            .style(container::rounded_box)
        });

        // Windows starts Wow.exe itself.
        let launch = current.filter(|_| !cfg!(windows)).map(|root| {
            let launch = config.launch.get(root);
            let env = launch.map_or("", |launch| launch.env.as_str());
            let command = launch.map_or("", |launch| launch.command.as_str());

            column![
                text("Starting the game").size(16).font(style::BOLD),
                field(
                    "Environment variables",
                    text_input("WINEPREFIX=~/.wine-wow WINEDEBUG=-all", env)
                        .id(ENV)
                        .on_input(Message::Env),
                    launch.and_then(|launch| launch.env_problem()),
                ),
                field(
                    "Command",
                    text_input(DEFAULT_COMMAND, command)
                        .id(COMMAND)
                        .on_input(Message::Command),
                    launch.and_then(|launch| launch.command_problem()),
                ),
                text(
                    "Left empty, the game runs with wine %command%. Other runners work \
                     too, such as umu-run %command% for Proton."
                )
                .size(13)
                .style(text::secondary),
            ]
            .spacing(10)
        });

        // Nothing to say of a folder that can't be read.
        let mods = current
            .filter(|root| !matches!(self.details.get(*root), Some(Some(Err(_)))))
            .map(|root| self.mods.view(root, playing).boxed().map(Message::Mods));

        // A short window scrolls what is under the title, so Close stays in
        // sight.
        let body =
            scrollable(column![picker, removal, about, launch, mods].spacing(14)).spacing(10);

        container(column![title, body].spacing(14))
            .padding(24)
            .width(Fill.max(620))
            .style(style::card)
    }
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
fn languages(details: &Details, playing: bool) -> Row<Element<'_, Message>> {
    match details.locales.as_slice() {
        [] => fact("Languages", "None found".to_owned()),
        [locale] => fact("Languages", locale.clone()),
        locales => {
            let picker = pick_list(details.locale.clone(), locales, String::clone)
                .text_size(14)
                .padding([2, 10]);

            let language = if playing {
                labelled("Language", style::tip(picker, game::KEEPS_CONFIG))
            } else {
                labelled("Language", picker.on_select(Message::Locale))
            };

            language.align_y(Center)
        }
    }
}

/// Whether the game may use 4 GB of memory (`on`), as `Wow.exe` says, which
/// is changed there, but not while the game runs (`playing`).
fn memory<'a>(on: bool, playing: bool) -> Row<Element<'a, Message>> {
    let check = checkbox(on)
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
            locales: vec!["enUS".into()],
            locale: Some("enUS".into()),
            realmlist,
            large_address_aware: Some(false),
        }
    }

    fn realmlist(server: Option<&str>) -> Realmlist {
        Realmlist {
            path: PathBuf::from(WOTLK).join("Data/enUS/realmlist.wtf"),
            server: server.map(str::to_owned),
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

        let mut ui = simulator(state.view(&config, Some(root), None, false));
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

        let mut ui = simulator(state.view(&config, Some(root), None, false));
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
            let mut ui = simulator(state.view(&config, Some(root), None, false));
            assert!(ui.find("Memory").is_ok());
            let _ = ui.click(label).unwrap();
            ui.into_messages().collect()
        };
        assert!(matches!(messages[..], [Message::LargeAddressAware(true)]));

        let messages: Vec<Message> = {
            let mut ui = simulator(state.view(&config, Some(root), None, true));
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
            iced::widget::center(state.view(&config, Some(root), None, false)),
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

        let mut ui = simulator(state.view(&config, Some(root), Some(&installation), false));
        assert!(ui.find("Language").is_ok());
        assert!(ui.find("us.example.com").is_ok());
        let _ = ui.click("enUS").unwrap();
        let _ = ui.click("esES").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            matches!(messages.as_slice(), [Message::Locale(locale)] if locale == "esES"),
            "{messages:?}"
        );

        let switch = state.update(
            Message::Locale("esES".into()),
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
        switched.set_locale("esES").unwrap();
        let done = state.update(
            Message::Switched("esES".into(), Ok(switched)),
            &mut config,
            Some(root),
            Some(&installation),
            false,
            Err("offline"),
            &mut errors,
        );
        assert!(matches!(
            done.instruction,
            Some(Instruction::Switched(switched)) if switched.locale() == Some("esES")
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

        let mut ui = simulator(state.view(&config, Some(root), Some(&installation), false));
        assert!(ui.find("esES").is_ok());
        assert!(ui.find("es.example.com").is_ok());
        assert!(errors.messages().is_empty(), "{:?}", errors.messages());
    }

    #[test]
    fn picking_the_language_played_in_writes_nothing() {
        let (_folder, installation, mut state, mut config) = bilingual();

        let action = state.update(
            Message::Locale("enUS".into()),
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

        let mut ui = simulator(state.view(&config, Some(root), Some(&installation), true));
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
                "esES".into(),
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
        let (mut state, _) = opened(Some(details(None)));
        let root = Path::new(WOTLK);

        state.close();
        assert_eq!(state.open(Some(root)).units(), 0, "remembered");

        // Addons came or went while the dialog was closed: only its mods are
        // looked for at once, for the banners above the addons.
        state.close();
        assert_eq!(state.changed(root, None).units(), 1, "read when next shown");
        assert!(state.open(Some(root)).units() > 0);
        assert!(
            matches!(state.details.get(root), Some(Some(Ok(_)))),
            "the old reading shows meanwhile"
        );

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
                    release: None,
                    problems: vec![crate::core::mods::Problem::Unchecked],
                },
            )]),
            foreign: BTreeMap::new(),
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
                "Lumenveil is in this client. The launcher needs to check it before starting the \
                 game."
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
    fn a_realmlist_is_edited_in_place_and_read_again_on_return() {
        let (mut state, mut config) = opened(Some(details(Some(realmlist(None)))));
        let root = Path::new(WOTLK);

        let mut ui = simulator(state.view(&config, Some(root), None, false));
        assert!(ui.find("Not set").is_ok());
        let _ = ui.click(selector::id(EDIT_REALMLIST)).unwrap();
        // Not handled here: that would open an editor.
        assert!(matches!(
            ui.into_messages().next(),
            Some(Message::EditRealmlist(path)) if path == realmlist(None).path
        ));

        let refresh = update(&mut state, &mut config, Message::Focused);
        assert!(refresh.task.units() > 0, "read again");
        let _ = update(
            &mut state,
            &mut config,
            Message::Reread(root.into(), Ok(Some(realmlist(Some("logon.example.com"))))),
        );

        let mut ui = simulator(state.view(&config, Some(root), None, false));
        assert!(ui.find("logon.example.com").is_ok());
    }

    #[test]
    fn a_client_without_a_realmlist_offers_nothing_to_edit() {
        let (state, config) = opened(Some(details(None)));

        let mut ui = simulator(state.view(&config, Some(Path::new(WOTLK)), None, false));

        assert!(ui.find("No realmlist.wtf").is_ok());
        assert!(ui.find(selector::id(EDIT_REALMLIST)).is_err());
    }

    #[test]
    fn picking_a_client_shows_it_and_reads_its_folder() {
        let (mut state, mut config) = opened(None);
        let root = Path::new(WOTLK);

        let mut ui = simulator(state.view(&config, Some(root), None, false));
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

        let mut ui = simulator(state.view(&config, Some(root), None, false));
        let _ = ui.click(selector::id(REMOVE)).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            let action = update(&mut state, &mut config, message);
            assert!(action.instruction.is_none(), "not yet");
        }

        let mut ui = simulator(state.view(&config, Some(root), None, false));
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
        let mut ui = simulator(state.view(&config, Some(root), None, false));
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

        let mut ui = simulator(state.view(&config, Some(Path::new(WOTLK)), None, false));
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

    #[cfg(not(windows))]
    #[test]
    fn how_each_client_starts_is_typed_in() {
        use crate::core::Launch;

        let (mut state, mut config) = opened(None);
        let root = Path::new(WOTLK);

        let mut ui = simulator(state.view(&config, Some(root), None, false));
        assert!(ui.find("wine %command%").is_ok(), "the default, as a hint");
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
        let mut ui = simulator(state.view(&config, Some(root), None, false));
        assert!(ui.find("umu-run is not a NAME=value pair.").is_ok());
        drop(ui);

        let closed = update(&mut state, &mut config, Message::Close);
        assert!(matches!(
            closed.instruction,
            Some(Instruction::Close { edited: true })
        ));
    }
}
