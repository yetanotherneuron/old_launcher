//! Every addon in one list: the catalog's, installed or not, and the ones
//! installed without it, searchable, filterable and installable.

mod card;
mod confirm;

pub use confirm::Confirm;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use iced::futures::StreamExt;
use iced::task::{Straw, sipper};
use iced::widget::operation::Animation;
use iced::widget::operation::scrollable::{RelativeOffset, snap_to};
use iced::widget::{
    Button, button, center, column, container, pick_list, row, scrollable, space, text, text_input,
};
use iced::{Center, Element, Fill, FillPortion, Function, Task, Widget, padding};

use crate::action::Action;
use crate::catalog::{self, Catalog, Entry};
use crate::core::catalog::Download;
use crate::core::library::{Filter, Library, Order, Origin, Status};
use crate::core::search::Query;
use crate::core::{Installation, Snapshot};
use crate::errors::Errors;
use crate::{blocking, describe, game, github, gitlab, style};

/// The id of the search field, for keyboard focus.
pub const SEARCH: &str = "addons-search";

/// The id of the list, to scroll it back up when the filters change.
const LIST: &str = "addons-list";

/// The addons screen's state.
pub struct State {
    query: String,
    filter: Filter,
    jobs: HashMap<Key, Job>,
    confirm: Option<Confirm>,
    /// Whether a confirmed deletion is running.
    working: bool,
    /// How downloads sign in to GitHub, when they do.
    github: Option<github::Signed>,
}

/// Identifies a catalog entry across catalog refreshes: its
/// [`key`](Entry::key), the folders it installs.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key(String);

/// Where an entry installs from: the source the catalog lists first, or
/// one the user picks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// The catalog's order: the newest first.
    Auto,
    /// This source, by id, first.
    Source(String),
}

/// Archives bigger than this are not downloaded: no addon comes close, and
/// a server claiming more must not have the launcher set aside the memory.
const LARGEST_ARCHIVE: u64 = 512 * 1024 * 1024;

/// An installation in progress.
struct Job {
    /// The name of the entry being installed, to report a failure with.
    name: String,
    stage: Stage,
    /// Set to stop the download. Once extracting, the job runs to its end
    /// either way: folders are already moving.
    cancel: Arc<AtomicBool>,
}

/// How far an installation has come.
#[derive(Debug, Clone)]
pub enum Stage {
    /// Downloading the archive.
    Downloading {
        /// The source it comes from.
        source: String,
        /// Bytes received so far.
        received: u64,
        /// The archive size, when the server announces it.
        total: Option<u64>,
    },
    /// Extracting and moving folders into place.
    Installing,
    /// Asked to stop, and about to.
    Cancelling,
}

/// Why an installation didn't happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The user stopped it while downloading.
    Cancelled,
    /// The download or the install failed; this says how, naming the
    /// source.
    Failed(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("stopped"),
            Self::Failed(why) => f.write_str(why),
        }
    }
}

/// Interactions with the addons screen.
#[derive(Debug, Clone)]
pub enum Message {
    /// The search field was edited.
    QueryChanged(String),
    /// Show addons by whether and how they are installed.
    StatusSelected(Status),
    /// Show addons by where they come from.
    OriginSelected(Origin),
    /// Sort the addons.
    OrderSelected(Order),
    /// Install an entry from this source first, from now on.
    SourceSelected(Key, Choice),
    /// Show everything again.
    ClearFilters,
    /// Fetch the published catalog again.
    Reload,
    /// Install (or reinstall) an entry into the selected client.
    Install(Key),
    /// Stop a download in progress.
    Stop(Key),
    /// An installation advanced.
    Progressed(Key, Stage),
    /// An installation ended, with the folders it installed.
    Finished(Key, Result<Vec<String>, Failure>),
    /// Ask before deleting something.
    Ask(Confirm),
    /// Choose whether uninstalling deletes the saved settings too.
    DeleteData(bool),
    /// Close the confirmation without deleting.
    Cancel,
    /// Go ahead with the pending deletion.
    Confirm,
    /// The deletion ended; a failure says what failed.
    Done(Result<(), String>),
    /// Open a web page about an addon or its source.
    OpenLink(String),
}

/// What the addons screen asks of its parent.
pub enum Instruction {
    /// Files in the client changed.
    Changed,
    /// The user wants a fresh catalog.
    Reload,
    /// The user picked where the entry installs from: this source, or
    /// none for the catalog's order. To remember, and to apply.
    Prefer {
        /// The entry.
        key: Key,
        /// The source id, or none.
        source: Option<String>,
    },
}

impl Key {
    /// The key of `entry`.
    pub fn of(entry: &Entry) -> Self {
        Self(entry.key())
    }

    /// The key as the settings name it.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl State {
    /// An addons screen showing everything, with nothing going on.
    pub fn new() -> Self {
        Self {
            query: String::new(),
            filter: Filter::default(),
            jobs: HashMap::new(),
            confirm: None,
            working: false,
            github: None,
        }
    }

    /// Makes the downloads started from now on sign in to GitHub this way,
    /// or not at all.
    pub fn sign_in(&mut self, github: Option<github::Signed>) {
        self.github = github;
    }

    /// Handles an interaction. Changes happen in `installation`, whose saved
    /// settings are left alone while its game runs (`playing`): the game
    /// writes them back. Installs download with `client` (or say why there
    /// is none) and replace what `snapshot` says the same entry installed
    /// before. Whatever fails goes to `errors`.
    #[expect(
        clippy::too_many_arguments,
        reason = "each is a dependency of its own, as in the other screens"
    )]
    pub fn update(
        &mut self,
        message: Message,
        catalog: &Catalog,
        client: Result<&reqwest::Client, &str>,
        installation: Option<&Installation>,
        snapshot: Option<&Snapshot>,
        playing: bool,
        errors: &mut Errors,
    ) -> Action<Instruction, Message> {
        match message {
            Message::QueryChanged(query) => {
                self.query = query;
                Action::task(to_top())
            }
            Message::StatusSelected(status) => {
                self.filter.status = status;
                Action::task(to_top())
            }
            Message::OriginSelected(origin) => {
                self.filter.origin = origin;
                Action::task(to_top())
            }
            Message::OrderSelected(order) => {
                self.filter.order = order;
                Action::task(to_top())
            }
            Message::SourceSelected(key, choice) => {
                let source = match choice {
                    Choice::Auto => None,
                    Choice::Source(source) => Some(source),
                };

                Action::instruction(Instruction::Prefer { key, source })
            }
            Message::ClearFilters => {
                self.query.clear();
                self.filter = Filter::default();
                Action::task(to_top())
            }
            Message::Reload => {
                log::info!("Reloading the catalog on request");
                Action::instruction(Instruction::Reload)
            }
            Message::Install(key) => {
                let Some(installation) = installation else {
                    return Action::none();
                };
                let Some(entry) = catalog.entries.iter().find(|entry| Key::of(entry) == key) else {
                    return Action::none();
                };
                if self.jobs.contains_key(&key) {
                    return Action::none();
                }
                let client = match client {
                    Ok(client) => client,
                    Err(error) => {
                        errors.report(unavailable(error));
                        return Action::none();
                    }
                };

                let replacing = snapshot
                    .map(|snapshot| {
                        snapshot
                            .installed(entry)
                            .into_iter()
                            .map(|addon| addon.name.clone())
                            .collect()
                    })
                    .unwrap_or_default();

                let Some(download) = entry.preferred().cloned() else {
                    return Action::none();
                };
                let source = catalog
                    .source(&download.source)
                    .map_or_else(|| download.source.clone(), |source| source.name.clone());
                let cancel = Arc::new(AtomicBool::new(false));

                log::info!(
                    "Installing {} into {}",
                    entry.name,
                    installation.root().display()
                );

                let task = Task::sip(
                    install(
                        client.clone(),
                        self.github.clone(),
                        source.clone(),
                        download,
                        installation.clone(),
                        replacing,
                        Arc::clone(&cancel),
                    ),
                    Message::Progressed.with(key.clone()),
                    Message::Finished.with(key.clone()),
                );

                let _ = self.jobs.insert(
                    key,
                    Job {
                        name: entry.name.clone(),
                        stage: Stage::Downloading {
                            source,
                            received: 0,
                            total: None,
                        },
                        cancel,
                    },
                );

                Action::task(task)
            }
            Message::Stop(key) => {
                // Only downloads stop; once installing, folders are already
                // moving and the job runs to its end. The job stays until it
                // says it stopped, so nothing is ever left unaccounted for.
                if let Some(job) = self.jobs.get_mut(&key)
                    && matches!(job.stage, Stage::Downloading { .. })
                {
                    job.cancel.store(true, Ordering::Relaxed);
                    job.stage = Stage::Cancelling;
                }

                Action::none()
            }
            Message::Progressed(key, progress) => {
                if let Some(job) = self.jobs.get_mut(&key)
                    && !matches!(job.stage, Stage::Cancelling)
                {
                    job.stage = progress;
                }

                Action::none()
            }
            Message::Finished(key, result) => {
                let job = self.jobs.remove(&key);
                let name = job.map_or_else(|| "an addon".to_owned(), |job| job.name);

                match result {
                    Ok(folders) => log::info!("Installed {name}: {}", folders.join(", ")),
                    Err(Failure::Cancelled) => {
                        log::info!("Stopped downloading {name}");
                        return Action::none();
                    }
                    Err(Failure::Failed(error)) => {
                        errors.report(format!("Could not install {name}: {error}"));
                    }
                }

                // Even a failed install may have removed the previous version.
                Action::instruction(Instruction::Changed)
            }
            Message::Ask(confirm) => {
                if !self.working {
                    self.confirm = Some(confirm);
                }
                Action::none()
            }
            Message::DeleteData(delete) => {
                if let Some(Confirm::Uninstall { delete_data, .. }) = &mut self.confirm {
                    *delete_data = delete && !playing;
                }
                Action::none()
            }
            Message::Cancel => {
                self.confirm = None;
                Action::none()
            }
            Message::Confirm => {
                let (Some(mut confirm), Some(installation)) = (self.confirm.take(), installation)
                else {
                    return Action::none();
                };

                // As the dialog shows it while the game runs.
                if playing && !confirm.while_playing() {
                    self.confirm = Some(confirm);
                    return Action::none();
                }
                let installation = installation.clone();
                let failure = confirm.failure();

                log::info!("{}", confirm.describe());
                self.working = true;

                Action::task(Task::perform(
                    blocking(move || confirm.run(&installation)),
                    move |result| {
                        Message::Done(result.map_err(|error| format!("{failure}: {error}")))
                    },
                ))
            }
            Message::Done(result) => {
                self.working = false;

                match result {
                    Ok(()) => log::info!("Done"),
                    Err(error) => errors.report(error),
                }

                Action::instruction(Instruction::Changed)
            }
            Message::OpenLink(url) => {
                if let Err(error) = open::that_detached(&url) {
                    errors.report(format!("Could not open {url}: {error}"));
                }

                Action::none()
            }
        }
    }

    /// The `catalog` and the addons of `installation`, as last scanned
    /// (`snapshot`). Nothing can be installed until both are there, and no
    /// saved settings deleted while its game runs (`playing`).
    pub fn view<'a>(
        &'a self,
        catalog: &'a Catalog,
        installation: Option<&'a Installation>,
        snapshot: Option<&'a Snapshot>,
        refresh: catalog::Refresh,
        playing: bool,
    ) -> impl Widget<Message> + 'a {
        let library = Library::new(catalog, snapshot);
        let query = Query::new(&self.query);
        let can_install = installation.is_some() && snapshot.is_some();

        let leftovers: Vec<(String, &'a [PathBuf])> = snapshot
            .map(Snapshot::orphaned_data)
            .unwrap_or_default()
            .into_iter()
            .filter(|(name, _)| query.matches(&[name.as_str()]))
            .collect();

        // Each status says how many addons it would show, given the search
        // and the source picked, so the one picked counts what is listed.
        let mut counts = library.counts(&query, &self.filter);
        for (status, count) in &mut counts {
            if *status == Status::LeftoverData {
                *count = leftovers.len();
            }
        }

        let status = pick_list(Some(self.filter.status), Status::ALL, move |status| {
            let count = counts
                .iter()
                .find(|(counted, _)| counted == status)
                .map_or(0, |(_, count)| *count);
            let label = match status {
                Status::All => "All addons",
                Status::Installed => "Installed",
                Status::Updatable => "Updates available",
                Status::NotInstalled => "Not installed",
                Status::Attention => "Needs attention",
                Status::LeftoverData => "Leftover data",
            };

            format!("{label} ({count})")
        })
        .on_select(Message::StatusSelected)
        .padding(10)
        .width(Fill.max(200))
        .ellipsis(text::Ellipsis::End);

        let mut sources: Vec<_> = catalog.sources.iter().collect();
        sources.sort_by_cached_key(|source| source.name.to_lowercase());

        let origins: Vec<Origin> = [Origin::Any]
            .into_iter()
            .chain(
                sources
                    .into_iter()
                    .map(|source| Origin::Source(source.id.clone())),
            )
            .chain([Origin::Manual])
            .collect();

        let origin = pick_list(Some(&self.filter.origin), origins, |origin| match origin {
            Origin::Any => "All sources".to_owned(),
            Origin::Source(id) => catalog
                .source(id)
                .map_or_else(|| id.clone(), |source| source.name.clone()),
            Origin::Manual => "Installed manually".to_owned(),
        })
        .on_select(Message::OriginSelected)
        .padding(10)
        .width(Fill.max(220))
        .ellipsis(text::Ellipsis::End);

        let order = pick_list(Some(self.filter.order), Order::ALL, |order| {
            match order {
                Order::Name => "Sort by name",
                Order::Updated => "Recently updated",
            }
            .to_owned()
        })
        .on_select(Message::OrderSelected)
        .padding(10)
        .width(Fill.max(180))
        .ellipsis(text::Ellipsis::End);

        let toolbar = row![
            // Twice the room of a dropdown when space is short.
            text_input("Search addons…", &self.query)
                .id(SEARCH)
                .on_input(Message::QueryChanged)
                .padding(10)
                .width(FillPortion(2)),
            status,
            origin,
            order,
        ]
        .spacing(8)
        .align_y(Center);

        let filtered = !query.is_empty() || self.filter.origin != Origin::Any;
        let needs_client = matches!(
            self.filter.status,
            Status::Installed | Status::Attention | Status::LeftoverData
        );

        let list: Element<'a, Message> = if needs_client && snapshot.is_none() {
            empty(
                if installation.is_some() {
                    "Reading Interface/AddOns…"
                } else {
                    "Select a client to see its addons."
                },
                None,
            )
            .boxed()
        } else if self.filter.status == Status::LeftoverData {
            self.leftovers(leftovers, filtered, playing)
        } else {
            let shown = library.shown(&query, &self.filter);

            if shown.is_empty() {
                let (message, action) = match self.filter.status {
                    _ if filtered => ("No addon matches.", Some(CLEAR)),
                    Status::All => match refresh {
                        catalog::Refresh::Running => ("Fetching the list of addons…", None),
                        catalog::Refresh::Idle => (
                            "No addons listed yet: check the connection and try again.",
                            Some(("Try again", Message::Reload)),
                        ),
                    },
                    Status::Installed => ("No addons installed yet.", None),
                    Status::Updatable => ("Every installed addon is up to date.", None),
                    Status::NotInstalled => ("Every addon available is installed.", None),
                    Status::Attention => ("Every installed addon loads fine.", None),
                    Status::LeftoverData => ("No leftover data.", None),
                };

                empty(message, action).boxed()
            } else {
                scrollable(
                    column(shown.into_iter().map(|item| {
                        self.card(&library, item, catalog, snapshot, can_install, playing)
                    }))
                    .spacing(8)
                    .padding(padding::right(14)),
                )
                .id(LIST)
                .height(Fill)
                .boxed()
            }
        };

        let content = column![toolbar, container(list).height(Fill)].spacing(12);

        style::modal(
            content,
            self.confirm
                .as_ref()
                .zip(installation)
                .map(|(confirm, installation)| confirm.view(installation.root(), playing)),
            Message::Cancel,
        )
    }

    /// The data that removed addons left behind, by addon name, which can't
    /// be deleted while the game runs (`playing`).
    fn leftovers<'a>(
        &self,
        leftovers: Vec<(String, &'a [PathBuf])>,
        filtered: bool,
        playing: bool,
    ) -> Element<'a, Message> {
        if leftovers.is_empty() {
            return empty(
                if filtered {
                    "No leftover data matches."
                } else {
                    "No leftover data."
                },
                filtered.then_some(CLEAR),
            )
            .boxed();
        }

        let all: Vec<PathBuf> = leftovers
            .iter()
            .flat_map(|(_, files)| files.iter().cloned())
            .collect();
        let deletable = !self.working && !playing;
        // Why the buttons are off, when it's the game.
        let explained = |button: Button<'a, Message, &'static str>| -> Element<'a, Message> {
            if playing {
                style::tip(button, game::KEEPS_SETTINGS).boxed()
            } else {
                button.boxed()
            }
        };

        let header =
            row![
                column![
                    text("Leftover data").size(17).font(style::BOLD),
                    text("Saved settings of addons that are no longer installed.")
                        .size(13)
                        .style(text::secondary),
                ]
                .spacing(2),
                space::horizontal(),
                explained(button("Delete all").style(button::danger).on_press_maybe(
                    deletable.then(|| {
                        Message::Ask(Confirm::Clean {
                            title: "all leftover data".to_owned(),
                            files: all,
                        })
                    }),
                )),
            ]
            .align_y(Center);

        let rows =
            leftovers.into_iter().map(|(name, files)| {
                container(
                    row![
                        text(name.clone()).width(Fill),
                        text(if files.len() == 1 {
                            "1 file".to_owned()
                        } else {
                            format!("{} files", files.len())
                        })
                        .size(13)
                        .style(text::secondary),
                        explained(button("Delete").style(button::text).on_press_maybe(
                            deletable.then(|| {
                                Message::Ask(Confirm::Clean {
                                    title: name,
                                    files: files.to_vec(),
                                })
                            }),
                        )),
                    ]
                    .spacing(12)
                    .align_y(Center),
                )
                .padding([6, 14])
                .style(style::card)
            });

        scrollable(
            column![header, column(rows).spacing(8)]
                .spacing(8)
                .padding(padding::right(14)),
        )
        .id(LIST)
        .height(Fill)
        .boxed()
    }
}

/// The way back to everything, for a list empty because of a search or
/// filter: what [`empty`] offers then.
const CLEAR: (&str, Message) = ("Clear filters", Message::ClearFilters);

/// A centered `message` for an empty list, with a button offering a way
/// out (`action`: its label, and what pressing it sends) when there is one.
fn empty<'a>(message: &'a str, action: Option<(&'a str, Message)>) -> impl Widget<Message> + 'a {
    center(
        column![
            text(message).style(text::secondary),
            action.map(|(label, message)| button(label).style(button::secondary).on_press(message)),
        ]
        .spacing(12)
        .align_x(Center),
    )
}

/// What the banner says when there is no HTTP client to download with, and
/// `error` is why.
pub fn unavailable(error: &str) -> String {
    format!("Downloads are unavailable: {error}")
}

/// Scrolls the list back to the top, for a list that changed contents.
fn to_top() -> Task<Message> {
    snap_to(LIST, RelativeOffset::START, Animation::Instant)
}

/// Installs `download`, from the source named `source`, into
/// `installation`, replacing the folders in `replacing`, and reports
/// progress. It downloads with `client`, or signed in to GitHub with
/// `github` when there is a login. Nothing else is tried when it fails: the
/// failure names the source and says what went wrong. Setting `cancel` stops
/// it while it is still downloading.
fn install(
    client: reqwest::Client,
    github: Option<github::Signed>,
    source: String,
    download: Download,
    installation: Installation,
    replacing: Vec<String>,
    cancel: Arc<AtomicBool>,
) -> impl Straw<Vec<String>, Stage, Failure> {
    sipper(async move |mut progress| {
        let attempt = async {
            let response = match &github {
                Some(signed) => signed.get(&download.url).await.map_err(Failure::Failed)?,
                None => client
                    .get(&download.url)
                    .send()
                    .await
                    .map_err(|error| Failure::Failed(describe(&error)))?,
            };

            if let Some(refusal) =
                github::refusal(&response, github.is_some()).or_else(|| gitlab::refusal(&response))
            {
                return Err(Failure::Failed(refusal.to_owned()));
            }

            let response = response
                .error_for_status()
                .map_err(|error| Failure::Failed(describe(&error)))?;

            let total = response.content_length();
            if let Some(total) = total.filter(|total| *total > LARGEST_ARCHIVE) {
                return Err(Failure::Failed(format!(
                    "the archive is {}, more than the launcher downloads",
                    crate::size(total)
                )));
            }

            let mut archive = Vec::with_capacity(total.unwrap_or_default() as usize);
            let mut reported = 0;
            let mut chunks = response.bytes_stream();

            progress
                .send(Stage::Downloading {
                    source: source.clone(),
                    received: 0,
                    total,
                })
                .await;

            while let Some(chunk) = chunks.next().await {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Failure::Cancelled);
                }

                archive
                    .extend_from_slice(&chunk.map_err(|error| Failure::Failed(describe(&error)))?);
                if archive.len() as u64 > LARGEST_ARCHIVE {
                    return Err(Failure::Failed(format!(
                        "the archive is more than {}, which the launcher doesn't download",
                        crate::size(LARGEST_ARCHIVE)
                    )));
                }

                // Reporting every chunk would redraw the UI thousands of times.
                if archive.len() - reported >= 64 * 1024 {
                    reported = archive.len();
                    progress
                        .send(Stage::Downloading {
                            source: source.clone(),
                            received: reported as u64,
                            total,
                        })
                        .await;
                }
            }

            if cancel.load(Ordering::Relaxed) {
                return Err(Failure::Cancelled);
            }

            progress.send(Stage::Installing).await;

            let marker = download.marker();

            blocking(move || installation.install(&archive, &marker, &replacing))
                .await
                .map_err(Failure::Failed)
        };

        attempt.await.map_err(|failure| match failure {
            Failure::Failed(error) => Failure::Failed(format!("{source}: {error}")),
            Failure::Cancelled => Failure::Cancelled,
        })
    })
}

#[cfg(test)]
mod tests;
