//! Starting the game: `Wow.exe` itself on Windows, and elsewhere whatever
//! command the client is set up with (Wine, Proton through umu-launcher,
//! …), the way Steam runs a game's launch options.

use std::ffi::OsStr;
use std::fs;
use std::io;
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
#[cfg(windows)]
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::error::io_at;
use crate::{Error, Installation};

/// What stands for the program the launcher runs in a [`Launch`] command:
/// `Wow.exe`, or a mod's setup program.
pub const PLACEHOLDER: &str = "%command%";

/// The command running `Wow.exe` with the system's Wine.
const WINE_COMMAND: &str = "wine %command%";

/// The command running `Wow.exe` with Proton, through umu-launcher.
const UMU_COMMAND: &str = "umu-run %command%";

/// GameMode's wrapper, which asks the system for its optimizations while
/// the program it runs is running: around the runner, when installed.
const GAMEMODE: &str = "gamemoderun";

/// The Wine prefix builds up to 1.2.0 made for a client, in its folder
/// beside `Wow.exe`, which [`move_old_prefix`] moves to [`prefix`].
const OLD_PREFIX: &str = ".prefix";

/// The Wine prefix the client at `root` runs in unless its environment
/// variables set `WINEPREFIX`: a folder of its own in the user's data
/// folder, named after the client folder and told apart from others of the
/// same name by an id made of its path. Not in the client folder: `Wow.exe`
/// goes through every file in its folder as it starts and as it quits, and
/// a prefix holds thousands.
pub fn prefix(root: &Path) -> PathBuf {
    use sha1::{Digest, Sha1};

    let name = root.file_name().unwrap_or_default().to_string_lossy();
    let digest = Sha1::digest(root.as_os_str().as_encoded_bytes());
    let id: String = digest[..4]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    dirs::data_dir()
        .unwrap_or_default()
        .join("old_launcher")
        .join("prefixes")
        .join(format!("{name}-{id}"))
}

/// Moves the prefix builds up to 1.2.0 made in the folder of the client at
/// `root` to where its prefix is now ([`prefix`]), when the client runs in
/// the launcher's: when `launch` doesn't set `WINEPREFIX`. A prefix there
/// already is kept, and the old one left. Within a disk it is renamed;
/// across disks it is copied, which takes a while for hundreds of
/// megabytes, then deleted.
pub fn move_old_prefix(root: &Path, launch: &Launch) -> Result<(), Error> {
    let old = root.join(OLD_PREFIX);
    let its_own = launch
        .names()
        .is_ok_and(|names| names.iter().any(|name| name == PREFIX_VARIABLE));

    if cfg!(windows) || its_own || fs::symlink_metadata(&old).is_err() {
        return Ok(());
    }

    moved(&old, &prefix(root))
}

/// Moves the folder `old` to `new`, unless something is there already.
fn moved(old: &Path, new: &Path) -> Result<(), Error> {
    if fs::symlink_metadata(new).is_ok() {
        log::warn!(
            "Left the prefix {}: {} is there already",
            old.display(),
            new.display()
        );
        return Ok(());
    }
    if let Some(parent) = new.parent() {
        fs::create_dir_all(parent).map_err(io_at(parent))?;
    }
    log::info!("Moving the prefix {} to {}", old.display(), new.display());

    match fs::rename(old, new) {
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {}
        renamed => return renamed.map_err(io_at(old)),
    }

    // Copied beside it and renamed once whole, so it is never found half
    // copied: what a copy cut short left is copied again.
    let mut beside = new.as_os_str().to_owned();
    beside.push(".partial");
    let beside = PathBuf::from(beside);
    if fs::symlink_metadata(&beside).is_ok() {
        fs::remove_dir_all(&beside).map_err(io_at(&beside))?;
    }

    copied(old, &beside)?;
    fs::rename(&beside, new).map_err(io_at(&beside))?;
    fs::remove_dir_all(old).map_err(io_at(old))
}

/// Copies what is at `from` to `to`: a folder with all it holds, a file, or
/// a link as a link to the same place. Wine links a prefix's drives to
/// where they are; Proton, many of its files to its own.
fn copied(from: &Path, to: &Path) -> Result<(), Error> {
    let kind = fs::symlink_metadata(from).map_err(io_at(from))?.file_type();

    if kind.is_symlink() {
        let target = fs::read_link(from).map_err(io_at(from))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, to).map_err(io_at(to))?;
        #[cfg(not(unix))]
        return Err(io_at(to)(io::Error::other(format!(
            "links to {} can't be made here",
            target.display()
        ))));
    } else if kind.is_dir() {
        fs::create_dir(to).map_err(io_at(to))?;
        for entry in fs::read_dir(from).map_err(io_at(from))? {
            let entry = entry.map_err(io_at(from))?;
            copied(&entry.path(), &to.join(entry.file_name()))?;
        }
    } else {
        let _ = fs::copy(from, to).map_err(io_at(from))?;
    }

    Ok(())
}

/// What the launcher sets for a client outside Windows, which the client's
/// settings may set otherwise: the environment variables, set before the
/// client's own, and the command, when it has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Defaults {
    /// `WINEPREFIX`: the client's [`prefix`].
    pub prefix: PathBuf,
    /// The DLLs mods put beside `Wow.exe`, by name without `.dll`, which
    /// `WINEDLLOVERRIDES` tells Wine to load from there before its own:
    /// without that, Wine loads its own `version.dll`, and a mod in its place
    /// never runs.
    pub natives: Vec<String>,
    /// The command (see [`default_command`]).
    pub command: &'static str,
}

impl Defaults {
    /// Those of the client at `root`, on this system, with no DLLs of mods.
    pub fn of(root: &Path) -> Self {
        Self {
            prefix: prefix(root),
            natives: Vec::new(),
            command: default_command(),
        }
    }

    /// The environment variables they set, as written among a client's
    /// (`NAME=value`, quoted), in the order they are set: `WINEPREFIX`, and
    /// `WINEDLLOVERRIDES` when mods put DLLs beside `Wow.exe`. An error when
    /// the prefix can't be written in a command line.
    pub fn env(&self) -> Result<Vec<String>, Error> {
        let mut env = vec![format!("{PREFIX_VARIABLE}={}", quoted(&self.prefix)?)];
        if !self.natives.is_empty() {
            let natives: Vec<String> = self
                .natives
                .iter()
                .map(|dll| format!("{dll}=n,b"))
                .collect();
            env.push(assignment(&natives.join(";")));
        }

        Ok(env)
    }
}

/// The command a client starts with when it has none, on this system:
/// [`UMU_COMMAND`] where umu-launcher is installed (`umu-run` is on the
/// `PATH`), or else [`WINE_COMMAND`], under [`GAMEMODE`] where that is
/// installed too. Looked for once.
pub fn default_command() -> &'static str {
    static DEFAULT: OnceLock<String> = OnceLock::new();

    DEFAULT.get_or_init(|| {
        let path = std::env::var_os("PATH").unwrap_or_default();

        composed(installed("umu-run", &path), installed(GAMEMODE, &path))
    })
}

/// The default command on a system with umu-launcher (`umu`) and GameMode
/// (`gamemode`) installed, or not.
fn composed(umu: bool, gamemode: bool) -> String {
    let runner = if umu { UMU_COMMAND } else { WINE_COMMAND };

    if gamemode {
        format!("{GAMEMODE} {runner}")
    } else {
        runner.to_owned()
    }
}

/// Whether `program` is in one of the folders of `path` (as the `PATH`
/// lists them), as a file that may run.
fn installed(program: &str, path: &OsStr) -> bool {
    std::env::split_paths(path).any(|folder| {
        std::fs::metadata(folder.join(program)).is_ok_and(|metadata| {
            #[cfg(unix)]
            let runs = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let runs = true;

            metadata.is_file() && runs
        })
    })
}

/// How a client is started, and what runs beside it: outside Windows,
/// Steam's launch options, with the environment variables in a field of
/// their own; everywhere, a program started with the game and stopped when
/// it exits.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    /// Environment variables, `NAME=value` separated by spaces and quoted as
    /// in a shell: `WINEPREFIX=~/.wine-wow WINEDEBUG=-all`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub env: String,
    /// A shell command line in which [`PLACEHOLDER`] stands for the program
    /// the launcher runs, already quoted: `umu-run %command%`. It must have
    /// one: the launcher runs mods' setups through it too, with the game's
    /// runner. [`default_command`] when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub command: String,
    /// A shell command line (`cmd`'s on Windows) started with the game and
    /// stopped, with whatever it started, when the game exits: a program
    /// the game works with. Nothing when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub companion: String,
}

impl Launch {
    /// Whether nothing is set, so the client starts as the launcher's
    /// [`Defaults`] say, with nothing beside it.
    pub fn is_default(&self) -> bool {
        self.env.is_empty() && self.command.is_empty() && self.companion.is_empty()
    }

    /// The command line started with the game, if one is set.
    pub fn companion(&self) -> Option<&str> {
        Some(self.companion.trim()).filter(|line| !line.is_empty())
    }

    /// Why the command line started with the game can't be used, if it
    /// can't: a quote never closed. `cmd` closes one itself on Windows.
    pub fn companion_problem(&self) -> Option<String> {
        (!cfg!(windows) && shlex::split(&self.companion).is_none())
            .then(|| "A quote is never closed.".to_owned())
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

    /// Why the command can't be used, if it can't: a quote never closed, or
    /// no [`PLACEHOLDER`].
    pub fn command_problem(&self) -> Option<String> {
        if shlex::split(&self.command).is_none() {
            return Some("A quote is never closed.".to_owned());
        }

        let command = self.command.trim();
        (!command.is_empty() && !command.contains(PLACEHOLDER))
            .then(|| format!("{PLACEHOLDER} is missing: the launcher puts what it runs there."))
    }

    /// The shell command line that starts `executable`: the environment
    /// variables, exported for the whole command, then the command with
    /// [`PLACEHOLDER`] replaced by `executable`, quoted. `defaults`' variables
    /// come first, so the client's own, set after them, win; and its command
    /// is the client's when it has none.
    ///
    /// Nothing else is quoted, so `~` and `$HOME` expand as in a terminal.
    pub fn line(&self, defaults: &Defaults, executable: &Path) -> Result<String, Error> {
        self.line_running(defaults, &quoted(executable)?, &[], true)
    }

    /// The shell command line that runs another Windows program of the
    /// client, `program` (a path relative to the client folder, which the
    /// line runs in) with `arguments`, the way [`Launch::line`] starts the
    /// game: in place of [`PLACEHOLDER`], all of them quoted, with the
    /// game's runner and prefix. Plain `wine` is no stand-in for it: a Wine
    /// other than the game's updates the prefix to its own version.
    ///
    /// Unlike the game, the program goes as a Windows path, relative, with
    /// backslashes: given a Unix path, Proton hands a program to a Windows
    /// wrapper of its (`start /unix`, `umu.exe`) that opens a console
    /// program in a console window of its own and returns at once, so the
    /// launcher saw nothing of a mod's setup and waited for nothing. Given a
    /// Windows path, it runs the program itself, whose output then reaches
    /// the launcher. GameMode is left out too: it is for the game.
    pub fn tool_line(
        &self,
        defaults: &Defaults,
        program: &str,
        arguments: &[String],
    ) -> Result<String, Error> {
        let program = program.replace('/', "\\");
        let program = shlex::try_quote(&program).map_err(|error| Error::Launch {
            setting: "command",
            problem: error.to_string(),
        })?;

        self.line_running(defaults, &program, arguments, false)
    }

    /// The shell command line of the command with `program` (quoted already)
    /// and `arguments`, quoted, in place of [`PLACEHOLDER`], after the
    /// environment variables, which `defaults`' come before; `gamemode` says
    /// whether the command keeps the [`GAMEMODE`] it may start with.
    fn line_running(
        &self,
        defaults: &Defaults,
        program: &str,
        arguments: &[String],
        gamemode: bool,
    ) -> Result<String, Error> {
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

        let added = defaults.env()?;
        let words = std::iter::once(Ok(program.into()))
            .chain(arguments.iter().map(|argument| shlex::try_quote(argument)))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| Error::Launch {
                setting: "command",
                problem: error.to_string(),
            })?
            .join(" ");

        let command = match self.command.trim() {
            "" => defaults.command,
            command => command,
        };
        let command = if gamemode {
            command
        } else {
            without_gamemode(command)
        }
        .replace(PLACEHOLDER, &words);

        let mut exported: Vec<String> = Vec::new();
        for name in added
            .iter()
            .filter_map(|word| name(word))
            .map(str::to_owned)
            .chain(names)
        {
            if !exported.contains(&name) {
                exported.push(name);
            }
        }
        // The launcher's first: the shell keeps the last value set.
        let env = added
            .into_iter()
            .chain(Some(self.env.trim().to_owned()).filter(|env| !env.is_empty()))
            .collect::<Vec<_>>()
            .join(" ");

        // Assigned, then exported: set before a command, they would only
        // reach its first part (`A=1 cp … && printenv A` prints nothing).
        Ok(format!("{env}; export {}; {command}", exported.join(" ")))
    }
}

/// `command` without the [`GAMEMODE`] it starts with, if it does: the rest
/// of the line, which runs the program on its own.
fn without_gamemode(command: &str) -> &str {
    command
        .strip_prefix(GAMEMODE)
        .filter(|rest| rest.starts_with(char::is_whitespace))
        .map(str::trim_start)
        .unwrap_or(command)
}

/// The variable that tells Wine which DLLs to load from a program's folder
/// before its own.
const OVERRIDES: &str = "WINEDLLOVERRIDES";

/// The variable that tells Wine, and umu-launcher, which prefix to run in.
const PREFIX_VARIABLE: &str = "WINEPREFIX";

/// `path`, quoted for a command line, if it can be written in one.
fn quoted(path: &Path) -> Result<std::borrow::Cow<'_, str>, Error> {
    path.to_str()
        .and_then(|text| shlex::try_quote(text).ok())
        .ok_or_else(|| Error::Launch {
            setting: "client folder",
            problem: format!("{} can't be written in a command line.", path.display()),
        })
}

/// `WINEDLLOVERRIDES` set to `value`, quoted as the shell needs (its `;`
/// would end the assignment).
fn assignment(value: &str) -> String {
    let quoted = shlex::try_quote(value).map_or_else(|_| format!("'{value}'"), Into::into);

    format!("{OVERRIDES}={quoted}")
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
/// `launch`'s [`line`](Launch::line), run by `sh`, with the client's
/// [`Defaults`] and Wine told to load the `natives` (DLLs by name without
/// `.dll`, which mods put beside `Wow.exe`) from there before its own.
pub fn command(
    installation: &Installation,
    launch: &Launch,
    natives: &[String],
) -> Result<Command, Error> {
    let mut command = if cfg!(windows) {
        Command::new(installation.executable())
    } else {
        let defaults = Defaults {
            natives: natives.to_vec(),
            ..Defaults::of(installation.root())
        };
        let mut shell = Command::new("sh");
        shell
            .arg("-c")
            .arg(launch.line(&defaults, installation.executable())?);
        shell
    };

    command.current_dir(installation.root());

    Ok(command)
}

/// The process that runs another Windows program of `installation`'s client,
/// `program` (relative to the client folder, in which it runs) with
/// `arguments`: itself on Windows, and elsewhere the way the game starts
/// (see [`Launch::tool_line`]), run by `sh`.
pub fn tool(
    installation: &Installation,
    launch: &Launch,
    program: &str,
    arguments: &[String],
) -> Result<Command, Error> {
    let mut command = if cfg!(windows) {
        let mut tool = Command::new(installation.root().join(program));
        let _ = tool.args(arguments);
        tool
    } else {
        let mut shell = Command::new("sh");
        let _ = shell.arg("-c").arg(launch.tool_line(
            &Defaults::of(installation.root()),
            program,
            arguments,
        )?);
        shell
    };

    let _ = command.current_dir(installation.root());

    Ok(command)
}

/// Which of the clients at `roots` have their game running, going by the
/// processes named `Wow.exe`: on Windows the program itself, and under Wine
/// or Proton a process named after it, whose command line is its Windows
/// path and which works in its folder. A client's game is one started from
/// the client folder or working in it, or else started from a folder of the
/// same name (as a runner's own drive sees the client, say).
pub fn running(roots: &[PathBuf]) -> Vec<PathBuf> {
    #[cfg(windows)]
    let games = games(roots);
    #[cfg(not(windows))]
    let games = games();

    roots
        .iter()
        .filter(|root| {
            let name = root.file_name().map(|name| name.to_string_lossy());

            games.iter().any(|game| {
                game.exe
                    .as_deref()
                    .and_then(Path::parent)
                    .is_some_and(|folder| same(folder, root))
                    || game.cwd.as_deref().is_some_and(|cwd| same(cwd, root))
                    || game
                        .program
                        .as_deref()
                        .and_then(folder)
                        .zip(name.as_deref())
                        .is_some_and(|(folder, name)| folder.eq_ignore_ascii_case(name))
            })
        })
        .cloned()
        .collect()
}

/// A running `Wow.exe`, as much of it as tells whose game it is.
#[derive(Debug, Default)]
struct Game {
    /// Its program file.
    exe: Option<PathBuf>,
    /// The folder it works in.
    cwd: Option<PathBuf>,
    /// The program its command line starts with: under Wine or Proton, the
    /// Windows path of `Wow.exe`.
    program: Option<String>,
}

/// The processes named `Wow.exe`: under Wine or Proton, the game's.
#[cfg(not(windows))]
fn games() -> Vec<Game> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

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

    games
        .iter()
        .filter_map(|pid| system.process(*pid))
        .map(|game| Game {
            exe: game.exe().map(Path::to_path_buf),
            cwd: game.cwd().map(Path::to_path_buf),
            program: game
                .cmd()
                .first()
                .map(|program| program.to_string_lossy().into_owned()),
        })
        .collect()
}

/// The `Wow.exe` processes found for the clients asked about, kept open so
/// that later looks only ask each whether it exited.
#[cfg(windows)]
static FOUND: Mutex<Vec<Found>> = Mutex::new(Vec::new());

/// A `Wow.exe` found running, kept open with the right to tell when it
/// exits, and no other but asking its path.
#[cfg(windows)]
struct Found {
    /// Its process id, which no other process takes while it is open.
    id: u32,
    /// Its program file.
    exe: PathBuf,
    process: OwnedHandle,
}

/// The `Wow.exe` processes of the clients at `roots`, by their program file
/// alone. Those found before are only asked whether they exited. The
/// processes are looked at again only when one of `roots` has no game
/// found, and then as little as Windows lets: a snapshot of their names,
/// which opens none of them, then each `Wow.exe`'s path. Security software
/// rightly suspects a program that opens other processes to read their
/// memory, as a general listing of processes does.
#[cfg(windows)]
fn games(roots: &[PathBuf]) -> Vec<Game> {
    let mut found = FOUND.lock().unwrap_or_else(PoisonError::into_inner);
    found.retain(|game| alive(&game.process));

    let of =
        |game: &Found, root: &PathBuf| game.exe.parent().is_some_and(|folder| same(folder, root));
    let unknown = roots
        .iter()
        .any(|root| !found.iter().any(|game| of(game, root)));

    if unknown {
        for id in named_wow() {
            if found.iter().any(|game| game.id == id) {
                continue;
            }
            let game = open(id).filter(|game| roots.iter().any(|root| of(game, root)));
            found.extend(game);
        }
    }

    found
        .iter()
        .map(|game| Game {
            exe: Some(game.exe.clone()),
            ..Game::default()
        })
        .collect()
}

/// Whether the process held is still running: asked without waiting.
#[cfg(windows)]
fn alive(process: &OwnedHandle) -> bool {
    use windows_sys::Win32::Foundation::WAIT_TIMEOUT;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;

    // SAFETY: the handle is open, with the right to wait for it.
    unsafe { WaitForSingleObject(process.as_raw_handle(), 0) == WAIT_TIMEOUT }
}

/// The ids of the processes named `Wow.exe`, from a snapshot of the names of
/// the processes, which opens none of them.
#[cfg(windows)]
fn named_wow() -> Vec<u32> {
    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    // SAFETY: a plain call; the handle is checked, then owned.
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    // SAFETY: a handle just opened, which nothing else owns.
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot) };

    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..PROCESSENTRY32W::default()
    };
    let mut ids = Vec::new();
    // SAFETY: `entry` is a PROCESSENTRY32W whose size field is set, as both
    // calls require, and the snapshot is open.
    let mut listed = unsafe { Process32FirstW(snapshot.as_raw_handle(), &mut entry) } != 0;

    while listed {
        let name = &entry.szExeFile;
        let length = name
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(name.len());

        if String::from_utf16_lossy(&name[..length]).eq_ignore_ascii_case("Wow.exe") {
            ids.push(entry.th32ProcessID);
        }
        // SAFETY: as for the first.
        listed = unsafe { Process32NextW(snapshot.as_raw_handle(), &mut entry) } != 0;
    }

    ids
}

/// The process `id`, opened with the right to tell when it exits and to
/// ask its path, and no other: nothing of it can be read, and Task Manager
/// asks as much. Nothing when Windows refuses, or it has no path.
#[cfg(windows)]
fn open(id: u32) -> Option<Found> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        QueryFullProcessImageNameW,
    };

    // SAFETY: a plain call; the handle is checked, then owned.
    let process = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            id,
        )
    };
    if process.is_null() {
        return None;
    }
    // SAFETY: a handle just opened, which nothing else owns.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };

    // As long as a path gets.
    let mut path = vec![0u16; 32_768];
    let mut length = path.len() as u32;
    // SAFETY: `path` holds `length` units, which the call is told, and the
    // process is open.
    let named = unsafe {
        QueryFullProcessImageNameW(
            process.as_raw_handle(),
            PROCESS_NAME_WIN32,
            path.as_mut_ptr(),
            &mut length,
        )
    } != 0;

    path.get(..length as usize)
        .filter(|_| named)
        .map(|exe| Found {
            id,
            exe: PathBuf::from(OsString::from_wide(exe)),
            process,
        })
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
            ..Launch::default()
        }
    }

    /// The defaults of a client whose prefix is at `prefix`, with Wine.
    fn wine(prefix: &str) -> Defaults {
        Defaults {
            prefix: PathBuf::from(prefix),
            natives: Vec::new(),
            command: WINE_COMMAND,
        }
    }

    #[test]
    fn without_settings_the_client_runs_in_its_own_prefix_with_umu_or_wine() {
        let line = |defaults: &Defaults| {
            Launch::default()
                .line(defaults, Path::new("/games/wow/Wow.exe"))
                .unwrap()
        };

        assert_eq!(
            line(&wine("/prefixes/wow-1a2b3c4d")),
            "WINEPREFIX=/prefixes/wow-1a2b3c4d; export WINEPREFIX; wine /games/wow/Wow.exe"
        );
        assert_eq!(
            line(&Defaults {
                command: UMU_COMMAND,
                ..wine("/prefixes/wow-1a2b3c4d")
            }),
            "WINEPREFIX=/prefixes/wow-1a2b3c4d; export WINEPREFIX; umu-run /games/wow/Wow.exe"
        );
    }

    #[test]
    fn each_client_has_a_prefix_of_its_own_out_of_its_folder() {
        let wow = Path::new("/games/WoW");
        let prefix = prefix(wow);

        assert_eq!(prefix, super::prefix(wow), "the same every time");
        assert!(!prefix.starts_with(wow));
        assert!(prefix.starts_with(dirs::data_dir().unwrap().join("old_launcher/prefixes")));

        let name = prefix.file_name().unwrap().to_str().unwrap();
        let (folder, id) = name.rsplit_once('-').unwrap();
        assert_eq!(folder, "WoW");
        assert_eq!(id.len(), 8);
        assert!(id.bytes().all(|byte| byte.is_ascii_hexdigit()));

        let other = super::prefix(Path::new("/mnt/games/WoW"));
        assert_ne!(other, prefix, "another client of the same name");
        assert!(
            other
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("WoW-")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_prefix_moves_whole_with_its_links() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join("WoW/.prefix");
        fs::create_dir_all(old.join("drive_c/windows")).unwrap();
        fs::create_dir_all(old.join("dosdevices")).unwrap();
        fs::write(old.join("system.reg"), "WINE REGISTRY Version 2").unwrap();
        fs::write(old.join("drive_c/windows/win.ini"), "[fonts]").unwrap();
        symlink("../drive_c", old.join("dosdevices/c:")).unwrap();
        symlink("/", old.join("dosdevices/z:")).unwrap();

        let holds = |prefix: &Path| {
            assert_eq!(
                fs::read_to_string(prefix.join("dosdevices/c:/windows/win.ini")).unwrap(),
                "[fonts]"
            );
            assert_eq!(
                fs::read_link(prefix.join("dosdevices/z:")).unwrap(),
                Path::new("/")
            );
            assert_eq!(
                fs::read_link(prefix.join("dosdevices/c:")).unwrap(),
                Path::new("../drive_c"),
                "a link, not what it leads to"
            );
        };

        // Across disks, copied: as rename would move it, links as links.
        let copy = directory.path().join("copy");
        copied(&old, &copy).unwrap();
        holds(&copy);

        let new = directory
            .path()
            .join("data/old_launcher/prefixes/WoW-1a2b3c4d");
        moved(&old, &new).unwrap();
        holds(&new);
        assert!(fs::symlink_metadata(&old).is_err());

        // One there already is kept, and so is the old one.
        fs::create_dir(&old).unwrap();
        moved(&old, &new).unwrap();
        assert!(old.is_dir());
        holds(&new);
    }

    /// A real prefix, copied into the temporary folder, moves from there to
    /// another disk as it was: every file, mode, folder and link.
    #[cfg(unix)]
    #[test]
    #[ignore = "copies the prefix in $OLD_LAUNCHER_TEST_PREFIX, and moves the copy into \
                $OLD_LAUNCHER_TEST_OTHER_DISK, on another disk"]
    fn a_real_prefix_moves_across_disks() {
        use std::os::unix::fs::PermissionsExt;

        /// Asserts that what is at `a` is what is at `b`, and says how many
        /// files, folders and links that is.
        fn alike(a: &Path, b: &Path) -> usize {
            let (ours, theirs) = (
                fs::symlink_metadata(a).unwrap(),
                fs::symlink_metadata(b).unwrap(),
            );
            assert_eq!(ours.file_type(), theirs.file_type(), "{}", b.display());

            if ours.is_symlink() {
                assert_eq!(fs::read_link(a).unwrap(), fs::read_link(b).unwrap());
                1
            } else if ours.is_dir() {
                let names = |folder: &Path| {
                    let mut names: Vec<_> = fs::read_dir(folder)
                        .unwrap()
                        .map(|entry| entry.unwrap().file_name())
                        .collect();
                    names.sort();
                    names
                };
                let ours = names(a);
                assert_eq!(ours, names(b), "{}", b.display());

                1 + ours
                    .iter()
                    .map(|name| alike(&a.join(name), &b.join(name)))
                    .sum::<usize>()
            } else {
                assert_eq!(
                    ours.permissions().mode(),
                    theirs.permissions().mode(),
                    "{}",
                    b.display()
                );
                assert!(
                    fs::read(a).unwrap() == fs::read(b).unwrap(),
                    "{}",
                    b.display()
                );
                1
            }
        }

        let variable = |name: &str| {
            PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set")))
        };
        let real = variable("OLD_LAUNCHER_TEST_PREFIX");
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir_in(variable("OLD_LAUNCHER_TEST_OTHER_DISK")).unwrap();
        let probe = here.path().join("probe");
        fs::write(&probe, b"?").unwrap();
        assert_eq!(
            fs::rename(&probe, there.path().join("probe"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::CrossesDevices,
            "the two are on one disk"
        );

        let old = here.path().join("WoW").join(OLD_PREFIX);
        fs::create_dir(old.parent().unwrap()).unwrap();
        copied(&real, &old).unwrap();
        let new = there.path().join("prefixes/WoW-1a2b3c4d");
        moved(&old, &new).unwrap();

        assert!(fs::symlink_metadata(&old).is_err(), "deleted once moved");
        assert!(fs::symlink_metadata(new.with_extension("partial")).is_err());
        let entries = alike(&real, &new);
        assert!(entries > 1, "{entries}");
        println!("{entries} files, folders and links moved");
    }

    #[test]
    fn an_old_prefix_stays_where_the_client_sets_its_own() {
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join(OLD_PREFIX);
        fs::create_dir(&old).unwrap();

        move_old_prefix(directory.path(), &launch("WINEPREFIX=~/wow", "")).unwrap();
        assert!(old.is_dir(), "the client's own setting, kept as it is");

        let without = tempfile::tempdir().unwrap();
        move_old_prefix(without.path(), &Launch::default()).unwrap();
    }

    #[test]
    fn umu_run_is_the_default_where_it_is_installed() {
        let folder = tempfile::tempdir().unwrap();
        let path = std::env::join_paths([Path::new("/nowhere"), folder.path()]).unwrap();
        let umu = folder.path().join("umu-run");
        assert!(!installed("umu-run", &path));

        std::fs::write(&umu, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert!(!installed("umu-run", &path), "a file that can't run");
            std::fs::set_permissions(&umu, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(installed("umu-run", &path));
        assert!(!installed("umu-run", OsStr::new("")));

        // And GameMode wraps whichever it is.
        assert_eq!(composed(false, false), "wine %command%");
        assert_eq!(composed(true, false), "umu-run %command%");
        assert_eq!(composed(false, true), "gamemoderun wine %command%");
        assert_eq!(composed(true, true), "gamemoderun umu-run %command%");
        let default = default_command();
        assert!(
            [UMU_COMMAND, WINE_COMMAND]
                .iter()
                .any(|runner| default.ends_with(runner)),
            "{default}"
        );
    }

    #[test]
    fn the_executable_is_quoted_and_the_rest_left_to_the_shell() {
        let line = launch(
            "WINEPREFIX=~/.wine-wow  PROTONPATH='GE Proton'",
            "  gamemoderun umu-run %command% -console ",
        )
        .line(
            &wine("/prefixes/World of Warcraft 3.3.5a-1a2b3c4d"),
            Path::new("/games/World of Warcraft 3.3.5a/Wow.exe"),
        )
        .unwrap();

        assert_eq!(
            line,
            "WINEPREFIX='/prefixes/World of Warcraft 3.3.5a-1a2b3c4d' WINEPREFIX=~/.wine-wow  \
             PROTONPATH='GE Proton'; export WINEPREFIX PROTONPATH; \
             gamemoderun umu-run '/games/World of Warcraft 3.3.5a/Wow.exe' -console",
            "the client's own prefix after the launcher's, which it overrides"
        );
    }

    #[test]
    fn a_command_needs_the_placeholder_or_nothing() {
        let handing_over = launch("", "lutris lutris:rungameid/3");

        assert_eq!(
            handing_over.command_problem().as_deref(),
            Some("%command% is missing: the launcher puts what it runs there.")
        );
        assert!(matches!(
            handing_over.line(&wine("/prefixes/wow"), Path::new("/games/wow/Wow.exe")),
            Err(Error::Launch {
                setting: "command",
                ..
            })
        ));
        assert_eq!(launch("", "  ").command_problem(), None, "the default");
        assert_eq!(launch("", "umu-run %command%").command_problem(), None);
    }

    #[test]
    fn a_tool_runs_the_way_the_game_does_with_its_arguments() {
        let arguments = ["--data".to_owned(), "Data".to_owned(), "a b".to_owned()];
        let tool = "Lumenveil/lamp_extract.exe";
        let defaults = wine("/prefixes/World of Warcraft-1a2b3c4d");

        assert_eq!(
            launch("WINEPREFIX=~/wow", "")
                .tool_line(&defaults, tool, &arguments)
                .unwrap(),
            "WINEPREFIX='/prefixes/World of Warcraft-1a2b3c4d' WINEPREFIX=~/wow; export WINEPREFIX; \
             wine \"Lumenveil\\\\lamp_extract.exe\" --data Data 'a b'",
            "the program as a Windows path, which Proton runs itself"
        );
        assert!(matches!(
            launch("", "lutris lutris:rungameid/3").tool_line(&defaults, tool, &arguments),
            Err(Error::Launch {
                setting: "command",
                ..
            })
        ));
    }

    #[test]
    fn a_tool_runs_without_gamemode_which_the_game_keeps() {
        let tool = "Lumenveil/lamp_extract.exe";
        let game = Path::new("/games/wow/Wow.exe");
        let gaming = Defaults {
            command: "gamemoderun umu-run %command%",
            ..wine("/prefixes/wow")
        };

        assert_eq!(
            launch("", "").tool_line(&gaming, tool, &[]).unwrap(),
            "WINEPREFIX=/prefixes/wow; export WINEPREFIX; \
             umu-run \"Lumenveil\\\\lamp_extract.exe\"",
            "the default's"
        );
        assert_eq!(
            launch("", "").line(&gaming, game).unwrap(),
            "WINEPREFIX=/prefixes/wow; export WINEPREFIX; gamemoderun umu-run /games/wow/Wow.exe"
        );
        assert_eq!(
            launch("", "  gamemoderun  wine %command%")
                .tool_line(&gaming, tool, &[])
                .unwrap(),
            "WINEPREFIX=/prefixes/wow; export WINEPREFIX; \
             wine \"Lumenveil\\\\lamp_extract.exe\"",
            "the client's own"
        );
        assert_eq!(
            without_gamemode("gamemoderun-ish %command%"),
            "gamemoderun-ish %command%",
            "only the word"
        );
    }

    #[test]
    fn wine_is_told_to_load_the_dlls_mods_put_beside_wow_exe_from_there() {
        let defaults = wine("/prefixes/World of Warcraft-1a2b3c4d");
        assert_eq!(
            defaults.env().unwrap(),
            ["WINEPREFIX='/prefixes/World of Warcraft-1a2b3c4d'"]
        );

        let modded = Defaults {
            natives: vec!["CoAVolFog".to_owned(), "version".to_owned()],
            ..defaults
        };
        assert_eq!(
            modded.env().unwrap(),
            [
                "WINEPREFIX='/prefixes/World of Warcraft-1a2b3c4d'",
                "WINEDLLOVERRIDES='CoAVolFog=n,b;version=n,b'",
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_launchers_variables_come_first_so_the_clients_own_override_them() {
        let (_root, installation) = crate::testing::client(&[]);
        let line = |command: Command| command.get_args().nth(1).unwrap().to_owned();
        let game = |client: &Launch| {
            line(
                command(
                    &installation,
                    client,
                    &["version".to_owned(), "Glow".to_owned()],
                )
                .unwrap(),
            )
        };
        let prefix = prefix(installation.root());
        let prefix = shlex::try_quote(prefix.to_str().unwrap()).unwrap();
        let added = format!("WINEPREFIX={prefix} WINEDLLOVERRIDES='version=n,b;Glow=n,b'");

        assert!(
            game(&launch("DXVK_HUD=1", "umu-run %command%"))
                .to_str()
                .unwrap()
                .starts_with(&format!(
                    "{added} DXVK_HUD=1; export WINEPREFIX WINEDLLOVERRIDES DXVK_HUD; umu-run "
                ))
        );

        let client = launch(
            "WINEPREFIX=~/wow WINEDLLOVERRIDES='d3d9=n'",
            "umu-run %command%",
        );
        assert!(
            game(&client).to_str().unwrap().starts_with(&format!(
                "{added} WINEPREFIX=~/wow WINEDLLOVERRIDES='d3d9=n'; export WINEPREFIX \
                 WINEDLLOVERRIDES; umu-run "
            )),
            "set again, the client's own win"
        );

        let setup = tool(&installation, &client, "Glow/setup.exe", &[]).unwrap();
        assert!(
            line(setup).to_str().unwrap().starts_with(&format!(
                "WINEPREFIX={prefix} WINEPREFIX=~/wow WINEDLLOVERRIDES='d3d9=n'; export \
                 WINEPREFIX WINEDLLOVERRIDES; umu-run "
            )),
            "a setup is no game: its DLLs needn't load"
        );
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
    fn a_program_run_with_the_game_is_a_line_or_nothing() {
        let with = |line: &str| Launch {
            companion: line.into(),
            ..Launch::default()
        };

        assert!(with("").is_default());
        assert!(!with("music-bridge").is_default());
        assert_eq!(with("  ").companion(), None, "nothing set");
        assert_eq!(
            with(" music-bridge --game 'World of Warcraft' ").companion(),
            Some("music-bridge --game 'World of Warcraft'")
        );
        assert_eq!(with("music-bridge").companion_problem(), None);
        assert_eq!(
            with("music-bridge 'World of")
                .companion_problem()
                .as_deref(),
            if cfg!(windows) {
                None
            } else {
                Some("A quote is never closed.")
            }
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
            unclosed.line(&wine("/prefixes/wow"), Path::new("/games/wow/Wow.exe")),
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

        // A process takes its new name a moment after it starts, so this
        // looks a few times, as the launcher does every second.
        let found = (0..100).find_map(|_| {
            let found = running(&clients);
            if found.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            (!found.is_empty()).then_some(found)
        });
        assert_eq!(found.as_deref(), Some(std::slice::from_ref(&root)));

        game.kill().unwrap();
        game.wait().unwrap();
        assert!(running(&clients).is_empty());
    }

    /// Set for [`the_game`] when it stands for the game.
    #[cfg(windows)]
    const GAME: &str = "OLD_LAUNCHER_TEST_GAME";

    /// The game of the test below, which runs this test program as a
    /// client's `Wow.exe`, with this test alone: it waits to be found.
    #[cfg(windows)]
    #[test]
    #[ignore = "the game a_game_is_running_while_its_wow_exe_runs runs"]
    fn the_game() {
        if std::env::var_os(GAME).is_some() {
            std::thread::sleep(std::time::Duration::from_secs(30));
        }
    }

    /// This test program, copied as a client's `Wow.exe`, stands for the
    /// game.
    #[cfg(windows)]
    #[test]
    fn a_game_is_running_while_its_wow_exe_runs() {
        use std::fs;

        let directory = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(directory.path()).unwrap();
        let other = root.join("other");
        fs::create_dir(&other).unwrap();
        fs::copy(std::env::current_exe().unwrap(), root.join("Wow.exe")).unwrap();
        let clients = [root.clone(), other];

        assert!(running(&clients).is_empty());

        let mut game = Command::new(root.join("Wow.exe"))
            .args(["launch::tests::the_game", "--exact", "--ignored", "--quiet"])
            .env(GAME, "1")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();

        let found = (0..100).find_map(|_| {
            let found = running(&clients);
            if found.is_empty() {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            (!found.is_empty()).then_some(found)
        });
        assert_eq!(found.as_deref(), Some(std::slice::from_ref(&root)));
        // Kept open since: asked only whether it exited.
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
        use crate::testing::{WOTLK, fake_executable};

        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("World of Warcraft 3.3.5a");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("Wow.exe"), fake_executable(WOTLK)).unwrap();
        let installation = Installation::open(&root).unwrap();

        let printed = |env: &str| {
            let status = command(
                &installation,
                &launch(
                    env,
                    "cp %command% copy.exe && printenv GREETING WINEPREFIX > env.txt",
                ),
                &[],
            )
            .unwrap()
            .env("HOME", "/home/player")
            .env("WINEPREFIX", "/home/player/.wine")
            .status()
            .unwrap();
            assert!(status.success());

            std::fs::read_to_string(root.join("env.txt")).unwrap()
        };

        assert_eq!(
            printed("GREETING='hello there'"),
            format!("hello there\n{}\n", prefix(installation.root()).display()),
            "its own prefix, whatever the launcher's environment says"
        );
        assert_eq!(
            std::fs::read(root.join("copy.exe")).unwrap(),
            fake_executable(WOTLK)
        );
        assert_eq!(
            printed("GREETING='hello there' WINEPREFIX=~/.wine-wow"),
            "hello there\n/home/player/.wine-wow\n",
            "the client's own, set for the whole command, with ~ expanded"
        );
    }
}
