//! Deletions the addons screen asks about before going ahead.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use iced::widget::{button, checkbox, column, container, row, scrollable, space, text};
use iced::{Element, Fill, Fit};

use super::Message;
use crate::core::saved_variables::Scope;
use crate::core::{Error, Installation};
use crate::{counted, style};

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
    /// client's `root`.
    pub fn view<'a>(&'a self, root: &'a Path) -> Element<'a, Message> {
        let relative = |file: &PathBuf| {
            file.strip_prefix(root)
                .unwrap_or(file)
                .display()
                .to_string()
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
                if *delete_data {
                    "These folders and settings files will be deleted."
                } else {
                    "These folders will be deleted from Interface/AddOns."
                },
                folders
                    .iter()
                    .map(|folder| format!("Interface/AddOns/{folder}"))
                    .chain(data.iter().filter(|_| *delete_data).map(relative))
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
                "These SavedVariables files will be deleted, resetting the settings they hold. \
                 Close the game first: it rewrites them when you log out.",
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
                    let consequence = if *delete_data {
                        "Close the game first: it rewrites them when you log out."
                    } else {
                        "They stay in WTF for a reinstall, and show under Leftover data."
                    };

                    column![
                        checkbox(*delete_data)
                            .label(format!("Also delete its saved settings ({files})"))
                            .on_toggle(Message::DeleteData),
                        text(consequence).size(13).style(text::secondary),
                    ]
                    .spacing(4)
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

                column![]
                    .push(required)
                    .push(optional)
                    .push(settings)
                    .spacing(6)
            }
            Self::Repair { .. } | Self::Clean { .. } => column![],
        };

        container(
            column![
                text(title).size(20).font(style::BOLD),
                text(explanation).style(text::secondary),
                warnings,
                container(
                    scrollable(
                        column(items.into_iter().map(|item| text(item).size(13).into())).spacing(2)
                    )
                    .height(Fit.max(240))
                    .width(Fill)
                )
                .padding(10)
                .width(Fill)
                .style(container::rounded_box),
                row![
                    space::horizontal(),
                    button("Cancel")
                        .style(button::text)
                        .on_press(Message::Cancel),
                    button(action)
                        .style(button::danger)
                        .on_press(Message::Confirm),
                ]
                .spacing(8),
            ]
            .spacing(14),
        )
        .padding(24)
        .width(Fill.max(560))
        .style(style::card)
        .into()
    }
}

/// Names as a sentence: `A`, `A and B`, `A, B and C`.
pub fn names(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
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
