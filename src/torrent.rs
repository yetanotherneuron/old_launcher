//! The BitTorrent engine, librqbit: one session for the whole launcher,
//! started the first time a torrent is needed. It finds the torrent of a
//! link or a file, and downloads; checking a client folder against its
//! torrent is core's (`core::torrent::check`), and only reads.

use std::collections::HashSet;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, DhtSessionConfig, ListenerOptions,
    ManagedTorrent, Session, SessionOptions, TorrentStatsState,
};

/// How long a magnet link gets to find someone sharing its torrent.
const RESOLVING: Duration = Duration::from_secs(120);

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

/// The engine: a librqbit session.
#[derive(Clone)]
pub struct Engine {
    session: Arc<Session>,
}

impl fmt::Debug for Engine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Engine")
    }
}

/// A torrent being downloaded, or checked before it is.
#[derive(Clone)]
pub struct Download {
    handle: Arc<ManagedTorrent>,
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
    /// Starts the session. Peers are found through the trackers a torrent
    /// names and the DHT (whose state isn't kept between starts), and
    /// connected to. It listens for them too, on a port of the system's
    /// choosing that the router is asked to forward (UPnP), as games do, so
    /// others take pieces from it while it downloads: what it has, it
    /// shares.
    pub async fn start() -> Result<Self, String> {
        let options = SessionOptions {
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
            ..SessionOptions::default()
        };

        Self::with(options).await
    }

    async fn with(options: SessionOptions) -> Result<Self, String> {
        // librqbit's HTTP client needs rustls' provider, as the launcher's
        // does; installed already, this fails, which is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let session = Session::new_with_opts(std::env::temp_dir(), options)
            .await
            .map_err(|error| format!("{error:#}"))?;

        Ok(Self { session })
    }

    /// The bytes of the `.torrent` of a magnet `link`, asked of the peers
    /// sharing it: those found, and `peers`.
    async fn resolve(&self, link: &str, peers: Vec<SocketAddr>) -> Result<Vec<u8>, String> {
        let options = AddTorrentOptions {
            list_only: true,
            initial_peers: (!peers.is_empty()).then_some(peers),
            ..AddTorrentOptions::default()
        };
        let listed = self
            .session
            .add_torrent(AddTorrent::from_url(link), Some(options));

        match tokio::time::timeout(RESOLVING, listed).await {
            Ok(Ok(AddTorrentResponse::ListOnly(listed))) => Ok(listed.torrent_bytes.to_vec()),
            Ok(Ok(_)) => Err("the torrent was not listed".to_owned()),
            Ok(Err(error)) => Err(format!("{error:#}")),
            Err(_) => Err("nobody sharing it was found".to_owned()),
        }
    }

    /// Downloads `torrent` (its bytes) into `folder`, only the `files` by
    /// index when given. What the folder holds already is checked first and
    /// kept where it is right, so this resumes too. `peers` are connected to
    /// besides those found.
    pub async fn download(
        &self,
        torrent: &[u8],
        folder: &Path,
        files: Option<Vec<usize>>,
        peers: Vec<SocketAddr>,
    ) -> Result<Download, String> {
        let options = AddTorrentOptions {
            overwrite: true,
            output_folder: Some(folder.to_string_lossy().into_owned()),
            only_files: files,
            initial_peers: (!peers.is_empty()).then_some(peers),
            ..AddTorrentOptions::default()
        };
        let added = self
            .session
            .add_torrent(AddTorrent::from_bytes(torrent.to_vec()), Some(options))
            .await
            .map_err(|error| format!("{error:#}"))?;

        match added {
            AddTorrentResponse::Added(_, handle)
            | AddTorrentResponse::AlreadyManaged(_, handle) => Ok(Download { handle }),
            AddTorrentResponse::ListOnly(_) => Err("the torrent was only listed".to_owned()),
        }
    }

    /// Makes `download` download the `files`, by index, and no others.
    pub async fn select(&self, download: &Download, files: &[usize]) -> Result<(), String> {
        let files: HashSet<usize> = files.iter().copied().collect();

        self.session
            .update_only_files(&download.handle, &files)
            .await
            .map_err(|error| format!("{error:#}"))
    }

    /// Stops `download`, keeping what it downloaded, and shares nothing more.
    pub async fn forget(&self, download: &Download) -> Result<(), String> {
        self.session
            .delete(download.handle.id().into(), false)
            .await
            .map_err(|error| format!("{error:#}"))
    }
}

/// The bytes of the `.torrent` of `source`: read from its file, downloaded
/// with `client` from its address, or asked by `engine` of the peers sharing
/// a magnet link's torrent (and of `peers`), the engine started first if
/// there is none yet. The engine comes back, if there is one.
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
            let engine = match engine {
                Some(engine) => engine,
                None => Engine::start().await?,
            };
            let bytes = engine.resolve(&link, peers).await?;

            Ok((bytes, Some(engine)))
        }
    }
}

impl Download {
    /// How far it got, now.
    pub fn progress(&self) -> Progress {
        let stats = self.handle.stats();
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
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fs;

    use super::*;
    use crate::core::testing::torrent_of;
    use crate::core::torrent::Torrent;

    /// An engine on this computer alone: listening on localhost, with no
    /// DHT, trackers or local discovery.
    pub(crate) async fn local() -> Engine {
        let options = SessionOptions {
            dht: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            persistence: None,
            listen: Some(ListenerOptions {
                listen_addr: ([127, 0, 0, 1], 0).into(),
                ..ListenerOptions::default()
            }),
            ..SessionOptions::default()
        };

        Engine::with(options).await.unwrap()
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

    #[tokio::test(flavor = "multi_thread")]
    async fn a_torrent_downloads_from_a_peer_and_only_the_files_asked_for() {
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
            .download(
                &bytes,
                receiving.path(),
                Some(vec![torrent.executable().unwrap()]),
                vec![address(&seeder)],
            )
            .await
            .unwrap();
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
