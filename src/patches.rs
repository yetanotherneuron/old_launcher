//! Looking at the client's patches in the background: which archives the
//! game loads and in what order, what each says (see
//! [`patches`](crate::core::patches)), and what they add up to.
//!
//! One look at a time per client folder, on a blocking thread, whenever a
//! client opens, its files change, F5 is pressed, or the window gets the
//! focus back and the `Data` folders' listing changed since the last look. A
//! look asked for meanwhile comes after the one under way. Every archive is
//! read once until it changes (the facts of each are kept in the launcher's
//! cache, see [`cache`]), so a look at a client already seen reads small
//! files and builds the graph.
//!
//! The look reports its progress without a clock in the window: the thread
//! takes the time when it starts, and every message it sends says how long
//! it has been (an archive started, every [`FILES`] files read inside one,
//! an archive ended). The banner appears with the first message that says
//! more than [`SLOW`] has passed, and stays until the look ends; one that
//! ends sooner shows nothing.
//!
//! Until the first look at a client ends, [`State::holds_back`] is true:
//! Play, the mods' buttons and the file integrity check wait. A look that
//! fails as a whole is reported and releases them: the player is never
//! locked out. An archive that can't be read is noted and left out, and the
//! rest goes on.
//!
//! A look also reads the archives the folders hold that the game doesn't
//! load (backups, switched off ones, names it never lists, but not
//! Blizzard's own `base-` and `backup-`) through the same cache, to tell
//! which needs of the archives loaded they would meet. That, the case-twins
//! and the archives that can't be read are what the file integrity check says
//! of the patches ([`Findings`]), from memory.
//!
//! Only the graph of the client shown is kept.

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use iced::task::{Straw, sipper};
use iced::widget::{container, row, text};
use iced::{Center, Element, Function, Task, Widget};
use sha2::{Digest, Sha256};

use crate::activity::activity;
use crate::core::mods::{Mods, Problem, Status};
use crate::core::mpq::{Key, key};
use crate::core::patches::facts::{Entry, Opened, Source};
use crate::core::patches::order::{Reason, order_with};
use crate::core::patches::{Cache, Facts, Graph, Kind, Layer, Order, strip};
use crate::core::{Error, Locale};
use crate::errors::Errors;
use crate::{blocking, describe, style};

/// What the hint of a button says while the client's patches are looked at.
pub const LOOKING: &str = "Looking at this client's patches… Wait until it's done.";

/// How long a look goes on before the banner says so.
pub const SLOW: Duration = Duration::from_secs(2);

/// How many files read inside an archive make a progress message.
pub const FILES: usize = 200;

/// Where the launcher keeps what it reads of the archives: in its cache,
/// one folder per client.
pub fn cache() -> Cache {
    Cache::at(
        dirs::cache_dir()
            .unwrap_or_default()
            .join("old_launcher")
            .join("mpq"),
    )
}

/// The patches of each client folder looked at, and the look under way.
pub struct State {
    cache: Cache,
    /// The client folder shown: the only one whose result is kept.
    shown: Option<PathBuf>,
    clients: BTreeMap<PathBuf, Client>,
}

/// What is known and being done about a client folder's patches.
struct Client {
    /// The language it plays in, as of the last time a look was asked for.
    locale: Locale,
    /// The look under way.
    running: Option<Running>,
    /// Whether another was asked for meanwhile: the folder may have changed.
    again: bool,
    /// What the last look found.
    found: Option<Arc<Scan>>,
}

/// A look under way.
#[derive(Default)]
struct Running {
    /// Whether it went on for more than [`SLOW`], so the banner shows.
    slow: bool,
    /// What it last said it was at.
    at: Option<Progress>,
}

/// Where a look is, and how long it has been going.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// The time since the look started.
    pub elapsed: Duration,
    /// The archive it is at, from the game folder.
    pub archive: String,
    /// Which one that is, counting from 1.
    pub index: usize,
    /// How many the game loads.
    pub total: usize,
    /// What happened to it.
    pub step: Step,
}

/// What happened to an archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Its turn came.
    Started,
    /// This many files of it were read so far.
    Files(usize),
    /// It is done.
    Ended(How),
}

/// How an archive was done with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    /// What the launcher kept of it still held.
    Kept,
    /// It was read.
    Read,
    /// It couldn't be, and is left out.
    Unreadable,
}

/// An archive of the `Data` folders that couldn't be read, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    /// Its path from the game folder.
    pub path: String,
    /// What is wrong with it.
    pub why: String,
}

/// The files of the `Data` folders that can be archives: path, size and
/// modification time in nanoseconds. A change in it is a reason to look
/// again.
pub type Listing = Vec<(String, u64, u64)>;

/// What a look found out about a client's patches.
pub struct Scan {
    /// The archives in the order the game loads them, and those it doesn't.
    pub order: Order,
    /// What the loaded archives add up to.
    pub graph: Graph,
    /// The archives that couldn't be read.
    pub unreadable: Vec<Unreadable>,
    /// The needs that nothing loaded meets but an archive aside (a backup, a
    /// switched off one) would.
    pub unmet: Vec<Unmet>,
    /// The `Data` folders as they were when the look started.
    listing: Listing,
}

impl std::fmt::Debug for Scan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scan")
            .field("order", &self.order)
            .field("unreadable", &self.unreadable)
            .finish_non_exhaustive()
    }
}

/// A need of an archive that a file in the folder the game doesn't load
/// would meet: a backup of what it needs, say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unmet {
    /// The archive whose file needs, from the game folder.
    pub archive: String,
    /// What that file is.
    pub kind: Kind,
    /// The archive in the folder that would meet it, as it is called there.
    pub instead: String,
    /// Why the game doesn't load that one.
    pub reason: Reason,
}

/// What the integrity check says of the patches, before it has the words:
/// the raw pieces.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Findings {
    /// Archives that differ only in case, together.
    pub twins: Vec<Vec<String>>,
    /// Needs that nothing loaded meets but an archive aside would.
    pub unmet: Vec<Unmet>,
    /// Archives that couldn't be read.
    pub unreadable: Vec<Unreadable>,
    /// What is wrong with the files of the mods installed, in words. The
    /// patches' look knows nothing of it: the check fills it.
    pub mods: Vec<String>,
}

impl Findings {
    /// What is wrong, a line each. `owner` tells the name of the mod that
    /// installed the archive at a path (in lowercase), if any did: a mod's
    /// archive is named by it, not called custom.
    pub fn lines(&self, owner: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
        let mut lines: Vec<String> = self
            .twins
            .iter()
            .map(|twins| {
                format!(
                    "{} differ only in case. Windows would allow only one of them, and the game \
                     may load either: rename one, or remove it.",
                    list(twins)
                )
            })
            .collect();

        // By who needs and what would meet it, the kinds of file together.
        let mut grouped: BTreeMap<(&str, &str), (BTreeSet<Kind>, &Reason)> = BTreeMap::new();
        for unmet in &self.unmet {
            grouped
                .entry((&unmet.archive, &unmet.instead))
                .or_insert_with(|| (BTreeSet::new(), &unmet.reason))
                .0
                .insert(unmet.kind);
        }
        for ((archive, instead), (kinds, reason)) in grouped {
            let kinds = list(&kinds.into_iter().map(kind).collect::<Vec<_>>());
            let who = match owner(&archive.to_ascii_lowercase()) {
                Some(name) => format!("The {kinds} of “{name}”"),
                None => format!("Your custom {archive}'s {kinds}"),
            };
            let (needed, state) = match reason {
                Reason::Backup => (original(instead), format!("it is backed up as {instead}")),
                Reason::Disabled => (
                    original(instead),
                    format!("it is switched off as {instead}"),
                ),
                Reason::NotListed | Reason::Unreadable(_) => (
                    instead,
                    "the game never looks at a name like that".to_owned(),
                ),
            };

            lines.push(format!("{who} need {needed}, which isn't loaded: {state}."));
        }

        lines.extend(self.unreadable.iter().map(|archive| {
            format!(
                "{} couldn't be read, so what it changes is unknown.",
                archive.path
            )
        }));
        lines.extend(self.mods.iter().cloned());

        lines
    }
}

/// What is wrong with the files of the mods in `list` that `held` says a
/// client folder holds, a line each: those that are missing or not as
/// released, which Repair puts back.
pub fn mods_lines(mods: &Mods, held: &BTreeMap<String, Status>) -> Vec<String> {
    let mut lines = Vec::new();

    for listed in &mods.mods {
        let Some(status) = held.get(&listed.id) else {
            continue;
        };
        for problem in &status.problems {
            let (files, what) = match problem {
                Problem::Missing(files) => (files, "missing"),
                Problem::Changed(files) => (files, "not as released"),
                _ => continue,
            };
            // The first few are named.
            let named: Vec<String> = files
                .iter()
                .take(3)
                .cloned()
                .chain((files.len() > 3).then(|| format!("{} more", files.len() - 3)))
                .collect();
            let (verb, them) = if files.len() == 1 {
                ("is", "it")
            } else {
                ("are", "them")
            };

            lines.push(format!(
                "“{}”'s {} {verb} {what}: Repair, under Mods, puts {them} back.",
                listed.name,
                list(&named)
            ));
        }
    }

    lines
}

/// `names` as a sentence lists them: `a, b and c`.
fn list(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [only] => only.clone(),
        [first @ .., last] => format!("{} and {last}", first.join(", ")),
    }
}

/// How a message calls the files of a kind.
fn kind(kind: Kind) -> String {
    match kind {
        Kind::Tables => "tables",
        Kind::Models => "models",
        Kind::Textures => "textures",
        Kind::Sounds => "sounds",
        Kind::Interface => "interface",
        Kind::World => "world",
        Kind::Other => "files",
    }
    .to_owned()
}

/// The archive that `aside`, a backup or a switched off archive, was before:
/// `Data/patch-F.mpq.backup-1` was `Data/patch-F.mpq`.
fn original(aside: &str) -> &str {
    aside
        .to_ascii_lowercase()
        .find(".mpq.")
        .and_then(|at| aside.get(..at + ".mpq".len()))
        .unwrap_or(aside)
}

/// What happens to the looks.
#[derive(Debug, Clone)]
pub enum Message {
    /// The look at this client folder got somewhere.
    Progressed(PathBuf, Progress),
    /// The look at this client folder ended.
    Looked(PathBuf, Result<Arc<Scan>, String>),
    /// The `Data` folders of this client folder, listed after the window got
    /// the focus back.
    Listed(PathBuf, Result<Listing, String>),
}

impl State {
    /// A state keeping what it reads in `cache`.
    pub fn new(cache: Cache) -> Self {
        Self {
            cache,
            shown: None,
            clients: BTreeMap::new(),
        }
    }

    /// Looks at the patches of the client folder at `root`, which plays in
    /// `locale` and is the one shown from now on: once at a time, a look
    /// asked for meanwhile coming after the one under way.
    pub fn look(&mut self, root: &Path, locale: Locale) -> Task<Message> {
        if self.shown.as_deref() != Some(root) {
            // Only the client shown keeps its graph.
            for client in self.clients.values_mut() {
                client.found = None;
            }
            self.shown = Some(root.to_path_buf());
        }

        let client = self
            .clients
            .entry(root.to_path_buf())
            .or_insert_with(|| Client {
                locale,
                running: None,
                again: false,
                found: None,
            });
        client.locale = locale;

        if client.running.is_some() {
            client.again = true;
            return Task::none();
        }

        self.start(root)
    }

    /// Starts a look at the client folder at `root`, which none is under
    /// way at.
    fn start(&mut self, root: &Path) -> Task<Message> {
        let Some(client) = self.clients.get_mut(root) else {
            return Task::none();
        };
        log::info!("Looking at the patches of {}", root.display());
        client.running = Some(Running::default());
        client.again = false;

        let (progressed, looked) = (root.to_path_buf(), root.to_path_buf());

        Task::sip(
            scan(root.to_path_buf(), client.locale, self.cache.clone()),
            Message::Progressed.with(progressed),
            Message::Looked.with(looked),
        )
    }

    /// Looks at the `Data` folders of the client folder at `root` after the
    /// window got the focus back, to look at its patches again if they
    /// changed. Not while a look is under way: it will tell.
    pub fn regained(&self, root: &Path, locale: Locale) -> Task<Message> {
        if self.holds_back(root) {
            return Task::none();
        }
        let root = root.to_path_buf();

        Task::perform(
            blocking({
                let root = root.clone();
                move || listing(&root, locale)
            }),
            Message::Listed.with(root),
        )
    }

    /// The client folder at `root` was removed from the launcher: what is
    /// kept of it goes. A look under way when this is called drops its own
    /// when it ends.
    pub fn forget(&mut self, root: &Path) {
        let _ = self.clients.remove(root);
        if self.shown.as_deref() == Some(root) {
            self.shown = None;
        }
        self.cache.forget(root);
    }

    /// Handles what happened to the looks; failures go to `errors`.
    pub fn update(&mut self, message: Message, errors: &mut Errors) -> Task<Message> {
        match message {
            Message::Progressed(root, progress) => {
                if let Some(running) = self
                    .clients
                    .get_mut(&root)
                    .and_then(|client| client.running.as_mut())
                {
                    running.slow |= progress.elapsed > SLOW;
                    running.at = Some(progress);
                }

                Task::none()
            }
            Message::Looked(root, result) => {
                let Some(client) = self.clients.get_mut(&root) else {
                    // Removed meanwhile: the look wrote what it was told to
                    // drop.
                    self.cache.forget(&root);
                    return Task::none();
                };
                client.running = None;
                client.found = None;

                let shown = self.shown.as_deref() == Some(root.as_path());
                match result {
                    Ok(scan) if shown => {
                        log::info!(
                            "Looked at the patches of {}: {} loaded, {} not read",
                            root.display(),
                            scan.order.loaded.len(),
                            scan.unreadable.len()
                        );
                        client.found = Some(scan);
                    }
                    Ok(_) => {}
                    Err(error) => errors.report(format!(
                        "Could not look at the patches of {}: {error}",
                        root.display()
                    )),
                }

                if client.again && shown {
                    self.start(&root)
                } else {
                    client.again = false;
                    Task::none()
                }
            }
            Message::Listed(root, Ok(listed)) => {
                let Some(client) = self.clients.get(&root) else {
                    return Task::none();
                };
                let changed = client
                    .found
                    .as_ref()
                    .is_none_or(|scan| scan.listing != listed);

                if changed && client.running.is_none() && self.shown.as_deref() == Some(&root) {
                    log::info!("The Data folders of {} changed", root.display());
                    let locale = client.locale;
                    self.look(&root, locale)
                } else {
                    Task::none()
                }
            }
            Message::Listed(root, Err(error)) => {
                log::warn!("Can't list the Data folders of {}: {error}", root.display());
                Task::none()
            }
        }
    }

    /// Whether the client folder at `root` is being looked at: starting its
    /// game, and what depends on the patches, waits.
    pub fn holds_back(&self, root: &Path) -> bool {
        self.clients
            .get(root)
            .is_some_and(|client| client.running.is_some())
    }

    /// The client folders being looked at.
    pub fn waiting(&self) -> BTreeSet<PathBuf> {
        self.clients
            .iter()
            .filter(|(_, client)| client.running.is_some())
            .map(|(root, _)| root.clone())
            .collect()
    }

    /// What the last look at the client folder at `root` found.
    pub fn scan(&self, root: &Path) -> Option<&Scan> {
        self.clients.get(root)?.found.as_deref()
    }

    /// What the last look at the client folder at `root` found, once the
    /// look is over: none while another is under way.
    pub fn settled(&self, root: &Path) -> Option<Arc<Scan>> {
        let client = self.clients.get(root)?;

        client
            .running
            .is_none()
            .then(|| client.found.clone())
            .flatten()
    }

    /// What the client folder at `root`'s archives add up to.
    #[allow(dead_code, reason = "the install gate reads it")]
    pub fn graph(&self, root: &Path) -> Option<&Graph> {
        self.scan(root).map(|scan| &scan.graph)
    }

    /// The order the game loads the archives of the client folder at `root`
    /// in.
    #[allow(dead_code, reason = "the install gate reads it")]
    pub fn order(&self, root: &Path) -> Option<&Order> {
        self.scan(root).map(|scan| &scan.order)
    }

    /// What is amiss with the patches of the client folder at `root`, in the
    /// pieces the integrity check turns into sentences.
    pub fn findings(&self, root: &Path) -> Option<Findings> {
        let scan = self.scan(root)?;

        Some(Findings {
            twins: scan.order.twins().into_iter().map(<[_]>::to_vec).collect(),
            unmet: scan.unmet.clone(),
            unreadable: scan.unreadable.clone(),
            mods: Vec::new(),
        })
    }

    /// The banner above the addons about the look at the client folder at
    /// `root`, once it went on for long enough to be worth saying.
    pub fn banner(&self, root: &Path) -> Option<Element<'_, Message>> {
        let running = self.clients.get(root)?.running.as_ref()?;

        if !running.slow {
            return None;
        }
        let said = match &running.at {
            Some(at) => format!(
                "Looking at this client's patches: {}, {} of {}…",
                at.archive, at.index, at.total
            ),
            None => "Looking at this client's patches…".to_owned(),
        };

        Some(
            container(
                row![activity(160.0, 6.0), text(said)]
                    .spacing(10)
                    .align_y(Center),
            )
            .padding([6, 12])
            .style(style::banner(style::Tone::Warning))
            .boxed(),
        )
    }
}

/// Looks at the patches of the client folder at `root` on a blocking thread,
/// saying how far it got.
fn scan(root: PathBuf, locale: Locale, cache: Cache) -> impl Straw<Arc<Scan>, Progress, String> {
    sipper(async move |mut progress| {
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let work = tokio::task::spawn_blocking(move || {
            look(&root, locale, &cache, &|progress| {
                let _ = sender.send(progress);
            })
        });

        // The thread's end drops the sender, and so ends this.
        while let Some(said) = receiver.recv().await {
            progress.send(said).await;
        }

        work.await.map_err(|error| error.to_string())?.map(Arc::new)
    })
}

/// Looks at the patches of the client folder at `root`, which plays in
/// `locale`, keeping what it reads of each archive in `cache`, and telling
/// `report` how it goes.
///
/// Each archive's facts come from `cache` unless it changed. Those of an
/// archive with a list of its files need nothing else; one without can only
/// be named by the others, so the archives are done in two rounds: first the
/// ones with a list (and any kept), then, with the names they give, the ones
/// without, kept under a digest of those names.
pub fn look(
    root: &Path,
    locale: Locale,
    cache: &Cache,
    report: &dyn Fn(Progress),
) -> Result<Scan, String> {
    let started = Instant::now();
    let listing = listing(root, locale).map_err(|error| describe(&error))?;

    // What the cache keeps of an archive with a list of its files is told
    // apart by the language alone: Blizzard's archives are stripped by it.
    let kept_as = locale.code();
    let mut kept = BTreeMap::new();
    let order = order_with(root, locale, |full| {
        let path = relative(root, full);

        // One that is kept opened once; nothing reads it again.
        if let Some(path) = path
            && let Some(facts) = cache.archive(root, &path, kept_as)
        {
            let _ = kept.insert(path, facts);
            return Ok(());
        }

        Opened::open(full)
            .map(drop)
            .map_err(|error| describe(&error))
    });

    let paths: Vec<(u32, String)> = order
        .loaded
        .iter()
        .flat_map(|slot| slot.paths.iter().map(|path| (slot.priority, path.clone())))
        .collect();
    let total = paths.len();
    let index = Cell::new(0);
    let say = |archive: &str, step| {
        report(Progress {
            elapsed: started.elapsed(),
            archive: archive.to_owned(),
            index: index.get(),
            total,
            step,
        });
    };
    let begin = |archive: &str| {
        index.set(index.get() + 1);
        say(archive, Step::Started);
    };
    // The facts of an archive read now, counting the files it reads.
    let compute = |path: &str, opened: &Opened, known: &[String]| -> Facts {
        let counted = Counting::new(opened, |files| say(path, Step::Files(files)));

        strip(path, Facts::of(&counted, known), locale)
    };

    let mut facts: BTreeMap<String, Facts> = BTreeMap::new();
    let mut unreadable = Vec::new();
    let mut deferred = Vec::new();

    // First round.
    for (_, path) in &paths {
        if let Some(own) = kept.remove(path) {
            begin(path);
            let _ = facts.insert(path.clone(), own);
            say(path, Step::Ended(How::Kept));
            continue;
        }

        match Opened::open(&root.join(path)) {
            Ok(opened) if !has_listfile(&opened) => deferred.push(path.clone()),
            Ok(opened) => {
                begin(path);
                let mut read = false;
                let done = cache.of_archive(root, path, kept_as, || {
                    read = true;
                    Ok(compute(path, &opened, &[]))
                });
                finish(path, done, read, &say, &mut facts, &mut unreadable);
            }
            Err(error) => {
                begin(path);
                say(path, Step::Ended(How::Unreadable));
                unreadable.push(Unreadable {
                    path: path.clone(),
                    why: describe(&error),
                });
            }
        }
    }

    // The archives aside that the check may find a need met by: backups,
    // switched off ones and names the game never lists, but not Blizzard's
    // own `base-` and `backup-`, which nobody made as a backup.
    let aside: Vec<&str> = order
        .unloaded
        .iter()
        .filter(|unloaded| !matches!(unloaded.reason, Reason::Unreadable(_)))
        .map(|unloaded| unloaded.path.as_str())
        .filter(|path| !never_loaded(path, locale))
        .collect();

    // Second round: the names the first gave, to name what has no list.
    let mut kept_aside: Vec<(&str, Facts)> = Vec::new();
    if !deferred.is_empty() || !aside.is_empty() {
        let pool: BTreeSet<Key> = facts
            .values()
            .flat_map(|facts| facts.names.keys().copied())
            .collect();
        let tag = format!("{kept_as}:{}", digest(&pool));
        let mut known: Option<Vec<String>> = None;

        for path in &deferred {
            begin(path);
            let mut read = false;
            let done = cache.of_archive(root, path, &tag, || {
                read = true;
                let known = known.get_or_insert_with(|| names_of(&facts));
                Opened::open(&root.join(path)).map(|opened| compute(path, &opened, known))
            });
            finish(path, done, read, &say, &mut facts, &mut unreadable);
        }

        // The ones aside, through the same cache, saying nothing of how it
        // goes: they are no part of the count the player sees.
        for path in aside {
            let kept = cache
                .archive(root, path, kept_as)
                .or_else(|| cache.archive(root, path, &tag));
            if let Some(own) = kept {
                kept_aside.push((path, own));
                continue;
            }

            let opened = match Opened::open(&root.join(path)) {
                Ok(opened) => opened,
                Err(error) => {
                    log::debug!("Can't read {path}, aside: {}", describe(&error));
                    continue;
                }
            };
            // One without a list of its files is named by the others, and
            // kept under their names.
            let (under, names) = if has_listfile(&opened) {
                (kept_as, &[][..])
            } else {
                (
                    tag.as_str(),
                    known.get_or_insert_with(|| names_of(&facts)).as_slice(),
                )
            };

            match cache.of_archive(root, path, under, || {
                Ok(strip(path, Facts::of(&opened, names), locale))
            }) {
                Ok(own) => kept_aside.push((path, own)),
                Err(error) => log::debug!("Can't keep {path}, aside: {}", describe(&error)),
            }
        }
    }

    let layers: Vec<Layer> = paths
        .iter()
        .filter_map(|(priority, path)| {
            Some(Layer::new(
                *priority,
                path.clone(),
                facts.remove(path)?,
                locale,
            ))
        })
        .collect();
    let graph = Graph::build(&layers, locale);
    drop(layers);

    let present: Vec<(&str, &Facts)> = kept_aside
        .iter()
        .map(|(path, facts)| (*path, facts))
        .collect();
    let unmet = met_aside(&graph, &order, &present);
    drop(present);
    drop(kept_aside);

    unreadable.extend(
        order
            .unloaded
            .iter()
            .filter_map(|unloaded| match &unloaded.reason {
                Reason::Unreadable(why) => Some(Unreadable {
                    path: unloaded.path.clone(),
                    why: why.clone(),
                }),
                _ => None,
            }),
    );
    unreadable.sort_by(|a, b| a.path.cmp(&b.path));

    // What is kept of archives no longer there goes.
    let present: Vec<String> = paths
        .iter()
        .map(|(_, path)| path.clone())
        .chain(order.unloaded.iter().map(|unloaded| unloaded.path.clone()))
        .collect();
    cache.sweep(root, &present);

    Ok(Scan {
        order,
        graph,
        unreadable,
        unmet,
        listing,
    })
}

/// The names the archives in `facts` give to their files, each once.
fn names_of(facts: &BTreeMap<String, Facts>) -> Vec<String> {
    let mut names: Vec<&String> = facts
        .values()
        .flat_map(|facts| facts.names.values())
        .collect();
    names.sort_unstable();
    names.dedup();

    names.into_iter().cloned().collect()
}

/// Whether `path` is one of the archives Blizzard ships in the language's
/// folder that the game never loads, `base-` and `backup-`: not a backup
/// anyone made.
fn never_loaded(path: &str, locale: Locale) -> bool {
    let lower = path.to_ascii_lowercase();
    let code = locale.code().to_ascii_lowercase();

    ["base", "backup"]
        .iter()
        .any(|name| lower == format!("data/{code}/{name}-{code}.mpq"))
}

/// The needs of the archives loaded that nothing loaded meets and one of the
/// archives aside (`present`, with their facts) would, as `order` says why
/// each is aside.
fn met_aside(graph: &Graph, order: &Order, present: &[(&str, &Facts)]) -> Vec<Unmet> {
    graph
        .unmet_but_present(present)
        .into_iter()
        .filter_map(|found| {
            let instead = found.present_in.into_iter().next()?;
            let reason = order
                .unloaded
                .iter()
                .find(|unloaded| unloaded.path == instead)?
                .reason
                .clone();

            Some(Unmet {
                archive: found.need.archive,
                kind: found.need.file.kind,
                instead,
                reason,
            })
        })
        .collect()
}

/// Files an archive's facts, or notes that it can't be read, and says the
/// archive is done.
fn finish(
    path: &str,
    done: Result<Facts, Error>,
    read: bool,
    say: &dyn Fn(&str, Step),
    facts: &mut BTreeMap<String, Facts>,
    unreadable: &mut Vec<Unreadable>,
) {
    match done {
        Ok(own) => {
            let _ = facts.insert(path.to_owned(), own);
            say(path, Step::Ended(if read { How::Read } else { How::Kept }));
        }
        Err(error) => {
            log::warn!("Can't read {path}: {}", describe(&error));
            unreadable.push(Unreadable {
                path: path.to_owned(),
                why: describe(&error),
            });
            say(path, Step::Ended(How::Unreadable));
        }
    }
}

/// Whether `archive` lists its files.
fn has_listfile(archive: &Opened) -> bool {
    let listfile = key("(listfile)");

    archive
        .entries()
        .iter()
        .any(|entry| entry.key == listfile && !entry.deleted)
}

/// A digest of the names `pool` holds the hashes of.
fn digest(pool: &BTreeSet<Key>) -> String {
    let mut hash = Sha256::new();

    for Key(a, b) in pool {
        hash.update(a.to_le_bytes());
        hash.update(b.to_le_bytes());
    }

    hash.finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `full`, inside the client folder `root`, as the archives are named: from
/// it, with `/` between folders.
fn relative(root: &Path, full: &Path) -> Option<String> {
    let inside = full.strip_prefix(root).ok()?;
    let parts: Vec<String> = inside
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();

    Some(parts.join("/"))
}

/// A source that tells how many files were read from it, every [`FILES`].
struct Counting<'a, S, T> {
    inner: &'a S,
    read: Cell<usize>,
    tell: T,
}

impl<'a, S, T: Fn(usize)> Counting<'a, S, T> {
    fn new(inner: &'a S, tell: T) -> Self {
        Self {
            inner,
            read: Cell::new(0),
            tell,
        }
    }

    /// Counts a read.
    fn count(&self) {
        let read = self.read.get() + 1;

        self.read.set(read);
        if read.is_multiple_of(FILES) {
            (self.tell)(read);
        }
    }
}

impl<S: Source, T: Fn(usize)> Source for Counting<'_, S, T> {
    fn entries(&self) -> Vec<Entry> {
        self.inner.entries()
    }

    fn read(&self, name: &str) -> std::io::Result<Vec<u8>> {
        self.count();
        self.inner.read(name)
    }

    fn read_range(&self, name: &str, offset: u32, len: u32) -> std::io::Result<Vec<u8>> {
        self.count();
        self.inner.read_range(name, offset, len)
    }
}

/// The files of the `Data` folder of the client folder at `root`, and of the
/// folder of `locale` in it, that can be archives. Folders are found
/// whatever their case. A client with no `Data` yet has an empty listing;
/// one whose `Data` can't be listed is an error.
pub fn listing(root: &Path, locale: Locale) -> Result<Listing, Error> {
    let mut found = Listing::new();
    let Some((data, spelled)) = folder(root, "Data")? else {
        return Ok(found);
    };

    let entries = fs::read_dir(&data).map_err(|source| Error::Io {
        path: data.clone(),
        source,
    })?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();

        if entry.path().is_dir() && name.eq_ignore_ascii_case(locale.code()) {
            if let Ok(inner) = fs::read_dir(entry.path()) {
                add(&mut found, &format!("{spelled}/{name}"), inner.flatten());
            }
        } else if entry.path().is_file() {
            add(&mut found, &spelled, std::iter::once(entry));
        }
    }
    found.sort();

    Ok(found)
}

/// The folder of `parent` called `name` in any case, and how it is spelled.
fn folder(parent: &Path, name: &str) -> Result<Option<(PathBuf, String)>, Error> {
    let entries = fs::read_dir(parent).map_err(|source| Error::Io {
        path: parent.to_path_buf(),
        source,
    })?;

    Ok(entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|own| own.eq_ignore_ascii_case(name))
        .min()
        .map(|own| (parent.join(&own), own)))
}

/// Adds the files of `entries`, which are in the folder `inside`, that can
/// be archives to `found`.
fn add(found: &mut Listing, inside: &str, entries: impl Iterator<Item = fs::DirEntry>) {
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.to_ascii_lowercase().contains("mpq") {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .and_then(|since| u64::try_from(since.as_nanos()).ok())
            .unwrap_or(0);

        found.push((format!("{inside}/{name}"), metadata.len(), modified));
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs::File;
    use std::time::SystemTime;

    use iced_test::simulator;

    use super::*;
    use crate::core::mpq::writer::{Options, Writer};

    /// An archive holding `files`, each one byte, with a list of them.
    fn archive(files: &[&str]) -> Writer {
        files
            .iter()
            .fold(Writer::new(), |writer, name| writer.file(name, b"x"))
            .with_listfile(Options::default())
    }

    /// A client folder holding `archives` (path, archive).
    fn client(archives: &[(&str, &Writer)]) -> (tempfile::TempDir, PathBuf) {
        let (root, installation) = crate::testing::client(&[]);
        let path = installation.root().to_path_buf();

        for (name, archive) in archives {
            let target = path.join(name);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            archive.write(&target);
        }

        (root, path)
    }

    /// A model of version 264 that needs `skins` skins.
    fn model(skins: u32) -> Vec<u8> {
        let mut bytes = vec![0; 0x58];

        bytes[..4].copy_from_slice(b"MD20");
        bytes[4..8].copy_from_slice(&264u32.to_le_bytes());
        bytes[0x44..0x48].copy_from_slice(&skins.to_le_bytes());
        bytes[0x20..0x24].copy_from_slice(&0x58u32.to_le_bytes());
        bytes[0x54..0x58].copy_from_slice(&0x58u32.to_le_bytes());

        bytes
    }

    /// An archive of the model `Creature\Foo\Foo.m2`, which needs a skin.
    fn wants_a_skin() -> Writer {
        Writer::new()
            .file("Creature\\Foo\\Foo.m2", &model(1))
            .with_listfile(Options::default())
    }

    /// An archive of the skin that model needs.
    fn gives_the_skin() -> Writer {
        Writer::new()
            .file("Creature\\Foo\\Foo00.skin", b"skin")
            .with_listfile(Options::default())
    }

    /// Looks at `root`, saying what the look reported.
    fn looked(root: &Path, cache: &Cache) -> (Result<Scan, String>, Vec<Progress>) {
        let said = RefCell::new(Vec::new());
        let scan = look(root, Locale::EnUs, cache, &|progress| {
            said.borrow_mut().push(progress);
        });

        (scan, said.into_inner())
    }

    /// How each archive was done with, by path.
    fn ended(said: &[Progress]) -> BTreeMap<&str, How> {
        said.iter()
            .filter_map(|progress| match progress.step {
                Step::Ended(how) => Some((progress.archive.as_str(), how)),
                _ => None,
            })
            .collect()
    }

    /// Sets the modification time of the file at `path`.
    fn touch(path: &Path, to: SystemTime) {
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(to)
            .unwrap();
    }

    #[test]
    fn a_second_look_reads_only_what_changed() {
        let (_root, root) = client(&[
            ("Data/patch-a.mpq", &archive(&["a.txt"])),
            ("Data/patch-b.mpq", &archive(&["b.txt"])),
        ]);
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::at(folder.path());

        let (scan, said) = looked(&root, &cache);
        assert!(scan.is_ok());
        assert_eq!(
            ended(&said),
            BTreeMap::from([
                ("Data/patch-a.mpq", How::Read),
                ("Data/patch-b.mpq", How::Read)
            ])
        );

        let (scan, said) = looked(&root, &cache);
        let scan = scan.unwrap();
        assert_eq!(
            ended(&said),
            BTreeMap::from([
                ("Data/patch-a.mpq", How::Kept),
                ("Data/patch-b.mpq", How::Kept)
            ])
        );
        assert_eq!(scan.graph.provider(key("a.txt")), Some("Data/patch-a.mpq"));

        // Garbage of the same size and time: its stamp is the same, so
        // nothing reads it.
        let target = root.join("Data/patch-a.mpq");
        let (size, modified) = {
            let metadata = fs::metadata(&target).unwrap();
            (metadata.len(), metadata.modified().unwrap())
        };
        fs::write(&target, vec![7; size as usize]).unwrap();
        touch(&target, modified);
        let (scan, said) = looked(&root, &cache);
        let scan = scan.unwrap();
        assert_eq!(ended(&said)["Data/patch-a.mpq"], How::Kept);
        assert!(scan.unreadable.is_empty());
        assert_eq!(scan.graph.provider(key("a.txt")), Some("Data/patch-a.mpq"));

        // Touched, it is read again, and it can't be.
        touch(&target, modified + Duration::from_secs(5));
        let (scan, said) = looked(&root, &cache);
        let scan = scan.unwrap();
        assert_eq!(ended(&said).get("Data/patch-b.mpq"), Some(&How::Kept));
        assert_eq!(
            scan.unreadable
                .iter()
                .map(|archive| archive.path.as_str())
                .collect::<Vec<_>>(),
            ["Data/patch-a.mpq"]
        );
        assert_eq!(scan.graph.provider(key("a.txt")), None);
        assert_eq!(scan.graph.provider(key("b.txt")), Some("Data/patch-b.mpq"));
    }

    #[test]
    fn an_archive_that_cant_be_read_is_skipped_and_the_rest_is_known() {
        let (_root, root) = client(&[("Data/patch-a.mpq", &archive(&["a.txt"]))]);
        fs::write(root.join("Data/patch-b.mpq"), b"not an archive at all").unwrap();
        let folder = tempfile::tempdir().unwrap();

        let (scan, said) = looked(&root, &Cache::at(folder.path()));
        let scan = scan.unwrap();

        assert_eq!(scan.graph.provider(key("a.txt")), Some("Data/patch-a.mpq"));
        assert_eq!(
            scan.unreadable
                .iter()
                .map(|archive| archive.path.as_str())
                .collect::<Vec<_>>(),
            ["Data/patch-b.mpq"]
        );
        assert!(!scan.unreadable[0].why.is_empty(), "it says why");
        assert_eq!(scan.order.loaded.len(), 1);
        assert_eq!(
            ended(&said),
            BTreeMap::from([("Data/patch-a.mpq", How::Read)])
        );
    }

    #[test]
    fn an_archive_without_a_list_of_its_files_is_named_by_the_others() {
        let name = "Creature\\Bear\\Bear.m2";
        // One that lists its files, naming the model, and one that doesn't.
        let listed = archive(&[name]);
        let unlisted = Writer::new().file(name, b"MD21 not a model");
        let (_root, root) = client(&[
            ("Data/patch-a.mpq", &listed),
            ("Data/patch-b.mpq", &unlisted),
        ]);
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::at(folder.path());

        let (scan, said) = looked(&root, &cache);
        let scan = scan.unwrap();
        assert!(scan.unreadable.is_empty());
        assert_eq!(scan.graph.provider(key(name)), Some("Data/patch-b.mpq"));
        assert_eq!(scan.graph.name(key(name)), Some(name));
        assert_eq!(
            ended(&said),
            BTreeMap::from([
                ("Data/patch-a.mpq", How::Read),
                ("Data/patch-b.mpq", How::Read)
            ])
        );
        // The archive with a list went first, then the one it names.
        let started: Vec<&str> = said
            .iter()
            .filter(|progress| progress.step == Step::Started)
            .map(|progress| progress.archive.as_str())
            .collect();
        assert_eq!(started, ["Data/patch-a.mpq", "Data/patch-b.mpq"]);

        let (_, said) = looked(&root, &cache);
        assert!(ended(&said).values().all(|how| *how == How::Kept));

        // The names changed, so what they name in the other changes.
        archive(&[name, "Creature\\Bear\\BearFur.blp"]).write(&root.join("Data/patch-a.mpq"));
        let (_, said) = looked(&root, &cache);
        assert_eq!(ended(&said)["Data/patch-a.mpq"], How::Read);
        assert_eq!(ended(&said)["Data/patch-b.mpq"], How::Read);
    }

    #[test]
    fn a_long_archive_still_reports_progress_every_two_hundred_files() {
        let models: Vec<String> = (0..450)
            .map(|number| format!("World\\Model{number}.m2"))
            .collect();
        let names: Vec<&str> = models.iter().map(String::as_str).collect();
        let (_root, root) = client(&[("Data/patch-a.mpq", &archive(&names))]);
        let folder = tempfile::tempdir().unwrap();

        let (scan, said) = looked(&root, &Cache::at(folder.path()));
        assert!(scan.is_ok());

        let steps: Vec<Step> = said.iter().map(|progress| progress.step).collect();
        assert_eq!(
            steps,
            [
                Step::Started,
                Step::Files(200),
                Step::Files(400),
                Step::Ended(How::Read)
            ]
        );
        assert!(said.iter().all(|progress| progress.total == 1));
        assert!(
            said.windows(2)
                .all(|pair| pair[0].elapsed <= pair[1].elapsed),
            "each says the time since the start"
        );
    }

    #[test]
    fn a_scan_that_cant_list_the_data_folder_fails_as_a_whole() {
        let (_root, root) = client(&[]);
        fs::write(root.join("Data"), b"a file, not a folder").unwrap();
        let folder = tempfile::tempdir().unwrap();

        let (scan, said) = looked(&root, &Cache::at(folder.path()));

        assert!(scan.unwrap_err().contains("Data"));
        assert!(said.is_empty());
    }

    #[test]
    fn the_listing_changes_with_an_archive_and_not_with_other_files() {
        let (_root, root) = client(&[("Data/enUS/patch-enUS.mpq", &archive(&["a.txt"]))]);
        let before = listing(&root, Locale::EnUs).unwrap();

        assert_eq!(before.len(), 1);
        assert_eq!(before[0].0, "Data/enUS/patch-enUS.mpq");

        fs::write(root.join("Data/notes.txt"), b"hello").unwrap();
        fs::write(root.join("Data/enUS/realmlist.wtf"), b"set realmlist x").unwrap();
        assert_eq!(listing(&root, Locale::EnUs).unwrap(), before);

        archive(&["a.txt", "b.txt"]).write(&root.join("Data/enUS/patch-enUS.mpq"));
        assert_ne!(listing(&root, Locale::EnUs).unwrap(), before);

        // Another language's folder isn't this client's.
        fs::create_dir_all(root.join("Data/esES")).unwrap();
        archive(&[]).write(&root.join("Data/esES/patch-esES.mpq"));
        assert_eq!(listing(&root, Locale::EnUs).unwrap().len(), 1);
    }

    #[test]
    fn the_banner_says_where_the_look_is_once_it_went_on_for_long_enough() {
        let (_root, root) = client(&[]);
        let folder = tempfile::tempdir().unwrap();
        let mut state = State::new(Cache::at(folder.path()));
        let mut errors = Errors::default();
        let _ = state.look(&root, Locale::EnUs);
        let said = |state: &State| state.banner(&root).map(|_| true);
        let progress = |seconds: f32, index| Progress {
            elapsed: Duration::from_secs_f32(seconds),
            archive: "Data/patch-H.mpq".to_owned(),
            index,
            total: 35,
            step: Step::Started,
        };

        assert!(state.holds_back(&root));
        assert_eq!(said(&state), None, "nothing at first");
        let _ = state.update(
            Message::Progressed(root.clone(), progress(2.0, 11)),
            &mut errors,
        );
        assert_eq!(said(&state), None, "two seconds exactly is not more");
        let _ = state.update(
            Message::Progressed(root.clone(), progress(2.1, 12)),
            &mut errors,
        );
        assert_eq!(said(&state), Some(true));
        let banner = state.banner(&root).unwrap();
        assert!(
            simulator(banner)
                .find("Looking at this client's patches: Data/patch-H.mpq, 12 of 35…")
                .is_ok()
        );

        // It stays until the look ends.
        let _ = state.update(
            Message::Progressed(root.clone(), progress(0.1, 13)),
            &mut errors,
        );
        assert_eq!(said(&state), Some(true));

        let (scan, _) = looked(&root, &Cache::at(folder.path()));
        let _ = state.update(
            Message::Looked(root.clone(), scan.map(Arc::new)),
            &mut errors,
        );
        assert!(!state.holds_back(&root));
        assert_eq!(said(&state), None);
        assert!(state.graph(&root).is_some());
        assert!(errors.messages().is_empty());
    }

    #[test]
    fn a_need_that_an_archive_aside_would_meet_is_found_whatever_the_archive_is_called() {
        let (_root, root) = client(&[
            ("Data/patch-G.mpq", &wants_a_skin()),
            ("Data/patch-F.mpq.backup", &gives_the_skin()),
        ]);
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::at(folder.path());

        let (scan, said) = looked(&root, &cache);
        let scan = scan.unwrap();
        assert_eq!(
            scan.unmet,
            [Unmet {
                archive: "Data/patch-G.mpq".to_owned(),
                kind: Kind::Models,
                instead: "Data/patch-F.mpq.backup".to_owned(),
                reason: Reason::Backup,
            }]
        );
        assert_eq!(
            said.iter()
                .map(|progress| progress.archive.as_str())
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["Data/patch-G.mpq"]),
            "the archive aside is no part of the count"
        );

        // Changed, it is read again, and says nothing.
        fs::write(
            root.join("Data/patch-F.mpq.backup"),
            b"not an archive any more, but its stamp is the same size",
        )
        .unwrap();
        let (again, _) = looked(&root, &cache);
        assert!(
            again.unwrap().unmet.is_empty(),
            "read again, it says nothing"
        );

        // Switched off, or with a name the game never looks at.
        fs::remove_file(root.join("Data/patch-F.mpq.backup")).unwrap();
        gives_the_skin().write(&root.join("Data/patch-7.mpq.disabled"));
        let (scan, _) = looked(&root, &cache);
        assert_eq!(scan.unwrap().unmet[0].reason, Reason::Disabled);

        fs::remove_file(root.join("Data/patch-7.mpq.disabled")).unwrap();
        gives_the_skin().write(&root.join("Data/patch-F.Ompq"));
        let (scan, _) = looked(&root, &cache);
        let scan = scan.unwrap();
        assert_eq!(scan.unmet[0].reason, Reason::NotListed);
        assert_eq!(scan.unmet[0].instead, "Data/patch-F.Ompq");
    }

    #[test]
    fn blizzards_unloaded_base_and_backup_archives_are_not_backups() {
        let (_root, root) = client(&[
            ("Data/patch-G.mpq", &wants_a_skin()),
            ("Data/enUS/base-enUS.MPQ", &gives_the_skin()),
            ("Data/enUS/backup-enUS.MPQ", &gives_the_skin()),
        ]);
        let folder = tempfile::tempdir().unwrap();

        let (scan, _) = looked(&root, &Cache::at(folder.path()));
        let scan = scan.unwrap();

        assert!(scan.unmet.is_empty(), "{:?}", scan.unmet);
        assert!(
            scan.graph.unmet().len() == 1,
            "the need is unmet all the same"
        );
    }

    #[test]
    fn the_findings_say_who_needs_what_and_name_a_mods_archive() {
        let unmet = |archive: &str, kind, instead: &str, reason| Unmet {
            archive: archive.to_owned(),
            kind,
            instead: instead.to_owned(),
            reason,
        };
        let findings = Findings {
            twins: vec![vec![
                "Data/patch-W.mpq".to_owned(),
                "Data/patch-w.mpq".to_owned(),
            ]],
            unmet: vec![
                unmet(
                    "Data/patch-G.mpq",
                    Kind::Tables,
                    "Data/patch-F.mpq.backup-1",
                    Reason::Backup,
                ),
                unmet(
                    "Data/patch-G.mpq",
                    Kind::Models,
                    "Data/patch-F.mpq.backup-1",
                    Reason::Backup,
                ),
                unmet(
                    "Data/patch-G.mpq",
                    Kind::Models,
                    "Data/patch-7.mpq.disabled",
                    Reason::Disabled,
                ),
                unmet(
                    "Data/enUS/patch-enUS-M.MPQ",
                    Kind::Tables,
                    "Data/patch-W.Ompq",
                    Reason::NotListed,
                ),
            ],
            unreadable: vec![Unreadable {
                path: "Data/patch-X.mpq".to_owned(),
                why: "bad".to_owned(),
            }],
            mods: vec!["“Patch Y”'s Data/patch-y.mpq is not as released.".to_owned()],
        };
        let owner =
            |path: &str| (path == "data/enus/patch-enus-m.mpq").then(|| "Dungeon Maps".to_owned());

        assert_eq!(
            findings.lines(&owner),
            [
                "Data/patch-W.mpq and Data/patch-w.mpq differ only in case. Windows would allow \
                 only one of them, and the game may load either: rename one, or remove it.",
                "The tables of “Dungeon Maps” need Data/patch-W.Ompq, which isn't loaded: the \
                 game never looks at a name like that.",
                "Your custom Data/patch-G.mpq's models need Data/patch-7.mpq, which isn't loaded: \
                 it is switched off as Data/patch-7.mpq.disabled.",
                "Your custom Data/patch-G.mpq's tables and models need Data/patch-F.mpq, which \
                 isn't loaded: it is backed up as Data/patch-F.mpq.backup-1.",
                "Data/patch-X.mpq couldn't be read, so what it changes is unknown.",
                "“Patch Y”'s Data/patch-y.mpq is not as released.",
            ]
        );
        assert!(Findings::default().lines(&owner).is_empty());
    }

    /// The memory this process uses now and at most, where Linux says.
    fn memory() -> String {
        fs::read_to_string("/proc/self/status")
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with("VmRSS") || line.starts_with("VmHWM"))
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `OLD_LAUNCHER_TEST_CLIENT=<client folder> cargo test --release --lib
    /// patches::tests::a_real_clients_check -- --ignored --nocapture`, the
    /// folder only ever read: looks at its patches with a cache in a
    /// temporary folder, and prints what the integrity check says of them.
    #[test]
    #[ignore = "reads every archive of the client in $OLD_LAUNCHER_TEST_CLIENT"]
    fn a_real_clients_check_says_what_is_amiss_with_its_patches() {
        let root = PathBuf::from(
            std::env::var("OLD_LAUNCHER_TEST_CLIENT")
                .expect("OLD_LAUNCHER_TEST_CLIENT is a client folder"),
        );
        let locale = crate::core::Installation::open(&root)
            .expect("it is a client")
            .locale()
            .expect("it has a language");
        let folder = tempfile::tempdir().unwrap();
        let cache = Cache::at(folder.path());
        let mut state = State::new(cache.clone());
        let _ = state.look(&root, locale);
        let scan = look(&root, locale, &cache, &|_| {}).map(Arc::new);
        let _ = state.update(Message::Looked(root.clone(), scan), &mut Errors::default());

        let lines = state
            .findings(&root)
            .expect("it is looked at")
            .lines(&|_| None);
        println!("{} things to look at:", lines.len());
        for line in lines {
            println!("  {line}");
        }
    }

    /// `OLD_LAUNCHER_TEST_CLIENT=<client folder> cargo test --release --lib
    /// patches::tests::a_real_clients_patches -- --ignored --nocapture`, the folder
    /// only ever read: looks at its patches twice with a cache in a
    /// temporary folder, and prints the times and the memory. Under
    /// `/usr/bin/time -v` the peak is the first look's.
    #[test]
    #[ignore = "reads every archive of the client in $OLD_LAUNCHER_TEST_CLIENT"]
    fn a_real_clients_patches_are_looked_at_twice() {
        let root = PathBuf::from(
            std::env::var("OLD_LAUNCHER_TEST_CLIENT")
                .expect("OLD_LAUNCHER_TEST_CLIENT is a client folder"),
        );
        let locale = crate::core::Installation::open(&root)
            .expect("it is a client")
            .locale()
            .expect("it has a language");
        let folder = tempfile::tempdir().unwrap();
        // Another run's cache, to measure a start on its own.
        let kept = std::env::var("OLD_LAUNCHER_TEST_CACHE")
            .map_or_else(|_| folder.path().to_path_buf(), PathBuf::from);
        let cache = Cache::at(&kept);

        for time in ["first", "second"] {
            let said = RefCell::new(Vec::new());
            let started = Instant::now();
            let scan = look(&root, locale, &cache, &|progress| {
                said.borrow_mut().push(progress);
            })
            .expect("it is looked at");
            let took = started.elapsed();
            let said = said.into_inner();

            println!(
                "{time} look: {took:?}, {} archives, {} unreadable, first past 2 s at {:?}; {}",
                scan.order
                    .loaded
                    .iter()
                    .map(|slot| slot.paths.len())
                    .sum::<usize>(),
                scan.unreadable.len(),
                said.iter()
                    .find(|progress| progress.elapsed > SLOW)
                    .map(|progress| &progress.archive),
                memory()
            );
            for (archive, how) in ended(&said) {
                println!("  {archive}: {how:?}");
            }
        }

        let size: u64 = fs::read_dir(&kept)
            .unwrap()
            .flatten()
            .flat_map(|client| fs::read_dir(client.path()).unwrap().flatten())
            .map(|file| file.metadata().map_or(0, |metadata| metadata.len()))
            .sum();
        println!("kept: {size} bytes");
    }
}
