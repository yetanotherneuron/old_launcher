//! The form that adds a client to the launcher: a folder on this computer,
//! or one to download with BitTorrent.

use std::path::PathBuf;

use iced::widget::{button, column, container, operation, row, space, text, text_input};
use iced::{Center, Fill, Task, Widget};

use crate::action::Action;
use crate::core::Installation;
use crate::torrent::Source;
use crate::{blocking, style};

/// The id of the path field, for keyboard focus.
pub const PATH: &str = "setup-path";

/// The id of the torrent field, for keyboard focus.
pub const TORRENT: &str = "setup-torrent";

/// The form's state.
pub struct State {
    mode: Mode,
    path: String,
    /// The torrent: a magnet link, the address of a `.torrent` file, or the
    /// file.
    torrent: String,
    /// The folder the torrent downloads into, in a folder of its own.
    into: String,
    stage: Stage,
    error: Option<String>,
    cancellable: bool,
}

/// Where the client comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// A folder on this computer.
    Folder,
    /// A torrent, to download.
    Torrent,
}

enum Stage {
    Editing,
    Picking,
    Checking,
}

/// Interactions with the form.
#[derive(Debug, Clone)]
pub enum Message {
    /// Where the client comes from was chosen.
    Mode(Mode),
    /// The path field was edited.
    PathChanged(String),
    /// "Browse…" was pressed.
    Browse,
    /// The folder picker closed.
    Picked(Option<PathBuf>),
    /// The torrent field was edited.
    TorrentChanged(String),
    /// "Choose file…" was pressed.
    ChooseTorrent,
    /// The file picker closed.
    ChoseTorrent(Option<PathBuf>),
    /// The field of the folder to download into was edited.
    IntoChanged(String),
    /// "Browse…" beside it was pressed.
    BrowseInto,
    /// Its folder picker closed.
    PickedInto(Option<PathBuf>),
    /// "Add client" or "Continue" was pressed.
    Submit,
    /// The folder was checked for a 3.3.5a client.
    Checked(Result<Installation, String>),
    /// The form was dismissed.
    Cancel,
}

/// What the form asks of its parent.
pub enum Instruction {
    /// A valid client was chosen.
    Added(Installation),
    /// Download the client of the torrent of `source` into `into`.
    Torrent { source: Source, into: PathBuf },
    /// The user dismissed the form.
    Cancelled,
}

impl State {
    /// An empty form. Without `cancellable`, the form cannot be dismissed:
    /// the launcher has nothing to show until a client is added.
    pub fn new(cancellable: bool) -> Self {
        Self {
            mode: Mode::Folder,
            path: String::new(),
            torrent: String::new(),
            into: dirs::home_dir()
                .map(|home| home.display().to_string())
                .unwrap_or_default(),
            stage: Stage::Editing,
            error: None,
            cancellable,
        }
    }

    /// Says `why` what was submitted was refused, for another try.
    pub fn refuse(&mut self, why: String) {
        self.error = Some(why);
    }

    /// Handles a form interaction.
    pub fn update(&mut self, message: Message) -> Action<Instruction, Message> {
        match message {
            Message::Mode(mode) => {
                self.mode = mode;
                self.error = None;
                Action::task(operation::focus(match mode {
                    Mode::Folder => PATH,
                    Mode::Torrent => TORRENT,
                }))
            }
            Message::PathChanged(path) => {
                self.path = path;
                self.error = None;
                Action::none()
            }
            Message::Browse => {
                self.stage = Stage::Picking;
                Action::task(Task::perform(
                    pick_folder("Select your WoW 3.3.5a folder (the one with Wow.exe)"),
                    Message::Picked,
                ))
            }
            Message::Picked(folder) => {
                self.stage = Stage::Editing;

                let Some(folder) = folder else {
                    return Action::none();
                };

                self.path = folder.display().to_string();
                self.update(Message::Submit)
            }
            Message::TorrentChanged(torrent) => {
                self.torrent = torrent;
                self.error = None;
                Action::none()
            }
            Message::ChooseTorrent => {
                self.stage = Stage::Picking;
                Action::task(Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Select the client's .torrent file")
                            .add_filter("Torrent", &["torrent"])
                            .pick_file()
                            .await
                            .map(|file| file.path().to_path_buf())
                    },
                    Message::ChoseTorrent,
                ))
            }
            Message::ChoseTorrent(file) => {
                self.stage = Stage::Editing;
                if let Some(file) = file {
                    self.torrent = file.display().to_string();
                    self.error = None;
                }
                Action::none()
            }
            Message::IntoChanged(into) => {
                self.into = into;
                self.error = None;
                Action::none()
            }
            Message::BrowseInto => {
                self.stage = Stage::Picking;
                Action::task(Task::perform(
                    pick_folder("Select where to download the client"),
                    Message::PickedInto,
                ))
            }
            Message::PickedInto(folder) => {
                self.stage = Stage::Editing;
                if let Some(folder) = folder {
                    self.into = folder.display().to_string();
                    self.error = None;
                }
                Action::none()
            }
            Message::Submit if !matches!(self.stage, Stage::Editing) => Action::none(),
            Message::Submit => match self.mode {
                Mode::Folder => {
                    let path = PathBuf::from(self.path.trim());
                    if path.as_os_str().is_empty() {
                        return Action::none();
                    }

                    self.stage = Stage::Checking;
                    self.error = None;
                    log::info!("Checking {} for a 3.3.5a client", path.display());
                    Action::task(Task::perform(
                        blocking(move || Installation::open(path)),
                        Message::Checked,
                    ))
                }
                Mode::Torrent => {
                    if self.torrent.trim().is_empty() {
                        return Action::none();
                    }
                    let into = PathBuf::from(self.into.trim());
                    if into.as_os_str().is_empty() {
                        self.error = Some("Choose a folder to download into.".to_owned());
                        return Action::none();
                    }

                    Action::instruction(Instruction::Torrent {
                        source: Source::parse(&self.torrent),
                        into,
                    })
                }
            },
            Message::Checked(Ok(installation)) => {
                log::info!(
                    "Adding the client at {}, version {}",
                    installation.root().display(),
                    installation.version()
                );
                self.stage = Stage::Editing;
                Action::instruction(Instruction::Added(installation))
            }
            Message::Checked(Err(error)) => {
                log::warn!("Not a usable client: {error}");
                self.stage = Stage::Editing;
                self.error = Some(error);
                Action::none()
            }
            Message::Cancel if self.cancellable => Action::instruction(Instruction::Cancelled),
            Message::Cancel => Action::none(),
        }
    }

    /// The form.
    pub fn view(&self) -> impl Widget<Message> + '_ {
        let idle = matches!(self.stage, Stage::Editing);

        let choice = |mode: Mode, label: &'static str| {
            button(text(label).size(14))
                .style(if self.mode == mode {
                    button::primary
                } else {
                    button::secondary
                })
                .on_press_maybe((idle && self.mode != mode).then_some(Message::Mode(mode)))
        };
        let modes = row![
            choice(Mode::Folder, "A folder on this computer"),
            choice(Mode::Torrent, "Download with a torrent"),
        ]
        .spacing(8);

        let fields = match self.mode {
            Mode::Folder => column![
                text(
                    "Choose the folder that contains Wow.exe. Only 3.3.5a clients \
                     (build 12340) are supported for now."
                )
                .style(text::secondary),
                row![
                    text_input("/games/World of Warcraft 3.3.5a", &self.path)
                        .id(PATH)
                        .on_input_maybe(idle.then_some(Message::PathChanged))
                        .on_submit(Message::Submit)
                        .padding(10),
                    browse("Browse…", idle.then_some(Message::Browse)),
                ]
                .spacing(8),
            ],
            Mode::Torrent => column![
                text(
                    "Give a magnet link, the address of a .torrent file, or the file. The \
                     launcher checks it holds a 3.3.5a client before downloading it."
                )
                .style(text::secondary),
                row![
                    text_input("magnet:?xt=… or https://…/client.torrent", &self.torrent)
                        .id(TORRENT)
                        .on_input_maybe(idle.then_some(Message::TorrentChanged))
                        .on_submit(Message::Submit)
                        .padding(10),
                    browse("Choose file…", idle.then_some(Message::ChooseTorrent)),
                ]
                .spacing(8),
                text("Download into").size(14),
                row![
                    text_input("/games", &self.into)
                        .on_input_maybe(idle.then_some(Message::IntoChanged))
                        .on_submit(Message::Submit)
                        .padding(10),
                    browse("Browse…", idle.then_some(Message::BrowseInto)),
                ]
                .spacing(8),
                text("The client gets a folder of its own there, named after the torrent.")
                    .size(13)
                    .style(text::secondary),
            ],
        }
        .spacing(10);

        let error = self
            .error
            .as_deref()
            .map(|error| text(error).style(text::danger));

        let cancel = self.cancellable.then(|| {
            button("Cancel")
                .style(button::text)
                .on_press(Message::Cancel)
        });

        let filled = match self.mode {
            Mode::Folder => !self.path.trim().is_empty(),
            Mode::Torrent => !self.torrent.trim().is_empty(),
        };
        let submit = button(match (&self.stage, self.mode) {
            (Stage::Editing, Mode::Folder) => "Add client",
            (Stage::Editing, Mode::Torrent) => "Continue",
            (Stage::Picking, _) => "Choosing…",
            (Stage::Checking, _) => "Checking…",
        })
        .style(button::primary)
        .on_press_maybe((idle && filled).then_some(Message::Submit));

        container(
            column![
                text("Add a WoW 3.3.5a client").size(22).font(style::BOLD),
                modes,
                fields,
                error,
                row![space::horizontal(), cancel, submit]
                    .spacing(8)
                    .align_y(Center),
            ]
            .spacing(14),
        )
        .padding(24)
        .width(Fill.max(620))
        .style(style::card)
    }
}

/// A button beside a field, which picks what goes in it.
fn browse(label: &str, on_press: Option<Message>) -> impl Widget<Message> + '_ {
    button(label)
        .style(button::secondary)
        .padding(10)
        .on_press_maybe(on_press)
}

/// Asks for a folder, under `title`.
async fn pick_folder(title: &'static str) -> Option<PathBuf> {
    rfd::AsyncFileDialog::new()
        .set_title(title)
        .pick_folder()
        .await
        .map(|folder| folder.path().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced_test::simulator;

    #[test]
    fn the_first_run_form_cannot_be_dismissed() {
        let mut state = State::new(false);

        let mut ui = simulator(state.view());
        assert!(ui.find("Cancel").is_err());
        drop(ui);

        assert!(state.update(Message::Cancel).instruction.is_none());
    }

    #[test]
    fn an_added_client_can_be_dismissed_later() {
        let mut state = State::new(true);

        let mut ui = simulator(state.view());
        let _ = ui.click("Cancel").unwrap();
        let message = ui.into_messages().next().unwrap();

        assert!(matches!(
            state.update(message).instruction,
            Some(Instruction::Cancelled)
        ));
    }

    #[test]
    fn a_rejected_folder_shows_why() {
        let mut state = State::new(false);
        let _ = state.update(Message::PathChanged("/tmp/not-wow".into()));
        let _ = state.update(Message::Submit);

        let mut ui = simulator(state.view());
        assert!(ui.find("Checking…").is_ok());
        drop(ui);

        let _ = state.update(Message::Checked(Err(
            "no Wow.exe found in /tmp/not-wow".into()
        )));
        let mut ui = simulator(state.view());
        assert!(ui.find("no Wow.exe found in /tmp/not-wow").is_ok());
        assert!(ui.find("Add client").is_ok());
    }

    #[test]
    fn a_torrent_downloads_into_the_folder_chosen() {
        let mut state = State::new(false);

        let messages: Vec<Message> = {
            let mut ui = simulator(state.view());
            let _ = ui.click("Download with a torrent").unwrap();
            ui.into_messages().collect()
        };
        for message in messages {
            let _ = state.update(message);
        }
        let mut ui = simulator(state.view());
        assert!(ui.find("Download into").is_ok());
        assert!(ui.find("Continue").is_ok());
        drop(ui);

        let _ = state.update(Message::TorrentChanged(" magnet:?xt=urn:btih:abc ".into()));
        let _ = state.update(Message::IntoChanged(String::new()));
        assert!(state.update(Message::Submit).instruction.is_none());
        let mut ui = simulator(state.view());
        assert!(ui.find("Choose a folder to download into.").is_ok());
        drop(ui);

        let _ = state.update(Message::IntoChanged("/games".into()));
        let Some(Instruction::Torrent { source, into }) = state.update(Message::Submit).instruction
        else {
            panic!("no torrent to download");
        };
        assert_eq!(source, Source::Magnet("magnet:?xt=urn:btih:abc".into()));
        assert_eq!(into, PathBuf::from("/games"));
    }
}
