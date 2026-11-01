//! What the addons screen asks about before going ahead: deletions, and
//! installs that take folders away from other addons.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use iced::widget::{button, checkbox, column, container, row, scrollable, space, text};
use iced::{Fill, Fit, Widget};

use super::{Key, Message};
use crate::core::saved_variables::Scope;
use crate::core::{Error, Installation};
use crate::{counted, game, style};

/// A deletion waiting for the user's confirmation.
#[derive(Debug, Clone)]
pub enum Confirm {
    /// Delete addon folders.
    Uninstall {
        /// What the user knows the folders as.
        title: String,
        /// The folders under `Interface/AddOns`.
        folders: Vec<String>,
        /// Their SavedVariables files under `WTF`.
        data: Vec<PathBuf>,
        /// Whether to delete `data` too. Once the folders are gone, nothing
        /// on screen belongs to the data anymore but the leftover data list.
        delete_data: bool,
        /// Installed addons that will not load without these folders.
        required_by: Vec<String>,
        /// Installed addons that use these folders when present.
        optionally_used_by: Vec<String>,
    },
    /// Move the addons nested in a folder the client cannot load into place.
    Repair {
        /// The folder under `Interface/AddOns`.
        folder: String,
    },
    /// Delete SavedVariables files.
    Clean {
        /// What the user knows the data as.
        title: String,
        /// The files under `WTF`.
        files: Vec<PathBuf>,
    },
}

impl Confirm {
    /// What is about to happen, for the log.
    pub fn describe(&self) -> String {
        match self {
            Self::Uninstall {
                title,
                folders,
                data,
                delete_data,
                ..
            } => format!(
                "Uninstalling {title}: {}{}",
                counted(folders.len(), "folder"),
                if *delete_data {
                    format!(", and {} of saved settings", counted(data.len(), "file"))
                } else {
                    String::new()
                }
            ),
            Self::Repair { folder } => format!("Repairing {folder}"),
            Self::Clean { title, files } => format!(
                "Deleting the saved settings of {title}: {}",
                counted(files.len(), "file")
            ),
        }
    }

    /// What failed, when [`Confirm::run`] does.
    pub fn failure(&self) -> String {
        match self {
            Self::Uninstall { title, .. } => format!("Could not uninstall {title}"),
            Self::Repair { folder } => format!("Could not repair {folder}"),
            Self::Clean { title, .. } => format!("Could not delete the saved data of {title}"),
        }
    }

    /// Leaves out what the game would undo while it runs, as it writes saved
    /// settings back when it logs out: an uninstall keeps them, and deleting
    /// only them can't go ahead (false).
    pub fn while_playing(&mut self) -> bool {
        match self {
            Self::Uninstall { delete_data, .. } => {
                *delete_data = false;
                true
            }
            Self::Repair { .. } => true,
            Self::Clean { .. } => false,
        }
    }

    /// Carries out the deletion in `installation`.
    pub fn run(self, installation: &Installation) -> Result<(), Error> {
        match self {
            Self::Uninstall {
                folders,
                data,
                delete_data,
                ..
            } => {
                installation.uninstall(&folders)?;

                if delete_data {
                    installation.clean(&data)?;
                }

                Ok(())
            }
            Self::Repair { folder } => installation.repair(&folder).map(|_| ()),
            Self::Clean { files, .. } => installation.clean(&files),
        }
    }

    /// The dialog asking for confirmation, with paths shown relative to the
    /// client's `root`. While its game runs (`playing`), saved settings are
    /// left alone.
    pub fn view<'a>(&'a self, root: &'a Path, playing: bool) -> impl Widget<Message> + 'a {
        // With the slashes the rest of the dialog uses, on Windows too.
        let relative = |file: &PathBuf| match file.strip_prefix(root) {
            Ok(relative) => relative
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/"),
            Err(_) => file.display().to_string(),
        };

        let (title, explanation, items, action) = match self {
            Self::Uninstall {
                title,
                folders,
                data,
                delete_data,
                ..
            } => (
                format!("Uninstall {title}?"),
                if *delete_data && !playing {
                    "These folders and settings files will be deleted."
                } else {
                    "These folders will be deleted from Interface/AddOns."
                },
                folders
                    .iter()
                    .map(|folder| format!("Interface/AddOns/{folder}"))
                    .chain(
                        data.iter()
                            .filter(|_| *delete_data && !playing)
                            .map(relative),
                    )
                    .collect::<Vec<_>>(),
                "Uninstall",
            ),
            Self::Repair { folder } => (
                format!("Repair {folder}?"),
                "The client only loads Interface/AddOns/<Name>/<Name>.toc. Every addon folder \
                 inside this one moves up into Interface/AddOns, named after its .toc file, \
                 replacing folders with the same name. Everything else in it is deleted.",
                vec![format!("Interface/AddOns/{folder}")],
                "Repair",
            ),
            Self::Clean { title, files } => (
                format!("Delete the saved data of {title}?"),
                "These SavedVariables files will be deleted, resetting the settings they hold.",
                files.iter().map(relative).collect(),
                "Delete",
            ),
        };

        let warnings = match self {
            Self::Uninstall {
                title,
                data,
                delete_data,
                required_by,
                optionally_used_by,
                ..
            } => {
                let settings = (!data.is_empty()).then(|| {
                    let files = if data.len() == 1 {
                        "1 file".to_owned()
                    } else {
                        format!("{} files", data.len())
                    };
                    let label = format!("Also delete its saved settings ({files})");
                    let (checkbox, consequence) = if playing {
                        (
                            checkbox(false).label(label).style(style::check),
                            text(game::KEEPS_SETTINGS).style(text::warning),
                        )
                    } else {
                        (
                            checkbox(*delete_data)
                                .style(style::check)
                                .label(label)
                                .on_toggle(Message::DeleteData),
                            text(if *delete_data {
                                "Deleting them resets the settings they hold."
                            } else {
                                "They stay in WTF for a reinstall, and show under Leftover data."
                            })
                            .style(text::secondary),
                        )
                    };

                    column![checkbox, consequence.size(13)].spacing(4)
                });
                let required = (!required_by.is_empty()).then(|| {
                    let verb = if required_by.len() == 1 {
                        "requires"
                    } else {
                        "require"
                    };

                    text(format!(
                        "{} {verb} {title} and will not load without it.",
                        names(required_by)
                    ))
                    .style(text::warning)
                });
                let optional = (!optionally_used_by.is_empty()).then(|| {
                    let verb = if optionally_used_by.len() == 1 {
                        "uses"
                    } else {
                        "use"
                    };

                    text(format!(
                        "{} optionally {verb} {title}; the rest keeps working without it.",
                        names(optionally_used_by)
                    ))
                    .style(text::secondary)
                });

                column![required, optional, settings].spacing(6)
            }
            Self::Clean { .. } if playing => {
                column![text(game::KEEPS_SETTINGS).style(text::warning)]
            }
            Self::Repair { .. } | Self::Clean { .. } => column![],
        };
        let blocked = playing && matches!(self, Self::Clean { .. });

        frame(
            title,
            explanation.to_owned(),
            warnings,
            items,
            action.to_owned(),
            blocked,
        )
    }
}

/// An install over folders that other addons hold, waiting for the user's
/// say: the install goes ahead only once they agree to what it takes away,
/// and not at all while it would break an addon that needs what goes.
#[derive(Debug, Clone)]
pub struct Conflict {
    /// The entry to install.
    pub key: Key,
    /// What the user knows the entry as.
    pub name: String,
    /// The addons that hold a folder the entry installs, whether installed
    /// from the list or by hand. They are uninstalled first, keeping their
    /// saved settings.
    pub uninstall: Vec<Uninstall>,
}

/// An addon that an install takes a folder from, so that it is uninstalled
/// first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uninstall {
    /// What the user knows it as.
    pub name: String,
    /// Every folder of it, as named on disk.
    pub folders: Vec<String>,
    /// Installed addons that will not load without a folder of it that the
    /// install doesn't bring back.
    pub required_by: Vec<String>,
}

impl Conflict {
    /// The folders removed before the entry's own go in.
    pub fn replacing(&self) -> Vec<String> {
        self.uninstall
            .iter()
            .flat_map(|addon| addon.folders.iter().cloned())
            .collect()
    }

    /// The names of the addons uninstalled first.
    fn uninstalled(&self) -> Vec<String> {
        self.uninstall
            .iter()
            .map(|addon| addon.name.clone())
            .collect()
    }

    /// Whether the install can't go ahead: an addon that stays needs one
    /// that goes.
    pub fn blocked(&self) -> bool {
        self.uninstall
            .iter()
            .any(|addon| !addon.required_by.is_empty())
    }

    /// What the install takes away, for the log.
    pub fn describe(&self) -> String {
        format!(
            "Installing {}: uninstalling {} first",
            self.name,
            names(&self.uninstalled())
        )
    }

    /// The dialog asking whether to go ahead.
    pub fn view(&self) -> impl Widget<Message> + '_ {
        let addons = self.uninstalled();
        let owners = if addons.len() == 1 { "addon" } else { "addons" };
        let question = text(format!(
            "Do you want to uninstall {} first, and then install \"{}\"?",
            quoted(&addons),
            self.name
        ));
        let required = self
            .uninstall
            .iter()
            .filter(|addon| !addon.required_by.is_empty())
            .map(|addon| {
                let (verb, them) = if addon.required_by.len() == 1 {
                    ("requires", "it")
                } else {
                    ("require", "them")
                };

                text(format!(
                    "{} {verb} \"{}\", so this replacement would break {them}. Uninstall {} first.",
                    quoted(&addon.required_by),
                    addon.name,
                    quoted(&addon.required_by),
                ))
                .style(text::warning)
            });
        let items = self
            .uninstall
            .iter()
            .flat_map(|addon| {
                addon
                    .folders
                    .iter()
                    .map(move |folder| format!("Interface/AddOns/{folder} — {}", addon.name))
            })
            .collect();

        frame(
            format!("Install {}?", self.name),
            format!(
                "Some folders of \"{}\" already exist in Interface/AddOns, owned by {owners} {}.",
                self.name,
                quoted(&addons)
            ),
            column(std::iter::once(question).chain(required)).spacing(6),
            items,
            format!("Replace {}", names(&addons)),
            self.blocked(),
        )
    }
}

/// A dialog: its `title`, an `explanation` under it, `warnings` where there
/// are any, the `items` it is about in a list, and the button `action` that
/// goes ahead beside Cancel, disabled while `blocked`.
fn frame<'a>(
    title: String,
    explanation: String,
    warnings: impl Widget<Message> + 'a,
    items: Vec<String>,
    action: String,
    blocked: bool,
) -> impl Widget<Message> + 'a {
    container(
        column![
            style::heading(title, 20.0),
            text(explanation).style(text::secondary),
            warnings,
            container(
                scrollable(column(items.into_iter().map(|item| text(item).size(13))).spacing(2))
                    .height(Fit.max(240))
                    .style(style::scroll)
                    .width(Fill)
            )
            .padding(10)
            .width(Fill)
            .style(style::inset),
            row![
                space::horizontal(),
                button("Cancel")
                    .style(style::quiet)
                    .on_press(Message::Cancel),
                button(text(action))
                    .style(style::danger)
                    .on_press_maybe((!blocked).then_some(Message::Confirm)),
            ]
            .spacing(8),
        ]
        .spacing(14),
    )
    .padding(24)
    .width(Fill.max(560))
    .style(style::card)
}

/// Names as a sentence: `A`, `A and B`, `A, B and C`.
pub fn names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// [`names`] with each in quotes: `"A" and "B"`.
fn quoted(list: &[String]) -> String {
    names(
        &list
            .iter()
            .map(|name| format!("\"{name}\""))
            .collect::<Vec<_>>(),
    )
}

/// What "Clean data" would delete, in one sentence: how many files, and
/// whose settings they hold. The confirmation lists the files themselves.
pub fn data_summary(files: &[PathBuf]) -> String {
    if files.is_empty() {
        return "No saved settings found.".to_owned();
    }

    let mut account = 0;
    let mut characters = BTreeSet::new();
    let mut character_files = 0;

    for file in files {
        match Scope::of(file) {
            Some(Scope::Account(_)) => account += 1,
            Some(Scope::Character { realm, name }) => {
                character_files += 1;
                let _ = characters.insert((realm, name));
            }
            None => {}
        }
    }

    let total = counted(files.len(), "file");
    let characters = counted(characters.len(), "character");

    match (account, character_files) {
        (0, 0) => format!("Saved settings in {total}."),
        (_, 0) => format!("Saved settings in {total}, shared by the whole account."),
        (0, _) => format!("Saved settings in {total}, for {characters}."),
        (account, character_files) => format!(
            "Saved settings in {total}: {account} shared by the whole account, and \
             {character_files} for {characters}."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::Confirm;
    use crate::testing;

    #[test]
    fn uninstalling_keeps_the_saved_settings_unless_asked() {
        let (_root, installation) = testing::client(&[
            "Interface/AddOns/Skada/Skada.toc",
            "WTF/Account/ME/SavedVariables/Skada.lua",
        ]);
        let data = installation
            .scan()
            .unwrap()
            .data_files(&installation.scan().unwrap().groups()[0]);

        Confirm::Uninstall {
            title: "Skada".into(),
            folders: vec!["Skada".into()],
            data: data.clone(),
            delete_data: false,
            required_by: Vec::new(),
            optionally_used_by: Vec::new(),
        }
        .run(&installation)
        .unwrap();

        assert!(!installation.addons_directory().join("Skada").exists());
        assert!(data[0].exists());
    }

    #[test]
    fn while_the_game_runs_only_the_folders_go() {
        let (_root, installation) = testing::client(&[
            "Interface/AddOns/Skada/Skada.toc",
            "WTF/Account/ME/SavedVariables/Skada.lua",
        ]);
        let data = installation
            .scan()
            .unwrap()
            .data_files(&installation.scan().unwrap().groups()[0]);
        let mut uninstall = Confirm::Uninstall {
            title: "Skada".into(),
            folders: vec!["Skada".into()],
            data: data.clone(),
            delete_data: true,
            required_by: Vec::new(),
            optionally_used_by: Vec::new(),
        };

        assert!(uninstall.while_playing());
        uninstall.run(&installation).unwrap();

        assert!(!installation.addons_directory().join("Skada").exists());
        assert!(data[0].exists(), "the game would write it back anyway");
        assert!(
            !Confirm::Clean {
                title: "Skada".into(),
                files: data,
            }
            .while_playing()
        );
        assert!(
            Confirm::Repair {
                folder: "Skada".into(),
            }
            .while_playing()
        );
    }

    #[test]
    fn names_read_as_a_sentence() {
        let list = |names: &[&str]| {
            super::names(
                &names
                    .iter()
                    .map(|name| name.to_string())
                    .collect::<Vec<_>>(),
            )
        };

        assert_eq!(list(&["A"]), "A");
        assert_eq!(list(&["A", "B"]), "A and B");
        assert_eq!(list(&["A", "B", "C"]), "A, B and C");
    }
}
