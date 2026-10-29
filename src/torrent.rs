//! The BitTorrent engine, librqbit: one session for the whole launcher,
//! started the first time a torrent is needed, and another that shares
//! nothing for the torrents librqbit can't share (see [`shareable`]). It
//! finds the torrent of a link or a file, and downloads; checking a client
//! folder against its torrent is core's (`core::torrent::check`), and only
//! reads.
//!
//! It runs on threads of its own, and the launcher waits on it for so long
//! only, never from the window: should it hang, the window, stopping and
//! quitting still work, and the next download starts another.

use std::collections::HashSet;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, DhtSessionConfig, ListenerOptions,
    ManagedTorrent, Session, SessionOptions, TorrentStatsState,
};
use librqbit_peer_protocol::MAX_MSG_LEN;
use tokio::runtime::{Handle, Runtime};
use tokio::sync::OnceCell;

use crate::core::torrent::Torrent;

/// How long a magnet link gets to find someone sharing its torrent.
const RESOLVING: Duration = Duration::from_secs(120);

/// How long the engine gets to answer, besides any wait it is asked to
/// make: one that takes longer is stuck.
const PATIENCE: Duration = Duration::from_secs(30);

/// How often a download is asked how far it got.
const WATCHING: Duration = Duration::from_secs(1);

/// What asking a stuck engine anything gets.
const STUCK: &str = "the BitTorrent engine stopped answering";

/// The largest `.torrent` file downloaded: a client's is well below.
const LARGEST_TORRENT: usize = 16 * 1024 * 1024;

/// Where a torrent comes from, as the player gave it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A magnet link.
    Magnet(String),
    /// The address of a `.torrent` file.
    Url(String),
    /// A `.torrent` file on this computer.
    File(PathBuf),
}

impl Source {
    /// What `text` is: a magnet link, a web address, or else a file path.
    pub fn parse(text: &str) -> Self {
        let text = text.trim();
        if text.starts_with("magnet:") {
            Self::Magnet(text.to_owned())
        } else if text.starts_with("http://") || text.starts_with("https://") {
            Self::Url(text.to_owned())
        } else {
            Self::File(PathBuf::from(text.strip_prefix("file://").unwrap_or(text)))
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Magnet(link) | Self::Url(link) => f.write_str(link),
            Self::File(path) => write!(f, "{}", path.display()),
        }
    }
}

/// The engine: librqbit sessions.
#[derive(Clone)]
pub struct Engine {
    /// The session sharing what it downloads.
    session: Arc<Session>,
    /// The session sharing nothing, once a torrent needs it.
    unshared: Arc<OnceCell<Arc<Session>>>,
    /// The options of its sessions, sharing or not.
    options: fn(bool) -> SessionOptions,
    /// Whether it once took too long to answer.
    stuck: Arc<AtomicBool>,
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Engine")
    }
}

/// A torrent being downloaded, or checked before it is.
#[derive(Clone)]
pub struct Download {
    /// The session it is in.
    session: Arc<Session>,
    handle: Arc<ManagedTorrent>,
    /// What it said last.
    said: Arc<Mutex<Said>>,
}

/// What a download said last, of the files it was told to download.
struct Said {
    /// How many times it was told which files: whatever was asked of it
    /// before the last time is of other files.
    told: u64,
    progress: Progress,
}

impl fmt::Debug for Download {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Download({})", self.handle.id())
    }
}

/// How far a download got.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Progress {
    /// Whether it is looking at what the folder holds already.
    pub checking: bool,
    /// The bytes it has, of those it downloads.
    pub done: u64,
    /// The bytes it downloads.
    pub total: u64,
    /// The peers it exchanges pieces with.
    pub peers: u32,
    /// The peers it is connecting to.
    pub connecting: u32,
    /// Bytes a second received, and sent.
    pub down: u64,
    pub up: u64,
    /// Whether it has everything it downloads.
    pub finished: bool,
    /// Why it stopped, if it failed.
    pub error: Option<String>,
}

impl Engine {
    /// Starts the session (see [`swarm`]).
    pub async fn start() -> Result<Self, String> {
        Self::with(swarm).await
    }

    /// Starts the session sharing what it downloads, with `options(true)`;
    /// the other gets `options(false)`.
    async fn with(options: fn(bool) -> SessionOptions) -> Result<Self, String> {
        // librqbit's HTTP client needs rustls' provider, as the launcher's
        // does; installed already, this fails, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let stuck = Arc::new(AtomicBool::new(false));
        let session = on_engine(PATIENCE, &stuck, session(options(true))).await?;

        Ok(Self {
            session,
            unshared: Arc::default(),
            options,
            stuck,
        })
    }

    /// Whether it answers yet: one that took too long once is left behind.
    pub fn answers(&self) -> bool {
        !self.stuck.load(Ordering::Relaxed)
    }

    /// Does `work` on the engine's threads, waiting for it `patience` at
    /// most.
    async fn ask<T: Send + 'static>(
        &self,
        patience: Duration,
        work: impl Future<Output = Result<T, String>> + Send + 'static,
    ) -> Result<T, String> {
        on_engine(patience, &self.stuck, work).await
    }

    /// The session sharing nothing, started the first time it is asked for.
    async fn unshared(&self) -> Result<Arc<Session>, String> {
        let options = (self.options)(false);

        self.unshared
            .get_or_try_init(|| self.ask(PATIENCE, session(options)))
            .await
            .cloned()
    }

    /// The bytes of the `.torrent` of a magnet `link`, asked of the peers
    /// sharing it: those found, and `peers`.
    async fn resolve(&self, link: &str, peers: Vec<SocketAddr>) -> Result<Vec<u8>, String> {
        let options = AddTorrentOptions {
            list_only: true,
            initial_peers: (!peers.is_empty()).then_some(peers),
            ..AddTorrentOptions::default()
        };
        let link = link.to_owned();
        let session = self.session.clone();

        self.ask(RESOLVING + PATIENCE, async move {
            let listed = session.add_torrent(AddTorrent::from_url(&link), Some(options));
            match tokio::time::timeout(RESOLVING, listed).await {
                Ok(Ok(AddTorrentResponse::ListOnly(listed))) => Ok(listed.torrent_bytes.to_vec()),
                Ok(Ok(_)) => Err("the torrent was not listed".to_owned()),
                Ok(Err(error)) => Err(failed(error)),
                Err(_) => Err("nobody sharing it was found".to_owned()),
            }
        })
        .await
    }

    /// Downloads `torrent` (its bytes) into `folder`, only the `files` by
    /// index when given. What the folder holds already is checked first and
    /// kept where it is right, so this resumes too. `peers` are connected to
    /// besides those found. What it downloads, it shares, if it can.
    pub async fn download(
        &self,
        torrent: &[u8],
        folder: &Path,
        files: Option<Vec<usize>>,
        peers: Vec<SocketAddr>,
    ) -> Result<Download, String> {
        let parsed = Torrent::parse(torrent).map_err(|error| error.to_string())?;
        let shares = shareable(parsed.pieces());
        if !shares {
            log::info!(
                "{} has too many pieces to share: downloading it sharing nothing",
                parsed.name
            );
        }

        self.download_in(shares, torrent, folder, files, peers)
            .await
    }

    /// [`Engine::download`], in the session sharing what it downloads if
    /// `shares`, else in the one sharing nothing.
    async fn download_in(
        &self,
        shares: bool,
        torrent: &[u8],
        folder: &Path,
        files: Option<Vec<usize>>,
        peers: Vec<SocketAddr>,
    ) -> Result<Download, String> {
        let session = if shares {
            self.session.clone()
        } else {
            self.unshared().await?
        };
        let options = AddTorrentOptions {
            overwrite: true,
            output_folder: Some(folder.to_string_lossy().into_owned()),
            only_files: files,
            initial_peers: (!peers.is_empty()).then_some(peers),
            ..AddTorrentOptions::default()
        };
        let torrent = torrent.to_vec();

        self.ask(PATIENCE, async move {
            let added = session
                .add_torrent(AddTorrent::from_bytes(torrent), Some(options))
                .await
                .map_err(failed)?;
            let handle = match added {
                AddTorrentResponse::Added(_, handle)
                | AddTorrentResponse::AlreadyManaged(_, handle) => handle,
                AddTorrentResponse::ListOnly(_) => {
                    return Err("the torrent was only listed".to_owned());
                }
            };
            let said = Arc::new(Mutex::new(Said {
                told: 0,
                progress: progress_of(&handle),
            }));
            tokio::spawn(watch(handle.clone(), Arc::downgrade(&said)));

            Ok(Download {
                session,
                handle,
                said,
            })
        })
        .await
    }

    /// Makes `download` download the `files`, by index, and no others. It
    /// is paused meanwhile: changed while it runs, librqbit 9.0.1 can
    /// deadlock, holding the torrent's lock while it walks the peers that
    /// take it (ikatson/rqbit#659, fixed after 9.0.1).
    pub async fn select(&self, download: &Download, files: &[usize]) -> Result<(), String> {
        let files: HashSet<usize> = files.iter().copied().collect();
        let Download {
            session,
            handle,
            said,
        } = download.clone();

        self.ask(PATIENCE, async move {
            session.pause(&handle).await.map_err(failed)?;
            session
                .update_only_files(&handle, &files)
                .await
                .map_err(failed)?;
            session.unpause(&handle).await.map_err(failed)?;
            let progress = progress_of(&handle);
            let mut said = lock(&said);
            said.told += 1;
            said.progress = progress;
            Ok(())
        })
        .await
    }

    /// Stops `download`, keeping what it downloaded, and shares nothing more.
    pub async fn forget(&self, download: &Download) -> Result<(), String> {
        let (session, id) = (download.session.clone(), download.handle.id());

        self.ask(PATIENCE, async move {
            session.delete(id.into(), false).await.map_err(failed)
        })
        .await
    }
}

/// The options of a session out in the swarms. Peers are found through the
/// trackers a torrent names and the DHT (whose state isn't kept between
/// starts), and connected to. It listens for them too, on a port of the
/// system's choosing that the router is asked to forward (UPnP), as games
/// do, so others take pieces from it while it downloads: what it has, it
/// shares, if it `shares`.
fn swarm(shares: bool) -> SessionOptions {
    SessionOptions {
        dht: Some(DhtSessionConfig {
            persistence: None,
            ..DhtSessionConfig::default()
        }),
        persistence: None,
        fastresume: false,
        listen: Some(ListenerOptions {
            enable_upnp_port_forwarding: true,
            ..ListenerOptions::default()
        }),
        disable_upload: !shares,
        ..SessionOptions::default()
    }
}

/// Starts a session with `options`.
async fn session(options: SessionOptions) -> Result<Arc<Session>, String> {
    Session::new_with_opts(std::env::temp_dir(), options)
        .await
        .map_err(failed)
}

/// Whether librqbit can share a torrent of `pieces` pieces. To each peer it
/// connects to, it first says which pieces it has, in one message that must
/// fit its buffer: a torrent of more pieces than that has room for (some
/// 132,000; the HD client has 153,881) has every peer dropped once it has a
/// piece. A session sharing nothing never says.
fn shareable(pieces: usize) -> bool {
    // The message's length and kind, then a bit a piece.
    5 + pieces.div_ceil(8) <= MAX_MSG_LEN
}

/// The engine's threads: whatever happens on them, the window's go on.
/// They are never dropped, so quitting doesn't wait for them either.
fn threads() -> Result<&'static Handle, String> {
    static RUNTIME: OnceLock<Result<Runtime, String>> = OnceLock::new();

    RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .thread_name("torrent")
                .enable_all()
                .build()
                .map_err(failed)
        })
        .as_ref()
        .map(Runtime::handle)
        .map_err(Clone::clone)
}

/// Does `work` on the engine's threads, waiting for it `patience` at most:
/// the engine is `stuck` if it takes longer, and then fails at once.
async fn on_engine<T: Send + 'static>(
    patience: Duration,
    stuck: &AtomicBool,
    work: impl Future<Output = Result<T, String>> + Send + 'static,
) -> Result<T, String> {
    if stuck.load(Ordering::Relaxed) {
        return Err(STUCK.to_owned());
    }

    match tokio::time::timeout(patience, threads()?.spawn(work)).await {
        Ok(Ok(done)) => done,
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => {
            log::warn!("The BitTorrent engine took over {patience:?} to answer");
            stuck.store(true, Ordering::Relaxed);
            Err(STUCK.to_owned())
        }
    }
}

/// Notes how `handle` goes in `said`, every [`WATCHING`], while anyone
/// looks at it: what it says is read under its locks, which only the
/// engine's threads wait on.
async fn watch(handle: Arc<ManagedTorrent>, said: Weak<Mutex<Said>>) {
    while let Some(looking) = said.upgrade() {
        let told = lock(&looking).told;
        let progress = progress_of(&handle);
        {
            let mut said = lock(&looking);
            // Told other files meanwhile: this is of the ones before.
            if said.told == told {
                said.progress = progress;
            }
        }
        drop(looking);
        tokio::time::sleep(WATCHING).await;
    }
}

/// What a download said, locked: only ever to read or write it, so a
/// poisoned one holds what it did.
fn lock(said: &Mutex<Said>) -> MutexGuard<'_, Said> {
    said.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An error of librqbit's, with its causes.
fn failed(error: impl fmt::Display) -> String {
    format!("{error:#}")
}

/// How far `handle` got, now.
fn progress_of(handle: &ManagedTorrent) -> Progress {
    let stats = handle.stats();
    let live = stats.live.as_ref();

    Progress {
        checking: matches!(stats.state, TorrentStatsState::Initializing { .. }),
        done: stats.progress_bytes,
        total: stats.total_bytes,
        peers: live.map_or(0, |live| live.snapshot.peer_stats.live),
        connecting: live.map_or(0, |live| live.snapshot.peer_stats.connecting),
        down: live.map_or(0, |live| live.download_speed.as_bytes()),
        up: live.map_or(0, |live| live.upload_speed.as_bytes()),
        finished: stats.finished,
        error: stats.error,
    }
}

/// The bytes of the `.torrent` of `source`: read from its file, downloaded
/// with `client` from its address, or asked by `engine` of the peers sharing
/// a magnet link's torrent (and of `peers`), the engine started first if
/// there is none yet, or it stopped answering. The engine comes back, if
/// there is one.
pub async fn fetch(
    source: Source,
    engine: Option<Engine>,
    client: Result<reqwest::Client, String>,
    peers: Vec<SocketAddr>,
) -> Result<(Vec<u8>, Option<Engine>), String> {
    match source {
        Source::File(path) => {
            let bytes = tokio::fs::read(&path)
                .await
                .map_err(|error| format!("{}: {error}", path.display()))?;

            Ok((bytes, engine))
        }
        Source::Url(url) => {
            let response = client?
                .get(&url)
                .send()
                .await
                .and_then(reqwest::Response::error_for_status)
                .map_err(|error| crate::describe(&error))?;
            if response
                .content_length()
                .is_some_and(|length| length > LARGEST_TORRENT as u64)
            {
                return Err(format!("{url} is too big for a .torrent file"));
            }
            let bytes = response
                .bytes()
                .await
                .map_err(|error| crate::describe(&error))?;
            if bytes.len() > LARGEST_TORRENT {
                return Err(format!("{url} is too big for a .torrent file"));
            }

            Ok((bytes.to_vec(), engine))
        }
        Source::Magnet(link) => {
            let engine = match engine.filter(Engine::answers) {
                Some(engine) => engine,
                None => Engine::start().await?,
            };
            let bytes = engine.resolve(&link, peers).await?;

            Ok((bytes, Some(engine)))
        }
    }
}

impl Download {
    /// How far it got, as it said last: it is asked on the engine's
    /// threads, so this never waits on the engine.
    pub fn progress(&self) -> Progress {
        lock(&self.said).progress.clone()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;
    use std::time::Instant;

    use super::*;
    use crate::core::testing::torrent_of;
    use crate::core::torrent::Torrent;

    /// An engine on this computer alone (see [`localhost`]).
    pub(crate) async fn local() -> Engine {
        Engine::with(localhost).await.unwrap()
    }

    /// The options of a session on this computer alone: listening on
    /// localhost, with no DHT, trackers or local discovery; sharing, if it
    /// `shares`.
    fn localhost(shares: bool) -> SessionOptions {
        SessionOptions {
            dht: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            persistence: None,
            listen: Some(ListenerOptions {
                listen_addr: ([127, 0, 0, 1], 0).into(),
                ..ListenerOptions::default()
            }),
            disable_upload: !shares,
            ..SessionOptions::default()
        }
    }

    /// Where `engine` listens.
    pub(crate) fn address(engine: &Engine) -> SocketAddr {
        engine.session.listen_addr().unwrap()
    }

    /// Waits for `download` to have everything it downloads.
    pub(crate) async fn finished(download: &Download) -> Progress {
        for _ in 0..600 {
            let progress = download.progress();
            assert_eq!(progress.error, None);
            if progress.finished {
                return progress;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("not finished: {:?}", download.progress());
    }

    #[test]
    fn what_was_given_tells_where_the_torrent_comes_from() {
        assert_eq!(
            Source::parse(" magnet:?xt=urn:btih:abc "),
            Source::Magnet("magnet:?xt=urn:btih:abc".to_owned())
        );
        assert_eq!(
            Source::parse("https://example.com/client.torrent"),
            Source::Url("https://example.com/client.torrent".to_owned())
        );
        assert_eq!(
            Source::parse("file:///games/client.torrent"),
            Source::File("/games/client.torrent".into())
        );
    }

    /// On one thread, as the window has: a hang holds up the engine's own.
    #[tokio::test(flavor = "current_thread")]
    async fn an_engine_that_hangs_is_waited_for_so_long_and_then_not_at_all() {
        let stuck = AtomicBool::new(false);
        assert_eq!(on_engine(PATIENCE, &stuck, async { Ok(1) }).await, Ok(1));

        let asked = Instant::now();
        let hanging = async {
            // As a deadlock does: its thread waits, and never gives way.
            std::thread::sleep(Duration::from_secs(2));
            Ok(1)
        };
        assert_eq!(
            on_engine(Duration::from_millis(100), &stuck, hanging).await,
            Err(STUCK.to_owned())
        );
        assert!(asked.elapsed() < Duration::from_secs(1), "not waited out");
        assert!(stuck.load(Ordering::Relaxed));

        assert_eq!(
            on_engine(PATIENCE, &stuck, async { Ok(1) }).await,
            Err(STUCK.to_owned()),
            "stuck, it isn't asked again"
        );
    }

    #[test]
    fn torrents_of_more_pieces_than_librqbit_can_list_are_not_shared() {
        assert!(shareable(1));
        assert!(shareable(131_960));
        assert!(!shareable(131_961));
        assert!(!shareable(153_881), "the HD client's");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_torrent_downloads_from_a_peer_and_only_the_files_asked_for() {
        downloads_from_a_peer(true).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_torrent_it_cant_share_downloads_in_a_session_sharing_nothing() {
        downloads_from_a_peer(false).await;
    }

    /// A torrent downloads from a peer into a session sharing it if
    /// `shares`, Wow.exe first and then the rest, and is forgotten.
    async fn downloads_from_a_peer(shares: bool) {
        let files: Vec<(&str, &[u8])> = vec![
            ("Wow.exe", b"MZ, the game"),
            ("Data/common.MPQ", &[7; 40_000]),
            ("Data/enUS/locale-enUS.MPQ", b"words"),
        ];
        let bytes = torrent_of("WotLK", &files, 16 * 1024);
        let torrent = Torrent::parse(&bytes).unwrap();

        let seeding = tempfile::tempdir().unwrap();
        for (path, contents) in &files {
            let path = seeding.path().join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        let seeder = local().await;
        let seed = seeder
            .download(&bytes, seeding.path(), None, Vec::new())
            .await
            .unwrap();
        assert!(finished(&seed).await.finished, "it has everything already");

        let receiving = tempfile::tempdir().unwrap();
        let receiver = local().await;
        let download = receiver
            .download_in(
                shares,
                &bytes,
                receiving.path(),
                Some(vec![torrent.executable().unwrap()]),
                vec![address(&seeder)],
            )
            .await
            .unwrap();
        assert_eq!(Arc::ptr_eq(&download.session, &receiver.session), shares);
        finished(&download).await;
        assert_eq!(
            fs::read(receiving.path().join("Wow.exe")).unwrap(),
            b"MZ, the game"
        );

        receiver.select(&download, &[0, 1, 2]).await.unwrap();
        let progress = finished(&download).await;
        assert_eq!(progress.total, torrent.size());
        for (path, contents) in &files {
            assert_eq!(
                fs::read(receiving.path().join(path)).unwrap(),
                *contents,
                "{path}"
            );
        }

        receiver.forget(&download).await.unwrap();
        assert!(receiving.path().join("Wow.exe").exists(), "kept");
    }
}
