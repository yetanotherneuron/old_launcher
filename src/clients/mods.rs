//! Mods in a client, as the list of mods describes them: looked for whenever
//! the client is read, with a banner above the addons while the launcher
//! needs to check or repair one before starting the game, and a section of
//! the clients dialog that installs, updates, repairs and removes them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use iced::futures::StreamExt;
use iced::task::{Straw, sipper};
use iced::widget::{button, checkbox, column, container, progress_bar, row, space, text};
use iced::{Center, Element, Fill, Right, Task, Widget};
use sipper::Sender;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::action::Action;
use crate::activity::activity;
use crate::core::mods::{self, Mod, Mods, Problem, Release, Status};
use crate::core::{Config, Installation, Launch, launch};
use crate::errors::Errors;
use crate::{blocking, describe, github, size, style, update};

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
    /// The client folders being looked at, and whether to look again once
    /// done: asked meanwhile, the folder may have changed since.
    finding: BTreeMap<PathBuf, bool>,
    /// The work under way.
    busy: Option<Work>,
    /// The client folder and the mod whose removal from it waits for a yes,
    /// and whether the player's files go too.
    removing: Option<(PathBuf, String, bool)>,
    /// What the last work did.
    done: Option<Work>,
    /// Downloads from GitHub signed in, when there is a login.
    github: Option<github::Signed>,
}

/// Work on a mod in a client folder, and what it is or did, for people.
#[derive(Debug, Clone)]
struct Work {
    root: PathBuf,
    id: String,
    said: String,
    /// How far an install got, while it goes on.
    stage: Option<Stage>,
    /// How many steps an install has: the download, putting the files in
    /// place, and each step of the setup.
    steps: usize,
}

/// How far installing a mod got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stage {
    /// Downloading its release: the bytes received, and how many there are
    /// when the server says.
    Downloading { received: u64, total: Option<u64> },
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
    /// own (see [`mods::foreign`]): they keep it from being installed or
    /// updated.
    pub foreign: BTreeMap<String, Vec<String>>,
}

/// Interactions with the banners and the section, and what their work comes
/// back with. Mods go by id.
#[derive(Debug, Clone)]
pub enum Message {
    /// What a client folder holds of the mods was looked at.
    Found(PathBuf, Seen),
    /// A mod's name, a link to its page, was pressed.
    OpenPage(String),
    /// Its page couldn't be opened: why.
    Unopened(String),
    /// Install, or update to the newest release, was pressed.
    Install(String),
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
/// found is known by its release, or as installed by hand; and the files
/// of something else where each would put its own.
pub fn look(root: &Path, list: &Mods) -> Seen {
    let mut seen = Seen::default();

    for listed in &list.mods {
        let status = match mods::check(root, listed) {
            Ok(status) => status,
            Err(error) => {
                log::warn!(
                    "Could not check {} in {}: {error}",
                    listed.name,
                    root.display()
                );
                mods::status(root, listed)
            }
        };
        if let Some(status) = status {
            let _ = seen.held.insert(listed.id.clone(), status);
        }
        let foreign = listed
            .newest()
            .map(|newest| mods::foreign(root, listed, newest))
            .unwrap_or_default();
        if !foreign.is_empty() {
            let _ = seen.foreign.insert(listed.id.clone(), foreign);
        }
    }

    seen
}

impl State {
    /// Downloads from GitHub sign in with `github` from now on.
    pub fn sign_in(&mut self, github: Option<github::Signed>) {
        self.github = github;
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

    /// Why `listed` can't be installed in the client folder at `root`, or
    /// updated there, if it can't: another mod there it can't be beside, or
    /// files of something else where it puts its own.
    fn blocked(&self, root: &Path, listed: &Mod) -> Option<String> {
        let held = self.found.get(root);
        let installing = !held.is_some_and(|held| held.contains_key(&listed.id));

        let beside = self
            .list
            .mods
            .iter()
            .filter(|other| other.id != listed.id && installing)
            .filter(|other| held.is_some_and(|held| held.contains_key(&other.id)))
            .find_map(|other| {
                let shared = mods::conflicts(listed, other);
                (!shared.is_empty()).then_some((other, shared))
            });
        if let Some((other, shared)) = beside {
            return Some(format!(
                "It can't be beside {}, which this client holds: both use {}. Uninstall {} to \
                 install {}.",
                other.name,
                names(&shared),
                other.name,
                listed.name,
            ));
        }

        let foreign = self.foreign.get(root)?.get(&listed.id)?;
        let (is, it) = if foreign.len() == 1 {
            ("isn't", "it")
        } else {
            ("aren't", "them")
        };
        Some(format!(
            "{} here {is} {}'s: another mod may use {it}. Remove {it} to {} {}.",
            names(foreign),
            listed.name,
            if installing { "install" } else { "update" },
            listed.name,
        ))
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
            Message::Found(root, seen) => {
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
                let _ = self.found.insert(root.clone(), seen.held);
                let _ = self.foreign.insert(root.clone(), seen.foreign);
                // Asked meanwhile: the folder may have changed since.
                if self.finding.remove(&root) == Some(true) {
                    return Action::task(self.find(&root));
                }
                Action::none()
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
                let Some(listed) = self.listed(&id).cloned() else {
                    return Action::none();
                };
                // As the section says, with no button to press.
                if current.is_some_and(|root| self.blocked(root, &listed).is_some()) {
                    return Action::none();
                }
                let Some(release) = listed.newest().cloned() else {
                    return Action::none();
                };
                let busy = format!("Installing {} {}", listed.name, release.version);
                let done = format!("{} {} is installed.", listed.name, release.version);

                self.start(
                    installation,
                    config,
                    playing,
                    client,
                    errors,
                    listed,
                    release,
                    busy,
                    done,
                )
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
                let busy = format!("Repairing {}", listed.name);
                let done = format!("{} is repaired.", listed.name);

                self.start(
                    installation,
                    config,
                    playing,
                    client,
                    errors,
                    listed,
                    release,
                    busy,
                    done,
                )
            }
            Message::Remove(id) => {
                if let Some(installation) = installation {
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
                    said: format!("Removing {}", listed.name),
                    stage: None,
                    steps: 0,
                });
                self.done = None;

                let installation = installation.clone();
                let removed = format!("{} is removed.", listed.name);
                Action::task(Task::perform(
                    blocking(move || mods::uninstall(&installation, &listed, !delete)),
                    move |result| Message::Done(root, id, result.map(|()| removed)),
                ))
            }
            Message::Dismiss => {
                self.done = None;
                Action::none()
            }
            Message::Progressed(root, id, stage) => {
                if let Some(work) = self.busy.as_mut().filter(|work| work.on(&root, &id)) {
                    work.stage = Some(stage);
                }
                Action::none()
            }
            Message::Done(root, id, result) => {
                self.busy = None;
                match result {
                    Ok(said) => {
                        log::info!("{}: {said}", root.display());
                        self.done = Some(Work {
                            root: root.clone(),
                            id,
                            said,
                            stage: None,
                            steps: 0,
                        });
                    }
                    Err(error) => errors.report(format!("{}: {error}", self.name(&id))),
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

    /// Installs `release` of `listed` in `installation`, the current client,
    /// which starts as `config` says, in place of what is there, then runs
    /// its setup, unless its game runs (`playing`) or something else is
    /// being done. `busy` says what is being done meanwhile, with how far it
    /// got, and `done` what was once it is.
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
        busy: String,
        done: String,
    ) -> Action<Instruction, Message> {
        let Some(installation) = installation.filter(|_| !playing && self.busy.is_none()) else {
            return Action::none();
        };
        let client = match client {
            Ok(client) => client.clone(),
            Err(error) => {
                errors.report(format!("Could not download {}: {error}", listed.name));
                return Action::none();
            }
        };
        let root = installation.root().to_path_buf();
        let id = listed.id.clone();
        self.removing = None;
        self.done = None;

        log::info!(
            "Installing {} {} in {}",
            listed.name,
            release.version,
            root.display()
        );
        self.busy = Some(Work {
            root: root.clone(),
            id: id.clone(),
            said: busy,
            stage: Some(Stage::Downloading {
                received: 0,
                total: None,
            }),
            steps: 2 + release.setup.len(),
        });

        let (progressed, worked) = ((root.clone(), id.clone()), (root.clone(), id));
        Action::task(Task::sip(
            work(
                client,
                self.github.clone(),
                installation.clone(),
                config.launch(&root),
                listed,
                release,
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
        let done = self.done.as_ref().filter(|work| work.on(root, &listed.id));

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
                let repair: Element<'_, Message> = if playing {
                    style::tip(repair, CLOSE_THE_GAME).boxed()
                } else {
                    repair.on_press(Message::Repair(listed.id.clone())).boxed()
                };

                (
                    text(format!(
                        "The launcher needs to repair {} before starting the game. {}",
                        listed.name,
                        why.join(" ")
                    ))
                    .boxed(),
                    row![repair],
                    style::Tone::Warning,
                )
            }
            (None, None, Some(work)) => (
                text(&work.said).boxed(),
                row![
                    button("Dismiss")
                        .style(style::quiet)
                        .on_press(Message::Dismiss)
                ],
                style::Tone::Success,
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
        // Only once the folder was looked at, and when there is something
        // to install.
        let blocked = found
            .filter(|_| status.is_none() || newer.is_some())
            .and_then(|_| self.blocked(root, listed));

        let state: Element<'a, Message> = match (found, status) {
            (None, _) => text("Looking…").size(14).style(text::secondary).boxed(),
            (Some(_), None) => text("Not installed.").size(14).boxed(),
            (Some(_), Some(status)) => text(match (by_hand, &status.release, newer) {
                (Some(Some(version)), ..) => format!("Version {version} installed manually."),
                (Some(None), ..) => "Installed manually.".to_owned(),
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

            if playing {
                style::tip(button, CLOSE_THE_GAME).boxed()
            } else {
                button.on_press_maybe(idle.then_some(message)).boxed()
            }
        };

        // A copy installed by hand is offered the newest release in its
        // place; one installed from the list, a newer one.
        let offered = if by_hand.is_some() {
            newest.map(|release| (format!("Install {}", release.version), release))
        } else {
            newer.map(|release| (format!("Update to {}", release.version), release))
        };
        let buttons = match status {
            None => row![newest.filter(|_| blocked.is_none()).map(|release| action(
                format!("Install {}", release.version),
                Message::Install(listed.id.clone()),
                style::primary,
            ))],
            Some(_) => row![
                offered
                    .filter(|_| blocked.is_none())
                    .map(|(label, _)| action(
                        label,
                        Message::Install(listed.id.clone()),
                        style::primary,
                    )),
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
                        text(format!("Remove {} from this client?", listed.name)).size(14),
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

        column![
            // What it is, and beside it what can be done, and how it is.
            row![
                column![
                    row![
                        button(
                            text(format!("{} ↗", listed.name))
                                .size(14)
                                .font(style::BOLD)
                        )
                        .style(style::quiet)
                        .padding(0)
                        .on_press(Message::OpenPage(listed.source.page.clone())),
                        text(&listed.source.name).size(13).style(text::secondary),
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
            blocked.map(|why| text(why).size(13).style(text::warning)),
            column(problems).spacing(4),
            note,
            removal,
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
/// (the download, putting the files in place, then each step of the setup),
/// with a bar for that step, and what the setup last said. The bar fills
/// as the step goes when the step can tell how far it got, and glides
/// while it goes on otherwise.
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
        Stage::Installing => (2, "putting its files in place".to_owned(), None),
        Stage::SettingUp { step, said, .. } => {
            (2 + step, "running its setup".to_owned(), said.as_deref())
        }
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

/// Installs `release` of `listed` in `installation`'s folder, in place of
/// what is there, downloading with `client`, or signed in to GitHub with
/// `github`, then runs its setup the way `launch` starts the game, saying
/// how far it got.
fn work(
    client: reqwest::Client,
    github: Option<github::Signed>,
    installation: Installation,
    launch: Launch,
    listed: Mod,
    release: Release,
) -> impl Straw<(), Stage, String> {
    sipper(async move |mut progress| {
        // Downloaded before anything changes.
        let download = get(&client, github.as_ref(), &release.url, &mut progress).await?;

        progress.send(Stage::Installing).await;
        let (target, installed, set) = (installation.clone(), release.clone(), listed.clone());
        blocking(move || mods::install(&target, &listed, &installed, &download)).await?;

        set_up(&installation, &launch, &release, &mut progress).await?;
        // Only now: until then, the marker says the setup didn't finish.
        blocking(move || mods::finish_setup(&installation, &set, &release)).await
    })
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

                Ok(argument.replace("{locale}", locale))
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
            inside: None,
            setup: vec![Step {
                run: "Glow/setup.exe".to_owned(),
                arguments: vec!["--locale".to_owned(), "{locale}".to_owned()],
                creates: "glow.bin".to_owned(),
            }],
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
            "Glow ↗",
            "Light everywhere.",
            "Not installed.",
            "Install 2.0",
            "Servers may forbid modified clients, and their anti-cheat may detect it.",
        ] {
            assert!(shows(&state, false, wanted), "{wanted}");
        }
        assert!(state.banner(Path::new(ROOT), false).is_none());
        assert!(!state.holds_back(Path::new(ROOT)));
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

        assert!(shows(
            &state,
            false,
            "It can't be beside Glow, which this client holds: both use Version.dll. Uninstall \
             Glow to install Glare."
        ));
        assert!(!shows(&state, false, "Install 0.5"));
        let _ = update(&mut state, Message::Install("glare".to_owned()), None);
        assert!(state.busy.is_none());
    }

    #[test]
    fn files_of_something_else_where_it_goes_keep_it_out_until_removed() {
        let foreign = |state: &mut State| {
            let _ = state.foreign.insert(
                ROOT.into(),
                BTreeMap::from([("glow".to_owned(), vec!["version.dll".to_owned()])]),
            );
        };

        let mut absent = found(None);
        foreign(&mut absent);
        assert!(shows(
            &absent,
            false,
            "version.dll here isn't Glow's: another mod may use it. Remove it to install Glow."
        ));
        assert!(!shows(&absent, false, "Install 2.0"));

        let mut older = found(Some(status(Some("1.0"), Vec::new())));
        foreign(&mut older);
        assert!(shows(
            &older,
            false,
            "version.dll here isn't Glow's: another mod may use it. Remove it to update Glow."
        ));
        assert!(!shows(&older, false, "Update to 2.0"));
        assert!(
            shows(&older, false, "Uninstall"),
            "what is there stays manageable"
        );
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
            "The launcher needs to repair Glow before starting the game. Not as released: \
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
        assert!(ui.find("Installed manually.").is_ok());
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
        });
        assert!(state.find(&root).units() == 0);
        assert!(state.finding.is_empty());
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
        assert_eq!(
            target(&glow(), &status(None, Vec::new())).unwrap().version,
            "2.0",
            "the newest goes in its place"
        );
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
            assert!(ui.find("Remove Glow from this client?").is_ok());
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
        assert!(ui.find("Remove Glow from this client?").is_err());
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
        mods::install(&installation, &lumenveil, &newest, &download).unwrap();

        assert_eq!(
            mods::status(installation.root(), &lumenveil),
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
}
