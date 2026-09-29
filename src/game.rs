//! Starting the game, and following it: being set up, running, over.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use iced::{Function, Subscription, Task};

use crate::core::{Installation, Launch, launch};
use crate::errors::Errors;
use crate::{blocking, describe};

/// The games started from the launcher or found running, by client folder,
/// until over.
#[derive(Default)]
pub struct State {
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
    pub const PATIENCE: u8 = 60;
}

/// Where a game started from the launcher is at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Its `Wow.exe` hasn't shown up yet.
    SettingUp,
    /// Its `Wow.exe` is running.
    Running,
}

/// What happens to the games.
#[derive(Debug, Clone)]
pub enum Message {
    /// The command that started the game of this client folder exited, with
    /// this exit code if it had one, or failed to start.
    Exited(PathBuf, Result<Option<i32>, String>),
    /// Time to look for the games started, and the selected client's.
    Watch,
    /// The client folders whose game is running.
    Watched(Result<Vec<PathBuf>, String>),
}

impl State {
    /// Where the game of the client at `root` is at, if there is one.
    pub fn phase(&self, root: &Path) -> Option<Phase> {
        self.games.get(root).map(|game| {
            if game.started {
                Phase::Running
            } else {
                Phase::SettingUp
            }
        })
    }

    /// Starts `installation`'s game as `launch` says, unless it is running
    /// already. What fails goes to `errors`.
    pub fn play(
        &mut self,
        installation: &Installation,
        launch: &Launch,
        errors: &mut Errors,
    ) -> Task<Message> {
        let root = installation.root().to_path_buf();

        if self.games.contains_key(&root) {
            return Task::none();
        }

        match launch::command(installation, launch) {
            Ok(command) => {
                log::info!("Starting the game: {command:?}");

                let _ = self.games.insert(
                    root.clone(),
                    Game {
                        // On Windows the command is Wow.exe itself: nothing
                        // to set up, and the game ends when it exits.
                        started: cfg!(windows),
                        command: true,
                        patience: Game::PATIENCE,
                    },
                );

                Task::perform(play(command), Message::Exited.with(root))
            }
            Err(error) => {
                errors.report(format!("Could not start World of Warcraft: {error}"));
                Task::none()
            }
        }
    }

    /// Looks for the games started, and for the `selected` client's, which
    /// may have been started elsewhere or before the launcher was.
    pub fn watch(&self, selected: Option<&Path>) -> Task<Message> {
        let mut roots: Vec<PathBuf> = self.games.keys().cloned().collect();
        roots.extend(
            selected
                .filter(|root| !self.games.contains_key(*root))
                .map(Path::to_path_buf),
        );

        if roots.is_empty() {
            return Task::none();
        }
        log::trace!("Looking for the games of {roots:?}");

        Task::perform(
            blocking(move || Ok(launch::running(&roots))),
            Message::Watched,
        )
    }

    /// Handles what happened to the games; `selected` is the client shown.
    pub fn update(
        &mut self,
        message: Message,
        selected: Option<&Path>,
        errors: &mut Errors,
    ) -> Task<Message> {
        match message {
            Message::Exited(root, result) => {
                log::info!("The game's command exited: {result:?}");

                // Elsewhere than on Windows, a fine exit may leave the game
                // running, or about to (see `Game::patience`): the watching
                // tells.
                if cfg!(windows) || result != Ok(Some(0)) {
                    let _ = self.games.remove(&root);
                } else if let Some(game) = self.games.get_mut(&root) {
                    game.command = false;
                }

                match result {
                    Ok(None | Some(0)) => {}
                    // What `sh` says when it can't find or run the command.
                    Ok(Some(127)) if !cfg!(windows) => errors.report(
                        "Could not start World of Warcraft: the command to start it was not \
                         found. Check it under Manage clients.",
                    ),
                    Ok(Some(126)) if !cfg!(windows) => errors.report(
                        "Could not start World of Warcraft: the command to start it can't be \
                         run. Check it under Manage clients.",
                    ),
                    Ok(Some(code)) => {
                        errors.report(format!("World of Warcraft exited with status {code}."));
                    }
                    Err(error) => {
                        errors.report(format!("Could not start World of Warcraft: {error}"));
                    }
                }

                Task::none()
            }
            Message::Watch => self.watch(selected),
            Message::Watched(Ok(running)) => {
                for root in &running {
                    let _ = self.games.entry(root.clone()).or_insert_with(|| {
                        log::info!("Found the game of {} running", root.display());

                        Game {
                            started: true,
                            command: false,
                            patience: 0,
                        }
                    });
                }

                self.games.retain(|root, game| {
                    if running.contains(root) {
                        if !game.started {
                            log::info!("The game of {} is running", root.display());
                        }
                        game.started = true;
                        return true;
                    }

                    // Gone once seen: the game is over, even if what started
                    // it lingers (Lutris' window, say).
                    if game.started {
                        log::info!("The game of {} is over", root.display());
                        return false;
                    }
                    if game.command {
                        return true;
                    }

                    game.patience = game.patience.saturating_sub(1);
                    if game.patience == 0 {
                        log::info!(
                            "The game of {} never showed up after its command exited",
                            root.display()
                        );
                    }
                    game.patience > 0
                });

                Task::none()
            }
            // The next look will do.
            Message::Watched(Err(error)) => {
                log::debug!("Could not list the processes: {error}");
                Task::none()
            }
        }
    }

    /// A look for the games every second, while there are any.
    pub fn subscription(&self) -> Subscription<Message> {
        if self.games.is_empty() {
            Subscription::none()
        } else {
            iced::time::every(Duration::from_secs(1)).map(|_| Message::Watch)
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing;

    /// A game started for the client at `root`, on Linux: its command is
    /// still running and its Wow.exe not seen yet. The command itself is
    /// not run: the task is dropped.
    fn started(state: &mut State, installation: &Installation) -> PathBuf {
        let mut errors = Errors::default();
        let _ = state.play(installation, &Launch::default(), &mut errors);
        assert!(errors.messages().is_empty(), "{:?}", errors.messages());

        installation.root().to_path_buf()
    }

    #[cfg(not(windows))]
    #[test]
    fn a_game_is_set_up_then_running_until_its_wow_exe_is_gone() {
        let (_root, installation) = testing::client(&[]);
        let mut state = State::default();
        let mut errors = Errors::default();
        let root = started(&mut state, &installation);

        let _ = state.update(Message::Watched(Ok(Vec::new())), None, &mut errors);
        assert_eq!(
            state.phase(&root),
            Some(Phase::SettingUp),
            "no Wow.exe yet: Proton being set up, say"
        );

        let _ = state.update(Message::Watched(Ok(vec![root.clone()])), None, &mut errors);
        assert_eq!(state.phase(&root), Some(Phase::Running));

        // wine or umu-run exit with the game, which then goes.
        let _ = state.update(
            Message::Exited(root.clone(), Ok(Some(0))),
            None,
            &mut errors,
        );
        assert_eq!(state.phase(&root), Some(Phase::Running), "until it is gone");
        let _ = state.update(Message::Watched(Ok(Vec::new())), None, &mut errors);
        assert_eq!(state.phase(&root), None);
        assert!(errors.messages().is_empty(), "a normal exit");

        let _ = started(&mut state, &installation);
        let _ = state.update(
            Message::Exited(root.clone(), Ok(Some(3))),
            None,
            &mut errors,
        );
        assert_eq!(state.phase(&root), None, "over at once");
        assert_eq!(
            errors.messages(),
            ["World of Warcraft exited with status 3."]
        );
    }

    #[cfg(windows)]
    #[test]
    fn on_windows_the_game_is_the_command_itself() {
        let (_root, installation) = testing::client(&[]);
        let mut state = State::default();
        let mut errors = Errors::default();
        let root = started(&mut state, &installation);

        assert_eq!(
            state.phase(&root),
            Some(Phase::Running),
            "nothing to set up"
        );

        let _ = state.update(
            Message::Exited(root.clone(), Ok(Some(0))),
            None,
            &mut errors,
        );
        assert_eq!(state.phase(&root), None, "over when Wow.exe exits");
    }

    #[cfg(not(windows))]
    #[test]
    fn a_game_handed_to_another_launcher_is_waited_for() {
        let (_root, installation) = testing::client(&[]);
        let mut state = State::default();
        let mut errors = Errors::default();
        let root = started(&mut state, &installation);

        // `steam steam://rungameid/…` exits as soon as Steam has the game.
        let _ = state.update(
            Message::Exited(root.clone(), Ok(Some(0))),
            None,
            &mut errors,
        );
        for _ in 1..Game::PATIENCE {
            let _ = state.update(Message::Watched(Ok(Vec::new())), None, &mut errors);
        }
        assert_eq!(state.phase(&root), Some(Phase::SettingUp));
        let _ = state.update(Message::Watched(Ok(vec![root.clone()])), None, &mut errors);
        assert_eq!(state.phase(&root), Some(Phase::Running));
        let _ = state.update(Message::Watched(Ok(Vec::new())), None, &mut errors);
        assert_eq!(state.phase(&root), None);

        // And when the game never shows up, the launcher stops waiting.
        let _ = started(&mut state, &installation);
        let _ = state.update(
            Message::Exited(root.clone(), Ok(Some(0))),
            None,
            &mut errors,
        );
        for _ in 0..Game::PATIENCE {
            let _ = state.update(Message::Watched(Ok(Vec::new())), None, &mut errors);
        }
        assert_eq!(state.phase(&root), None);
    }

    #[test]
    fn a_game_already_running_is_found_and_followed() {
        let (_root, installation) = testing::client(&[]);
        let mut state = State::default();
        let mut errors = Errors::default();
        let root = installation.root().to_path_buf();

        // Nothing was started, but the selected client is looked at.
        assert!(state.watch(Some(&root)).units() > 0);
        assert_eq!(state.watch(None).units(), 0, "nothing to look for");

        let _ = state.update(Message::Watched(Ok(vec![root.clone()])), None, &mut errors);
        assert_eq!(state.phase(&root), Some(Phase::Running));
        assert!(state.subscription().units() > 0, "followed every second");

        let _ = state.update(Message::Watched(Ok(Vec::new())), None, &mut errors);
        assert_eq!(state.phase(&root), None);
        assert_eq!(state.subscription().units(), 0);
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
}
