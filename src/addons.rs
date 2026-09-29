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
use iced::widget::operation::{self, Animation, RelativeOffset};
use iced::widget::{
    button, center, column, container, pick_list, row, scrollable, space, text, text_input,
};
use iced::{Center, Element, Fill, FillPortion, Function, Task, padding};

use crate::action::Action;
use crate::catalog::{self, Catalog, Entry};
use crate::core::catalog::Download;
use crate::core::library::{Filter, Library, Order, Origin, Status};
use crate::core::search::Query;
use crate::core::{Installation, Snapshot};
use crate::errors::Errors;
use crate::style;
use crate::{blocking, describe};

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
}

/// Identifies a catalog entry across catalog refreshes: the folders it
/// installs, which the catalog never gives two entries.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key(Vec<String>);

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
    /// Every download failed, or the install did; this says how.
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
}

impl Key {
    /// The key of `entry`.
    pub fn of(entry: &Entry) -> Self {
        let mut folders: Vec<String> = entry
            .folders()
            .iter()
            .map(|folder| folder.name.to_lowercase())
            .collect();
        folders.sort();

        Self(folders)
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
        }
    }

    /// Handles an interaction. Changes happen in `installation`; installs
    /// download with `client` (or say why there is none) and replace what
    /// `snapshot` says the same entry installed before. Whatever fails goes
    /// to `errors`.
    pub fn update(
        &mut self,
        message: Message,
        catalog: &Catalog,
        client: Result<&reqwest::Client, &str>,
        installation: Option<&Installation>,
        snapshot: Option<&Snapshot>,
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
            Message::ClearFilters => {
                self.query.clear();
                self.filter = Filter::default();
                Action::task(to_top())
            }
            Message::Reload => Action::instruction(Instruction::Reload),
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

                let downloads: Vec<(String, Download)> = entry
                    .downloads
                    .iter()
                    .map(|download| {
                        let source = catalog
                            .source(&download.source)
                            .map_or_else(|| download.source.clone(), |source| source.name.clone());

                        (source, download.clone())
                    })
                    .collect();
                let first = downloads
                    .first()
                    .map(|(source, _)| source.clone())
                    .unwrap_or_default();
                let cancel = Arc::new(AtomicBool::new(false));

                log::info!(
                    "Installing {} into {}",
                    entry.name,
                    installation.root().display()
                );

                let task = Task::sip(
                    install(
                        client.clone(),
                        downloads,
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
                            source: first,
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
                    *delete_data = delete;
                }
                Action::none()
            }
            Message::Cancel => {
                self.confirm = None;
                Action::none()
            }
            Message::Confirm => {
                let (Some(confirm), Some(installation)) = (self.confirm.take(), installation)
                else {
                    return Action::none();
                };
                let installation = installation.clone();
                let failure = confirm.failure();

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

                if let Err(error) = result {
                    errors.report(error);
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
    /// (`snapshot`). Nothing can be installed until both are there.
    pub fn view<'a>(
        &'a self,
        catalog: &'a Catalog,
        installation: Option<&'a Installation>,
        snapshot: Option<&'a Snapshot>,
        refresh: catalog::Refresh,
    ) -> Element<'a, Message> {
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

        let reload = match refresh {
            catalog::Refresh::Unavailable => None,
            catalog::Refresh::Idle => Some(
                button("Reload")
                    .style(button::secondary)
                    .padding(10)
                    .on_press(Message::Reload),
            ),
            catalog::Refresh::Running => {
                Some(button("Refreshing…").style(button::secondary).padding(10))
            }
        };

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
        .push(reload)
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
                false,
            )
        } else if self.filter.status == Status::LeftoverData {
            self.leftovers(leftovers, filtered)
        } else {
            let shown = library.shown(&query, &self.filter);

            if shown.is_empty() {
                empty(
                    match self.filter.status {
                        _ if filtered => "No addon matches.",
                        Status::All => "The catalog is empty.",
                        Status::Installed => "No addons installed yet.",
                        Status::Updatable => "Every installed addon is at the catalog's version.",
                        Status::NotInstalled => "Every addon in the catalog is installed.",
                        Status::Attention => "Every installed addon loads fine.",
                        Status::LeftoverData => "No leftover data.",
                    },
                    filtered,
                )
            } else {
                scrollable(
                    column(
                        shown
                            .into_iter()
                            .map(|item| self.card(&library, item, catalog, snapshot, can_install)),
                    )
                    .spacing(8)
                    .padding(padding::right(14)),
                )
                .id(LIST)
                .height(Fill)
                .into()
            }
        };

        let content = column![toolbar, container(list).height(Fill)].spacing(12);

        style::modal(
            content,
            self.confirm
                .as_ref()
                .zip(installation)
                .map(|(confirm, installation)| confirm.view(installation.root())),
            Message::Cancel,
        )
    }

    /// The data that removed addons left behind, by addon name.
    fn leftovers<'a>(
        &self,
        leftovers: Vec<(String, &'a [PathBuf])>,
        filtered: bool,
    ) -> Element<'a, Message> {
        if leftovers.is_empty() {
            return empty(
                if filtered {
                    "No leftover data matches."
                } else {
                    "No leftover data."
                },
                filtered,
            );
        }

        let all: Vec<PathBuf> = leftovers
            .iter()
            .flat_map(|(_, files)| files.iter().cloned())
            .collect();

        let header = row![
            column![
                text("Leftover data").size(17).font(style::BOLD),
                text("Saved settings of addons that are no longer installed.")
                    .size(13)
                    .style(text::secondary),
            ]
            .spacing(2),
            space::horizontal(),
            button("Delete all")
                .style(button::danger)
                .on_press_maybe((!self.working).then(|| {
                    Message::Ask(Confirm::Clean {
                        title: "all leftover data".to_owned(),
                        files: all,
                    })
                })),
        ]
        .align_y(Center);

        let rows = leftovers.into_iter().map(|(name, files)| {
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
                    button("Delete")
                        .style(button::text)
                        .on_press_maybe((!self.working).then(|| {
                            Message::Ask(Confirm::Clean {
                                title: name,
                                files: files.to_vec(),
                            })
                        })),
                ]
                .spacing(12)
                .align_y(Center),
            )
            .padding([6, 14])
            .style(style::card)
            .into()
        });

        scrollable(
            column![header]
                .extend(rows)
                .spacing(8)
                .padding(padding::right(14)),
        )
        .id(LIST)
        .height(Fill)
        .into()
    }
}

/// A centered `message` for an empty list, with a way back to everything
/// when the list is empty because of a search or filter (`clear`).
fn empty<'a>(message: &'a str, clear: bool) -> Element<'a, Message> {
    center(
        column![text(message).style(text::secondary)]
            .push(clear.then(|| {
                button("Clear filters")
                    .style(button::secondary)
                    .on_press(Message::ClearFilters)
            }))
            .spacing(12)
            .align_x(Center),
    )
    .into()
}

/// What the banner says when there is no HTTP client to download with, and
/// `error` is why.
pub fn unavailable(error: &str) -> String {
    format!("Downloads are unavailable: {error}")
}

/// Scrolls the list back to the top, for a list that changed contents.
fn to_top() -> Task<Message> {
    operation::snap_to(LIST, RelativeOffset::START, Animation::Instant)
}

/// Installs into `installation` from the first of `downloads` (source name,
/// download) that works, falling back to the next when one fails to download
/// or install, and reports progress. Setting `cancel` stops it while it is
/// still downloading.
fn install(
    client: reqwest::Client,
    downloads: Vec<(String, Download)>,
    installation: Installation,
    replacing: Vec<String>,
    cancel: Arc<AtomicBool>,
) -> impl Straw<Vec<String>, Stage, Failure> {
    sipper(async move |mut progress| {
        let mut failures = Vec::new();

        for (source, download) in downloads {
            let attempt = async {
                let response = client
                    .get(&download.url)
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)
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

                    archive.extend_from_slice(
                        &chunk.map_err(|error| Failure::Failed(describe(&error)))?,
                    );
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

                let installation = installation.clone();
                let replacing = replacing.clone();
                let marker = download.marker();

                blocking(move || installation.install(&archive, &marker, &replacing))
                    .await
                    .map_err(Failure::Failed)
            };

            match attempt.await {
                Ok(folders) => return Ok(folders),
                Err(Failure::Cancelled) => return Err(Failure::Cancelled),
                Err(Failure::Failed(error)) => failures.push(format!("{source}: {error}")),
            }
        }

        Err(Failure::Failed(failures.join("; ")))
    })
}

#[cfg(test)]
mod tests;
