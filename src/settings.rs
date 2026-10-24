//! The settings dialog, opened by the cog in the header: whether downloads
//! from GitHub may sign in with the GitHub CLI's login, and whether Discord
//! shows the game.

use iced::widget::{button, checkbox, column, container, row, space, text};
use iced::{Fill, Widget};

use crate::action::Action;
use crate::core::Config;
use crate::github::{Login, VARIABLES};
use crate::style;

/// The id of the cog that opens the dialog, for tests to find it by.
pub const OPEN: &str = "settings-open";

/// The GitHub CLI checkbox's label.
pub const USE_CLI: &str = "Use my GitHub CLI login (gh auth token)";

/// What the status line opens with.
const STATUS: &str = "Current downloads:";

/// The Discord checkbox's label.
pub const SHOW_ON_DISCORD: &str = "Show on Discord when I'm playing";

/// What showing the game on Discord takes, and when it shows.
const ON_DISCORD: &str = "From the next game on, Discord's desktop app shows you playing World \
                          of Warcraft 3.3.5a while a game started here runs, even with the \
                          launcher closed.";

/// The games Discord isn't told of: their command hands them over and
/// exits.
const HANDED_OVER: &str = "Not for games handed to Steam or Lutris.";

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
    /// The Discord checkbox was ticked or unticked.
    ShowOnDiscord(bool),
    /// Close the dialog.
    Close,
}

/// What the dialog asks of the launcher.
pub enum Instruction {
    /// Sign in to GitHub with the GitHub CLI's login from now on, or stop:
    /// to remember, and to apply.
    UseCli(bool),
    /// Show the game on Discord from the next one on, or stop: to remember.
    ShowOnDiscord(bool),
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
            Message::ShowOnDiscord(show) => Action::instruction(Instruction::ShowOnDiscord(show)),
            Message::Close => {
                self.open = false;
                Action::none()
            }
        }
    }

    /// The dialog, with the boxes ticked as `config` says, saying what
    /// `login` means for downloads.
    pub fn view<'a>(&'a self, config: &Config, login: &'a Login) -> impl Widget<Message> + use<'a> {
        let tone = match login {
            Login::Environment { .. } | Login::Cli(_) => text::success,
            Login::CliMissing | Login::CliFailed(_) => text::warning,
            Login::Anonymous | Login::Asking => text::secondary,
        };
        let [first, second] = VARIABLES;
        // Steam and Lutris run games on Linux only.
        let on_discord = if cfg!(windows) {
            ON_DISCORD.to_owned()
        } else {
            format!("{ON_DISCORD} {HANDED_OVER}")
        };

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
                    checkbox(config.github_cli)
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
                column![
                    text("Discord").size(16).font(style::BOLD),
                    text(on_discord).size(13).style(text::secondary),
                ]
                .spacing(4),
                checkbox(config.discord)
                    .label(SHOW_ON_DISCORD)
                    .text_size(14)
                    .on_toggle(Message::ShowOnDiscord),
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
    }
}

#[cfg(test)]
mod tests {
    use iced_test::simulator;

    use super::*;

    #[test]
    fn the_cli_box_names_the_environment_variables_beside_it() {
        let state = State { open: true };
        let mut ui = simulator(state.view(&Config::default(), &Login::Anonymous));

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
        let config = Config {
            github_cli: true,
            ..Config::default()
        };

        for login in [
            Login::Asking,
            Login::CliMissing,
            Login::CliFailed("no oauth token found for github.com".into()),
        ] {
            let mut ui = simulator(state.view(&config, &login));
            assert!(ui.find(login.status()).is_ok(), "{login:?}");
        }
    }

    #[test]
    fn ticking_the_box_asks_to_use_the_cli_and_close_closes() {
        let mut state = State { open: true };

        let mut ui = simulator(state.view(&Config::default(), &Login::Anonymous));
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

    #[test]
    fn ticking_the_discord_box_asks_to_show_the_game() {
        let mut state = State { open: true };

        let mut ui = simulator(state.view(&Config::default(), &Login::Anonymous));
        assert!(ui.find("Discord").is_ok());
        assert!(ui.find(SHOW_ON_DISCORD).is_ok());
        let explanation = if cfg!(windows) {
            ON_DISCORD.to_owned()
        } else {
            format!("{ON_DISCORD} {HANDED_OVER}")
        };
        assert!(ui.find(explanation.as_str()).is_ok());

        let _ = ui.click(SHOW_ON_DISCORD).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            matches!(messages[..], [Message::ShowOnDiscord(true)]),
            "{messages:?}"
        );

        let action = state.update(Message::ShowOnDiscord(true));
        assert!(matches!(
            action.instruction,
            Some(Instruction::ShowOnDiscord(true))
        ));

        let config = Config {
            discord: true,
            ..Config::default()
        };
        let mut ui = simulator(state.view(&config, &Login::Anonymous));
        let _ = ui.click(SHOW_ON_DISCORD).unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(
            matches!(messages[..], [Message::ShowOnDiscord(false)]),
            "ticked, so unticking: {messages:?}"
        );
    }
}
