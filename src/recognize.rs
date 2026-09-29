//! Addons installed without the launcher that match a catalog entry: the
//! launcher proposes to recognize them, and marks them as installed from
//! that entry when the user agrees.

use std::collections::BTreeSet;

use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Center, Element, Fill, Fit, Task, padding};

use crate::action::Action;
use crate::catalog::{Catalog, Entry};
use crate::core::{Adoption, Installation, Snapshot};
use crate::errors::Errors;
use crate::{blocking, style};

/// The addons proposed for recognition.
#[derive(Default)]
pub struct State {
    /// What the last scan found, minus what was declined.
    proposed: Vec<Adoption>,
    /// The folder sets (lowercase, sorted) the user declined, for this run
    /// of the launcher.
    declined: BTreeSet<Vec<String>>,
    /// Whether the list of proposed addons is open.
    details: bool,
    /// Whether the markers are being written.
    working: bool,
}

/// Interactions with the proposal.
#[derive(Debug, Clone)]
pub enum Message {
    /// List the proposed addons.
    Show,
    /// Close the list.
    Close,
    /// Recognize them: write the markers.
    Accept,
    /// Leave them as they are, and don't ask again about them.
    Decline,
    /// The markers were written, for these addons.
    Adopted(Result<Vec<Adoption>, String>),
}

/// What the proposal asks of the parent.
pub enum Instruction {
    /// This many addons are now marked, so the client needs scanning again.
    Recognized(usize),
}

impl State {
    /// Proposes the addons of `snapshot` that match one of `entries`,
    /// unless they were declined, replacing any earlier proposal.
    pub fn propose(&mut self, snapshot: &Snapshot, entries: &[Entry]) {
        if self.working {
            return;
        }

        self.proposed = Adoption::plan(snapshot, entries)
            .into_iter()
            .filter(|adoption| !self.declined.contains(&key(adoption)))
            .collect();
        self.proposed
            .sort_by_cached_key(|adoption| adoption.name.to_lowercase());

        if self.proposed.is_empty() {
            self.details = false;
        } else {
            log::info!(
                "{} installed without the launcher match the catalog: {}",
                crate::counted(self.proposed.len(), "addon"),
                self.proposed
                    .iter()
                    .map(|adoption| adoption.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }

    /// Forgets the proposal: another client is being shown.
    pub fn clear(&mut self) {
        self.proposed.clear();
        self.details = false;
    }

    /// Whether the list of proposed addons is open.
    pub fn details_open(&self) -> bool {
        self.details
    }

    /// Handles an interaction; the markers are written into `installation`,
    /// and failures go to `errors`.
    pub fn update(
        &mut self,
        message: Message,
        installation: Option<&Installation>,
        errors: &mut Errors,
    ) -> Action<Instruction, Message> {
        match message {
            Message::Show => {
                self.details = !self.proposed.is_empty();
                Action::none()
            }
            Message::Close => {
                self.details = false;
                Action::none()
            }
            Message::Accept => {
                let Some(installation) = installation.filter(|_| !self.working) else {
                    return Action::none();
                };
                let installation = installation.clone();
                let adoptions = self.proposed.clone();

                self.working = true;
                log::info!("Recognizing {} addons", adoptions.len());

                Action::task(Task::perform(
                    blocking(move || {
                        let mut adopted = Vec::new();

                        for adoption in adoptions {
                            if installation.adopt(&adoption)? > 0 {
                                adopted.push(adoption);
                            }
                        }

                        Ok(adopted)
                    }),
                    Message::Adopted,
                ))
            }
            Message::Decline => {
                log::info!(
                    "Not recognizing {} for now",
                    crate::counted(self.proposed.len(), "addon")
                );
                self.declined.extend(self.proposed.iter().map(key));
                self.clear();
                Action::none()
            }
            Message::Adopted(result) => {
                self.working = false;

                match result {
                    Ok(adopted) => {
                        log::info!("Recognized {}", crate::counted(adopted.len(), "addon"));
                        self.clear();
                        Action::instruction(Instruction::Recognized(adopted.len()))
                    }
                    Err(error) => {
                        errors.report(format!("Could not recognize the addons: {error}"));
                        Action::none()
                    }
                }
            }
        }
    }

    /// The banner proposing the addons, while there are any.
    pub fn banner(&self) -> Option<Element<'_, Message>> {
        let proposal = match self.proposed.as_slice() {
            [] => return None,
            [one] => format!(
                "{} was installed without the launcher and matches the catalog. Recognize \
                 it, so it shows as installed and can be updated from here?",
                one.name
            ),
            all => format!(
                "{} addons installed without the launcher match the catalog. Recognize \
                 them, so they show as installed and can be updated from here?",
                all.len()
            ),
        };

        Some(
            container(
                row![
                    text(proposal).width(Fill),
                    button("Details")
                        .style(button::secondary)
                        .on_press(Message::Show),
                    self.accept(),
                    button("Not now")
                        .style(button::text)
                        .on_press_maybe((!self.working).then_some(Message::Decline)),
                ]
                .spacing(8)
                .align_y(Center),
            )
            .padding([6, 12])
            .style(style::banner(style::Tone::Success))
            .into(),
        )
    }

    /// The list of proposed addons, while open: each with the catalog
    /// source it would be recorded under and its folders.
    pub fn dialog<'a>(&'a self, catalog: &'a Catalog) -> Option<Element<'a, Message>> {
        if !self.details {
            return None;
        }

        let addons = self.proposed.iter().map(|adoption| {
            let source = catalog
                .source(&adoption.marker.source)
                .map_or(adoption.marker.source.as_str(), |source| {
                    source.name.as_str()
                });

            column![
                row![
                    text(&adoption.name).font(style::BOLD).width(Fill),
                    text(source).size(13).style(text::secondary),
                ]
                .spacing(8),
                text(format!("Folders: {}", adoption.folders.join(", ")))
                    .size(12)
                    .style(text::secondary),
            ]
            .spacing(2)
            .into()
        });

        Some(
            container(
                column![
                    text("Addons to recognize").size(20).font(style::BOLD),
                    text(
                        "These folders in Interface/AddOns match a catalog entry exactly. \
                         Recognizing them lists them as installed from that entry, so they \
                         can be updated, reinstalled and uninstalled like any other. Their \
                         files stay as they are: each folder only gets a small \
                         .old_launcher.toml file recording where it comes from."
                    )
                    .style(text::secondary),
                    container(
                        scrollable(column(addons).spacing(10).padding(padding::right(14)))
                            .height(Fit.max(360))
                            .width(Fill)
                    )
                    .padding(10)
                    .width(Fill)
                    .style(container::rounded_box),
                    row![
                        space::horizontal(),
                        button("Close").style(button::text).on_press(Message::Close),
                        self.accept(),
                    ]
                    .spacing(8),
                ]
                .spacing(14),
            )
            .padding(24)
            .width(Fill.max(620))
            .style(style::card)
            .into(),
        )
    }

    /// The button that recognizes the proposed addons.
    fn accept(&self) -> Element<'_, Message> {
        button(if self.working {
            "Recognizing…"
        } else {
            "Recognize"
        })
        .style(button::primary)
        .on_press_maybe((!self.working).then_some(Message::Accept))
        .into()
    }
}

/// What identifies an adoption across scans: its folders, lowercase and
/// sorted.
fn key(adoption: &Adoption) -> Vec<String> {
    let mut folders: Vec<String> = adoption
        .folders
        .iter()
        .map(|folder| folder.to_lowercase())
        .collect();
    folders.sort();
    folders
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Marker;
    use crate::testing;
    use iced_test::simulator;

    fn catalog() -> Catalog {
        Catalog::parse(
            br#"{ "version": 2,
                  "sources": [{ "id": "nom0re", "name": "NoM0Re", "homepage": "https://example.com" }],
                  "entries": [{ "name": "Addon Control Panel",
                                "downloads": [{ "source": "nom0re", "name": "ACP",
                                                "url": "https://example.com/ACP.zip",
                                                "folders": [{ "name": "ACP" }] }] }] }"#,
        )
        .unwrap()
    }

    #[test]
    fn matching_addons_are_proposed_and_recognized_on_request() {
        let (_root, installation) = testing::client(&["Interface/AddOns/ACP/ACP.toc"]);
        let catalog = catalog();
        let mut state = State::default();
        let mut errors = Errors::default();

        state.propose(&installation.scan().unwrap(), &catalog.entries);

        let mut ui = simulator(state.banner().expect("a proposal"));
        assert!(
            ui.find(
                "Addon Control Panel was installed without the launcher and matches the \
                 catalog. Recognize it, so it shows as installed and can be updated from here?"
            )
            .is_ok()
        );
        let _ = ui.click("Details").unwrap();
        for message in ui.into_messages() {
            let _ = state.update(message, Some(&installation), &mut errors);
        }

        let mut ui = simulator(state.dialog(&catalog).expect("the list"));
        assert!(ui.find("Addons to recognize").is_ok());
        assert!(
            ui.find("NoM0Re").is_ok(),
            "the source it would be recorded under"
        );
        assert!(ui.find("Folders: ACP").is_ok());
        let _ = ui.click("Recognize").unwrap();
        let messages: Vec<Message> = ui.into_messages().collect();
        for message in messages {
            let _ = state.update(message, Some(&installation), &mut errors);
        }
        assert!(state.working, "writing the markers");

        // The task itself isn't run here; this is what it reports.
        let adopted = Adoption::plan(&installation.scan().unwrap(), &catalog.entries);
        let action = state.update(
            Message::Adopted(Ok(adopted)),
            Some(&installation),
            &mut errors,
        );

        assert!(matches!(
            action.instruction,
            Some(Instruction::Recognized(1))
        ));
        assert!(state.banner().is_none() && !state.details_open());
        assert!(
            Marker::read(&installation.addons_directory().join("ACP")).is_none(),
            "nothing was written by the test itself"
        );
    }

    #[test]
    fn declined_addons_are_not_proposed_again() {
        let (_root, installation) = testing::client(&["Interface/AddOns/ACP/ACP.toc"]);
        let catalog = catalog();
        let snapshot = installation.scan().unwrap();
        let mut state = State::default();
        let mut errors = Errors::default();
        state.propose(&snapshot, &catalog.entries);

        let mut ui = simulator(state.banner().expect("a proposal"));
        let _ = ui.click("Not now").unwrap();
        for message in ui.into_messages() {
            let _ = state.update(message, Some(&installation), &mut errors);
        }

        assert!(state.banner().is_none());
        state.propose(&snapshot, &catalog.entries);
        assert!(state.banner().is_none(), "declined for this run");
    }
}
