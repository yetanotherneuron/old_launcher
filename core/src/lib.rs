//! Data and filesystem logic for managing the addons of WoW 3.3.5a clients.
//!
//! Nothing here knows about the GUI or the network: the app and the
//! `indexer` download bytes and hand them over. There is no database either;
//! everything is read from the client folder itself (`Interface/AddOns`,
//! `WTF`), with a small [`Marker`] file inside each addon folder the launcher
//! installs.
#![warn(missing_docs)]

pub mod addon;
pub mod adoption;
pub mod archive;
pub mod catalog;
pub mod config;
pub mod error;
pub mod installation;
pub mod launch;
pub mod layout;
pub mod library;
pub mod marker;
pub mod saved_variables;
pub mod search;
pub mod snapshot;
pub mod toc;
pub mod transfer;
pub mod version;

pub use addon::Addon;
pub use adoption::Adoption;
pub use catalog::Catalog;
pub use config::Config;
pub use error::Error;
pub use installation::{Details, Installation, Realmlist};
pub use launch::Launch;
pub use marker::Marker;
pub use saved_variables::SavedVariables;
pub use snapshot::Snapshot;
pub use toc::Toc;
pub use version::Version;
