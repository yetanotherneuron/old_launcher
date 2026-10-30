//! What installing a mod's release would do to the client's patches, in
//! words: the gate of the install.
//!
//! The core simulates the install on the graph of the client's archives
//! (see [`Graph::gate`]) and says which needs it would **break**, which
//! files it would **override** of a custom archive's, and which of its own
//! a custom archive would override. This turns that into the sentences the
//! row and the confirm box show, naming the mod an archive belongs to
//! (`Mods::owned`) and calling every other custom archive the player's:
//! "Your custom Data/patch-G.mpq's models need Data/patch-F.mpq, which
//! installing “Patch Y” would override." Only the breaks of other archives
//! count: what the release's own tables name that the install replaces is
//! not told (the files it sets aside are, in the confirm box).
//!
//! [`judge`] does it for every mod of the list whose release's archives
//! have their facts in the launcher's cache, before anything is pressed;
//! [`install_of`] for one whose archives were just read.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use super::names;
use crate::core::Locale;
use crate::core::mods::{self, Mod, Mods, Release, Status};
use crate::core::mpq::{Key, key};
use crate::core::patches::graph::{Break, Named};
use crate::core::patches::{Cache, Change, Facts, Gate, Graph, Kind, is_blizzard};
use crate::patches::{Scan, words};

/// How many files or archives a sentence names before "and N more".
const FEW: usize = 4;

/// The kinds of files, in the order sentences list them.
const ORDER: [Kind; 7] = [
    Kind::Models,
    Kind::Tables,
    Kind::Textures,
    Kind::Sounds,
    Kind::Interface,
    Kind::World,
    Kind::Other,
];

/// Why an install is blocked: the short sentence the row and the banner
/// say, and the detailed ones that show on hover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    /// "This mod would break one of your patches."
    pub short: String,
    /// What would break, archive by archive.
    pub details: String,
}

/// What installing the newest release of a mod does to the patches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Install {
    /// Why it can't be installed: it would break the needs of another
    /// archive. No "anyway".
    pub blocked: Option<Blocked>,
    /// What the player is asked about first: files it would override, and
    /// what would override it.
    pub asks: Vec<String>,
}

/// What is known of the patches of a mod, from the client's graph.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Verdict {
    /// What installing its newest release does, once the facts of its
    /// archives are known.
    pub install: Option<Install>,
    /// What overrides the archives it has installed, if anything does.
    pub installed: Vec<String>,
}

/// Whether the release has an archive of the game's among its files.
pub fn has_archives(release: &Release) -> bool {
    release
        .files
        .iter()
        .chain(release.languages.values().flatten())
        .any(|(path, _)| mods::is_archive(path))
}

/// Who the archives of a client belong to.
struct Owners {
    /// The id of the mod each path (in lowercase) was installed by.
    owned: BTreeMap<String, String>,
    /// The name of each mod, by id.
    names: BTreeMap<String, String>,
}

impl Owners {
    /// The owners of the archives of the client folder at `root`, as the
    /// mods of `list` installed them.
    fn of(list: &Mods, root: &Path) -> Self {
        Self {
            owned: list.owned(root),
            names: list
                .mods
                .iter()
                .map(|listed| (listed.id.clone(), listed.name.clone()))
                .collect(),
        }
    }

    /// The id of the mod `path` was installed by.
    fn id(&self, path: &str) -> Option<&str> {
        self.owned
            .get(&path.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// The name of the mod `path` was installed by.
    fn name(&self, path: &str) -> Option<&str> {
        self.names.get(self.id(path)?).map(String::as_str)
    }

    /// `path` as a sentence's subject: the mod's, or the player's.
    fn whose(&self, path: &str) -> String {
        match self.name(path) {
            Some(name) => format!("“{name}”'s {path}"),
            None => path.to_owned(),
        }
    }
}

/// The files of one archive, as a sentence tells them: tables by name, the
/// rest by count.
#[derive(Default)]
struct Files {
    tables: Vec<String>,
    counts: BTreeMap<Kind, usize>,
}

impl Files {
    fn add(&mut self, file: &Named) {
        match (&file.name, file.kind) {
            (Some(name), Kind::Tables) => {
                let short = name.rsplit(['\\', '/']).next().unwrap_or(name);
                if !self.tables.iter().any(|known| known == short) {
                    self.tables.push(short.to_owned());
                }
            }
            (_, kind) => *self.counts.entry(kind).or_default() += 1,
        }
    }

    /// The tables, named one by one, in alphabetical order.
    fn tables(&self) -> Option<String> {
        let mut tables = self.tables.clone();
        tables.sort_by_key(|name| name.to_ascii_lowercase());

        (!tables.is_empty()).then(|| few(&tables))
    }

    /// The other kinds, counted: "212 models and 40 textures".
    fn others(&self) -> Option<String> {
        let counts: Vec<String> = ORDER
            .iter()
            .filter_map(|kind| Some(count(*self.counts.get(kind)?, *kind)))
            .collect();

        (!counts.is_empty()).then(|| names(&counts))
    }
}

/// `items` as a sentence, the first few and then "and N more".
fn few(items: &[String]) -> String {
    match items.get(..FEW) {
        Some(first) if items.len() > FEW => {
            format!("{} and {} more", first.join(", "), items.len() - FEW)
        }
        _ => names(items),
    }
}

/// `number` files of `kind`: "1 model", "40 textures".
fn count(number: usize, kind: Kind) -> String {
    let (one, many) = words(kind);

    format!("{number} {}", if number == 1 { one } else { many })
}

/// The kinds as words, in the order sentences list them: "models and
/// tables".
fn kinds(kinds: &BTreeSet<Kind>) -> String {
    let words: Vec<String> = ORDER
        .iter()
        .filter(|kind| kinds.contains(kind))
        .map(|kind| words(*kind).1.to_owned())
        .collect();

    names(&words)
}

/// What breaks, a sentence for each archive whose files would lose what they
/// need, and for each set of kinds of file that need the same archives.
fn breaks(breaks: &[Break], owners: &Owners, listed: &Mod) -> Vec<String> {
    // By the archive that needs, then by what it needs (and whether it is
    // the mod's own), with the kinds of the files that do.
    type Needs = BTreeMap<String, BTreeMap<(bool, String), BTreeSet<Kind>>>;
    let mut needs: Needs = BTreeMap::new();

    for lost in breaks {
        // What the mod brings now and the new release doesn't: an archive
        // of the mod as installed. A file of something else where the
        // release goes is the player's, whatever its name.
        let own = owners.id(&lost.was_provided_by) == Some(listed.id.as_str());

        let _ = needs
            .entry(lost.archive.clone())
            .or_default()
            .entry((own, lost.was_provided_by.clone()))
            .or_default()
            .insert(lost.file.kind);
    }

    let mut said = Vec::new();

    for (archive, provided) in needs {
        // The archives that are needed for the same kinds of files go in one
        // sentence.
        let mut together: BTreeMap<(bool, Vec<usize>), Vec<String>> = BTreeMap::new();
        for ((own, provider), set) in provided {
            let order: Vec<usize> = ORDER
                .iter()
                .enumerate()
                .filter(|(_, kind)| set.contains(kind))
                .map(|(at, _)| at)
                .collect();

            together.entry((own, order)).or_default().push(provider);
        }

        for ((own, order), providers) in together {
            let set: BTreeSet<Kind> = order
                .iter()
                .filter_map(|at| ORDER.get(*at))
                .copied()
                .collect();
            let kinds = kinds(&set);
            let subject = if let Some(name) = owners.name(&archive) {
                format!("The {kinds} of “{name}”")
            } else {
                format!("Your custom {archive}'s {kinds}")
            };
            let wanted = few(&providers);

            said.push(if own {
                format!("{subject} need what {wanted} has now, which this release leaves out.")
            } else {
                format!(
                    "{subject} need {wanted}, which installing “{}” would override.",
                    listed.name
                )
            });
        }
    }

    said
}

/// What a custom archive's files overriding (or being overridden by) the
/// mod's are, a sentence for the tables by name and one for the rest
/// counted, for each archive: `subject` is the archive, `verb` what it does
/// and `of` whose files they are.
fn overlaps(
    by_archive: BTreeMap<String, Files>,
    subject: impl Fn(&str) -> String,
    verb: &str,
    of: &str,
) -> Vec<String> {
    let mut said = Vec::new();

    for (archive, files) in by_archive {
        let subject = subject(&archive);

        if let Some(tables) = files.tables() {
            said.push(format!("{subject} {verb} the tables {of}: {tables}."));
        }
        if let Some(others) = files.others() {
            said.push(format!("{subject} {verb} {others} {of}."));
        }
    }

    said
}

/// What `gate`, the simulation of installing a release of `listed`, says,
/// the archives belonging to `owners`.
fn sentences(gate: &Gate, owners: &Owners, listed: &Mod) -> Install {
    let broken = breaks(&gate.breaks, owners, listed);
    let mut asks = Vec::new();

    let mut replaced: BTreeMap<String, Files> = BTreeMap::new();
    for overlap in &gate.overrides {
        replaced
            .entry(overlap.loser.clone())
            .or_default()
            .add(&overlap.file);
    }
    for (archive, files) in replaced {
        let whose = match owners.name(&archive) {
            Some(name) => format!("“{name}”"),
            None => format!("your custom {archive}"),
        };
        let installing = format!("Installing “{}” would override", listed.name);

        if let Some(tables) = files.tables() {
            asks.push(format!(
                "{installing} the tables {whose} replaces: {tables}."
            ));
        }
        if let Some(others) = files.others() {
            asks.push(format!("{installing} {others} {whose} replaces."));
        }
    }

    let mut winners: BTreeMap<String, Files> = BTreeMap::new();
    for overlap in &gate.overridden {
        winners
            .entry(overlap.winner.clone())
            .or_default()
            .add(&overlap.file);
    }
    asks.extend(overlaps(
        winners,
        |archive| owners.whose(archive),
        "would override",
        &format!("“{}” brings", listed.name),
    ));

    // The archives that would break, counted for the short sentence.
    let archives: BTreeSet<&str> = gate
        .breaks
        .iter()
        .map(|lost| lost.archive.as_str())
        .collect();

    Install {
        blocked: (!broken.is_empty()).then(|| Blocked {
            short: match archives.len() {
                0 | 1 => "This mod would break one of your patches.".to_owned(),
                many => format!("This mod would break {many} of your patches."),
            },
            details: broken.join(" "),
        }),
        asks,
    }
}

/// What installing a release of `listed` that makes `change` in the client
/// folder at `root` does to the patches in `graph`, `facts` being those of
/// the release's archives by checksum. Nothing when the release can't go in
/// the client.
pub fn install_in(
    root: &Path,
    list: &Mods,
    listed: &Mod,
    change: &Change,
    graph: &Graph,
    facts: &BTreeMap<String, Facts>,
) -> Option<Install> {
    install_with(&Owners::of(list, root), listed, change, graph, facts)
}

/// [`install_in`], the archives belonging to `owners`.
fn install_with(
    owners: &Owners,
    listed: &Mod,
    change: &Change,
    graph: &Graph,
    facts: &BTreeMap<String, Facts>,
) -> Option<Install> {
    if change.adds.is_empty() {
        return None;
    }

    Some(sentences(&graph.gate(change, facts), owners, listed))
}

/// What installing `release` of `listed` over the client folder at `root`
/// changes there, the languages it lacks being those of `instead` (see
/// [`mods::change`]).
pub fn change_of(
    root: &Path,
    list: &Mods,
    listed: &Mod,
    release: &Release,
    instead: Option<Locale>,
) -> Change {
    mods::change(root, listed, release, &list.alternatives(listed), instead)
}

/// What overrides the archives `listed` has installed in the client folder
/// at `root`, in `graph`, from the facts of its release's archives in
/// `cache`: "Data/patch-w.mpq overrides the tables it brings: …".
fn installed_of(
    root: &Path,
    owners: &Owners,
    listed: &Mod,
    graph: &Graph,
    locale: Locale,
    cache: &Cache,
) -> Vec<String> {
    let archives = mods::installed_archives(root, listed);
    let own: BTreeSet<String> = archives
        .iter()
        .map(|(path, _)| path.to_ascii_lowercase())
        .collect();
    let notes: Vec<Key> = ["(listfile)", "(attributes)", "(signature)"]
        .into_iter()
        .map(key)
        .collect();
    let mut winners: BTreeMap<String, Files> = BTreeMap::new();
    let mut seen = BTreeSet::new();

    for (_, sum) in &archives {
        let Some(facts) = cache.release(sum) else {
            continue;
        };

        for (file, entry) in &facts.files {
            if entry.deleted || notes.contains(file) {
                continue;
            }
            let Some(provider) = graph.provider(*file) else {
                continue;
            };

            // The mod's own archives overriding each other are no concern,
            // and neither are Blizzard's.
            if own.contains(&provider.to_ascii_lowercase())
                || owners.id(provider) == Some(listed.id.as_str())
                || is_blizzard(provider, locale)
                || !seen.insert((provider.to_owned(), *file))
            {
                continue;
            }

            winners.entry(provider.to_owned()).or_default().add(&Named {
                key: *file,
                name: facts
                    .names
                    .get(file)
                    .cloned()
                    .or_else(|| graph.name(*file).map(str::to_owned)),
                kind: entry.kind,
            });
        }
    }

    overlaps(
        winners,
        |archive| owners.whose(archive),
        "overrides",
        "it brings",
    )
}

/// What the client folder at `root`, whose mods are `found`, says of every
/// mod of `list` with archives, from `scan`'s graph and what `cache` knows
/// of their archives: for the newest release, once all its archives are
/// known, and for what is installed, the languages a mod lacks being those
/// `instead` has for it by id. Mods without archives have no entry.
pub fn judge(
    root: &Path,
    list: &Mods,
    found: &BTreeMap<String, Status>,
    scan: &Scan,
    locale: Locale,
    instead: &BTreeMap<String, Locale>,
    cache: &Cache,
) -> BTreeMap<String, Verdict> {
    let mut verdicts = BTreeMap::new();
    let owners = Owners::of(list, root);

    for listed in &list.mods {
        let Some(newest) = listed.newest().filter(|release| has_archives(release)) else {
            continue;
        };
        let status = found.get(&listed.id);
        let installable = status.is_none_or(|status| {
            status.release.as_deref() != Some(newest.version.as_str())
                || status
                    .problems
                    .iter()
                    .any(|problem| matches!(problem, mods::Problem::Unknown { .. }))
        });
        let mut verdict = Verdict::default();

        if installable {
            let change = change_of(root, list, listed, newest, instead.get(&listed.id).copied());
            let facts: Option<BTreeMap<String, Facts>> = change
                .adds
                .iter()
                .filter(|(path, _)| mods::is_archive(path))
                .map(|(_, sum)| Some((sum.clone(), cache.release(sum)?)))
                .collect();

            verdict.install =
                facts.and_then(|facts| install_with(&owners, listed, &change, &scan.graph, &facts));
        }
        if status.is_some() {
            verdict.installed = installed_of(root, &owners, listed, &scan.graph, locale, cache);
        }

        if verdict.install.is_some() || !verdict.installed.is_empty() {
            let _ = verdicts.insert(listed.id.clone(), verdict);
        }
    }

    verdicts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::mods::Source;
    use crate::core::patches::graph::Overlap;

    /// What an install's block says on hover.
    fn details(install: &Install) -> Option<&str> {
        install.blocked.as_ref().map(|block| block.details.as_str())
    }

    /// A mod called “Patch Y”.
    fn patch_y() -> Mod {
        Mod {
            id: "patch-y".to_owned(),
            name: "Patch Y".to_owned(),
            description: String::new(),
            source: Source {
                name: "Patch Y on GitHub".to_owned(),
                page: "https://example.com/patch-y".to_owned(),
            },
            releases: Vec::new(),
            settings: Vec::new(),
            writes: Vec::new(),
            config: BTreeMap::new(),
            platforms: Vec::new(),
            requires: Vec::new(),
            excludes: Vec::new(),
            conflicts: Vec::new(),
            alternatives: Vec::new(),
            version_in: None,
        }
    }

    /// Owners of archives: (path in lowercase, mod id) and (mod id, name).
    fn owners(owned: &[(&str, &str)], names: &[(&str, &str)]) -> Owners {
        Owners {
            owned: owned
                .iter()
                .map(|(path, id)| ((*path).to_owned(), (*id).to_owned()))
                .collect(),
            names: names
                .iter()
                .map(|(id, name)| ((*id).to_owned(), (*name).to_owned()))
                .collect(),
        }
    }

    fn named(name: &str) -> Named {
        Named {
            key: key(name),
            name: Some(name.to_owned()),
            kind: Kind::of(name),
        }
    }

    fn lost(archive: &str, file: &str, needs: &str, by: &str) -> Break {
        Break {
            archive: archive.to_owned(),
            file: named(file),
            needs: named(needs),
            was_provided_by: by.to_owned(),
        }
    }

    fn overlap(file: &str, winner: &str, loser: &str) -> Overlap {
        Overlap {
            file: named(file),
            winner: winner.to_owned(),
            loser: loser.to_owned(),
        }
    }

    #[test]
    fn what_breaks_is_told_by_the_needing_archive_and_by_what_it_needs() {
        let none = owners(&[], &[]);
        let table = "DBFilesClient\\T.dbc";
        let gate = Gate {
            breaks: vec![
                lost("Data/patch-G.mpq", "a.m2", "a00.skin", "Data/patch-F.mpq"),
                lost("Data/patch-G.mpq", table, "b.blp", "Data/patch-F.mpq"),
                lost("Data/patch-G.mpq", "a.m2", "c00.skin", "Data/patch-E.mpq"),
                lost("Data/patch-G.mpq", table, "d.blp", "Data/patch-E.mpq"),
            ],
            ..Gate::default()
        };

        assert_eq!(
            details(&sentences(&gate, &none, &patch_y())),
            Some(
                "Your custom Data/patch-G.mpq's models and tables need Data/patch-E.mpq and \
                 Data/patch-F.mpq, which installing “Patch Y” would override."
            )
        );

        let gate = Gate {
            breaks: vec![lost(
                "Data/patch-G.mpq",
                "a.m2",
                "a00.skin",
                "Data/patch-F.mpq",
            )],
            ..Gate::default()
        };
        assert_eq!(
            details(&sentences(&gate, &none, &patch_y())),
            Some(
                "Your custom Data/patch-G.mpq's models need Data/patch-F.mpq, which installing \
                 “Patch Y” would override."
            )
        );
    }

    #[test]
    fn a_block_says_in_short_how_many_patches_would_break() {
        let one = |archive: &str, file: &str| lost(archive, file, "a00.skin", "Data/patch-F.mpq");
        let short = |breaks: Vec<Break>| {
            let gate = Gate {
                breaks,
                ..Gate::default()
            };

            sentences(&gate, &owners(&[], &[]), &patch_y())
                .blocked
                .map(|block| block.short)
        };

        assert_eq!(
            short(vec![
                one("Data/patch-G.mpq", "a.m2"),
                one("Data/patch-G.mpq", "b.m2")
            ])
            .as_deref(),
            Some("This mod would break one of your patches.")
        );
        assert_eq!(
            short(vec![
                one("Data/patch-G.mpq", "a.m2"),
                one("Data/patch-H.mpq", "b.m2")
            ])
            .as_deref(),
            Some("This mod would break 2 of your patches.")
        );
        assert_eq!(short(Vec::new()), None);
    }

    #[test]
    fn a_mods_archive_is_named_by_the_mod_and_its_own_needs_by_the_mod() {
        let owned = owners(
            &[("data/enus/patch-enus-m.mpq", "dungeon-maps")],
            &[("dungeon-maps", "Dungeon Maps")],
        );
        let gate = Gate {
            breaks: vec![lost(
                "Data/enUS/patch-enUS-M.MPQ",
                "DBFilesClient\\DungeonMap.dbc",
                "x.blp",
                "Data/patch-F.mpq",
            )],
            ..Gate::default()
        };

        let install = sentences(&gate, &owned, &patch_y());
        assert_eq!(
            details(&install),
            Some(
                "The tables of “Dungeon Maps” need Data/patch-F.mpq, which installing “Patch Y” \
                 would override."
            ),
        );
        assert_eq!(install.asks, Vec::<String>::new());
    }

    #[test]
    fn a_file_the_mods_previous_release_had_and_this_one_leaves_out_is_told_so() {
        let owned = owners(
            &[("data/patch-y.mpq", "patch-y")],
            &[("patch-y", "Patch Y")],
        );
        let gate = Gate {
            breaks: vec![lost(
                "Data/patch-G.mpq",
                "DBFilesClient\\CreatureDisplayInfo.dbc",
                "y.blp",
                "Data/patch-y.mpq",
            )],
            ..Gate::default()
        };

        assert_eq!(
            details(&sentences(&gate, &owned, &patch_y())),
            Some(
                "Your custom Data/patch-G.mpq's tables need what Data/patch-y.mpq has now, \
                 which this release leaves out."
            )
        );
    }

    #[test]
    fn a_mods_own_gaps_neither_block_nor_ask() {
        // Its tables name what the file of something else where it goes
        // has, and the install sets that file aside: the simulation finds
        // no break in it, and nothing is told.
        let owned = owners(&[], &[("patch-y", "Patch Y")]);

        assert_eq!(
            sentences(&Gate::default(), &owned, &patch_y()),
            Install::default()
        );
    }

    #[test]
    fn overrides_name_the_tables_and_count_the_rest() {
        let owned = owners(
            &[("data/enus/patch-enus-m.mpq", "dungeon-maps")],
            &[("dungeon-maps", "Dungeon Maps")],
        );
        let table = |name: &str| format!("DBFilesClient\\{name}.dbc");
        let mut overrides = vec![
            overlap(
                &table("CreatureDisplayInfo"),
                "Data/patch-y.mpq",
                "Data/patch-G.mpq",
            ),
            overlap(
                &table("CreatureModelData"),
                "Data/patch-y.mpq",
                "Data/patch-G.mpq",
            ),
            overlap(
                &table("WorldMapArea"),
                "Data/patch-y.mpq",
                "Data/enUS/patch-enUS-M.MPQ",
            ),
        ];
        overrides.extend((0..40).map(|at| {
            overlap(
                &format!("Textures\\{at}.blp"),
                "Data/patch-y.mpq",
                "Data/patch-H.mpq",
            )
        }));
        overrides.extend((0..212).map(|at| {
            overlap(
                &format!("Creature\\{at}.m2"),
                "Data/patch-y.mpq",
                "Data/patch-H.mpq",
            )
        }));
        let gate = Gate {
            overrides,
            ..Gate::default()
        };
        let install = sentences(&gate, &owned, &patch_y());

        assert_eq!(install.blocked, None);
        assert_eq!(
            install.asks,
            [
                "Installing “Patch Y” would override the tables “Dungeon Maps” replaces: \
                 WorldMapArea.dbc.",
                "Installing “Patch Y” would override the tables your custom Data/patch-G.mpq \
                 replaces: CreatureDisplayInfo.dbc and CreatureModelData.dbc.",
                "Installing “Patch Y” would override 212 models and 40 textures your custom \
                 Data/patch-H.mpq replaces.",
            ]
        );
    }

    #[test]
    fn what_overrides_the_mod_is_told_the_other_way_round() {
        let owned = owners(&[("data/patch-z.mpq", "other")], &[("other", "Other")]);
        let table = |name: &str| format!("DBFilesClient\\{name}.dbc");
        let gate = Gate {
            overridden: vec![
                overlap(
                    &table("WorldMapArea"),
                    "Data/patch-w.mpq",
                    "Data/patch-y.mpq",
                ),
                overlap(&table("DungeonMap"), "Data/patch-w.mpq", "Data/patch-y.mpq"),
                overlap("Textures\\a.blp", "Data/patch-z.mpq", "Data/patch-y.mpq"),
            ],
            ..Gate::default()
        };

        assert_eq!(
            sentences(&gate, &owned, &patch_y()).asks,
            [
                "Data/patch-w.mpq would override the tables “Patch Y” brings: DungeonMap.dbc \
                 and WorldMapArea.dbc.",
                "“Other”'s Data/patch-z.mpq would override 1 texture “Patch Y” brings.",
            ]
        );
    }

    #[test]
    fn long_lists_name_the_first_few_and_count_the_rest() {
        let items: Vec<String> = (1..=7).map(|at| format!("t{at}.dbc")).collect();

        assert_eq!(few(&items[..4]), "t1.dbc, t2.dbc, t3.dbc and t4.dbc");
        assert_eq!(few(&items), "t1.dbc, t2.dbc, t3.dbc, t4.dbc and 3 more");
        assert_eq!(
            few(&items[..5]),
            "t1.dbc, t2.dbc, t3.dbc, t4.dbc and 1 more"
        );
    }
}
