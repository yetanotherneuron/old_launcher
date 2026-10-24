//! The form that adds a client folder to the launcher.

use std::path::PathBuf;

use iced::widget::{button, column, container, row, space, text, text_input};
use iced::{Center, Fill, Task, Widget};

use crate::action::Action;
use crate::core::Installation;
use crate::{blocking, style};

/// The id of the path field, for keyboard focus.
pub const PATH: &str = "setup-path";

/// The form's state.
pub struct State {
    path: String,
    stage: Stage,
    error: Option<String>,
    cancellable: bool,
}

enum Stage {
    Editing,
    Picking,
    Checking,
}

/// Interactions with the form.
#[derive(Debug, Clone)]
pub enum Message {
    /// The path field was edited.
    PathChanged(String),
    /// "Browse…" was pressed.
    Browse,
    /// The folder picker closed.
    Picked(Option<PathBuf>),
    /// "Add client" was pressed.
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
    /// The user dismissed the form.
    Cancelled,
}

impl State {
    /// An empty form. Without `cancellable`, the form cannot be dismissed:
    /// the launcher has nothing to show until a client is added.
    pub fn new(cancellable: bool) -> Self {
        Self {
            path: String::new(),
            stage: Stage::Editing,
            error: None,
            cancellable,
        }
    }

    /// Handles a form interaction.
    pub fn update(&mut self, message: Message) -> Action<Instruction, Message> {
        match message {
            Message::PathChanged(path) => {
                self.path = path;
                self.error = None;
                Action::none()
            }
            Message::Browse => {
                self.stage = Stage::Picking;
                Action::task(Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .set_title("Select your WoW 3.3.5a folder (the one with Wow.exe)")
                            .pick_folder()
                            .await
                            .map(|folder| folder.path().to_path_buf())
                    },
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
            Message::Submit => {
                let path = PathBuf::from(self.path.trim());

                if path.as_os_str().is_empty() || !matches!(self.stage, Stage::Editing) {
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

        let path = text_input("/games/World of Warcraft 3.3.5a", &self.path)
            .id(PATH)
            .on_input_maybe(idle.then_some(Message::PathChanged))
            .on_submit(Message::Submit)
            .padding(10);

        let browse = button("Browse…")
            .style(button::secondary)
            .padding(10)
            .on_press_maybe(idle.then_some(Message::Browse));

        let error = self
            .error
            .as_deref()
            .map(|error| text(error).style(text::danger));

        let cancel = self.cancellable.then(|| {
            button("Cancel")
                .style(button::text)
                .on_press(Message::Cancel)
        });

        let submit = button(match self.stage {
            Stage::Editing => "Add client",
            Stage::Picking => "Choosing folder…",
            Stage::Checking => "Checking…",
        })
        .style(button::primary)
        .on_press_maybe((idle && !self.path.trim().is_empty()).then_some(Message::Submit));

        container(
            column![
                text("Add a WoW 3.3.5a client").size(22).font(style::BOLD),
                text(
                    "Choose the folder that contains Wow.exe. Only 3.3.5a clients \
                     (build 12340) are supported for now."
                )
                .style(text::secondary),
                row![path, browse].spacing(8),
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
}
