# The client's patches: what each one changes and needs

Status: decided on 2026-10-30, being built; packages 1 and 2 started.
Written against commit 798e721 (version 1.1.2); names refer to the code as
it is now. This plan supersedes `mod-table-overlap.md`, whose idea (warn
when a mod replaces a table another patch replaces) is one case of the
warn-and-ask below; package 6 deletes that file.

## Why

Installing Lau's "Patch Y" crashed a client on entering Dalaran. The install
backed up three of the player's own archives that sat in the mod's slots or
in its `excludes`, among them `Data/patch-F.mpq`, an HD creature pack. The
player's own `Data/patch-G.mpq` references models that only `patch-F`
provides, and Patch Y's `CreatureDisplayInfo`/`CreatureModelData` tables
took the place of patch-G's. The launcher saw none of it: it knows which
*paths* a mod takes, not which *files inside the archives* the game ends up
loading, nor what those files need.

The launcher should read the client's archives the way the game does, know
which archive provides each file and which files each archive needs, and
use that before it changes anything:

- an install that would **break** something is **blocked**, with no
  "anyway";
- an install that only **overrides** what another custom patch replaces
  **warns and asks**, through the existing "Install “X” anyway?" flow;
- the **File integrity check**, now for every client, also says which
  patches' needs are unmet *now* (a dependency left as `.backup`), which
  archives are case-twins, and which mods' files aren't as released;
- the reading runs **in the background at startup**, cached per archive,
  blocking only Play and the mods section until it is done;
- everything is computed **by the launcher, locally**: the list of mods
  carries nothing about archives' contents.

## What is known

An investigation verified the game's loader in `Wow.exe` (Ghidra), under
Wine, and on the 138 archives of three clients. Its report is the reference
for every number and rule below; where this plan and the report disagree,
the report wins. The points this design rests on:

- **Load order.** Base archives have fixed priorities 40–49. Patches are
  found by enumerating `Data\patch-?.MPQ` and `Data\<locale>\patch-<locale>-?.MPQ`
  (`?` is exactly one character, ASCII case-insensitive), sorted descending
  and case-insensitively on the full path, then `patch.MPQ` and
  `patch-<locale>.MPQ` are appended; the list is opened last to first with
  priorities counting up from 64, skipping archives that don't open. Higher
  priority wins. For enUS and most locales every `Data\<locale>\patch-…`
  loads *below* every `Data\patch-…`. `*.mpq.backup`, `*.mpq.disabled` and
  `patch-10.MPQ` never match. Only the locale `WTF/Config.wtf` sets is
  loaded: the HD client plays in esES and its `data/enus/` patches aren't
  loaded at all.
- **The format a reader needs.** Headers v0 and v1 only, 4096-byte sectors,
  u32 offsets (patch.MPQ is just under 4 GB), hash tables up to 524,288
  entries with deleted entries to probe past, locale always 0, the platform
  field to ignore. File flags: COMPRESS, SECTOR_CRC, SINGLE_UNIT, ENCRYPTED,
  FIX_KEY, DELETE_MARKER. Compression bytes: 0x02 zlib (17.8M sectors), 0x10
  bzip2 (8 sectors), raw sectors, stored files. Never implode, Huffman,
  ADPCM, sparse or LZMA. Five live archives have no `(listfile)`; three
  clients' listfiles are encrypted.
- **Where files reference files.** DBCs with path columns, by fixed column
  index (never by heuristic): CreatureModelData, CreatureDisplayInfo (whose
  textures resolve against the model's folder, which may come from another
  archive), CreatureDisplayInfoExtra, ItemDisplayInfo, GameObjectDisplayInfo,
  SpellVisualEffectName. `.mdx`/`.mdl` names mean `.m2`. An M2 (version 264)
  needs `<stem>%02d.skin` for its skin count, `<stem>%04d-%02d.anim` for the
  sequences not stored inline (following aliases), and its type-0 textures.
- **Size.** The live client: 35 loaded archives, 25 GB, 228k unique names,
  245 DBCs (25 from custom patches), 6,777 custom M2s. A full scan in
  Python took 15 s; in Rust the hashing and table work is milliseconds and
  what remains is decompressing about 1 GB of DBCs and M2 headers.
- **Case.** Wine finds files case-insensitively; the live `Data` has both
  `patch-w.mpq` and `patch-W.mpq`, which NTFS could not hold.

## Decisions

### The environment is Windows

The design assumes the most restrictive environment, Windows, where two
names differing only in case are one file. So:

- An archive is identified by its path from the game folder **compared
  case-insensitively**; the load order dedups on that, and nothing the
  launcher writes (cache files, messages, paths it creates) relies on case.
- Two archives whose names differ only in case (`Data/patch-w.mpq` and
  `Data/patch-W.mpq`) are **case-twins**: something that can't exist on
  Windows and whose order the game leaves undefined. The scan doesn't order
  them. Their files count as one archive at that slot (the union; where both
  provide a name, the first by byte order is taken and the choice is noted
  as arbitrary), so the graph never reports a false unmet need because of
  them, and the File integrity check reports the pair as a problem for the
  player to resolve. The install gate treats a twin pair as one path, which
  `mods::at` already finds whatever its case.
- The cache names its files by a hash of the archive's exact relative
  path, so twins don't collide, and no two cache files differ only in case.

### What counts as a break

The scan builds the **graph**: for the client as it is now, the archives
the game loads in order, the **provider** of each file name (the loaded
copy that wins), and the **needs** of each winning file (the names its
tables or model headers reference). An install is **simulated**: the
archives it backs up or removes leave the list, the release's archives
join it at the slots their paths give them, and the graph is built again.

A **break** is a need that is **met before the install and unmet after
it**, of a file that is still the winning copy after the install, where
that file comes from:

- a **custom** archive (not Blizzard's: see below), the player's own or a
  mod's;
- or one of **the mod's own** archives being installed. Patch Y's tables
  needed patch-F's models; the install that set patch-F aside broke itself.

Needs of Blizzard's archives are left out: the base game ships with a few
hundred references nothing provides and runs fine, and a message starting
"Your custom Data/common.MPQ" would be wrong. Needs that are unmet already
are not a break either: the install made nothing worse. A need met only by
an archive present but not loaded (`.backup`, `.disabled`) is not met.

The check is per file, not per archive, so an update whose new archive
still provides what the old one did breaks nothing, and an alternative
edition taking the place of another is fine as long as the files that
others need are still provided.

What this can't see: two archives providing a model and its skins in
versions that don't match. Out of scope.

### Blizzard's archives and custom ones

Blizzard's archives are the retail 3.3.5a layout, which the loader's table
fixes: `expansion`, `lichking`, `common`, `common-2`, `patch`, `patch-2`,
`patch-3`, and in the locale folder `locale-`, `speech-`,
`expansion-locale-`, `lichking-locale-`, `expansion-speech-`,
`lichking-speech-`, `patch-<locale>`, `patch-<locale>-2`, `patch-<locale>-3`
(`base-` and `backup-` are never loaded). That list is data about the game,
like `Locale`, and lives in core. Everything else is **custom**: the
player's, a torrent's (the HD client ships `patch-a`…`patch-x` and Lau's
Patch Y), or a mod's. An archive a mod installed is known by
`Mods::owned`, and messages name the mod instead of calling it custom.
Archives a torrent shipped are the player's client all the same: "Your
custom …" is the one wording.

A torrent is not the baseline: its archives are custom too, since their
needs are exactly what an install can break.

### Files are keyed by their two name hashes

The hash table keys a file by two 32-bit Storm hashes of its normalized
name (ASCII uppercase, `/` as `\`). The graph uses that pair as the key,
`core::mpq::Key(pub u32, pub u32)` (Copy, Eq, Ord, Hash; serde as 16
lowercase hex digits), from `core::mpq::key(name)`, so archives without a
listfile take part, and names matter only for messages and for telling a
file's kind. A **name pool**, the union of every listfile plus the names
that tables and model headers reference, gives names to keys; the files it
can't name (314 in the live client) are "files" in messages.

### Overrides worth the warn-and-ask

A mod's archive **overrides** a file when, after the install, its copy is
the winning one and before the install the winner came from a **custom**
archive (not Blizzard's, not the mod's previous release, not an alternative
it takes the place of). Overriding Blizzard's files is what patches do and
says nothing. The confirm box lists the overrides grouped by the archive
overridden and by the kind of the files (tables, models, textures, …),
naming `DBFilesClient` tables one by one, since a whole table replaced loses
everything the other patch changed in it, and counting the rest.

### A mod overridden by a later patch

The same simulation shows the reverse: files the release brings whose
winning copy, after the install, is another custom archive's. In the live
client `Data/patch-w.mpq`'s `WorldMapArea.dbc` and `DungeonMap*.dbc`
override Dungeon Maps' (`Data/enUS/patch-enUS-M.MPQ`), since locale patches
load below `Data\patch-?`. The mod's row says so once installed, and
installing it says so in the confirm box, with the same "anyway?".

### Everything is computed locally

The list of mods stays as it is: no description of archives' contents in
`mods.json`, nothing new for the indexer. The launcher reads a release's
archives itself, once per archive ever, and keeps their facts in its own
cache by SHA-256 (the sum `mods.json` already lists for each file), beside
the client scan's cache. Each launcher does the job.

The consequence: before the first download of a release, the launcher
doesn't know what its archives provide or need, so the gate can't decide
then. The install flow becomes:

1. **Install pressed.** If every archive of the release has its facts in
   the cache (an earlier install, in this or another client; or the release
   has no archive, like Lumenveil's DLLs or DXVK's), the whole gate runs
   now from the client's graph, and the flow is the one below from step 4.
   Otherwise the install starts with the download, as it does today, with
   no question yet: without the facts, a break by what the install removes
   might be healed by what it brings, and the launcher never blocks on a
   guess.
2. **After the download, before anything is placed or backed up**, the
   release's archives are read from the download (one more step in the
   progress: "looking at its archives") and their facts cached by SHA-256.
3. **The gate runs** on the client's graph with the simulated change.
4. **A break** stops the install: nothing is backed up, nothing placed, the
   download stays in the launcher's cache; the row shows the block sentence
   and the Install button is off with it as its tip. **Anything to ask**
   (files to back up or override, a copy installed by hand, overrides, a
   higher patch overriding the mod) shows the confirm box, with every line
   at once, and the install waits for "Install anyway?"; cancelling keeps
   the download in the cache. **Nothing to say** places the files at once.

So the confirm box is asked once, when the launcher knows everything:
before the download when the facts are cached, after it the first time. A
mod without archives keeps today's flow exactly. Once the facts are cached,
the row shows the gate's verdict without pressing anything: the block
sentence, or the "would override" and "would be overridden" lines appear
beside "In conflict with …" so the player knows before pressing Install.
Before the facts are known, the row says nothing about it.

The mod's archives installed in a client are read again by the client scan
under their stamp, like any archive; the two caches are independent and
small.

### The File integrity check covers every client

The "File integrity" **Check** in the clients dialog is enabled for every
client. It runs the **patch checks** from the scan's results, which cost
nothing: case-twins, patches whose needs are unmet now, archives that
couldn't be read. It also **hashes every installed mod's files** against
their release (see below). With a torrent set it also compares the files
with the torrent, as today, in the same job and the same results dialog.
Without one, the results dialog ends with a short note that setting the
client's torrent would let the check compare its files with it too. The
button's tip ("Set the client's torrent first…") goes away.

**Which unmet needs** it lists: those that a file **present in the client
but not loaded** would meet (`X.mpq.backup`, `X.mpq.backup-1`,
`X.mpq.disabled`, or any MPQ in `Data` or `Data/<locale>` whose name the
game never enumerates, like `patch-W.Ompq`). Those are actionable: rename
it back. Needs nothing in the folder meets are left out: the live custom
M2s have 224 of them (Shadowlands glue-screen textures and the like), and
the player can do nothing about them. The check only tells: restoring a
backup stays the player's move, as decided.

### The mods' files: stamps at startup, hashes in the check

Today every look at a client (`mods::check` → `compare`) hashes every file
of every installed mod's release, over a gigabyte of SHA-256 for Patch Y
and the HD pack, and Play waits for it. From now on:

- When a file is found as released, its **size and mtime** are noted with
  the mod's marker in `.old_launcher.toml` (the torrent's `Verified` does
  the same). A look trusts a file whose stamp matches and hashes only the
  rest: the first look after this ships hashes everything once, later ones
  read nothing. Missing files and files whose stamp changed are reported as
  today (`Problem::Missing`, `Problem::Changed`).
- The **File integrity check** hashes every installed mod's files whatever
  their stamps, through `mods::check` in a verifying mode, and its result
  replaces what the look knew: a file that changed under an unchanged stamp
  surfaces as `Problem::Changed`, which the mod's row and the banner above
  the addons already know how to say and repair. The results dialog lists
  it in its own words and points at the row: "“Patch Y”'s Data/patch-y.mpq
  is not as released: Repair, under Mods, puts it back." Nothing new to
  repair with: the existing Repair is the fix.

### The background scan

- **How it runs.** A `Task::sip` over a `spawn_blocking` scan, the way
  `work` installs a mod: one scan per client folder at a time, a scan
  asked for meanwhile running after it (as `finding` does in
  `src/clients/mods.rs`). It starts when a client opens (beside `rescan`),
  after an install or removal (`files_changed`), on F5, and when the window
  regains the focus if the `Data` folders' listing (names, sizes, mtimes)
  changed since the last scan.
- **Progress, without timers.** The blocking thread takes an `Instant` when
  the scan starts and puts the **time since then** in every progress
  message. A message goes out when an archive starts and when it ends, and
  **inside an archive** every 200 files read, so a long one (patch-H with
  40k files and most of the custom M2s) is never silent for more than a
  fraction of a second: the reader package 2 reads files through a trait,
  and the scan wraps that reader with one that counts reads and sends
  progress through a `tokio::sync::mpsc` unbounded sender, which the sipper
  forwards. No `Subscription`, no sleep, no clock in the UI.
- **The banner** appears with the **first progress message that says more
  than 2 s have passed**, and stays until the scan ends; a scan that
  finishes sooner shows nothing. It sits above the addons like the mods'
  banners, tone Warning, with the gliding activity bar: "Looking at this
  client's patches: Data/patch-H.mpq, 12 of 35…". Nothing is said on
  success.
- **What it gates**, from the first moment, banner or not. `holds_back(root)`
  is true from the moment the client opens until the scan's result is in:
  Play is off, and the mods' rows show "Looking…" with the buttons off
  (today's state while `found` has no entry), their tip "Looking at this
  client's patches… Wait until it's done." The Check button waits the same
  way. A scan that **fails** as a whole (the `Data` folder can't be listed)
  reports the error and releases Play: the player is never locked out for
  good. An archive that can't be read is noted and skipped; the rest of the
  scan goes on.
- **The cache**, `~/.cache/old_launcher/mpq/<client>/<archive>.json`
  (`%LOCALAPPDATA%\old_launcher\mpq\…` on Windows), `<client>` the first 16
  hex digits of the SHA-256 of the canonical client path, `<archive>` the
  same of the archive's path relative to it; and
  `~/.cache/old_launcher/mpq/releases/<sha256>.json` for the mods'
  archives. Each client file holds the archive's path, size and mtime, and
  its **facts** (below). A matching stamp skips the archive entirely: a
  second startup reads 35 small JSON files and builds the graph in well
  under a second. Files of archives no longer in the folder go at the end
  of each scan; a client's whole folder goes when the client is removed
  from the launcher; the releases' files stay (a few hundred KB each).
  Blizzard's archives never change, so their facts are computed once per
  client.
- **Facts, not resolved needs, are cached.** A table's needs depend on other
  tables (CreatureDisplayInfo's textures resolve against the model path in
  CreatureModelData, which may come from another archive), so the cache
  stores what an archive *says*: its names, its keys with their flags, the
  interesting columns of its tables, and each of its M2s' skins, anims and
  textures as keys. Resolution happens when the graph is built, in memory,
  in milliseconds.

### The reader is our own

A minimal reader is a few hundred lines: `mpq 0.8.1` reads whole archives
into memory and panics, `wow-mpq` drags in 118 crates. zlib (`flate2` on
`zlib-rs`) and bzip2 (`bzip2` on `libbz2-rs-sys`) are already in the tree
through the `zip` crate; core gains `bzip2` as a direct dependency and no
C code.

Shape: it streams. The hash and block tables are read once (at most 8 MB
each); a file is read sector by sector from a `BufReader<File>`, never the
whole archive; a byte range of a file reads only the sectors it spans, so
M2 headers cost a few sectors, not the model. It never panics on bad data:
every length is checked, every table index bounded, unknown compression
bytes and sizes that don't add up are errors naming the file and the
archive. Memory target for the first full scan of the live client: under
150 MB over the launcher's baseline (tables 33 MB, names about 20 MB).

### Other slow parts

Everything on the startup path already runs off the UI thread (`open`,
`scan`, `Details::read`, the mods' `look`, the torrent check). The one
that held Play was the mods' hashing, settled above (package 8). Nothing
else is worth it. Watching the folder for changes once the launcher runs,
in place of reading it again on focus, is a later idea:
`watching-the-client.md`.

## What the player sees

Paths are from the game folder, as the folder spells them. Mod names are
quoted “like this”. "Override" is the word, never "set aside" or "back up"
in these sentences (the confirm box's own "overrides it safely (backed up)"
lines stay as they are). Kinds of files: tables, models, textures, sounds,
interface, world, and "files" for the rest.

**Blocked** (the row shows the sentence in warning tone; the Install button
is off with the same sentence as its tip; after a download, the same
sentence where the progress was):

- Your custom Data/patch-G.mpq's models need Data/patch-F.mpq, which
  installing “Patch Y” would override.
- Your custom Data/patch-G.mpq's models and tables need Data/patch-F.mpq
  and Data/patch-E.mpq, which installing “Patch Y” would override.
- The tables of “Dungeon Maps” need Data/patch-F.mpq, which installing
  “Patch Y” would override. *(the needing archive is a mod's)*
- The tables “Patch Y” brings need Data/patch-F.mpq, which installing it
  would override. *(the mod's own needs)*

**Warn and ask** (lines in the confirm box, above the existing "Install
“Patch Y” anyway?"):

- Installing “Patch Y” would override the tables your custom
  Data/patch-G.mpq replaces: CreatureDisplayInfo.dbc and
  CreatureModelData.dbc.
- Installing “Patch Y” would override 212 models and 40 textures your custom
  Data/patch-H.mpq replaces.
- Data/patch-w.mpq would override the tables “Dungeon Maps” brings:
  WorldMapArea.dbc and DungeonMap.dbc. *(the reverse)*

**A mod's row**, once installed and overridden:

- Data/patch-w.mpq overrides the tables it brings: WorldMapArea.dbc and
  DungeonMap.dbc.

**The install's progress**, the first time a release is installed: "Step 2
of 4: looking at its archives" between the download and putting the files
in place.

**File integrity**, in the results dialog (each a line; the first few named,
then "and N more"):

- Data/patch-w.mpq and Data/patch-W.mpq differ only in case. Windows would
  allow only one of them, and the game may load either: rename one, or
  remove it.
- Your custom Data/patch-G.mpq's models need Data/patch-F.mpq, which isn't
  loaded: it is backed up as Data/patch-F.mpq.backup.
- Your custom Data/patch-G.mpq's models need Data/patch-7.mpq, which isn't
  loaded: it is switched off as Data/patch-7.mpq.disabled.
- Data/patch-X.mpq couldn't be read, so what it changes is unknown.
- “Patch Y”'s Data/patch-y.mpq is not as released: Repair, under Mods, puts
  it back.
- “Lumenveil”'s version.dll is missing: Repair, under Mods, puts it back.

Without a torrent, nothing found:

> Nothing is wrong with this client's patches, and every mod is as
> released.
>
> Want a deeper check? Set the torrent this client came from, and the
> check will also compare every file with it and repair what's off.

Without a torrent, problems found: the lines above under "This client has N
things to look at:", then the same note. With a torrent: today's "Every
file is as its torrent has it: there is nothing to repair." or the repair
list, then the lines (or "Nothing is wrong with its patches, and every mod
is as released."). The one-line summary in the row, after a check: today's
text for the torrent, plus "Nothing is wrong with its patches." or "N
things to look at: see the check." for every client.

**The banner**, after 2 s: "Looking at this client's patches:
Data/patch-H.mpq, 12 of 35…". The disabled buttons' tip, from the start:
"Looking at this client's patches… Wait until it's done."

## Shape of the code

Core, no GUI, no network:

- `core/src/mpq.rs`: the format. `Archive::open(path)`, `entries()` (every
  existing hash entry: key, block index, flags, sizes, delete marker),
  `read(key)`, `read_range(key, range)`, `listfile()` (`None` without one),
  `key(name) -> Key`, `Key`'s 16-hex serde. `core/src/mpq/writer.rs`,
  under `#[cfg(any(test, feature = "testing"))]`: writes a small v0 archive
  (hash table, block table, zlib or stored sectors, optional encryption and
  FIX_KEY, optional listfile, deleted entries) for every test here and in
  `src/`.
- `core/src/patches.rs`, with `patches/order.rs`, `patches/facts.rs`,
  `patches/graph.rs`, `patches/cache.rs`:
  - `order(root, locale) -> Order`: the loaded archives with their
    priorities, by path as on disk, case-twins grouped; and the MPQs
    present but not loaded, each with why (`.backup`, `.disabled`, a name
    the game never enumerates).
  - `BLIZZARD`: the retail names; `is_blizzard(path, locale)`.
  - `Facts::of(&impl Source, known: &[String]) -> Facts`: names, keys with
    kind and flags, the tables' interesting columns, each M2's references;
    `known` are the other archives' names, tried on an archive without a
    listfile since models are read by name. `Facts` is the
    cache's content; it reads files through the `Source` trait in
    `facts.rs`, which `Archive` implements and which the scan wraps to
    count reads.
  - `Cache::at(dir)`: `of_archive(root, path, pool, compute) -> Facts`
    keyed by path, size, mtime and `pool` (a digest of the name pool for an
    archive without a listfile, empty otherwise) under `<client>/`,
    `of_release(sha256, compute) -> Facts` under
    `releases/`, `sweep(client, keep)`, `forget(client)`. Files are
    written beside and renamed over.
  - `Graph::build(&[(priority, path, &Facts)])`: providers, winning files,
    resolved needs with who meets them. `unmet(&self) -> Vec<Need>` and
    `unmet_but_present(&self, &Order)` for the integrity check.
  - `Change { adds: Vec<(path, sha256)>, removes: Vec<path> }` and
    `Graph::gate(&self, &Change, facts: &BTreeMap<sha256, Facts>) ->
    Gate { breaks, overrides, overridden }`, each with what the messages
    need (archive, owner, kinds, names).
- `core/src/mods.rs`: `change(root, mod, release, alternatives) -> Change`
  listing what `install_downloaded` would back up (`foreign_of`,
  `excluded`), remove (the alternatives' and the mod's own files, by
  splitting `uninstall_of` into a listing and a removal) and place
  (`release.placed`); the stamps in `Marker` and a verifying mode of
  `check`.

The launcher:

- `src/patches.rs`, a state like `game.rs`: scans by client folder, the one
  running with its progress and when it started, the cache, `update`,
  `banner`, `holds_back`, `graph(root)`, `findings(root)` for the integrity
  check, `forget(root)` when a client is removed, and the focus-regain
  listing check. `lib.rs` owns it and routes `Message::Patches`.
- `src/clients/mods.rs`: `work` split in two tasks, `fetch` (download,
  facts, gate) and `place` (today's `install_downloaded` and setup), with
  the pending install held between them; `gates: BTreeMap<PathBuf,
  BTreeMap<String, Gate>>` computed once `Found`, the graph and the facts
  are in; `blocked` consults the breaks, `conflicting` adds the overrides,
  the row shows the lines.
- `src/clients.rs` and `src/download.rs`: the Check for every client.

## Work packages

Each package is sized for one agent in a worktree. Tests are named as
sentences and use synthetic fixtures only (the writer in `mpq/writer.rs`,
`core::testing::client_with`); tests on the real clients are `#[ignore]`d
and read `OLD_LAUNCHER_TEST_CLIENT`. Every package runs `cargo fmt --all`,
`cargo clippy --workspace --all-targets` and `cargo test --workspace`
clean, and updates the module docs it touches; DEVELOPMENT.md gets one
line under Layout for the new modules and one under Tests for the new
ignored test, nothing more. Packages 1 and 2 are under way and share the
interface fixed above (`Key`, `key`, the `Source` trait).

### 1. The reader (`core/src/mpq.rs`, `core/src/mpq/writer.rs`)

Touches: `core/src/mpq.rs` (new), `core/src/mpq/writer.rs` (new),
`core/src/lib.rs`, `core/src/error.rs`, `core/Cargo.toml` (`bzip2`,
default features off, `libbz2-rs-sys`; it is in `Cargo.lock` already).

Implements: header v0/v1 (other versions: an error naming the archive),
the Storm hash table built from seed 0x00100001 and the four hash kinds
(index, name A, name B, file key), table decryption (keys `(hash table)`
0xC3AF3770 and `(block table)` 0xEC83B3A3), probing with wrap-around past
deleted entries (0xFFFFFFFE) and ignoring locale and platform, block flags,
sector reading with the offset table (decrypted with key − 1 when
encrypted, one extra entry under SECTOR_CRC), sectors raw when their packed
length equals their length, 0x02 zlib and 0x10 bzip2, stored files,
SINGLE_UNIT, the file key from the base name and FIX_KEY
(`(key + offset) ^ size`), `read_range`. Name normalization works on
bytes, not UTF-8.

Tests (`mpq::tests`):
- `names_hash_as_the_game_hashes_them`: `(listfile)` → index 0x5F3DE859,
  A 0xFD657910, B 0x4E9B98A7, key 0x2D2F0A94; `Interface\Icons\INV_Misc_QuestionMark.blp`
  and `interface/icons/inv_misc_questionmark.blp` hash alike (A 0x80C4C06C,
  B 0x1E693E27).
- `a_written_archive_reads_back_file_by_file` (zlib, raw and stored
  sectors, a single-unit file, a file spanning several sectors).
- `encrypted_files_decrypt_with_and_without_a_fixed_key`.
- `deleted_entries_are_probed_past_and_a_delete_marker_is_told`.
- `an_archive_without_a_listfile_still_lists_its_keys`.
- `a_range_reads_only_the_sectors_it_spans` (a counting reader).
- `garbage_truncation_and_wrong_sizes_are_errors_not_panics` (fuzz-ish:
  every prefix of a valid archive, flipped header fields).
- `#[ignore]` `every_archive_of_a_real_client_reads_its_listfile`: opens
  every `*.MPQ`/`*.mpq` under `$OLD_LAUNCHER_TEST_CLIENT/Data`, reads each
  listfile, and reads the first and last named file of each.

Accepts when: the tests pass; clippy clean with no `unwrap`/`expect`;
the ignored test passes on the test client and on the live one (read
only) with peak RSS under 50 MB (`/usr/bin/time -v`); the writer is
documented as test-only.

Depends on nothing. Runs in parallel with 2.

### 2. Facts from tables and models (`core/src/patches/facts.rs`)

Touches: `core/src/patches.rs` (new, the module and `Facts`, `Kind`,
`BLIZZARD`), `core/src/patches/facts.rs` (new), `core/src/lib.rs`.

Implements, as pure functions over bytes: the WDBC header and record
layout with fixed 4-byte fields (the five DBCs with byte fields aren't
read); the columns of the six tables in "What is known", kept as the
cached columns (`CreatureModelData` id → path; `CreatureDisplayInfo` id →
model id, three texture names; `CreatureDisplayInfoExtra` → bake name;
`ItemDisplayInfo` → two models, two textures, icon, eight components;
`GameObjectDisplayInfo` and `SpellVisualEffectName` → path); the M2
header (magic `MD20`, version 264 only; others noted as unloadable), skin
count, sequences with the inline flag 0x20 and alias chains, type-0
texture names; `.mdx`/`.mdl` → `.m2`; a file's `Kind` from its extension;
`Facts` with serde (keys as 16 hex digits) and `Facts::of` over the
`Source` trait (so this package needs no real reader: tests feed it an
in-memory map of name → bytes).

Tests, each building its bytes in the test:
- `a_table_yields_the_paths_of_its_fixed_columns`.
- `a_small_integer_is_never_taken_for_a_string` (the heuristic trap).
- `a_model_names_its_skins_anims_and_textures` (alias chains, inline
  sequences skipped, a 272 model noted as unloadable).
- `mdx_and_mdl_names_mean_m2`.
- `facts_round_trip_through_json`.
- `a_table_with_byte_fields_is_left_alone`.

Accepts when: tests pass; `Facts` is documented as the cache's content; no
panics on truncated bytes.

Depends on nothing. Runs in parallel with 1.

### 3. The order and the graph (`core/src/patches/order.rs`, `graph.rs`)

Touches: `core/src/patches.rs`, `core/src/patches/order.rs` (new),
`core/src/patches/graph.rs` (new), `core/src/patches/facts.rs`
(`Archive` implements `Source`).

Implements: `order(root, locale)` following the loader (the recipe in the
report; base rows 40–49, the two enumerations, the descending
case-insensitive sort, the two appended, priorities from 64), with
case-twins grouped into one slot and the MPQs present but not loaded with
their reason; `is_blizzard`; `Graph::build` (providers by key, highest
priority wins, a delete marker hides lower copies as StormLib does, which
the report notes as unconfirmed in `Wow.exe` and which can only make the
base game look worse, never an install), the name pool, resolution of
every table's columns into needs (CreatureDisplayInfo textures against the
winning CreatureModelData's folder; ItemDisplayInfo models probed in the
seven component folders and helmets by `_<RaceSex>` suffix; components in
the eight regions with `_U/_M/_F`), each M2's needs, `unmet`,
`unmet_but_present`; `Change`, `Graph::gate` with `breaks` (per the rule
above), `overrides`, `overridden`.

Tests, with tiny archives from the writer in a `testing::client_with`
folder:
- `patches_load_in_the_games_order` (digits, letters, `_`, locale below
  `Data` for enUS, `patch.MPQ` and `patch-enUS.MPQ` last, `.disabled`,
  `.backup` and `patch-10` left out, an archive that doesn't open skipped).
- `two_names_differing_only_in_case_are_one_slot_and_reported`.
- `the_highest_patch_provides_a_file_and_a_delete_marker_hides_it`.
- `a_tables_textures_resolve_against_the_model_from_another_archive`.
- `a_need_met_by_a_backup_is_unmet_but_present`.
- `an_install_that_removes_what_a_custom_patch_needs_is_a_break`.
- `an_install_whose_own_tables_need_what_it_removes_is_a_break`.
- `an_install_that_only_replaces_blizzards_files_is_quiet`.
- `an_install_that_replaces_a_custom_patchs_table_is_an_override`.
- `an_install_whose_files_a_higher_patch_overrides_is_overridden`.
- `an_update_that_still_provides_what_others_need_breaks_nothing`.
- `a_need_unmet_before_the_install_is_not_a_break`.
- `#[ignore]` `a_real_clients_order_matches_the_reference`: prints the
  order of `$OLD_LAUNCHER_TEST_CLIENT` for comparison with
  `scripts/loadorder.py`, and asserts the live client's known facts (35
  loaded, `patch-enUS-M` below `patch-w`).

Accepts when: tests pass; a full `Facts` scan of the live client (read
only, ignored test, `--release`) finishes in under 10 s warm and under 150
MB RSS; its order equals `loadorder.py`'s.

Depends on 1 and 2.

### 4. The caches and what an install changes (`core/src/patches/cache.rs`, `core/src/mods.rs`)

Touches: `core/src/patches/cache.rs` (new), `core/src/patches.rs`,
`core/src/mods.rs` (`change`, the `uninstall_of` split into a listing and
a removal), `core/src/error.rs`.

Implements: `Cache` with `of_archive` (stamp-keyed under `<client>/`),
`of_release` (SHA-256-keyed under `releases/`), `sweep`, `forget`, JSON
written beside and renamed over, a file that doesn't read treated as
absent; `mods::change`, mirroring `install_downloaded` exactly (what
`foreign_of` and `excluded` back up, what the alternatives' and the mod's
own `uninstall_of` would remove, what `release.placed` places), with
`install_downloaded` using the same listing so the two can't drift.

Tests:
- `an_archive_is_read_once_until_its_stamp_changes` (a counting reader).
- `a_releases_facts_are_kept_by_checksum_whatever_the_path`.
- `a_cache_file_that_doesnt_read_is_computed_again`.
- `sweeping_keeps_the_archives_present_and_forgetting_removes_the_client`.
- `a_change_lists_what_an_install_backs_up_removes_and_places` (against
  the fixtures of `installing_replaces_what_was_there_but_the_players_files`
  and `installing_an_alternative_takes_the_place_of_the_one_there`: what
  `change` lists is what the install then did).

Accepts when: tests pass; no two cache file names differ only in case.

Depends on 2 (the `Facts` type) and 1 (to read archives); runs in
parallel with 3 and 5.

### 5. The background scan, the banner and the gating (`src`)

Touches: `src/patches.rs` (new), `src/lib.rs` (the state, messages,
`holds_back` in the Play message and button, the banner, `Opened`,
`files_changed`, `Focused`, F5, `forget` on client removal), `src/clients/
mods.rs` (the "Looking…" state and tip also wait for the scan), `src/
clients.rs` (passes the state through), `src/style.rs` if a tone is
missing.

Implements: the scan as a sipper on `spawn_blocking` reading `Facts` per
archive through the cache, with the counting `Source` wrapper sending
progress (archive started, every 200 files, archive ended) through a
`tokio::sync::mpsc` unbounded channel that the sipper forwards, each
message carrying the time since the scan started; `Graph::build` at the
end, the result kept per client folder; the sweep; the banner shown from
the first progress past 2 s; the gating of Play, the mods' buttons and the
Check button from the start; focus regain compares the `Data` listings and
rescans on change; failure handling as decided.

Tests (`iced_test::simulator`, a `testing::client_with` folder with two
tiny archives from the writer, the `testing` feature on core; progress
messages are fed by hand with the elapsed time they carry):
- `play_and_the_mods_buttons_wait_for_the_patches_to_be_looked_at`.
- `the_banner_appears_with_the_first_progress_after_two_seconds_and_not_before`.
- `a_long_archive_still_reports_progress_every_two_hundred_files`.
- `a_second_look_reads_only_what_changed` (truncate an archive without
  touching its stamp: the cache is used; touch it: it is read again and
  reported unreadable).
- `an_archive_that_cant_be_read_is_skipped_and_the_rest_is_known`.
- `a_scan_that_fails_releases_play_and_reports`.
- `regaining_the_focus_looks_again_only_when_data_changed`.
- `removing_a_client_forgets_its_cache`.

Accepts when: tests pass; on the live client the first startup shows the
banner after 2 s and Play enables within about 10 s, the second startup
shows no banner and enables Play within a second (logged timings);
`RUST_LOG=old_launcher=debug` names each archive read and each cache hit.

Depends on 3 and 4's `Cache` (start from 4's branch if 4 isn't merged;
`of_archive` is all it needs). Runs in parallel with 4's `mods::change`.

### 6. The install gate (`src/clients/mods.rs`)

Touches: `src/clients/mods.rs` (the two-task install, the pending install,
gates, `blocked`, `conflicting`, the row lines, the confirm box lines, the
new progress step), `src/lib.rs` (hands the graph to the mods when a scan
or a look ends), `docs/plans/mod-table-overlap.md` (deleted).

Implements the flow of "Everything is computed locally": `fetch`
(download as today, then `Cache::of_release` for every `.mpq` of the
release, then `Graph::gate` on `mods::change`) ending in `Fetched { gate,
downloads }`; a break shows its sentence where the progress was and in the
row, and the button goes off with it; anything to ask fills the confirm
box; "Install anyway?" or nothing to ask runs `place` (today's
`install_downloaded` and setup). When the facts are cached already, the
gate runs on `Found` and the verdict shows in the row before any press, and
the confirm box comes before the download as today. The row's lines: the
block sentence, "would override", "would be overridden", and for an
installed mod "overrides the tables it brings". Nothing is gated for a mod
being repaired at the same release. A release's archive that can't be read
is treated as providing and needing nothing, with a debug log line: the
gate works on what it removes.

Tests (simulator, with the writer's archives in a `testing` client):
- `a_first_install_downloads_looks_at_its_archives_and_then_asks`.
- `an_install_that_would_break_a_custom_patch_stops_before_touching_anything`.
- `an_install_that_overrides_a_custom_patchs_tables_asks_first`.
- `an_install_a_higher_patch_would_override_says_so_and_asks`.
- `a_release_whose_facts_are_cached_is_judged_before_the_download`.
- `an_installed_mod_says_which_patch_overrides_it`.
- `a_mod_without_archives_keeps_todays_flow` (Lumenveil's existing tests
  stay green as they are).
- `cancelling_after_the_download_keeps_it_for_next_time`.

Accepts when: tests pass; in a copy of the live client (never the live one)
with Lau's old rules replayed (patch-F in `excludes`), Install on Patch Y
downloads, then shows the blocked sentence of the Why section word for
word and leaves every file untouched; with the current rules it shows the
override warning for patch-G's tables; pressing Install again shows the
verdict before downloading anything.

Depends on 3, 4 and 5.

### 7. The File integrity check for every client (`src/clients.rs`, `src/download.rs`)

Touches: `src/clients.rs` (`integrity_of`: the button enabled for every
client unless a download, check or scan is under way; the tip about the
torrent removed; `Message::Check` no longer requires a torrent), `src/
download.rs` (`Request::Check` takes the patch findings; without a saved
torrent the job goes straight to a new `Stage::Checked(Findings)` instead
of `Finding`; `Stage::Intact` and `Stage::Damaged` render the findings
after their own lines; `integrity()` adds the one-line summary; the
no-torrent note), `src/lib.rs` (`Instruction::Check` fetches
`patches.findings(root)` and passes it on), `src/patches.rs`
(`Findings { twins, unmet_but_present, unreadable }` from the scan's
`Order` and `Graph`, plus the sentences).

Rendering: without a torrent and nothing found, the headline and the note;
with problems, "This client has N things to look at:" and the lines (first
few named, "and N more"), then the note; with a torrent, today's dialogs
with the findings appended. The row's summary as in "What the player
sees". The patch findings are read from memory: that part of a check costs
nothing beyond the torrent comparison. The mods' lines come with package 8;
this package leaves room for them (`Findings.mods`, empty).

Tests (simulator, `src/download.rs` and `src/clients.rs`):
- `the_check_is_offered_to_a_client_without_a_torrent`.
- `a_check_without_a_torrent_says_all_is_fine_and_offers_the_deeper_one`.
- `a_check_without_a_torrent_lists_twins_and_needs_a_backup_would_meet`.
- `a_check_with_a_torrent_adds_the_patch_findings_to_its_results`.
- `the_rows_summary_counts_the_problems`.
- `the_check_waits_for_the_patches_to_be_looked_at`.

Accepts when: tests pass; on the live client (no torrent) Check reports
`Data/patch-w.mpq` and `Data/patch-W.mpq` as twins; with `patch-F.mpq`
renamed to `.backup` in a copy, it reports patch-G's unmet need.

Depends on 5. Runs in parallel with 6.

### 8. The mods' files: stamps at startup, hashes in the check (`core/src/mods.rs`, `src/download.rs`)

Touches: `core/src/mods.rs` (`Marker.files: BTreeMap<String, Stamp>` with
serde default, noted by `check`/`compare` when a file is found as
released; `compare` trusts a matching stamp; `verify(root, mod,
alternatives)` hashing everything and renewing the stamps), `core/src/
notes.rs` if `Stamp` moves out of `torrent.rs` to be shared, `src/clients/
mods.rs` (`look` unchanged; a `Message::Verified` taking the check's
result as a `Found`), `src/download.rs` and `src/patches.rs`
(`Findings.mods`: the check runs `verify` for every installed mod in its
blocking task, with progress, and lists the problems with the "Repair,
under Mods" sentence), `DEVELOPMENT.md` ("No database": one clause on the
stamps).

Tests:
- `a_file_found_as_released_is_not_hashed_again_until_its_stamp_changes`
  (core; a counting read).
- `a_marker_without_stamps_is_read_as_before` (1.1.2's notes).
- `verifying_hashes_everything_and_renews_the_stamps`.
- `the_check_lists_a_mods_file_that_changed_under_its_stamp_and_the_row_offers_repair`
  (simulator).

Accepts when: tests pass; a second `look` at a client holding Patch Y and
the HD pack reads no archive (debug log), and Check hashes them all and
reports a byte flipped in `Data/patch-y.mpq`.

Depends on 7.

### Order and parallelism

1 ∥ 2 → 3 ∥ 4 (4 needs only 1 and 2) → 5 → 6 ∥ 7 → 8. Each later package
starts from `main` once what it depends on is merged. All eight get built.
