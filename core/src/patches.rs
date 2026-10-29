//! What the archives of a client's `Data` folder say about each other: which
//! files they hold, which of those reference other files (the tables with
//! paths in their columns, the models that need skins, animations and
//! textures), and so what each archive needs.
//!
//! This is the first layer: [`facts`] reads what one archive says, as pure
//! functions over bytes. The launcher keeps the result of each archive in
//! its own cache, stamped with the archive's size and modification time, so
//! an archive is read once until it changes.

pub mod facts;

pub use facts::{Facts, Kind};

/// The names of the archives Blizzard ships with the 3.3.5a client, without
/// folder or extension and in any case, [`LOCALE`](crate::mods::LOCALE)
/// standing for the language of the folders they live in (`Data` for the
/// first seven, `Data/<language>` for the rest). Everything else in `Data`
/// is custom: the player's, a torrent's or a mod's. The game never loads
/// `base-` or `backup-`, so they are not here.
pub const BLIZZARD: &[&str] = &[
    "expansion",
    "lichking",
    "common",
    "common-2",
    "patch",
    "patch-2",
    "patch-3",
    "locale-{locale}",
    "speech-{locale}",
    "expansion-locale-{locale}",
    "lichking-locale-{locale}",
    "expansion-speech-{locale}",
    "lichking-speech-{locale}",
    "patch-{locale}",
    "patch-{locale}-2",
    "patch-{locale}-3",
];
