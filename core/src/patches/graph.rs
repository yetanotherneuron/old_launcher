//! What the client's archives add up to: which archive provides each file,
//! which files need other files, and what a change to the archives would
//! break, override or be overridden by.
//!
//! The graph is built from the [`Facts`] of the archives the game loads, in
//! the order it loads them (see [`order`](super::order)). Files are keyed by
//! their two name hashes, so an archive without a listfile takes part; names
//! matter only for messages, and come from the **name pool**: every
//! listfile, plus the paths the tables reference.
//!
//! **Providers.** A file is provided by the loaded archive with the highest
//! priority that holds it, and a delete marker hides the copies below it
//! (as StormLib does; the game's own handling of markers isn't confirmed,
//! and this can only make the base game look worse, never an install).
//! Archives that differ only in case share a priority and count as one: where
//! both hold a file, the first by byte order is taken.
//!
//! **Needs.** Of each winning file of a custom archive (Blizzard's needs are
//! left out: the base game ships references nothing provides), the files it
//! references: a model's skins, animations and textures, and what the six
//! tables name, resolved the way the game does. `CreatureDisplayInfo`'s
//! textures are looked up in the folder of the model that the winning
//! `CreatureModelData` names for them, which may come from another archive;
//! an `ItemDisplayInfo` model is probed in the seven component folders (and,
//! for helmets, with the suffix of each race and sex); its components in the
//! eight regions with `_U`, `_M` and `_F`. A need with several ways to be met
//! is met by any of them.
//!
//! **The gate.** A [`Change`] says which archives leave and which come, and
//! [`Graph::gate`] simulates it, putting each archive that comes at the slot
//! its path gives it. It finds:
//! - **breaks**: a need met before and unmet after, of a file that wins
//!   after the change (and before it, if it is not the change's own), from a
//!   custom archive or from one of the change's. Needs unmet already are not
//!   breaks, and neither are Blizzard's. The check is per file, so a new
//!   archive that provides what the old one did breaks nothing;
//! - **overrides**: files of the change's archives that win after the change
//!   where a custom archive's file won before (and stays);
//! - **overridden**: files of the change's archives that lose to another
//!   custom archive's.
//!
//! Everything compares paths case-insensitively: on Windows two names that
//! differ only in case are one file.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use super::Change;
use super::facts::{Facts, Kind, Model, model_name};
use super::order::{Rank, is_blizzard, rank_of};
use crate::locale::Locale;
use crate::mpq::{Key, key};

const CREATURE_MODEL_DATA: &str = "DBFilesClient\\CreatureModelData.dbc";
const CREATURE_DISPLAY_INFO: &str = "DBFilesClient\\CreatureDisplayInfo.dbc";
const CREATURE_DISPLAY_INFO_EXTRA: &str = "DBFilesClient\\CreatureDisplayInfoExtra.dbc";
const ITEM_DISPLAY_INFO: &str = "DBFilesClient\\ItemDisplayInfo.dbc";
const GAME_OBJECT_DISPLAY_INFO: &str = "DBFilesClient\\GameObjectDisplayInfo.dbc";
const SPELL_VISUAL_EFFECT_NAME: &str = "DBFilesClient\\SpellVisualEffectName.dbc";

/// What archives keep about themselves, which every one has its own of and
/// no file overrides.
const METADATA: [&str; 3] = ["(listfile)", "(attributes)", "(signature)"];

/// The tables whose columns name files.
const TABLES: [&str; 6] = [
    CREATURE_MODEL_DATA,
    CREATURE_DISPLAY_INFO,
    CREATURE_DISPLAY_INFO_EXTRA,
    ITEM_DISPLAY_INFO,
    GAME_OBJECT_DISPLAY_INFO,
    SPELL_VISUAL_EFFECT_NAME,
];

/// The folders of the components of items, under `Item\ObjectComponents`.
const SLOTS: [&str; 7] = [
    "Head", "Shoulder", "Weapon", "Shield", "Quiver", "Ammo", "Cape",
];

/// The races whose helmets carry their race and sex in the name
/// (`Helm_01_HuM.m2`).
const RACES: [&str; 11] = [
    "Hu", "Or", "Dw", "Ni", "Sc", "Ta", "Gn", "Tr", "Go", "Be", "Dr",
];

/// The folders of the eight regions an `ItemDisplayInfo` has textures for,
/// under `Item\TextureComponents`, in its columns' order.
const REGIONS: [&str; 8] = [
    "ArmUpperTexture",
    "ArmLowerTexture",
    "HandTexture",
    "TorsoUpperTexture",
    "TorsoLowerTexture",
    "LegUpperTexture",
    "LegLowerTexture",
    "FootTexture",
];

/// An archive of the load order, with what it says.
#[derive(Clone, Debug)]
pub struct Layer {
    /// Its priority: the higher, the more it wins. Archives with the same
    /// are case-twins and count as one.
    pub priority: u32,
    /// Its path from the game folder, as the folder spells it.
    pub path: String,
    /// What it says.
    pub facts: Arc<Facts>,
}

impl Layer {
    /// The layer of the archive at `path` (priority `priority`) in a client
    /// that plays in `locale`, with its `facts`. Blizzard's archives lose
    /// what the graph never reads of them: their needs are left out, so only
    /// the `CreatureModelData` that custom tables are resolved against is
    /// kept, and they are most of the tables' weight.
    pub fn new(priority: u32, path: String, facts: Facts, locale: Locale) -> Layer {
        let facts = strip(&path, facts, locale);

        Layer {
            priority,
            path,
            facts: Arc::new(facts),
        }
    }
}

/// `facts` of the archive at `path`, in a client that plays in `locale`,
/// without what the graph never reads of it (see [`Layer::new`]): Blizzard's
/// archives lose their needs but the `CreatureModelData` that custom tables
/// are resolved against, and they are most of the tables' weight. The
/// launcher keeps the facts stripped, so the next start reads small files.
/// Stripping again changes nothing.
pub fn strip(path: &str, mut facts: Facts, locale: Locale) -> Facts {
    if is_blizzard(path, locale) {
        let models = std::mem::take(&mut facts.tables.creature_model_data);

        facts.models = BTreeMap::new();
        facts.tables = super::facts::Tables {
            creature_model_data: models,
            ..Default::default()
        };
    }

    facts
}

/// A file, as a message names it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Named {
    /// Its key.
    pub key: Key,
    /// Its name, if any archive or table gave it one. The messages say
    /// "files" for the rest.
    pub name: Option<String>,
    /// What it is, by its name.
    pub kind: Kind,
}

/// A file that needs another, or a table that names it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Need {
    /// The archive of the file that needs, as the folder spells it.
    pub archive: String,
    /// The file that needs: a table or a model.
    pub file: Named,
    /// What it needs. If it could be met in several ways, the first.
    pub needs: Named,
}

/// A need that no loaded archive meets, but that an archive in the folder
/// that isn't loaded would.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Presence {
    /// The need.
    pub need: Need,
    /// The archives that would meet it.
    pub present_in: Vec<String>,
}

/// A need that a change would leave unmet.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Break {
    /// The archive whose file needs, as the folder spells it, or the
    /// change's path if it is the change's own.
    pub archive: String,
    /// Whether that archive is one the change brings.
    pub in_change: bool,
    /// The file that needs.
    pub file: Named,
    /// What it needs, and the change takes away.
    pub needs: Named,
    /// The archive that provided it, which the change removes or overrides.
    pub was_provided_by: String,
}

/// A file that two archives hold, and the one that wins.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Overlap {
    /// The file.
    pub file: Named,
    /// The archive whose copy wins after the change.
    pub winner: String,
    /// The archive whose copy is hidden.
    pub loser: String,
}

/// What a change would do.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Gate {
    /// Needs the change would leave unmet.
    pub breaks: Vec<Break>,
    /// Files of the change that would win over a custom archive's.
    pub overrides: Vec<Overlap>,
    /// Files of the change that a custom archive's would win over.
    pub overridden: Vec<Overlap>,
}

/// An archive, as the graph keeps it.
struct Held {
    priority: u32,
    path: String,
    rank: Rank,
    custom: bool,
    facts: Arc<Facts>,
}

/// The layer that provides a file, or the one whose delete marker hides it.
#[derive(Clone, Copy)]
struct Hit {
    layer: u32,
    deleted: bool,
}

/// One way or several to meet a need.
#[derive(Clone, Debug)]
enum Want {
    One(Key),
    Any(Box<[Key]>),
}

impl Want {
    fn keys(&self) -> &[Key] {
        match self {
            Want::One(key) => std::slice::from_ref(key),
            Want::Any(keys) => keys,
        }
    }
}

/// A file of a layer and one of its needs.
struct Ref {
    /// The layer of the file, in the scene it was resolved in.
    layer: usize,
    /// The file that needs.
    file: Key,
    kind: Kind,
    /// Its name, when it is a table.
    table: Option<&'static str>,
    want: Want,
    /// What the first way to meet it is called, kept when it isn't met.
    name: Option<Box<str>>,
    /// Whether anything meets it in the scene it was resolved in.
    met: bool,
}

/// An archive in a [`Scene`].
struct Ground<'a> {
    path: &'a str,
    facts: &'a Facts,
    custom: bool,
    /// Whether it comes with the change.
    added: bool,
}

/// How a scene finds the winner of a file.
enum Lookup<'a> {
    /// In the graph as built: the map has it.
    Before(&'a HashMap<Key, Hit>),
    /// After a change: the files of the archives that left or came are
    /// looked for in the layers; the rest are as they were.
    After {
        before: &'a HashMap<Key, Hit>,
        affected: HashSet<Key>,
        /// Where each layer of the graph went, if it stayed.
        moved: &'a [Option<usize>],
    },
}

/// The archives in the order they win, and who wins each file.
struct Scene<'a> {
    layers: Vec<Ground<'a>>,
    lookup: Lookup<'a>,
}

impl Scene<'_> {
    /// The layer whose copy of `key` wins, and whether it is a delete
    /// marker.
    fn raw(&self, key: Key) -> Option<(usize, bool)> {
        match &self.lookup {
            Lookup::Before(winners) => winners
                .get(&key)
                .map(|hit| (hit.layer as usize, hit.deleted)),
            Lookup::After {
                before,
                affected,
                moved,
            } => {
                if affected.contains(&key) {
                    return self.layers.iter().enumerate().find_map(|(at, ground)| {
                        ground.facts.files.get(&key).map(|file| (at, file.deleted))
                    });
                }

                let hit = before.get(&key)?;
                let at = (*moved.get(hit.layer as usize)?)?;

                Some((at, hit.deleted))
            }
        }
    }

    /// The layer that provides `key`, if a file does.
    fn winner(&self, key: Key) -> Option<usize> {
        self.raw(key)
            .and_then(|(layer, deleted)| (!deleted).then_some(layer))
    }

    /// Whether anything meets `want`.
    fn met(&self, want: &Want) -> bool {
        want.keys().iter().any(|key| self.winner(*key).is_some())
    }
}

/// Reads the tables and models of a scene into the needs of their files.
struct Resolver<'s, 'a> {
    scene: &'s Scene<'a>,
    refs: Vec<Ref>,
    /// The file being read: its key, kind and table name.
    file: (Key, Kind, Option<&'static str>),
}

/// The needs of the files of the layers `wanted` picks that win in `scene`.
fn resolve(scene: &Scene<'_>, wanted: impl Fn(usize) -> bool) -> Vec<Ref> {
    let mut resolver = Resolver {
        scene,
        refs: Vec::new(),
        file: (Key(0, 0), Kind::Other, None),
    };

    for (table, name) in TABLES.into_iter().enumerate() {
        let file = key(name);

        if let Some(layer) = scene.winner(file)
            && wanted(layer)
            && let Some(ground) = scene.layers.get(layer)
        {
            resolver.file = (file, Kind::Tables, Some(name));
            resolver.table(table, layer, ground.facts);
        }
    }

    for (layer, ground) in scene.layers.iter().enumerate() {
        if !wanted(layer) {
            continue;
        }

        for (file, model) in &ground.facts.models {
            let Model::Needs(needs) = model else {
                continue;
            };

            if scene.winner(*file) != Some(layer) {
                continue;
            }

            resolver.file = (*file, Kind::Models, None);

            for need in needs {
                resolver.want(layer, Want::One(*need), "");
            }
        }
    }

    resolver.refs
}

impl Resolver<'_, '_> {
    /// Notes that the file being read needs `want`, first called `name`.
    fn want(&mut self, layer: usize, want: Want, name: &str) {
        let met = self.scene.met(&want);
        let (file, kind, table) = self.file;

        self.refs.push(Ref {
            layer,
            file,
            kind,
            table,
            name: (!met && !name.is_empty()).then(|| name.into()),
            want,
            met,
        });
    }

    fn one(&mut self, layer: usize, name: &str) {
        self.want(layer, Want::One(key(name)), name);
    }

    /// Reads the `table`th of [`TABLES`], which `facts` hold.
    fn table(&mut self, table: usize, layer: usize, facts: &Facts) {
        match table {
            0 => self.creature_models(layer, facts),
            1 => self.creature_displays(layer, facts),
            2 => self.creature_extras(layer, facts),
            3 => self.item_displays(layer, facts),
            4 => self.object_displays(layer, facts),
            _ => self.spell_effects(layer, facts),
        }
    }

    fn creature_models(&mut self, layer: usize, facts: &Facts) {
        for path in facts.tables.creature_model_data.values() {
            self.one(layer, &model_name(path));
        }
    }

    fn creature_displays(&mut self, layer: usize, facts: &Facts) {
        // The textures are in the folder of the model, which the table
        // that wins names, whichever archive it is in.
        let scene = self.scene;
        let models = scene
            .winner(key(CREATURE_MODEL_DATA))
            .and_then(|at| scene.layers.get(at))
            .map(|ground| &ground.facts.tables.creature_model_data);

        for display in facts.tables.creature_display_info.values() {
            let Some(path) = models.and_then(|models| models.get(&display.model)) else {
                continue;
            };
            let folder = path
                .rsplit_once(['\\', '/'])
                .map_or("", |(folder, _)| folder);

            for texture in display
                .textures
                .iter()
                .filter(|texture| !texture.is_empty())
            {
                if folder.is_empty() {
                    self.one(layer, &format!("{texture}.blp"));
                } else {
                    self.one(layer, &format!("{folder}\\{texture}.blp"));
                }
            }
        }
    }

    fn creature_extras(&mut self, layer: usize, facts: &Facts) {
        for bake in facts.tables.creature_display_info_extra.values() {
            self.one(layer, &format!("Textures\\BakedNpcTextures\\{bake}"));
        }
    }

    fn item_displays(&mut self, layer: usize, facts: &Facts) {
        for item in facts.tables.item_display_info.values() {
            for (index, model) in item.models.iter().enumerate() {
                if model.is_empty() {
                    continue;
                }

                let file = model_name(model);
                let stem = file
                    .get(..file.len().saturating_sub(3))
                    .filter(|_| file.to_ascii_lowercase().ends_with(".m2"))
                    .unwrap_or(&file);
                let mut options: Vec<(&str, String)> = SLOTS
                    .iter()
                    .map(|slot| (*slot, format!("Item\\ObjectComponents\\{slot}\\{file}")))
                    .collect();

                for race in RACES {
                    for sex in ["M", "F"] {
                        options.push((
                            "Head",
                            format!("Item\\ObjectComponents\\Head\\{stem}_{race}{sex}.m2"),
                        ));
                    }
                }

                let keys: Box<[Key]> = options.iter().map(|(_, name)| key(name)).collect();
                let found = options
                    .iter()
                    .zip(&keys)
                    .find(|(_, key)| self.scene.winner(**key).is_some())
                    .map(|((slot, _), _)| *slot);
                let first = options.first().map_or("", |(_, name)| name.as_str());

                self.want(layer, Want::Any(keys), first);

                // Its texture is in the folder where the model was found.
                let texture = item
                    .model_textures
                    .get(index)
                    .filter(|name| !name.is_empty());

                if let (Some(slot), Some(texture)) = (found, texture) {
                    self.one(
                        layer,
                        &format!("Item\\ObjectComponents\\{slot}\\{texture}.blp"),
                    );
                }
            }

            for icon in item.icons.iter().filter(|icon| !icon.is_empty()) {
                self.one(layer, &format!("Interface\\Icons\\{icon}.blp"));
            }

            for (region, component) in REGIONS.iter().zip(&item.components) {
                if component.is_empty() {
                    continue;
                }

                let names = ["U", "M", "F"]
                    .map(|sex| format!("Item\\TextureComponents\\{region}\\{component}_{sex}.blp"));
                let keys: Box<[Key]> = names.iter().map(|name| key(name)).collect();

                self.want(layer, Want::Any(keys), &names[0]);
            }
        }
    }

    fn object_displays(&mut self, layer: usize, facts: &Facts) {
        for path in facts.tables.game_object_display_info.values() {
            self.one(layer, &model_name(path));
        }
    }

    fn spell_effects(&mut self, layer: usize, facts: &Facts) {
        for path in facts.tables.spell_visual_effect_name.values() {
            self.one(layer, &model_name(path));
        }
    }
}

/// An archive that comes with a change.
struct Added<'a> {
    path: &'a str,
    rank: Rank,
    custom: bool,
    facts: &'a Facts,
}

/// Where a layer of a simulated scene comes from.
enum Source {
    Old(usize),
    New(usize),
}

/// What the loaded archives of a client add up to.
pub struct Graph {
    locale: Locale,
    /// The archives, the one that wins first.
    layers: Vec<Held>,
    /// The winner of every file.
    winners: HashMap<Key, Hit>,
    /// The needs of the winning files of custom archives.
    refs: Vec<Ref>,
}

impl Graph {
    /// The graph of `layers`, of a client that plays in `locale`.
    pub fn build(layers: &[Layer], locale: Locale) -> Graph {
        let mut held: Vec<Held> = layers
            .iter()
            .map(|layer| Held {
                priority: layer.priority,
                path: layer.path.clone(),
                rank: rank_of(&layer.path, locale).unwrap_or(Rank::Base(layer.priority)),
                custom: !is_blizzard(&layer.path, locale),
                facts: Arc::clone(&layer.facts),
            })
            .collect();
        held.sort_by(|a, b| {
            b.priority
                .cmp(&a.priority)
                .then_with(|| a.path.cmp(&b.path))
        });

        let mut winners = HashMap::new();

        for (at, layer) in held.iter().enumerate() {
            for (file, entry) in &layer.facts.files {
                winners.entry(*file).or_insert(Hit {
                    layer: at as u32,
                    deleted: entry.deleted,
                });
            }
        }

        let mut graph = Graph {
            locale,
            layers: held,
            winners,
            refs: Vec::new(),
        };
        let scene = graph.scene();
        graph.refs = resolve(&scene, |layer| {
            scene.layers.get(layer).is_some_and(|ground| ground.custom)
        });

        graph
    }

    /// The graph's archives as a scene, the way they are.
    fn scene(&self) -> Scene<'_> {
        Scene {
            layers: self
                .layers
                .iter()
                .map(|held| Ground {
                    path: &held.path,
                    facts: &held.facts,
                    custom: held.custom,
                    added: false,
                })
                .collect(),
            lookup: Lookup::Before(&self.winners),
        }
    }

    /// The path of the archive that provides `key`, if one does.
    pub fn provider(&self, key: Key) -> Option<&str> {
        let hit = self.winners.get(&key).filter(|hit| !hit.deleted)?;

        self.layers
            .get(hit.layer as usize)
            .map(|held| held.path.as_str())
    }

    /// The name of the file `key`, if any listfile or table gave it one.
    pub fn name(&self, key: Key) -> Option<&str> {
        self.layers
            .iter()
            .find_map(|held| held.facts.names.get(&key))
            .map(String::as_str)
    }

    /// `key` as a message names it, `table` being its name if it is a
    /// table, and `more` the facts of archives not in the graph.
    fn named(&self, key: Key, table: Option<&str>, more: &[&Facts], kind: Option<Kind>) -> Named {
        // The six tables as the game spells them, whatever case an archive's
        // listfile gives them.
        let name = table.map(str::to_owned).or_else(|| {
            TABLES
                .iter()
                .find(|name| crate::mpq::key(name) == key)
                .map(|name| (*name).to_owned())
                .or_else(|| {
                    self.name(key)
                        .map(str::to_owned)
                        .or_else(|| more.iter().find_map(|facts| facts.names.get(&key)).cloned())
                })
        });
        let kind = name
            .as_deref()
            .map(Kind::of)
            .or(kind)
            .unwrap_or(Kind::Other);

        Named { key, name, kind }
    }

    fn need_of(&self, entry: &Ref) -> Need {
        let first = entry.want.keys().first().copied().unwrap_or(Key(0, 0));
        let mut needs = self.named(first, None, &[], None);

        if needs.name.is_none() {
            needs.name = entry.name.as_deref().map(str::to_owned);
            needs.kind = needs.name.as_deref().map_or(Kind::Other, Kind::of);
        }

        Need {
            archive: self
                .layers
                .get(entry.layer)
                .map(|held| held.path.clone())
                .unwrap_or_default(),
            file: self.named(entry.file, entry.table, &[], Some(entry.kind)),
            needs,
        }
    }

    /// The needs of the custom archives' winning files that no loaded
    /// archive meets.
    pub fn unmet(&self) -> Vec<Need> {
        self.refs
            .iter()
            .filter(|entry| !entry.met)
            .map(|entry| self.need_of(entry))
            .collect()
    }

    /// The unmet needs that an archive in `present` would meet: the facts
    /// of the archives in the folder that the game doesn't load (see
    /// [`Order::unloaded`](super::order::Order::unloaded)), by path.
    pub fn unmet_but_present(&self, present: &[(&str, &Facts)]) -> Vec<Presence> {
        self.refs
            .iter()
            .filter(|entry| !entry.met)
            .filter_map(|entry| {
                let present_in: Vec<String> = present
                    .iter()
                    .filter(|(_, facts)| {
                        entry
                            .want
                            .keys()
                            .iter()
                            .any(|key| facts.files.get(key).is_some_and(|file| !file.deleted))
                    })
                    .map(|(path, _)| (*path).to_owned())
                    .collect();

                (!present_in.is_empty()).then(|| Presence {
                    need: self.need_of(entry),
                    present_in,
                })
            })
            .collect()
    }

    /// What `change` would do, `facts` being those of the archives that
    /// come, by checksum. An archive without facts there provides and needs
    /// nothing.
    pub fn gate(&self, change: &Change, facts: &BTreeMap<String, Facts>) -> Gate {
        let lower = |path: &str| path.replace('\\', "/").to_ascii_lowercase();
        let empty = Facts::default();

        // What leaves, and what an archive that comes lands on.
        let mut gone: HashSet<String> = change.removes.iter().map(|path| lower(path)).collect();
        let mut added = Vec::new();

        for (path, sum) in &change.adds {
            gone.insert(lower(path));

            if let Some(rank) = rank_of(path, self.locale) {
                added.push(Added {
                    path,
                    rank,
                    custom: !is_blizzard(path, self.locale),
                    facts: facts.get(sum).unwrap_or(&empty),
                });
            }
        }

        let mut order: Vec<(&Rank, &str, Source)> = Vec::new();

        for (at, held) in self.layers.iter().enumerate() {
            if !gone.contains(&lower(&held.path)) {
                order.push((&held.rank, &held.path, Source::Old(at)));
            }
        }

        for (at, new) in added.iter().enumerate() {
            order.push((&new.rank, new.path, Source::New(at)));
        }

        order.sort_by(|a, b| b.0.cmp(a.0).then_with(|| a.1.cmp(b.1)));

        let mut moved = vec![None; self.layers.len()];
        let mut grounds = Vec::new();

        for (at, (_, path, source)) in order.iter().enumerate() {
            match source {
                Source::Old(old) => {
                    if let (Some(held), Some(slot)) = (self.layers.get(*old), moved.get_mut(*old)) {
                        *slot = Some(at);
                        grounds.push(Ground {
                            path,
                            facts: &held.facts,
                            custom: held.custom,
                            added: false,
                        });
                    }
                }
                Source::New(new) => {
                    if let Some(new) = added.get(*new) {
                        grounds.push(Ground {
                            path,
                            facts: new.facts,
                            custom: new.custom,
                            added: true,
                        });
                    }
                }
            }
        }

        // The files whose winner can change: those of what leaves and comes.
        let mut affected: HashSet<Key> = added
            .iter()
            .flat_map(|new| new.facts.files.keys().copied())
            .collect();

        for (old, held) in self.layers.iter().enumerate() {
            if moved.get(old).copied().flatten().is_none() {
                affected.extend(held.facts.files.keys().copied());
            }
        }

        let before = self.scene();
        let after = Scene {
            layers: grounds,
            lookup: Lookup::After {
                before: &self.winners,
                affected,
                moved: &moved,
            },
        };
        let more: Vec<&Facts> = added.iter().map(|new| new.facts).collect();
        let mut gate = Gate::default();

        // The needs of custom archives' files that stay, met before and not
        // after.
        for entry in &self.refs {
            let Some(layer) = moved.get(entry.layer).copied().flatten() else {
                continue;
            };

            if !entry.met || after.winner(entry.file) != Some(layer) || after.met(&entry.want) {
                continue;
            }

            let mut need = self.need_of(entry);
            let was = entry.want.keys().iter().find_map(|key| {
                let at = before.winner(*key)?;

                Some((*key, self.layers.get(at)?.path.clone()))
            });

            if let Some((met, path)) = was {
                need.needs = self.named(met, None, &more, None);
                gate.breaks.push(Break {
                    archive: need.archive,
                    in_change: false,
                    file: need.file,
                    needs: need.needs,
                    was_provided_by: path,
                });
            }
        }

        // The needs of the change's own files.
        let own = resolve(&after, |layer| {
            after
                .layers
                .get(layer)
                .is_some_and(|ground| ground.added && ground.custom)
        });

        for entry in own.iter().filter(|entry| !entry.met) {
            let was = entry.want.keys().iter().find_map(|key| {
                let at = before.winner(*key)?;

                Some((*key, self.layers.get(at)?.path.clone()))
            });
            let Some((met, path)) = was else {
                continue;
            };
            let Some(ground) = after.layers.get(entry.layer) else {
                continue;
            };

            gate.breaks.push(Break {
                archive: ground.path.to_owned(),
                in_change: true,
                file: self.named(entry.file, entry.table, &more, Some(entry.kind)),
                needs: self.named(met, None, &more, None),
                was_provided_by: path,
            });
        }

        // Overrides and overridden: of the game's files, not the archives'
        // own notes about themselves.
        let notes: Vec<Key> = METADATA.iter().map(|name| key(name)).collect();

        for (at, ground) in after.layers.iter().enumerate() {
            if !ground.added || !ground.custom {
                continue;
            }

            for (file, entry) in &ground.facts.files {
                if notes.contains(file) {
                    continue;
                }

                let Some((winner, _)) = after.raw(*file) else {
                    continue;
                };
                let overlap = |winner: &str, loser: &str| Overlap {
                    file: self.named(*file, None, &more, Some(entry.kind)),
                    winner: winner.to_owned(),
                    loser: loser.to_owned(),
                };

                if winner == at {
                    let Some((was, false)) = before.raw(*file) else {
                        continue;
                    };

                    if let Some(held) = self.layers.get(was)
                        && held.custom
                        && moved.get(was).copied().flatten().is_some()
                    {
                        gate.overrides.push(overlap(ground.path, &held.path));
                    }
                } else if let Some(other) = after.layers.get(winner)
                    && !other.added
                    && other.custom
                    && !entry.deleted
                {
                    gate.overridden.push(overlap(other.path, ground.path));
                }
            }
        }

        gate.breaks.sort_by(|a, b| {
            (&a.archive, a.file.key, a.needs.key).cmp(&(&b.archive, b.file.key, b.needs.key))
        });
        gate.breaks.dedup();
        gate.overrides.sort_by(|a, b| {
            (&a.loser, &a.file.name, a.file.key).cmp(&(&b.loser, &b.file.name, b.file.key))
        });
        gate.overridden.sort_by(|a, b| {
            (&a.winner, &a.file.name, a.file.key).cmp(&(&b.winner, &b.file.name, b.file.key))
        });

        gate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mpq::writer::Writer;
    use crate::patches::facts::Opened;
    use crate::patches::fixtures::{
        archive, display_info, facts_of, m2, model_data, object_displays, put, tiny,
    };
    use crate::patches::order::{Reason, order};
    use crate::testing::client;

    const PLAYER: Locale = Locale::EnUs;

    /// The facts of the archive at `path` under `root`, as the scan reads
    /// them.
    fn read(root: &std::path::Path, path: &str) -> Facts {
        Facts::of(&Opened::open(&root.join(path)).unwrap(), &[])
    }

    /// A client holding `archives` (path, archive), and its graph.
    fn scanned(archives: &[(&str, Writer)]) -> (tempfile::TempDir, Graph) {
        let (dir, _) = client(&[]);

        for (path, archive) in archives {
            put(dir.path(), path, archive);
        }

        let order = order(dir.path(), PLAYER);
        let layers: Vec<Layer> = order
            .loaded
            .iter()
            .flat_map(|slot| slot.paths.iter().map(move |path| (slot.priority, path)))
            .map(|(priority, path)| Layer {
                priority,
                path: path.clone(),
                facts: Arc::new(read(dir.path(), path)),
            })
            .collect();

        (dir, Graph::build(&layers, PLAYER))
    }

    /// A change that removes `removes` and brings `adds` (path, archive).
    fn changing(removes: &[&str], adds: &[(&str, &Writer)]) -> (Change, BTreeMap<String, Facts>) {
        let change = Change {
            adds: adds
                .iter()
                .enumerate()
                .map(|(at, (path, _))| ((*path).to_owned(), format!("sum{at}")))
                .collect(),
            removes: removes.iter().map(|path| (*path).to_owned()).collect(),
        };
        let facts = adds
            .iter()
            .enumerate()
            .map(|(at, (_, archive))| (format!("sum{at}"), facts_of(archive)))
            .collect();

        (change, facts)
    }

    fn names(files: &[Overlap]) -> Vec<&str> {
        files
            .iter()
            .filter_map(|overlap| overlap.file.name.as_deref())
            .collect()
    }

    /// An archive holding a model that wants one skin.
    fn with_skin_need() -> Writer {
        archive(&[("Creature\\Foo\\Foo.m2", m2(1))])
    }

    fn gives_skin() -> Writer {
        archive(&[("Creature\\Foo\\Foo00.skin", b"skin".to_vec())])
    }

    fn wants_foo() -> Writer {
        archive(&[(
            "DBFilesClient\\GameObjectDisplayInfo.dbc",
            object_displays(&[(1, "World\\Foo.mdx")]),
        )])
    }

    fn gives_foo() -> Writer {
        archive(&[("World\\Foo.m2", m2(0))])
    }

    #[test]
    fn the_highest_patch_provides_a_file_and_a_delete_marker_hides_it() {
        let (_dir, graph) = scanned(&[
            (
                "Data/patch-2.MPQ",
                archive(&[
                    ("Blizzard.txt", b"b".to_vec()),
                    ("Hidden.txt", b"h".to_vec()),
                ]),
            ),
            (
                "Data/patch-A.MPQ",
                archive(&[("Shared.txt", b"a".to_vec()), ("Hidden.txt", b"a".to_vec())]),
            ),
            (
                "Data/patch-B.MPQ",
                archive(&[("shared.TXT", b"b".to_vec()), ("Only.txt", b"b".to_vec())]),
            ),
            (
                "Data/patch-C.MPQ",
                Writer::new().delete_marker("Hidden.txt"),
            ),
        ]);
        let provider = |name| graph.provider(key(name));

        assert_eq!(provider("Blizzard.txt"), Some("Data/patch-2.MPQ"));
        assert_eq!(provider("SHARED.txt"), Some("Data/patch-B.MPQ"));
        assert_eq!(provider("Only.txt"), Some("Data/patch-B.MPQ"));
        assert_eq!(provider("Hidden.txt"), None);
        assert_eq!(provider("Nowhere.txt"), None);
    }

    #[test]
    fn a_tables_textures_resolve_against_the_model_from_another_archive() {
        // The table is in a higher archive than the model data it uses.
        let models = (
            "Data/patch-A.MPQ",
            archive(&[
                (
                    "DBFilesClient\\CreatureModelData.dbc",
                    model_data(&[(5, "Creature\\Foo\\Foo.mdx")]),
                ),
                ("Creature\\Foo\\Foo.m2", m2(0)),
            ]),
        );
        let displays = (
            "Data/patch-B.MPQ",
            archive(&[(
                "DBFilesClient\\CreatureDisplayInfo.dbc",
                display_info(&[(7, 5, "FooSkin"), (8, 99, "NoModel")]),
            )]),
        );
        let (_dir, graph) = scanned(&[models, displays]);
        let unmet = graph.unmet();

        assert_eq!(unmet.len(), 1, "{unmet:?}");
        assert_eq!(unmet[0].archive, "Data/patch-B.MPQ");
        assert_eq!(
            unmet[0].file.name.as_deref(),
            Some("DBFilesClient\\CreatureDisplayInfo.dbc")
        );
        assert_eq!(unmet[0].file.kind, Kind::Tables);
        assert_eq!(
            unmet[0].needs.name.as_deref(),
            Some("Creature\\Foo\\FooSkin.blp")
        );
        assert_eq!(unmet[0].needs.kind, Kind::Textures);

        // Once the texture is in the model's folder, in a third archive,
        // nothing is unmet.
        let (_dir, graph) = scanned(&[
            (
                "Data/patch-A.MPQ",
                archive(&[
                    (
                        "DBFilesClient\\CreatureModelData.dbc",
                        model_data(&[(5, "Creature\\Foo\\Foo.mdx")]),
                    ),
                    ("Creature\\Foo\\Foo.m2", m2(0)),
                ]),
            ),
            (
                "Data/patch-B.MPQ",
                archive(&[(
                    "DBFilesClient\\CreatureDisplayInfo.dbc",
                    display_info(&[(7, 5, "FooSkin")]),
                )]),
            ),
            (
                "Data/patch-C.MPQ",
                archive(&[("creature/foo/fooskin.BLP", b"t".to_vec())]),
            ),
        ]);

        assert_eq!(graph.unmet(), Vec::new());
    }

    #[test]
    fn a_models_skins_and_textures_are_its_needs_and_blizzards_are_left_out() {
        let (_dir, graph) = scanned(&[
            // Blizzard's: its model needs a skin nothing has.
            ("Data/patch-2.MPQ", with_skin_need()),
            (
                "Data/patch-G.MPQ",
                archive(&[("Creature\\Bar\\Bar.m2", m2(2))]),
            ),
            (
                "Data/patch-H.MPQ",
                archive(&[("Creature\\Bar\\Bar01.skin", b"s".to_vec())]),
            ),
        ]);
        let unmet = graph.unmet();

        assert_eq!(unmet.len(), 1, "{unmet:?}");
        assert_eq!(unmet[0].archive, "Data/patch-G.MPQ");
        assert_eq!(unmet[0].file.kind, Kind::Models);
        assert_eq!(unmet[0].file.name.as_deref(), Some("Creature\\Bar\\Bar.m2"));
        assert_eq!(unmet[0].needs.key, key("Creature\\Bar\\Bar00.skin"));
    }

    #[test]
    fn stripping_leaves_blizzards_facts_their_model_data_and_nothing_else_changes() {
        let facts = facts_of(&archive(&[
            ("Creature\\Foo\\Foo.m2", m2(1)),
            (
                "DBFilesClient\\CreatureModelData.dbc",
                model_data(&[(1, "Creature\\Foo\\Foo.mdx")]),
            ),
            (
                "DBFilesClient\\GameObjectDisplayInfo.dbc",
                object_displays(&[(1, "World\\Foo.mdx")]),
            ),
        ]));
        assert!(!facts.models.is_empty() && !facts.tables.game_object_display_info.is_empty());

        let stripped = strip("Data/patch-2.MPQ", facts.clone(), PLAYER);
        assert!(stripped.models.is_empty());
        assert!(stripped.tables.game_object_display_info.is_empty());
        assert_eq!(
            stripped.tables.creature_model_data,
            facts.tables.creature_model_data
        );
        assert_eq!(stripped.files, facts.files);
        assert_eq!(stripped.names, facts.names);
        assert_eq!(
            strip("Data/patch-2.MPQ", stripped.clone(), PLAYER),
            stripped,
            "again, nothing changes"
        );
        assert_eq!(strip("Data/patch-G.MPQ", facts.clone(), PLAYER), facts);
    }

    #[test]
    fn a_need_met_by_a_backup_is_unmet_but_present() {
        let (dir, graph) = scanned(&[
            ("Data/patch-G.mpq", wants_foo()),
            ("Data/patch-F.mpq.backup", gives_foo()),
            ("Data/patch-E.mpq.disabled", tiny()),
        ]);
        let order = order(dir.path(), PLAYER);
        let present: Vec<(String, Facts)> = order
            .unloaded
            .iter()
            .filter(|archive| matches!(archive.reason, Reason::Backup | Reason::Disabled))
            .map(|archive| (archive.path.clone(), read(dir.path(), &archive.path)))
            .collect();
        let present: Vec<(&str, &Facts)> = present
            .iter()
            .map(|(path, facts)| (path.as_str(), facts))
            .collect();

        assert_eq!(graph.unmet().len(), 1);

        let found = graph.unmet_but_present(&present);

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].present_in, vec!["Data/patch-F.mpq.backup"]);
        assert_eq!(found[0].need.archive, "Data/patch-G.mpq");
        assert_eq!(found[0].need.needs.name.as_deref(), Some("World\\Foo.m2"));
        // The disabled archive meets nothing.
        assert_eq!(graph.unmet_but_present(&present[..1]), Vec::new());
    }

    #[test]
    fn an_install_that_removes_what_a_custom_patch_needs_is_a_break() {
        let (_dir, graph) = scanned(&[
            ("Data/patch-F.mpq", gives_skin()),
            ("Data/patch-G.mpq", with_skin_need()),
        ]);
        let y = archive(&[("Item\\Y.blp", b"y".to_vec())]);
        let (change, facts) = changing(&["data/patch-f.MPQ"], &[("Data/patch-y.mpq", &y)]);
        let gate = graph.gate(&change, &facts);

        assert_eq!(gate.breaks.len(), 1, "{gate:?}");
        assert_eq!(gate.breaks[0].archive, "Data/patch-G.mpq");
        assert!(!gate.breaks[0].in_change);
        assert_eq!(gate.breaks[0].file.kind, Kind::Models);
        assert_eq!(
            gate.breaks[0].file.name.as_deref(),
            Some("Creature\\Foo\\Foo.m2")
        );
        assert_eq!(
            gate.breaks[0].needs.name.as_deref(),
            Some("Creature\\Foo\\Foo00.skin")
        );
        assert_eq!(gate.breaks[0].was_provided_by, "Data/patch-F.mpq");
        assert_eq!(gate.overrides, Vec::new());
        assert_eq!(gate.overridden, Vec::new());

        // Without removing it, nothing breaks.
        let (change, facts) = changing(&[], &[("Data/patch-y.mpq", &y)]);

        assert_eq!(graph.gate(&change, &facts), Gate::default());
    }

    #[test]
    fn an_install_whose_own_tables_need_what_it_removes_is_a_break() {
        let (_dir, graph) = scanned(&[("Data/patch-F.mpq", gives_foo())]);
        let y = wants_foo();
        let (change, facts) = changing(&["Data/patch-F.mpq"], &[("Data/patch-y.mpq", &y)]);
        let gate = graph.gate(&change, &facts);

        assert_eq!(gate.breaks.len(), 1, "{gate:?}");
        assert_eq!(gate.breaks[0].archive, "Data/patch-y.mpq");
        assert!(gate.breaks[0].in_change);
        assert_eq!(gate.breaks[0].file.kind, Kind::Tables);
        assert_eq!(
            gate.breaks[0].file.name.as_deref(),
            Some("DBFilesClient\\GameObjectDisplayInfo.dbc")
        );
        assert_eq!(gate.breaks[0].needs.name.as_deref(), Some("World\\Foo.m2"));
        assert_eq!(gate.breaks[0].was_provided_by, "Data/patch-F.mpq");

        // Keeping it, the same release breaks nothing.
        let (change, facts) = changing(&[], &[("Data/patch-y.mpq", &y)]);

        assert_eq!(graph.gate(&change, &facts), Gate::default());
    }

    #[test]
    fn an_install_that_only_replaces_blizzards_files_is_quiet() {
        let (_dir, graph) = scanned(&[
            (
                "Data/patch-2.MPQ",
                archive(&[
                    ("Item\\Foo.blp", b"b".to_vec()),
                    (
                        "DBFilesClient\\GameObjectDisplayInfo.dbc",
                        object_displays(&[(1, "World\\Foo.mdx")]),
                    ),
                ]),
            ),
            ("Data/patch-F.mpq", gives_foo()),
        ]);
        let y = archive(&[("Item\\Foo.blp", b"y".to_vec())]);
        // Blizzard's table needs what patch-F provides, and it leaves.
        let (change, facts) = changing(&["Data/patch-F.mpq"], &[("Data/patch-y.mpq", &y)]);

        assert_eq!(graph.gate(&change, &facts), Gate::default());
    }

    #[test]
    fn an_install_that_replaces_a_custom_patchs_table_is_an_override() {
        let tables = |id| {
            archive(&[
                (
                    "DBFilesClient\\CreatureModelData.dbc",
                    model_data(&[(id, "Creature\\Foo\\Foo.mdx")]),
                ),
                (
                    "DBFilesClient\\CreatureDisplayInfo.dbc",
                    display_info(&[(7, id, "")]),
                ),
                ("Item\\Shared.blp", b"x".to_vec()),
            ])
        };
        let (_dir, graph) = scanned(&[
            (
                "Data/patch-2.MPQ",
                archive(&[("Item\\Shared.blp", b"b".to_vec())]),
            ),
            ("Data/patch-G.mpq", tables(1)),
        ]);
        let y = tables(2);
        let (change, facts) = changing(&[], &[("Data/patch-y.mpq", &y)]);
        let gate = graph.gate(&change, &facts);

        // Blizzard's copy of the shared file is not what Patch Y overrides:
        // patch-G's is the one that won.
        assert_eq!(
            names(&gate.overrides),
            vec![
                "DBFilesClient\\CreatureDisplayInfo.dbc",
                "DBFilesClient\\CreatureModelData.dbc",
                "Item\\Shared.blp",
            ]
        );
        assert!(
            gate.overrides
                .iter()
                .all(|o| o.winner == "Data/patch-y.mpq" && o.loser == "Data/patch-G.mpq")
        );
        assert_eq!(gate.overrides[0].file.kind, Kind::Tables);
        assert_eq!(gate.overrides[2].file.kind, Kind::Textures);
        assert_eq!(gate.overridden, Vec::new());
        assert_eq!(gate.breaks, Vec::new());

        // Once the older release leaves, its files are not overridden.
        let (change, facts) = changing(&["Data/patch-G.mpq"], &[("Data/patch-y.mpq", &y)]);

        assert_eq!(graph.gate(&change, &facts).overrides, Vec::new());
    }

    #[test]
    fn an_install_whose_files_a_higher_patch_overrides_is_overridden() {
        let (_dir, graph) = scanned(&[
            ("Data/patch-2.MPQ", tiny()),
            (
                "Data/patch-w.mpq",
                archive(&[(
                    "DBFilesClient\\WorldMapArea.dbc",
                    dbc_of("WorldMapArea of w"),
                )]),
            ),
        ]);
        let maps = archive(&[
            (
                "DBFilesClient\\WorldMapArea.dbc",
                dbc_of("WorldMapArea of the mod"),
            ),
            ("Interface\\Maps.blp", b"m".to_vec()),
        ]);
        let (change, facts) = changing(&[], &[("Data/enUS/patch-enUS-M.MPQ", &maps)]);
        let gate = graph.gate(&change, &facts);

        assert_eq!(gate.overrides, Vec::new());
        assert_eq!(gate.breaks, Vec::new());
        assert_eq!(gate.overridden.len(), 1, "{gate:?}");
        assert_eq!(
            gate.overridden[0].file.name.as_deref(),
            Some("DBFilesClient\\WorldMapArea.dbc")
        );
        assert_eq!(gate.overridden[0].winner, "Data/patch-w.mpq");
        assert_eq!(gate.overridden[0].loser, "Data/enUS/patch-enUS-M.MPQ");
    }

    /// Some bytes that differ by `text`.
    fn dbc_of(text: &str) -> Vec<u8> {
        text.as_bytes().to_vec()
    }

    #[test]
    fn an_update_that_still_provides_what_others_need_breaks_nothing() {
        let (_dir, graph) = scanned(&[
            ("Data/patch-y.mpq", gives_foo()),
            ("Data/patch-G.mpq", wants_foo()),
        ]);

        // The same name, a new release that has it still.
        let newer = archive(&[("World\\Foo.m2", m2(0)), ("Extra.blp", b"e".to_vec())]);
        let (change, facts) = changing(&["Data/patch-y.mpq"], &[("Data/patch-y.mpq", &newer)]);

        assert_eq!(graph.gate(&change, &facts), Gate::default());

        // An alternative edition in another slot, with the file too.
        let (change, facts) = changing(&["Data/patch-y.mpq"], &[("Data/patch-x.mpq", &newer)]);

        assert_eq!(graph.gate(&change, &facts), Gate::default());

        // One that no longer has it does break what needs it.
        let lacking = tiny();
        let (change, facts) = changing(&["Data/patch-y.mpq"], &[("Data/patch-y.mpq", &lacking)]);
        let gate = graph.gate(&change, &facts);

        assert_eq!(gate.breaks.len(), 1, "{gate:?}");
        assert_eq!(gate.breaks[0].archive, "Data/patch-G.mpq");
        assert_eq!(gate.breaks[0].was_provided_by, "Data/patch-y.mpq");
    }

    #[test]
    fn a_need_unmet_before_the_install_is_not_a_break() {
        let (_dir, graph) = scanned(&[
            ("Data/patch-F.mpq", tiny()),
            ("Data/patch-G.mpq", wants_foo()),
        ]);

        assert_eq!(graph.unmet().len(), 1);

        let y = wants_foo();
        let (change, facts) = changing(&["Data/patch-F.mpq"], &[("Data/patch-y.mpq", &y)]);

        // Patch Y's table needs what nothing has, as patch-G's did.
        let gate = graph.gate(&change, &facts);

        assert_eq!(gate.breaks, Vec::new());
        assert_eq!(gate.overridden, Vec::new());
    }

    /// The memory this process uses now and at most, where Linux says.
    fn memory() -> String {
        std::fs::read_to_string("/proc/self/status")
            .unwrap_or_default()
            .lines()
            .filter(|line| line.starts_with("VmRSS") || line.starts_with("VmHWM"))
            .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// `cargo test -p old_launcher_core --release patches::graph::tests::a_real_client
    /// -- --ignored --nocapture`, with `OLD_LAUNCHER_TEST_CLIENT` a client
    /// folder, only ever read, and optionally `OLD_LAUNCHER_TEST_LOADED` the
    /// number of archives it loads. Prints the order, to compare with
    /// the reference model of the loader, and the times of a full scan.
    #[test]
    #[ignore = "reads every archive of the client in $OLD_LAUNCHER_TEST_CLIENT"]
    fn a_real_clients_order_matches_the_reference() {
        use std::time::Instant;

        let root = std::path::PathBuf::from(
            std::env::var("OLD_LAUNCHER_TEST_CLIENT")
                .expect("OLD_LAUNCHER_TEST_CLIENT is a client folder"),
        );
        let locale = crate::installation::locale(&root).expect("the client has a language");
        let started = Instant::now();
        let order = order(&root, locale);

        println!(
            "{locale}: {} archives in {:?}",
            order.loaded.len(),
            started.elapsed()
        );

        for slot in &order.loaded {
            for path in &slot.paths {
                println!("{:3}  {path}", slot.priority);
            }
        }

        for archive in &order.unloaded {
            println!("not loaded: {} ({:?})", archive.path, archive.reason);
        }

        println!("twins: {:?}", order.twins());

        let priorities: Vec<u32> = order.loaded.iter().map(|slot| slot.priority).collect();

        assert!(priorities.windows(2).all(|pair| pair[0] > pair[1]));

        if let Ok(count) = std::env::var("OLD_LAUNCHER_TEST_LOADED") {
            assert_eq!(order.loaded.len().to_string(), count);
        }

        let at = |name: &str| {
            order.loaded.iter().find(|slot| {
                slot.paths
                    .iter()
                    .any(|path| path.to_ascii_lowercase().ends_with(name))
            })
        };

        if let (Some(w), Some(m)) = (at("/patch-w.mpq"), at("/patch-enus-m.mpq")) {
            assert!(m.priority < w.priority);
        }

        // Every listfile first, so the archives without one can be probed
        // with the names the others give.
        let paths: Vec<&String> = order.loaded.iter().flat_map(|slot| &slot.paths).collect();
        let started = Instant::now();
        let mut known = Vec::new();

        for path in &paths {
            let opened = Opened::open(&root.join(path)).expect("it opens");

            known.extend(
                opened
                    .listfile()
                    .expect("its listfile reads")
                    .unwrap_or_default(),
            );
        }

        known.sort_unstable();
        known.dedup();
        known.shrink_to_fit();

        println!("{} names known; {}", known.len(), memory());

        let mut layers = Vec::new();
        let mut problems = 0;

        for slot in &order.loaded {
            for path in &slot.paths {
                let opened = Opened::open(&root.join(path)).expect("it opens");
                let listed = opened.listfile().expect("its listfile reads").is_some();
                let facts = Facts::of(&opened, if listed { &[] } else { &known });

                problems += facts.problems.len();
                for problem in &facts.problems {
                    println!("{path}: {problem}");
                }

                layers.push(Layer::new(slot.priority, path.clone(), facts, locale));
            }
        }

        println!(
            "facts of {} archives in {:?}, {problems} problems; {}",
            layers.len(),
            started.elapsed(),
            memory()
        );

        // Only the archives without a listfile needed them.
        drop(known);

        let started = Instant::now();
        let graph = Graph::build(&layers, locale);

        println!("graph in {:?}; {}", started.elapsed(), memory());

        let unmet = graph.unmet();
        let mut by_archive: BTreeMap<&str, usize> = BTreeMap::new();

        for need in &unmet {
            *by_archive.entry(&need.archive).or_default() += 1;
        }

        println!(
            "{} unmet needs of custom archives: {by_archive:?}",
            unmet.len()
        );

        let started = Instant::now();
        let present: Vec<(String, Facts)> = order
            .unloaded
            .iter()
            .filter_map(|archive| {
                let opened = Opened::open(&root.join(&archive.path)).ok()?;

                Some((archive.path.clone(), Facts::of(&opened, &[])))
            })
            .collect();
        let present: Vec<(&str, &Facts)> = present
            .iter()
            .map(|(path, facts)| (path.as_str(), facts))
            .collect();
        let found = graph.unmet_but_present(&present);

        println!(
            "{} unmet but present, {} unloaded archives read in {:?}",
            found.len(),
            present.len(),
            started.elapsed()
        );

        for presence in found.iter().take(20) {
            println!(
                "{} {:?} needs {:?}, in {:?}",
                presence.need.archive,
                presence.need.file.name,
                presence.need.needs.name,
                presence.present_in
            );
        }
    }

    #[test]
    fn archives_that_differ_only_in_case_are_one_and_the_union_meets_needs() {
        let (dir, _) = client(&[]);

        put(dir.path(), "Data/patch-g.mpq", &wants_foo());
        put(dir.path(), "Data/patch-G.mpq", &gives_foo());

        if std::fs::read_dir(dir.path().join("Data")).unwrap().count() < 2 {
            return;
        }

        let order = order(dir.path(), PLAYER);
        let layers: Vec<Layer> = order
            .loaded
            .iter()
            .flat_map(|slot| slot.paths.iter().map(move |path| (slot.priority, path)))
            .map(|(priority, path)| Layer {
                priority,
                path: path.clone(),
                facts: Arc::new(read(dir.path(), path)),
            })
            .collect();
        let graph = Graph::build(&layers, PLAYER);

        assert_eq!(order.twins().len(), 1);
        assert_eq!(graph.unmet(), Vec::new());
    }
}
