//! Mods in a client, as the list of mods describes them: looked for whenever
//! the client is read, with a banner above the addons while the launcher
//! needs to check or repair one before starting the game, and a section of
//! the clients dialog that installs, updates, repairs and removes them.
//!
//! The last download of each mod is kept in the launcher's cache (see
//! [`cache`]): installing it again, in another client or to repair one,
//! downloads nothing.
//!
//! Installing a release with archives in `Data` goes through a gate (see
//! [`gate`]): what it would do to the patches the client holds is told
//! before anything is touched. The launcher knows what a release's archives
//! provide and need only once it has read them, so when it has not yet,
//! installing is two tasks: [`fetch`] downloads the release, looks at its
//! archives and simulates the install on the client's graph; then, if it
//! breaks nothing and the player agrees to what it overrides, [`place`]
//! puts the files in place and runs the setup. When it has (an earlier
//! install, in this client or another), the verdict shows in the mod's row
//! before any press, and the question comes before the download. A release
//! without archives goes straight to [`place`], as it always did.

mod gate;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use iced::futures::StreamExt;
use iced::task::{Straw, sipper};
use iced::widget::{
    button, checkbox, column, container, pick_list, progress_bar, row, space, text,
};
use iced::{Center, Element, Fill, Right, Task, Widget};
use sipper::Sender;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::action::Action;
use crate::activity::activity;
use crate::core::mods::{self, Mod, Mods, Problem, Release, Status};
use crate::core::patches::Cache;
use crate::core::{Config, Installation, Launch, Locale, launch};
use crate::errors::Errors;
use crate::patches::Scan;
use crate::{blocking, describe, github, size, style, update};
use gate::{Install, Verdict};

/// What the hint of a button says while the game runs.
const CLOSE_THE_GAME: &str = "Close the game first.";

/// How a problem is colored.
type Tone = fn(&iced::Theme) -> text::Style;

/// What is known of the mods in each client, and what is being done.
#[derive(Default)]
pub struct State {
    /// The list of mods, as last fetched.
    list: Mods,
    /// Why the list couldn't be fetched, while there is none.
    unfetched: Option<String>,
    /// What each client folder looked at holds of the mods, by mod id: the
    /// ones it holds.
    found: BTreeMap<PathBuf, BTreeMap<String, Status>>,
    /// In each client folder looked at, the files of something else where a
    /// mod's newest release puts its own, by mod id (see [`Seen::foreign`]).
    foreign: BTreeMap<PathBuf, BTreeMap<String, Vec<String>>>,
    /// In each client folder looked at, the mods that don't suit it (see
    /// [`Seen::unfit`]).
    unfit: BTreeMap<PathBuf, BTreeSet<String>>,
    /// In each client folder looked at, the files in conflict with mods
    /// that it has (see [`Seen::excluded`]).
    excluded: BTreeMap<PathBuf, BTreeMap<String, Vec<String>>>,
    /// In each client folder looked at, its languages mods don't come in
    /// (see [`Seen::lacking`]).
    lacking: BTreeMap<PathBuf, BTreeMap<String, Vec<Locale>>>,
    /// In each client folder, the language of a mod chosen for those of its
    /// languages the mod doesn't come in, by mod id: [`mods::fallback`]
    /// until one is.
    instead: BTreeMap<PathBuf, BTreeMap<String, Locale>>,
    /// The client folders being looked at, and whether to look again once
    /// done: asked meanwhile, the folder may have changed since.
    finding: BTreeMap<PathBuf, bool>,
    /// The client folders whose patches are being looked at: what a mod
    /// does to them can't be told until that is done.
    patches: BTreeSet<PathBuf>,
    /// The work under way.
    busy: Option<Work>,
    /// What each client folder's patches, as last looked at, say of each mod
    /// with archives, by mod id (see [`gate::judge`]). Only for the client
    /// shown.
    gates: BTreeMap<PathBuf, BTreeMap<String, Verdict>>,
    /// What the last look at the patches of each client folder found, and
    /// the language it plays in.
    scans: BTreeMap<PathBuf, (Arc<Scan>, Locale)>,
    /// How many times each client folder's mods were judged: the newest
    /// answer is the one that counts.
    judged: BTreeMap<PathBuf, u64>,
    /// Where the facts of the releases' archives are kept: none in tests.
    facts: Option<Cache>,
    /// The install that went to look at the release's archives, and waits
    /// for them or for a yes.
    pending: Option<Pending>,
    /// The install that was stopped before it touched anything, and why.
    stopped: Option<Work>,
    /// The client folder and the mod whose removal from it waits for a yes,
    /// and whether the player's files go too.
    removing: Option<(PathBuf, String, bool)>,
    /// The client folder and the mod whose install in it, over the files
    /// in conflict with it, waits for a yes.
    confirming: Option<(PathBuf, String)>,
    /// The client folders whose mods are repaired once looked at, one after
    /// the other (see [`State::complete`]), and those repaired so far.
    completing: BTreeMap<PathBuf, BTreeSet<String>>,
    /// What the last work did.
    done: Option<Work>,
    /// Downloads from GitHub signed in, when there is a login.
    github: Option<github::Signed>,
    /// Where the mods' downloads are kept, so installing a release again
    /// needs no download: none in tests.
    cache: Option<PathBuf>,
}

/// Work on a mod in a client folder, and what it is or did, for people.
#[derive(Debug, Clone)]
struct Work {
    root: PathBuf,
    id: String,
    said: String,
    /// How far an install got, while it goes on.
    stage: Option<Stage>,
    /// How many steps an install has: the download, looking at its
    /// archives (if `looks`), putting the files in place, and each step of
    /// the setup.
    steps: usize,
    /// Whether looking at the release's archives is a step of its own.
    looks: bool,
}

/// An install between its two tasks (see [`fetch`] and [`place`]): what
/// [`place`] needs, and what [`fetch`] found.
#[derive(Debug, Clone)]
struct Pending {
    root: PathBuf,
    listed: Mod,
    release: Release,
    /// The language chosen for the client's that the mod doesn't come in.
    instead: Option<Locale>,
    /// What is said while it goes on, and once it is done.
    busy: String,
    done: String,
    /// Whether the player already said yes to what it overrides.
    confirmed: bool,
    /// What the first task found, once it is done.
    fetched: Option<Fetched>,
}

/// What [`fetch`] found of the release it downloaded.
#[derive(Clone)]
pub struct Fetched {
    /// What installing it does to the patches, unless that can't be told.
    install: Option<Install>,
    /// The downloads that are not kept in the launcher's cache, by address:
    /// [`place`] needs them.
    downloads: Arc<BTreeMap<String, Vec<u8>>>,
}

impl std::fmt::Debug for Fetched {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Fetched")
            .field("install", &self.install)
            .field("downloads", &self.downloads.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// How far installing a mod got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// Downloading its release: the bytes received, and how many there are
    /// when the server says.
    Downloading { received: u64, total: Option<u64> },
    /// Looking at what its archives provide and need.
    Looking,
    /// Checking its files and putting them in place.
    Installing,
    /// Running `step` of the `steps` of its setup, which last said `said`.
    SettingUp {
        step: usize,
        steps: usize,
        said: Option<String>,
    },
}

impl Stage {
    /// How far this step got, from 0 to 1, when it can tell: only a
    /// download whose size the server said can.
    fn fraction(&self) -> Option<f32> {
        match self {
            Self::Downloading {
                received,
                total: Some(total),
            } if *total > 0 => Some(*received as f32 / *total as f32),
            _ => None,
        }
    }
}

impl Work {
    /// Whether it is on the mod `id` in the client folder at `root`.
    fn on(&self, root: &Path, id: &str) -> bool {
        self.root == root && self.id == id
    }
}

/// What a client folder holds of the mods in the list, by mod id.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    /// The mods it holds, as they are.
    pub held: BTreeMap<String, Status>,
    /// The files of something else where each mod's newest release puts its
    /// own (see [`mods::foreign`]): installing or updating it sets them
    /// aside, once the player says yes.
    pub foreign: BTreeMap<String, Vec<String>>,
    /// The mods that don't suit it, which it isn't offered (see
    /// [`mods::fits`]): editions made for an HD model pack it doesn't have,
    /// say.
    pub unfit: BTreeSet<String>,
    /// The files in conflict with each mod that it has (see
    /// [`mods::excluded`]): half an HD model pack. Installing it asks first.
    pub excluded: BTreeMap<String, Vec<String>>,
    /// Its languages each mod doesn't come in (see [`mods::lacking`]):
    /// installing it puts another of the mod's there, as the player
    /// chooses.
    pub lacking: BTreeMap<String, Vec<Locale>>,
}

/// Interactions with the banners and the section, and what their work comes
/// back with. Mods go by id.
#[derive(Debug, Clone)]
pub enum Message {
    /// What a client folder holds of the mods was looked at.
    Found(PathBuf, Seen),
    /// The File integrity check read every file of the mods in a client
    /// folder: what it found, to replace what the look knew.
    Verified(PathBuf, Seen),
    /// A mod's name, a link to its page, was pressed.
    OpenPage(String),
    /// Its page couldn't be opened: why.
    Unopened(String),
    /// Install, or update to the newest release, was pressed: files in
    /// conflict with it ask first.
    Install(String),
    /// Installing it over the files in conflict with it was confirmed.
    ConfirmInstall,
    /// Installing it over them was thought better of.
    CancelInstall,
    /// A language of the mod was picked for the client's languages it
    /// doesn't come in.
    Instead(String, Locale),
    /// Repair was pressed.
    Repair(String),
    /// Uninstall was pressed: ask first.
    Remove(String),
    /// The box deleting the player's files too was ticked or unticked.
    DeleteSettings(bool),
    /// Removing it was confirmed.
    ConfirmRemove,
    /// Removing it was thought better of.
    CancelRemove,
    /// The banner saying what was done was dismissed.
    Dismiss,
    /// Installing a mod in a client folder got further.
    Progressed(PathBuf, String, Stage),
    /// The release being installed was downloaded and its archives looked
    /// at: what it found, or why it failed.
    Fetched(PathBuf, String, Result<Fetched, String>),
    /// What a client folder's patches say of its mods was worked out, for
    /// the `u64`th time (see [`State::judge`]).
    Judged(PathBuf, u64, BTreeMap<String, Verdict>),
    /// The work on a mod in a client folder ended: what it did, or why it
    /// failed.
    Done(PathBuf, String, Result<String, String>),
}

/// What the mods ask of the dialog.
pub enum Instruction {
    /// The client folder at this path changed: read it again.
    Changed(PathBuf),
}

/// What the launcher needs to do to a mod in a client before starting the
/// game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Need {
    /// Repair it: install its release again, or the newest.
    Repair,
}

/// Looks at what the client folder at `root` holds of the mods in `list`,
/// away from the window (see [`look`]).
fn find(root: &Path, list: &Mods) -> Task<Message> {
    let root = root.to_path_buf();
    let list = list.clone();

    Task::perform(
        blocking({
            let root = root.clone();
            move || Ok(look(&root, &list))
        }),
        move |seen| Message::Found(root, seen.unwrap_or_default()),
    )
}

/// What the client folder at `root` holds of the mods in `list`: each one
/// there checked against its releases (see [`mods::check`]), so that one
/// found is known by its release, or as installed by hand; the files of
/// something else where each would put its own; and the mods that don't
/// suit it.
///
/// Those that were found as released and haven't changed in size or time
/// since are not read again.
pub fn look(root: &Path, list: &Mods) -> Seen {
    see(root, list, None)
}

/// [`look`], but reading every file of the mods there, whatever its size and
/// time: what the File integrity check does. `read` hears how many bytes
/// each file read was.
pub fn verify(root: &Path, list: &Mods, read: &mut dyn FnMut(u64)) -> Seen {
    see(root, list, Some(read))
}

/// How many bytes [`verify`] reads in the client folder at `root`, give or
/// take: nothing is read to tell.
pub fn weight(root: &Path, list: &Mods) -> u64 {
    list.mods
        .iter()
        .map(|listed| mods::weight(root, listed))
        .sum()
}

/// [`look`], reading every file if `verifying` hears how much of them.
fn see(root: &Path, list: &Mods, mut verifying: Option<&mut dyn FnMut(u64)>) -> Seen {
    let mut seen = Seen::default();

    for listed in &list.mods {
        let alternatives = list.alternatives(listed);
        let checked = match verifying.as_deref_mut() {
            Some(read) => mods::verify(root, listed, &alternatives, read),
            None => mods::check(root, listed, &alternatives),
        };
        let status = match checked {
            Ok(status) => status,
            Err(error) => {
                log::warn!(
                    "Could not check {} in {}: {error}",
                    listed.name,
                    root.display()
                );
                mods::status(root, listed, &alternatives)
            }
        };
        if let Some(status) = status {
            let _ = seen.held.insert(listed.id.clone(), status);
        }
        let foreign = listed
            .newest()
            .map(|newest| mods::foreign(root, listed, newest, &alternatives))
            .unwrap_or_default();
        if !foreign.is_empty() {
            let _ = seen.foreign.insert(listed.id.clone(), foreign);
        }
        if !mods::fits(root, listed) {
            let _ = seen.unfit.insert(listed.id.clone());
        }
        let excluded = mods::excluded(root, listed, &alternatives);
        if !excluded.is_empty() {
            let _ = seen.excluded.insert(listed.id.clone(), excluded);
        }
        let lacking = mods::lacking(root, listed);
        if !lacking.is_empty() {
            let _ = seen.lacking.insert(listed.id.clone(), lacking);
        }
    }

    seen
}

/// What a button's hint says while `work` goes on: nothing else is done
/// meanwhile.
fn wait(work: &Work) -> String {
    format!("{}… Wait until it's done.", work.said)
}

/// What the hint of a button says while the client is being looked at.
const LOOKING: &str = "Looking at this client… Wait until it's done.";

impl State {
    /// Downloads from GitHub sign in with `github` from now on.
    pub fn sign_in(&mut self, github: Option<github::Signed>) {
        self.github = github;
    }

    /// The mods' downloads are kept in `folder` from now on (see [`cache`]).
    pub fn keep_downloads_in(&mut self, folder: PathBuf) {
        self.cache = Some(folder);
    }

    /// What is read of the releases' archives is kept in `cache` from now
    /// on.
    pub fn keep_facts_in(&mut self, cache: Cache) {
        self.facts = Some(cache);
    }

    /// The patches of the client folder at `root` were looked at, and found
    /// `scan` (none while they are being looked at again), the client
    /// playing in `locale`: what installing a mod does to them is worked
    /// out again.
    pub fn patched(
        &mut self,
        root: &Path,
        scan: Option<Arc<Scan>>,
        locale: Locale,
    ) -> Task<Message> {
        let same = match (&scan, self.scans.get(root)) {
            (Some(new), Some((old, _))) => Arc::ptr_eq(new, old),
            (None, None) => true,
            _ => false,
        };
        if same {
            return Task::none();
        }

        let _ = self.gates.remove(root);
        match scan {
            Some(scan) => {
                let _ = self.scans.insert(root.to_path_buf(), (scan, locale));
                self.judge(root)
            }
            None => {
                let _ = self.scans.remove(root);
                Task::none()
            }
        }
    }

    /// Works out what the client folder at `root`'s patches say of its mods,
    /// away from the window, once the client is known and so are its
    /// patches.
    fn judge(&mut self, root: &Path) -> Task<Message> {
        let (Some((scan, locale)), Some(found), Some(facts)) =
            (self.scans.get(root), self.found.get(root), &self.facts)
        else {
            return Task::none();
        };
        if self.patches.contains(root) {
            return Task::none();
        }

        let (scan, locale, found, facts) = (scan.clone(), *locale, found.clone(), facts.clone());
        let list = self.list.clone();
        let ticket = self.judged.entry(root.to_path_buf()).or_default();
        *ticket += 1;
        let ticket = *ticket;
        let root = root.to_path_buf();

        Task::perform(
            blocking({
                let root = root.clone();
                move || Ok(gate::judge(&root, &list, &found, &scan, locale, &facts))
            }),
            move |verdicts| Message::Judged(root, ticket, verdicts.unwrap_or_default()),
        )
    }

    /// What the gate says of installing the newest release of the mod `id`
    /// in the client folder at `root`, if it has been worked out.
    fn install_of(&self, root: &Path, id: &str) -> Option<&Install> {
        self.gates.get(root)?.get(id)?.install.as_ref()
    }

    /// Whether installing the newest release of `listed` in the client
    /// folder at `root` has to look at its archives first, since the
    /// launcher doesn't know yet what they provide and need.
    fn unjudged(&self, root: &Path, listed: &Mod) -> bool {
        self.facts.is_some()
            && self.scans.contains_key(root)
            && listed.newest().is_some_and(gate::has_archives)
            && self.install_of(root, &listed.id).is_none()
    }

    /// The patches of the client folders at `roots` are being looked at,
    /// and no others: nothing is done to the mods there until that ends.
    pub fn waiting_for_patches(&mut self, roots: BTreeSet<PathBuf>) {
        self.patches = roots;
    }

    /// Whether the patches of the client folder at `root` are being looked
    /// at.
    pub fn is_waiting_for_patches(&self, root: &Path) -> bool {
        self.patches.contains(root)
    }

    /// Takes `list` as the list of mods, those for this system, and looks at
    /// the client folder at `current` again with it.
    pub fn list(&mut self, list: Mods, current: Option<&Path>) -> Task<Message> {
        self.list = Mods {
            mods: list
                .mods
                .into_iter()
                .filter(Mod::is_for_this_system)
                .collect(),
            ..list
        };
        self.unfetched = None;
        self.found.clear();
        self.foreign.clear();
        self.unfit.clear();
        self.excluded.clear();
        self.lacking.clear();
        self.gates.clear();
        // What is being looked at with the old list is looked at again.
        for again in self.finding.values_mut() {
            *again = true;
        }

        current.map_or_else(Task::none, |root| self.find(root))
    }

    /// Notes why the list couldn't be fetched, which the section says while
    /// there is none.
    pub fn unfetched(&mut self, error: String) {
        if self.list.mods.is_empty() {
            self.unfetched = Some(error);
        }
    }

    /// Looks at the client folder at `root` again: once at a time, a look
    /// asked for meanwhile coming after the one under way, since the
    /// folder may have changed; and not while something is being done to a
    /// mod there, which looks once done.
    pub fn find(&mut self, root: &Path) -> Task<Message> {
        if self.busy.as_ref().is_some_and(|work| work.root == root) {
            return Task::none();
        }

        match self.finding.get_mut(root) {
            Some(again) => {
                *again = true;
                Task::none()
            }
            None => {
                let _ = self.finding.insert(root.to_path_buf(), false);
                find(root, &self.list)
            }
        }
    }

    /// Repairs the mods in the client folder at `root` that need it, once
    /// it is next looked at, one after the other, each once: a language
    /// came, and the mods with files in each language want theirs in it.
    pub fn complete(&mut self, root: &Path) {
        let _ = self.completing.insert(root.to_path_buf(), BTreeSet::new());
    }

    /// Looks at the client folder at `root`, which the dialog shows, if it
    /// wasn't yet and isn't being.
    pub fn shown(&mut self, root: &Path) -> Task<Message> {
        if self.found.contains_key(root) || self.finding.contains_key(root) {
            Task::none()
        } else {
            self.find(root)
        }
    }

    /// The mod `id` of the list.
    fn listed(&self, id: &str) -> Option<&Mod> {
        self.list.mods.iter().find(|listed| listed.id == id)
    }

    /// What the client folder at `root` holds of the mod `id`, as last
    /// looked at.
    fn status(&self, root: &Path, id: &str) -> Option<&Status> {
        self.found.get(root)?.get(id)
    }

    /// Whether the client folder at `root` is offered `listed`: it holds it,
    /// or it suits the mod. A mod that suits only some clients waits for the
    /// folder to be looked at.
    fn offered(&self, root: &Path, listed: &Mod) -> bool {
        self.status(root, &listed.id).is_some()
            || match self.unfit.get(root) {
                Some(unfit) => !unfit.contains(&listed.id),
                None => listed.requires.is_empty() && listed.excludes.is_empty(),
            }
    }

    /// The files in the client folder at `root` in conflict with `listed`:
    /// those of something else where its newest release puts its own,
    /// which installing it sets aside, and those it isn't made for, which
    /// stay. Installing or updating it asks first.
    fn conflicting(&self, root: &Path, listed: &Mod) -> (&[String], &[String]) {
        fn of<'a>(
            files: &'a BTreeMap<PathBuf, BTreeMap<String, Vec<String>>>,
            root: &Path,
            id: &str,
        ) -> &'a [String] {
            files
                .get(root)
                .and_then(|files| files.get(id))
                .map_or(&[], Vec::as_slice)
        }

        (
            of(&self.foreign, root, &listed.id),
            of(&self.excluded, root, &listed.id),
        )
    }

    /// The language of `listed` chosen for the languages of the client
    /// folder at `root` it doesn't come in, if it lacks some: the one the
    /// player picked, or [`mods::fallback`].
    fn instead(&self, root: &Path, listed: &Mod) -> Option<Locale> {
        self.lacking
            .get(root)
            .and_then(|lacking| lacking.get(&listed.id))
            .filter(|lacking| !lacking.is_empty())?;

        self.instead
            .get(root)
            .and_then(|chosen| chosen.get(&listed.id))
            .copied()
            .or_else(|| mods::fallback(listed))
    }

    /// Whether installing the newest release of the mod `id` in the client
    /// folder at `root` updates it: it holds a release of the list, not a
    /// copy installed by hand, which is installed over.
    fn updating(&self, root: &Path, id: &str) -> bool {
        self.status(root, id).is_some_and(|status| {
            !status
                .problems
                .iter()
                .any(|problem| matches!(problem, Problem::Unknown { .. }))
        })
    }

    /// Why `listed` can't be installed in the client folder at `root`, or
    /// updated there, if it can't: another mod there it can't be beside, an
    /// alternative to it or one that uses the same paths.
    fn blocked(&self, root: &Path, listed: &Mod) -> Option<String> {
        let held = self.found.get(root)?;
        // What its archives would break comes first: it holds for an
        // update as much as for a first install.
        if let Some(why) = self
            .install_of(root, &listed.id)
            .and_then(|install| install.blocked.clone())
        {
            return Some(why);
        }
        if held.contains_key(&listed.id) {
            return None;
        }

        let alternative = self
            .list
            .alternatives(listed)
            .into_iter()
            .find(|other| held.contains_key(&other.id));
        if let Some(other) = alternative {
            return Some(format!(
                "“{}” is installed, and it conflicts with this one.",
                other.name
            ));
        }

        self.list
            .mods
            .iter()
            .filter(|other| other.id != listed.id && held.contains_key(&other.id))
            .find_map(|other| {
                let shared = mods::conflicts(listed, other);
                (!shared.is_empty()).then(|| {
                    format!(
                        "“{}” is installed, and it conflicts with this one: both use {}.",
                        other.name,
                        names(&shared),
                    )
                })
            })
    }

    /// What is being done to a mod that quitting would cut short, for
    /// people: installing, updating, repairing or removing it.
    pub fn under_way(&self) -> Option<String> {
        self.busy.as_ref().map(|work| work.said.clone())
    }

    /// Whether starting the game of the client at `root` waits for the
    /// launcher to look at its mods first, to repair one, or to finish
    /// doing something to one.
    pub fn holds_back(&self, root: &Path) -> bool {
        self.busy.as_ref().is_some_and(|work| work.root == root)
            || self
                .found
                .get(root)
                .is_none_or(|found| found.values().any(|status| need(status).is_some()))
    }

    /// The DLLs the mods in the client folder at `root` put beside
    /// `Wow.exe`, by name without `.dll`: Wine has to be told to load those
    /// from there. A release's as released, and whichever a copy installed
    /// by hand has.
    pub fn natives(&self, root: &Path) -> Vec<String> {
        self.found
            .get(root)
            .into_iter()
            .flatten()
            .flat_map(|(id, status)| {
                let by_hand = status.problems.iter().find_map(|problem| match problem {
                    Problem::Unknown { natives, .. } => Some(natives.clone()),
                    _ => None,
                });
                by_hand.unwrap_or_else(|| {
                    status
                        .problems
                        .is_empty()
                        .then(|| self.listed(id)?.release(status.release.as_deref()?))
                        .flatten()
                        .map(Release::natives)
                        .unwrap_or_default()
                })
            })
            .collect()
    }

    /// Takes note of what the client folder at `root` holds of the mods, as
    /// `seen` says.
    fn note(&mut self, root: &Path, seen: Seen) {
        for (id, status) in &seen.held {
            log::info!("{id} in {}: {status:?}", root.display());
        }
        for (id, files) in &seen.foreign {
            log::info!(
                "{id} in {}: {} belong to something else",
                root.display(),
                files.join(", ")
            );
        }
        let _ = self.found.insert(root.to_path_buf(), seen.held);
        let _ = self.foreign.insert(root.to_path_buf(), seen.foreign);
        let _ = self.unfit.insert(root.to_path_buf(), seen.unfit);
        let _ = self.excluded.insert(root.to_path_buf(), seen.excluded);
        let _ = self.lacking.insert(root.to_path_buf(), seen.lacking);
    }

    /// Handles an interaction with the banners or the section of the
    /// `current` client, `installation` once it opened: `config` says how it
    /// starts, nothing is changed while its game runs (`playing`),
    /// downloads use `client`, and failures go to `errors`.
    #[allow(clippy::too_many_arguments)]
    pub fn update(
        &mut self,
        message: Message,
        current: Option<&Path>,
        installation: Option<&Installation>,
        config: &Config,
        playing: bool,
        client: Result<&reqwest::Client, &str>,
        errors: &mut Errors,
    ) -> Action<Instruction, Message> {
        let installation = installation.filter(|installation| Some(installation.root()) == current);

        match message {
            Message::Verified(root, seen) => {
                // A look going on keeps going: it reads what this found
                // amiss, as the files that differ are no longer trusted.
                self.note(&root, seen);

                Action::task(self.judge(&root))
            }
            Message::Found(root, seen) => {
                self.note(&root, seen);
                // Asked meanwhile: the folder may have changed since.
                if self.finding.remove(&root) == Some(true) {
                    return Action::task(self.find(&root));
                }

                // What its patches say of the mods is worked out now that
                // the mods are known.
                let judging = self.judge(&root);

                // The next mod to repair after a language came, in the
                // client shown; once done, it is looked at again.
                let Some(repaired) = self.completing.get(&root) else {
                    return Action::task(judging);
                };
                let next = self.found.get(&root).and_then(|found| {
                    found
                        .iter()
                        .find(|(id, status)| need(status).is_some() && !repaired.contains(*id))
                        .map(|(id, _)| id.clone())
                });
                match next {
                    Some(id) if current == Some(root.as_path()) && self.busy.is_none() => {
                        if let Some(repaired) = self.completing.get_mut(&root) {
                            let _ = repaired.insert(id.clone());
                        }
                        log::info!("Repairing {id} in {}: a language came", root.display());
                        let mut action = self.update(
                            Message::Repair(id),
                            current,
                            installation,
                            config,
                            playing,
                            client,
                            errors,
                        );
                        action.task = Task::batch([action.task, judging]);
                        action
                    }
                    Some(_) => Action::task(judging),
                    None => {
                        let _ = self.completing.remove(&root);
                        Action::task(judging)
                    }
                }
            }
            Message::OpenPage(page) => {
                log::info!("Opening {page}");
                Action::task(crate::opening(page, None).map(Message::Unopened))
            }
            Message::Unopened(error) => {
                errors.report(error);
                Action::none()
            }
            Message::Install(id) => {
                let (Some(root), Some(listed)) = (current, self.listed(&id)) else {
                    return Action::none();
                };
                // As the section says, with no button to press: not before
                // the client is looked at again.
                if !self.found.contains_key(root)
                    || self.patches.contains(root)
                    || self.blocked(root, listed).is_some()
                {
                    return Action::none();
                }
                let (foreign, excluded) = self.conflicting(root, listed);
                // A copy installed by hand is overridden, backed up.
                let manual = self.status(root, &id).is_some() && !self.updating(root, &id);
                // What its archives override, or what overrides them, once
                // the launcher knows. Until it does, nothing is asked: the
                // release is downloaded and looked at first, and everything
                // is asked at once.
                let overrides = self
                    .install_of(root, &id)
                    .is_some_and(|install| !install.asks.is_empty());
                if !self.unjudged(root, listed)
                    && (manual || overrides || !foreign.is_empty() || !excluded.is_empty())
                {
                    self.removing = None;
                    self.confirming = Some((root.to_path_buf(), id));
                    return Action::none();
                }

                self.install(
                    &id,
                    false,
                    current,
                    installation,
                    config,
                    playing,
                    client,
                    errors,
                )
            }
            Message::ConfirmInstall => {
                let Some((root, id)) = self.confirming.take() else {
                    return Action::none();
                };
                if current != Some(root.as_path()) {
                    return Action::none();
                }

                // After the download: its files are in the cache, or here.
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.fetched.is_some() && pending.listed.id == id)
                {
                    return self.place_pending(true, installation, config, playing, client, errors);
                }

                self.install(
                    &id,
                    true,
                    current,
                    installation,
                    config,
                    playing,
                    client,
                    errors,
                )
            }
            Message::CancelInstall => {
                self.confirming = None;
                // What was downloaded stays in the cache for next time.
                self.pending = None;
                Action::none()
            }
            Message::Instead(id, language) => {
                if let Some(root) = current {
                    let _ = self
                        .instead
                        .entry(root.to_path_buf())
                        .or_default()
                        .insert(id, language);
                }
                Action::none()
            }
            Message::Repair(id) => {
                let Some(listed) = self.listed(&id).cloned() else {
                    return Action::none();
                };
                let Some(release) = current
                    .and_then(|root| self.status(root, &id))
                    .and_then(|status| target(&listed, status))
                    .cloned()
                else {
                    return Action::none();
                };
                let busy = format!("Repairing “{}”", listed.name);
                let done = format!("“{}” is repaired.", listed.name);

                self.start(
                    installation,
                    config,
                    playing,
                    client,
                    errors,
                    listed,
                    release,
                    false,
                    // As its marker says.
                    None,
                    busy,
                    done,
                    // The same release as there: nothing to judge.
                    false,
                )
            }
            Message::Remove(id) => {
                if let Some(installation) = installation {
                    self.confirming = None;
                    self.removing = Some((installation.root().to_path_buf(), id, false));
                }
                Action::none()
            }
            Message::DeleteSettings(delete) => {
                if let Some((_, _, deleting)) = &mut self.removing {
                    *deleting = delete;
                }
                Action::none()
            }
            Message::CancelRemove => {
                self.removing = None;
                Action::none()
            }
            Message::ConfirmRemove => {
                let Some(installation) = installation.filter(|_| !playing && self.busy.is_none())
                else {
                    return Action::none();
                };
                let root = installation.root().to_path_buf();
                let Some((id, delete)) = self
                    .removing
                    .as_ref()
                    .filter(|(from, ..)| *from == root)
                    .map(|(_, id, delete)| (id.clone(), *delete))
                else {
                    return Action::none();
                };
                let Some(listed) = self.listed(&id).cloned() else {
                    return Action::none();
                };
                self.removing = None;
                log::info!("Removing {} from {}", listed.name, root.display());
                self.busy = Some(Work {
                    root: root.clone(),
                    id: id.clone(),
                    said: format!("Removing “{}”", listed.name),
                    stage: None,
                    steps: 0,
                    looks: false,
                });
                self.done = None;
                self.stopped = None;

                let installation = installation.clone();
                let removed = format!("“{}” is removed.", listed.name);
                Action::task(Task::perform(
                    blocking(move || mods::uninstall(&installation, &listed, !delete)),
                    move |result| Message::Done(root, id, result.map(|()| removed)),
                ))
            }
            Message::Dismiss => {
                self.done = None;
                self.stopped = None;
                Action::none()
            }
            Message::Progressed(root, id, stage) => {
                if let Some(work) = self.busy.as_mut().filter(|work| work.on(&root, &id)) {
                    work.stage = Some(stage);
                }
                Action::none()
            }
            Message::Judged(root, ticket, verdicts) => {
                if self.judged.get(&root) == Some(&ticket) && self.scans.contains_key(&root) {
                    let gates = self.gates.entry(root).or_default();

                    // What an install just found out stays, if this didn't
                    // know yet.
                    for (id, mut verdict) in verdicts {
                        if verdict.install.is_none() {
                            verdict.install = gates.get(&id).and_then(|old| old.install.clone());
                        }
                        let _ = gates.insert(id, verdict);
                    }
                }
                Action::none()
            }
            Message::Fetched(root, id, result) => {
                let Some(mut pending) = self
                    .pending
                    .take()
                    .filter(|pending| pending.root == root && pending.listed.id == id)
                else {
                    return Action::none();
                };

                let fetched = match result {
                    Ok(fetched) => fetched,
                    Err(error) => {
                        return self.update(
                            Message::Done(root, id, Err(error)),
                            current,
                            installation,
                            config,
                            playing,
                            client,
                            errors,
                        );
                    }
                };
                self.busy = None;

                if let Some(install) = &fetched.install {
                    self.gates
                        .entry(root.clone())
                        .or_default()
                        .entry(id.clone())
                        .or_default()
                        .install = Some(install.clone());
                }
                let (foreign, excluded) = self.conflicting(&root, &pending.listed);
                let asks = fetched
                    .install
                    .as_ref()
                    .is_some_and(|install| !install.asks.is_empty())
                    || !foreign.is_empty()
                    || !excluded.is_empty()
                    || (self.status(&root, &id).is_some() && !self.updating(&root, &id));
                let blocked = fetched
                    .install
                    .as_ref()
                    .and_then(|install| install.blocked.clone());

                // Nothing was touched: what was downloaded stays in the
                // cache for next time.
                if let Some(why) = blocked {
                    log::info!("Not installing {id} in {}: {why}", root.display());
                    self.stopped = Some(Work {
                        root,
                        id,
                        said: why,
                        stage: None,
                        steps: 0,
                        looks: false,
                    });
                    return Action::none();
                }

                pending.fetched = Some(fetched);
                if asks && !pending.confirmed {
                    self.removing = None;
                    self.confirming = Some((root, id));
                    self.pending = Some(pending);
                    return Action::none();
                }

                let over = pending.confirmed;
                self.pending = Some(pending);
                self.place_pending(over, installation, config, playing, client, errors)
            }
            Message::Done(root, id, result) => {
                self.busy = None;
                self.pending = None;
                // What was known of the client is stale: the rows wait,
                // their buttons off, for it to be looked at again, which
                // replaces the rest.
                let _ = self.found.remove(&root);
                let _ = self.gates.remove(&root);
                match result {
                    Ok(said) => {
                        log::info!("{}: {said}", root.display());
                        self.done = Some(Work {
                            root: root.clone(),
                            id,
                            said,
                            stage: None,
                            steps: 0,
                            looks: false,
                        });
                    }
                    Err(error) => errors.report(format!("“{}”: {error}", self.name(&id))),
                }

                Action::instruction(Instruction::Changed(root))
            }
        }
    }

    /// The name of the mod `id`, or the id when the list has it no more.
    fn name(&self, id: &str) -> String {
        self.listed(id)
            .map_or_else(|| id.to_owned(), |listed| listed.name.clone())
    }

    /// Installs the newest release of the mod `id` in `installation`, the
    /// `current` client, `over` the files of something else where it goes
    /// or not (see [`State::start`]).
    #[allow(clippy::too_many_arguments)]
    fn install(
        &mut self,
        id: &str,
        over: bool,
        current: Option<&Path>,
        installation: Option<&Installation>,
        config: &Config,
        playing: bool,
        client: Result<&reqwest::Client, &str>,
        errors: &mut Errors,
    ) -> Action<Instruction, Message> {
        let Some(listed) = self.listed(id).cloned() else {
            return Action::none();
        };
        let Some(release) = listed.newest().cloned() else {
            return Action::none();
        };
        let updating = current.is_some_and(|root| self.updating(root, id));
        let (name, version) = (&listed.name, &release.version);
        let (busy, done) = if updating {
            (
                format!("Updating “{name}” to {version}"),
                format!("“{name}” is updated to {version}."),
            )
        } else {
            (
                format!("Installing “{name}” {version}"),
                format!("“{name}” {version} is installed."),
            )
        };
        let instead = current.and_then(|root| self.instead(root, &listed));
        // Its archives are looked at first unless it is known what they do.
        let gated = current.is_some_and(|root| self.unjudged(root, &listed));

        self.start(
            installation,
            config,
            playing,
            client,
            errors,
            listed,
            release,
            over,
            instead,
            busy,
            done,
            gated,
        )
    }

    /// Installs `release` of `listed` in `installation`, the current client,
    /// which starts as `config` says, in place of what is there, its
    /// alternatives too, and `over` the files of something else where it
    /// goes, backing them up, or not; then runs its setup. The client's
    /// languages it doesn't come in get those of `instead`, or of the one
    /// its marker names. Not while its game runs (`playing`) or something
    /// else is being done. `busy` says what is being done meanwhile, with
    /// how far it got, and `done` what was once it is. When `gated`, the
    /// release is only downloaded and its archives looked at (see
    /// [`fetch`]): [`State::place_pending`] goes on from what that finds.
    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        installation: Option<&Installation>,
        config: &Config,
        playing: bool,
        client: Result<&reqwest::Client, &str>,
        errors: &mut Errors,
        listed: Mod,
        release: Release,
        over: bool,
        instead: Option<Locale>,
        busy: String,
        done: String,
        gated: bool,
    ) -> Action<Instruction, Message> {
        let Some((installation, client)) =
            self.ready(installation, playing, client, errors, &listed.name)
        else {
            return Action::none();
        };
        let root = installation.root().to_path_buf();
        let id = listed.id.clone();
        self.removing = None;
        self.confirming = None;
        self.done = None;
        self.stopped = None;
        self.pending = None;

        log::info!(
            "Installing {} {} in {}",
            listed.name,
            release.version,
            root.display()
        );
        let gate = gated
            .then(|| Some((self.facts.clone()?, self.scans.get(&root)?.0.clone())))
            .flatten();
        let Some((facts, scan)) = gate else {
            return self.place(
                &installation,
                config,
                client,
                Pending {
                    root,
                    listed,
                    release,
                    instead,
                    busy,
                    done,
                    confirmed: over,
                    fetched: None,
                },
                over,
                BTreeMap::new(),
                false,
            );
        };

        self.busy = Some(Work {
            root: root.clone(),
            id: id.clone(),
            said: busy.clone(),
            stage: Some(Stage::Downloading {
                received: 0,
                total: None,
            }),
            steps: 3 + release.setup.len(),
            looks: true,
        });
        self.pending = Some(Pending {
            root: root.clone(),
            listed: listed.clone(),
            release: release.clone(),
            instead,
            busy,
            done,
            confirmed: over,
            fetched: None,
        });

        let (progressed, fetched) = ((root.clone(), id.clone()), (root.clone(), id));
        Action::task(Task::sip(
            fetch(
                client,
                self.github.clone(),
                installation.clone(),
                listed,
                release,
                self.list.clone(),
                instead,
                self.cache.clone(),
                facts,
                scan,
            ),
            move |stage| Message::Progressed(progressed.0.clone(), progressed.1.clone(), stage),
            move |result| Message::Fetched(fetched.0, fetched.1, result),
        ))
    }

    /// The client to install in, and the way to download: not while its game
    /// runs (`playing`) or something else is being done, and not without a
    /// way to download.
    fn ready(
        &self,
        installation: Option<&Installation>,
        playing: bool,
        client: Result<&reqwest::Client, &str>,
        errors: &mut Errors,
        name: &str,
    ) -> Option<(Installation, reqwest::Client)> {
        let installation = installation.filter(|_| !playing && self.busy.is_none())?;
        let client = match client {
            Ok(client) => client.clone(),
            Err(error) => {
                errors.report(format!("Could not download “{name}”: {error}"));
                return None;
            }
        };

        Some((installation.clone(), client))
    }

    /// Goes on with the install that [`fetch`] found out about, `over` the
    /// files of something else or not.
    fn place_pending(
        &mut self,
        over: bool,
        installation: Option<&Installation>,
        config: &Config,
        playing: bool,
        client: Result<&reqwest::Client, &str>,
        errors: &mut Errors,
    ) -> Action<Instruction, Message> {
        let Some(name) = self
            .pending
            .as_ref()
            .map(|pending| pending.listed.name.clone())
        else {
            return Action::none();
        };
        let Some((installation, client)) = self.ready(installation, playing, client, errors, &name)
        else {
            return Action::none();
        };
        let Some(mut pending) = self.pending.take() else {
            return Action::none();
        };
        let looks = pending.fetched.is_some();
        let carried = pending
            .fetched
            .take()
            .map(|fetched| {
                Arc::try_unwrap(fetched.downloads).unwrap_or_else(|shared| (*shared).clone())
            })
            .unwrap_or_default();

        self.confirming = None;
        self.place(&installation, config, client, pending, over, carried, looks)
    }

    /// Puts the release of `pending` in `installation` (see [`place`]), with
    /// the `carried` downloads that are not in the cache; `looks` says
    /// whether its archives were looked at first.
    #[allow(clippy::too_many_arguments)]
    fn place(
        &mut self,
        installation: &Installation,
        config: &Config,
        client: reqwest::Client,
        pending: Pending,
        over: bool,
        carried: BTreeMap<String, Vec<u8>>,
        looks: bool,
    ) -> Action<Instruction, Message> {
        let Pending {
            root,
            listed,
            release,
            instead,
            busy,
            done,
            ..
        } = pending;
        let id = listed.id.clone();

        self.busy = Some(Work {
            root: root.clone(),
            id: id.clone(),
            said: busy,
            stage: Some(Stage::Downloading {
                received: 0,
                total: None,
            }),
            steps: 2 + usize::from(looks) + release.setup.len(),
            looks,
        });

        let alternatives: Vec<Mod> = self
            .list
            .alternatives(&listed)
            .into_iter()
            .cloned()
            .collect();
        let (progressed, worked) = ((root.clone(), id.clone()), (root.clone(), id));
        Action::task(Task::sip(
            place(
                client,
                self.github.clone(),
                installation.clone(),
                config.launch(&root),
                listed,
                release,
                alternatives,
                over,
                instead,
                self.cache.clone(),
                carried,
            ),
            move |stage| Message::Progressed(progressed.0.clone(), progressed.1.clone(), stage),
            move |result| Message::Done(worked.0, worked.1, result.map(|()| done)),
        ))
    }

    /// The banners above the addons about the mods in the client folder at
    /// `root`: those the launcher needs to check or repair before starting
    /// the game, and what was just done; `playing` says whether its game
    /// runs.
    pub fn banner(&self, root: &Path, playing: bool) -> Option<Element<'_, Message>> {
        let banners: Vec<Element<'_, Message>> = self
            .list
            .mods
            .iter()
            .filter_map(|listed| self.banner_of(root, listed, playing))
            .collect();

        (!banners.is_empty()).then(|| column(banners).spacing(14).boxed())
    }

    /// The banner about `listed` in the client folder at `root`, if there is
    /// something to say.
    fn banner_of<'a>(
        &'a self,
        root: &Path,
        listed: &'a Mod,
        playing: bool,
    ) -> Option<Element<'a, Message>> {
        let busy = self.busy.as_ref().filter(|work| work.on(root, &listed.id));
        let needed = self
            .status(root, &listed.id)
            .and_then(|status| Some((status, need(status)?)));
        // An install that was stopped says why, where its progress was.
        let done = self
            .stopped
            .as_ref()
            .filter(|work| work.on(root, &listed.id))
            .map(|work| (work, style::Tone::Warning))
            .or_else(|| {
                self.done
                    .as_ref()
                    .filter(|work| work.on(root, &listed.id))
                    .map(|work| (work, style::Tone::Success))
            });

        let (said, buttons, tone): (Element<'a, Message>, _, _) = match (busy, needed, done) {
            (Some(work), _, _) => (
                column![text(format!("{}…", work.said)), progress(work)]
                    .spacing(4)
                    .boxed(),
                row![],
                style::Tone::Warning,
            ),
            (None, Some((status, Need::Repair)), _) => {
                let why: Vec<String> = status
                    .problems
                    .iter()
                    .map(|problem| sentence(listed, problem))
                    .collect();
                let repair = button("Repair").style(style::primary);
                let repair: Element<'_, Message> = match &self.busy {
                    _ if playing => style::tip(repair, CLOSE_THE_GAME).boxed(),
                    Some(work) => style::tip(repair, wait(work)).boxed(),
                    None => repair.on_press(Message::Repair(listed.id.clone())).boxed(),
                };

                (
                    text(format!(
                        "The launcher needs to repair “{}” before starting the game. {}",
                        listed.name,
                        why.join(" ")
                    ))
                    .boxed(),
                    row![repair],
                    style::Tone::Warning,
                )
            }
            (None, None, Some((work, tone))) => (
                text(&work.said).boxed(),
                row![
                    button("Dismiss")
                        .style(style::quiet)
                        .on_press(Message::Dismiss)
                ],
                tone,
            ),
            (None, None, None) => return None,
        };

        Some(
            container(
                row![container(said).width(Fill), buttons.spacing(8)]
                    .spacing(8)
                    .align_y(Center),
            )
            .padding([6, 12])
            .style(style::banner(tone))
            .boxed(),
        )
    }

    /// The section of the clients dialog, for the client folder at `root`;
    /// `playing` says whether its game runs.
    pub fn view<'a>(&'a self, root: &'a Path, playing: bool) -> impl Widget<Message> + 'a {
        let unlisted = self.list.mods.is_empty().then(|| {
            text(match &self.unfetched {
                Some(error) => format!("Could not fetch the list of mods: {error}"),
                None => "None listed yet.".to_owned(),
            })
            .size(13)
            .style(text::secondary)
        });

        column![
            style::heading("Mods", 16.0),
            unlisted,
            column(
                self.list
                    .mods
                    .iter()
                    .filter(|listed| self.offered(root, listed))
                    .map(|listed| self.view_of(root, listed, playing))
            )
            .spacing(14),
            text("Servers may forbid modified clients, and their anti-cheat may detect it.")
                .size(12)
                .style(text::secondary),
        ]
        .spacing(8)
        .width(Fill)
    }

    /// The warning that `listed` doesn't come in some of the languages of
    /// the client folder at `root`, beside the pick of the one of its that
    /// goes there instead, when installing it (`installing`).
    fn elsewhere<'a>(
        &'a self,
        root: &Path,
        listed: &'a Mod,
        installing: bool,
    ) -> Option<Element<'a, Message>> {
        let lacking = self
            .lacking
            .get(root)?
            .get(&listed.id)
            .filter(|lacking| installing && !lacking.is_empty())?;
        let id = listed.id.clone();
        let picker = pick_list(
            self.instead(root, listed),
            mods::editions(listed),
            Locale::to_string,
        )
        .style(style::picker)
        .menu_style(style::choices)
        .text_size(13)
        .padding([2, 8])
        .on_select(move |language| Message::Instead(id.clone(), language));

        Some(
            row![
                text(format!(
                    "“{}” doesn't come in {}: it goes there in",
                    listed.name,
                    Locale::codes(lacking)
                ))
                .size(13)
                .style(text::warning),
                picker,
                text("instead, its text in that language.")
                    .size(13)
                    .style(text::warning),
            ]
            .spacing(6)
            .align_y(Center)
            .wrap()
            .boxed(),
        )
    }

    /// What the section says of `listed` in the client folder at `root`.
    fn view_of<'a>(
        &'a self,
        root: &'a Path,
        listed: &'a Mod,
        playing: bool,
    ) -> Element<'a, Message> {
        let found = self.found.get(root);
        let status = found.and_then(|found| found.get(&listed.id));
        let newest = listed.newest();
        let newer = status
            .and_then(|status| status.release.as_deref())
            .zip(newest)
            .filter(|(installed, newest)| update::newer(&newest.version, installed))
            .map(|(_, newest)| newest);
        let needed = status.and_then(need);
        // Installed by hand, matching no release: at this version, if its
        // files say.
        let by_hand = status.and_then(|status| {
            status.problems.iter().find_map(|problem| match problem {
                Problem::Unknown { version, .. } => Some(version.as_deref()),
                _ => None,
            })
        });
        let idle = self.busy.is_none();
        // Not there, older, or a copy installed by hand: the newest release
        // is offered.
        let installable = status.is_none() || newer.is_some() || by_hand.is_some();
        // Only once the folder was looked at, and when there is something
        // to install.
        let blocked = found
            .filter(|_| installable)
            .and_then(|_| self.blocked(root, listed));
        // Until the folder is known, after it was worked on or while it is
        // looked at again, nothing is done to it.
        let patches = self.is_waiting_for_patches(root);
        let looking = found.is_none() || self.finding.contains_key(root) || patches;

        let state: Element<'a, Message> = match (found, status) {
            (None, _) => text("Looking…").size(14).style(text::secondary).boxed(),
            (Some(_), _) if patches => text("Looking…").size(14).style(text::secondary).boxed(),
            (Some(_), None) => text("Not installed.").size(14).boxed(),
            (Some(_), Some(status)) => text(match (by_hand, &status.release, newer) {
                (Some(Some(version)), ..) if listed.release(version).is_some() => {
                    format!(
                        "Version {version} installed manually; its files differ from the release."
                    )
                }
                (Some(Some(version)), ..) => format!("Version {version} installed manually."),
                (Some(None), ..) => "Installed manually, unrecognised version".to_owned(),
                (None, Some(version), Some(newer)) => {
                    format!("Version {version} installed; {} is out.", newer.version)
                }
                (None, Some(version), None) => format!("Version {version} installed."),
                (None, None, _) => "Installed.".to_owned(),
            })
            .size(14)
            .boxed(),
        };

        let action = |label: String,
                      message: Message,
                      look: fn(&iced::Theme, button::Status) -> button::Style|
         -> Element<'a, Message> {
            let button = button(text(label).size(13)).style(look).padding([4, 12]);

            match &self.busy {
                _ if playing => style::tip(button, CLOSE_THE_GAME).boxed(),
                Some(work) => style::tip(button, wait(work)).boxed(),
                None if patches => style::tip(button, crate::patches::LOOKING).boxed(),
                None if looking => style::tip(button, LOOKING).boxed(),
                None => button.on_press(message).boxed(),
            }
        };
        // What installs the newest release, off with the reason it can't.
        let install = |label: String| -> Element<'a, Message> {
            match &blocked {
                Some(why) => style::tip(
                    button(text(label).size(13))
                        .style(style::primary)
                        .padding([4, 12]),
                    why.clone(),
                )
                .boxed(),
                None => action(label, Message::Install(listed.id.clone()), style::primary),
            }
        };

        // A copy installed by hand is offered the newest release in its
        // place; one installed from the list, a newer one.
        let offered = if by_hand.is_some() {
            newest.map(|release| (format!("Install {}", release.version), release))
        } else {
            newer.map(|release| (format!("Update to {}", release.version), release))
        };
        // Files in conflict with it, when there is something to install:
        // installing asks first.
        let (foreign, excluded) = found
            .filter(|_| installable)
            .map_or((&[][..], &[][..]), |_| self.conflicting(root, listed));
        let conflicts: Vec<String> = foreign.iter().chain(excluded).cloned().collect();
        // A copy installed by hand is overridden, backed up: installing asks.
        let manual = by_hand.is_some() && found.is_some();
        // What the patches say of it, once its archives are known: what
        // installing it would break, what it would override or be
        // overridden by, and what overrides what it has.
        let verdict = self
            .gates
            .get(root)
            .and_then(|gates| gates.get(&listed.id))
            .filter(|_| found.is_some() && !patches);
        let judged = verdict
            .and_then(|verdict| verdict.install.as_ref())
            .filter(|_| installable);
        let gate_says: Vec<&str> = judged
            .into_iter()
            .flat_map(|install| {
                install
                    .blocked
                    .as_deref()
                    .into_iter()
                    .chain(install.asks.iter().map(String::as_str))
            })
            .chain(
                verdict
                    .filter(|_| status.is_some())
                    .into_iter()
                    .flat_map(|verdict| verdict.installed.iter().map(String::as_str)),
            )
            .collect();
        let asked = judged.map_or(&[][..], |judged| judged.asks.as_slice());
        let anything = manual || !conflicts.is_empty() || !asked.is_empty();

        let buttons = match status {
            None => row![newest.map(|release| install(format!("Install {}", release.version)))],
            Some(_) => row![
                offered.map(|(label, _)| install(label)),
                (needed == Some(Need::Repair)).then(|| action(
                    "Repair".to_owned(),
                    Message::Repair(listed.id.clone()),
                    style::secondary
                )),
                action(
                    "Uninstall".to_owned(),
                    Message::Remove(listed.id.clone()),
                    style::secondary
                ),
            ],
        }
        .spacing(8);

        // A copy installed by hand is said to be, above: no problem.
        let problems = status
            .into_iter()
            .flat_map(|status| &status.problems)
            .filter(|problem| !matches!(problem, Problem::Unknown { .. }))
            .map(|problem| {
                let tone: Tone = text::warning;
                text(sentence(listed, problem)).size(13).style(tone)
            });

        let note = self
            .busy
            .as_ref()
            .filter(|work| work.on(root, &listed.id))
            .map(|work| {
                column![
                    text(format!("{}…", work.said))
                        .size(13)
                        .style(text::secondary),
                    progress(work),
                ]
                .spacing(4)
                .boxed()
            })
            .or_else(|| {
                self.done
                    .as_ref()
                    .filter(|work| work.on(root, &listed.id))
                    .map(|work| text(&work.said).size(13).style(text::success).boxed())
            });

        let removal = self
            .removing
            .as_ref()
            .filter(|(from, id, _)| from == root && *id == listed.id && status.is_some())
            .map(|(_, _, delete)| {
                container(
                    column![
                        text(format!("Remove “{}” from this client?", listed.name)).size(14),
                        row![
                            (!listed.settings.is_empty()).then(|| {
                                checkbox(*delete)
                                    .style(style::check)
                                    .label("Delete its settings too")
                                    .on_toggle(Message::DeleteSettings)
                                    .text_size(13)
                            }),
                            space::horizontal(),
                            button("Cancel")
                                .style(style::quiet)
                                .on_press(Message::CancelRemove),
                            button("Remove")
                                .style(style::danger)
                                .on_press_maybe(idle.then_some(Message::ConfirmRemove)),
                        ]
                        .spacing(8)
                        .align_y(Center),
                    ]
                    .spacing(6),
                )
                .padding([6, 12])
                .style(style::banner(style::Tone::Danger))
            });

        let confirmation = self
            .confirming
            .as_ref()
            .filter(|(from, id)| from == root && *id == listed.id && anything)
            .map(|_| {
                let (verb, doing) = if self.updating(root, &listed.id) {
                    ("Update", "updating")
                } else {
                    ("Install", "installing")
                };
                // Each is backed up, those of something else where it goes
                // and those it isn't made for alike.
                let version =
                    newest.map_or("the newest release", |release| release.version.as_str());
                let files = manual
                    .then(|| {
                        text(format!(
                            "“{}” is installed manually: {doing} {version} overrides it safely \
                             (backed up).",
                            listed.name,
                        ))
                        .size(13)
                        .boxed()
                    })
                    .into_iter()
                    .chain(conflicts.iter().map(|path| {
                        text(format!(
                            "{path} belongs to something else: {doing} “{}” overrides it \
                             safely (backed up).",
                            listed.name,
                        ))
                        .size(13)
                        .boxed()
                    }))
                    .chain(asked.iter().map(|line| text(line).size(13).boxed()));

                container(
                    column![
                        column(files).spacing(4),
                        row![
                            text(format!("{verb} “{}” anyway?", listed.name))
                                .size(14)
                                .width(Fill),
                            button("Cancel")
                                .style(style::quiet)
                                .on_press(Message::CancelInstall),
                            button(text(format!("{verb} anyway")))
                                .style(style::primary)
                                .on_press_maybe(idle.then_some(Message::ConfirmInstall)),
                        ]
                        .spacing(8)
                        .align_y(Center),
                    ]
                    .spacing(6),
                )
                .padding([6, 12])
                .style(style::banner(style::Tone::Warning))
            });

        column![
            // What it is, and beside it what can be done, and how it is.
            row![
                column![
                    row![
                        text(&listed.name)
                            .size(14)
                            .font(style::BOLD)
                            .style(style::gold),
                        button(text(format!("{} ↗", listed.source.name)).size(13))
                            .style(style::link)
                            .padding(0)
                            .on_press(Message::OpenPage(listed.source.page.clone())),
                    ]
                    .spacing(8)
                    .align_y(Center),
                    (!listed.description.is_empty())
                        .then(|| text(&listed.description).size(13).style(text::secondary)),
                ]
                .spacing(8)
                .width(Fill),
                column![buttons, state].spacing(6).align_x(Right),
            ]
            .spacing(12),
            (!conflicts.is_empty()).then(|| {
                text(format!("In conflict with {}.", names(&conflicts)))
                    .size(13)
                    .style(text::warning)
            }),
            column(gate_says.into_iter().map(|line| {
                let tone: Tone = text::warning;
                text(line).size(13).style(tone)
            }))
            .spacing(4),
            self.elsewhere(root, listed, installable && newer.is_none()),
            column(problems).spacing(4),
            note,
            removal,
            confirmation,
        ]
        .spacing(8)
        .boxed()
    }
}

/// What the launcher needs to do to a mod, which a client folder holds as
/// `status` says, before starting the game, if anything: a copy installed
/// by hand is the player's, and one that couldn't be checked is left as
/// it is.
fn need(status: &Status) -> Option<Need> {
    let problems = status
        .problems
        .iter()
        .any(|problem| !matches!(problem, Problem::Unknown { .. } | Problem::Unchecked));

    problems.then_some(Need::Repair)
}

/// The release of `listed` that repairing installs, in a client folder
/// that holds it as `status` says: the one its marker names, or else the
/// newest.
fn target<'a>(listed: &'a Mod, status: &Status) -> Option<&'a Release> {
    status
        .release
        .as_deref()
        .and_then(|version| listed.release(version))
        .or_else(|| listed.newest())
}

/// How far `work` got, when it is an install: which of its steps it is at
/// (the download, looking at its archives when that is a step, putting the
/// files in place, then each step of the setup), with a bar for that step,
/// and what the setup last said. The bar fills as the step goes when the
/// step can tell how far it got, and glides while it goes on otherwise.
fn progress(work: &Work) -> Option<Element<'_, Message>> {
    let stage = work.stage.as_ref()?;
    let (step, what, said) = match stage {
        Stage::Downloading { received, total } => (
            1,
            match total {
                Some(total) => format!("downloading, {} of {}", size(*received), size(*total)),
                None => format!("downloading, {}", size(*received)),
            },
            None,
        ),
        Stage::Looking => (2, "looking at its archives".to_owned(), None),
        // Looking at its archives is a step of its own, when it is done.
        Stage::Installing => (
            2 + usize::from(work.looks),
            "putting its files in place".to_owned(),
            None,
        ),
        Stage::SettingUp { step, said, .. } => (
            2 + usize::from(work.looks) + step,
            "running its setup".to_owned(),
            said.as_deref(),
        ),
    };
    let steps = work.steps.max(step);
    let bar: Element<'_, Message> = match stage.fraction() {
        Some(fraction) => progress_bar(0.0..=1.0, fraction)
            .style(style::bar)
            .length(160)
            .girth(6)
            .boxed(),
        None => activity(160.0, 6.0).boxed(),
    };

    Some(
        column![
            row![
                bar,
                text(format!("Step {step} of {steps}: {what}"))
                    .size(13)
                    .style(text::secondary),
            ]
            .spacing(8)
            .align_y(Center),
            said.map(|said| text(said).size(12).style(text::secondary)),
        ]
        .spacing(4)
        .boxed(),
    )
}

/// What `problem` of `listed` means, for people.
fn sentence(listed: &Mod, problem: &Problem) -> String {
    let newest = listed
        .newest()
        .map_or("the newest release", |release| release.version.as_str());

    match problem {
        Problem::Unchecked => "Its files couldn't be checked against its releases.".to_owned(),
        Problem::Unknown { version, .. } => match version {
            Some(version) => format!("Version {version}, installed manually."),
            None => "Installed manually.".to_owned(),
        },
        Problem::Unlisted(version) => {
            format!("Version {version} is no longer listed: repairing installs {newest}.")
        }
        Problem::Missing(files) => format!("Missing: {}.", files.join(", ")),
        Problem::Changed(files) => format!("Not as released: {}.", files.join(", ")),
        Problem::NotSetUp(files) => format!("Its setup needs to run to make {}.", names(files)),
        Problem::Config(settings) => settings
            .iter()
            .map(|(name, set, needed)| {
                format!("Config.wtf sets {name} to {set}; it needs {needed}.")
            })
            .collect::<Vec<_>>()
            .join(" "),
    }
}

/// `names` as a sentence: `A`, `A and B`, `A, B and C`.
fn names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Downloads the release `listed` has in `release`, as it goes in
/// `installation`'s folder (see [`Release::downloads`]), looks at its
/// archives, and says what installing it would do to the client's patches
/// in `scan`'s graph (see [`gate`]), saying how far it got. Nothing is
/// changed in the client. Each download comes from the copy kept in
/// `cache` if there is one, else from `client`, or signed in to GitHub with
/// `github`; and is kept there as soon as it is known to be the release, so
/// stopping now loses nothing. What isn't kept goes on to [`place`]. What is
/// read of the archives is kept in `facts`.
#[allow(clippy::too_many_arguments)]
fn fetch(
    client: reqwest::Client,
    github: Option<github::Signed>,
    installation: Installation,
    listed: Mod,
    release: Release,
    list: Mods,
    instead: Option<Locale>,
    cache: Option<PathBuf>,
    facts: Cache,
    scan: Arc<Scan>,
) -> impl Straw<Fetched, Stage, String> {
    sipper(async move |mut progress| {
        let instead = instead.or_else(|| {
            listed
                .marker(installation.root())
                .and_then(|marker| marker.instead)
        });
        let placed = release
            .placed(installation.locale(), &installation.locales(), instead)
            .ok_or_else(|| describe(&crate::core::Error::NoLanguage))?;
        let urls: Vec<String> = placed.downloads().into_iter().map(|(url, _)| url).collect();
        let copy_of = |url: &str| {
            cache
                .as_deref()
                .map(|cache| kept(cache, &listed, &release, url))
        };

        let (downloads, cached) = gather(
            &client,
            github.as_ref(),
            &urls,
            &copy_of,
            BTreeMap::new(),
            &mut progress,
        )
        .await?;

        progress.send(Stage::Looking).await;
        let looked = tokio::task::spawn_blocking({
            let (installation, listed, release) =
                (installation.clone(), listed.clone(), release.clone());
            let scratch = cache.clone();

            move || {
                let archives = mods::read_archives(
                    installation.root(),
                    &listed,
                    &release,
                    &downloads,
                    instead,
                    scratch.as_deref(),
                    &facts,
                );
                let install = archives.map(|archives| {
                    let change = gate::change_of(installation.root(), &list, &listed, &release);

                    gate::install_in(
                        installation.root(),
                        &list,
                        &listed,
                        &change,
                        &scan.graph,
                        &archives,
                    )
                });

                (install, downloads)
            }
        })
        .await
        .map_err(|error| error.to_string())?;
        let (install, mut downloads) = looked;
        let install = install.map_err(|error| describe(&error))?;

        // What came from the net is kept now, since it is the release, and
        // dropped from memory when it is.
        for url in urls.iter().filter(|url| !cached.contains(url)) {
            if let (Some(path), Some(download)) = (copy_of(url), downloads.get(url)) {
                keep(&path, download, &release.version).await;

                if tokio::fs::metadata(&path).await.is_ok() {
                    let _ = downloads.remove(url);
                }
            }
        }

        Ok(Fetched {
            install,
            downloads: Arc::new(downloads),
        })
    })
}

/// Downloads what `urls` name that `copy_of` has no copy kept of, or reads
/// the copy, saying how much arrived; the `carried` downloads are there
/// already. Returns the downloads by address, and the addresses that came
/// from a copy kept.
async fn gather(
    client: &reqwest::Client,
    github: Option<&github::Signed>,
    urls: &[String],
    copy_of: &impl Fn(&str) -> Option<PathBuf>,
    mut carried: BTreeMap<String, Vec<u8>>,
    progress: &mut Sender<Stage>,
) -> Result<(BTreeMap<String, Vec<u8>>, Vec<String>), String> {
    let mut downloads = BTreeMap::new();
    let mut cached = Vec::new();

    for url in urls {
        let download = match carried.remove(url) {
            Some(download) => download,
            None => {
                let copy = match copy_of(url) {
                    Some(path) => tokio::fs::read(path).await.ok(),
                    None => None,
                };

                match copy {
                    Some(copy) => {
                        log::info!("{url}: as kept");
                        cached.push(url.clone());
                        copy
                    }
                    None => get(client, github, url, progress).await?,
                }
            }
        };
        let _ = downloads.insert(url.clone(), download);
    }

    Ok((downloads, cached))
}

/// Installs `release` of `listed` in `installation`'s folder, in place of
/// what is there and of its `alternatives` there, and `over` the files of
/// something else where it goes or not, then runs its setup the way
/// `launch` starts the game, saying how far it got. Each of its downloads
/// (one, or one per language: see [`Release::downloads`]) is one of the
/// `carried` ones that [`fetch`] could not keep, or comes from the copy
/// kept in `cache`, if there is one, or else is downloaded with `client`,
/// or signed in to GitHub with `github`, and kept there once installed.
/// Copies kept that aren't the release (cut short, or changed on disk) are
/// downloaded again. The client's languages it doesn't come in get those of
/// `instead`, or of the one its marker names.
#[allow(clippy::too_many_arguments)]
fn place(
    client: reqwest::Client,
    github: Option<github::Signed>,
    installation: Installation,
    launch: Launch,
    listed: Mod,
    release: Release,
    alternatives: Vec<Mod>,
    over: bool,
    instead: Option<Locale>,
    cache: Option<PathBuf>,
    carried: BTreeMap<String, Vec<u8>>,
) -> impl Straw<(), Stage, String> {
    sipper(async move |mut progress| {
        let instead = instead.or_else(|| {
            listed
                .marker(installation.root())
                .and_then(|marker| marker.instead)
        });
        // As it goes in this client: its downloads, and its setup.
        let placed = release
            .placed(installation.locale(), &installation.locales(), instead)
            .ok_or_else(|| describe(&crate::core::Error::NoLanguage))?;
        let urls: Vec<String> = placed.downloads().into_iter().map(|(url, _)| url).collect();
        let copy_of = |url: &str| {
            cache
                .as_deref()
                .map(|cache| kept(cache, &listed, &release, url))
        };

        // Downloaded before anything changes, but those kept.
        let (downloads, cached) = gather(
            &client,
            github.as_ref(),
            &urls,
            &copy_of,
            carried,
            &mut progress,
        )
        .await?;

        progress.send(Stage::Installing).await;
        // What installing says, and the downloads back, to keep.
        let install = |downloads: BTreeMap<String, Vec<u8>>| {
            let (target, listed, release, alternatives) = (
                installation.clone(),
                listed.clone(),
                release.clone(),
                alternatives.clone(),
            );
            tokio::task::spawn_blocking(move || {
                let alternatives: Vec<&Mod> = alternatives.iter().collect();
                let installed = mods::install_downloaded(
                    &target,
                    &listed,
                    &release,
                    &downloads,
                    &alternatives,
                    over,
                    instead,
                );
                (installed, downloads)
            })
        };
        let (installed, mut downloads) = install(downloads)
            .await
            .map_err(|error| error.to_string())?;
        let mut fresh: Vec<&String> = urls.iter().filter(|url| !cached.contains(url)).collect();
        match installed {
            Ok(()) => {}
            Err(error) if !cached.is_empty() && spoilt(&error) => {
                log::warn!(
                    "The copies kept of {} {} are no good ({}): downloading them again",
                    listed.name,
                    release.version,
                    describe(&error)
                );
                for url in &cached {
                    if let Some(path) = copy_of(url) {
                        let _ = tokio::fs::remove_file(path).await;
                    }
                    let download = get(&client, github.as_ref(), url, &mut progress).await?;
                    let _ = downloads.insert(url.clone(), download);
                }
                fresh = urls.iter().collect();
                progress.send(Stage::Installing).await;
                let (installed, again) = install(downloads)
                    .await
                    .map_err(|error| error.to_string())?;
                installed.map_err(|error| describe(&error))?;
                downloads = again;
            }
            Err(error) => return Err(describe(&error)),
        }
        for url in fresh {
            if let (Some(path), Some(download)) = (copy_of(url), downloads.get(url)) {
                keep(&path, download, &release.version).await;
            }
        }

        // Its setup runs what it put where the client's language has it.
        set_up(&installation, &launch, &placed, &mut progress).await?;
        // Only now: until then, the marker says the setup didn't finish.
        blocking(move || mods::finish_setup(&installation, &listed, &release)).await
    })
}

/// Where the mods' downloads are kept: in the launcher's cache, one folder
/// per mod.
pub fn cache() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_default()
        .join("old_launcher")
        .join("mods")
}

/// Where the download at `url` of `release` of `listed` is kept in `cache`:
/// in the mod's folder, named after the release's version and the file the
/// address names.
fn kept(cache: &Path, listed: &Mod, release: &Release, url: &str) -> PathBuf {
    let file = url
        .split(['?', '#'])
        .next()
        .and_then(|url| url.rsplit('/').next())
        .filter(|name| !name.is_empty())
        .unwrap_or("download");

    cache.join(plain(&listed.id)).join(format!(
        "{}-{}",
        plain(&release.version),
        plain(file).trim_start_matches('.')
    ))
}

/// `name` with plain characters alone, to name a file: names come from the
/// list of mods.
fn plain(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Keeps `download`, of the release of `version`, at `path`, in place of
/// the mod's downloads of other releases kept beside it: one release a mod
/// is enough. Written beside its place and renamed over it, so a copy cut
/// short never passes for whole. That it can't be kept changes nothing
/// else.
async fn keep(path: &Path, download: &[u8], version: &str) {
    let (Some(folder), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let partial = folder.join(format!("{}.partial", name.to_string_lossy()));
    let written = async {
        tokio::fs::create_dir_all(folder).await?;
        tokio::fs::write(&partial, download).await?;
        tokio::fs::rename(&partial, path).await
    };
    if let Err(error) = written.await {
        log::warn!("Could not keep {}: {error}", path.display());
        let _ = tokio::fs::remove_file(&partial).await;
        return;
    }

    let this = format!("{}-", plain(version));
    if let Ok(mut entries) = tokio::fs::read_dir(folder).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if !entry.file_name().to_string_lossy().starts_with(&this) {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }
}

/// Whether `error`, installing a download, says it isn't the release: a
/// copy kept of it is no good.
fn spoilt(error: &crate::core::Error) -> bool {
    use crate::core::Error;

    matches!(
        error,
        Error::UnknownArchive
            | Error::Zip(_)
            | Error::SevenZip(_)
            | Error::Rar(_)
            | Error::UnsafeEntry(_)
            | Error::Checksum(_)
            | Error::NotInRelease(_)
    )
}

/// Runs the setup of `release` in `installation`'s folder, the way `launch`
/// starts the game, saying which step it is at and what it last printed.
/// What it prints goes to the launcher's log, and so does what Wine says
/// meanwhile, at the debug level.
async fn set_up(
    installation: &Installation,
    launch: &Launch,
    release: &Release,
    progress: &mut Sender<Stage>,
) -> Result<(), String> {
    let steps = release.setup.len();

    for (index, step) in release.setup.iter().enumerate() {
        let number = index + 1;
        let arguments = step
            .arguments
            .iter()
            .map(|argument| {
                if !argument.contains("{locale}") {
                    return Ok(argument.clone());
                }
                let locale = installation
                    .locale()
                    .ok_or("the client has no language data in its Data folder")?;

                Ok(argument.replace("{locale}", locale.code()))
            })
            .collect::<Result<Vec<_>, String>>()?;

        let command = launch::tool(installation, launch, &step.run, &arguments)
            .map_err(|error| describe(&error))?;
        // The tool is a console program: no window pops up for it.
        #[cfg(windows)]
        let command = {
            use std::os::windows::process::CommandExt;
            let mut command = command;
            let _ = command.creation_flags(0x0800_0000);
            command
        };

        log::info!("Running {} {}", step.run, arguments.join(" "));
        progress
            .send(Stage::SettingUp {
                step: number,
                steps,
                said: None,
            })
            .await;

        let could_not = |error: std::io::Error| format!("could not run {}: {error}", step.run);
        let mut child = tokio::process::Command::from(command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(could_not)?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            return Err(format!("could not read what {} says", step.run));
        };

        // Where a tool says what went wrong, and Wine says a lot besides.
        let complaint = tokio::spawn({
            let run = step.run.clone();
            async move {
                let mut lines = BufReader::new(stderr).lines();
                let mut last = None;

                while let Ok(Some(line)) = lines.next_line().await {
                    if wine(&line) {
                        log::debug!("{run}: {line}");
                    } else if !line.trim().is_empty() {
                        log::info!("{run}: {line}");
                        last = Some(line.trim().to_owned());
                    }
                }

                last
            }
        });

        let mut lines = BufReader::new(stdout).lines();
        let mut last = None;
        while let Some(line) = lines.next_line().await.map_err(could_not)? {
            log::info!("{}: {line}", step.run);
            let said = line.split_whitespace().collect::<Vec<_>>().join(" ");

            if !said.is_empty() {
                progress
                    .send(Stage::SettingUp {
                        step: number,
                        steps,
                        said: Some(said.clone()),
                    })
                    .await;
                last = Some(said);
            }
        }

        let status = child.wait().await.map_err(could_not)?;
        let complaint = complaint.await.ok().flatten();
        if !status.success() {
            return Err(match complaint.or(last) {
                Some(line) => format!("its setup failed: {line}"),
                None => "its setup failed".to_owned(),
            });
        }
    }

    Ok(())
}

/// Whether `line` is one of Wine's own messages (`0034:fixme:keyboard:…`),
/// which says nothing of the program it runs.
fn wine(line: &str) -> bool {
    line.split_once(':').is_some_and(|(thread, rest)| {
        thread.len() == 4
            && thread.chars().all(|c| c.is_ascii_hexdigit())
            && ["fixme:", "err:", "warn:", "trace:"]
                .iter()
                .any(|class| rest.starts_with(class))
    })
}

/// Downloads `url` with `client`, or signed in to GitHub with `github`,
/// saying how much of it arrived.
async fn get(
    client: &reqwest::Client,
    github: Option<&github::Signed>,
    url: &str,
    progress: &mut Sender<Stage>,
) -> Result<Vec<u8>, String> {
    let response = match github {
        Some(signed) => signed.get(url).await?,
        None => client
            .get(url)
            .send()
            .await
            .map_err(|error| describe(&error))?,
    };

    if let Some(refusal) = github::refusal(&response, github.is_some()) {
        return Err(refusal.to_owned());
    }

    let response = response
        .error_for_status()
        .map_err(|error| describe(&error))?;
    let total = response.content_length();
    // What the server announces sets aside no more than this up front.
    let mut download = Vec::with_capacity(total.unwrap_or_default().min(64 << 20) as usize);
    let mut reported = 0;
    let mut chunks = response.bytes_stream();

    progress
        .send(Stage::Downloading { received: 0, total })
        .await;
    while let Some(chunk) = chunks.next().await {
        download.extend_from_slice(&chunk.map_err(|error| describe(&error))?);

        // Reporting every chunk would redraw the window thousands of times.
        if download.len() - reported >= 64 * 1024 {
            reported = download.len();
            progress
                .send(Stage::Downloading {
                    received: reported as u64,
                    total,
                })
                .await;
        }
    }

    Ok(download)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use iced::task::Sipper;
    use iced_test::simulator;

    use super::*;
    use crate::core::mods::{Source, Step};

    const ROOT: &str = "/games/wotlk";

    /// Glow, a mod of two releases, the newest first.
    fn glow() -> Mod {
        let release = |version: &str| Release {
            version: version.to_owned(),
            url: format!("https://example.com/Glow-{version}.zip"),
            updated: None,
            changes: None,
            files: vec![
                ("version.dll".to_owned(), format!("loader {version}")),
                ("Glow.dll".to_owned(), format!("renderer {version}")),
                ("Glow/setup.exe".to_owned(), "setup".to_owned()),
            ],
            from: BTreeMap::new(),
            inside: None,
            setup: vec![Step {
                run: "Glow/setup.exe".to_owned(),
                arguments: vec!["--locale".to_owned(), "{locale}".to_owned()],
                creates: "glow.bin".to_owned(),
            }],
            languages: BTreeMap::new(),
        };

        Mod {
            id: "glow".to_owned(),
            name: "Glow".to_owned(),
            description: "Light everywhere.".to_owned(),
            source: Source {
                name: "Glow on GitHub".to_owned(),
                page: "https://example.com/glow".to_owned(),
            },
            releases: vec![release("2.0"), release("1.0")],
            settings: vec!["Glow.ini".to_owned()],
            writes: Vec::new(),
            config: BTreeMap::new(),
            platforms: Vec::new(),
            requires: Vec::new(),
            excludes: Vec::new(),
            conflicts: Vec::new(),
            alternatives: Vec::new(),
            version_in: None,
        }
    }

    /// The section's state, with Glow listed, having found `found` of it in
    /// the client folder at [`ROOT`].
    fn found(found: Option<Status>) -> State {
        let mut state = State {
            list: Mods {
                version: Mods::VERSION,
                mods: vec![glow()],
            },
            ..State::default()
        };
        let _ = state.found.insert(
            ROOT.into(),
            found
                .map(|status| BTreeMap::from([("glow".to_owned(), status)]))
                .unwrap_or_default(),
        );
        let _ = state.unfit.insert(ROOT.into(), BTreeSet::new());

        state
    }

    fn status(release: Option<&str>, problems: Vec<Problem>) -> Status {
        Status {
            release: release.map(str::to_owned),
            problems,
        }
    }

    /// Whether `state`'s section shows `wanted`.
    fn shows(state: &State, playing: bool, wanted: &str) -> bool {
        let mut ui = simulator(state.view(Path::new(ROOT), playing));

        ui.find(wanted).is_ok()
    }

    /// Whether `state` has a banner, and it shows `wanted`.
    fn banner_shows(state: &State, playing: bool, wanted: &str) -> bool {
        state
            .banner(Path::new(ROOT), playing)
            .is_some_and(|banner| simulator(banner).find(wanted).is_ok())
    }

    /// What pressing `label` on `state`'s banner asks for.
    fn press(state: &State, playing: bool, label: &str) -> Vec<Message> {
        let mut ui = simulator(state.banner(Path::new(ROOT), playing).unwrap());
        let _ = ui.click(label).unwrap();

        ui.into_messages().collect()
    }

    /// Handles `message` for the client at [`ROOT`], which is
    /// `installation` once it opened.
    fn update(
        state: &mut State,
        message: Message,
        installation: Option<&Installation>,
    ) -> Action<Instruction, Message> {
        state.update(
            message,
            Some(installation.map_or(Path::new(ROOT), Installation::root)),
            installation,
            &Config::default(),
            false,
            Err("offline"),
            &mut Errors::default(),
        )
    }

    #[test]
    fn a_client_without_it_offers_the_newest_release_and_says_the_risk() {
        let state = found(None);

        for wanted in [
            "Mods",
            "Glow",
            "Glow on GitHub ↗",
            "Light everywhere.",
            "Not installed.",
            "Install 2.0",
            "Servers may forbid modified clients, and their anti-cheat may detect it.",
        ] {
            assert!(shows(&state, false, wanted), "{wanted}");
        }
        assert!(state.banner(Path::new(ROOT), false).is_none());
        assert!(!state.holds_back(Path::new(ROOT)));

        // Where it comes from opens its page; its name is just its name.
        let mut ui = simulator(state.view(Path::new(ROOT), false));
        let _ = ui.click("Glow on GitHub ↗").unwrap();
        assert!(matches!(
            &ui.into_messages().collect::<Vec<_>>()[..],
            [Message::OpenPage(page)] if page == "https://example.com/glow"
        ));
        let mut ui = simulator(state.view(Path::new(ROOT), false));
        let _ = ui.click("Glow").unwrap();
        assert_eq!(ui.into_messages().count(), 0);
    }

    #[test]
    fn mods_for_another_system_are_not_listed() {
        let elsewhere = if cfg!(windows) { "linux" } else { "windows" };
        let mut foreign = glow();
        foreign.id = "elsewhere".to_owned();
        foreign.name = "Elsewhere".to_owned();
        foreign.platforms = vec![elsewhere.to_owned()];
        let mut here = glow();
        here.platforms = vec![std::env::consts::OS.to_owned()];
        let mut state = State::default();

        let _ = state.list(
            Mods {
                version: Mods::VERSION,
                mods: vec![foreign, here],
            },
            None,
        );

        assert_eq!(
            state
                .list
                .mods
                .iter()
                .map(|listed| listed.id.as_str())
                .collect::<Vec<_>>(),
            ["glow"]
        );
    }

    #[test]
    fn a_mod_that_cant_be_beside_one_there_says_why_and_offers_no_install() {
        let mut glare = glow();
        glare.id = "glare".to_owned();
        glare.name = "Glare".to_owned();
        glare.settings.clear();
        glare.releases.truncate(1);
        glare.releases[0].version = "0.5".to_owned();
        glare.releases[0].setup.clear();
        glare.releases[0].files = vec![("Version.dll".to_owned(), "loader".to_owned())];
        let mut state = found(Some(status(Some("2.0"), Vec::new())));
        state.list.mods.push(glare);

        // The hint can't be hovered here: it is what blocks the button.
        assert_eq!(
            state.blocked(Path::new(ROOT), &state.list.mods[1]),
            Some(
                "“Glow” is installed, and it conflicts with this one: both use Version.dll."
                    .to_owned()
            )
        );
        assert!(shows(&state, false, "Install 0.5"));
        let mut ui = simulator(state.view(Path::new(ROOT), false));
        let _ = ui.click("Install 0.5").unwrap();
        assert_eq!(ui.into_messages().count(), 0, "the button is off");
        let _ = update(&mut state, Message::Install("glare".to_owned()), None);
        assert!(state.busy.is_none());
    }

    /// Glare, an edition of Glow: its alternative, for clients with the HD
    /// models' `patch-f.mpq`.
    fn glare() -> Mod {
        let mut glare = glow();
        glare.id = "glare".to_owned();
        glare.name = "Glare".to_owned();
        glare.settings.clear();
        glare.releases.truncate(1);
        glare.releases[0].version = "0.5".to_owned();
        glare.releases[0].setup.clear();
        glare.requires = vec!["Data/patch-f.mpq".to_owned()];
        glare.alternatives = vec!["glow".to_owned()];

        glare
    }

    #[test]
    fn a_mod_is_offered_only_to_the_clients_it_suits() {
        let mut state = found(None);
        state.list.mods.push(glare());
        let _ = state.found.remove(Path::new(ROOT));
        let _ = state.unfit.remove(Path::new(ROOT));
        assert!(
            !shows(&state, false, "Glare"),
            "it waits for the client to be looked at"
        );
        assert!(shows(&state, false, "Glow"), "it suits any");

        let mut looked = |unfit: &[&str], held: Option<Status>| {
            let _ = state.found.insert(
                ROOT.into(),
                held.map(|status| BTreeMap::from([("glare".to_owned(), status)]))
                    .unwrap_or_default(),
            );
            let _ = state.unfit.insert(
                ROOT.into(),
                unfit.iter().copied().map(str::to_owned).collect(),
            );
            shows(&state, false, "Glare")
        };
        assert!(!looked(&["glare"], None), "without the HD models");
        assert!(looked(&[], None), "with them");
        assert!(
            looked(&["glare"], Some(status(Some("0.5"), Vec::new()))),
            "held, it stays manageable"
        );
    }

    #[test]
    fn an_installed_alternative_keeps_install_disabled() {
        let mut state = found(Some(status(Some("2.0"), Vec::new())));
        state.list.mods.push(glare());

        assert!(shows(&state, false, "Install 0.5"));
        let mut ui = simulator(state.view(Path::new(ROOT), false));
        let _ = ui.click("Install 0.5").unwrap();
        assert_eq!(ui.into_messages().count(), 0, "the button is off");
        assert_eq!(
            state.blocked(Path::new(ROOT), &glare()),
            Some("“Glow” is installed, and it conflicts with this one.".to_owned())
        );
        assert_eq!(
            state.blocked(Path::new(ROOT), &glow()),
            None,
            "nothing of Glare there"
        );
        let _ = update(&mut state, Message::Install("glare".to_owned()), None);
        assert!(state.busy.is_none());
    }

    #[test]
    fn files_a_mod_isnt_made_for_are_named_and_installing_asks_first() {
        let mut state = found(None);
        let _ = state.excluded.insert(
            ROOT.into(),
            BTreeMap::from([("glow".to_owned(), vec!["Data/patch-f.mpq".to_owned()])]),
        );

        assert!(shows(&state, false, "In conflict with Data/patch-f.mpq."));
        let _ = update(&mut state, Message::Install("glow".to_owned()), None);
        assert!(state.busy.is_none(), "it asks first");
        for wanted in [
            "Install “Glow” anyway?",
            "Data/patch-f.mpq belongs to something else: installing “Glow” overrides it safely \
             (backed up).",
            "Install anyway",
        ] {
            assert!(shows(&state, false, wanted), "{wanted}");
        }
        assert!(!shows(&state, false, "belongs to something else"));

        let _ = update(&mut state, Message::CancelInstall, None);
        assert!(!shows(&state, false, "Install “Glow” anyway?"));
        assert!(shows(&state, false, "Install 2.0"), "still offered");
    }

    #[test]
    fn a_mod_not_in_a_language_of_the_client_goes_there_in_another_as_picked() {
        let mut state = found(None);
        let maps = &mut state.list.mods[0];
        maps.name = "Maps".to_owned();
        maps.releases.truncate(1);
        maps.releases[0].files.clear();
        maps.releases[0].setup.clear();
        maps.releases[0].url = "https://example.com/patch-{locale}-M.MPQ".to_owned();
        maps.releases[0].languages = [Locale::DeDe, Locale::EnUs]
            .into_iter()
            .map(|language| (language, Vec::new()))
            .collect();
        let maps = maps.clone();
        let _ = state.lacking.insert(
            ROOT.into(),
            BTreeMap::from([("glow".to_owned(), vec![Locale::EsMx])]),
        );

        for wanted in [
            "“Maps” doesn't come in esMX: it goes there in",
            "enUS",
            "instead, its text in that language.",
            "Install 2.0",
        ] {
            assert!(shows(&state, false, wanted), "{wanted}");
        }
        assert_eq!(state.instead(Path::new(ROOT), &maps), Some(Locale::EnUs));

        let _ = update(
            &mut state,
            Message::Instead("glow".to_owned(), Locale::DeDe),
            None,
        );
        assert_eq!(state.instead(Path::new(ROOT), &maps), Some(Locale::DeDe));
        assert!(shows(&state, false, "deDE"));

        // Installed, it goes on as its marker says.
        let _ = state.found.insert(
            ROOT.into(),
            BTreeMap::from([("glow".to_owned(), status(Some("2.0"), Vec::new()))]),
        );
        assert!(!shows(&state, false, "doesn't come in"));
    }

    #[test]
    fn after_a_language_came_each_mod_needing_it_is_repaired_once() {
        let mut state = found(None);
        let seen = || Seen {
            held: BTreeMap::from([(
                "glow".to_owned(),
                status(
                    Some("2.0"),
                    vec![Problem::Missing(vec!["Data/frFR/glow.mpq".to_owned()])],
                ),
            )]),
            ..Seen::default()
        };

        state.complete(Path::new(ROOT));
        let _ = update(&mut state, Message::Found(ROOT.into(), seen()), None);
        assert_eq!(
            state.completing.get(Path::new(ROOT)),
            Some(&BTreeSet::from(["glow".to_owned()])),
            "repaired"
        );

        // Looked at again after, the same: once is enough.
        let _ = update(&mut state, Message::Found(ROOT.into(), seen()), None);
        assert!(state.completing.is_empty());
    }

    #[test]
    fn while_one_mod_is_worked_on_the_others_wait() {
        let mut state = found(Some(status(
            Some("1.0"),
            vec![Problem::Changed(vec!["Glow.dll".to_owned()])],
        )));
        let mut other = glare();
        other.requires.clear();
        other.alternatives.clear();
        other.releases[0].files = vec![("d3d9.dll".to_owned(), "graphics".to_owned())];
        state.list.mods.push(other);
        state.busy = Some(Work {
            root: ROOT.into(),
            id: "elsewhere".to_owned(),
            said: "Installing Lumenveil 0.1.1".to_owned(),
            stage: None,
            steps: 0,
            looks: false,
        });

        let mut ui = simulator(state.view(Path::new(ROOT), false));
        for label in ["Install 0.5", "Update to 2.0", "Repair", "Uninstall"] {
            let _ = ui.click(label).unwrap();
        }
        assert_eq!(ui.into_messages().count(), 0, "nothing starts meanwhile");
        assert!(
            press(&state, false, "Repair").is_empty(),
            "the banner's waits too"
        );

        state.busy = None;
        assert!(matches!(
            &press(&state, false, "Repair")[..],
            [Message::Repair(id)] if id == "glow"
        ));
    }

    #[test]
    fn files_of_something_else_where_it_goes_are_set_aside_once_confirmed() {
        let foreign = |state: &mut State| {
            let _ = state.foreign.insert(
                ROOT.into(),
                BTreeMap::from([("glow".to_owned(), vec!["version.dll".to_owned()])]),
            );
        };

        let mut absent = found(None);
        foreign(&mut absent);
        assert!(shows(&absent, false, "In conflict with version.dll."));
        let mut ui = simulator(absent.view(Path::new(ROOT), false));
        let _ = ui.click("Install 2.0").unwrap();
        assert!(matches!(
            &ui.into_messages().collect::<Vec<_>>()[..],
            [Message::Install(id)] if id == "glow"
        ));
        let _ = update(&mut absent, Message::Install("glow".to_owned()), None);
        assert!(absent.busy.is_none(), "it asks first");
        assert!(shows(
            &absent,
            false,
            "version.dll belongs to something else: installing “Glow” overrides it safely \
             (backed up)."
        ));
        let mut ui = simulator(absent.view(Path::new(ROOT), false));
        let _ = ui.click("Install anyway").unwrap();
        assert!(matches!(
            &ui.into_messages().collect::<Vec<_>>()[..],
            [Message::ConfirmInstall]
        ));
        let _ = update(&mut absent, Message::ConfirmInstall, None);
        assert!(absent.confirming.is_none());

        let mut older = found(Some(status(Some("1.0"), Vec::new())));
        foreign(&mut older);
        assert!(shows(&older, false, "Update to 2.0"));
        let _ = update(&mut older, Message::Install("glow".to_owned()), None);
        assert!(shows(&older, false, "Update “Glow” anyway?"));
        assert!(shows(&older, false, "Update anyway"));
        assert!(
            shows(&older, false, "Uninstall"),
            "what is there stays manageable"
        );
        assert!(shows(
            &older,
            false,
            "version.dll belongs to something else: updating “Glow” overrides it safely \
                 (backed up)."
        ));

        // A copy installed by hand is installed over, as its button says.
        let mut by_hand = found(Some(status(
            None,
            vec![Problem::Unknown {
                version: Some("0.9".to_owned()),
                natives: Vec::new(),
            }],
        )));
        foreign(&mut by_hand);
        assert!(shows(&by_hand, false, "Install 2.0"));
        let _ = update(&mut by_hand, Message::Install("glow".to_owned()), None);
        assert!(shows(&by_hand, false, "Install “Glow” anyway?"));
        assert!(shows(
            &by_hand,
            false,
            "“Glow” is installed manually: installing 2.0 overrides it safely (backed up)."
        ));
        assert!(shows(
            &by_hand,
            false,
            "version.dll belongs to something else: installing “Glow” overrides it safely (backed up)."
        ));
        assert!(!by_hand.updating(Path::new(ROOT), "glow"));
        assert!(older.updating(Path::new(ROOT), "glow"));

        // Up to date: nothing to install, nothing in conflict.
        let mut current = found(Some(status(Some("2.0"), Vec::new())));
        foreign(&mut current);
        assert!(!shows(&current, false, "In conflict with"));
    }

    #[test]
    fn what_keeps_it_from_being_as_released_is_said_and_repaired() {
        let state = found(Some(status(
            Some("1.0"),
            vec![
                Problem::Changed(vec!["Glow.dll".to_owned()]),
                Problem::NotSetUp(vec!["glow.bin".to_owned()]),
                Problem::Config(vec![(
                    "gxApi".to_owned(),
                    "d3d9ex".to_owned(),
                    "d3d9".to_owned(),
                )]),
            ],
        )));

        for wanted in [
            "Version 1.0 installed; 2.0 is out.",
            "Update to 2.0",
            "Repair",
            "Uninstall",
            "Not as released: Glow.dll.",
            "Its setup needs to run to make glow.bin.",
            "Config.wtf sets gxApi to d3d9ex; it needs d3d9.",
        ] {
            assert!(shows(&state, false, wanted), "{wanted}");
        }

        assert!(banner_shows(
            &state,
            false,
            "The launcher needs to repair “Glow” before starting the game. Not as released: \
             Glow.dll. Its setup needs to run to make glow.bin. Config.wtf sets gxApi to d3d9ex; \
             it needs d3d9."
        ));
        assert!(!banner_shows(&state, false, "Not now"));
        assert!(state.holds_back(Path::new(ROOT)));
        assert!(state.natives(Path::new(ROOT)).is_empty(), "not as released");
        assert!(matches!(
            &press(&state, false, "Repair")[..],
            [Message::Repair(id)] if id == "glow"
        ));
        assert!(press(&state, true, "Repair").is_empty(), "the game runs");
        assert_eq!(
            target(&glow(), &status(Some("1.0"), Vec::new()))
                .unwrap()
                .version,
            "1.0",
            "repairing installs the release there again"
        );
    }

    #[test]
    fn a_client_is_looked_at_once_at_a_time_and_the_game_waits_for_it() {
        // Glow's files, of a version the list doesn't know.
        let (_root, installation) = crate::core::testing::client_with(&[
            ("version.dll", "loader 1.0"),
            ("Glow.dll", "renderer 1.0"),
            ("Glow.log", "ran"),
        ]);
        let root = installation.root().to_path_buf();
        let mut state = found(None);
        assert!(state.holds_back(&root), "not looked at yet");
        assert!(state.natives(&root).is_empty());

        // One look at a time: asked again meanwhile, it looks again after.
        assert!(state.find(&root).units() > 0);
        assert!(state.find(&root).units() == 0);
        assert!(state.shown(&root).units() == 0);
        assert_eq!(state.finding.get(&root), Some(&true));

        let seen = look(&root, &state.list);
        assert_eq!(
            seen.held.get("glow"),
            Some(&status(
                None,
                vec![Problem::Unknown {
                    version: None,
                    natives: vec!["Glow".to_owned(), "version".to_owned()],
                }]
            )),
            "installed by hand: its files match no release"
        );
        let again = update(
            &mut state,
            Message::Found(root.clone(), seen.clone()),
            Some(&installation),
        );
        assert!(again.task.units() > 0, "looked at again, as asked");
        assert_eq!(state.finding.get(&root), Some(&false));
        let _ = update(
            &mut state,
            Message::Found(root.clone(), seen),
            Some(&installation),
        );
        assert!(state.finding.is_empty());

        // The player's own copy: the game starts, with its DLLs loaded.
        assert!(!state.holds_back(&root));
        assert_eq!(state.natives(&root), ["Glow", "version"]);
        assert!(state.banner(&root, false).is_none());
        let mut ui = simulator(state.view(&root, false));
        assert!(ui.find("Installed manually, unrecognised version").is_ok());
        assert!(ui.find("Install 2.0").is_ok(), "the newest, in its place");
        assert!(ui.find("Repair").is_err());
        drop(ui);

        // Its log alone is no copy of it.
        std::fs::remove_file(root.join("version.dll")).unwrap();
        std::fs::remove_file(root.join("Glow.dll")).unwrap();
        assert!(look(&root, &state.list).held.is_empty());

        // Not looked at while something is done to a mod there: that looks
        // once done.
        state.busy = Some(Work {
            root: root.clone(),
            id: "glow".to_owned(),
            said: "Installing Glow".to_owned(),
            stage: None,
            steps: 0,
            looks: false,
        });
        assert!(state.find(&root).units() == 0);
        assert!(state.finding.is_empty());
    }

    #[test]
    fn while_the_patches_are_looked_at_the_row_says_so_and_its_buttons_are_off() {
        let mut state = found(Some(status(Some("1.0"), Vec::new())));
        let root = Path::new(ROOT);
        assert!(shows(&state, false, "Version 1.0 installed; 2.0 is out."));

        state.waiting_for_patches(BTreeSet::from([root.to_path_buf()]));
        assert!(shows(&state, false, "Looking…"));
        assert!(!shows(&state, false, "Version 1.0 installed; 2.0 is out."));
        let mut ui = simulator(state.view(root, false));
        let _ = ui.click("Update to 2.0").unwrap();
        let _ = ui.click("Uninstall").unwrap();
        assert_eq!(ui.into_messages().count(), 0, "off");
        let _ = update(&mut state, Message::Install("glow".to_owned()), None);
        assert!(state.confirming.is_none() && state.busy.is_none());

        // The tip says why.
        let mut ui = simulator(state.view(root, false));
        let at = ui.find("Update to 2.0").unwrap().bounds().center();
        ui.point_at(at);
        let _ = ui.simulate([iced::Event::Mouse(iced::mouse::Event::CursorMoved {
            position: at,
        })]);
        assert!(ui.find(crate::patches::LOOKING).is_ok());
        drop(ui);

        // Another client's patches are no concern of this one.
        state.waiting_for_patches(BTreeSet::from(["/games/other".into()]));
        assert!(shows(&state, false, "Version 1.0 installed; 2.0 is out."));
        let mut ui = simulator(state.view(root, false));
        let _ = ui.click("Uninstall").unwrap();
        assert!(ui.into_messages().count() > 0);
    }

    #[test]
    fn after_work_is_done_the_row_waits_for_the_next_look() {
        let mut state = found(Some(status(Some("1.0"), Vec::new())));
        state.list.mods.push(glare());
        let root = Path::new(ROOT);
        assert!(shows(&state, false, "Update to 2.0"));
        assert!(shows(&state, false, "Glare"), "it suits the client");
        assert!(!state.holds_back(root));

        let _ = update(
            &mut state,
            Message::Done(ROOT.into(), "glow".to_owned(), Ok("Done.".to_owned())),
            None,
        );
        assert!(!state.found.contains_key(root), "what was known is stale");
        assert!(state.holds_back(root));
        assert!(shows(&state, false, "Looking…"));
        assert!(shows(&state, false, "Install 2.0"));
        assert!(shows(&state, false, "Glare"), "it stays in the list");
        let _ = update(&mut state, Message::Install("glow".to_owned()), None);
        assert!(state.busy.is_none(), "nothing starts before the look");
        let mut ui = simulator(state.view(root, false));
        let _ = ui.click("Install 2.0").unwrap();
        assert_eq!(ui.into_messages().count(), 0, "off until it is looked at");

        let seen = Seen {
            held: BTreeMap::from([("glow".to_owned(), status(Some("2.0"), Vec::new()))]),
            ..Seen::default()
        };
        let _ = update(&mut state, Message::Found(ROOT.into(), seen), None);
        assert!(!state.holds_back(root));
        let mut ui = simulator(state.view(root, false));
        let _ = ui.click("Uninstall").unwrap();
        assert!(matches!(
            &ui.into_messages().collect::<Vec<_>>()[..],
            [Message::Remove(id)] if id == "glow"
        ));

        // Looked at again meanwhile, the buttons wait too.
        let _ = state.finding.insert(root.to_path_buf(), false);
        let mut ui = simulator(state.view(root, false));
        let _ = ui.click("Uninstall").unwrap();
        assert_eq!(ui.into_messages().count(), 0);
    }

    #[test]
    fn a_copy_installed_by_hand_says_its_version() {
        let state = found(Some(status(
            None,
            vec![Problem::Unknown {
                version: Some("3.0".to_owned()),
                natives: Vec::new(),
            }],
        )));

        assert!(shows(&state, false, "Version 3.0 installed manually."));
        assert!(!state.holds_back(Path::new(ROOT)));

        // A release of the list, its files not as released.
        let state = found(Some(status(
            None,
            vec![Problem::Unknown {
                version: Some("2.0".to_owned()),
                natives: Vec::new(),
            }],
        )));
        assert!(shows(
            &state,
            false,
            "Version 2.0 installed manually; its files differ from the release."
        ));
        assert_eq!(
            target(&glow(), &status(None, Vec::new())).unwrap().version,
            "2.0",
            "the newest goes in its place"
        );
    }

    #[test]
    fn installing_over_a_copy_installed_by_hand_asks_first() {
        let mut state = found(Some(status(
            None,
            vec![Problem::Unknown {
                version: None,
                natives: Vec::new(),
            }],
        )));
        assert!(shows(
            &state,
            false,
            "Installed manually, unrecognised version"
        ));

        let mut ui = simulator(state.view(Path::new(ROOT), false));
        let _ = ui.click("Install 2.0").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        assert!(matches!(&messages[..], [Message::Install(id)] if id == "glow"));
        let _ = update(&mut state, Message::Install("glow".to_owned()), None);
        assert!(state.busy.is_none(), "nothing is downloaded yet");
        assert!(shows(
            &state,
            false,
            "“Glow” is installed manually: installing 2.0 overrides it safely (backed up)."
        ));
        assert!(shows(&state, false, "Install “Glow” anyway?"));

        let mut ui = simulator(state.view(Path::new(ROOT), false));
        let _ = ui.click("Install anyway").unwrap();
        assert!(matches!(
            &ui.into_messages().collect::<Vec<_>>()[..],
            [Message::ConfirmInstall]
        ));
    }

    #[test]
    fn files_that_couldnt_be_checked_are_left_as_they_are() {
        let state = found(Some(status(None, vec![Problem::Unchecked])));

        assert!(shows(
            &state,
            false,
            "Its files couldn't be checked against its releases."
        ));
        assert!(state.banner(Path::new(ROOT), false).is_none());
        assert!(!state.holds_back(Path::new(ROOT)));
    }

    #[test]
    fn nothing_is_changed_while_the_game_runs() {
        let mut state = found(None);
        let (_root, installation) = crate::core::testing::client(&[]);
        let _ = state
            .found
            .insert(installation.root().into(), BTreeMap::new());

        let mut ui = simulator(state.view(installation.root(), true));
        let _ = ui.click("Install 2.0").unwrap();
        assert!(ui.into_messages().next().is_none());

        let _ = state.update(
            Message::Install("glow".to_owned()),
            Some(installation.root()),
            Some(&installation),
            &Config::default(),
            true,
            Err("offline"),
            &mut Errors::default(),
        );
        assert!(state.busy.is_none());
    }

    #[test]
    fn removing_asks_first_and_deletes_the_settings_only_if_ticked() {
        let (_root, installation) = crate::core::testing::client(&["Glow.dll"]);
        let root = installation.root().to_path_buf();
        let mut state = found(None);
        let _ = state.found.insert(
            root.clone(),
            BTreeMap::from([("glow".to_owned(), status(Some("2.0"), Vec::new()))]),
        );

        let messages: Vec<Message> = {
            let mut ui = simulator(state.view(&root, false));
            let _ = ui.click("Uninstall").unwrap();
            ui.into_messages().collect()
        };
        assert!(matches!(&messages[..], [Message::Remove(id)] if id == "glow"));
        let _ = update(
            &mut state,
            Message::Remove("glow".to_owned()),
            Some(&installation),
        );

        let messages: Vec<Message> = {
            let mut ui = simulator(state.view(&root, false));
            assert!(ui.find("Remove “Glow” from this client?").is_ok());
            let _ = ui.click("Delete its settings too").unwrap();
            ui.into_messages().collect()
        };
        assert!(matches!(messages[..], [Message::DeleteSettings(true)]));
        let _ = update(
            &mut state,
            Message::DeleteSettings(true),
            Some(&installation),
        );

        // The question is this client's: another holding the mod asks
        // nothing.
        let other = Path::new("/games/other");
        let _ = state.found.insert(
            other.to_path_buf(),
            BTreeMap::from([("glow".to_owned(), status(Some("2.0"), Vec::new()))]),
        );
        let mut ui = simulator(state.view(other, false));
        assert!(ui.find("Remove “Glow” from this client?").is_err());
        drop(ui);

        let action = update(&mut state, Message::ConfirmRemove, Some(&installation));
        assert!(state.busy.is_some(), "removing");
        assert!(action.instruction.is_none());
        assert!(state.holds_back(&root), "the game waits for it");
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn the_setup_runs_the_way_the_game_starts_and_says_how_far_it_got() {
        let (_root, installation) =
            crate::core::testing::client(&["Data/esES/locale-esES.MPQ", "Glow/setup.exe"]);
        let release = glow().releases.remove(0);

        let echoing = Launch {
            env: "WHO=tool".to_owned(),
            command: "echo $WHO %command%; echo '  map    0 Azeroth  '; \
                      echo 0034:fixme:keyboard:klid >&2"
                .to_owned(),
        };
        let (mut progress, stages) = Sender::channel(16);
        set_up(&installation, &echoing, &release, &mut progress)
            .await
            .unwrap();
        drop(progress);
        let said: Vec<Option<String>> = stages
            .map(|stage| match stage {
                Stage::SettingUp {
                    step: 1,
                    steps: 1,
                    said,
                } => said,
                stage => panic!("{stage:?}"),
            })
            .collect()
            .await;
        assert_eq!(said.len(), 3, "{said:?}");
        assert_eq!(said[0], None, "the step starts");
        assert!(
            said[1].as_ref().is_some_and(|said| {
                said.starts_with("tool ") && said.ends_with("Glow\\setup.exe --locale esES")
            }),
            "{said:?}"
        );
        assert_eq!(
            said[2].as_deref(),
            Some("map 0 Azeroth"),
            "what Wine says isn't the tool's"
        );

        let failing = Launch {
            env: String::new(),
            command: "echo 0034:fixme:keyboard:klid >&2; echo out of memory >&2; echo working; \
                      false %command%"
                .to_owned(),
        };
        let (mut progress, _) = Sender::channel(16);
        assert_eq!(
            set_up(&installation, &failing, &release, &mut progress).await,
            Err("its setup failed: out of memory".to_owned())
        );
    }

    /// Serves `body` at any address on this computer, counting the
    /// requests.
    async fn serve(body: Vec<u8>) -> (String, Arc<AtomicUsize>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(AtomicUsize::new(0));

        // The server lives as long as the test's runtime.
        drop(tokio::spawn({
            let requests = requests.clone();
            async move {
                loop {
                    let (mut stream, _) = listener.accept().await.unwrap();
                    let mut request = vec![0; 4096];
                    let _ = stream.read(&mut request).await.unwrap();
                    let _ = requests.fetch_add(1, Ordering::SeqCst);
                    let mut response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    )
                    .into_bytes();
                    response.extend(&body);
                    stream.write_all(&response).await.unwrap();
                }
            }
        }));

        (format!("http://{address}"), requests)
    }

    #[tokio::test]
    async fn a_release_downloaded_once_installs_again_from_the_copy_kept() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = crate::http_client().build().unwrap();
        let patch = b"MPQ\x1a".to_vec();
        let (server, requests) = serve(patch.clone()).await;
        let mut listed = glow();
        listed.id = "patch-4".to_owned();
        listed.settings.clear();
        listed.releases.truncate(1);
        listed.releases[0] = Release {
            version: "1".to_owned(),
            url: format!("{server}/files/patch-4.MPQ?raw=1"),
            files: vec![("Data/patch-4.MPQ".to_owned(), mods::sha256(&patch))],
            setup: Vec::new(),
            ..listed.releases[0].clone()
        };
        let release = listed.releases[0].clone();
        let (_root, installation) = crate::core::testing::client(&["Data/enUS/locale-enUS.MPQ"]);
        let cache = tempfile::tempdir().unwrap();
        let copy = kept(cache.path(), &listed, &release, &release.url);
        assert_eq!(copy, cache.path().join("patch-4").join("1-patch-4.MPQ"));
        // Another release kept before.
        fs::create_dir_all(copy.parent().unwrap()).unwrap();
        fs::write(copy.with_file_name("0-patch-4.MPQ"), b"old").unwrap();

        let installed = || {
            let (progress, _) = Sender::channel(64);
            place(
                client.clone(),
                None,
                installation.clone(),
                Launch::default(),
                listed.clone(),
                release.clone(),
                Vec::new(),
                false,
                None,
                Some(cache.path().to_path_buf()),
                BTreeMap::new(),
            )
            .run(progress)
        };

        installed().await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(fs::read(&copy).unwrap(), patch, "kept");
        assert_eq!(
            fs::read_dir(copy.parent().unwrap()).unwrap().count(),
            1,
            "in place of the other"
        );

        // Installed again: no download.
        fs::remove_file(installation.root().join("Data/patch-4.MPQ")).unwrap();
        installed().await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert_eq!(
            fs::read(installation.root().join("Data/patch-4.MPQ")).unwrap(),
            patch
        );

        // A copy gone bad is downloaded again.
        fs::write(&copy, b"MPQ\x00").unwrap();
        installed().await.unwrap();
        assert_eq!(requests.load(Ordering::SeqCst), 2);
        assert_eq!(fs::read(&copy).unwrap(), patch);
    }

    #[tokio::test]
    async fn a_release_of_a_file_per_language_downloads_those_of_the_client() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = crate::http_client().build().unwrap();
        let patch = b"MPQ\x1a maps".to_vec();
        let (server, requests) = serve(patch.clone()).await;
        let path = "Data/{locale}/patch-{locale}-M.MPQ".to_owned();
        let mut listed = glow();
        listed.id = "maps".to_owned();
        listed.settings.clear();
        listed.releases.truncate(1);
        listed.releases[0] = Release {
            version: "2.4.5".to_owned(),
            url: format!("{server}/download/patch-{{locale}}-M.MPQ"),
            files: Vec::new(),
            setup: Vec::new(),
            languages: [Locale::DeDe, Locale::EnUs, Locale::EsEs]
                .into_iter()
                .map(|language| (language, vec![(path.clone(), mods::sha256(&patch))]))
                .collect(),
            ..listed.releases[0].clone()
        };
        let release = listed.releases[0].clone();
        let (_root, installation) = crate::core::testing::client(&[
            "Data/enUS/locale-enUS.MPQ",
            "Data/esES/locale-esES.MPQ",
        ]);
        let root = installation.root();
        let cache = tempfile::tempdir().unwrap();

        let (progress, _) = Sender::channel(64);
        place(
            client.clone(),
            None,
            installation.clone(),
            Launch::default(),
            listed.clone(),
            release.clone(),
            Vec::new(),
            false,
            None,
            Some(cache.path().to_path_buf()),
            BTreeMap::new(),
        )
        .run(progress)
        .await
        .unwrap();

        assert_eq!(requests.load(Ordering::SeqCst), 2, "its languages alone");
        for language in ["enUS", "esES"] {
            assert_eq!(
                fs::read(root.join(format!("Data/{language}/patch-{language}-M.MPQ"))).unwrap(),
                patch
            );
            assert!(
                cache
                    .path()
                    .join(format!("maps/2.4.5-patch-{language}-M.MPQ"))
                    .is_file(),
                "each kept"
            );
        }
        assert!(!root.join("Data/deDE").exists());
        assert_eq!(
            mods::status(root, &listed, &[]).unwrap().problems,
            [],
            "as released"
        );

        // In a language it doesn't come in: another's, as picked, which its
        // repairs keep to.
        let (_root, mexican) = crate::core::testing::client(&["Data/esMX/locale-esMX.MPQ"]);
        let (progress, _) = Sender::channel(64);
        place(
            client,
            None,
            mexican.clone(),
            Launch::default(),
            listed.clone(),
            release,
            Vec::new(),
            false,
            Some(Locale::EnUs),
            Some(cache.path().to_path_buf()),
            BTreeMap::new(),
        )
        .run(progress)
        .await
        .unwrap();

        assert_eq!(requests.load(Ordering::SeqCst), 2, "enUS's, as kept");
        assert_eq!(
            fs::read(mexican.root().join("Data/esMX/patch-esMX-M.MPQ")).unwrap(),
            patch
        );
        assert_eq!(
            listed
                .marker(mexican.root())
                .and_then(|marker| marker.instead),
            Some(Locale::EnUs)
        );
        assert_eq!(
            mods::status(mexican.root(), &listed, &[]).unwrap().problems,
            []
        );
    }

    #[test]
    fn an_install_says_which_of_its_steps_it_is_at() {
        let mut state = found(None);
        state.busy = Some(Work {
            root: ROOT.into(),
            id: "glow".to_owned(),
            said: "Installing Glow 2.0".to_owned(),
            stage: Some(Stage::Downloading {
                received: 524_288,
                total: Some(1_048_576),
            }),
            steps: 3,
            looks: false,
        });
        assert!(banner_shows(&state, false, "Installing Glow 2.0…"));
        assert!(banner_shows(
            &state,
            false,
            "Step 1 of 3: downloading, 512 KB of 1.0 MB"
        ));
        assert!(state.holds_back(Path::new(ROOT)));

        let _ = update(
            &mut state,
            Message::Progressed(
                ROOT.into(),
                "glow".to_owned(),
                Stage::SettingUp {
                    step: 1,
                    steps: 1,
                    said: Some("map 0 Azeroth".to_owned()),
                },
            ),
            None,
        );
        for wanted in ["Step 3 of 3: running its setup", "map 0 Azeroth"] {
            assert!(banner_shows(&state, false, wanted), "{wanted}");
            assert!(shows(&state, false, wanted), "{wanted}");
        }
    }

    #[test]
    fn only_a_download_of_known_size_says_how_far_its_step_got() {
        let downloading = |total| Stage::Downloading {
            received: 524_288,
            total,
        };
        assert_eq!(downloading(Some(1_048_576)).fraction(), Some(0.5));
        assert_eq!(downloading(None).fraction(), None);
        assert_eq!(downloading(Some(0)).fraction(), None);
        assert_eq!(Stage::Installing.fraction(), None);
        assert_eq!(
            Stage::SettingUp {
                step: 2,
                steps: 2,
                said: None,
            }
            .fraction(),
            None,
            "the setup glides, the last step included"
        );
    }

    // The gate: a client with archives made by the test, and Patch Y, whose
    // one archive is served from this computer.

    use crate::core::mpq::writer::{Options, Writer};

    /// An archive holding `files` (name, bytes), with a listfile naming them.
    fn archive(files: &[(&str, Vec<u8>)]) -> Writer {
        files
            .iter()
            .fold(Writer::new(), |writer, (name, bytes)| {
                writer.file(name, bytes)
            })
            .with_listfile(Options::default())
    }

    /// A table the launcher reads nothing of.
    fn table() -> Vec<u8> {
        b"WDBC".to_vec()
    }

    /// A model of version 264 with `skins` skins, no animations and no
    /// textures.
    fn model(skins: u32) -> Vec<u8> {
        let mut bytes = vec![0; 0x58];

        bytes[..4].copy_from_slice(b"MD20");
        bytes[4..8].copy_from_slice(&264u32.to_le_bytes());
        bytes[0x44..0x48].copy_from_slice(&skins.to_le_bytes());
        bytes[0x20..0x24].copy_from_slice(&0x58u32.to_le_bytes());
        bytes[0x54..0x58].copy_from_slice(&0x58u32.to_le_bytes());

        bytes
    }

    /// Every file under `root`, by path, with its contents.
    fn tree(root: &Path) -> BTreeMap<String, Vec<u8>> {
        fn walk(root: &Path, folder: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
            for entry in fs::read_dir(folder).unwrap() {
                let path = entry.unwrap().path();

                if path.is_dir() {
                    walk(root, &path, files);
                } else {
                    let name = path.strip_prefix(root).unwrap().display().to_string();
                    let _ = files.insert(name, fs::read(&path).unwrap());
                }
            }
        }

        let mut files = BTreeMap::new();
        walk(root, root, &mut files);

        files
    }

    /// A client in a temporary folder, a Blizzard archive of its language
    /// and `archives` (path, archive); the state of the mods there, with
    /// Patch Y listed, whose archive is `brings`, and the launcher's
    /// caches in temporary folders too.
    struct Gated {
        _root: tempfile::TempDir,
        installation: Installation,
        state: State,
        client: reqwest::Client,
        requests: Arc<AtomicUsize>,
        downloads: tempfile::TempDir,
        facts: tempfile::TempDir,
        brings: Vec<u8>,
    }

    impl Gated {
        async fn new(archives: &[(&str, &Writer)], brings: &Writer, excludes: &[&str]) -> Self {
            let _ = rustls::crypto::ring::default_provider().install_default();
            let (root, _) = crate::core::testing::client(&[]);
            let player = archive(&[("x.txt", b"x".to_vec())]);

            for (path, archive) in archives
                .iter()
                .chain(&[("Data/enUS/locale-enUS.MPQ", &player)])
            {
                let target = root.path().join(path);
                fs::create_dir_all(target.parent().unwrap()).unwrap();
                archive.write(&target);
            }
            let installation = Installation::open(root.path()).unwrap();
            assert_eq!(installation.locale(), Some(Locale::EnUs));

            let brings = brings.build();
            let (server, requests) = serve(brings.clone()).await;
            let mut listed = glow();
            listed.id = "patch-y".to_owned();
            listed.name = "Patch Y".to_owned();
            listed.settings.clear();
            listed.excludes = excludes.iter().map(|path| (*path).to_owned()).collect();
            listed.releases.truncate(1);
            listed.releases[0] = Release {
                version: "3.1.0".to_owned(),
                url: format!("{server}/Patch-Y.mpq"),
                files: vec![("Data/patch-y.mpq".to_owned(), mods::sha256(&brings))],
                setup: Vec::new(),
                ..listed.releases[0].clone()
            };

            let mut state = State {
                list: Mods {
                    version: Mods::VERSION,
                    mods: vec![listed, glow()],
                },
                ..State::default()
            };
            let downloads = tempfile::tempdir().unwrap();
            let facts = tempfile::tempdir().unwrap();
            state.keep_downloads_in(downloads.path().to_path_buf());
            state.keep_facts_in(Cache::at(facts.path()));

            let mut gated = Self {
                _root: root,
                installation,
                state,
                client: crate::http_client().build().unwrap(),
                requests,
                downloads,
                facts,
                brings,
            };
            gated.looked();

            gated
        }

        fn root(&self) -> PathBuf {
            self.installation.root().to_path_buf()
        }

        fn listed(&self) -> Mod {
            self.state.listed("patch-y").unwrap().clone()
        }

        /// The client's mods are looked at, and its patches too (the
        /// launcher does it in the background, in this order or the
        /// other).
        fn looked(&mut self) {
            let root = self.root();
            let seen = look(&root, &self.state.list);
            let _ = self.state.unfit.insert(root.clone(), seen.unfit.clone());
            let _ = self
                .state
                .excluded
                .insert(root.clone(), seen.excluded.clone());
            let _ = self.state.found.insert(root.clone(), seen.held);
            let _ = self.state.foreign.insert(root.clone(), seen.foreign);
            let _ = self.state.gates.remove(&root);
            let _ = self.state.scans.remove(&root);

            let scan = crate::patches::look(
                &root,
                Locale::EnUs,
                &Cache::at(self.facts.path().join("client")),
                &|_| {},
            )
            .unwrap();
            let _ = self
                .state
                .patched(&root, Some(Arc::new(scan)), Locale::EnUs);
        }

        /// What the patches say of the mods is worked out, as the
        /// launcher does in the background.
        fn judged(&mut self) {
            let root = self.root();
            let (scan, locale) = self.state.scans[&root].clone();
            let _ = self.state.judge(&root);
            let ticket = self.state.judged[&root];
            let verdicts = gate::judge(
                &root,
                &self.state.list,
                &self.state.found[&root],
                &scan,
                locale,
                self.state.facts.as_ref().unwrap(),
            );

            let _ = self.go(Message::Judged(root, ticket, verdicts));
        }

        /// What pressing something asks of the state, for the client.
        fn go(&mut self, message: Message) -> Action<Instruction, Message> {
            self.state.update(
                message,
                Some(self.installation.root()),
                Some(&self.installation),
                &Config::default(),
                false,
                Ok(&self.client),
                &mut Errors::default(),
            )
        }

        fn shows(&self, wanted: &str) -> bool {
            let root = self.root();
            let mut ui = simulator(self.state.view(&root, false));

            ui.find(wanted).is_ok()
        }

        fn banner_shows(&self, wanted: &str) -> bool {
            let root = self.root();

            self.state
                .banner(&root, false)
                .is_some_and(|banner| simulator(banner).find(wanted).is_ok())
        }

        /// What the install in [`State::start`] does first, run here: the
        /// download and the look at its archives, the stages it went
        /// through.
        async fn fetched(&self) -> (Fetched, Vec<Stage>) {
            let (progress, stages) = Sender::channel(64);
            let scan = self.state.scans[&self.root()].0.clone();
            let fetched = fetch(
                self.client.clone(),
                None,
                self.installation.clone(),
                self.listed(),
                self.listed().releases[0].clone(),
                self.state.list.clone(),
                None,
                Some(self.downloads.path().to_path_buf()),
                self.state.facts.clone().unwrap(),
                scan,
            )
            .run(progress)
            .await
            .unwrap();

            (fetched, stages.collect().await)
        }

        /// What it does after, run here too.
        async fn placed(&self, over: bool, carried: BTreeMap<String, Vec<u8>>) {
            let (progress, _) = Sender::channel(64);
            let listed = self.listed();

            place(
                self.client.clone(),
                None,
                self.installation.clone(),
                Launch::default(),
                listed.clone(),
                listed.releases[0].clone(),
                Vec::new(),
                over,
                None,
                Some(self.downloads.path().to_path_buf()),
                carried,
            )
            .run(progress)
            .await
            .unwrap();
        }

        /// The archive of Patch Y is kept in the launcher's downloads.
        fn kept(&self) -> bool {
            let listed = self.listed();

            kept(
                self.downloads.path(),
                &listed,
                &listed.releases[0],
                &listed.releases[0].url,
            )
            .is_file()
        }

        fn requests(&self) -> usize {
            self.requests.load(Ordering::SeqCst)
        }
    }

    fn install(gated: &mut Gated) {
        let _ = gated.go(Message::Install("patch-y".to_owned()));
    }

    /// The tables of the players' patches and Patch Y's, that it replaces.
    fn tables(names: &[&str]) -> Vec<(String, Vec<u8>)> {
        names
            .iter()
            .map(|name| (format!("DBFilesClient\\{name}.dbc"), table()))
            .collect()
    }

    fn archive_of(files: &[(String, Vec<u8>)]) -> Writer {
        let files: Vec<(&str, Vec<u8>)> = files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.clone()))
            .collect();

        archive(&files)
    }

    #[tokio::test]
    async fn a_first_install_downloads_looks_at_its_archives_and_then_asks() {
        let g = archive_of(&tables(&["CreatureDisplayInfo"]));
        let brings = archive_of(&tables(&["CreatureDisplayInfo", "Spell"]));
        let mut gated = Gated::new(&[("Data/patch-G.mpq", &g)], &brings, &[]).await;

        // Nothing is known of its archives yet: it downloads, and asks
        // nothing before.
        assert!(!gated.shows("Installing “Patch Y” would override the tables your custom Data/patch-G.mpq replaces: CreatureDisplayInfo.dbc."));
        install(&mut gated);
        assert!(gated.state.confirming.is_none());
        assert!(gated.state.pending.is_some());
        assert_eq!(gated.requests(), 0);
        assert!(gated.banner_shows("Step 1 of 3: downloading, 0 KB"));
        let _ = gated.go(Message::Progressed(
            gated.root(),
            "patch-y".to_owned(),
            Stage::Looking,
        ));
        assert!(gated.banner_shows("Step 2 of 3: looking at its archives"));

        let (fetched, stages) = gated.fetched().await;
        assert_eq!(gated.requests(), 1);
        assert!(stages.contains(&Stage::Looking), "{stages:?}");
        assert!(
            gated.kept(),
            "kept as soon as it is known to be the release"
        );
        let _ = gated.go(Message::Fetched(
            gated.root(),
            "patch-y".to_owned(),
            Ok(fetched),
        ));

        // Nothing is placed until the player says yes.
        assert!(gated.state.busy.is_none());
        assert!(!gated.root().join("Data/patch-y.mpq").exists());
        assert!(gated.shows("Install “Patch Y” anyway?"));
        assert!(gated.shows(
            "Installing “Patch Y” would override the tables your custom Data/patch-G.mpq \
             replaces: CreatureDisplayInfo.dbc."
        ));

        let _ = gated.go(Message::ConfirmInstall);
        assert!(gated.state.busy.is_some(), "now it puts its files in place");
        assert_eq!(gated.state.busy.as_ref().map(|work| work.looks), Some(true));
        assert!(gated.state.pending.is_none());
        let _ = gated.go(Message::Progressed(
            gated.root(),
            "patch-y".to_owned(),
            Stage::Installing,
        ));
        assert!(gated.banner_shows("Step 3 of 3: putting its files in place"));

        gated.placed(true, BTreeMap::new()).await;
        assert!(gated.root().join("Data/patch-y.mpq").is_file());
        assert_eq!(gated.requests(), 1, "downloaded once");
    }

    #[tokio::test]
    async fn an_install_that_would_break_a_custom_patch_stops_before_touching_anything() {
        // Patch G's model needs a skin that only Patch F provides, and
        // Patch Y excludes Patch F (the HD pack's language archive isn't
        // there, so it is half a pack: backed up).
        let f = archive(&[("Creature\\Foo\\Foo00.skin", b"skin".to_vec())]);
        let g = archive(&[("Creature\\Foo\\Foo.m2", model(1))]);
        let brings = archive(&[("x.txt", b"y".to_vec())]);
        let mut gated = Gated::new(
            &[("Data/patch-F.mpq", &f), ("Data/patch-G.mpq", &g)],
            &brings,
            &["Data/patch-F.mpq", "Data/{locale}/patch-{locale}-F.MPQ"],
        )
        .await;
        let before = tree(&gated.root());
        let sentence = "Your custom Data/patch-G.mpq's models need Data/patch-F.mpq, which \
                        installing “Patch Y” would override.";

        install(&mut gated);
        assert!(gated.state.pending.is_some(), "nothing is known yet");
        let (fetched, _) = gated.fetched().await;
        let _ = gated.go(Message::Fetched(
            gated.root(),
            "patch-y".to_owned(),
            Ok(fetched),
        ));

        // It stopped where the progress was, and says so in the row.
        assert!(gated.state.busy.is_none() && gated.state.pending.is_none());
        assert!(gated.state.confirming.is_none());
        assert!(gated.banner_shows(sentence));
        assert!(gated.shows(sentence));
        assert_eq!(
            gated.state.blocked(&gated.root(), &gated.listed()),
            Some(sentence.to_owned()),
            "the button is off, with it for a tip"
        );
        let root = gated.root();
        let mut ui = simulator(gated.state.view(&root, false));
        let _ = ui.click("Install 3.1.0").unwrap();
        assert_eq!(ui.into_messages().count(), 0);

        // Nothing was touched, and the download is kept.
        assert_eq!(tree(&gated.root()), before);
        assert!(gated.kept());
        assert_eq!(gated.requests(), 1);

        // Pressing it again does nothing.
        install(&mut gated);
        assert!(gated.state.busy.is_none());
        assert_eq!(gated.requests(), 1);
    }

    #[tokio::test]
    async fn an_install_that_overrides_a_custom_patchs_tables_asks_first() {
        let mut g = tables(&["CreatureDisplayInfo", "CreatureModelData"]);
        g.push(("Creature\\Kept.m2".to_owned(), model(0)));
        let h = archive(&[
            ("Creature\\A.m2", model(0)),
            ("Creature\\B.m2", model(0)),
            ("Textures\\A.blp", b"blp".to_vec()),
        ]);
        let mut brings = tables(&["CreatureDisplayInfo", "CreatureModelData"]);
        brings.extend([
            ("Creature\\A.m2".to_owned(), model(0)),
            ("Creature\\B.m2".to_owned(), model(0)),
            ("Textures\\A.blp".to_owned(), b"other".to_vec()),
        ]);
        let mut gated = Gated::new(
            &[
                ("Data/patch-G.mpq", &archive_of(&g)),
                ("Data/patch-H.mpq", &h),
            ],
            &archive_of(&brings),
            &[],
        )
        .await;

        install(&mut gated);
        let (fetched, _) = gated.fetched().await;
        let _ = gated.go(Message::Fetched(
            gated.root(),
            "patch-y".to_owned(),
            Ok(fetched),
        ));

        for wanted in [
            "Installing “Patch Y” would override the tables your custom Data/patch-G.mpq \
             replaces: CreatureDisplayInfo.dbc and CreatureModelData.dbc.",
            "Installing “Patch Y” would override 2 models and 1 texture your custom \
             Data/patch-H.mpq replaces.",
            "Install “Patch Y” anyway?",
        ] {
            assert!(gated.shows(wanted), "{wanted}");
        }
        assert!(gated.state.stopped.is_none(), "it is no break");
        assert!(!gated.root().join("Data/patch-y.mpq").exists());

        // The player's patches stay where they are.
        let _ = gated.go(Message::ConfirmInstall);
        assert!(gated.state.busy.is_some());
        gated.placed(true, BTreeMap::new()).await;
        assert!(gated.root().join("Data/patch-y.mpq").is_file());
        assert!(gated.root().join("Data/patch-G.mpq").is_file());
        assert!(gated.root().join("Data/patch-H.mpq").is_file());
    }

    #[tokio::test]
    async fn an_install_a_higher_patch_would_override_says_so_and_asks() {
        let z = archive_of(&tables(&["WorldMapArea", "DungeonMap"]));
        let brings = archive_of(&tables(&["WorldMapArea", "DungeonMap", "Other"]));
        let mut gated = Gated::new(&[("Data/patch-Z.mpq", &z)], &brings, &[]).await;
        let sentence = "Data/patch-Z.mpq would override the tables “Patch Y” brings: \
                        DungeonMap.dbc and WorldMapArea.dbc.";

        install(&mut gated);
        let (fetched, _) = gated.fetched().await;
        let _ = gated.go(Message::Fetched(
            gated.root(),
            "patch-y".to_owned(),
            Ok(fetched),
        ));

        assert!(gated.state.stopped.is_none());
        assert!(gated.shows(sentence));
        assert!(gated.shows("Install “Patch Y” anyway?"));
        assert!(!gated.root().join("Data/patch-y.mpq").exists());
    }

    #[tokio::test]
    async fn a_release_whose_facts_are_cached_is_judged_before_the_download() {
        let g = archive_of(&tables(&["CreatureDisplayInfo"]));
        let brings = archive_of(&tables(&["CreatureDisplayInfo"]));
        let mut gated = Gated::new(&[("Data/patch-G.mpq", &g)], &brings, &[]).await;
        let sentence = "Installing “Patch Y” would override the tables your custom \
                        Data/patch-G.mpq replaces: CreatureDisplayInfo.dbc.";

        // Before the launcher read its archives, the row says nothing.
        gated.judged();
        assert!(!gated.shows(sentence));

        // Read once, by an install in this client or another.
        let listed = gated.listed();
        let download = BTreeMap::from([(listed.releases[0].url.clone(), gated.brings.clone())]);
        let _ = mods::read_archives(
            &gated.root(),
            &listed,
            &listed.releases[0],
            &download,
            None,
            None,
            gated.state.facts.as_ref().unwrap(),
        )
        .unwrap();
        gated.judged();

        // Now the verdict shows before any press, and the question comes
        // before the download.
        assert!(gated.shows(sentence));
        install(&mut gated);
        assert!(gated.state.busy.is_none() && gated.state.pending.is_none());
        assert!(gated.state.confirming.is_some());
        assert!(gated.shows("Install “Patch Y” anyway?"));
        assert_eq!(gated.requests(), 0);

        let _ = gated.go(Message::ConfirmInstall);
        assert_eq!(gated.requests(), 0, "downloading is the next step");
        assert_eq!(
            gated.state.busy.as_ref().map(|work| work.looks),
            Some(false)
        );
        gated.placed(true, BTreeMap::new()).await;
        assert_eq!(gated.requests(), 1);
        assert!(gated.root().join("Data/patch-y.mpq").is_file());
    }

    #[tokio::test]
    async fn an_installed_mod_says_which_patch_overrides_it() {
        let z = archive_of(&tables(&["WorldMapArea", "DungeonMap"]));
        let brings = archive_of(&tables(&["WorldMapArea", "DungeonMap", "Other"]));
        let mut gated = Gated::new(&[("Data/patch-Z.mpq", &z)], &brings, &[]).await;
        let listed = gated.listed();
        let sentence = "Data/patch-Z.mpq overrides the tables it brings: DungeonMap.dbc and \
                        WorldMapArea.dbc.";

        mods::install(
            &gated.installation,
            &listed,
            &listed.releases[0],
            &gated.brings,
            &[],
            false,
        )
        .unwrap();
        let download = BTreeMap::from([(listed.releases[0].url.clone(), gated.brings.clone())]);
        let _ = mods::read_archives(
            &gated.root(),
            &listed,
            &listed.releases[0],
            &download,
            None,
            None,
            gated.state.facts.as_ref().unwrap(),
        )
        .unwrap();
        gated.looked();
        gated.judged();

        assert_eq!(
            gated.state.found[&gated.root()]["patch-y"],
            status(Some("3.1.0"), Vec::new())
        );
        assert!(gated.shows(sentence));
        assert!(gated.shows("Version 3.1.0 installed."));
        assert!(!gated.shows("Install 3.1.0"), "nothing to install");
    }

    #[tokio::test]
    async fn a_mod_without_archives_keeps_todays_flow() {
        let brings = archive(&[("x.txt", b"x".to_vec())]);
        let mut gated = Gated::new(&[], &brings, &[]).await;
        gated.judged();
        assert!(
            gated
                .state
                .gates
                .get(&gated.root())
                .is_none_or(|gates| !gates.contains_key("glow"))
        );

        // Glow has none: the download starts at once, then the files go in
        // place, then the setup.
        let _ = gated.go(Message::Install("glow".to_owned()));
        assert!(gated.state.pending.is_none());
        assert!(gated.state.confirming.is_none());
        let work = gated.state.busy.as_ref().unwrap();
        assert_eq!((work.steps, work.looks), (3, false));
        assert!(gated.banner_shows("Step 1 of 3: downloading, 0 KB"));

        // Files of something else where it goes are asked about before
        // the download, as always.
        let mut asking = Gated::new(&[], &brings, &[]).await;
        let root = asking.root();
        let _ = asking.state.foreign.insert(
            root,
            BTreeMap::from([("glow".to_owned(), vec!["version.dll".to_owned()])]),
        );
        let _ = asking.go(Message::Install("glow".to_owned()));
        assert!(asking.state.busy.is_none() && asking.state.confirming.is_some());
        assert!(asking.shows("Install “Glow” anyway?"));
    }

    #[tokio::test]
    async fn cancelling_after_the_download_keeps_it_for_next_time() {
        let g = archive_of(&tables(&["CreatureDisplayInfo"]));
        let brings = archive_of(&tables(&["CreatureDisplayInfo"]));
        let mut gated = Gated::new(&[("Data/patch-G.mpq", &g)], &brings, &[]).await;

        install(&mut gated);
        let (fetched, _) = gated.fetched().await;
        let _ = gated.go(Message::Fetched(
            gated.root(),
            "patch-y".to_owned(),
            Ok(fetched),
        ));
        assert!(gated.state.confirming.is_some());

        let _ = gated.go(Message::CancelInstall);
        assert!(gated.state.confirming.is_none() && gated.state.pending.is_none());
        assert!(!gated.root().join("Data/patch-y.mpq").exists());
        assert!(gated.kept());

        // Next time it is known what it does: asked before, and the copy
        // kept is used.
        gated.judged();
        install(&mut gated);
        assert!(gated.state.confirming.is_some() && gated.state.busy.is_none());
        let _ = gated.go(Message::ConfirmInstall);
        gated.placed(true, BTreeMap::new()).await;
        assert_eq!(gated.requests(), 1, "downloaded once");
        assert!(gated.root().join("Data/patch-y.mpq").is_file());
    }

    /// `OLD_LAUNCHER_TEST_CLIENT=<client folder> cargo test --release --lib
    /// clients::mods::tests::the_gate -- --ignored --nocapture`, the folder
    /// only ever read: downloads Lau's Patch Y for clients without the HD
    /// models and says what installing it would do to the client's
    /// patches, with the rules the list had when installing it crashed
    /// Dalaran (it excluded `patch-f.mpq` too) and with today's. Scratch
    /// files and the caches are in temporary folders.
    #[tokio::test]
    #[ignore = "downloads Patch Y from GitHub and reads every archive of the client in $OLD_LAUNCHER_TEST_CLIENT"]
    async fn the_gate_on_a_real_client_says_what_installing_patch_y_does() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let root = PathBuf::from(
            std::env::var("OLD_LAUNCHER_TEST_CLIENT")
                .expect("OLD_LAUNCHER_TEST_CLIENT is a client folder"),
        );
        let locale = Installation::open(&root)
            .expect("it is a client")
            .locale()
            .expect("it has a language");
        let json = fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/index/mods.json")).unwrap();
        let list = Mods::parse(&json).unwrap();
        let today = list
            .mods
            .iter()
            .find(|listed| listed.id == "lau-non-hd")
            .unwrap()
            .clone();
        let mut then = today.clone();
        then.excludes = vec![
            "Data/patch-f.mpq".to_owned(),
            "Data/{locale}/patch-{locale}-F.MPQ".to_owned(),
        ];
        let release = today.newest().unwrap().clone();

        // Everything the client holds, to see it is as it was.
        fn listing(folder: &Path, files: &mut BTreeMap<PathBuf, (u64, std::time::SystemTime)>) {
            for entry in fs::read_dir(folder).unwrap().flatten() {
                let metadata = entry.metadata().unwrap();

                if metadata.is_dir() {
                    listing(&entry.path(), files);
                } else {
                    let _ =
                        files.insert(entry.path(), (metadata.len(), metadata.modified().unwrap()));
                }
            }
        }
        let mut before = BTreeMap::new();
        listing(&root, &mut before);

        let scratch = tempfile::tempdir().unwrap();
        let facts = Cache::at(scratch.path().join("facts"));
        let started = std::time::Instant::now();
        let scan = crate::patches::look(&root, locale, &facts, &|_| {}).unwrap();
        println!("looked at the patches in {:?}", started.elapsed());

        let client = crate::http_client().build().unwrap();
        let (mut progress, _) = Sender::channel(1);
        let download = get(&client, None, &release.url, &mut progress)
            .await
            .unwrap();
        let downloads = BTreeMap::from([(release.url.clone(), download)]);
        let started = std::time::Instant::now();
        let archives = mods::read_archives(
            &root,
            &today,
            &release,
            &downloads,
            None,
            Some(scratch.path()),
            &facts,
        )
        .unwrap();
        println!("read its archives in {:?}", started.elapsed());

        for (rules, listed) in [("old", &then), ("current", &today)] {
            let started = std::time::Instant::now();
            let change = gate::change_of(&root, &list, listed, &release);
            let install = gate::install_in(&root, &list, listed, &change, &scan.graph, &archives)
                .expect("it goes in the client");

            println!("--- with the {rules} rules ({:?})", started.elapsed());
            println!("removes {:?}", change.removes);
            println!("adds {:?}", change.adds);
            for lost in &scan.graph.gate(&change, &archives).breaks {
                println!("break: {lost:?}");
            }
            println!("blocked: {:?}", install.blocked);
            for line in &install.asks {
                println!("asks: {line}");
            }
        }

        let mut after = BTreeMap::new();
        listing(&root, &mut after);
        assert_eq!(before, after, "the client is as it was");
    }

    #[tokio::test]
    #[ignore = "downloads Lumenveil's newest release from GitHub"]
    async fn the_listed_lumenveil_installs_into_a_client() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = crate::http_client().build().unwrap();
        let (_root, installation) = crate::core::testing::client(&[]);
        let json = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/index/mods.json")).unwrap();
        let lumenveil = Mods::parse(&json)
            .unwrap()
            .mods
            .into_iter()
            .find(|listed| listed.id == "lumenveil")
            .unwrap();
        let newest = lumenveil.newest().unwrap().clone();

        let (mut progress, _) = Sender::channel(1);
        let download = get(&client, None, &newest.url, &mut progress)
            .await
            .unwrap();
        mods::install(&installation, &lumenveil, &newest, &download, &[], false).unwrap();

        assert_eq!(
            mods::status(installation.root(), &lumenveil, &[]),
            Some(Status {
                release: Some(newest.version.clone()),
                problems: vec![Problem::NotSetUp(vec![
                    "lampdata.bin".to_owned(),
                    "liquiddata.bin".to_owned()
                ])],
            }),
            "the setup is left to run"
        );
        assert_eq!(newest.natives(), ["CoAVolFog", "version"]);
    }

    #[tokio::test]
    #[ignore = "downloads two of Lau's editions from GitHub"]
    async fn the_listed_lau_editions_take_each_others_place() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = crate::http_client().build().unwrap();
        let json = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/index/mods.json")).unwrap();
        let list = Mods::parse(&json).unwrap();
        let listed = |id: &str| list.mods.iter().find(|listed| listed.id == id).unwrap();
        let (non_hd, original) = (listed("lau-non-hd"), listed("lau-hd-original-spells"));
        let (_root, installation) = crate::core::testing::client(&[
            "Data/common.MPQ",
            "Data/deDE/locale-deDE.MPQ",
            "Data/enUS/locale-enUS.MPQ",
        ]);
        let root = installation.root();
        let install = async |game_mod: &Mod| {
            let newest = game_mod.newest().unwrap();
            let (mut progress, _) = Sender::channel(1);
            let download = get(&client, None, &newest.url, &mut progress)
                .await
                .unwrap();
            mods::install(
                &installation,
                game_mod,
                newest,
                &download,
                &list.alternatives(game_mod),
                false,
            )
            .unwrap();
        };

        assert!(mods::fits(root, non_hd) && !mods::fits(root, original));
        install(non_hd).await;
        for locale in ["enUS", "deDE"] {
            assert!(
                root.join(format!("Data/{locale}/patch-{locale}-Y.MPQ"))
                    .is_file(),
                "every language the client has"
            );
        }

        // Given the HD models, the client suits the HD editions instead.
        std::fs::write(root.join("Data/patch-f.mpq"), "").unwrap();
        std::fs::write(root.join("Data/enUS/patch-enUS-F.MPQ"), "").unwrap();
        assert!(!mods::fits(root, non_hd) && mods::fits(root, original));
        install(original).await;

        let seen = look(root, &list);
        assert_eq!(
            seen.held.keys().map(String::as_str).collect::<Vec<_>>(),
            ["lau-hd-original-spells"]
        );
        assert_eq!(seen.held["lau-hd-original-spells"].problems, []);
        assert!(seen.unfit.contains("lau-non-hd"));
    }
}
