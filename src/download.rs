//! Clients downloaded with BitTorrent: a new client from a torrent, the rest
//! of one whose download was cut short, and the updates of one, found by
//! comparing it with its torrent. One at a time, in a dialog that says how
//! it goes and can be hidden behind a banner while it goes on.
//!
//! Downloads go into a staging folder, `.download`, in the folder
//! their files go in, so they need room on that disk alone, and the files
//! move into place once complete, with no copying: a new client is added
//! then. The launcher keeps a copy of each client's `.torrent` in its cache,
//! named after its info hash, to check the client against later, and the client folder
//! notes where the torrent comes from, a `.torrent` file given as its magnet
//! link: with the copy gone, or the client added back, it is found again.
//! Any client can be given a torrent to update from, the same way.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::net::SocketAddr;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use iced::widget::{Row, button, checkbox, column, container, progress_bar, row, space, text};
use iced::{Center, Element, Fill, Function, Subscription, Task, Widget};

use crate::action::Action;
use crate::activity::activity;
use crate::core::config::ClientTorrent;
use crate::core::torrent::{Check, Torrent, client_in, folder_of_client, keep_origin, origin};
use crate::core::{Config, Installation};
use crate::errors::Errors;
use crate::torrent::{self, Download, Engine, Progress, Source};
use crate::{blocking, counted, describe, size, style};

/// How often a download or a check is asked how it goes.
const TICK: Duration = Duration::from_millis(500);

/// Why an update waits for the game.
const CLOSE_THE_GAME: &str = "Close the game first: an update replaces some of its files.";

/// How many of the files an update downloads it names.
const NAMED: usize = 8;

/// The staging folder a download goes into, in the folder its files go in.
const STAGING: &str = ".download";

/// The state: the engine, once started, and the work under way.
pub struct State {
    /// Where the copies of the clients' `.torrent` files are kept.
    store: PathBuf,
    /// The BitTorrent engine, started the first time it is needed.
    engine: Option<Engine>,
    job: Option<Job>,
    /// How many jobs there were. Each is told apart by its number, so what
    /// another one sends back is left alone.
    jobs: u64,
    /// The download about to be discarded, by info hash, pending a yes.
    discarding: Option<String>,
    /// Peers to download from besides those found: none, but in tests.
    peers: Vec<SocketAddr>,
}

/// What the launcher is asked to do.
#[derive(Debug, Clone)]
pub enum Request {
    /// Download the client of the torrent of `source`, into a folder named
    /// after the torrent in `into`.
    New { source: Source, into: PathBuf },
    /// Download the rest of the client being downloaded with this info hash.
    Resume(String),
    /// Compare the client at this folder with its torrent, to update it.
    Check(PathBuf),
    /// Update the client at `root` from the torrent of `source` from now
    /// on, and compare it with it.
    Adopt { root: PathBuf, source: Source },
}

/// The work under way.
struct Job {
    id: u64,
    kind: Kind,
    /// What the launcher keeps of the client's torrent: for a new client,
    /// from when the player says to download it.
    saved: Option<ClientTorrent>,
    /// The client folder being updated.
    root: Option<PathBuf>,
    /// The torrent, once found.
    found: Option<Found>,
    /// The folder it downloads into, once the torrent is known.
    staging: Option<PathBuf>,
    /// The engine's download, once it began.
    download: Option<Download>,
    /// The files it downloads, by index in the torrent.
    files: Vec<usize>,
    progress: Progress,
    stage: Stage,
    /// Whether the dialog is hidden, behind a banner.
    hidden: bool,
    /// Whether it was stopped while waiting for something, which stops it
    /// once that comes.
    stopping: bool,
}

#[derive(Debug, Clone)]
enum Kind {
    New { source: Source },
    Resume,
    Check,
}

enum Stage {
    /// Fetching or reading the torrent.
    Finding,
    /// Waiting for the engine to begin downloading, or to download all of
    /// a new client.
    Beginning,
    /// Downloading a new client's `Wow.exe`, to tell its version.
    Identifying,
    /// Reading it.
    Opening,
    /// Asking whether to download the new client, and in which of the
    /// `languages` its torrent holds, when several: those `chosen` so far.
    /// A torrent in one language or none leaves nothing to choose.
    Confirming {
        installation: Installation,
        languages: Vec<String>,
        chosen: BTreeSet<String>,
    },
    /// Comparing the client with its torrent: `read` says how far it got,
    /// and `stop` stops it.
    Checking {
        read: Arc<AtomicU64>,
        stop: Arc<AtomicBool>,
    },
    /// The client is as its torrent has it.
    Current,
    /// The client differs from its torrent.
    Outdated(Check),
    /// Copying the files that differ beside the download, for it to fetch
    /// only the pieces that do.
    Staging,
    /// Downloading; `pieces` is how many pieces hold the files.
    Downloading {
        pieces: usize,
    },
    /// Stopping sharing and moving the files into the client folder:
    /// `moved` says how far it got.
    Moving {
        moved: Arc<AtomicU64>,
    },
    Failed(String),
}

/// A torrent found.
#[derive(Debug, Clone)]
pub struct Found {
    bytes: Arc<Vec<u8>>,
    torrent: Arc<Torrent>,
    /// The folder its files go in once downloaded.
    folder: PathBuf,
    /// The engine, if finding the torrent took starting it.
    engine: Option<Engine>,
    /// What the launcher keeps of the client's torrent now, when that
    /// changed: a newer torrent, kept from a magnet link now, or one given.
    saved: Option<ClientTorrent>,
}

/// A download the engine began.
#[derive(Debug, Clone)]
pub struct Began {
    engine: Engine,
    download: Download,
}

/// Interactions with the dialog and the banners, and the work's progress.
#[derive(Debug, Clone)]
pub enum Message {
    /// The torrent of a job was found, or not.
    Found(u64, Result<Found, String>),
    /// The engine began downloading, or couldn't.
    Began(u64, Result<Began, String>),
    /// A new client's `Wow.exe` was read.
    Identified(u64, Result<Installation, String>),
    /// The new client's torrent was kept, and all of it asked for.
    Selected(u64, Result<(), String>),
    /// The client was compared with its torrent; `None` when stopped.
    Checked(u64, Result<Option<Check>, String>),
    /// The files that differ were copied beside the download.
    Staged(u64, Result<(), String>),
    /// The files downloaded are in the client folder: a new client's
    /// comes with them.
    Moved(u64, Result<Option<Installation>, String>),
    /// What a stopped job, or a download discarded, left was cleaned up.
    CleanedUp(Result<(), String>),
    /// Time to look at how it goes.
    Tick,
    /// "Resume download" was pressed in the banner of the download with
    /// this info hash.
    Resume(String),
    /// "Discard" was pressed in that banner: ask first.
    Discard(String),
    /// Discarding it was confirmed.
    ConfirmDiscard(String),
    /// Discarding it was thought better of.
    KeepDownload,
    /// A language of the new client was ticked, or unticked.
    Language(String, bool),
    /// "Download" was pressed: get the new client.
    Download,
    /// "Update" was pressed.
    Update,
    /// "Stop", "Cancel", "Not now" or "Close" was pressed.
    Stop,
    /// "Hide" was pressed, or the dialog dismissed: it goes on behind a
    /// banner.
    Hide,
    /// "Show" was pressed in the banner.
    Show,
}

/// What the dialog asks of its parent.
pub enum Instruction {
    /// A new client is being downloaded from this torrent: keep it, to
    /// resume.
    Pending(ClientTorrent),
    /// A new client was downloaded: add it, with its torrent.
    Added {
        installation: Installation,
        torrent: ClientTorrent,
        notice: String,
    },
    /// The client at this folder has a newer torrent.
    Saved(PathBuf, ClientTorrent),
    /// The client at `root` was updated, as `notice` says.
    Updated { root: PathBuf, notice: String },
    /// The download with this info hash was discarded.
    Discarded(String),
}

/// What is left to do after a job stops.
enum After {
    Nothing,
    /// Remove what was downloaded, in this staging folder, and the folder
    /// it is in if that leaves it empty.
    Remove(PathBuf),
}

impl State {
    /// Nothing under way; the `.torrent` copies are kept in `store`.
    pub fn new(store: PathBuf) -> Self {
        Self {
            store,
            engine: None,
            job: None,
            jobs: 0,
            discarding: None,
            peers: Vec::new(),
        }
    }

    /// Starts doing what is asked, for the clients of `config`, fetching
    /// with `client`; unless something else is under way.
    pub fn start(
        &mut self,
        request: Request,
        config: &Config,
        client: Result<&reqwest::Client, &str>,
    ) -> Task<Message> {
        if self.is_running() {
            return Task::none();
        }
        self.jobs += 1;
        let id = self.jobs;
        let client = client.cloned().map_err(str::to_owned);

        let (kind, saved, root, task) = match request {
            Request::New { source, into } => {
                log::info!("Finding the torrent of {source}");
                let task = Task::perform(
                    find_new(
                        source.clone(),
                        into,
                        self.engine.clone(),
                        client,
                        self.peers.clone(),
                    ),
                    Message::Found.with(id),
                );
                (Kind::New { source }, None, None, task)
            }
            Request::Resume(info_hash) => {
                let Some(saved) = config
                    .downloads
                    .iter()
                    .find(|saved| saved.info_hash == info_hash)
                    .cloned()
                else {
                    return Task::none();
                };
                log::info!("Resuming the download of {}", saved.name);
                let task = Task::perform(
                    find_kept(
                        saved.clone(),
                        self.store.clone(),
                        self.engine.clone(),
                        self.peers.clone(),
                    ),
                    Message::Found.with(id),
                );
                (Kind::Resume, Some(saved), None, task)
            }
            Request::Check(root) => {
                let Some(saved) = config.torrents.get(&root).cloned() else {
                    return Task::none();
                };
                log::info!("Checking {} for updates", root.display());
                let task = Task::perform(
                    find_again(
                        saved.clone(),
                        root.clone(),
                        self.store.clone(),
                        self.engine.clone(),
                        client,
                        self.peers.clone(),
                    ),
                    Message::Found.with(id),
                );
                (Kind::Check, Some(saved), Some(root), task)
            }
            Request::Adopt { root, source } => {
                log::info!(
                    "Finding the torrent of {source}, to update {} from",
                    root.display()
                );
                let task = Task::perform(
                    find_given(
                        source,
                        root.clone(),
                        self.store.clone(),
                        self.engine.clone(),
                        client,
                        self.peers.clone(),
                    ),
                    Message::Found.with(id),
                );
                (Kind::Check, None, Some(root), task)
            }
        };

        self.job = Some(Job {
            id,
            kind,
            saved,
            root,
            found: None,
            staging: None,
            download: None,
            files: Vec::new(),
            progress: Progress::default(),
            stage: Stage::Finding,
            hidden: false,
            stopping: false,
        });

        task
    }

    /// Deletes what a new client not yet said yes to, or turned down,
    /// downloaded: on the way out of the launcher, which runs no task
    /// then. A client being downloaded, and an update, keep theirs to
    /// resume.
    pub fn abandon(&mut self) {
        let Some(job) = self.job.take() else {
            return;
        };
        let After::Remove(staging) = job.after() else {
            return;
        };

        log::info!(
            "Deleting {}: the client wasn't downloaded",
            staging.display()
        );
        match fs::remove_dir_all(&staging) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                log::warn!("Could not delete {}: {error}", staging.display());
            }
            _ => {}
        }
        // The folder made for it, if nothing else is there.
        if let Some(folder) = staging.parent() {
            let _ = fs::remove_dir(folder);
        }
    }

    /// Where the copy of the torrent with `info_hash` is kept.
    fn kept(&self, info_hash: &str) -> PathBuf {
        kept_in(&self.store, info_hash)
    }

    /// Where the copy of the torrent with `info_hash` is kept, unless a
    /// client of `config` is checked against it: discarding a download of
    /// it deletes the copy then, and leaves it otherwise.
    fn kept_unless_shared(&self, config: &Config, info_hash: &str) -> Option<PathBuf> {
        let shared = config
            .torrents
            .values()
            .any(|saved| saved.info_hash == info_hash);

        (!shared).then(|| self.kept(info_hash))
    }

    /// Whether something is under way, or waits for the player: another
    /// job can't start.
    pub fn is_running(&self) -> bool {
        self.job
            .as_ref()
            .is_some_and(|job| !matches!(job.stage, Stage::Current | Stage::Failed(_)))
    }

    /// Whether the dialog shows.
    pub fn is_open(&self) -> bool {
        self.job.as_ref().is_some_and(|job| !job.hidden)
    }

    /// The folder of the client being updated.
    pub fn root(&self) -> Option<&Path> {
        self.job.as_ref()?.root.as_deref()
    }

    /// Whether starting the game of the client at `root` waits for its
    /// update to put its files in place.
    pub fn holds_back(&self, root: &Path) -> bool {
        self.job.as_ref().is_some_and(|job| {
            job.root.as_deref() == Some(root) && matches!(job.stage, Stage::Moving { .. })
        })
    }

    /// What is being done that quitting would cut short, for people.
    pub fn under_way(&self) -> Option<String> {
        let job = self.job.as_ref()?;

        match job.stage {
            Stage::Identifying | Stage::Opening => Some(format!(
                "Downloading Wow.exe of {}, to check its version",
                job.name()
            )),
            Stage::Staging | Stage::Beginning | Stage::Downloading { .. } => Some(format!(
                "{} {}: {}",
                job.verb(),
                job.name(),
                percent(&job.progress)
            )),
            Stage::Moving { .. } => Some(format!(
                "Putting the files of {} in place: {}",
                job.name(),
                percent(&job.progress)
            )),
            _ => None,
        }
    }

    /// Stops the job of the client at `root`, which the launcher forgets.
    pub fn forget(&mut self, root: &Path) -> Task<Message> {
        if self.root() != Some(root) {
            return Task::none();
        }

        self.stop()
    }

    /// Handles an interaction, for the clients of `config`, fetching with
    /// `client`; `playing` says whether the game of the client being
    /// updated runs. What a stopped job leaves that can't be cleaned up
    /// goes to `errors`.
    pub fn update(
        &mut self,
        message: Message,
        config: &Config,
        client: Result<&reqwest::Client, &str>,
        playing: bool,
        errors: &mut Errors,
    ) -> Action<Instruction, Message> {
        match message {
            Message::Found(id, found) => {
                // An engine started is kept, whoever started it.
                if let Ok(Found {
                    engine: Some(engine),
                    ..
                }) = &found
                {
                    self.engine = Some(engine.clone());
                }
                let Some(job) = self.job.as_mut().filter(|job| job.id == id) else {
                    return Action::none();
                };
                let found = match found {
                    Ok(found) => found,
                    Err(error) => return Action::task(self.fail(error)),
                };
                let torrent = found.torrent.clone();
                let folder = found.folder.clone();
                let staging = folder.join(STAGING);

                let saved = match (&found.saved, &job.root) {
                    (Some(saved), Some(root)) => {
                        job.saved = Some(saved.clone());
                        Some(Instruction::Saved(root.clone(), saved.clone()))
                    }
                    _ => None,
                };
                let bytes = found.bytes.clone();
                job.found = Some(found);
                job.staging = Some(staging.clone());

                let task = match job.kind {
                    Kind::New { .. } | Kind::Resume => {
                        job.files = if matches!(job.kind, Kind::New { .. }) {
                            log::info!(
                                "Downloading Wow.exe of {} into {}, to check its version",
                                torrent.name,
                                staging.display()
                            );
                            torrent.executable().into_iter().collect()
                        } else {
                            // In the languages chosen when it began.
                            let languages = job
                                .saved
                                .as_ref()
                                .map_or(&[][..], |saved| saved.languages.as_slice());
                            torrent.files_in(languages)
                        };
                        job.stage = Stage::Beginning;
                        let files = job.files.clone();
                        self.begin(id, bytes, staging, files)
                    }
                    Kind::Check => {
                        let read = Arc::new(AtomicU64::new(0));
                        let stop = Arc::new(AtomicBool::new(false));
                        job.progress = Progress {
                            total: torrent.files.iter().map(|file| file.length).sum(),
                            ..Progress::default()
                        };
                        job.stage = Stage::Checking {
                            read: read.clone(),
                            stop: stop.clone(),
                        };
                        Task::perform(
                            blocking(move || {
                                crate::core::torrent::check(&folder, &torrent, &mut |bytes| {
                                    read.store(bytes, Ordering::Relaxed);
                                    if stop.load(Ordering::Relaxed) {
                                        ControlFlow::Break(())
                                    } else {
                                        ControlFlow::Continue(())
                                    }
                                })
                            }),
                            Message::Checked.with(id),
                        )
                    }
                };

                Action {
                    instruction: saved,
                    task,
                }
            }
            Message::Began(id, began) => {
                let Some(job) = self.job.as_mut().filter(|job| job.id == id) else {
                    // Its job is gone: so goes the download.
                    return Action::task(match began {
                        Ok(began) => {
                            clean_up(Some(began.engine), Some(began.download), After::Nothing)
                        }
                        Err(_) => Task::none(),
                    });
                };
                match began {
                    Ok(began) => {
                        self.engine = Some(began.engine);
                        job.download = Some(began.download);
                        if job.stopping {
                            return Action::task(self.wind_down());
                        }
                        job.stage = match (&job.kind, &job.found) {
                            (Kind::New { .. }, _) => Stage::Identifying,
                            (_, Some(found)) => Stage::Downloading {
                                pieces: found.torrent.pieces_of(&job.files),
                            },
                            (_, None) => Stage::Downloading { pieces: 0 },
                        };
                        Action::none()
                    }
                    Err(error) if job.stopping => {
                        log::debug!("Stopped before beginning: {error}");
                        Action::task(self.wind_down())
                    }
                    Err(error) => Action::task(self.fail(error)),
                }
            }
            Message::Tick => {
                let Some(job) = &mut self.job else {
                    return Action::none();
                };
                match &job.stage {
                    Stage::Checking { read, .. } => {
                        job.progress.done = read.load(Ordering::Relaxed);
                        return Action::none();
                    }
                    Stage::Moving { moved } => {
                        job.progress.done = moved.load(Ordering::Relaxed);
                        return Action::none();
                    }
                    Stage::Identifying | Stage::Downloading { .. } => {}
                    _ => return Action::none(),
                }
                let Some(download) = &job.download else {
                    return Action::none();
                };

                job.progress = download.progress();
                if let Some(error) = job.progress.error.clone() {
                    return Action::task(self.fail(error));
                }
                if !job.progress.finished {
                    return Action::none();
                }
                let (Some(found), Some(engine), Some(staging)) =
                    (&job.found, &self.engine, &job.staging)
                else {
                    return Action::none();
                };

                let id = job.id;
                if matches!(job.stage, Stage::Identifying) {
                    job.stage = Stage::Opening;
                    let root = within(staging, &found.torrent.client().unwrap_or_default());
                    return Action::task(Task::perform(
                        blocking(move || Installation::open(root)),
                        Message::Identified.with(id),
                    ));
                }
                // Its files can't change under the game.
                if matches!(job.kind, Kind::Check) && playing {
                    return Action::none();
                }
                // Stopped by `finish`, so that stopping or failing afterwards
                // finds no download to stop again.
                let Some(download) = job.download.take() else {
                    return Action::none();
                };

                log::info!(
                    "Downloaded {}: putting its files in {}",
                    job.name(),
                    found.folder.display()
                );
                let moved = Arc::new(AtomicU64::new(0));
                let files = job.files.clone();
                job.progress = Progress {
                    total: files
                        .iter()
                        .filter_map(|index| found.torrent.files.get(*index))
                        .filter(|file| !file.padding)
                        .map(|file| file.length)
                        .sum(),
                    ..Progress::default()
                };
                job.stage = Stage::Moving {
                    moved: moved.clone(),
                };
                let new = job
                    .saved
                    .clone()
                    .filter(|_| !matches!(job.kind, Kind::Check));

                Action::task(Task::perform(
                    finish(
                        engine.clone(),
                        download,
                        staging.clone(),
                        found.folder.clone(),
                        found.torrent.clone(),
                        files,
                        moved,
                        new,
                    ),
                    Message::Moved.with(id),
                ))
            }
            Message::Identified(id, installation) => {
                let Some(job) = self.job.as_mut().filter(|job| job.id == id) else {
                    return Action::none();
                };
                if job.stopping {
                    return Action::task(self.wind_down());
                }

                match installation {
                    Ok(installation) => {
                        let languages = job
                            .found
                            .as_ref()
                            .map_or_else(Vec::new, |found| found.torrent.languages());
                        log::info!(
                            "{} holds a client, version {}, in {}: asking whether to download it",
                            job.name(),
                            installation.version(),
                            if languages.is_empty() {
                                "no language".to_owned()
                            } else {
                                languages.join(", ")
                            }
                        );
                        // One language takes no choosing; several do.
                        let chosen = if languages.len() > 1 {
                            BTreeSet::new()
                        } else {
                            languages.iter().cloned().collect()
                        };
                        job.stage = Stage::Confirming {
                            installation,
                            languages,
                            chosen,
                        };
                        Action::none()
                    }
                    Err(error) => Action::task(self.fail(format!(
                        "This is not a client the launcher supports: {error}"
                    ))),
                }
            }
            Message::Language(language, ticked) => {
                let Some(job) = &mut self.job else {
                    return Action::none();
                };
                if let Stage::Confirming {
                    languages, chosen, ..
                } = &mut job.stage
                    && languages.contains(&language)
                {
                    if ticked {
                        let _ = chosen.insert(language);
                    } else {
                        let _ = chosen.remove(&language);
                    }
                }
                Action::none()
            }
            Message::Download => {
                let Some(job) = &mut self.job else {
                    return Action::none();
                };
                let (
                    Stage::Confirming {
                        languages, chosen, ..
                    },
                    Kind::New { source },
                    Some(found),
                ) = (&job.stage, &job.kind, &job.found)
                else {
                    return Action::none();
                };
                // Several languages, none chosen: nothing to download yet.
                if languages.len() > 1 && chosen.is_empty() {
                    return Action::none();
                }
                let torrent = found.torrent.clone();
                // What the player chose among several; all of one.
                let languages: Vec<String> = if languages.len() > 1 {
                    chosen.iter().cloned().collect()
                } else {
                    Vec::new()
                };
                let files = torrent.files_in(&languages);
                let saved = ClientTorrent {
                    name: torrent.name.clone(),
                    info_hash: torrent.info_hash.clone(),
                    source: kept_source(source, &torrent),
                    folder: found.folder.clone(),
                    languages: languages.clone(),
                };
                log::info!(
                    "Downloading {}, {}{}, for {}",
                    torrent.name,
                    size(torrent.size_of(&files)),
                    if languages.is_empty() {
                        String::new()
                    } else {
                        format!(" in {}", languages.join(", "))
                    },
                    found.folder.display()
                );
                let pending = Instruction::Pending(saved.clone());
                job.saved = Some(saved);
                job.files = files;
                // Until the engine was told, it says Wow.exe is all done.
                job.stage = Stage::Beginning;

                let kept = kept_in(&self.store, &torrent.info_hash);
                let (bytes, files) = (found.bytes.clone(), job.files.clone());
                let (engine, download) = (self.engine.clone(), job.download.clone());
                let task = Task::perform(
                    async move {
                        keep_copy(kept, bytes).await?;
                        match (engine, download) {
                            (Some(engine), Some(download)) => {
                                engine.select(&download, &files).await
                            }
                            _ => Err("the download stopped".to_owned()),
                        }
                    },
                    Message::Selected.with(job.id),
                );

                Action::new(pending, task)
            }
            Message::Selected(id, selected) => {
                let Some(job) = self.job.as_mut().filter(|job| job.id == id) else {
                    return Action::none();
                };
                if job.stopping {
                    return Action::task(self.wind_down());
                }

                match (selected, &job.found) {
                    (Ok(()), Some(found)) => {
                        job.stage = Stage::Downloading {
                            pieces: found.torrent.pieces_of(&job.files),
                        };
                        Action::none()
                    }
                    (Ok(()), None) => Action::none(),
                    (Err(error), _) => Action::task(self.fail(error)),
                }
            }
            Message::Checked(id, check) => {
                let Some(job) = self.job.as_mut().filter(|job| job.id == id) else {
                    return Action::none();
                };
                // Finished before it heard it was stopped.
                if job.stopping {
                    log::info!("Stopped checking {}", job.name());
                    self.job = None;
                    return Action::none();
                }

                match check {
                    Ok(None) => {
                        log::info!("Stopped checking {}", job.name());
                        self.job = None;
                        Action::none()
                    }
                    Ok(Some(check)) if check.is_current() => {
                        log::info!("{} is as its torrent has it", job.name());
                        job.stage = Stage::Current;
                        // What an update cut short downloaded is of no use.
                        let left = job.staging.clone();
                        Action::task(left.map_or_else(Task::none, |staging| {
                            clean_up(None, None, After::Remove(staging))
                        }))
                    }
                    Ok(Some(check)) => {
                        log::info!(
                            "{} differs from its torrent: {} missing, {} different",
                            job.name(),
                            counted(check.missing.len(), "file"),
                            counted(check.different.len(), "file")
                        );
                        job.stage = Stage::Outdated(check);
                        Action::none()
                    }
                    Err(error) => {
                        Action::task(self.fail(format!("Could not check the client: {error}")))
                    }
                }
            }
            Message::Update => {
                let Some(job) = &mut self.job else {
                    return Action::none();
                };
                let (Stage::Outdated(check), Some(found), Some(staging)) =
                    (&job.stage, &job.found, &job.staging)
                else {
                    return Action::none();
                };
                if playing {
                    return Action::none();
                }

                log::info!(
                    "Updating {}: {}, {}",
                    job.name(),
                    counted(check.files().len(), "file"),
                    size(check.bytes(&found.torrent))
                );
                job.files = check.files();
                job.progress = Progress::default();
                let (staging, folder, torrent, files) = (
                    staging.clone(),
                    found.folder.clone(),
                    found.torrent.clone(),
                    job.files.clone(),
                );
                job.stage = Stage::Staging;

                Action::task(Task::perform(
                    blocking(move || {
                        crate::core::torrent::stage(&staging, &folder, &torrent, &files)
                    }),
                    Message::Staged.with(job.id),
                ))
            }
            Message::Staged(id, staged) => {
                let Some(job) = self.job.as_mut().filter(|job| job.id == id) else {
                    return Action::none();
                };
                if job.stopping {
                    return Action::task(self.wind_down());
                }
                if let Err(error) = staged {
                    return Action::task(
                        self.fail(format!("Could not ready the files to update: {error}")),
                    );
                }
                let (Some(found), Some(staging)) = (&job.found, &job.staging) else {
                    return Action::none();
                };

                job.stage = Stage::Beginning;
                let (bytes, staging, files) =
                    (found.bytes.clone(), staging.clone(), job.files.clone());
                Action::task(self.begin(id, bytes, staging, files))
            }
            Message::Moved(id, moved) => {
                if !self.job.as_ref().is_some_and(|job| job.id == id) {
                    return Action::none();
                }
                let installation = match moved {
                    Ok(installation) => installation,
                    Err(error) => {
                        return Action::task(
                            self.fail(format!("Could not put the files in place: {error}")),
                        );
                    }
                };
                let Some(job) = self.job.take() else {
                    return Action::none();
                };

                match (installation, job.saved.clone(), job.root.clone()) {
                    (Some(installation), Some(saved), _) => {
                        let notice = format!("Downloaded {}: it is ready to play.", job.name());
                        log::info!("{notice}");
                        // The folder above the client as it is named now.
                        let client = job
                            .found
                            .as_ref()
                            .and_then(|found| found.torrent.client())
                            .unwrap_or_default();
                        let folder = folder_of_client(installation.root(), &client);

                        Action::instruction(Instruction::Added {
                            installation,
                            torrent: ClientTorrent { folder, ..saved },
                            notice,
                        })
                    }
                    (None, _, Some(root)) => {
                        let notice = format!(
                            "Updated {}: {} downloaded.",
                            job.name(),
                            counted(job.files.len(), "file")
                        );
                        log::info!("{notice}");
                        Action::instruction(Instruction::Updated { root, notice })
                    }
                    _ => Action::none(),
                }
            }
            Message::CleanedUp(result) => {
                if let Err(error) = result {
                    errors.report(format!("Could not tidy up after a download: {error}"));
                }
                Action::none()
            }
            Message::Resume(info_hash) => {
                self.discarding = None;
                Action::task(self.start(Request::Resume(info_hash), config, client))
            }
            Message::Discard(info_hash) => {
                self.discarding = Some(info_hash);
                Action::none()
            }
            Message::KeepDownload => {
                self.discarding = None;
                Action::none()
            }
            Message::ConfirmDiscard(info_hash) => {
                self.discarding = None;
                let running = self.job.as_ref().is_some_and(|job| {
                    job.saved
                        .as_ref()
                        .is_some_and(|saved| saved.info_hash == info_hash)
                });
                let Some(pending) = config
                    .downloads
                    .iter()
                    .find(|pending| pending.info_hash == info_hash)
                else {
                    return Action::none();
                };
                if running {
                    return Action::none();
                }
                log::info!("Discarding the download of {}", pending.name);

                let staging = pending.folder.join(STAGING);
                let kept = self.kept_unless_shared(config, &info_hash);
                Action::new(
                    Instruction::Discarded(info_hash),
                    Task::perform(
                        async move {
                            tidy(None, None, After::Remove(staging)).await?;
                            let Some(kept) = kept else {
                                return Ok(());
                            };
                            match tokio::fs::remove_file(&kept).await {
                                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                                    Err(format!("{}: {error}", kept.display()))
                                }
                                _ => Ok(()),
                            }
                        },
                        Message::CleanedUp,
                    ),
                )
            }
            Message::Stop => Action::task(self.stop()),
            Message::Hide => {
                if let Some(job) = &mut self.job {
                    job.hidden = true;
                }
                Action::none()
            }
            Message::Show => {
                if let Some(job) = &mut self.job {
                    job.hidden = false;
                }
                Action::none()
            }
        }
    }

    /// Begins downloading the `files` of the job `id`'s torrent (its
    /// `bytes`) into `staging`.
    fn begin(
        &self,
        id: u64,
        bytes: Arc<Vec<u8>>,
        staging: PathBuf,
        files: Vec<usize>,
    ) -> Task<Message> {
        Task::perform(
            begin(
                self.engine.clone(),
                bytes,
                staging,
                files,
                self.peers.clone(),
            ),
            Message::Began.with(id),
        )
    }

    /// Stops the job: at once, unless it waits for something, which stops
    /// it when it comes. Its download stops; what a new client turned down
    /// or an update downloaded goes, and a client being downloaded resumes
    /// later.
    fn stop(&mut self) -> Task<Message> {
        let Some(job) = &mut self.job else {
            return Task::none();
        };
        // What is fetched is left unread: nothing was made yet.
        let waiting = match job.stage {
            Stage::Beginning | Stage::Opening | Stage::Staging | Stage::Moving { .. } => true,
            Stage::Checking { ref stop, .. } => {
                stop.store(true, Ordering::Relaxed);
                true
            }
            Stage::Finding
            | Stage::Identifying
            | Stage::Downloading { .. }
            | Stage::Confirming { .. }
            | Stage::Current
            | Stage::Outdated(_)
            | Stage::Failed(_) => false,
        };
        if waiting {
            job.stopping = true;
            return Task::none();
        }

        self.wind_down()
    }

    /// Stops the job at once, cleaning up after it as [`State::stop`] says.
    fn wind_down(&mut self) -> Task<Message> {
        let Some(job) = self.job.take() else {
            return Task::none();
        };
        if !matches!(job.stage, Stage::Current | Stage::Failed(_)) {
            log::info!("Stopped {}", job.name());
        }
        let after = job.after();
        clean_up(self.engine.clone(), job.download, after)
    }

    /// Fails the job, saying why, and cleans up as stopping it would.
    fn fail(&mut self, error: String) -> Task<Message> {
        let Some(job) = &mut self.job else {
            return Task::none();
        };
        log::warn!("{}: {error}", job.name());
        // The player stopped it already: it just stops.
        if job.stopping {
            return self.wind_down();
        }

        let task = clean_up(self.engine.clone(), job.download.take(), job.after());
        // Cleaned up once.
        job.staging = None;
        job.stage = Stage::Failed(error);

        task
    }

    /// Looks at how the work goes, while there is something to see.
    pub fn subscription(&self) -> Subscription<Message> {
        let ticking = self.job.as_ref().is_some_and(|job| {
            matches!(
                job.stage,
                Stage::Identifying
                    | Stage::Downloading { .. }
                    | Stage::Checking { .. }
                    | Stage::Moving { .. }
            )
        });

        if ticking {
            iced::time::every(TICK).map(|_| Message::Tick)
        } else {
            Subscription::none()
        }
    }

    /// The banners above the addons: what goes on while the dialog is
    /// hidden, and the clients of `config` being downloaded but not now.
    pub fn banner(&self, config: &Config) -> Option<Element<'_, Message>> {
        let hidden = self.job.as_ref().filter(|job| job.hidden).map(|job| {
            banner(
                job.summary(),
                row![button("Show").style(style::quiet).on_press(Message::Show)],
                style::Tone::Primary,
            )
        });

        let pending = config
            .downloads
            .iter()
            .filter(|saved| {
                !self.job.as_ref().is_some_and(|job| {
                    job.saved
                        .as_ref()
                        .is_some_and(|working| working.info_hash == saved.info_hash)
                })
            })
            .map(|saved| {
                let hash = &saved.info_hash;
                if self.discarding.as_ref() == Some(hash) {
                    return banner(
                        format!(
                            "Delete what was downloaded of {}? It can't be resumed then.",
                            saved.name
                        ),
                        row![
                            button("Keep it")
                                .style(style::quiet)
                                .on_press(Message::KeepDownload),
                            button("Delete")
                                .style(style::danger)
                                .on_press(Message::ConfirmDiscard(hash.clone())),
                        ],
                        style::Tone::Danger,
                    );
                }

                banner(
                    format!("{} is not fully downloaded yet.", saved.name),
                    row![
                        button("Discard")
                            .style(style::quiet)
                            .on_press(Message::Discard(hash.clone())),
                        button("Resume download")
                            .style(style::primary)
                            .on_press_maybe(
                                (!self.is_running()).then(|| Message::Resume(hash.clone())),
                            ),
                    ],
                    style::Tone::Warning,
                )
            });

        let banners: Vec<Element<'_, Message>> = hidden.into_iter().chain(pending).collect();
        (!banners.is_empty()).then(|| column(banners).spacing(14).boxed())
    }

    /// The dialog, while it shows; `playing` says whether the game of the
    /// client being updated runs.
    pub fn dialog(&self, playing: bool) -> Option<impl Widget<Message> + '_> {
        let job = self.job.as_ref().filter(|job| !job.hidden)?;
        let found = job.found.as_ref();

        let hide = button("Hide").style(style::quiet).on_press(Message::Hide);
        let stop = |label: &'static str| {
            button(label)
                .style(style::secondary)
                .on_press(Message::Stop)
        };

        let (body, buttons): (Element<'_, Message>, Row<Element<'_, Message>>) = if job.stopping {
            (waiting("Stopping…"), row![])
        } else {
            match &job.stage {
                Stage::Finding => (waiting("Finding the torrent…"), row![stop("Stop")]),
                Stage::Beginning => (waiting("Connecting…"), row![hide, stop("Stop")]),
                Stage::Identifying => (
                    column![
                        text("Downloading Wow.exe, to check this is a 3.3.5a client."),
                        transfer(job, None),
                    ]
                    .spacing(10)
                    .boxed(),
                    row![hide, stop("Stop")],
                ),
                Stage::Opening => (waiting("Reading Wow.exe…"), row![stop("Stop")]),
                Stage::Confirming {
                    installation,
                    languages,
                    chosen,
                } => {
                    let torrent = found.map(|found| &*found.torrent);
                    let choosing = languages.len() > 1;
                    // What downloads: with several languages and none chosen
                    // yet, what the languages add to.
                    let picked: Vec<String> = if choosing {
                        chosen.iter().cloned().collect()
                    } else {
                        Vec::new()
                    };
                    let files = torrent.map_or_else(Vec::new, |torrent| {
                        if choosing && picked.is_empty() {
                            torrent.files_in(&[String::new()])
                        } else {
                            torrent.files_in(&picked)
                        }
                    });
                    let bytes = torrent.map_or(0, |torrent| torrent.size_of(&files));
                    let count = torrent.map_or(0, |torrent| {
                        files
                            .iter()
                            .filter(|index| !torrent.files[**index].padding)
                            .count()
                    });
                    let folder = found.map_or_else(String::new, |found| {
                        within(&found.folder, &found.torrent.client().unwrap_or_default())
                            .display()
                            .to_string()
                    });
                    let choice = choosing.then(|| {
                        let boxes = languages.iter().map(|language| {
                            let bytes = torrent
                                .map_or(0, |torrent| torrent.size_of(&torrent.files_of(language)));
                            let language = language.clone();
                            checkbox(chosen.contains(&language))
                                .label(format!("{language} ({})", size(bytes)))
                                .text_size(14)
                                .on_toggle(move |ticked| {
                                    Message::Language(language.clone(), ticked)
                                })
                                .style(style::check)
                                .boxed()
                        });

                        row![
                            text("Languages").size(14).width(100).style(text::secondary),
                            column(boxes).spacing(4),
                        ]
                        .spacing(12)
                    });
                    let download = button(text(format!("Download {}", size(bytes))))
                        .style(style::primary)
                        .on_press_maybe(
                            (!(choosing && picked.is_empty())).then_some(Message::Download),
                        );

                    (
                        column![
                            text("This torrent holds a client the launcher supports."),
                            fact("Version", installation.version().to_string()),
                            choice,
                            fact("Size", size(bytes)),
                            fact("Files", counted(count, "file")),
                            fact("Folder", folder),
                        ]
                        .spacing(6)
                        .boxed(),
                        row![stop("Cancel"), download],
                    )
                }
                Stage::Checking { .. } => (
                    column![
                        text("Comparing the client's files with its torrent…"),
                        bar(&job.progress),
                    ]
                    .spacing(10)
                    .boxed(),
                    row![hide, stop("Stop")],
                ),
                Stage::Current => (
                    text("Every file is as its torrent has it: there is nothing to download.")
                        .boxed(),
                    row![stop("Close")],
                ),
                Stage::Outdated(check) => {
                    let update = button("Update").style(style::primary);
                    let update: Element<'_, Message> = if playing {
                        style::tip(update, CLOSE_THE_GAME).boxed()
                    } else {
                        update.on_press(Message::Update).boxed()
                    };

                    (outdated(check, found), row![stop("Not now"), update])
                }
                Stage::Staging => (waiting("Readying the files to update…"), row![]),
                Stage::Downloading { .. }
                    if job.progress.finished && playing && matches!(job.kind, Kind::Check) =>
                {
                    (
                        text("Downloaded. The files go in place once the game is closed.").boxed(),
                        row![hide, stop("Stop")],
                    )
                }
                Stage::Downloading { pieces } => {
                    (transfer(job, Some(*pieces)), row![hide, stop("Stop")])
                }
                Stage::Moving { .. } => (
                    column![text("Putting the files in place…"), bar(&job.progress)]
                        .spacing(10)
                        .boxed(),
                    row![hide],
                ),
                Stage::Failed(error) => (
                    text(error.as_str()).style(text::danger).boxed(),
                    row![stop("Close")],
                ),
            }
        };

        Some(
            container(
                column![
                    style::heading(job.title(), 20.0),
                    body,
                    row![space::horizontal(), buttons.spacing(8)].align_y(Center),
                ]
                .spacing(14),
            )
            .padding(24)
            .width(Fill.max(620))
            .style(style::card),
        )
    }
}

impl Job {
    /// What is downloaded, for people.
    fn name(&self) -> &str {
        self.saved
            .as_ref()
            .map(|saved| saved.name.as_str())
            .or_else(|| self.found.as_ref().map(|found| found.torrent.name.as_str()))
            .or_else(|| self.root.as_ref()?.file_name()?.to_str())
            .unwrap_or("the torrent")
    }

    /// What the job does to the client, for people.
    fn verb(&self) -> &'static str {
        match self.kind {
            Kind::New { .. } | Kind::Resume => "Downloading",
            Kind::Check => "Updating",
        }
    }

    /// The dialog's title.
    fn title(&self) -> String {
        match (&self.kind, &self.found) {
            (Kind::New { .. }, None) => "A client from a torrent".to_owned(),
            (Kind::Check, _)
                if !matches!(
                    self.stage,
                    Stage::Staging
                        | Stage::Beginning
                        | Stage::Downloading { .. }
                        | Stage::Moving { .. }
                ) =>
            {
                format!("Updates for {}", self.name())
            }
            _ => self.name().to_owned(),
        }
    }

    /// How it goes, in a line, for the banner.
    fn summary(&self) -> String {
        match &self.stage {
            Stage::Finding | Stage::Opening => format!("Looking at {}…", self.name()),
            Stage::Identifying => format!("Downloading Wow.exe of {}…", self.name()),
            Stage::Confirming { .. } => format!("{} is ready to download.", self.name()),
            Stage::Checking { .. } => format!(
                "Checking {} for updates: {}",
                self.name(),
                percent(&self.progress)
            ),
            Stage::Current => format!("{} is up to date.", self.name()),
            Stage::Outdated(_) => format!("{} has updates.", self.name()),
            Stage::Staging | Stage::Beginning => format!("{} {}…", self.verb(), self.name()),
            Stage::Downloading { .. } => format!(
                "{} {}: {}, {}/s",
                self.verb(),
                self.name(),
                percent(&self.progress),
                size(self.progress.down)
            ),
            Stage::Moving { .. } => format!(
                "Putting the files of {} in place: {}",
                self.name(),
                percent(&self.progress)
            ),
            Stage::Failed(error) => format!("{}: {error}", self.name()),
        }
    }

    /// What is left to do once it stops.
    fn after(&self) -> After {
        let Some(staging) = &self.staging else {
            return After::Nothing;
        };

        match self.kind {
            // Turned down before it was kept.
            Kind::New { .. } if self.saved.is_none() => After::Remove(staging.clone()),
            // A client being downloaded resumes later, and so does an
            // update: what it downloaded is there for the next.
            _ => After::Nothing,
        }
    }
}

/// Finds the torrent of `source` for a new client, fetching it with
/// `client`, or with `engine` for a magnet link (starting it if there is
/// none), asking `peers` too: it must hold a client, which goes into a
/// folder named after it in `into`, missing or empty.
async fn find_new(
    source: Source,
    into: PathBuf,
    engine: Option<Engine>,
    client: Result<reqwest::Client, String>,
    peers: Vec<SocketAddr>,
) -> Result<Found, String> {
    let (bytes, engine) = torrent::fetch(source, engine, client, peers)
        .await
        .map_err(|error| format!("Could not get the torrent: {error}"))?;
    let torrent = holding_a_client(&bytes)?;

    let folder = into.join(folder_name(&torrent.name, &torrent.info_hash));
    let taken = {
        let folder = folder.clone();
        tokio::task::spawn_blocking(move || {
            fs::read_dir(&folder).is_ok_and(|mut entries| {
                entries.any(|entry| entry.is_ok_and(|entry| entry.file_name() != STAGING))
            })
        })
        .await
        .unwrap_or(false)
    };
    // One that holds something is someone else's; a download of it cut
    // short resumes.
    if taken {
        return Err(format!(
            "{} is there already: choose another folder to download into.",
            folder.display()
        ));
    }

    Ok(Found {
        bytes: Arc::new(bytes),
        torrent: Arc::new(torrent),
        folder,
        engine,
        saved: None,
    })
}

/// The torrent of `bytes`, which must hold a client.
fn holding_a_client(bytes: &[u8]) -> Result<Torrent, String> {
    let torrent = Torrent::parse(bytes).map_err(|error| format!("This is {}", describe(&error)))?;
    if torrent.executable().is_none() {
        return Err(
            "This torrent holds no WoW client: no Wow.exe beside a Data folder.".to_owned(),
        );
    }

    Ok(torrent)
}

/// Finds the torrent of a client being downloaded, `saved`, as
/// [`kept_or_shared`] does with `store`, `engine` and `peers`.
async fn find_kept(
    saved: ClientTorrent,
    store: PathBuf,
    engine: Option<Engine>,
    peers: Vec<SocketAddr>,
) -> Result<Found, String> {
    let (bytes, torrent, engine) = kept_or_shared(&saved, &store, engine, peers).await?;

    Ok(Found {
        bytes: Arc::new(bytes),
        torrent: Arc::new(torrent),
        folder: saved.folder,
        engine,
        saved: None,
    })
}

/// Finds the torrent of `source` to update the client at `root` from, as
/// [`find_new`] does: it must hold a client, in a folder named as `root`'s
/// is when it has it in one. The launcher keeps a copy in `store`, and the
/// client folder notes where it comes from.
async fn find_given(
    source: Source,
    root: PathBuf,
    store: PathBuf,
    engine: Option<Engine>,
    client: Result<reqwest::Client, String>,
    peers: Vec<SocketAddr>,
) -> Result<Found, String> {
    let (bytes, engine) = torrent::fetch(source.clone(), engine, client, peers)
        .await
        .map_err(|error| format!("Could not get the torrent: {error}"))?;
    let torrent = holding_a_client(&bytes)?;
    let client = torrent.client().unwrap_or_default();
    let folder = folder_of_client(&root, &client);
    // Its files go where the torrent has them.
    if client_in(&root, &folder).as_deref() != Some(client.as_str()) {
        return Err(format!(
            "This torrent has the client in a folder named {}: give {} that name to update it \
             from this torrent.",
            client.trim_end_matches('/'),
            root.display()
        ));
    }

    log::info!(
        "Updating {} from {} from now on",
        root.display(),
        torrent.name
    );
    let saved = ClientTorrent {
        name: torrent.name.clone(),
        info_hash: torrent.info_hash.clone(),
        source: kept_source(&source, &torrent),
        folder: folder.clone(),
        languages: Vec::new(),
    };
    let bytes = Arc::new(bytes);
    keep_copy(kept_in(&store, &torrent.info_hash), bytes.clone()).await?;
    note_origin(root, saved.clone()).await;

    Ok(Found {
        bytes,
        torrent: Arc::new(torrent),
        folder,
        engine,
        saved: Some(saved),
    })
}

/// Finds the torrent of the client at `root`, `saved`, again: fetched with
/// `client` from where it came, a magnet link's being always the same, or
/// else as [`kept_or_shared`] has it, with `store`, `engine` and `peers`. A
/// newer one is kept in `store` too, as long as it holds the client where
/// the old one did. The client folder notes where its torrent comes from,
/// if it doesn't yet, with a `.torrent` file given noted as its magnet link
/// now.
async fn find_again(
    saved: ClientTorrent,
    root: PathBuf,
    store: PathBuf,
    engine: Option<Engine>,
    client: Result<reqwest::Client, String>,
    peers: Vec<SocketAddr>,
) -> Result<Found, String> {
    let source = Source::parse(&saved.source);
    let fetched = match &source {
        Source::Magnet(_) => None,
        source => match torrent::fetch(source.clone(), None, client, Vec::new()).await {
            Ok((bytes, _)) => match Torrent::parse(&bytes) {
                Ok(torrent) => Some((bytes, torrent)),
                Err(error) => {
                    log::warn!(
                        "The torrent of {} is no longer usable, checking against the copy \
                         kept: {}",
                        saved.name,
                        describe(&error)
                    );
                    None
                }
            },
            Err(error) => {
                log::warn!(
                    "Could not fetch the torrent of {} again, checking against the copy kept: \
                     {error}",
                    saved.name
                );
                None
            }
        },
    };

    let (bytes, torrent, engine) = match fetched {
        Some((bytes, torrent)) if torrent.info_hash != saved.info_hash => {
            if torrent.client() != client_in(&root, &saved.folder) {
                return Err(format!(
                    "The torrent of {} changed, and holds the client elsewhere now: add it as a \
                     new client.",
                    saved.name
                ));
            }
            log::info!(
                "The torrent of {} is newer now: {}",
                root.display(),
                torrent.info_hash
            );
            keep_copy(kept_in(&store, &torrent.info_hash), Arc::new(bytes.clone())).await?;
            (bytes, torrent, engine)
        }
        Some((bytes, torrent)) => (bytes, torrent, engine),
        None => kept_or_shared(&saved, &store, engine, peers).await?,
    };

    let now = ClientTorrent {
        name: torrent.name.clone(),
        info_hash: torrent.info_hash.clone(),
        source: kept_source(&source, &torrent),
        folder: saved.folder.clone(),
        languages: saved.languages.clone(),
    };
    note_origin(root, now.clone()).await;

    Ok(Found {
        bytes: Arc::new(bytes),
        torrent: Arc::new(torrent),
        folder: saved.folder.clone(),
        engine,
        saved: (now != saved).then_some(now),
    })
}

/// The torrent of a client, `saved`, as the launcher kept a copy of it in
/// `store`; or, the copy gone (the launcher's files deleted, the client
/// added on another computer), as the peers sharing it have it, asked by
/// `engine` (started if there is none) and of `peers` too, and kept again.
/// They share it by its info hash, which its magnet link names with the
/// trackers that find them, if it was given one.
async fn kept_or_shared(
    saved: &ClientTorrent,
    store: &Path,
    engine: Option<Engine>,
    peers: Vec<SocketAddr>,
) -> Result<(Vec<u8>, Torrent, Option<Engine>), String> {
    let kept = kept_in(store, &saved.info_hash);
    let (bytes, engine, shared) = match tokio::fs::read(&kept).await {
        Ok(bytes) => (bytes, engine, false),
        Err(error) => {
            log::info!(
                "The launcher's copy of the torrent of {} can't be read ({}: {error}): asking \
                 the peers sharing it",
                saved.name,
                kept.display()
            );
            let link = match Source::parse(&saved.source) {
                Source::Magnet(link) => link,
                _ => format!("magnet:?xt=urn:btih:{}", saved.info_hash),
            };
            let (bytes, engine) = torrent::fetch(
                Source::Magnet(link),
                engine,
                Err("a magnet link needs no web address".to_owned()),
                peers,
            )
            .await
            .map_err(|error| format!("Could not get the torrent of {}: {error}", saved.name))?;
            (bytes, engine, true)
        }
    };

    let torrent = Torrent::parse(&bytes).map_err(|error| describe(&error))?;
    if torrent.info_hash != saved.info_hash {
        return Err(format!(
            "The torrent found for {} is another one: {}",
            saved.name, torrent.info_hash
        ));
    }
    if shared {
        keep_copy(kept, Arc::new(bytes.clone())).await?;
    }

    Ok((bytes, torrent, engine))
}

/// Notes in the client folder at `root` where its torrent comes from,
/// `saved`, unless it does already. That it can't changes nothing else.
async fn note_origin(root: PathBuf, saved: ClientTorrent) {
    let name = saved.name.clone();
    let noted = blocking(move || {
        if origin(&root).as_ref() == Some(&saved) {
            return Ok(());
        }
        keep_origin(&root, &saved)
    })
    .await;

    if let Err(error) = noted {
        log::warn!("Could not note in its folder where the torrent of {name} comes from: {error}");
    }
}

/// What the launcher keeps of where `torrent` comes from, `source`: a
/// `.torrent` file on this computer may go, so its magnet link instead.
fn kept_source(source: &Source, torrent: &Torrent) -> String {
    match source {
        Source::File(_) => torrent.magnet(),
        Source::Magnet(_) | Source::Url(_) => source.to_string(),
    }
}

/// Where the copy of the torrent with `info_hash` is kept, in `store`.
fn kept_in(store: &Path, info_hash: &str) -> PathBuf {
    store.join(format!("{info_hash}.torrent"))
}

/// Where the copies of the clients' torrents are kept: in the cache, since
/// a copy gone is fetched again from the peers sharing it. Builds before
/// 1.1.1 kept them with the data: those are moved over once, here.
pub fn store() -> PathBuf {
    let store = dirs::cache_dir()
        .unwrap_or_default()
        .join("old_launcher")
        .join("torrents");
    let old = dirs::data_dir()
        .unwrap_or_default()
        .join("old_launcher")
        .join("torrents");
    relocate(&old, &store);

    store
}

/// Moves the `.torrent` copies kept in `old` to `store`, one the latter
/// has already being deleted, and removes `old` and the folder it is in
/// when that leaves them empty. What can't be moved is left, and fetched
/// again when needed.
fn relocate(old: &Path, store: &Path) {
    let Ok(entries) = fs::read_dir(old) else {
        return;
    };

    for entry in entries.flatten() {
        let from = entry.path();
        if from
            .extension()
            .is_none_or(|extension| extension != "torrent")
        {
            continue;
        }
        let Some(name) = from.file_name() else {
            continue;
        };
        let to = store.join(name);
        let moved = if to.exists() {
            fs::remove_file(&from)
        } else {
            fs::create_dir_all(store).and_then(|()| {
                fs::rename(&from, &to)
                    .or_else(|_| fs::copy(&from, &to).and_then(|_| fs::remove_file(&from)))
            })
        };
        match moved {
            Ok(()) => log::info!("Moved {} to the cache, {}", from.display(), store.display()),
            Err(error) => log::warn!("Could not move {} to the cache: {error}", from.display()),
        }
    }

    let _ = fs::remove_dir(old);
    if let Some(parent) = old.parent() {
        let _ = fs::remove_dir(parent);
    }
}

/// Keeps a copy of a torrent's `bytes` at `path`.
async fn keep_copy(path: PathBuf, bytes: Arc<Vec<u8>>) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        fs::write(&path, &*bytes).map_err(|error| format!("{}: {error}", path.display()))
    })
    .await
    .map_err(|error| error.to_string())?
}

/// The folder of a client in `folder`, where a torrent's files go, the
/// client being at `client` in it, as [`Torrent::client`] says it.
fn within(folder: &Path, client: &str) -> PathBuf {
    match client.trim_end_matches('/') {
        "" => folder.to_path_buf(),
        client => folder.join(client),
    }
}

/// The name of the folder a new client goes in: the torrent's, `name`,
/// with what no system allows in a name replaced, or its `info_hash` when
/// nothing is left.
fn folder_name(name: &str, info_hash: &str) -> String {
    let name: String = name
        .chars()
        .map(|character| {
            if character.is_control() || r#"<>:"/\|?*"#.contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect();
    let name = name.trim().trim_end_matches(['.', ' ']);

    if name.is_empty() {
        info_hash.to_owned()
    } else {
        name.to_owned()
    }
}

/// Starts `engine`, if there is none, and begins downloading the `files` of
/// the torrent of `bytes` into `staging`, from `peers` too.
async fn begin(
    engine: Option<Engine>,
    bytes: Arc<Vec<u8>>,
    staging: PathBuf,
    files: Vec<usize>,
    peers: Vec<SocketAddr>,
) -> Result<Began, String> {
    let engine = match engine {
        Some(engine) => engine,
        None => Engine::start()
            .await
            .map_err(|error| format!("Could not start BitTorrent: {error}"))?,
    };

    tokio::fs::create_dir_all(&staging)
        .await
        .map_err(|error| format!("{}: {error}", staging.display()))?;
    let download = engine
        .download(&bytes, &staging, Some(files), peers)
        .await
        .map_err(|error| format!("Could not begin downloading: {error}"))?;

    Ok(Began { engine, download })
}

/// Finishes a download: stops it, so nothing more is shared and its files
/// are closed; moves the `files` downloaded into `staging` to `folder`,
/// saying in `moved` how far it got, once a `Wow.exe` among them is known
/// to be 3.3.5a's; and notes them there as the torrent has them. For a new
/// client, `new` is what the launcher keeps of its torrent, which the
/// client folder notes too, and the client is opened.
#[allow(clippy::too_many_arguments)]
async fn finish(
    engine: Engine,
    download: Download,
    staging: PathBuf,
    folder: PathBuf,
    torrent: Arc<Torrent>,
    files: Vec<usize>,
    moved: Arc<AtomicU64>,
    new: Option<ClientTorrent>,
) -> Result<Option<Installation>, String> {
    engine.forget(&download).await?;

    let client = torrent.client().unwrap_or_default();
    // A torrent given to update from may hold another game.
    if torrent
        .executable()
        .is_some_and(|executable| files.contains(&executable))
    {
        let staged = within(&staging, &client);
        blocking(move || Installation::open(staged))
            .await
            .map_err(|error| format!("This is not a client the launcher supports: {error}"))?;
    }

    blocking(move || {
        crate::core::torrent::move_into(&staging, &folder, &torrent, &files, &mut |bytes| {
            moved.store(bytes, Ordering::Relaxed);
        })?;
        crate::core::torrent::record(&folder, &torrent, &files)?;
        new.map(|saved| {
            let root = within(&folder, &client);
            keep_origin(&root, &saved)?;
            Installation::open(root)
        })
        .transpose()
    })
    .await
}

/// Stops `download`, if any, and does what is left `after` (see [`tidy`]).
fn clean_up(engine: Option<Engine>, download: Option<Download>, after: After) -> Task<Message> {
    if download.is_none() && matches!(after, After::Nothing) {
        return Task::none();
    }

    Task::perform(tidy(engine, download, after), Message::CleanedUp)
}

/// Stops `download` with `engine`, if any, and then does what is left
/// `after`: with its files closed.
async fn tidy(
    engine: Option<Engine>,
    download: Option<Download>,
    after: After,
) -> Result<(), String> {
    let stopped = match (engine, download) {
        (Some(engine), Some(download)) => engine.forget(&download).await,
        _ => Ok(()),
    };
    let after = match after {
        After::Nothing => Ok(()),
        After::Remove(staging) => tokio::task::spawn_blocking(move || {
            match fs::remove_dir_all(&staging) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    return Err(format!("{}: {error}", staging.display()));
                }
                _ => {}
            }
            // The folder it was made in, if there is nothing else there.
            if let Some(folder) = staging.parent() {
                let _ = fs::remove_dir(folder);
            }
            Ok(())
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|removed| removed),
    };

    stopped.and(after)
}

/// A banner saying `said`, with `buttons`, tinted with `tone`.
fn banner<'a>(
    said: String,
    buttons: Row<Element<'a, Message>>,
    tone: style::Tone,
) -> Element<'a, Message> {
    container(
        row![text(said).width(Fill), buttons.spacing(8)]
            .spacing(8)
            .align_y(Center),
    )
    .padding([6, 12])
    .style(style::banner(tone))
    .boxed()
}

/// How far a download or a check got, as a percentage, rounded down so
/// 100% means done.
fn percent(progress: &Progress) -> String {
    if progress.total == 0 {
        return "0%".to_owned();
    }

    format!("{}%", progress.done * 100 / progress.total)
}

/// A bar of how far `progress` got, with the percentage.
fn bar(progress: &Progress) -> Row<Element<'_, Message>> {
    let fraction = if progress.total == 0 {
        0.0
    } else {
        progress.done as f32 / progress.total as f32
    };

    row![
        progress_bar(0.0..=1.0, fraction).girth(8).style(style::bar),
        text(percent(progress)).size(14).width(48),
    ]
    .spacing(10)
    .align_y(Center)
}

/// How a download goes: how far it got, and from how many peers how fast;
/// `pieces` is how many pieces hold what it downloads, when they are
/// worth counting.
fn transfer(job: &Job, pieces: Option<usize>) -> Element<'_, Message> {
    let progress = &job.progress;
    // Nothing heard from the engine yet.
    if progress.total == 0 {
        return waiting("Connecting…");
    }
    if progress.checking {
        return column![text("Checking what is there already…"), bar(progress)]
            .spacing(10)
            .boxed();
    }

    let piece_length = job
        .found
        .as_ref()
        .map_or(1, |found| found.torrent.piece_length.max(1));
    // What is left to download is whole pieces.
    let had = pieces.map(|pieces| {
        let left = progress
            .total
            .saturating_sub(progress.done)
            .div_ceil(piece_length);
        pieces.saturating_sub(usize::try_from(left).unwrap_or(usize::MAX))
    });
    let left = progress.total.saturating_sub(progress.done);
    let time_left = left
        .checked_div(progress.down)
        .map_or_else(|| "Unknown".to_owned(), duration);

    column![
        bar(progress),
        text(format!(
            "{} of {}",
            size(progress.done),
            size(progress.total)
        ))
        .size(13)
        .style(text::secondary),
        fact(
            "Peers",
            format!(
                "{} connected, {} connecting",
                progress.peers, progress.connecting
            ),
        ),
        had.zip(pieces)
            .map(|(had, pieces)| fact("Pieces", format!("{had} of {pieces}"))),
        fact(
            "Speed",
            format!("{}/s down, {}/s up", size(progress.down), size(progress.up)),
        ),
        fact("Time left", time_left),
    ]
    .spacing(6)
    .boxed()
}

/// What an update downloads: how much, and which files.
fn outdated<'a>(check: &'a Check, found: Option<&'a Found>) -> Element<'a, Message> {
    let Some(torrent) = found.map(|found| &*found.torrent) else {
        return space::horizontal().boxed();
    };
    let files = check.files();
    let named = files
        .iter()
        .take(NAMED)
        .map(|index| -> Element<'a, Message> {
            let path = &torrent.files[*index].path;
            let missing = check.missing.contains(index);
            text(if missing {
                format!("{path} (missing)")
            } else {
                path.clone()
            })
            .size(13)
            .boxed()
        });
    let more = (files.len() > NAMED).then(|| {
        text(format!("and {} more", counted(files.len() - NAMED, "file")))
            .size(13)
            .style(text::secondary)
            .boxed()
    });

    column![
        text(format!(
            "{} to download, {}:",
            counted(files.len(), "file"),
            size(check.bytes(torrent))
        )),
        container(column(named.chain(more)).spacing(2))
            .padding(10)
            .width(Fill)
            .style(style::inset),
        text("Your settings, addons and realmlist stay as they are.")
            .size(13)
            .style(text::secondary),
    ]
    .spacing(10)
    .boxed()
}

/// `said`, beside a bar that moves while nothing tells how far it got.
fn waiting(said: &str) -> Element<'_, Message> {
    row![activity(120.0, 6.0), text(said)]
        .spacing(10)
        .align_y(Center)
        .boxed()
}

/// A labelled fact.
fn fact(label: &str, value: String) -> Row<Element<'_, Message>> {
    row![
        text(label).size(14).width(100).style(text::secondary),
        text(value).size(14).wrapping(text::Wrapping::WordOrGlyph),
    ]
    .spacing(12)
}

/// About how long `seconds` is, for people.
fn duration(seconds: u64) -> String {
    match seconds {
        0..60 => "Less than a minute".to_owned(),
        60..3600 => format!("About {}", counted(seconds.div_ceil(60) as usize, "minute")),
        _ => {
            let hours = seconds / 3600;
            let minutes = (seconds % 3600) / 60;
            if minutes == 0 {
                format!("About {}", counted(hours as usize, "hour"))
            } else {
                format!(
                    "About {} {}",
                    counted(hours as usize, "hour"),
                    counted(minutes as usize, "minute")
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use iced_test::simulator;

    use super::*;
    use crate::core::testing::{WOTLK, fake_executable, torrent_naming, torrent_of};
    use crate::torrent::tests::{address, finished, local};

    const MB: u64 = 1024 * 1024;

    /// A client's files, as its torrent has them.
    fn files() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("Wow.exe", fake_executable(WOTLK)),
            ("Data/common.MPQ", vec![7; 40_000]),
            (
                "Data/enUS/realmlist.wtf",
                b"set realmlist logon.example.com".to_vec(),
            ),
            ("WTF/Config.wtf", b"SET locale \"enUS\"".to_vec()),
        ]
    }

    /// Writes `files` into `folder`.
    fn write(folder: &Path, files: &[(&str, Vec<u8>)]) {
        for (path, contents) in files {
            let path = folder.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
    }

    /// The torrent of `files`, named `WotLK`, and an engine on this
    /// computer sharing them from the folder returned.
    async fn seeded(files: &[(&str, Vec<u8>)]) -> (Vec<u8>, tempfile::TempDir, Engine) {
        let listed: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(path, contents)| (*path, contents.as_slice()))
            .collect();
        let bytes = torrent_of("WotLK", &listed, 16 * 1024);
        let folder = tempfile::tempdir().unwrap();
        write(folder.path(), files);

        let seeder = local().await;
        let seed = seeder
            .download(&bytes, folder.path(), None, Vec::new())
            .await
            .unwrap();
        let _ = finished(&seed).await;

        (bytes, folder, seeder)
    }

    /// Where a test keeps the torrents.
    struct Cache {
        store: tempfile::TempDir,
    }

    impl Cache {
        fn new() -> Self {
            Self {
                store: tempfile::tempdir().unwrap(),
            }
        }

        fn state(&self) -> State {
            State::new(self.store.path().to_path_buf())
        }
    }

    /// Nothing under way, downloading from `seeder` alone.
    async fn downloading_from(seeder: &Engine, cache: &Cache) -> State {
        State {
            engine: Some(local().await),
            peers: vec![address(seeder)],
            ..cache.state()
        }
    }

    /// The names of what `folder` holds, sorted.
    fn listed(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn job(state: &State) -> &Job {
        state.job.as_ref().unwrap()
    }

    fn update(
        state: &mut State,
        message: Message,
        config: &Config,
        playing: bool,
    ) -> Option<Instruction> {
        state
            .update(
                message,
                config,
                Err("offline"),
                playing,
                &mut Errors::default(),
            )
            .instruction
    }

    /// Asks how the download goes until it is done.
    async fn ticked_through(state: &mut State, config: &Config) {
        for _ in 0..600 {
            let _ = update(state, Message::Tick, config, false);
            if !matches!(
                job(state).stage,
                Stage::Identifying | Stage::Downloading { .. }
            ) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("still downloading: {:?}", job(state).progress);
    }

    /// What the task of a stage beginning a download does.
    async fn began(state: &State) -> Result<Began, String> {
        let job = job(state);

        begin(
            state.engine.clone(),
            job.found.as_ref().unwrap().bytes.clone(),
            job.staging.clone().unwrap(),
            job.files.clone(),
            state.peers.clone(),
        )
        .await
    }

    /// What the task of the stage moving the files does, with the
    /// `download` the job gave it.
    async fn moved(state: &State, download: Download) -> Result<Option<Installation>, String> {
        let job = job(state);
        let found = job.found.as_ref().unwrap();
        let Stage::Moving { moved } = &job.stage else {
            panic!("not moving");
        };
        assert!(job.download.is_none(), "given away");

        finish(
            state.engine.clone().unwrap(),
            download,
            job.staging.clone().unwrap(),
            found.folder.clone(),
            found.torrent.clone(),
            job.files.clone(),
            moved.clone(),
            job.saved
                .clone()
                .filter(|_| !matches!(job.kind, Kind::Check)),
        )
        .await
    }

    /// A new client from the `.torrent` file at `path`, into `into`, asked
    /// about once its `Wow.exe` was read.
    async fn confirming(state: &mut State, path: &Path, into: &Path) {
        let config = Config::default();
        let _ = state.start(
            Request::New {
                source: Source::File(path.to_path_buf()),
                into: into.to_path_buf(),
            },
            &config,
            Err("offline"),
        );
        let found = find_new(
            Source::File(path.to_path_buf()),
            into.to_path_buf(),
            state.engine.clone(),
            Err("offline".to_owned()),
            state.peers.clone(),
        )
        .await;
        let _ = update(state, Message::Found(1, found), &config, false);
        assert_eq!(job(state).files, [0], "Wow.exe alone, first");
        assert!(matches!(job(state).stage, Stage::Beginning));

        let began = began(state).await;
        let _ = update(state, Message::Began(1, began), &config, false);
        assert!(matches!(job(state).stage, Stage::Identifying));
        ticked_through(state, &config).await;
        assert!(matches!(job(state).stage, Stage::Opening));

        let staging = job(state).staging.clone().unwrap();
        let opened = Installation::open(staging).map_err(|error| describe(&error));
        let _ = update(state, Message::Identified(1, opened), &config, false);
        assert!(matches!(job(state).stage, Stage::Confirming { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_client_in_several_languages_downloads_those_chosen() {
        let mut files = files();
        files.push(("Data/enUS/locale-enUS.MPQ", vec![1; 20_000]));
        files.push(("Data/esES/locale-esES.MPQ", vec![2; 20_000]));
        files.push((
            "Data/esES/realmlist.wtf",
            b"set realmlist logon.example.com".to_vec(),
        ));
        let (bytes, _seeding, seeder) = seeded(&files).await;
        let given = tempfile::tempdir().unwrap();
        let path = given.path().join("client.torrent");
        fs::write(&path, &bytes).unwrap();
        let (cache, into) = (Cache::new(), tempfile::tempdir().unwrap());
        let config = Config::default();
        let mut state = downloading_from(&seeder, &cache).await;

        confirming(&mut state, &path, into.path()).await;
        let found = job(&state).found.clone().unwrap();
        let torrent = found.torrent.clone();
        let named = |files: &[usize]| -> Vec<String> {
            files
                .iter()
                .map(|index| torrent.files[*index].path.clone())
                .collect()
        };
        {
            let mut ui = simulator(state.dialog(false).unwrap());
            for line in [
                "Languages",
                "enUS (20 KB)",
                "esES (20 KB)",
                "Download 41 KB",
            ] {
                assert!(ui.find(line).is_ok(), "{line}");
            }
        }
        // Nothing chosen yet: Download does nothing.
        assert!(update(&mut state, Message::Download, &config, false).is_none());
        assert!(matches!(job(&state).stage, Stage::Confirming { .. }));

        let _ = update(
            &mut state,
            Message::Language("esES".to_owned(), true),
            &config,
            false,
        );
        let _ = update(
            &mut state,
            Message::Language("deDE".to_owned(), true),
            &config,
            false,
        );
        {
            let mut ui = simulator(state.dialog(false).unwrap());
            assert!(ui.find("Download 60 KB").is_ok(), "what Spanish adds");
        }
        let Some(Instruction::Pending(saved)) =
            update(&mut state, Message::Download, &config, false)
        else {
            panic!("not kept to resume");
        };
        assert_eq!(saved.languages, ["esES"], "not one it hasn't");
        assert_eq!(
            named(&job(&state).files),
            [
                "Wow.exe",
                "Data/common.MPQ",
                "WTF/Config.wtf",
                "Data/esES/locale-esES.MPQ",
                "Data/esES/realmlist.wtf",
            ]
        );

        keep_copy(state.kept(&saved.info_hash), found.bytes.clone())
            .await
            .unwrap();
        let (engine, download) = (
            state.engine.clone().unwrap(),
            job(&state).download.clone().unwrap(),
        );
        let selected = engine.select(&download, &job(&state).files).await;
        let _ = update(&mut state, Message::Selected(1, selected), &config, false);
        ticked_through(&mut state, &config).await;
        assert!(matches!(job(&state).stage, Stage::Moving { .. }));
        let moved = moved(&state, download).await;
        let Some(Instruction::Added { installation, .. }) =
            update(&mut state, Message::Moved(1, moved), &config, false)
        else {
            panic!("not added");
        };
        assert!(found.folder.join("Data/esES/locale-esES.MPQ").is_file());
        assert!(!found.folder.join("Data/enUS").exists(), "left out");
        assert_eq!(
            installation.locale(),
            Some("esES"),
            "its Config.wtf says enUS, which it hasn't"
        );

        // Checked against its torrent later, nothing is missing.
        let check =
            crate::core::torrent::check(
                &found.folder,
                &torrent,
                &mut |_| ControlFlow::Continue(()),
            )
            .unwrap()
            .unwrap();
        assert!(check.is_current(), "{check:?}");

        // Resumed, it downloads the same files.
        let resuming = Config {
            downloads: vec![saved.clone()],
            ..Config::default()
        };
        let mut state = downloading_from(&seeder, &cache).await;
        let _ = state.start(
            Request::Resume(saved.info_hash.clone()),
            &resuming,
            Err("offline"),
        );
        let found = find_kept(
            saved,
            cache.store.path().to_path_buf(),
            state.engine.clone(),
            state.peers.clone(),
        )
        .await;
        let _ = update(&mut state, Message::Found(1, found), &resuming, false);
        assert_eq!(
            named(&job(&state).files),
            [
                "Wow.exe",
                "Data/common.MPQ",
                "WTF/Config.wtf",
                "Data/esES/locale-esES.MPQ",
                "Data/esES/realmlist.wtf",
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_client_downloads_in_its_folder_aside_and_moves_in_whole() {
        let files = files();
        let (bytes, _seeding, seeder) = seeded(&files).await;
        let given = tempfile::tempdir().unwrap();
        let path = given.path().join("client.torrent");
        fs::write(&path, &bytes).unwrap();
        let (cache, into) = (Cache::new(), tempfile::tempdir().unwrap());
        let config = Config::default();
        let mut state = downloading_from(&seeder, &cache).await;

        confirming(&mut state, &path, into.path()).await;
        let found = job(&state).found.clone().unwrap();
        let staging = job(&state).staging.clone().unwrap();
        assert_eq!(found.folder, into.path().join("WotLK"));
        assert_eq!(staging, found.folder.join(".download"));
        assert_eq!(
            listed(&found.folder),
            [".download"],
            "nothing else goes there until it is whole"
        );
        {
            let mut ui = simulator(state.dialog(false).unwrap());
            for line in [
                "WotLK",
                "3.3.5.12340",
                "4 files",
                "Download 41 KB",
                &found.folder.display().to_string(),
            ] {
                assert!(ui.find(line).is_ok(), "{line}");
            }
        }

        let Some(Instruction::Pending(saved)) =
            update(&mut state, Message::Download, &config, false)
        else {
            panic!("not kept to resume");
        };
        assert_eq!(saved.folder, found.folder);
        assert_eq!(
            saved.source,
            found.torrent.magnet(),
            "the file given may go"
        );

        // What the task does: keep a copy of the torrent, and ask for all.
        keep_copy(state.kept(&saved.info_hash), found.bytes.clone())
            .await
            .unwrap();
        let (engine, download) = (
            state.engine.clone().unwrap(),
            job(&state).download.clone().unwrap(),
        );
        // Until the engine was told, Wow.exe is all it downloads, and done.
        assert!(matches!(job(&state).stage, Stage::Beginning));
        let _ = update(&mut state, Message::Tick, &config, false);
        assert!(matches!(job(&state).stage, Stage::Beginning));
        let selected = engine.select(&download, &job(&state).files).await;
        let _ = update(&mut state, Message::Selected(1, selected), &config, false);
        assert!(matches!(job(&state).stage, Stage::Downloading { .. }));
        ticked_through(&mut state, &config).await;
        assert!(matches!(job(&state).stage, Stage::Moving { .. }));
        assert_eq!(listed(&found.folder), [".download"], "still aside");

        let moved = moved(&state, download).await;
        let Some(Instruction::Added {
            installation,
            torrent,
            notice,
        }) = update(&mut state, Message::Moved(1, moved), &config, false)
        else {
            panic!("not added");
        };
        assert_eq!(notice, "Downloaded WotLK: it is ready to play.");
        // Spelled as the launcher spells a client folder: Windows' `\\?\`
        // left out.
        assert_eq!(
            installation.root(),
            Installation::open(&found.folder).unwrap().root()
        );
        assert_eq!(torrent.folder, installation.root());
        assert!(state.job.is_none());
        for (file, contents) in &files {
            assert_eq!(
                fs::read(found.folder.join(file)).unwrap(),
                *contents,
                "{file}"
            );
        }
        assert!(!staging.exists(), "nothing left beside it");
        assert!(state.kept(&torrent.info_hash).is_file());
        assert_eq!(
            origin(installation.root()),
            Some(torrent),
            "the client keeps where it comes from"
        );
        assert_eq!(
            listed(&found.folder),
            [".old_launcher.toml", "Data", "WTF", "Wow.exe"],
            "noted beside it too: what the next check needn't read"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_client_turned_down_leaves_nothing_and_goes_only_where_nothing_is() {
        let (bytes, _seeding, seeder) = seeded(&files()).await;
        let given = tempfile::tempdir().unwrap();
        let path = given.path().join("client.torrent");
        fs::write(&path, &bytes).unwrap();
        let (cache, into) = (Cache::new(), tempfile::tempdir().unwrap());
        let mut state = downloading_from(&seeder, &cache).await;

        confirming(&mut state, &path, into.path()).await;
        let staging = job(&state).staging.clone().unwrap();
        assert!(staging.join("Wow.exe").is_file());

        // What stopping cleans up, done as its task does it.
        let (after, download) = (job(&state).after(), job(&state).download.clone());
        assert!(matches!(&after, After::Remove(removed) if *removed == staging));
        let _ = update(&mut state, Message::Stop, &Config::default(), false);
        assert!(state.job.is_none());
        tidy(state.engine.clone(), download, after).await.unwrap();
        assert!(!staging.exists());
        assert!(listed(into.path()).is_empty(), "nor the folder made for it");

        // A folder holding a download of it cut short is its own.
        let folder = into.path().join("WotLK");
        fs::create_dir_all(folder.join(".download")).unwrap();
        let again = || {
            find_new(
                Source::File(path.clone()),
                into.path().to_path_buf(),
                None,
                Err("offline".to_owned()),
                Vec::new(),
            )
        };
        assert!(again().await.is_ok());
        // A folder holding something else is someone else's.
        fs::write(folder.join("notes.txt"), b"mine").unwrap();
        assert!(
            again()
                .await
                .is_err_and(|error| error.contains("is there already"))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_update_downloads_beside_the_client_and_moves_in_what_changed() {
        let files = files();
        let (bytes, _seeding, seeder) = seeded(&files).await;
        let torrent = Torrent::parse(&bytes).unwrap();
        let cache = Cache::new();
        let kept = cache
            .store
            .path()
            .join(format!("{}.torrent", torrent.info_hash));
        fs::write(&kept, &bytes).unwrap();

        // The client as the player has it: a game file changed since, and
        // their own realmlist.
        let client = tempfile::tempdir().unwrap();
        let root = client.path().to_path_buf();
        write(&root, &files);
        fs::write(root.join("Data/common.MPQ"), vec![8; 40_000]).unwrap();
        let realmlist = root.join("Data/enUS/realmlist.wtf");
        fs::write(&realmlist, b"set realmlist logon.other.org").unwrap();

        let saved = ClientTorrent {
            name: "WotLK".into(),
            info_hash: torrent.info_hash.clone(),
            source: format!("magnet:?xt=urn:btih:{}", torrent.info_hash),
            folder: root.clone(),
            languages: Vec::new(),
        };
        let config = Config {
            installations: vec![root.clone()],
            torrents: BTreeMap::from([(root.clone(), saved.clone())]),
            ..Config::default()
        };
        let mut state = downloading_from(&seeder, &cache).await;

        let _ = state.start(Request::Check(root.clone()), &config, Err("offline"));
        let found = find_again(
            saved.clone(),
            root.clone(),
            cache.store.path().to_path_buf(),
            state.engine.clone(),
            Err("offline".to_owned()),
            Vec::new(),
        )
        .await;
        assert!(found.as_ref().is_ok_and(|found| found.saved.is_none()));
        assert!(update(&mut state, Message::Found(1, found), &config, false).is_none());
        assert!(matches!(job(&state).stage, Stage::Checking { .. }));
        assert_eq!(
            origin(&root),
            Some(saved),
            "a client from before it was noted is now"
        );

        let checked =
            crate::core::torrent::check(&root, &torrent, &mut |_| ControlFlow::Continue(()))
                .map_err(|error| describe(&error));
        let _ = update(&mut state, Message::Checked(1, checked), &config, false);
        assert!(matches!(job(&state).stage, Stage::Outdated(_)));
        {
            let mut ui = simulator(state.dialog(true).unwrap());
            for line in [
                "Updates for WotLK",
                "1 file to download, 40 KB:",
                "Data/common.MPQ",
                "Your settings, addons and realmlist stay as they are.",
            ] {
                assert!(ui.find(line).is_ok(), "{line}");
            }
            let _ = ui.click("Update");
            assert!(ui.into_messages().next().is_none(), "not while playing");
        }
        let _ = update(&mut state, Message::Update, &config, true);
        assert!(matches!(job(&state).stage, Stage::Outdated(_)));

        let _ = update(&mut state, Message::Update, &config, false);
        assert!(matches!(job(&state).stage, Stage::Staging));
        assert_eq!(job(&state).files, [1]);
        let staging = job(&state).staging.clone().unwrap();
        let staged = crate::core::torrent::stage(&staging, &root, &torrent, &[1])
            .map_err(|error| describe(&error));
        let _ = update(&mut state, Message::Staged(1, staged), &config, false);
        assert!(matches!(job(&state).stage, Stage::Beginning));
        assert!(!state.holds_back(&root), "nothing changes there yet");

        let began = began(&state).await;
        let download = began.as_ref().unwrap().download.clone();
        let _ = update(&mut state, Message::Began(1, began), &config, false);
        // common.MPQ is in the first three 16 KB pieces.
        assert!(matches!(
            job(&state).stage,
            Stage::Downloading { pieces: 3 }
        ));
        for _ in 0..600 {
            // Its files wait for the game to close.
            let _ = update(&mut state, Message::Tick, &config, true);
            if job(&state).progress.finished {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(matches!(job(&state).stage, Stage::Downloading { .. }));
        let mut ui = simulator(state.dialog(true).unwrap());
        assert!(
            ui.find("Downloaded. The files go in place once the game is closed.")
                .is_ok()
        );
        drop(ui);
        assert_eq!(
            fs::read(root.join("Data/common.MPQ")).unwrap(),
            vec![8; 40_000]
        );

        let _ = update(&mut state, Message::Tick, &config, false);
        assert!(matches!(job(&state).stage, Stage::Moving { .. }));
        assert!(state.holds_back(&root));
        let moved = moved(&state, download).await;
        let Some(Instruction::Updated { notice, .. }) =
            update(&mut state, Message::Moved(1, moved), &config, false)
        else {
            panic!("not updated");
        };

        assert_eq!(notice, "Updated WotLK: 1 file downloaded.");
        assert_eq!(
            fs::read(root.join("Data/common.MPQ")).unwrap(),
            vec![7; 40_000]
        );
        assert_eq!(
            fs::read(&realmlist).unwrap(),
            b"set realmlist logon.other.org"
        );
        assert!(!staging.exists());
        let check =
            crate::core::torrent::check(&root, &torrent, &mut |_| ControlFlow::Continue(()))
                .unwrap()
                .unwrap();
        assert!(check.is_current(), "{check:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_newer_torrent_replaces_the_copy_kept_unless_it_moved_the_client() {
        let old: Vec<(&str, &[u8])> = vec![("Wow.exe", b"MZ"), ("Data/common.MPQ", b"old")];
        let old_bytes = torrent_of("WotLK", &old, 16);
        let old = Torrent::parse(&old_bytes).unwrap();
        let store = tempfile::tempdir().unwrap();
        fs::write(kept_in(store.path(), &old.info_hash), &old_bytes).unwrap();
        let client = tempfile::tempdir().unwrap();
        let given = store.path().join("given.torrent");
        // As the launcher kept a file given before it kept its magnet link.
        let saved = ClientTorrent {
            name: "WotLK".into(),
            info_hash: old.info_hash.clone(),
            source: given.display().to_string(),
            folder: client.path().to_path_buf(),
            languages: Vec::new(),
        };
        let again = || {
            find_again(
                saved.clone(),
                client.path().to_path_buf(),
                store.path().to_path_buf(),
                None,
                Err("offline".to_owned()),
                Vec::new(),
            )
        };

        // The file given is gone: the copy kept, and its magnet link.
        let found = again().await.unwrap();
        assert_eq!(found.torrent.info_hash, saved.info_hash);
        let kept = found.saved.unwrap();
        assert_eq!(kept.source, old.magnet());
        assert_eq!(origin(client.path()), Some(kept));

        let newer: Vec<(&str, &[u8])> = vec![("Wow.exe", b"MZ"), ("Data/common.MPQ", b"new")];
        let newer = torrent_of("WotLK", &newer, 16);
        fs::write(&given, &newer).unwrap();
        let found = again().await.unwrap();
        let kept = found.saved.unwrap();
        assert_eq!(kept.info_hash, found.torrent.info_hash);
        assert_ne!(kept.info_hash, saved.info_hash, "newer");
        assert!(kept_in(store.path(), &kept.info_hash).is_file());
        assert_eq!(origin(client.path()), Some(kept));

        let moved: Vec<(&str, &[u8])> = vec![
            ("Client/Wow.exe", b"MZ"),
            ("Client/Data/common.MPQ", b"new"),
        ];
        fs::write(&given, torrent_of("WotLK", &moved, 16)).unwrap();
        assert!(
            again()
                .await
                .is_err_and(|error| error.contains("elsewhere"))
        );

        let lacking: Vec<(&str, &[u8])> = vec![("readme.txt", b"hello")];
        fs::write(&given, torrent_of("WotLK", &lacking, 16)).unwrap();
        let none = find_new(
            Source::File(given),
            store.path().to_path_buf(),
            None,
            Err("offline".to_owned()),
            Vec::new(),
        )
        .await;
        assert!(none.is_err_and(|error| error.contains("holds no WoW client")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_the_copy_kept_the_torrent_comes_from_the_peers_sharing_it() {
        let files = files();
        let (bytes, _seeding, seeder) = seeded(&files).await;
        let torrent = Torrent::parse(&bytes).unwrap();
        let client = tempfile::tempdir().unwrap();
        let root = client.path().to_path_buf();
        write(&root, &files);

        // Found by its magnet link, or by its info hash alone.
        for source in [
            torrent.magnet(),
            "https://example.com/gone.torrent".to_owned(),
        ] {
            let cache = Cache::new();
            let state = downloading_from(&seeder, &cache).await;
            let saved = ClientTorrent {
                name: "WotLK".into(),
                info_hash: torrent.info_hash.clone(),
                source,
                folder: root.clone(),
                languages: Vec::new(),
            };

            let found = find_again(
                saved.clone(),
                root.clone(),
                cache.store.path().to_path_buf(),
                state.engine.clone(),
                Err("offline".to_owned()),
                state.peers.clone(),
            )
            .await
            .unwrap();
            assert_eq!(found.torrent.info_hash, torrent.info_hash);
            assert_eq!(found.torrent.files, torrent.files);
            assert!(found.saved.is_none(), "{:?}", found.saved);
            let kept = fs::read(state.kept(&torrent.info_hash)).unwrap();
            assert_eq!(
                Torrent::parse(&kept).unwrap().info_hash,
                torrent.info_hash,
                "kept again"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_client_given_a_torrent_keeps_it_and_is_checked_against_it() {
        let files = files();
        let listed: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(path, contents)| (*path, contents.as_slice()))
            .collect();
        let tracker = "udp://tracker.example.com:80/announce";
        let bytes = torrent_naming("WotLK", &listed, 16 * 1024, &[tracker]);
        let torrent = Torrent::parse(&bytes).unwrap();
        let given = tempfile::tempdir().unwrap();
        let path = given.path().join("client.torrent");
        fs::write(&path, &bytes).unwrap();
        let games = tempfile::tempdir().unwrap();
        let root = games.path().join("WoW");
        write(&root, &files);
        let config = Config {
            installations: vec![root.clone()],
            ..Config::default()
        };
        let cache = Cache::new();
        let mut state = cache.state();

        let _ = state.start(
            Request::Adopt {
                root: root.clone(),
                source: Source::File(path.clone()),
            },
            &config,
            Err("offline"),
        );
        {
            let mut ui = simulator(state.dialog(false).unwrap());
            assert!(ui.find("Updates for WoW").is_ok());
            assert!(ui.find("Finding the torrent…").is_ok());
        }
        let given_to = |root: PathBuf| {
            find_given(
                Source::File(path.clone()),
                root,
                cache.store.path().to_path_buf(),
                None,
                Err("offline".to_owned()),
                Vec::new(),
            )
        };
        let found = given_to(root.clone()).await;
        let Some(Instruction::Saved(saved_for, saved)) =
            update(&mut state, Message::Found(1, found), &config, false)
        else {
            panic!("not kept");
        };
        assert_eq!(saved_for, root);
        assert_eq!(
            saved,
            ClientTorrent {
                name: "WotLK".into(),
                info_hash: torrent.info_hash.clone(),
                source: torrent.magnet(),
                folder: root.clone(),
                languages: Vec::new(),
            }
        );
        assert!(saved.source.contains("&tr=udp"), "its trackers find it");
        assert!(matches!(job(&state).stage, Stage::Checking { .. }));
        assert!(state.kept(&torrent.info_hash).is_file());
        assert_eq!(origin(&root), Some(saved), "kept beside it too");

        // With the client in a folder of its own, its folder is named so.
        let nested: Vec<(&str, &[u8])> = vec![
            ("World of Warcraft/Data/common.MPQ", b"world"),
            ("World of Warcraft/Wow.exe", b"MZ"),
        ];
        fs::write(&path, torrent_of("HD", &nested, 16)).unwrap();
        assert!(
            given_to(root.clone())
                .await
                .is_err_and(|error| error.contains("a folder named World of Warcraft"))
        );
        let named = games.path().join("World of Warcraft");
        fs::create_dir(&named).unwrap();
        let found = given_to(named.clone()).await.unwrap();
        assert_eq!(found.folder, games.path());
        assert_eq!(
            origin(&named).map(|origin| origin.folder),
            Some(games.path().to_path_buf())
        );

        let lacking: Vec<(&str, &[u8])> = vec![("readme.txt", b"hello")];
        fs::write(&path, torrent_of("readme", &lacking, 16)).unwrap();
        assert!(
            given_to(root)
                .await
                .is_err_and(|error| error.contains("holds no WoW client"))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_update_never_puts_in_place_a_wow_exe_of_another_game() {
        let mut theirs = files();
        theirs[0].1 = fake_executable(crate::core::Version {
            major: 4,
            minor: 3,
            patch: 4,
            build: 15595,
        });
        let (bytes, _seeding, seeder) = seeded(&theirs).await;
        let given = tempfile::tempdir().unwrap();
        let path = given.path().join("client.torrent");
        fs::write(&path, &bytes).unwrap();
        let client = tempfile::tempdir().unwrap();
        let root = client.path().to_path_buf();
        write(&root, &files());
        let config = Config {
            installations: vec![root.clone()],
            ..Config::default()
        };
        let cache = Cache::new();
        let mut state = downloading_from(&seeder, &cache).await;

        let _ = state.start(
            Request::Adopt {
                root: root.clone(),
                source: Source::File(path.clone()),
            },
            &config,
            Err("offline"),
        );
        let found = find_given(
            Source::File(path),
            root.clone(),
            cache.store.path().to_path_buf(),
            state.engine.clone(),
            Err("offline".to_owned()),
            state.peers.clone(),
        )
        .await;
        let _ = update(&mut state, Message::Found(1, found), &config, false);
        let found = job(&state).found.clone().unwrap();
        let checked =
            crate::core::torrent::check(&root, &found.torrent, &mut |_| ControlFlow::Continue(()))
                .map_err(|error| describe(&error));
        let _ = update(&mut state, Message::Checked(1, checked), &config, false);
        let Stage::Outdated(check) = &job(&state).stage else {
            panic!("no update");
        };
        assert!(check.different.contains(&0), "Wow.exe: {check:?}");

        let _ = update(&mut state, Message::Update, &config, false);
        let staging = job(&state).staging.clone().unwrap();
        let staged =
            crate::core::torrent::stage(&staging, &root, &found.torrent, &job(&state).files)
                .map_err(|error| describe(&error));
        let _ = update(&mut state, Message::Staged(1, staged), &config, false);
        let began = began(&state).await;
        let download = began.as_ref().unwrap().download.clone();
        let _ = update(&mut state, Message::Began(1, began), &config, false);
        ticked_through(&mut state, &config).await;
        let moved = moved(&state, download).await;
        assert!(update(&mut state, Message::Moved(1, moved), &config, false).is_none());

        let Stage::Failed(error) = &job(&state).stage else {
            panic!("put in place");
        };
        assert!(
            error.contains("not a client the launcher supports"),
            "{error}"
        );
        assert_eq!(
            fs::read(root.join("Wow.exe")).unwrap(),
            fake_executable(WOTLK)
        );
    }

    /// A job of `kind` at `stage`, of a torrent with 1 MB pieces, with the
    /// dialog shown unless `hidden`.
    fn at(kind: Kind, stage: Stage, progress: Progress, hidden: bool) -> State {
        let files: Vec<(&str, &[u8])> = vec![("Wow.exe", b"MZ"), ("Data/common.MPQ", b"x")];
        let bytes = torrent_of("WotLK", &files, 1024 * 1024);
        let torrent = Torrent::parse(&bytes).unwrap();
        let root = matches!(kind, Kind::Check).then(|| PathBuf::from("/games/WotLK"));

        State {
            job: Some(Job {
                id: 1,
                kind,
                saved: None,
                root,
                staging: Some("/games/WotLK/.download".into()),
                found: Some(Found {
                    bytes: Arc::new(bytes),
                    torrent: Arc::new(torrent),
                    folder: "/games/WotLK".into(),
                    engine: None,
                    saved: None,
                }),
                download: None,
                files: vec![0, 1],
                progress,
                stage,
                hidden,
                stopping: false,
            }),
            jobs: 1,
            ..State::new("torrents".into())
        }
    }

    #[test]
    fn the_dialog_says_how_a_download_goes() {
        let progress = Progress {
            done: 1536 * MB,
            total: 2048 * MB,
            peers: 12,
            connecting: 3,
            down: 2 * MB,
            up: 100 * 1024,
            ..Progress::default()
        };
        let state = at(
            Kind::Resume,
            Stage::Downloading { pieces: 2048 },
            progress.clone(),
            false,
        );

        let mut ui = simulator(state.dialog(false).unwrap());
        for line in [
            "75%",
            "1.5 GB of 2.0 GB",
            "12 connected, 3 connecting",
            "1536 of 2048",
            "2.0 MB/s down, 100 KB/s up",
            "About 5 minutes",
        ] {
            assert!(ui.find(line).is_ok(), "{line}");
        }
        drop(ui);

        let checking = at(
            Kind::Resume,
            Stage::Downloading { pieces: 2048 },
            Progress {
                checking: true,
                ..progress
            },
            false,
        );
        let mut ui = simulator(checking.dialog(false).unwrap());
        assert!(ui.find("Checking what is there already…").is_ok());
        drop(ui);

        let moving = at(
            Kind::Resume,
            Stage::Moving {
                moved: Arc::new(AtomicU64::new(0)),
            },
            Progress {
                done: 1024 * MB,
                total: 2048 * MB,
                ..Progress::default()
            },
            false,
        );
        let mut ui = simulator(moving.dialog(false).unwrap());
        assert!(ui.find("Putting the files in place…").is_ok());
        assert!(ui.find("50%").is_ok());
        assert!(ui.find("Stop").is_err(), "it finishes");
    }

    #[test]
    fn a_hidden_download_goes_on_behind_a_banner() {
        let progress = Progress {
            done: 512 * MB,
            total: 2048 * MB,
            down: 3 * MB,
            ..Progress::default()
        };
        let mut state = at(
            Kind::Resume,
            Stage::Downloading { pieces: 2048 },
            progress,
            false,
        );
        let config = Config::default();
        assert!(state.is_open());
        assert!(
            state
                .under_way()
                .is_some_and(|line| line == "Downloading WotLK: 25%")
        );

        let _ = update(&mut state, Message::Hide, &config, false);
        assert!(!state.is_open() && state.dialog(false).is_none());
        let messages: Vec<Message> = {
            let mut ui = simulator(state.banner(&config).unwrap());
            assert!(ui.find("Downloading WotLK: 25%, 3.0 MB/s").is_ok());
            let _ = ui.click("Show").unwrap();
            ui.into_messages().collect()
        };
        for message in messages {
            let _ = update(&mut state, message, &config, false);
        }
        assert!(state.is_open());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_download_cut_short_is_resumed_or_discarded_from_its_banner() {
        let (cache, into) = (Cache::new(), tempfile::tempdir().unwrap());
        let pending = ClientTorrent {
            name: "WotLK".into(),
            info_hash: "c0ffee".into(),
            source: "magnet:?xt=urn:btih:c0ffee".into(),
            folder: into.path().join("WotLK"),
            languages: Vec::new(),
        };
        let config = Config {
            downloads: vec![pending.clone()],
            ..Config::default()
        };
        let staging = pending.folder.join(".download");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("Wow.exe"), b"MZ").unwrap();
        let mut state = cache.state();

        let clicked = |state: &State, label: &str| -> Vec<Message> {
            let mut ui = simulator(state.banner(&config).unwrap());
            assert!(ui.find("WotLK is not fully downloaded yet.").is_ok());
            let _ = ui.click(label).unwrap();
            ui.into_messages().collect()
        };
        assert!(matches!(
            clicked(&state, "Resume download").as_slice(),
            [Message::Resume(hash)] if hash == "c0ffee"
        ));

        for message in clicked(&state, "Discard") {
            let _ = update(&mut state, message, &config, false);
        }
        let messages: Vec<Message> = {
            let mut ui = simulator(state.banner(&config).unwrap());
            assert!(
                ui.find("Delete what was downloaded of WotLK? It can't be resumed then.")
                    .is_ok()
            );
            let _ = ui.click("Delete").unwrap();
            ui.into_messages().collect()
        };
        let action = state.update(
            messages.into_iter().next().unwrap(),
            &config,
            Err("offline"),
            false,
            &mut Errors::default(),
        );
        assert!(
            matches!(action.instruction, Some(Instruction::Discarded(hash)) if hash == "c0ffee")
        );

        // What the task does.
        tidy(None, None, After::Remove(staging.clone()))
            .await
            .unwrap();
        assert!(listed(into.path()).is_empty(), "nor the folder made for it");

        // The copy of its torrent goes with it, unless a client is checked
        // against it.
        assert_eq!(
            state.kept_unless_shared(&config, "c0ffee"),
            Some(state.kept("c0ffee"))
        );
        let shared = Config {
            torrents: BTreeMap::from([(PathBuf::from("/games/WotLK"), pending)]),
            ..Config::default()
        };
        assert_eq!(state.kept_unless_shared(&shared, "c0ffee"), None);
    }

    #[test]
    fn copies_kept_with_the_data_by_earlier_builds_move_to_the_cache() {
        let (data, cache) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let old = data.path().join("old_launcher/torrents");
        let store = cache.path().join("old_launcher/torrents");
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join("c0ffee.torrent"), b"old").unwrap();
        fs::write(old.join("decaf.torrent"), b"old too").unwrap();
        fs::write(old.join("notes.txt"), b"something else").unwrap();
        fs::create_dir_all(&store).unwrap();
        fs::write(store.join("decaf.torrent"), b"cached").unwrap();

        relocate(&old, &store);

        assert_eq!(fs::read(store.join("c0ffee.torrent")).unwrap(), b"old");
        assert_eq!(
            fs::read(store.join("decaf.torrent")).unwrap(),
            b"cached",
            "the cache's copy stays"
        );
        assert!(!old.join("decaf.torrent").exists(), "the old one goes");
        assert!(old.join("notes.txt").exists(), "only torrents move");
        assert!(old.exists(), "not empty, so kept");

        fs::remove_file(old.join("notes.txt")).unwrap();
        relocate(&old, &store);
        assert!(!data.path().join("old_launcher").exists(), "empty, gone");

        relocate(&old, &store);
        assert!(
            store.join("c0ffee.torrent").is_file(),
            "nothing to do, nothing done"
        );
    }

    #[test]
    fn on_the_way_out_only_a_client_not_said_yes_to_is_deleted() {
        let games = tempfile::tempdir().unwrap();
        let staging = games.path().join("WotLK/.download");
        let downloaded = || {
            fs::create_dir_all(staging.join("Data")).unwrap();
            fs::write(staging.join("Wow.exe"), b"MZ").unwrap();
        };
        let source = Source::Magnet("magnet:?xt=urn:btih:c0ffee".to_owned());

        // Its Wow.exe downloaded, not yet said yes to.
        downloaded();
        let mut state = at(
            Kind::New { source },
            Stage::Identifying,
            Progress::default(),
            false,
        );
        state.job.as_mut().unwrap().staging = Some(staging.clone());
        state.abandon();
        assert!(state.job.is_none());
        assert!(
            !games.path().join("WotLK").exists(),
            "nor the folder made for it"
        );

        // A client being downloaded, and an update, resume.
        for kind in [Kind::Resume, Kind::Check] {
            downloaded();
            let mut state = at(
                kind,
                Stage::Downloading { pieces: 2 },
                Progress::default(),
                false,
            );
            let job = state.job.as_mut().unwrap();
            job.staging = Some(staging.clone());
            job.files = vec![0, 1];
            state.abandon();
            assert!(staging.join("Wow.exe").is_file(), "kept to resume");
        }
    }

    #[test]
    fn stopping_while_waiting_stops_once_what_was_awaited_comes() {
        let config = Config::default();
        for (stage, came) in [
            (
                Stage::Beginning,
                Message::Began(1, Err("the session closed".into())),
            ),
            (
                Stage::Opening,
                Message::Identified(1, Err("no Wow.exe".into())),
            ),
            (Stage::Staging, Message::Staged(1, Ok(()))),
        ] {
            let mut state = at(Kind::Check, stage, Progress::default(), false);

            let _ = update(&mut state, Message::Stop, &config, false);
            assert!(job(&state).stopping);
            let mut ui = simulator(state.dialog(false).unwrap());
            assert!(ui.find("Stopping…").is_ok());
            drop(ui);

            let _ = update(&mut state, came, &config, false);
            assert!(state.job.is_none());
        }
    }

    #[test]
    fn a_failed_job_says_why_until_closed() {
        let mut state = at(
            Kind::Check,
            Stage::Failed("Could not get the torrent: nobody sharing it was found".into()),
            Progress::default(),
            false,
        );
        assert!(!state.is_running(), "another can start");

        let messages: Vec<Message> = {
            let mut ui = simulator(state.dialog(false).unwrap());
            assert!(
                ui.find("Could not get the torrent: nobody sharing it was found")
                    .is_ok()
            );
            let _ = ui.click("Close").unwrap();
            ui.into_messages().collect()
        };
        for message in messages {
            let _ = update(&mut state, message, &Config::default(), false);
        }
        assert!(state.job.is_none());
    }

    #[test]
    fn a_new_clients_folder_is_named_after_its_torrent() {
        assert_eq!(
            folder_name("World of Warcraft 3.3.5a", "c0ffee"),
            "World of Warcraft 3.3.5a"
        );
        assert_eq!(
            folder_name("WoW: WotLK <enUS>?", "c0ffee"),
            "WoW_ WotLK _enUS__"
        );
        assert_eq!(folder_name("../..", "c0ffee"), ".._");
        assert_eq!(folder_name(" .. ", "c0ffee"), "c0ffee");
        assert_eq!(folder_name("client. ", "c0ffee"), "client");
    }

    /// The HTTP client the launcher fetches with, as it starts (see
    /// [`App::new`](crate::App::new)).
    fn http() -> reqwest::Client {
        let _ = rustls::crypto::ring::default_provider().install_default();
        crate::http_client().build().unwrap()
    }

    /// The torrent of a 3.3.5a client given for the tests that join its
    /// swarm: a magnet link, or a `.torrent`'s address or file.
    fn real_torrent() -> Source {
        Source::parse(
            &std::env::var("OLD_LAUNCHER_TEST_TORRENT")
                .expect("OLD_LAUNCHER_TEST_TORRENT names the torrent of a 3.3.5a client"),
        )
    }

    /// The real engine, as the launcher starts it, and a state downloading
    /// with it into `cache`.
    async fn real(cache: &Cache) -> State {
        let engine = Engine::start().await.unwrap();
        println!("Listening for peers on {}", address(&engine));

        State {
            engine: Some(engine),
            ..cache.state()
        }
    }

    /// Asks how a download goes, printing it now and then, until it is
    /// done, for at most an hour.
    async fn followed(state: &mut State, config: &Config) {
        for tick in 0..7200 {
            let _ = update(state, Message::Tick, config, false);
            if !matches!(
                job(state).stage,
                Stage::Identifying | Stage::Downloading { .. }
            ) {
                return;
            }
            if tick % 10 == 0 {
                let progress = &job(state).progress;
                println!(
                    "{}{} of {}, {} peers ({} connecting), {}/s down, {}/s up",
                    if progress.checking { "Checking: " } else { "" },
                    size(progress.done),
                    size(progress.total),
                    progress.peers,
                    progress.connecting,
                    size(progress.down),
                    size(progress.up)
                );
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!("not downloaded in an hour");
    }

    /// `cargo test --lib download::tests::a_real_torrent -- --ignored
    /// --nocapture`: downloads the `Wow.exe` of the client of
    /// `OLD_LAUNCHER_TEST_TORRENT`, alone, from the peers the launcher
    /// finds, into a staging folder, reads its version, and turns the
    /// client down.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "joins the swarm of the torrent in $OLD_LAUNCHER_TEST_TORRENT"]
    async fn a_real_torrent_shows_its_client_before_downloading_it() {
        let (cache, into) = (Cache::new(), tempfile::tempdir().unwrap());
        let config = Config::default();
        let mut state = real(&cache).await;

        let _ = state.start(
            Request::New {
                source: real_torrent(),
                into: into.path().to_path_buf(),
            },
            &config,
            Ok(&http()),
        );
        let found = find_new(
            real_torrent(),
            into.path().to_path_buf(),
            state.engine.clone(),
            Ok(http()),
            Vec::new(),
        )
        .await;
        let torrent = found.as_ref().unwrap().torrent.clone();
        println!(
            "{}: {}, {}, client in {:?}",
            torrent.name,
            counted(torrent.files.len(), "file"),
            size(torrent.size()),
            torrent.client().unwrap()
        );
        let _ = update(&mut state, Message::Found(1, found), &config, false);
        let began = began(&state).await;
        let _ = update(&mut state, Message::Began(1, began), &config, false);
        followed(&mut state, &config).await;
        assert!(matches!(job(&state).stage, Stage::Opening));

        let staging = job(&state).staging.clone().unwrap();
        let opened = Installation::open(within(&staging, &torrent.client().unwrap()))
            .map_err(|error| describe(&error));
        println!("Wow.exe read: {opened:?}");
        let _ = update(&mut state, Message::Identified(1, opened), &config, false);
        let Stage::Confirming { installation, .. } = &job(&state).stage else {
            match &job(&state).stage {
                Stage::Failed(error) => panic!("not offered: {error}"),
                _ => panic!("not offered"),
            }
        };
        assert!(installation.version().is_wotlk());

        let (after, download) = (job(&state).after(), job(&state).download.clone());
        let _ = update(&mut state, Message::Stop, &config, false);
        tidy(state.engine.clone(), download, after).await.unwrap();
        assert!(!staging.exists(), "turned down, nothing is left");
        assert_eq!(fs::read_dir(into.path()).unwrap().count(), 0);
    }

    /// `cargo test --release --lib download::tests::a_real_client_is_checked
    /// -- --ignored --nocapture`, with `OLD_LAUNCHER_TEST_TORRENT_CLIENT`
    /// too: the folder that torrent's files went in, which the check notes
    /// what it found in. Compares it with the torrent twice, saying how long
    /// each took and what an update would download.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "joins the swarm of $OLD_LAUNCHER_TEST_TORRENT for a magnet link, and reads \
                $OLD_LAUNCHER_TEST_TORRENT_CLIENT whole"]
    async fn a_real_client_is_checked_against_its_torrent() {
        let folder =
            PathBuf::from(std::env::var("OLD_LAUNCHER_TEST_TORRENT_CLIENT").expect(
                "OLD_LAUNCHER_TEST_TORRENT_CLIENT is the folder the torrent's files went in",
            ));
        let (bytes, _engine) = torrent::fetch(real_torrent(), None, Ok(http()), Vec::new())
            .await
            .unwrap();
        let torrent = Arc::new(Torrent::parse(&bytes).unwrap());

        for pass in ["First", "Second"] {
            let started = std::time::Instant::now();
            let (folder, checked) = (folder.clone(), torrent.clone());
            let check = blocking(move || {
                crate::core::torrent::check(&folder, &checked, &mut |_| ControlFlow::Continue(()))
            })
            .await
            .unwrap()
            .unwrap();
            let named = |files: &[usize]| -> Vec<String> {
                files
                    .iter()
                    .map(|index| torrent.files[*index].path.clone())
                    .collect()
            };
            println!(
                "{pass} check, {:.1?}: {} to download; missing {:?}, different {:?}",
                started.elapsed(),
                size(check.bytes(&torrent)),
                named(&check.missing),
                named(&check.different)
            );
        }
    }

    /// `cargo test --release --lib download::tests::a_real_client_is_updated
    /// -- --ignored --nocapture`, with `OLD_LAUNCHER_TEST_TORRENT_CLIENT` a
    /// copy of the client that torrent downloads, changed: checks it for
    /// updates and updates it as the dialog does, through a staging folder,
    /// from the peers the launcher finds, saying how it goes, then checks
    /// it again.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "joins the swarm of $OLD_LAUNCHER_TEST_TORRENT and updates the folder in \
                $OLD_LAUNCHER_TEST_TORRENT_CLIENT"]
    async fn a_real_client_is_updated_from_its_torrent() {
        let root = PathBuf::from(
            std::env::var("OLD_LAUNCHER_TEST_TORRENT_CLIENT")
                .expect("OLD_LAUNCHER_TEST_TORRENT_CLIENT is a copy of the client to update"),
        );
        let cache = Cache::new();
        let (bytes, _engine) = torrent::fetch(real_torrent(), None, Ok(http()), Vec::new())
            .await
            .unwrap();
        let torrent = Arc::new(Torrent::parse(&bytes).unwrap());
        let mut state = real(&cache).await;
        let kept = state.kept(&torrent.info_hash);
        fs::write(&kept, &bytes).unwrap();
        let saved = ClientTorrent {
            name: torrent.name.clone(),
            info_hash: torrent.info_hash.clone(),
            source: real_torrent().to_string(),
            folder: root.clone(),
            languages: Vec::new(),
        };
        let config = Config {
            installations: vec![root.clone()],
            torrents: BTreeMap::from([(root.clone(), saved.clone())]),
            ..Config::default()
        };

        let _ = state.start(Request::Check(root.clone()), &config, Ok(&http()));
        let found = find_again(
            saved,
            root.clone(),
            cache.store.path().to_path_buf(),
            state.engine.clone(),
            Ok(http()),
            Vec::new(),
        )
        .await;
        let _ = update(&mut state, Message::Found(1, found), &config, false);
        let checking = |root: PathBuf, torrent: Arc<Torrent>| {
            blocking(move || {
                crate::core::torrent::check(&root, &torrent, &mut |_| ControlFlow::Continue(()))
            })
        };
        let started = std::time::Instant::now();
        let checked = checking(root.clone(), torrent.clone()).await;
        println!("Checked in {:.1?}", started.elapsed());
        let _ = update(&mut state, Message::Checked(1, checked), &config, false);
        let Stage::Outdated(check) = &job(&state).stage else {
            panic!("nothing to update");
        };
        let named = |files: &[usize]| -> Vec<String> {
            files
                .iter()
                .map(|index| torrent.files[*index].path.clone())
                .collect()
        };
        println!(
            "To download, {}: missing {:?}, different {:?}",
            size(check.bytes(&torrent)),
            named(&check.missing),
            named(&check.different)
        );

        let _ = update(&mut state, Message::Update, &config, false);
        let staging = job(&state).staging.clone().unwrap();
        let staged = {
            let (staging, root, torrent, files) = (
                staging.clone(),
                root.clone(),
                torrent.clone(),
                job(&state).files.clone(),
            );
            blocking(move || crate::core::torrent::stage(&staging, &root, &torrent, &files)).await
        };
        let _ = update(&mut state, Message::Staged(1, staged), &config, false);
        let began = began(&state).await;
        let download = began.as_ref().unwrap().download.clone();
        let _ = update(&mut state, Message::Began(1, began), &config, false);
        let Stage::Downloading { pieces } = job(&state).stage else {
            panic!("not downloading");
        };

        let mut shown = false;
        for tick in 0..7200 {
            let _ = update(&mut state, Message::Tick, &config, false);
            if !matches!(job(&state).stage, Stage::Downloading { .. }) {
                break;
            }
            let progress = job(&state).progress.clone();
            let had = pieces.saturating_sub(
                usize::try_from((progress.total - progress.done).div_ceil(torrent.piece_length))
                    .unwrap(),
            );
            let lines = [
                format!(
                    "{} connected, {} connecting",
                    progress.peers, progress.connecting
                ),
                format!("{had} of {pieces}"),
                format!("{}/s down, {}/s up", size(progress.down), size(progress.up)),
            ];
            if tick % 4 == 0 {
                println!(
                    "{}{} of {}, {}",
                    if progress.checking {
                        "Checking what is there: "
                    } else {
                        ""
                    },
                    size(progress.done),
                    size(progress.total),
                    lines.join(", ")
                );
            }
            // What the dialog says, once there is something to say.
            if !shown && !progress.checking && progress.peers > 0 && progress.down > 0 {
                let mut ui = simulator(state.dialog(false).unwrap());
                for line in &lines {
                    assert!(ui.find(line.as_str()).is_ok(), "the dialog says {line}");
                }
                println!("The dialog says: {}", lines.join("; "));
                shown = true;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        assert!(
            matches!(job(&state).stage, Stage::Moving { .. }),
            "downloaded in an hour"
        );

        let moved = moved(&state, download).await;
        let Some(Instruction::Updated { notice, .. }) =
            update(&mut state, Message::Moved(1, moved), &config, false)
        else {
            panic!("not updated");
        };
        println!("{notice}");
        assert!(!staging.exists(), "nothing left beside it");

        let started = std::time::Instant::now();
        let again = checking(root, torrent).await.unwrap().unwrap();
        println!("Checked again in {:.1?}: {again:?}", started.elapsed());
        assert!(again.is_current());
    }

    #[test]
    fn time_left_is_said_roughly() {
        assert_eq!(duration(30), "Less than a minute");
        assert_eq!(duration(90), "About 2 minutes");
        assert_eq!(duration(3600), "About 1 hour");
        assert_eq!(duration(5400), "About 1 hour 30 minutes");
        assert_eq!(duration(2 * 3600 + 60), "About 2 hours 1 minute");
    }
}
