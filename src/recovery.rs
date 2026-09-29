//! What the launcher shows instead of starting when its settings file can't
//! be read: why, what overwriting it would lose, and the ways out.

use std::path::PathBuf;

use iced::widget::{button, column, container, row, space, text};
use iced::{Element, Fill, Font};

use crate::action::Action;
use crate::core::{self, Config};
use crate::errors::Errors;
use crate::{describe, style};

/// The recovery screen's state.
pub struct State {
    /// The settings file.
    path: PathBuf,
    /// Why it can't be read.
    error: String,
    /// The client folders it still seems to list.
    clients: Vec<PathBuf>,
    /// Whether it was read again and still failed.
    retried: bool,
}

/// Interactions with the recovery screen.
#[derive(Debug, Clone)]
pub enum Message {
    /// Show the settings file's folder in the file manager.
    OpenFolder,
    /// Open the settings file in the default editor.
    Edit,
    /// Read the settings file again, after fixing it.
    Retry,
    /// Leave the launcher, to fix the file by hand.
    Quit,
    /// Replace the settings file with empty settings.
    Overwrite,
}

/// What the recovery screen asks of its parent.
pub enum Instruction {
    /// The settings file reads fine now.
    Loaded(Config),
    /// Start with empty settings, replacing the file.
    Overwrite,
    /// Close the launcher.
    Quit,
}

impl State {
    /// The screen for the settings file at `path`, which failed to load with
    /// `error`.
    pub fn new(path: PathBuf, error: &core::Error) -> Self {
        Self {
            clients: Config::salvage(&path),
            error: describe(error).trim_end().to_owned(),
            path,
            retried: false,
        }
    }

    /// Handles an interaction; what fails goes to `errors`.
    pub fn update(
        &mut self,
        message: Message,
        errors: &mut Errors,
    ) -> Action<Instruction, Message> {
        match message {
            Message::OpenFolder => {
                let folder = self.path.parent().unwrap_or(&self.path);

                if let Err(error) = open::that_detached(folder) {
                    errors.report(format!("Could not open {}: {error}", folder.display()));
                }

                Action::none()
            }
            Message::Edit => {
                if let Err(error) = open::that_detached(&self.path) {
                    errors.report(format!(
                        "Could not open {} in an editor: {error}",
                        self.path.display()
                    ));
                }

                Action::none()
            }
            Message::Retry => match Config::load(&self.path) {
                Ok(config) => Action::instruction(Instruction::Loaded(config)),
                Err(error) => {
                    *self = Self {
                        retried: true,
                        ..Self::new(self.path.clone(), &error)
                    };
                    Action::none()
                }
            },
            Message::Quit => Action::instruction(Instruction::Quit),
            Message::Overwrite => Action::instruction(Instruction::Overwrite),
        }
    }

    /// The explanation and the choices.
    pub fn view(&self) -> Element<'_, Message> {
        let path = self.path.display().to_string();

        let error = container(text(&self.error).font(Font::MONOSPACE).size(13))
            .padding(10)
            .width(Fill)
            .style(container::rounded_box);

        let retried = self.retried.then(|| {
            text("Read again just now: still unreadable.")
                .size(13)
                .style(text::warning)
        });

        // What overwriting forgets: the file holds nothing else.
        let loss: Element<'_, Message> = if self.clients.is_empty() {
            text(
                "No client folder can be made out in it, so overwriting it loses nothing \
                 recognizable. If you had added clients, you would add them again.",
            )
            .into()
        } else {
            column![text(if self.clients.len() == 1 {
                "Overwriting it forgets the client folder it lists, which you would add again:"
                    .to_owned()
            } else {
                format!(
                    "Overwriting it forgets the {} client folders it lists, and which one was \
                     selected. You would add them again:",
                    self.clients.len()
                )
            })]
            .extend(self.clients.iter().map(|client| {
                text(format!("• {}", client.display()))
                    .font(Font::MONOSPACE)
                    .size(13)
                    .into()
            }))
            .spacing(4)
            .into()
        };

        container(
            column![
                text("The settings file can't be read")
                    .size(22)
                    .font(style::BOLD),
                text(
                    "It lists the client folders you added to the launcher. Nothing was \
                     changed: the launcher stopped before using or saving it."
                )
                .style(text::secondary),
                row![
                    style::link_badge(&path, "Show it in the file manager", Message::OpenFolder),
                    button("Edit config file manually")
                        .style(button::secondary)
                        .on_press(Message::Edit),
                    button("Read it again")
                        .style(button::secondary)
                        .on_press(Message::Retry),
                ]
                .spacing(8)
                .align_y(iced::Center)
                .wrap()
                .vertical_spacing(8),
                error,
            ]
            .push(retried)
            .push(loss)
            .push(
                text(
                    "Your clients, their addons and their saved settings are not affected \
                     either way.",
                )
                .style(text::secondary),
            )
            .push(
                row![
                    space::horizontal(),
                    button("Quit to fix it manually")
                        .style(button::text)
                        .on_press(Message::Quit),
                    button("Overwrite with new settings")
                        .style(button::danger)
                        .on_press(Message::Overwrite),
                ]
                .spacing(8),
            )
            .spacing(14),
        )
        .padding(24)
        .width(Fill.max(680))
        .style(style::card)
        .into()
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use iced_test::simulator;

    use super::*;

    /// A settings file with `text` in a temporary directory, and the screen
    /// for it.
    fn broken(text: &str) -> (tempfile::TempDir, State) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        fs::write(&path, text).unwrap();

        let error = Config::load(&path).unwrap_err();
        (directory, State::new(path, &error))
    }

    /// A client folder as an absolute path, which is what a broken file
    /// is searched for; Windows needs a drive for that.
    fn games(name: &str) -> String {
        if cfg!(windows) {
            format!("C:/games/{name}")
        } else {
            format!("/games/{name}")
        }
    }

    #[test]
    fn the_screen_says_why_and_what_overwriting_loses() {
        let (_directory, state) = broken(&format!(
            "installations = [\"{}\", \"{}\"\n",
            games("wotlk"),
            games("warmane")
        ));

        let mut ui = simulator(state.view());

        assert!(ui.find(format!("{} ↗", state.path.display())).is_ok());
        assert!(ui.find(format!("• {}", games("wotlk"))).is_ok());
        assert!(ui.find(format!("• {}", games("warmane"))).is_ok());
        assert!(state.error.contains("line 1"), "{}", state.error);
    }

    #[test]
    fn the_way_out_is_quitting_or_overwriting() {
        let (_directory, mut state) = broken("installations = 7");
        let mut errors = Errors::default();

        for (label, expected) in [
            ("Quit to fix it manually", "quit"),
            ("Overwrite with new settings", "overwrite"),
        ] {
            let mut ui = simulator(state.view());
            let _ = ui.click(label).unwrap();
            let messages: Vec<Message> = ui.into_messages().collect();

            for message in messages {
                let action = state.update(message, &mut errors);
                let got = match action.instruction {
                    Some(Instruction::Quit) => "quit",
                    Some(Instruction::Overwrite) => "overwrite",
                    Some(Instruction::Loaded(_)) => "loaded",
                    None => "nothing",
                };
                assert_eq!(got, expected, "{label}");
            }
        }
    }

    #[test]
    fn a_fixed_file_is_picked_up_when_read_again() {
        let (_directory, mut state) = broken("installations = 7");
        let mut errors = Errors::default();

        let action = state.update(Message::Retry, &mut errors);
        assert!(action.instruction.is_none());
        assert!(state.retried);

        fs::write(&state.path, "installations = [\"/games/wotlk\"]\n").unwrap();
        let action = state.update(Message::Retry, &mut errors);

        assert!(matches!(
            action.instruction,
            Some(Instruction::Loaded(config)) if config.installations == [PathBuf::from("/games/wotlk")]
        ));
    }
}
