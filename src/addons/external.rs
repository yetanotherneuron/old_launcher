//! Installing an addon the list of addons lacks, from an archive or a folder
//! the user picks. The dialog shows what it would install, and what that
//! changes, before anything does, and which of its folders to install.

use std::path::{Path, PathBuf};

use iced::widget::{button, checkbox, column, container, row, scrollable, space, text};
use iced::{Center, Element, Fill, Fit, Font, Function, Widget};

use super::Message;
use crate::catalog::Catalog;
use crate::core::installation::Found;
use crate::core::{Marker, Snapshot, toc};
use crate::style;

/// The dialog, from opening to installing.
#[derive(Debug)]
pub enum External {
    /// Nothing picked yet.
    Choosing,
    /// Reading what the archive or folder at this path holds.
    Reading(PathBuf),
    /// What it holds, or why it can't be installed.
    Review {
        /// The archive or folder.
        path: PathBuf,
        /// Its addon folders, or why it can't be installed.
        found: Result<Vec<Found>, String>,
        /// The folders to install, by name.
        chosen: Vec<String>,
    },
    /// Installing it.
    Installing(PathBuf),
}

impl External {
    /// What `path` holds (`found`), with every folder chosen but those
    /// replacing an addon installed from the list, as `snapshot` saw it: the
    /// list's version stays unless asked.
    pub fn review(
        path: PathBuf,
        found: Result<Vec<Found>, String>,
        snapshot: Option<&Snapshot>,
    ) -> Self {
        let chosen = found
            .as_ref()
            .map(|found| {
                found
                    .iter()
                    .filter(|folder| listed(folder, snapshot).is_none())
                    .map(|folder| folder.name.clone())
                    .collect()
            })
            .unwrap_or_default();

        Self::Review {
            path,
            found,
            chosen,
        }
    }

    /// Chooses the folder `name` to install, or leaves it out (`chosen`).
    pub fn choose(&mut self, name: String, chosen: bool) {
        if let Self::Review { chosen: names, .. } = self {
            names.retain(|other| *other != name);
            if chosen {
                names.push(name);
            }
        }
    }

    /// The archive or folder picked, once there is one.
    fn path(&self) -> Option<&Path> {
        match self {
            Self::Choosing => None,
            Self::Reading(path) | Self::Review { path, .. } | Self::Installing(path) => Some(path),
        }
    }

    /// The dialog, saying what installing changes in the client as
    /// `snapshot` saw it, whose folders may come from the `catalog`.
    pub fn view<'a>(
        &'a self,
        snapshot: Option<&'a Snapshot>,
        catalog: &'a Catalog,
    ) -> impl Widget<Message> + 'a {
        let busy = matches!(self, Self::Reading(_) | Self::Installing(_));

        let choose = row![
            button("Choose a file…")
                .style(style::secondary)
                .on_press_maybe((!busy).then_some(Message::PickArchive)),
            button("Choose a folder…")
                .style(style::secondary)
                .on_press_maybe((!busy).then_some(Message::PickFolder)),
        ]
        .spacing(8);

        let picked = self.path().map(|path| {
            text(path.display().to_string())
                .font(Font::MONOSPACE)
                .size(13)
                .style(text::secondary)
        });

        let outcome: Option<Element<'a, Message>> = match self {
            Self::Choosing => None,
            Self::Reading(_) => Some(text("Reading…").style(text::secondary).boxed()),
            Self::Installing(_) => Some(text("Installing…").style(text::secondary).boxed()),
            Self::Review {
                found: Err(error), ..
            } => Some(text(sentence(error)).style(text::danger).boxed()),
            Self::Review {
                found: Ok(found),
                chosen,
                ..
            } => Some(review(found, chosen, snapshot, catalog).boxed()),
        };

        let installable = matches!(
            self,
            Self::Review { found: Ok(_), chosen, .. } if !chosen.is_empty()
        );

        container(
            column![
                style::heading("Install an addon", 20.0),
                text(
                    "From an archive (zip, 7z or rar) or a folder that holds it. It shows as \
                     installed manually: to update it, install a newer version the same way."
                )
                .style(text::secondary),
                choose,
                picked,
                outcome,
                row![
                    space::horizontal(),
                    button("Cancel").style(style::quiet).on_press_maybe(
                        (!matches!(self, Self::Installing(_))).then_some(Message::Cancel)
                    ),
                    button("Install")
                        .style(style::primary)
                        .on_press_maybe(installable.then_some(Message::InstallPicked)),
                ]
                .spacing(8),
            ]
            .spacing(14),
        )
        .padding(24)
        .width(Fill.max(620))
        .style(style::card)
    }
}

/// The addon folders `found`, each with a box to choose it and where it
/// comes from, and below them what installing those `chosen` changes or may
/// get wrong.
fn review<'a>(
    found: &'a [Found],
    chosen: &'a [String],
    snapshot: Option<&'a Snapshot>,
    catalog: &'a Catalog,
) -> impl Widget<Message> + 'a {
    let folders = column(found.iter().map(|folder| {
        let title = folder
            .toc
            .title
            .as_deref()
            .filter(|title| *title != folder.name);
        let from = (folder.from != folder.name).then(|| format!("from {}", folder.from));
        let installed = listed(folder, snapshot)
            .map(|marker| format!("already installed from {}", source(marker, catalog)));
        let details: Vec<&str> = [
            title,
            folder.toc.version.as_deref(),
            from.as_deref(),
            installed.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect();

        row![
            checkbox(chosen.contains(&folder.name))
                .style(style::check)
                .label(folder.name.as_str())
                .on_toggle(Message::Choose.with(folder.name.clone())),
            text(details.join(" · ")).size(13).style(text::secondary),
        ]
        .spacing(8)
        .align_y(Center)
        .wrap()
    }))
    .spacing(6);

    column![
        text("Choose the folders to install into Interface/AddOns:"),
        container(
            scrollable(folders)
                .height(Fit.max(200))
                .width(Fill)
                .style(style::scroll),
        )
        .padding(10)
        .width(Fill)
        .style(style::inset),
        column(
            warnings(found, chosen, snapshot, catalog)
                .into_iter()
                .map(|warning| text(warning).size(13).style(text::warning))
        )
        .spacing(4),
    ]
    .spacing(8)
}

/// The marker of the addon installed from the list that `folder` would
/// replace, in the client as `snapshot` saw it.
fn listed<'a>(folder: &Found, snapshot: Option<&'a Snapshot>) -> Option<&'a Marker> {
    snapshot?
        .addons
        .iter()
        .find(|addon| addon.name.eq_ignore_ascii_case(&folder.name))?
        .marker
        .as_ref()
}

/// The name of the source `marker` says a folder came from, as the
/// `catalog` names it.
fn source<'a>(marker: &'a Marker, catalog: &'a Catalog) -> &'a str {
    catalog
        .source(&marker.source)
        .map_or(marker.source.as_str(), |source| source.name.as_str())
}

/// `error` as a sentence of its own: errors are written to follow "Could
/// not …:", so they start in lowercase.
fn sentence(error: &str) -> String {
    let mut characters = error.chars();

    characters
        .next()
        .map(|first| first.to_uppercase().chain(characters).collect())
        .unwrap_or_default()
}

/// What installing the `chosen` folders of `found` changes or may get wrong,
/// in the client as `snapshot` saw it: folders they replace (one from the
/// `catalog`'s entries no longer updating), folders made for another game
/// version, and dependencies the game doesn't have (see [`Snapshot::has`])
/// that aren't chosen too. Not a dependency the game's archives may hold,
/// until they are known.
fn warnings(
    found: &[Found],
    chosen: &[String],
    snapshot: Option<&Snapshot>,
    catalog: &Catalog,
) -> Vec<String> {
    let installed = snapshot
        .map(|snapshot| snapshot.addons.as_slice())
        .unwrap_or_default();
    let mut warnings = Vec::new();

    for folder in found.iter().filter(|folder| chosen.contains(&folder.name)) {
        if let Some(addon) = installed
            .iter()
            .find(|addon| addon.name.eq_ignore_ascii_case(&folder.name))
        {
            warnings.push(match &addon.marker {
                Some(marker) => format!(
                    "Replaces {}, installed from {}: it won't update from there anymore.",
                    addon.name,
                    source(marker, catalog)
                ),
                None => format!("Replaces the {} folder already there.", addon.name),
            });
        }

        if let Some(version) = folder.toc.interface.and_then(toc::made_for) {
            warnings.push(format!(
                "{} was made for {version}: it might not work on 3.3.5a.",
                folder.name
            ));
        }

        for dependency in &folder.toc.dependencies {
            let missing = !chosen
                .iter()
                .any(|name| name.eq_ignore_ascii_case(dependency))
                && snapshot.and_then(|snapshot| snapshot.has(dependency)) == Some(false);

            if missing {
                warnings.push(format!(
                    "{} requires {dependency}, which won't be installed.",
                    folder.name
                ));
            }
        }
    }

    warnings
}
