//! What installing a release would do to the game's files, as the archives
//! see it: which are added and which are gone.

/// What an install changes in the client folder, for the game: a file
/// backed up is gone, as one removed is. Paths are relative to the client
/// folder, with `/` between folders, as spelled on disk.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Change {
    /// The files placed: path and SHA-256.
    pub adds: Vec<(String, String)>,
    /// The paths that are gone: removed or backed up.
    pub removes: Vec<String>,
}
