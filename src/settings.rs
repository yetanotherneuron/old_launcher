//! The settings dialog, opened by the cog in the header: whether downloads
//! from GitHub may sign in with the GitHub CLI's login.

use iced::widget::{button, checkbox, column, container, row, space, text};
use iced::{Element, Fill};

use crate::action::Action;
use crate::github::{Login, VARIABLES};
use crate::style;

/// The id of the cog that opens the dialog, for tests to find it by.
pub const OPEN: &str = "settings-open";

/// The checkbox's label.
pub const USE_CLI: &str = "Use my GitHub CLI login (gh auth token)";

/// What the status line opens with.
const STATUS: &str = "Current downloads:";

/// Whether the dialog is open.
#[derive(Debug, Default)]
pub struct State {
    open: bool,
}

/// Interactions with the dialog.
#[derive(Debug, Clone)]
pub enum Message {
    /// The GitHub CLI checkbox was ticked or unticked.
    UseCli(bool),
    /// Close the dialog.
    Close,
}

/// What the dialog asks of the launcher.
pub enum Instruction {
    /// Sign in to GitHub with the GitHub CLI's login from now on, or stop:
    /// to remember, and to apply.
    UseCli(bool),
}

impl State {
    /// Whether the dialog is showing.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Shows the dialog.
    pub fn open(&mut self) {
        self.open = true;
    }

    /// Handles an interaction.
    pub fn update(&mut self, message: Message) -> Action<Instruction, Message> {
        match message {
            Message::UseCli(use_cli) => Action::instruction(Instruction::UseCli(use_cli)),
            Message::Close => {
                self.open = false;
                Action::none()
            }
        }
    }

    /// The dialog, with the GitHub CLI box ticked when `use_cli`, saying what
    /// `login` means for downloads.
    pub fn view<'a>(&'a self, use_cli: bool, login: &'a Login) -> Element<'a, Message> {
        let tone = match login {
            Login::Environment { .. } | Login::Cli(_) => text::success,
            Login::CliMissing | Login::CliFailed(_) => text::warning,
            Login::Anonymous | Login::Asking => text::secondary,
        };
        let [first, second] = VARIABLES;

        container(
            column![
                text("Settings").size(20).font(style::BOLD),
                column![
                    text("GitHub").size(16).font(style::BOLD),
                    text(
                        "Many addons download from GitHub, which limits downloads made without \
                         an account. Signed in, the limits are far higher."
                    )
                    .size(13)
                    .style(text::secondary),
                ]
                .spacing(4),
                column![
                    checkbox(use_cli)
                        .label(USE_CLI)
                        .text_size(14)
                        .on_toggle(Message::UseCli),
                    text(format!(
                        "Or set {first} or {second} before starting the launcher. The launcher \
                         never stores a token."
                    ))
                    .size(13)
                    .style(text::secondary),
                ]
                .spacing(6),
                row![
                    text(STATUS).size(13),
                    text(login.status()).size(13).style(tone).width(Fill),
                ]
                .spacing(4),
                row![
                    space::horizontal(),
                    button("Close")
                        .style(button::secondary)
                        .on_press(Message::Close),
                ],
            ]
            .spacing(14),
        )
        .padding(24)
        .width(Fill.max(560))
        .style(style::card)
        .into()
    }
}

#[cfg(test)]
mod tests {
    use iced_test::simulator;

    use super::*;

    #[test]
    fn the_cli_box_names_the_environment_variables_beside_it() {
        let state = State { open: true };
        let mut ui = simulator(state.view(false, &Login::Anonymous));

        for line in [
            USE_CLI,
            "Or set GH_TOKEN or GITHUB_TOKEN before starting the launcher. The launcher never \
             stores a token.",
            STATUS,
            "anonymous",
        ] {
            assert!(ui.find(line).is_ok(), "{line}");
        }
    }

    #[test]
    fn the_dialog_says_what_each_login_means() {
        let state = State { open: true };

        for login in [
            Login::Asking,
            Login::CliMissing,
            Login::CliFailed("no oauth token found for github.com".into()),
        ] {
            let mut ui = simulator(state.view(true, &login));
            assert!(ui.find(login.status()).is_ok(), "{login:?}");
        }
    }

    #[test]
    fn ticking_the_box_asks_to_use_the_cli_and_close_closes() {
        let mut state = State { open: true };

        let mut ui = simulator(state.view(false, &Login::Anonymous));
        let _ = ui.click(USE_CLI).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            matches!(messages[..], [Message::UseCli(true)]),
            "{messages:?}"
        );

        let action = state.update(Message::UseCli(true));
        assert!(matches!(
            action.instruction,
            Some(Instruction::UseCli(true))
        ));
        assert!(state.is_open());

        let _ = state.update(Message::Close);
        assert!(!state.is_open());
    }
}
