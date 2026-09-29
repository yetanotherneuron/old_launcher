//! Starting the game: `Wow.exe` itself on Windows, and elsewhere whatever
//! command the client is set up with (Wine, Proton through umu-launcher,
//! Lutris, Steam, …), the way Steam runs a game's launch options.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::{Error, Installation};

/// What stands for the client's `Wow.exe` in a [`Launch`] command.
pub const PLACEHOLDER: &str = "%command%";

/// The command a client starts with when it has none: Wine running `Wow.exe`.
pub const DEFAULT_COMMAND: &str = "wine %command%";

/// How a client is started outside Windows: Steam's launch options, with the
/// environment variables in a field of their own.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    /// Environment variables, `NAME=value` separated by spaces and quoted as
    /// in a shell: `WINEPREFIX=~/.wine-wow WINEDEBUG=-all`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub env: String,
    /// A shell command line in which [`PLACEHOLDER`] stands for the client's
    /// `Wow.exe`, already quoted: `umu-run %command%`. [`DEFAULT_COMMAND`]
    /// when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
}

impl Launch {
    /// Whether nothing is set, so the client starts with [`DEFAULT_COMMAND`].
    pub fn is_default(&self) -> bool {
        self.env.is_empty() && self.command.is_empty()
    }

    /// Why the environment variables can't be used, if they can't.
    pub fn env_problem(&self) -> Option<String> {
        self.names().err()
    }

    /// The names of the environment variables, or why they can't be used.
    fn names(&self) -> Result<Vec<String>, String> {
        let words = shlex::split(&self.env).ok_or("A quote is never closed.")?;

        words
            .iter()
            .map(|word| {
                name(word)
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{word} is not a NAME=value pair."))
            })
            .collect()
    }

    /// Why the command can't be used, if it can't.
    pub fn command_problem(&self) -> Option<String> {
        shlex::split(&self.command)
            .is_none()
            .then(|| "A quote is never closed.".to_owned())
    }

    /// The shell command line that starts `executable`: the environment
    /// variables, exported for the whole command, then the command with
    /// [`PLACEHOLDER`] replaced by `executable`, quoted.
    ///
    /// Nothing else is quoted, so `~` and `$HOME` expand as in a terminal.
    pub fn line(&self, executable: &Path) -> Result<String, Error> {
        let names = self.names().map_err(|problem| Error::Launch {
            setting: "environment variables",
            problem,
        })?;
        if let Some(problem) = self.command_problem() {
            return Err(Error::Launch {
                setting: "command",
                problem,
            });
        }

        let quoted = executable
            .to_str()
            .and_then(|path| shlex::try_quote(path).ok())
            .ok_or_else(|| Error::Launch {
                setting: "client folder",
                problem: format!(
                    "{} can't be written in a command line.",
                    executable.display()
                ),
            })?;

        let command = match self.command.trim() {
            "" => DEFAULT_COMMAND,
            command => command,
        }
        .replace(PLACEHOLDER, &quoted);

        // Assigned, then exported: set before a command, they would only
        // reach its first part (`A=1 cp … && printenv A` prints nothing).
        Ok(if names.is_empty() {
            command
        } else {
            format!("{}; export {}; {command}", self.env.trim(), names.join(" "))
        })
    }
}

/// The variable `word` sets, if it is `NAME=value`: a name of letters,
/// digits and underscores that doesn't start with a digit.
fn name(word: &str) -> Option<&str> {
    let (name, _) = word.split_once('=')?;
    let mut chars = name.chars();

    let valid = chars
        .next()
        .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric());

    valid.then_some(name)
}

/// The process that starts `installation`'s client, from its folder:
/// `Wow.exe` on Windows, where `launch` is not used, and elsewhere
/// `launch`'s [`line`](Launch::line), run by `sh`.
pub fn command(installation: &Installation, launch: &Launch) -> Result<Command, Error> {
    let mut command = if cfg!(windows) {
        Command::new(installation.executable())
    } else {
        let mut shell = Command::new("sh");
        shell.arg("-c").arg(launch.line(installation.executable())?);
        shell
    };

    command.current_dir(installation.root());

    Ok(command)
}

/// Which of the clients at `roots` have their game running, going by the
/// processes named `Wow.exe`: on Windows the program itself, and under Wine
/// or Proton a process named after it, whose command line is its Windows
/// path and which works in its folder. A client's game is one started from
/// the client folder or working in it, or else started from a folder of the
/// same name (as a runner's own drive sees the client, say).
pub fn running(roots: &[PathBuf]) -> Vec<PathBuf> {
    // Every process is looked at, so none of their files stay open.
    sysinfo::set_open_files_limit(0);

    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().without_tasks(),
    );

    let games: Vec<Pid> = system
        .processes()
        .iter()
        .filter(|(_, process)| process.name().eq_ignore_ascii_case("Wow.exe"))
        .map(|(pid, _)| *pid)
        .collect();

    if games.is_empty() {
        return Vec::new();
    }

    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&games),
        true,
        ProcessRefreshKind::nothing()
            .without_tasks()
            .with_exe(UpdateKind::Always)
            .with_cwd(UpdateKind::Always)
            .with_cmd(UpdateKind::Always),
    );

    roots
        .iter()
        .filter(|root| {
            let name = root.file_name().map(|name| name.to_string_lossy());

            games
                .iter()
                .filter_map(|pid| system.process(*pid))
                .any(|game| {
                    game.exe()
                        .and_then(Path::parent)
                        .is_some_and(|folder| same(folder, root))
                        || game.cwd().is_some_and(|cwd| same(cwd, root))
                        || game.cmd().first().is_some_and(|program| {
                            folder(&program.to_string_lossy())
                                .zip(name.as_deref())
                                .is_some_and(|(folder, name)| folder.eq_ignore_ascii_case(name))
                        })
                })
        })
        .cloned()
        .collect()
}

/// Whether `a` and `b` are the same folder however they are spelled: with
/// Windows' `\\?\` prefix or without, either slash, any case.
fn same(a: &Path, b: &Path) -> bool {
    let spelled = |path: &Path| {
        path.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_owned()
    };

    spelled(a).eq_ignore_ascii_case(&spelled(b))
}

/// The name of the folder `Wow.exe` was started from, out of a command line
/// such as `Z:\games\WoW\Wow.exe` or `"C:\Program Files\WoW\Wow.exe" -opengl`.
fn folder(cmdline: &str) -> Option<&str> {
    let program = cmdline.split('\0').next()?;
    let end = program.to_ascii_lowercase().find("wow.exe")?;

    program[..end]
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .map(|folder| folder.trim_start_matches('"'))
        .filter(|folder| !folder.is_empty() && !folder.ends_with(':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(env: &str, command: &str) -> Launch {
        Launch {
            env: env.into(),
            command: command.into(),
        }
    }

    #[test]
    fn without_a_command_wine_runs_the_client() {
        let line = Launch::default()
            .line(Path::new("/games/wow/Wow.exe"))
            .unwrap();

        assert_eq!(line, "wine /games/wow/Wow.exe");
    }

    #[test]
    fn the_executable_is_quoted_and_the_rest_left_to_the_shell() {
        let line = launch(
            "WINEPREFIX=~/.wine-wow  PROTONPATH='GE Proton'",
            "  gamemoderun umu-run %command% -console ",
        )
        .line(Path::new("/games/World of Warcraft 3.3.5a/Wow.exe"))
        .unwrap();

        assert_eq!(
            line,
            "WINEPREFIX=~/.wine-wow  PROTONPATH='GE Proton'; export WINEPREFIX PROTONPATH; \
             gamemoderun umu-run '/games/World of Warcraft 3.3.5a/Wow.exe' -console"
        );
    }

    #[test]
    fn a_command_without_the_placeholder_runs_as_written() {
        let line = launch("", "lutris lutris:rungameid/3")
            .line(Path::new("/games/wow/Wow.exe"))
            .unwrap();

        assert_eq!(line, "lutris lutris:rungameid/3");
    }

    #[test]
    fn environment_variables_must_be_name_value_pairs() {
        assert_eq!(launch("A=1 _B2='x y' C=", "").env_problem(), None);
        assert_eq!(
            launch("WINEPREFIX=~/wow umu-run", "")
                .env_problem()
                .as_deref(),
            Some("umu-run is not a NAME=value pair.")
        );
        assert_eq!(
            launch("2A=1", "").env_problem().as_deref(),
            Some("2A=1 is not a NAME=value pair.")
        );
        assert_eq!(
            launch("A='open", "").env_problem().as_deref(),
            Some("A quote is never closed.")
        );
    }

    #[test]
    fn settings_with_a_problem_start_nothing() {
        let unclosed = launch("", "wine \"%command%");

        assert_eq!(
            unclosed.command_problem().as_deref(),
            Some("A quote is never closed.")
        );
        assert!(matches!(
            unclosed.line(Path::new("/games/wow/Wow.exe")),
            Err(Error::Launch {
                setting: "command",
                ..
            })
        ));
    }

    #[test]
    fn the_folder_a_game_was_started_from_is_on_its_command_line() {
        assert_eq!(folder("Z:\\mnt\\games\\WoW\\Wow.exe"), Some("WoW"));
        assert_eq!(
            folder("\"C:\\Program Files\\World of Warcraft\\WoW.EXE\" -opengl"),
            Some("World of Warcraft")
        );
        assert_eq!(folder("/mnt/games/WoW/Wow.exe\0-console\0"), Some("WoW"));
        assert_eq!(folder("Wow.exe"), None);
        assert_eq!(folder("Z:\\Wow.exe"), None);
        assert_eq!(folder("C:\\windows\\system32\\services.exe"), None);
    }

    #[test]
    fn a_folder_is_the_same_however_it_is_spelled() {
        assert!(same(
            Path::new(r"\\?\C:\Games\WoW"),
            Path::new(r"c:\games\wow\")
        ));
        assert!(same(Path::new("/games/WoW/"), Path::new("/games/WoW")));
        assert!(!same(Path::new("/games/WoW"), Path::new("/games/WoW2")));
    }

    /// Any program named `Wow.exe` looks like the game: here, `sleep`.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_game_is_running_while_a_wow_exe_works_in_the_client_folder() {
        use std::fs;

        let directory = tempfile::tempdir().unwrap();
        // The temporary folder's random name, so no real game matches it.
        let root = fs::canonicalize(directory.path()).unwrap();
        let other = root.join("other");
        fs::create_dir(&other).unwrap();
        fs::copy("/bin/sleep", root.join("Wow.exe")).unwrap();
        let clients = [root.clone(), other];

        assert!(running(&clients).is_empty());

        // Another test forking while the copy was open for writing makes
        // it "busy" for a moment.
        let mut game = loop {
            match Command::new(root.join("Wow.exe"))
                .arg("30")
                .current_dir(&root)
                .spawn()
            {
                Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                spawned => break spawned.unwrap(),
            }
        };

        assert_eq!(running(&clients), std::slice::from_ref(&root));

        game.kill().unwrap();
        game.wait().unwrap();
        assert!(running(&clients).is_empty());
    }

    /// Runs the real thing: `sh`, in the client folder, with the variables
    /// set and the quoted executable in place of the placeholder.
    #[cfg(unix)]
    #[test]
    fn the_command_runs_in_the_client_folder() {
        use crate::version::{Version, fake_executable};

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("World of Warcraft 3.3.5a");
        std::fs::create_dir(&root).unwrap();
        let wotlk = Version {
            major: 3,
            minor: 3,
            patch: 5,
            build: 12340,
        };
        std::fs::write(root.join("Wow.exe"), fake_executable(wotlk)).unwrap();
        let installation = Installation::open(&root).unwrap();

        let status = command(
            &installation,
            &launch(
                "GREETING='hello there' PREFIX=~/.wine-wow",
                "cp %command% copy.exe && printenv GREETING PREFIX > env.txt",
            ),
        )
        .unwrap()
        .env("HOME", "/home/player")
        .status()
        .unwrap();

        assert!(status.success());
        assert_eq!(
            std::fs::read(root.join("copy.exe")).unwrap(),
            fake_executable(wotlk)
        );
        assert_eq!(
            std::fs::read_to_string(root.join("env.txt")).unwrap(),
            "hello there\n/home/player/.wine-wow\n",
            "set for the whole command, with ~ expanded"
        );
    }
}
