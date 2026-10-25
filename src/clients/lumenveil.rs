//! Lumenveil, a graphics mod, in a client: looked for whenever the client
//! is read, with a banner above the addons while the game needs something
//! done before it loads it, and a section of the clients dialog that
//! installs, updates, repairs and removes it.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use iced::widget::{button, checkbox, column, container, row, space, text};
use iced::{Center, Element, Fill, Function, Task, Widget};
use serde::Deserialize;

use crate::action::Action;
use crate::core::lumenveil::{self, Problem, Status, Sums};
use crate::core::{Config, Installation, Launch, launch};
use crate::errors::Errors;
use crate::{blocking, describe, github, style, update};

/// Lumenveil's page.
pub const PAGE: &str = "https://github.com/lumenveil/Lumenveil";

/// Its releases, as GitHub's API lists them.
const RELEASES: &str = "https://api.github.com/repos/lumenveil/Lumenveil/releases";

/// The most a file of a release may weigh: its zip is about a megabyte.
const LARGEST: u64 = 64 * 1024 * 1024;

/// What the hint of a button says while the game runs.
const CLOSE_THE_GAME: &str = "Close the game first.";

/// How a problem is colored.
type Tone = fn(&iced::Theme) -> text::Style;

/// What is known of Lumenveil in each client, and what is being done.
#[derive(Default)]
pub struct State {
    /// The latest release, once GitHub answered, or why it didn't.
    latest: Option<Result<Release, String>>,
    /// Whether GitHub is being asked for it.
    asking: bool,
    /// The checksums of each version a client has, once fetched, or why
    /// they couldn't be.
    sums: BTreeMap<String, Result<Sums, String>>,
    /// What each client folder looked at holds of Lumenveil (`None`:
    /// nothing of it), its files checked against the checksums kept of its
    /// release, if any.
    found: BTreeMap<PathBuf, Option<Status>>,
    /// The client folders being checked against the release of their
    /// version: its checksums fetched, then their files read.
    checking: BTreeMap<PathBuf, String>,
    /// The client being worked on, and what is being done to it.
    busy: Option<(PathBuf, String)>,
    /// Whether removing it from the client shown waits for a yes, and
    /// whether the player's settings go too.
    removing: Option<bool>,
    /// What the last work on a client did, to say so.
    done: Option<(PathBuf, String)>,
    /// Downloads from GitHub signed in, when there is a login.
    github: Option<github::Signed>,
}

/// A release of Lumenveil, as GitHub lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Its version, as tagged without the `v`: `0.1.1`.
    pub version: String,
    /// Where its zip downloads from.
    archive: String,
    /// Where its `SHA256SUMS.txt` downloads from.
    sums: String,
}

/// Interactions with the banner and the section, and what their work comes
/// back with.
#[derive(Debug, Clone)]
pub enum Message {
    /// GitHub said which release is the latest, or why it couldn't.
    Latest(Result<Release, String>),
    /// What a client folder holds of Lumenveil was looked at.
    Found(PathBuf, Option<Status>),
    /// Check, which compares the files with their release's, was pressed.
    Check,
    /// The checksums of a version were fetched, or couldn't be.
    Sums(String, Result<Sums, String>),
    /// A client folder's files were checked against its release, or
    /// couldn't be read.
    Checked(PathBuf, Result<Option<Status>, String>),
    /// The name, a link to the page, was pressed.
    OpenPage,
    /// Install, or update to the latest release, was pressed.
    Install,
    /// Repair was pressed.
    Repair,
    /// Uninstall was pressed: ask first.
    Remove,
    /// The box deleting the player's settings too was ticked or unticked.
    DeleteSettings(bool),
    /// Removing it was confirmed.
    ConfirmRemove,
    /// Removing it was thought better of.
    CancelRemove,
    /// The banner saying what was done was dismissed.
    Dismiss,
    /// The work on a client folder ended: what it did, or why it failed.
    Done(PathBuf, Result<String, String>),
}

/// What the section asks of the dialog.
pub enum Instruction {
    /// The client folder at this path changed: read it again.
    Changed(PathBuf),
}

/// Which release to install, in place of what is there.
#[derive(Debug)]
enum Files {
    /// This one, as GitHub listed it.
    Release(Release),
    /// The one of this version, or the latest.
    Version(Option<String>),
}

/// What the launcher needs to do to Lumenveil in a client before starting
/// the game.
#[derive(Debug, PartialEq, Eq)]
enum Need {
    /// Check its files against the release of this version.
    Check(String),
    /// Repair it: install its release again, or the latest when its version
    /// can't be told.
    Repair,
}

/// Looks at what the client folder at `root` holds of Lumenveil, its files
/// checked against the checksums kept of its release, if any.
pub fn find(root: &Path) -> Task<Message> {
    let root = root.to_path_buf();

    Task::perform(
        blocking({
            let root = root.clone();
            move || Ok(lumenveil::status(&root, lumenveil::kept(&root).as_ref()))
        }),
        move |found| Message::Found(root, found.unwrap_or(None)),
    )
}

impl State {
    /// Downloads from GitHub sign in with `github` from now on.
    pub fn sign_in(&mut self, github: Option<github::Signed>) {
        self.github = github;
    }

    /// What the client folder at `root` holds of Lumenveil, as last looked
    /// at.
    fn status(&self, root: &Path) -> Option<&Status> {
        self.found.get(root).and_then(Option::as_ref)
    }

    /// Whether starting the game of the client at `root` waits for the
    /// launcher to check or repair Lumenveil in it, or to finish doing
    /// something to it.
    pub fn holds_back(&self, root: &Path) -> bool {
        self.busy.as_ref().is_some_and(|(busy, _)| busy == root)
            || self.status(root).and_then(need).is_some()
    }

    /// Notes that the clients dialog shows the client folder at `root`:
    /// it is looked at, if it wasn't yet, and GitHub is asked for the latest
    /// release, unless it said already, downloading with `client`.
    pub fn shown(&mut self, root: &Path, client: Result<&reqwest::Client, &str>) -> Task<Message> {
        let found = if self.found.contains_key(root) {
            Task::none()
        } else {
            find(root)
        };

        let latest = match client {
            Ok(client) if !matches!(self.latest, Some(Ok(_))) && !self.asking => {
                self.asking = true;
                log::info!("Asking GitHub for Lumenveil's latest release");
                Task::perform(
                    release(client.clone(), self.github.clone(), None),
                    Message::Latest,
                )
            }
            _ => Task::none(),
        };

        Task::batch([found, latest])
    }

    /// Handles an interaction with the banner or the section of the
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
        match message {
            Message::Latest(latest) => {
                self.asking = false;
                match &latest {
                    Ok(release) => log::info!("Lumenveil's latest release is {}", release.version),
                    Err(error) => log::warn!("Could not ask for Lumenveil's releases: {error}"),
                }
                self.latest = Some(latest);
                Action::none()
            }
            Message::Found(root, found) => {
                if let Some(status) = &found {
                    log::info!("Lumenveil in {}: {status:?}", root.display());
                }
                let _ = self.found.insert(root, found);
                Action::none()
            }
            Message::Check => {
                let Some(root) = current.filter(|root| !self.checking.contains_key(*root)) else {
                    return Action::none();
                };
                let Some(Need::Check(version)) = self.status(root).and_then(need) else {
                    return Action::none();
                };
                log::info!(
                    "Checking Lumenveil in {} against release {version}",
                    root.display()
                );
                self.done = None;
                let fetching = self.checking.values().any(|checked| *checked == version);
                let _ = self.checking.insert(root.to_path_buf(), version.clone());

                // Asked again when GitHub couldn't tell before.
                match (self.sums.get(&version), client) {
                    (Some(Ok(sums)), _) => Action::task(check(root.to_path_buf(), sums.clone())),
                    _ if fetching => Action::none(),
                    (_, Ok(client)) => Action::task(Task::perform(
                        checksums(client.clone(), self.github.clone(), version.clone()),
                        move |sums| Message::Sums(version, sums),
                    )),
                    (_, Err(error)) => {
                        let _ = self.checking.remove(root);
                        errors.report(format!("Could not check Lumenveil: {error}"));
                        Action::none()
                    }
                }
            }
            Message::Sums(version, sums) => {
                if let Err(error) = &sums {
                    log::warn!("Could not fetch the checksums of Lumenveil {version}: {error}");
                }
                let roots: Vec<PathBuf> = self
                    .checking
                    .iter()
                    .filter(|(_, checked)| **checked == version)
                    .map(|(root, _)| root.clone())
                    .collect();

                let checks = match &sums {
                    Ok(sums) => Task::batch(
                        roots
                            .into_iter()
                            .map(|root| check(root, sums.clone()))
                            .collect::<Vec<_>>(),
                    ),
                    // Said on the banner and in the section.
                    Err(_) => {
                        for root in &roots {
                            let _ = self.checking.remove(root);
                        }
                        Task::none()
                    }
                };
                let _ = self.sums.insert(version, sums);

                Action::task(checks)
            }
            Message::Checked(root, checked) => {
                let _ = self.checking.remove(&root);

                match checked {
                    Ok(found) => {
                        log::info!("Lumenveil in {}, checked: {found:?}", root.display());
                        if found.as_ref().is_some_and(|status| need(status).is_none()) {
                            self.done = Some((
                                root.clone(),
                                "Lumenveil is checked: its files are as released.".to_owned(),
                            ));
                        }
                        let _ = self.found.insert(root, found);
                    }
                    Err(error) => errors.report(format!("Could not check Lumenveil: {error}")),
                }
                Action::none()
            }
            Message::OpenPage => {
                log::info!("Opening {PAGE}");
                if let Err(error) = open::that_detached(PAGE) {
                    errors.report(format!("Could not open {PAGE}: {error}"));
                }
                Action::none()
            }
            Message::Install => {
                let Some(Ok(release)) = self.latest.clone() else {
                    return Action::none();
                };
                let busy = format!(
                    "Installing Lumenveil {}, then finding where the world's lamps and water \
                     are, which takes one to five minutes…",
                    release.version
                );
                let done = format!("Lumenveil {} is installed.", release.version);

                self.start(
                    current,
                    installation,
                    config,
                    playing,
                    client,
                    errors,
                    Files::Release(release),
                    busy,
                    done,
                )
            }
            Message::Repair => {
                let Some(version) = current
                    .and_then(|root| self.status(root))
                    .map(|status| status.version.clone())
                else {
                    return Action::none();
                };

                self.start(
                    current,
                    installation,
                    config,
                    playing,
                    client,
                    errors,
                    Files::Version(version),
                    "Repairing Lumenveil, then finding where the world's lamps and water are, \
                     which takes one to five minutes…"
                        .to_owned(),
                    "Lumenveil is repaired.".to_owned(),
                )
            }
            Message::Remove => {
                self.removing = Some(false);
                Action::none()
            }
            Message::DeleteSettings(delete) => {
                if self.removing.is_some() {
                    self.removing = Some(delete);
                }
                Action::none()
            }
            Message::CancelRemove => {
                self.removing = None;
                Action::none()
            }
            Message::ConfirmRemove => {
                let Some(delete) = self.removing.take() else {
                    return Action::none();
                };
                let Some(installation) = installation.filter(|_| !playing && self.busy.is_none())
                else {
                    return Action::none();
                };
                let root = installation.root().to_path_buf();
                log::info!("Removing Lumenveil from {}", root.display());
                self.busy = Some((root.clone(), "Removing Lumenveil…".to_owned()));
                self.done = None;

                let installation = installation.clone();
                Action::task(Task::perform(
                    blocking(move || lumenveil::uninstall(&installation, !delete)),
                    move |removed| {
                        Message::Done(root, removed.map(|()| "Lumenveil is removed.".to_owned()))
                    },
                ))
            }
            Message::Dismiss => {
                self.done = None;
                Action::none()
            }
            Message::Done(root, result) => {
                self.busy = None;
                match result {
                    Ok(done) => {
                        log::info!("{}: {done}", root.display());
                        self.done = Some((root.clone(), done));
                    }
                    Err(error) => {
                        errors.report(format!("Lumenveil: {error}"));
                    }
                }

                Action::instruction(Instruction::Changed(root))
            }
        }
    }

    /// Installs `files` in the `current` client, `installation`, which
    /// starts as `config` says, then runs the setup, unless its game runs
    /// (`playing`) or something else is being done. `busy` says what is
    /// being done meanwhile, and `done` what was once it is.
    #[allow(clippy::too_many_arguments)]
    fn start(
        &mut self,
        current: Option<&Path>,
        installation: Option<&Installation>,
        config: &Config,
        playing: bool,
        client: Result<&reqwest::Client, &str>,
        errors: &mut Errors,
        files: Files,
        busy: String,
        done: String,
    ) -> Action<Instruction, Message> {
        let Some(installation) = installation
            .filter(|installation| Some(installation.root()) == current)
            .filter(|_| !playing && self.busy.is_none())
        else {
            return Action::none();
        };
        let client = match client {
            Ok(client) => client.clone(),
            Err(error) => {
                errors.report(format!("Could not download Lumenveil: {error}"));
                return Action::none();
            }
        };
        let root = installation.root().to_path_buf();
        self.removing = None;
        self.done = None;

        log::info!("Installing Lumenveil in {}: {files:?}", root.display());
        self.busy = Some((root.clone(), busy));

        Action::task(Task::perform(
            work(
                client,
                self.github.clone(),
                installation.clone(),
                config.launch(&root),
                files,
            ),
            move |worked| Message::Done(root, worked.map(|()| done)),
        ))
    }

    /// The banner above the addons about Lumenveil in the client folder at
    /// `root`, while the launcher needs to check or repair it before
    /// starting the game, or to say what was just done; `playing` says
    /// whether its game runs.
    pub fn banner(&self, root: &Path, playing: bool) -> Option<Element<'_, Message>> {
        let needed = self
            .status(root)
            .and_then(|status| Some((status, need(status)?)));
        let busy = self.busy.as_ref().filter(|(busy, _)| busy == root);
        let done = self.done.as_ref().filter(|(done, _)| done == root);

        let (said, buttons, tone) = match (busy, needed, done) {
            (Some((_, doing)), _, _) => (doing.clone(), row![], style::Tone::Warning),
            (None, Some((_, Need::Check(version))), _) => {
                let checking = self.checking.contains_key(root);
                let said = match self.sums.get(&version) {
                    _ if checking => "Checking Lumenveil…".to_owned(),
                    Some(Err(error)) => format!(
                        "Lumenveil couldn't be checked: {error}. The launcher needs to check it \
                         before starting the game."
                    ),
                    _ => "Lumenveil is in this client. The launcher needs to check it before \
                          starting the game."
                        .to_owned(),
                };
                let check = button(if checking { "Checking…" } else { "Check" })
                    .style(button::primary)
                    .on_press_maybe((!checking).then_some(Message::Check));

                (said, row![check], style::Tone::Warning)
            }
            (None, Some((status, Need::Repair)), _) => {
                let why: Vec<String> = status
                    .problems
                    .iter()
                    .filter_map(|problem| match problem {
                        Problem::Unchecked => Some(
                            "Its version can't be told: repairing installs the latest release."
                                .to_owned(),
                        ),
                        problem if problem.repairable() => Some(sentence(problem)),
                        _ => None,
                    })
                    .collect();
                let repair = button("Repair").style(button::primary);
                let repair: Element<'_, Message> = if playing {
                    style::tip(repair, CLOSE_THE_GAME).boxed()
                } else {
                    repair.on_press(Message::Repair).boxed()
                };

                (
                    format!(
                        "The launcher needs to repair Lumenveil before starting the game. {}",
                        why.join(" ")
                    ),
                    row![repair],
                    style::Tone::Warning,
                )
            }
            (None, None, Some((_, done))) => (
                done.clone(),
                row![
                    button("Dismiss")
                        .style(button::text)
                        .on_press(Message::Dismiss)
                ],
                style::Tone::Success,
            ),
            (None, None, None) => return None,
        };

        Some(
            container(
                row![text(said).width(Fill), buttons.spacing(8)]
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
        let status = self.found.get(root).map(Option::as_ref);
        let busy = self
            .busy
            .as_ref()
            .filter(|(busy, _)| busy == root)
            .map(|(_, doing)| doing.as_str());
        let idle = self.busy.is_none();
        let latest = match &self.latest {
            Some(Ok(release)) => Some(release),
            _ => None,
        };

        let ours = status
            .flatten()
            .filter(|status| !status.problems.contains(&Problem::Foreign));
        let newer = ours.zip(latest).and_then(|(status, release)| {
            status
                .version
                .as_deref()
                .is_none_or(|version| update::newer(&release.version, version))
                .then_some(release)
        });

        let state: Element<'a, Message> = match (status, ours) {
            (None, _) => text("Looking…").style(text::secondary).boxed(),
            (Some(None), _) => text("Not installed.").boxed(),
            (Some(Some(_)), None) => text("Not installed.").boxed(),
            (Some(Some(_)), Some(status)) => text(match (&status.version, newer) {
                (Some(version), Some(release)) => {
                    format!("Version {version} installed; {} is out.", release.version)
                }
                (Some(version), None) => format!("Version {version} installed."),
                (None, _) => "Installed, of a version before 0.1.1.".to_owned(),
            })
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

        let needed = ours.and_then(need);
        // Checking writes nothing the game reads.
        let checking = self.checking.contains_key(root);
        let check = matches!(needed, Some(Need::Check(_))).then(|| {
            button(text(if checking { "Checking…" } else { "Check" }).size(13))
                .style(button::secondary)
                .padding([4, 12])
                .on_press_maybe((!checking).then_some(Message::Check))
        });
        let buttons = match ours {
            None => row![latest.map(|release| action(
                format!("Install {}", release.version),
                Message::Install,
                button::primary,
            ))],
            Some(_) => row![
                newer.map(|release| action(
                    format!("Update to {}", release.version),
                    Message::Install,
                    button::primary,
                )),
                check,
                (needed == Some(Need::Repair)).then(|| action(
                    "Repair".to_owned(),
                    Message::Repair,
                    button::secondary
                )),
                action("Uninstall".to_owned(), Message::Remove, button::secondary),
            ],
        }
        .spacing(8);

        // Amber what repairing fixes, red what it can't.
        let problems: Vec<(String, Tone)> = status
            .flatten()
            .map(|status| {
                status
                    .problems
                    .iter()
                    .map(|problem| match problem {
                        Problem::Unchecked => self.unchecked(root, status.version.as_deref()),
                        problem => {
                            let tone: Tone = if problem.repairable() {
                                text::warning
                            } else {
                                text::danger
                            };
                            (sentence(problem), tone)
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let fine = ours.is_some() && problems.is_empty();

        let note = busy
            .map(|doing| text(doing).size(13).style(text::secondary))
            .or_else(|| {
                self.done
                    .as_ref()
                    .filter(|(done, _)| done == root)
                    .map(|(_, done)| text(done).size(13).style(text::success))
            });
        let unreachable = match &self.latest {
            Some(Err(error)) if ours.is_none() => Some(
                text(format!("Could not ask GitHub for its releases: {error}"))
                    .size(13)
                    .style(text::secondary),
            ),
            _ => None,
        };

        let removal = self.removing.filter(|_| ours.is_some()).map(|delete| {
            container(
                column![
                    text("Remove Lumenveil from this client?"),
                    row![
                        checkbox(delete)
                            .label("Delete its settings and saved spots too")
                            .on_toggle(Message::DeleteSettings)
                            .text_size(13),
                        space::horizontal(),
                        button("Cancel")
                            .style(button::text)
                            .on_press(Message::CancelRemove),
                        button("Remove")
                            .style(button::danger)
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
            text("Graphics").size(14).font(style::BOLD),
            row![
                button(text("Lumenveil ↗").size(14))
                    .style(button::text)
                    .padding(0)
                    .on_press(Message::OpenPage),
                text("fog, lamp light, light shafts and water")
                    .size(13)
                    .style(text::secondary),
            ]
            .spacing(8)
            .align_y(Center),
            row![state, space::horizontal(), buttons]
                .spacing(8)
                .align_y(Center),
            column(
                problems
                    .into_iter()
                    .map(|(problem, tone)| text(problem).size(13).style(tone))
            )
            .spacing(4),
            fine.then(|| {
                text(
                    "In the game, Ctrl+F12 shows it on and off, and Ctrl+F7 opens its \
                     settings.",
                )
                .size(13)
                .style(text::secondary)
            }),
            unreachable,
            note,
            removal,
            text("Servers may forbid modified clients, and their anti-cheat may detect it.")
                .size(12)
                .style(text::secondary),
        ]
        .spacing(8)
        .width(Fill)
    }

    /// Why the files of the client folder at `root`, of `version`, weren't
    /// checked against their release, unless they are being.
    fn unchecked(&self, root: &Path, version: Option<&str>) -> (String, Tone) {
        let Some(version) = version else {
            return (
                "Its version can't be told: repairing installs the latest release.".to_owned(),
                text::warning,
            );
        };

        match self.sums.get(version) {
            _ if self.checking.contains_key(root) => {
                (format!("Checking version {version}…"), text::secondary)
            }
            Some(Err(error)) => (
                format!(
                    "Version {version} couldn't be checked: {error}. The launcher needs to check \
                     it before starting the game."
                ),
                text::danger,
            ),
            _ => (sentence(&Problem::Unchecked), text::warning),
        }
    }
}

/// What the launcher needs to do to Lumenveil, which a client folder holds
/// as `status` says, before starting the game, if anything.
fn need(status: &Status) -> Option<Need> {
    let problems = &status.problems;

    if problems.contains(&Problem::Foreign) {
        None
    } else if problems.iter().any(Problem::repairable) {
        Some(Need::Repair)
    } else if problems.contains(&Problem::Unchecked) {
        Some(status.version.clone().map_or(Need::Repair, Need::Check))
    } else {
        None
    }
}

/// What `problem` means, for people.
fn sentence(problem: &Problem) -> String {
    match problem {
        Problem::Foreign => "Another mod's version.dll is in the client folder: installing \
                             Lumenveil replaces it."
            .to_owned(),
        Problem::Missing(files) => format!("Missing: {}.", files.join(", ")),
        Problem::Changed(files) => format!("Not as released: {}.", files.join(", ")),
        Problem::Unchecked => "The launcher needs to check it before starting the game.".to_owned(),
        Problem::NoLamps => {
            "Lamps give no light yet: the setup that finds them hasn't run.".to_owned()
        }
        Problem::NoWater => {
            "The fog doesn't know where the water is yet: the setup that finds it hasn't run."
                .to_owned()
        }
        Problem::Direct3D9Ex => "The client draws with Direct3D 9Ex, which Lumenveil doesn't \
                                 draw on: repairing sets Direct3D 9 in Config.wtf."
            .to_owned(),
        Problem::WrongClient => "Its log says this Wow.exe isn't the 12340 build, or was \
                                 changed: Lumenveil stays off."
            .to_owned(),
        Problem::Shaders => "Its log says the graphics card or its driver refused its shaders: \
                             a newer driver, or DXVK, may help."
            .to_owned(),
    }
}

/// Checks the files of the client folder at `root` against `sums`, those of
/// the version installed there, and keeps them (see [`lumenveil::kept`]).
fn check(root: PathBuf, sums: Sums) -> Task<Message> {
    Task::perform(
        blocking({
            let root = root.clone();
            move || {
                let status = lumenveil::status(&root, Some(&sums));

                // Kept for the game to start with Lumenveil only while
                // nothing in it needs repairing, as checked here, even
                // where the launcher didn't install it.
                if status
                    .as_ref()
                    .is_some_and(|status| !status.problems.contains(&Problem::Foreign))
                    && let Err(error) = lumenveil::keep(&root, &sums)
                {
                    log::warn!("Could not keep the checksums of Lumenveil: {error}");
                }

                Ok(status)
            }
        }),
        Message::Checked.with(root),
    )
}

/// Installs the release `files` names in `installation`'s folder, in place
/// of what is there, downloading with `client`, or signed in to GitHub with
/// `github`, then runs the setup the way `launch` starts the game.
async fn work(
    client: reqwest::Client,
    github: Option<github::Signed>,
    installation: Installation,
    launch: Launch,
    files: Files,
) -> Result<(), String> {
    // Downloaded before anything changes.
    let (sums, archive) = download(&client, github, files).await?;

    let target = installation.clone();
    blocking(move || lumenveil::install(&target, &archive, &sums)).await?;

    tokio::task::spawn_blocking(move || set_up(&installation, &launch))
        .await
        .map_err(|error| error.to_string())?
}

/// The checksums and the zip of the release `files` names, downloaded with
/// `client`, or signed in to GitHub with `github`.
async fn download(
    client: &reqwest::Client,
    github: Option<github::Signed>,
    files: Files,
) -> Result<(Sums, Vec<u8>), String> {
    let release = match files {
        Files::Release(release) => release,
        Files::Version(version) => release(client.clone(), github.clone(), version).await?,
    };
    let sums = Sums::parse(&String::from_utf8_lossy(
        &get(client, github.as_ref(), &release.sums).await?,
    ));
    let archive = get(client, github.as_ref(), &release.archive).await?;

    Ok((sums, archive))
}

/// Runs the setup in `installation`'s folder, the way `launch` starts the
/// game, each run writing its output where `Lumenveil-Setup.cmd` does.
fn set_up(installation: &Installation, launch: &Launch) -> Result<(), String> {
    let locale = installation
        .locale()
        .ok_or("the client has no language data in its Data folder")?;

    for (arguments, log) in lumenveil::setup(locale) {
        let path = installation.root().join(log);
        let output = File::create(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let errors = output
            .try_clone()
            .map_err(|error| format!("{}: {error}", path.display()))?;

        let mut command = launch::tool(installation, launch, lumenveil::TOOL, &arguments)
            .map_err(|error| describe(&error))?;
        let _ = command.stdin(Stdio::null()).stdout(output).stderr(errors);
        // The tool is a console program: no window pops up for it.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = command.creation_flags(0x0800_0000);
        }

        log::info!("Running {} {}", lumenveil::TOOL, arguments.join(" "));
        let status = command
            .status()
            .map_err(|error| format!("could not run {}: {error}", lumenveil::TOOL))?;
        if !status.success() {
            return Err(format!(
                "the setup failed; {log} in the client folder says why"
            ));
        }
    }

    Ok(())
}

/// The release of `version`, or the latest, as GitHub lists it.
async fn release(
    client: reqwest::Client,
    github: Option<github::Signed>,
    version: Option<String>,
) -> Result<Release, String> {
    let url = match version {
        Some(version) => format!("{RELEASES}/tags/v{version}"),
        None => format!("{RELEASES}/latest"),
    };

    parse(&get(&client, github.as_ref(), &url).await?)
}

/// The checksums of the release of `version`.
async fn checksums(
    client: reqwest::Client,
    github: Option<github::Signed>,
    version: String,
) -> Result<Sums, String> {
    let release = release(client.clone(), github.clone(), Some(version)).await?;
    let sums = get(&client, github.as_ref(), &release.sums).await?;

    Ok(Sums::parse(&String::from_utf8_lossy(&sums)))
}

/// A release, from what GitHub's API says of it.
fn parse(json: &[u8]) -> Result<Release, String> {
    #[derive(Deserialize)]
    struct Answer {
        tag_name: String,
        assets: Vec<Asset>,
    }

    #[derive(Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
    }

    let answer: Answer = serde_json::from_slice(json)
        .map_err(|error| format!("GitHub's answer doesn't read: {error}"))?;
    let asset = |wanted: &dyn Fn(&str) -> bool| {
        answer
            .assets
            .iter()
            .find(|asset| wanted(&asset.name))
            .map(|asset| asset.browser_download_url.clone())
    };

    Ok(Release {
        version: answer.tag_name.trim_start_matches('v').to_owned(),
        archive: asset(&|name| name.ends_with(".zip")).ok_or("the release has no zip")?,
        sums: asset(&|name| name == "SHA256SUMS.txt").ok_or("the release has no SHA256SUMS.txt")?,
    })
}

/// Downloads `url` with `client`, or signed in to GitHub with `github`.
async fn get(
    client: &reqwest::Client,
    github: Option<&github::Signed>,
    url: &str,
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
    if response
        .content_length()
        .is_some_and(|length| length > LARGEST)
    {
        return Err(format!("{url} is larger than a release of Lumenveil is"));
    }

    let bytes = response.bytes().await.map_err(|error| describe(&error))?;

    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use iced_test::simulator;

    use super::*;

    const ROOT: &str = "/games/wotlk";

    fn status(version: Option<&str>, problems: Vec<Problem>) -> Status {
        Status {
            version: version.map(str::to_owned),
            loaded: true,
            problems,
        }
    }

    fn latest(version: &str) -> State {
        State {
            latest: Some(Ok(Release {
                version: version.to_owned(),
                archive: format!("https://example.com/Lumenveil-{version}.zip"),
                sums: "https://example.com/SHA256SUMS.txt".to_owned(),
            })),
            ..State::default()
        }
    }

    /// `state`, having found `found` in the client folder at [`ROOT`].
    fn found(mut state: State, found: Option<Status>) -> State {
        let _ = state.found.insert(ROOT.into(), found);
        state
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

    /// Handles `message` for the client at [`ROOT`], downloading with
    /// `client`.
    fn update(
        state: &mut State,
        message: Message,
        client: Result<&reqwest::Client, &str>,
    ) -> Action<Instruction, Message> {
        state.update(
            message,
            Some(Path::new(ROOT)),
            None,
            &Config::default(),
            false,
            client,
            &mut Errors::default(),
        )
    }

    #[test]
    fn releases_are_read_from_githubs_answer() {
        let release = parse(
            br#"{"tag_name": "v0.1.1", "assets": [
                {"name": "SHA256SUMS.txt", "browser_download_url": "https://github.com/s"},
                {"name": "Lumenveil-0.1.1.zip", "browser_download_url": "https://github.com/z"}
            ]}"#,
        )
        .unwrap();

        assert_eq!(
            release,
            Release {
                version: "0.1.1".to_owned(),
                archive: "https://github.com/z".to_owned(),
                sums: "https://github.com/s".to_owned(),
            }
        );
        assert!(parse(br#"{"tag_name": "v1", "assets": []}"#).is_err());
    }

    #[test]
    fn a_client_without_it_offers_the_latest_release_and_says_the_risk() {
        let state = found(latest("0.1.1"), None);

        for wanted in [
            "Not installed.",
            "Install 0.1.1",
            "Servers may forbid modified clients, and their anti-cheat may detect it.",
        ] {
            assert!(shows(&state, false, wanted), "{wanted}");
        }
        assert!(state.banner(Path::new(ROOT), false).is_none());

        let state = found(state, Some(status(None, vec![Problem::Foreign])));
        assert!(shows(&state, false, "Install 0.1.1"));
        assert!(shows(
            &state,
            false,
            "Another mod's version.dll is in the client folder: installing Lumenveil replaces it."
        ));
        assert!(
            state.banner(Path::new(ROOT), false).is_none(),
            "not Lumenveil"
        );
    }

    #[test]
    fn what_keeps_it_from_working_is_said_and_repairing_fixes_what_it_can() {
        let broken = status(
            Some("0.1.1"),
            vec![
                Problem::Changed(vec!["version.dll".to_owned()]),
                Problem::NoLamps,
                Problem::Direct3D9Ex,
                Problem::WrongClient,
            ],
        );
        let state = found(latest("0.1.2"), Some(broken.clone()));

        for wanted in [
            "Version 0.1.1 installed; 0.1.2 is out.",
            "Update to 0.1.2",
            "Repair",
            "Uninstall",
            "Not as released: version.dll.",
            "Lamps give no light yet: the setup that finds them hasn't run.",
            "Its log says this Wow.exe isn't the 12340 build, or was changed: Lumenveil stays off.",
        ] {
            assert!(shows(&state, false, wanted), "{wanted}");
        }

        // Above the addons, only what the launcher can repair, which it
        // needs to before starting the game.
        assert!(banner_shows(
            &state,
            false,
            "The launcher needs to repair Lumenveil before starting the game. Not as released: \
             version.dll. Lamps give no light yet: the setup that finds them hasn't run. The \
             client draws with Direct3D 9Ex, which Lumenveil doesn't draw on: repairing sets \
             Direct3D 9 in Config.wtf."
        ));
        assert!(!banner_shows(&state, false, "Not now"));
        assert!(state.holds_back(Path::new(ROOT)));
        assert!(matches!(
            press(&state, false, "Repair")[..],
            [Message::Repair]
        ));
        assert!(press(&state, true, "Repair").is_empty(), "the game runs");

        let state = found(state, Some(status(Some("0.1.2"), vec![Problem::Shaders])));
        assert!(!shows(&state, false, "Repair"));
        assert!(state.banner(Path::new(ROOT), false).is_none());
        assert!(!state.holds_back(Path::new(ROOT)), "nothing to repair");
    }

    #[test]
    fn found_unchecked_its_files_are_checked_against_its_release_on_request() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = crate::http_client().build().unwrap();
        let mut state = found(
            latest("0.1.1"),
            Some(status(Some("0.1.1"), vec![Problem::Unchecked])),
        );

        assert!(banner_shows(
            &state,
            false,
            "Lumenveil is in this client. The launcher needs to check it before starting the \
             game."
        ));
        assert!(!banner_shows(&state, false, "Not now"));
        assert!(state.holds_back(Path::new(ROOT)));
        assert!(shows(
            &state,
            false,
            "The launcher needs to check it before starting the game."
        ));
        assert!(
            matches!(press(&state, true, "Check")[..], [Message::Check]),
            "checking writes nothing the game reads"
        );

        let asking = update(&mut state, Message::Check, Ok(&client));
        assert!(asking.task.units() > 0, "GitHub is asked for its checksums");
        assert!(banner_shows(&state, false, "Checking Lumenveil…"));
        assert!(shows(&state, false, "Checking version 0.1.1…"));

        let _ = update(
            &mut state,
            Message::Sums("0.1.1".to_owned(), Err("GitHub is down".to_owned())),
            Ok(&client),
        );
        assert!(banner_shows(
            &state,
            false,
            "Lumenveil couldn't be checked: GitHub is down. The launcher needs to check it \
             before starting the game."
        ));
        assert!(state.holds_back(Path::new(ROOT)));
        let asking = update(&mut state, Message::Check, Ok(&client));
        assert!(asking.task.units() > 0, "asked again");

        let as_released = status(Some("0.1.1"), Vec::new());
        let _ = update(
            &mut state,
            Message::Checked(ROOT.into(), Ok(Some(as_released))),
            Ok(&client),
        );
        assert!(!state.holds_back(Path::new(ROOT)));
        assert!(banner_shows(
            &state,
            false,
            "Lumenveil is checked: its files are as released."
        ));
        assert!(matches!(
            press(&state, false, "Dismiss")[..],
            [Message::Dismiss]
        ));
        let _ = update(&mut state, Message::Dismiss, Ok(&client));
        assert!(state.banner(Path::new(ROOT), false).is_none());
    }

    #[test]
    fn found_of_a_version_that_cant_be_told_repairing_installs_the_latest() {
        let state = found(
            latest("0.1.1"),
            Some(status(None, vec![Problem::Unchecked])),
        );

        assert!(banner_shows(
            &state,
            false,
            "The launcher needs to repair Lumenveil before starting the game. Its version can't \
             be told: repairing installs the latest release."
        ));
        assert!(shows(
            &state,
            false,
            "Its version can't be told: repairing installs the latest release."
        ));
        assert!(state.holds_back(Path::new(ROOT)));
        assert!(matches!(
            press(&state, false, "Repair")[..],
            [Message::Repair]
        ));
    }

    #[test]
    fn nothing_is_done_while_the_game_runs() {
        let mut state = latest("0.1.1");
        let (_root, installation) = crate::core::testing::client(&[]);
        let root = installation.root().to_path_buf();
        let _ = state.found.insert(root.clone(), None);

        let mut ui = simulator(state.view(&root, true));
        let _ = ui.click("Install 0.1.1").unwrap();
        assert!(ui.into_messages().next().is_none());

        let _ = state.update(
            Message::Install,
            Some(&root),
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
        let mut state = latest("0.1.1");
        let (_root, installation) =
            crate::core::testing::client(&["CoAVolFog.dll", "CoAVolFog.ini"]);
        let root = installation.root().to_path_buf();
        let _ = state
            .found
            .insert(root.clone(), Some(status(Some("0.1.1"), Vec::new())));
        let mut errors = Errors::default();
        let mut update = |state: &mut State, message: Message| {
            state.update(
                message,
                Some(&root),
                Some(&installation),
                &Config::default(),
                false,
                Err("offline"),
                &mut errors,
            )
        };

        let messages: Vec<Message> = {
            let mut ui = simulator(state.view(&root, false));
            let _ = ui.click("Uninstall").unwrap();
            ui.into_messages().collect()
        };
        assert!(matches!(messages[..], [Message::Remove]));
        let _ = update(&mut state, Message::Remove);

        let messages: Vec<Message> = {
            let mut ui = simulator(state.view(&root, false));
            assert!(ui.find("Remove Lumenveil from this client?").is_ok());
            let _ = ui.click("Delete its settings and saved spots too").unwrap();
            ui.into_messages().collect()
        };
        assert!(matches!(messages[..], [Message::DeleteSettings(true)]));
        let _ = update(&mut state, Message::DeleteSettings(true));

        let action = update(&mut state, Message::ConfirmRemove);
        assert!(state.busy.is_some(), "removing");
        assert!(action.instruction.is_none());
    }

    #[tokio::test]
    #[ignore = "downloads Lumenveil's latest release from GitHub"]
    async fn the_latest_release_installs_into_a_client() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = crate::http_client().build().unwrap();
        let (_root, installation) = crate::core::testing::client(&[]);

        let latest = release(client.clone(), None, None).await.unwrap();
        let (sums, archive) = download(&client, None, Files::Release(latest.clone()))
            .await
            .unwrap();
        lumenveil::install(&installation, &archive, &sums).unwrap();

        // As GitHub gives them for its version.
        let sums = checksums(client, None, latest.version.clone())
            .await
            .unwrap();
        let status = lumenveil::status(installation.root(), Some(&sums)).unwrap();
        assert_eq!(status.version.as_deref(), Some(latest.version.as_str()));
        assert_eq!(
            status.problems,
            [Problem::NoLamps, Problem::NoWater],
            "the setup is left to run"
        );
        assert_eq!(lumenveil::kept(installation.root()), Some(sums));
        assert!(
            installation
                .root()
                .join("Lumenveil/lamp_extract.exe")
                .is_file()
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn the_setup_runs_the_tool_the_way_the_game_starts_and_keeps_its_output() {
        use std::fs;

        let (_root, installation) = crate::core::testing::client(&["Data/esES/locale-esES.MPQ"]);
        fs::create_dir_all(installation.root().join("Lumenveil")).unwrap();
        let launch = Launch {
            env: "WHO=tool".to_owned(),
            command: "echo $WHO %command%".to_owned(),
        };

        set_up(&installation, &launch).unwrap();

        let lamps =
            fs::read_to_string(installation.root().join("Lumenveil/setup-lamps.log")).unwrap();
        assert!(
            lamps.starts_with("tool ") && lamps.ends_with(
                "Lumenveil/lamp_extract.exe --data Data --out lampdata.bin --locale esES --background\n"
            ),
            "{lamps}"
        );

        let failing = Launch {
            env: String::new(),
            command: "false %command%".to_owned(),
        };
        assert_eq!(
            set_up(&installation, &failing),
            Err(
                "the setup failed; Lumenveil/setup-lamps.log in the client folder says why"
                    .to_owned()
            )
        );
    }
}
