//! One addon in the list: a catalog entry, installed or not, or folders
//! installed without the catalog.

use iced::widget::{button, column, container, pick_list, progress_bar, row, text, tooltip};
use iced::{Center, Fill, Fit, Widget};

use super::confirm::{self, Confirm};
use super::{Choice, Key, Message, Stage, State};
use crate::catalog::{Catalog, Entry};
use crate::core::library::{Item, Library, Versions};
use crate::core::toc;
use crate::core::{Addon, Snapshot, Toc};
use crate::style::{self, Tone};
use crate::{game, size};

impl State {
    /// The card of `item`, one of `library`'s. Install buttons are enabled
    /// only when a client is open (`can_install`), and saved settings can't
    /// be deleted while its game runs (`playing`).
    pub(super) fn card<'a>(
        &'a self,
        library: &Library<'a>,
        item: Item<'a>,
        catalog: &'a Catalog,
        snapshot: Option<&'a Snapshot>,
        can_install: bool,
        playing: bool,
    ) -> impl Widget<Message> + use<'a> {
        let entry = match item {
            Item::Entry { entry, .. } => Some(entry),
            Item::Manual(_) => None,
        };
        let group = library.installed(item);
        let title = library.name(item).to_owned();
        let key = entry.map(Key::of);
        let card_id = key.as_ref().map(id);
        let job = key.as_ref().and_then(|key| self.jobs.get(key));
        let settling = key.as_ref().is_some_and(|key| self.settling.contains(key));
        let update = library.update(item);

        let main = group.and_then(|group| group.addons.first().copied());
        let toc = main.and_then(|addon| addon.toc.as_ref());
        let broken: Vec<&Addon> = group
            .map(|group| {
                group
                    .addons
                    .iter()
                    .copied()
                    .filter(|addon| addon.toc.is_none())
                    .collect()
            })
            .unwrap_or_default();

        // The name and its badges, wrapping onto more lines when the window
        // is narrow, so the buttons beside them always keep their size.
        let mut badges = row![text(title.clone()).size(17).font(style::BOLD)]
            .spacing(8)
            .align_y(Center);

        match (entry, group) {
            (Some(entry), Some(group)) => {
                badges = badges.push(style::badge("Installed", Tone::Success).boxed());
                let changed = group.changed();
                if !changed.is_empty() {
                    badges = badges.push(
                        style::tip(
                            style::badge("Modified", Tone::Warning),
                            changed_hint(&changed, true),
                        )
                        .boxed(),
                    );
                }

                if let Some(update) = update {
                    badges = badges.push(
                        style::tip(
                            style::badge("Update available", Tone::Rare),
                            update_hint(update, main, entry, catalog),
                        )
                        .boxed(),
                    );
                }
            }
            (None, Some(group)) => match group.marker {
                // Installed by the launcher from an entry the catalog dropped
                // since.
                Some(marker) => {
                    badges = badges
                        .push(match catalog.source(&marker.source) {
                            Some(source) => style::link_badge(
                                &source.name,
                                &source.homepage,
                                Message::OpenLink(source.homepage.clone()),
                            )
                            .boxed(),
                            None => style::badge(marker.source.clone(), Tone::Primary).boxed(),
                        })
                        .push(style::badge("No longer available", Tone::Poor).boxed());
                    let changed = group.changed();
                    if !changed.is_empty() {
                        badges = badges.push(
                            style::tip(
                                style::badge("Modified", Tone::Warning),
                                changed_hint(&changed, false),
                            )
                            .boxed(),
                        );
                    }
                }
                None => {
                    badges = badges.push(style::badge("Installed manually", Tone::Primary).boxed());
                }
            },
            (_, None) => {}
        }

        // The interfaces of what is on disk, when installed; of what the
        // catalog says otherwise.
        let mut interfaces: Vec<u32> = match (group, entry) {
            (Some(group), _) => group
                .addons
                .iter()
                .filter_map(|addon| addon.toc.as_ref()?.interface)
                .collect(),
            (None, Some(entry)) => entry
                .folders()
                .iter()
                .filter_map(|folder| folder.interface)
                .collect(),
            (None, None) => Vec::new(),
        };
        interfaces.sort_unstable();
        interfaces.dedup();

        // One badge per game version other than 3.3.5a, however many folders
        // say so. Backports often keep another client's number, so the
        // badge only warns.
        let mut versions: Vec<(String, Vec<u32>)> = Vec::new();

        for interface in interfaces {
            let Some(version) = toc::made_for(interface) else {
                continue;
            };

            match versions.iter_mut().find(|(named, _)| *named == version) {
                Some((_, numbers)) => numbers.push(interface),
                None => versions.push((version, vec![interface])),
            }
        }

        badges = badges
            .extend(broken.iter().map(|addon| {
                style::badge(format!("No {}.toc: not loaded", addon.name), Tone::Danger).boxed()
            }))
            .extend(versions.into_iter().map(|(version, numbers)| {
                style::tip(
                    style::badge(format!("Made for {version}"), Tone::Warning),
                    made_for_hint(&numbers),
                )
                .boxed()
            }));

        if toc.is_some_and(|toc| toc.load_on_demand) {
            badges = badges.push(style::badge("Load on demand", Tone::Success).boxed());
        }

        // Where to install from, when there is a choice: the source the
        // catalog lists first, or one in particular.
        let picker = entry.zip(key.clone()).and_then(|(entry, key)| {
            let mut choices = vec![Choice::Auto];
            for download in &entry.downloads {
                let choice = Choice::Source(download.source.clone());
                if !choices.contains(&choice) {
                    choices.push(choice);
                }
            }
            if choices.len() < 3 {
                return None;
            }

            let name = |id: &str| {
                catalog
                    .source(id)
                    .map_or_else(|| id.to_owned(), |source| source.name.clone())
            };
            let auto = entry
                .downloads
                .first()
                .map(|download| name(&download.source))
                .unwrap_or_default();
            let chosen = match &entry.preferred {
                Some(source)
                    if entry
                        .downloads
                        .iter()
                        .any(|download| &download.source == source) =>
                {
                    Choice::Source(source.clone())
                }
                _ => Choice::Auto,
            };

            Some(style::tip(
                pick_list(Some(chosen), choices, move |choice| match choice {
                    Choice::Auto => format!("Auto ({auto})"),
                    Choice::Source(id) => name(id),
                })
                .style(style::picker)
                .menu_style(style::choices)
                .on_select(move |choice| Message::SourceSelected(key.clone(), choice)),
                "Where it installs from: the newest, or the source picked. Nothing else is \
                 tried when that fails.",
            ))
        });

        let install = entry
            .zip(key)
            .map(|(entry, key)| match job.map(|job| &job.stage) {
                Some(Stage::Downloading {
                    source,
                    received,
                    total,
                }) => {
                    let progress = match total {
                        Some(total) if *total > 0 => {
                            format!("{source}: {} / {}", size(*received), size(*total))
                        }
                        _ => format!("{source}: {}", size(*received)),
                    };
                    let fraction = total
                        .filter(|total| *total > 0)
                        .map_or(0.0, |total| *received as f32 / total as f32);

                    row![
                        column![
                            progress_bar(0.0..=1.0, fraction)
                                .length(140)
                                .girth(6)
                                .style(style::bar),
                            text(progress).size(12).style(text::secondary),
                        ]
                        .spacing(4),
                        button("Cancel")
                            .style(style::quiet)
                            .on_press(Message::Stop(key)),
                    ]
                    .spacing(8)
                    .align_y(Center)
                    .boxed()
                }
                Some(Stage::Installing) => text("Installing…").style(text::secondary).boxed(),
                Some(Stage::Cancelling) => text("Cancelling…").style(text::secondary).boxed(),
                None => {
                    let label = match (group.is_some(), update) {
                        (true, Some(update)) => format!("Update to {}", update.offered),
                        (true, None) => "Reinstall".to_owned(),
                        (false, _) => "Install".to_owned(),
                    };
                    let install = button(text(label))
                        .style(if group.is_some() && update.is_none() {
                            style::secondary
                        } else {
                            style::primary
                        })
                        .on_press_maybe(
                            (can_install && !settling).then_some(Message::Install(key)),
                        );

                    // Where the download comes from.
                    style::tip(install, source_hint(entry, catalog)).boxed()
                }
            });

        // What changed, beside the button that brings it: when the source
        // keeps a history of the version an update brings.
        let changes = update
            .and(entry)
            .and_then(|entry| entry.preferred()?.changes.as_deref())
            .map(|url| {
                button(text("Changes ↗").size(13))
                    .style(style::quiet)
                    .padding(0)
                    .on_press(Message::OpenLink(url.to_owned()))
            });

        let repair = broken.first().map(|addon| {
            button("Repair")
                .style(style::primary)
                .on_press_maybe(self.working.is_none().then(|| {
                    Message::Ask(Confirm::Repair {
                        folder: addon.name.clone(),
                    })
                }))
        });

        let required_by: Vec<String> = group
            .map(|group| {
                group
                    .required_by(&library.groups)
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let optionally_used_by: Vec<String> = group
            .map(|group| {
                group
                    .optionally_used_by(&library.groups)
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();

        let manage = group.map(|group| {
            let data = snapshot
                .map(|snapshot| snapshot.data_files(group))
                .unwrap_or_default();
            let summary = if playing && !data.is_empty() {
                format!("{} {}", confirm::data_summary(&data), game::KEEPS_SETTINGS)
            } else {
                confirm::data_summary(&data)
            };

            let clean = button("Clean data").style(style::secondary).on_press_maybe(
                (!data.is_empty() && self.working.is_none() && !playing).then(|| {
                    Message::Ask(Confirm::Clean {
                        title: title.clone(),
                        files: data.clone(),
                    })
                }),
            );

            let uninstall = style::delete_button(
                "Uninstall",
                (self.working.is_none() && !settling).then(|| {
                    Message::Ask(Confirm::Uninstall {
                        title: title.clone(),
                        folders: group.folders(),
                        data,
                        delete_data: false,
                        required_by: required_by.clone(),
                        optionally_used_by: optionally_used_by.clone(),
                    })
                }),
            );

            row![
                style::tip(clean, summary),
                container(uninstall).id(uninstall_id(&title)),
            ]
            .spacing(6)
            .align_y(Center)
        });

        let heading = row![
            badges.width(Fill).wrap().vertical_spacing(6),
            repair,
            picker,
            changes,
            install,
            manage,
        ]
        .spacing(10)
        .align_y(Center);

        let description = entry
            .map(|entry| entry.description.as_str())
            .filter(|description| !description.is_empty())
            .or_else(|| toc.and_then(|toc| toc.notes.as_deref()))
            .map(|description| text(description).size(14).style(text::secondary));

        // Both versions when the catalog's differs from the installed one,
        // the installed one otherwise, or the catalog's for an entry not
        // installed yet.
        let ahead = update.is_none().then(|| library.ahead(item)).flatten();
        // The folder, when it isn't the one the card describes.
        let named = |versions: Versions<'_>| {
            if main.is_some_and(|addon| addon.name == versions.folder) {
                String::new()
            } else {
                format!(" ({})", versions.folder)
            }
        };
        let version = match (update, ahead, toc, entry) {
            (Some(update), _, _, _) => Some(format!(
                "Installed {} · New version {} available{}",
                update.installed,
                update.offered,
                named(update)
            )),
            (None, Some(ahead), _, _) => Some(format!(
                "Installed {} · Your version is newer than the {} available to download{}",
                ahead.installed,
                ahead.offered,
                named(ahead)
            )),
            (None, None, Some(toc), _) => toc
                .version
                .as_ref()
                .map(|version| format!("Version {version}")),
            (None, None, None, Some(entry)) if group.is_none() => entry
                .folders()
                .first()
                .and_then(|folder| folder.version.as_ref())
                .map(|version| format!("Version {version}")),
            (None, None, None, _) => None,
        };

        let details: Vec<String> = [
            version,
            toc.and_then(|toc| toc.author.as_ref())
                .map(|author| format!("by {author}")),
        ]
        .into_iter()
        .flatten()
        .collect();
        let details = (!details.is_empty())
            .then(|| text(details.join(" · ")).size(13).style(text::secondary));

        let folders: Vec<String> = match (entry, group) {
            (Some(entry), _) => entry
                .folders()
                .iter()
                .map(|folder| folder.name.clone())
                .collect(),
            (None, Some(group)) => group.folders(),
            (None, None) => Vec::new(),
        };
        // A lone folder named like the addon says nothing new.
        let folders = (folders.len() > 1 || folders.first().is_some_and(|folder| *folder != title))
            .then_some(folders);

        let about = match entry {
            Some(entry) => {
                // Each source links to the addon's page there, in the order
                // installing tries them: the most recently updated first. A
                // source listing the addon twice names each listing.
                let pills: Vec<(String, &str)> = entry
                    .ranked()
                    .filter_map(|download| {
                        let source = catalog.source(&download.source)?;
                        let page = download.page.as_deref().unwrap_or(&source.homepage);
                        let listings = entry
                            .downloads
                            .iter()
                            .filter(|other| other.source == download.source)
                            .count();
                        let label = if listings > 1 {
                            format!("{} · {}", source.name, download.name)
                        } else {
                            source.name.clone()
                        };

                        Some((label, page))
                    })
                    .collect();

                let updated = entry
                    .preferred()
                    .and_then(|download| download.updated.as_deref())
                    .map(|updated| {
                        text(format!("Updated {updated}"))
                            .size(12)
                            .style(text::secondary)
                    });

                // Other pages about the addon: its author's, CurseForge, …
                let mut seen: Vec<&str> = pills.iter().map(|(_, url)| *url).collect();
                let links = entry
                    .downloads
                    .iter()
                    .flat_map(|download| &download.links)
                    .filter(|link| {
                        let new = !seen.contains(&link.url.as_str());
                        seen.push(&link.url);
                        new
                    })
                    .map(|link| {
                        button(text(format!("{} ↗", link.label)).size(13))
                            .style(style::quiet)
                            .padding(0)
                            .on_press(Message::OpenLink(link.url.clone()))
                            .boxed()
                    });

                // Wrapping onto more lines when the window is narrow.
                Some(
                    row(pills.into_iter().map(|(label, url)| {
                        style::link_badge(&label, url, Message::OpenLink(url.to_owned())).boxed()
                    }))
                    .push(updated.boxed())
                    .extend(links)
                    .spacing(10)
                    .align_y(Center)
                    .wrap()
                    .vertical_spacing(6)
                    .boxed(),
                )
            }
            // Not one the catalog offers: the pages its `.toc` names.
            None => toc.filter(|toc| !toc.websites.is_empty()).map(|toc| {
                row(toc.websites.iter().map(|url| {
                    button(text(format!("{} ↗", address(url))).size(13))
                        .style(style::quiet)
                        .padding(0)
                        .on_press(Message::OpenLink(url.clone()))
                }))
                .spacing(10)
                .align_y(Center)
                .wrap()
                .vertical_spacing(6)
                .boxed()
            }),
        };
        let folders = folders.map(|folders| folder_list(&folders));

        let required_by = (!required_by.is_empty()).then(|| {
            text(format!("Required by: {}", required_by.join(", ")))
                .size(13)
                .style(text::secondary)
        });

        let optionally_used_by = (!optionally_used_by.is_empty()).then(|| {
            text(format!(
                "Optionally used by: {}",
                optionally_used_by.join(", ")
            ))
            .size(13)
            .style(text::secondary)
        });

        let mut missing: Vec<&str> = match (group, snapshot) {
            (Some(group), Some(snapshot)) => group
                .addons
                .iter()
                .flat_map(|addon| snapshot.missing_dependencies(addon))
                .collect(),
            _ => Vec::new(),
        };
        missing.dedup_by(|a, b| a.eq_ignore_ascii_case(b));

        // Each missing dependency the catalog has can be installed from
        // right here.
        let missing = (!missing.is_empty()).then(|| {
            row![text("Missing dependencies:").size(13).style(text::warning)]
                .extend(missing.into_iter().map(|dependency| {
                    let provider = catalog.entries.iter().find(|entry| {
                        entry
                            .folders()
                            .iter()
                            .any(|folder| folder.name.eq_ignore_ascii_case(dependency))
                    });

                    match provider {
                        Some(provider) => {
                            let key = Key::of(provider);
                            let installing = self.busy(&key);

                            button(text(format!("Install {dependency}")).size(12))
                                .style(style::secondary)
                                .padding([2, 8])
                                .on_press_maybe(
                                    (can_install && !installing).then_some(Message::Install(key)),
                                )
                                .boxed()
                        }
                        None => style::tip(
                            text(dependency).size(13).style(text::warning),
                            "Not available here: install it yourself.",
                        )
                        .boxed(),
                    }
                }))
                .spacing(8)
                .align_y(Center)
                .wrap()
                .vertical_spacing(6)
        });

        let card = container(
            column![
                heading,
                description,
                details,
                about,
                folders,
                required_by,
                optionally_used_by,
                missing,
            ]
            .spacing(6),
        )
        .padding(14)
        .width(Fill)
        .style(style::card);

        // The list finds the card by its id, to scroll to it.
        match card_id {
            Some(id) => card.id(id),
            None => card,
        }
    }
}

/// The id of the card of the entry with `key`.
pub fn id(key: &Key) -> String {
    format!("card {}", key.as_str())
}

/// The `folders` an addon installs, on one line; all of them on hover, when
/// they don't fit.
fn folder_list(folders: &[String]) -> impl Widget<Message> + use<> {
    let list = folders.join(", ");

    tooltip(
        text(format!("Folders: {list}"))
            .size(12)
            .style(text::secondary)
            .wrapping(text::Wrapping::None)
            .ellipsis(text::Ellipsis::End)
            .width(Fill),
        container(text(list).size(12))
            .padding([4, 8])
            .width(Fit.max(480))
            .style(style::tooltip_box),
        tooltip::Position::Bottom,
    )
}

/// `url` as a card shows it: without its scheme, a leading `www.` or a
/// trailing slash, as in `github.com/bkader/Raven-WotLK`.
fn address(url: &str) -> &str {
    let address = url.split_once("://").map_or(url, |(_, rest)| rest);
    let address = address.strip_prefix("www.").unwrap_or(address);

    address.strip_suffix('/').unwrap_or(address)
}

/// What an "Update available" badge means: the version installed against
/// the one the catalog offers, and where that one comes from. The folder is
/// named when it isn't `main`, the one the card describes.
/// What the "Modified" badge says on hover: the files that `changed` since
/// the launcher installed the addon, and the way back when a `reinstall` is
/// offered.
fn changed_hint(changed: &[String], reinstall: bool) -> String {
    const NAMED: usize = 6;

    let mut files = changed
        .iter()
        .take(NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if changed.len() > NAMED {
        files.push_str(&format!(" and {} more", changed.len() - NAMED));
    }
    let back = if reinstall {
        " Reinstall puts the installed version back."
    } else {
        ""
    };

    format!("Changed since it was installed: {files}.{back}")
}

fn update_hint(
    update: Versions<'_>,
    main: Option<&Addon>,
    entry: &Entry,
    catalog: &Catalog,
) -> String {
    let download = entry.preferred();
    let source = download
        .and_then(|download| catalog.source(&download.source))
        .map(|source| format!(" from {}", source.name))
        .unwrap_or_default();
    let updated = download
        .and_then(|download| download.updated.as_deref())
        .map(|updated| format!(", updated {updated}"))
        .unwrap_or_default();
    let versions = format!(
        "{}; {} is available{source}{updated}.",
        update.installed, update.offered
    );

    if main.is_some_and(|main| main.name == update.folder) {
        format!("Installed {versions}")
    } else {
        format!("{}: installed {versions}", update.folder)
    }
}

/// Where the install button's download comes from: the source, and when
/// its copy was updated. Nothing else is tried when it fails.
fn source_hint(entry: &Entry, catalog: &Catalog) -> String {
    let Some(download) = entry.preferred() else {
        return String::new();
    };
    let source = catalog
        .source(&download.source)
        .map_or_else(|| download.source.clone(), |source| source.name.clone());
    let updated = download
        .updated
        .as_deref()
        .map(|updated| format!(", updated {updated}"))
        .unwrap_or_default();

    format!("From {source}{updated}.")
}

/// What a "Made for …" badge means, for folders declaring the interface
/// `numbers`.
fn made_for_hint(numbers: &[u32]) -> String {
    let declared = match numbers {
        [one] => format!("Its .toc declares interface {one}"),
        many => format!(
            "Its .toc files declare interfaces {}",
            confirm::names(&many.iter().map(u32::to_string).collect::<Vec<_>>())
        ),
    };

    format!(
        "{declared}; 3.3.5a's is {}. The game lists it as out of date and only \
         loads it with \u{201c}Load out of date AddOns\u{201d} ticked on the character \
         selection screen. Even then, it might not work, or only in part.",
        Toc::WOTLK_INTERFACE
    )
}

/// The id of the uninstall button of the addon listed as `title`.
pub fn uninstall_id(title: &str) -> String {
    format!("uninstall {title}")
}
