//! The clients dialog: which client the launcher shows, what its folder
//! holds and, outside Windows, how the game starts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use iced::widget::{
    Row, TextInput, button, column, container, pick_list, row, space, stack, text, text_input,
};
use iced::{Center, Element, Fill, Font, Function, Task, padding};

use crate::action::Action;
use crate::core::launch::DEFAULT_COMMAND;
use crate::core::{Config, Details, Installation, Realmlist};
use crate::errors::Errors;
use crate::{blocking, size, style};

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
}

/// What the dialog asks of its parent.
pub enum Instruction {
    /// Show the client at this folder.
    Select(PathBuf),
    /// Open the form that adds a client.
    Add,
    /// Forget the client shown.
    Remove,
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

    /// Closes the dialog without a word to the parent: there is nothing
    /// left to manage.
    pub fn close(&mut self) {
        self.open = false;
        self.removing = false;
    }

    /// Notes that the client folder at `root` changed (addons came or went),
    /// so what it holds is read again: now, if the dialog shows it
    /// (`current`), or when it next does.
    pub fn changed(&mut self, root: &Path, current: Option<&Path>) -> Task<Message> {
        let _ = self.stale.insert(root.to_path_buf());

        if self.open && current == Some(root) {
            self.read(root)
        } else {
            Task::none()
        }
    }

    /// Reads what the client folder at `root` holds, unless it was read
    /// already and hasn't changed since.
    pub fn read(&mut self, root: &Path) -> Task<Message> {
        if self.details.contains_key(root) && !self.stale.remove(root) {
            return Task::none();
        }
        let _ = self.details.entry(root.to_path_buf()).or_insert(None);

        let root = root.to_path_buf();

        Task::perform(
            blocking({
                let root = root.clone();
                move || Details::read(&root)
            }),
            Message::Read.with(root),
        )
    }

    /// Handles an interaction; how the `current` client starts is edited
    /// right in `config`, and failures go to `errors`.
    pub fn update(
        &mut self,
        message: Message,
        config: &mut Config,
        current: Option<&Path>,
        errors: &mut Errors,
    ) -> Action<Instruction, Message> {
        match message {
            Message::Select(path) => {
                self.removing = false;
                let read = self.read(&path);
                Action::new(Instruction::Select(path), read)
            }
            Message::Read(path, result) => {
                let _ = self.details.insert(path, Some(result));
                Action::none()
            }
            Message::EditRealmlist(path) => {
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
                let root = root.to_path_buf();

                Action::task(Task::perform(
                    blocking({
                        let root = root.clone();
                        move || Ok(Realmlist::of(&root))
                    }),
                    Message::Reread.with(root),
                ))
            }
            Message::Reread(path, Ok(realmlist)) => {
                if let Some(Some(Ok(details))) = self.details.get_mut(&path) {
                    details.realmlist = realmlist;
                }
                Action::none()
            }
            // The last reading stays: it was only a refresh.
            Message::Reread(_, Err(_)) => Action::none(),
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
        }
    }

    /// The dialog, showing the `current` client of `config`; `installation`
    /// is that client, once it opened.
    pub fn view<'a>(
        &'a self,
        config: &'a Config,
        current: Option<&'a Path>,
        installation: Option<&'a Installation>,
    ) -> Element<'a, Message> {
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

        let about =
            current.map(|root| {
                let details: Element<'a, Message> = match self.details.get(root) {
                    Some(Some(Ok(details))) => column![
                        fact(
                            "Languages",
                            if details.locales.is_empty() {
                                "None found".to_owned()
                            } else {
                                details.locales.join(", ")
                            }
                        ),
                        match &details.realmlist {
                            Some(realmlist) => fact(
                                "Realmlist",
                                realmlist
                                    .server
                                    .clone()
                                    .unwrap_or_else(|| "Not set".to_owned()),
                            )
                            .push(style::icon_button(
                                EDIT_REALMLIST,
                                style::PEN,
                                26.0,
                                button::text,
                                "Edit realmlist.wtf",
                                Some(Message::EditRealmlist(realmlist.path.clone())),
                            ))
                            .align_y(Center),
                            None => fact("Realmlist", "No realmlist.wtf".to_owned()),
                        },
                        fact("Size", size(details.size)),
                    ]
                    .spacing(6)
                    .into(),
                    Some(Some(Err(error))) => text(format!("Could not read the folder: {error}"))
                        .style(text::danger)
                        .into(),
                    Some(None) | None => text("Reading the folder…").style(text::secondary).into(),
                };

                container(
                    column![fact("Folder", root.display().to_string())]
                        .push(installation.map(|installation| {
                            fact("Version", installation.version().to_string())
                        }))
                        .push(details)
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
                text("Starting the game").size(14).font(style::BOLD),
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
                    "%command% stands for Wow.exe; left empty, the game runs with \
                     wine %command%. Other runners work too: umu-run %command% for \
                     Proton, or lutris lutris:rungameid/1 for a game set up in Lutris."
                )
                .size(12)
                .style(text::secondary),
            ]
            .spacing(10)
        });

        container(
            column![title, picker]
                .push(removal)
                .push(about)
                .push(launch)
                .spacing(14),
        )
        .padding(24)
        .width(Fill.max(620))
        .style(style::card)
        .into()
    }
}

/// A labelled fact about the client, to which more can be pushed after the
/// value.
fn fact<'a>(label: &'a str, value: String) -> Row<'a, Message> {
    row![
        text(label).width(100).style(text::secondary),
        text(value).wrapping(text::Wrapping::WordOrGlyph),
    ]
    .spacing(12)
}

/// A labelled text field, with what is wrong with its contents under it.
fn field<'a>(
    label: &'a str,
    input: TextInput<'a, Message>,
    problem: Option<String>,
) -> Element<'a, Message> {
    column![text(label).size(13), input.padding(8)]
        .push(problem.map(|problem| text(problem).size(12).style(text::danger)))
        .spacing(4)
        .into()
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
            locales: vec!["enGB".into(), "enUS".into()],
            realmlist,
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

        let mut ui = simulator(state.view(&config, Some(root), None));
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

        let mut ui = simulator(state.view(&config, Some(root), None));
        for line in [WOTLK, "enGB, enUS", "logon.example.com", "17.3 GB"] {
            assert!(ui.find(line).is_ok(), "{line}");
        }
    }

    #[test]
    fn what_a_folder_holds_is_read_once_until_it_changes() {
        let (mut state, _) = opened(Some(details(None)));
        let root = Path::new(WOTLK);

        state.close();
        assert_eq!(state.open(Some(root)).units(), 0, "remembered");

        // Addons came or went while the dialog was closed.
        state.close();
        assert_eq!(state.changed(root, None).units(), 0, "read when next shown");
        assert!(state.open(Some(root)).units() > 0);
        assert!(
            matches!(state.details.get(root), Some(Some(Ok(_)))),
            "the old reading shows meanwhile"
        );

        // And while it is open and showing that client.
        assert!(state.changed(root, Some(root)).units() > 0);
        assert_eq!(
            state
                .changed(Path::new("/games/warmane"), Some(root))
                .units(),
            0,
            "another client: when next shown"
        );
    }

    #[test]
    fn a_realmlist_is_edited_in_place_and_read_again_on_return() {
        let (mut state, mut config) = opened(Some(details(Some(realmlist(None)))));
        let root = Path::new(WOTLK);

        let mut ui = simulator(state.view(&config, Some(root), None));
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

        let mut ui = simulator(state.view(&config, Some(root), None));
        assert!(ui.find("logon.example.com").is_ok());
    }

    #[test]
    fn a_client_without_a_realmlist_offers_nothing_to_edit() {
        let (state, config) = opened(Some(details(None)));

        let mut ui = simulator(state.view(&config, Some(Path::new(WOTLK)), None));

        assert!(ui.find("No realmlist.wtf").is_ok());
        assert!(ui.find(selector::id(EDIT_REALMLIST)).is_err());
    }

    #[test]
    fn picking_a_client_shows_it_and_reads_its_folder() {
        let (mut state, mut config) = opened(None);
        let root = Path::new(WOTLK);

        let mut ui = simulator(state.view(&config, Some(root), None));
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

        let mut ui = simulator(state.view(&config, Some(root), None));
        let _ = ui.click(selector::id(REMOVE)).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            let action = update(&mut state, &mut config, message);
            assert!(action.instruction.is_none(), "not yet");
        }

        let mut ui = simulator(state.view(&config, Some(root), None));
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
        let mut ui = simulator(state.view(&config, Some(root), None));
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

        let mut ui = simulator(state.view(&config, Some(Path::new(WOTLK)), None));
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

        let mut ui = simulator(state.view(&config, Some(root), None));
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
        let mut ui = simulator(state.view(&config, Some(root), None));
        assert!(ui.find("umu-run is not a NAME=value pair.").is_ok());
        drop(ui);

        let closed = update(&mut state, &mut config, Message::Close);
        assert!(matches!(
            closed.instruction,
            Some(Instruction::Close { edited: true })
        ));
    }
}
