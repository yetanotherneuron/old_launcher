//! Showing on Discord that the game is played. With the setting on, the
//! wrapper the game runs under (see [`crate::wrapper`]) holds a connection
//! to the Discord desktop app while the game runs. Discord clears the
//! status when the connection closes, so it goes with the game, whether the
//! launcher is still open or not.
//!
//! Discord listens on a local socket (a named pipe on Windows). Each frame
//! on it is an opcode and the payload's length, both u32 little-endian,
//! then the payload, in JSON: a handshake naming the application, then
//! `SET_ACTIVITY`.

use std::convert::Infallible;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::describe;

/// The Discord application the status names: "Old Launcher", with its
/// image.
const APPLICATION: &str = "1562908962225918146";

/// What the status says under the application's name.
const DETAILS: &str = "Playing World of Warcraft 3.3.5a";

/// The image's key among the application's Rich Presence art assets.
const IMAGE: &str = "old_launcher";

/// How long to wait before looking for Discord again, when it isn't running
/// or quit.
const RETRY: Duration = Duration::from_secs(15);

/// The largest frame taken from Discord, whose own are a few kilobytes.
const LARGEST: u32 = 64 * 1024;

// The opcodes of the frames.
const HANDSHAKE: u32 = 0;
const FRAME: u32 = 1;
const CLOSE: u32 = 2;
const PING: u32 = 3;
const PONG: u32 = 4;

/// What the status shows, for a game that started at `start`: the
/// application's name and image, [`DETAILS`] and the time played.
pub(crate) fn activity(start: SystemTime) -> Value {
    let start = start
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|since| u64::try_from(since.as_millis()).ok())
        .unwrap_or_default();

    json!({
        "details": DETAILS,
        "timestamps": { "start": start },
        "assets": { "large_image": IMAGE },
    })
}

/// Shows `activity` on Discord for the process `pid`, through the first of
/// `sockets` it listens on, for as long as this runs. When Discord isn't
/// running, or quits, looks for it again every [`RETRY`].
pub(crate) async fn show(pid: u32, activity: &Value, sockets: &[PathBuf]) -> Infallible {
    loop {
        match connect(sockets).await {
            Some((path, socket)) => {
                log::info!("Connected to Discord, through {}", path.display());

                match present(socket, pid, activity).await {
                    Ok(()) => log::info!("Discord closed the connection"),
                    Err(why) => log::warn!("Could not show the game on Discord: {why}"),
                }
            }
            None => log::debug!("Discord isn't running"),
        }

        tokio::time::sleep(RETRY).await;
    }
}

/// How the launcher reaches Discord.
#[cfg(unix)]
type Socket = tokio::net::UnixStream;
#[cfg(windows)]
type Socket = tokio::net::windows::named_pipe::NamedPipeClient;

/// The first of `sockets` Discord listens on, connected.
async fn connect(sockets: &[PathBuf]) -> Option<(&Path, Socket)> {
    for path in sockets {
        #[cfg(unix)]
        let socket = tokio::net::UnixStream::connect(path).await;
        #[cfg(windows)]
        let socket = tokio::net::windows::named_pipe::ClientOptions::new().open(path);

        match socket {
            Ok(socket) => return Some((path.as_path(), socket)),
            Err(error) => log::trace!("No Discord at {}: {error}", path.display()),
        }
    }

    None
}

/// Where Discord may listen: named pipes on Windows.
#[cfg(windows)]
pub(crate) fn sockets() -> Vec<PathBuf> {
    (0..10)
        .map(|number| PathBuf::from(format!(r"\\.\pipe\discord-ipc-{number}")))
        .collect()
}

/// Where Discord may listen: in the first of `XDG_RUNTIME_DIR`, `TMPDIR`,
/// `TMP` and `TEMP` that is set, or else `/tmp`, as Discord picks.
#[cfg(unix)]
pub(crate) fn sockets() -> Vec<PathBuf> {
    let folder = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
        .into_iter()
        .find_map(std::env::var_os)
        .map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);

    sockets_in(&folder)
}

/// The sockets Discord may listen on in `folder`: its own, or those of its
/// Flatpak or Snap, or of Vesktop, which answers in its place.
#[cfg(unix)]
fn sockets_in(folder: &Path) -> Vec<PathBuf> {
    const APPS: [&str; 7] = [
        "",
        "app/com.discordapp.Discord",
        ".flatpak/com.discordapp.Discord/xdg-run",
        "snap.discord",
        "snap.discord-canary",
        "app/dev.vencord.Vesktop",
        ".flatpak/dev.vencord.Vesktop/xdg-run",
    ];

    APPS.into_iter()
        .flat_map(|app| {
            (0..10).map(move |number| folder.join(app).join(format!("discord-ipc-{number}")))
        })
        .collect()
}

/// Shows `activity` on Discord for the process `pid`, through `socket`,
/// until the connection ends: fine when Discord closed it, or else why it
/// ended.
async fn present<S: AsyncRead + AsyncWrite + Unpin>(
    mut socket: S,
    pid: u32,
    activity: &Value,
) -> Result<(), String> {
    let failed = |error: io::Error| describe(&error);

    send(
        &mut socket,
        HANDSHAKE,
        &json!({ "v": 1, "client_id": APPLICATION }),
    )
    .await
    .map_err(failed)?;

    match receive(&mut socket).await.map_err(failed)? {
        (FRAME, ready) if ready["evt"] == "READY" => {}
        (CLOSE, refusal) => {
            return Err(format!("Discord refused it: {}", message(&refusal)));
        }
        (_, answer) => return Err(format!("Discord answered {answer}")),
    }

    send(
        &mut socket,
        FRAME,
        &json!({
            "cmd": "SET_ACTIVITY",
            "args": { "pid": pid, "activity": activity },
            "nonce": "1",
        }),
    )
    .await
    .map_err(failed)?;

    loop {
        match receive(&mut socket).await {
            Ok((PING, payload)) => send(&mut socket, PONG, &payload).await.map_err(failed)?,
            Ok((CLOSE, _)) => return Ok(()),
            // Asking again would be refused again.
            Ok((FRAME, answer)) if answer["evt"] == "ERROR" => {
                log::warn!(
                    "Discord refused to show the game: {}",
                    message(&answer["data"])
                );
            }
            Ok((FRAME, answer)) if answer["cmd"] == "SET_ACTIVITY" => {
                log::info!("Discord shows the game");
            }
            Ok(_) => {}
            // A named pipe closed on the other end is broken.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
                ) =>
            {
                return Ok(());
            }
            Err(error) => return Err(failed(error)),
        }
    }
}

/// What Discord says in a refusal.
fn message(refusal: &Value) -> &str {
    refusal["message"].as_str().unwrap_or("no reason given")
}

/// Sends `payload` in a frame of `opcode`.
async fn send<S: AsyncWrite + Unpin>(
    socket: &mut S,
    opcode: u32,
    payload: &Value,
) -> io::Result<()> {
    let payload = serde_json::to_vec(payload)?;
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "a frame too large"))?;

    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend(opcode.to_le_bytes());
    frame.extend(length.to_le_bytes());
    frame.extend(payload);

    socket.write_all(&frame).await?;
    socket.flush().await
}

/// The next frame: its opcode and payload.
async fn receive<S: AsyncRead + Unpin>(socket: &mut S) -> io::Result<(u32, Value)> {
    let mut opcode = [0; 4];
    let mut length = [0; 4];
    socket.read_exact(&mut opcode).await?;
    socket.read_exact(&mut length).await?;

    let length = u32::from_le_bytes(length);
    if length > LARGEST {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a frame of {length} bytes"),
        ));
    }

    let mut payload = vec![0; length as usize];
    socket.read_exact(&mut payload).await?;

    Ok((
        u32::from_le_bytes(opcode),
        serde_json::from_slice(&payload)?,
    ))
}

#[cfg(test)]
mod tests {
    use std::process;

    use super::*;

    /// The shell command and its option to run a line with, as games start
    /// on Windows (`cmd`, Wine's under CI) or elsewhere.
    const SHELL: [&str; 2] = if cfg!(windows) {
        ["cmd", "/C"]
    } else {
        ["sh", "-c"]
    };

    /// A command running `line` in the [`SHELL`].
    fn shell(line: &str) -> tokio::process::Command {
        let [program, option] = SHELL;
        let mut command = tokio::process::Command::new(program);
        command.arg(option).arg(line);
        command
    }

    /// A Discord listening where only the test looks: a socket in a
    /// folder of its own, or a named pipe of its own on Windows.
    struct FakeDiscord {
        path: PathBuf,
        #[cfg(unix)]
        listener: (tempfile::TempDir, tokio::net::UnixListener),
        #[cfg(windows)]
        server: tokio::net::windows::named_pipe::NamedPipeServer,
    }

    impl FakeDiscord {
        #[cfg(unix)]
        fn new() -> Self {
            let folder = tempfile::tempdir().unwrap();
            let path = folder.path().join("discord-ipc-0");
            let listener = tokio::net::UnixListener::bind(&path).unwrap();

            Self {
                path,
                listener: (folder, listener),
            }
        }

        #[cfg(windows)]
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            // Tests run side by side in one process.
            static PIPES: AtomicUsize = AtomicUsize::new(0);

            let path = PathBuf::from(format!(
                r"\\.\pipe\old_launcher-test-{}-{}",
                process::id(),
                PIPES.fetch_add(1, Ordering::Relaxed)
            ));
            let server = tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(true)
                .create(&path)
                .unwrap();

            Self { path, server }
        }

        /// Its side of the connection the launcher makes.
        #[cfg(unix)]
        async fn accept(self) -> tokio::net::UnixStream {
            self.listener.1.accept().await.unwrap().0
        }

        /// Its side of the connection the launcher makes.
        #[cfg(windows)]
        async fn accept(self) -> tokio::net::windows::named_pipe::NamedPipeServer {
            self.server.connect().await.unwrap();
            self.server
        }
    }

    /// Discord's side of a connection: answers the handshake, and gives the
    /// activity asked for.
    async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(discord: &mut S) -> Value {
        let (opcode, handshake) = receive(discord).await.unwrap();
        assert_eq!(opcode, HANDSHAKE);
        assert_eq!(handshake, json!({ "v": 1, "client_id": APPLICATION }));

        send(
            discord,
            FRAME,
            &json!({ "cmd": "DISPATCH", "evt": "READY" }),
        )
        .await
        .unwrap();

        let (opcode, command) = receive(discord).await.unwrap();
        assert_eq!(opcode, FRAME);
        assert_eq!(command["cmd"], "SET_ACTIVITY");

        command["args"].clone()
    }

    #[tokio::test]
    async fn frames_are_an_opcode_and_a_length_then_json() {
        let (mut ours, mut theirs) = tokio::io::duplex(1024);

        send(&mut ours, PING, &json!({ "a": 1 })).await.unwrap();

        let mut header = [0; 8];
        theirs.read_exact(&mut header).await.unwrap();
        assert_eq!(header, [3, 0, 0, 0, 7, 0, 0, 0], "little-endian");
        let mut payload = [0; 7];
        theirs.read_exact(&mut payload).await.unwrap();
        assert_eq!(&payload, br#"{"a":1}"#);

        send(&mut theirs, PONG, &json!({ "a": 1 })).await.unwrap();
        assert_eq!(receive(&mut ours).await.unwrap(), (PONG, json!({ "a": 1 })));
    }

    #[tokio::test]
    async fn a_frame_larger_than_discord_sends_is_refused() {
        let (mut ours, mut theirs) = tokio::io::duplex(1024);

        let mut header = FRAME.to_le_bytes().to_vec();
        header.extend((LARGEST + 1).to_le_bytes());
        theirs.write_all(&header).await.unwrap();

        let error = receive(&mut ours).await.unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn discord_shows_the_game_until_it_closes_the_connection() {
        let (ours, mut discord) = tokio::io::duplex(4096);
        let start = UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        let shown = tokio::spawn(async move { present(ours, 42, &activity(start)).await });

        let args = handshake(&mut discord).await;
        assert_eq!(
            args,
            json!({
                "pid": 42,
                "activity": {
                    "details": "Playing World of Warcraft 3.3.5a",
                    "timestamps": { "start": 1_800_000_000_000_u64 },
                    "assets": { "large_image": "old_launcher" },
                },
            }),
            "the start in milliseconds"
        );

        send(
            &mut discord,
            FRAME,
            &json!({ "cmd": "SET_ACTIVITY", "evt": null, "nonce": "1" }),
        )
        .await
        .unwrap();
        // Refusing the activity doesn't end the connection.
        send(
            &mut discord,
            FRAME,
            &json!({ "evt": "ERROR", "data": { "message": "no" } }),
        )
        .await
        .unwrap();
        send(&mut discord, PING, &json!({ "n": 7 })).await.unwrap();
        assert_eq!(
            receive(&mut discord).await.unwrap(),
            (PONG, json!({ "n": 7 }))
        );

        drop(discord);
        assert_eq!(shown.await.unwrap(), Ok(()));
    }

    #[tokio::test]
    async fn discord_refusing_the_application_says_why() {
        let (ours, mut discord) = tokio::io::duplex(4096);
        let shown = tokio::spawn(async move { present(ours, 42, &json!({})).await });

        let _ = receive(&mut discord).await.unwrap();
        send(
            &mut discord,
            CLOSE,
            &json!({ "code": 4000, "message": "Invalid Client ID" }),
        )
        .await
        .unwrap();

        let why = shown.await.unwrap().unwrap_err();
        assert!(why.contains("Invalid Client ID"), "{why}");
    }

    #[cfg(unix)]
    #[test]
    fn discord_is_looked_for_in_its_flatpak_and_snap_folders_too() {
        let sockets = sockets_in(Path::new("/run/user/1000"));

        for socket in [
            "/run/user/1000/discord-ipc-0",
            "/run/user/1000/discord-ipc-9",
            "/run/user/1000/app/com.discordapp.Discord/discord-ipc-0",
            "/run/user/1000/snap.discord/discord-ipc-0",
        ] {
            assert!(sockets.contains(&PathBuf::from(socket)), "{socket}");
        }
        assert_eq!(sockets[0], Path::new("/run/user/1000/discord-ipc-0"));
    }

    #[cfg(windows)]
    #[test]
    fn discord_is_looked_for_in_its_named_pipes() {
        let sockets = sockets();

        assert_eq!(sockets.len(), 10);
        assert_eq!(sockets[0], Path::new(r"\\.\pipe\discord-ipc-0"));
        assert_eq!(sockets[9], Path::new(r"\\.\pipe\discord-ipc-9"));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn the_first_pipe_listened_on_is_used() {
        let discord = FakeDiscord::new();
        let nobody = PathBuf::from(r"\\.\pipe\old_launcher-test-nobody");
        let pipes = [nobody.clone(), discord.path.clone()];

        let (path, _pipe) = connect(&pipes).await.unwrap();

        assert_eq!(path, discord.path);
        assert!(connect(&[nobody]).await.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_first_socket_listened_on_is_used() {
        let folder = tempfile::tempdir().unwrap();
        let sockets = sockets_in(folder.path());
        // Left behind by a Discord that crashed: nobody listens. Bound and
        // never listened on, rather than a listener dropped: a process
        // another test starts holds a copy of every open socket until it
        // runs its program, which keeps a dropped listener listening.
        let left = tokio::net::UnixSocket::new_stream().unwrap();
        left.bind(&sockets[0]).unwrap();
        let flatpak = folder.path().join("app/com.discordapp.Discord");
        std::fs::create_dir_all(&flatpak).unwrap();
        let _listener = tokio::net::UnixListener::bind(flatpak.join("discord-ipc-0")).unwrap();

        let (path, _socket) = connect(&sockets).await.unwrap();

        assert_eq!(path, flatpak.join("discord-ipc-0"));
        assert!(connect(&sockets[..10]).await.is_none());
    }

    #[tokio::test]
    async fn the_status_goes_when_the_game_exits() {
        let discord = FakeDiscord::new();
        let socket = discord.path.clone();
        // A game that runs until told to quit.
        let line = if cfg!(windows) {
            "set /p line= & exit 5"
        } else {
            "read line; exit 5"
        };
        let mut child = shell(line).stdin(process::Stdio::piped()).spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        let pid = child.id().unwrap();

        let played =
            tokio::spawn(async move { crate::wrapper::follow(child, None, Some(&[socket])).await });
        let mut discord = discord.accept().await;
        let args = handshake(&mut discord).await;
        assert_eq!(args["pid"], pid, "the game's own process");
        assert_eq!(args["activity"]["details"], DETAILS);

        stdin.write_all(b"quit\r\n").await.unwrap();

        assert_eq!(played.await.unwrap(), 5);
        let closed = receive(&mut discord).await.unwrap_err();
        assert!(
            matches!(
                closed.kind(),
                io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
            ),
            "{closed}"
        );
    }
}
