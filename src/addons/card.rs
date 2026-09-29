//! One addon in the list: a catalog entry, installed or not, or folders
//! installed without the catalog.

use iced::widget::{button, column, container, progress_bar, row, text, tooltip};
use iced::{Center, Element, Fill, Fit};

use super::confirm::{self, Confirm};
use super::{Key, Message, Stage, State};
use crate::catalog::Catalog;
use crate::core::library::{Item, Library};
use crate::core::toc;
use crate::core::{Addon, Snapshot, Toc};
use crate::size;
use crate::style::{self, Tone};

impl State {
    /// The card of `item`, one of `library`'s. Install buttons are enabled
    /// only when a client is open (`can_install`).
    pub(super) fn card<'a>(
        &'a self,
        library: &Library<'a>,
        item: Item<'a>,
        catalog: &'a Catalog,
        snapshot: Option<&'a Snapshot>,
        can_install: bool,
    ) -> Element<'a, Message> {
        let entry = match item {
            Item::Entry { entry, .. } => Some(entry),
            Item::Manual(_) => None,
        };
        let group = library.installed(item);
        let title = library.name(item).to_owned();
        let key = entry.map(Key::of);
        let job = key.as_ref().and_then(|key| self.jobs.get(key));
        let update = library.update_available(item);

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
            (Some(_), Some(_)) => {
                badges = badges.push(style::badge("Installed", Tone::Success));

                if update {
                    badges = badges.push(style::tip(
                        style::badge("Update available", Tone::Warning),
                        "The catalog has another version than the one installed.",
                    ));
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
                            ),
                            None => style::badge(marker.source.clone(), Tone::Primary),
                        })
                        .push(style::badge("No longer in the catalog", Tone::Warning));
                }
                None => badges = badges.push(style::badge("Installed manually", Tone::Primary)),
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
                style::badge(format!("No {}.toc: not loaded", addon.name), Tone::Danger)
            }))
            .extend(versions.into_iter().map(|(version, numbers)| {
                style::tip(
                    style::badge(format!("Made for {version}"), Tone::Warning),
                    made_for_hint(&numbers),
                )
            }));

        if toc.is_some_and(|toc| toc.load_on_demand) {
            badges = badges.push(style::badge("Load on demand", Tone::Success));
        }

        let install: Option<Element<'a, Message>> =
            key.map(|key| match job.map(|job| &job.stage) {
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
                            progress_bar(0.0..=1.0, fraction).length(140).girth(6),
                            text(progress).size(12).style(text::secondary),
                        ]
                        .spacing(4),
                        button("Cancel")
                            .style(button::text)
                            .on_press(Message::Stop(key)),
                    ]
                    .spacing(8)
                    .align_y(Center)
                    .into()
                }
                Some(Stage::Installing) => text("Installing…").style(text::secondary).into(),
                Some(Stage::Cancelling) => text("Cancelling…").style(text::secondary).into(),
                None => button(match (group.is_some(), update) {
                    (true, true) => "Update",
                    (true, false) => "Reinstall",
                    (false, _) => "Install",
                })
                .style(if group.is_some() && !update {
                    button::secondary
                } else {
                    button::primary
                })
                .on_press_maybe(can_install.then_some(Message::Install(key)))
                .into(),
            });

        let repair = broken.first().map(|addon| {
            button("Repair")
                .style(button::primary)
                .on_press_maybe((!self.working).then(|| {
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
            let summary = confirm::data_summary(&data);

            let clean = button("Clean data")
                .style(button::secondary)
                .on_press_maybe((!data.is_empty() && !self.working).then(|| {
                    Message::Ask(Confirm::Clean {
                        title: title.clone(),
                        files: data.clone(),
                    })
                }));

            let uninstall = style::delete_button(
                "Uninstall",
                (!self.working).then(|| {
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

        let heading = row![badges.width(Fill).wrap().vertical_spacing(6)]
            .push(repair)
            .push(install)
            .push(manage)
            .spacing(10)
            .align_y(Center);

        let description = entry
            .map(|entry| entry.description.as_str())
            .filter(|description| !description.is_empty())
            .or_else(|| toc.and_then(|toc| toc.notes.as_deref()))
            .map(|description| text(description).size(14).style(text::secondary));

        let details: Vec<String> = [
            toc.and_then(|toc| toc.version.as_ref())
                .map(|version| format!("Version {version}")),
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
                    .downloads
                    .iter()
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
                    .downloads
                    .first()
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
                            .style(button::text)
                            .padding(0)
                            .on_press(Message::OpenLink(link.url.clone()))
                            .into()
                    });

                // Wrapping onto more lines when the window is narrow.
                Some(
                    row(pills.into_iter().map(|(label, url)| {
                        style::link_badge(label, url, Message::OpenLink(url.to_owned()))
                    }))
                    .push(updated)
                    .extend(links)
                    .spacing(10)
                    .align_y(Center)
                    .wrap()
                    .vertical_spacing(6),
                )
            }
            None => None,
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
                            let installing = self.jobs.contains_key(&key);

                            button(text(format!("Install {dependency}")).size(12))
                                .style(button::secondary)
                                .padding([2, 8])
                                .on_press_maybe(
                                    (can_install && !installing).then_some(Message::Install(key)),
                                )
                                .into()
                        }
                        None => style::tip(
                            text(dependency).size(13).style(text::warning),
                            "Not in the catalog: install it yourself.",
                        ),
                    }
                }))
                .spacing(8)
                .align_y(Center)
                .wrap()
                .vertical_spacing(6)
        });

        container(
            column![heading]
                .push(description)
                .push(details)
                .push(about)
                .push(folders)
                .push(required_by)
                .push(optionally_used_by)
                .push(missing)
                .spacing(6),
        )
        .padding(14)
        .width(Fill)
        .style(style::card)
        .into()
    }
}

/// The `folders` an addon installs, on one line; all of them on hover, when
/// they don't fit.
fn folder_list<'a>(folders: &[String]) -> Element<'a, Message> {
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
            .style(container::rounded_box),
        tooltip::Position::Bottom,
    )
    .into()
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
