# Contributing to Old Launcher

How to build, run and test the launcher, where things are, and the rules
the code follows. What each part does in detail is in the module docs
(`//!` at the top of each file): this guide points at them rather than
repeating them.

## Building and running

Stable Rust, edition 2024. The workspace has three crates and three patches:

```sh
cargo run --release              # the launcher
cargo run --release -p indexer   # rebuilds index/wotlk.json and index/mods.json (see The catalog)
cargo clippy --workspace --all-targets
cargo test --workspace
cargo fmt --all                  # before every commit
```

The launcher logs to stderr; `RUST_LOG=old_launcher=debug` says more.
While working on it, these point it elsewhere:

| Variable | Points the launcher at |
|---|---|
| `OLD_LAUNCHER_INDEX=<url or path>` | another catalog, such as a local `index/wotlk.json` |
| `OLD_LAUNCHER_MODS=<url or path>` | another list of mods |
| `OLD_LAUNCHER_CONFIG=<path>` | another settings file |
| `GH_TOKEN` / `GITHUB_TOKEN` | a GitHub login for downloads (never stored) |

Its own files, outside the clients: the settings
(`~/.config/old_launcher/config.toml`, `%APPDATA%\old_launcher\config.toml`)
and a cache (`~/.cache/old_launcher`, `%LOCALAPPDATA%\old_launcher`) of the
catalogs and of the clients' `.torrent` files, by info hash, which the
launcher gets again from the peers when a copy is missing.

## Layout

- `core/`: everything that reads or changes a client folder, with no GUI
  and no network: `installation` (a client and every change made to it),
  `snapshot` (what it holds), `marker` and `notes` (what the launcher
  leaves in it), `library` (what the list shows), `catalog` and `mods`
  (the data the launcher works from), `launch` (how the game starts),
  `torrent` (comparing a client with its torrent), `patches` (what the
  archives of `Data` say about each other), `config` (the settings
  file), `locale` (the game's languages, the only ones a `Data` folder can
  be of), `mpq` (a streaming reader of the game's archives, with a writer
  of small ones for tests). Tested against temporary folders.
- `src/`: the [iced](https://github.com/iced-rs/iced) app. `lib.rs` is the
  shell; each screen is a module with its own `State`, `Message`, `update`
  and `view` (`setup`, `clients`, `settings`, `download`, `addons` with its
  cards and dialogs, `patches` for the background look at a client's
  archives). `torrent.rs` wraps librqbit, `github.rs` signs
  downloads in, `discord.rs` is the wrapper that shows the game on Discord,
  `style.rs` the look. Screens are tested with `iced_test::simulator`,
  downloads between two librqbit sessions on localhost.
- `indexer/`: the CLI that builds the catalog and the list of mods, one
  module per upstream source (`source/`) and per mod (`mods/`).
- `librqbit-sha1-wrapper/`: librqbit's hashes on RustCrypto, patched in
  over its OpenSSL and ring options, so no C crypto library gets built.
- `patches/`: iced's `iced_graphics` and the `cosmic-text` it uses, at the
  commits iced pins, patched in so that a start reads no font file it
  doesn't draw with (`Cargo.toml` says how); each goes once its upstream
  does the same. They aren't workspace members, so `cargo test --workspace`
  doesn't run their tests; cosmic-text's need the fonts and images its
  repository keeps in Git LFS, which the copy leaves out (GitLab refuses
  a push of LFS pointers without their files).
- `index/`: the generated data, committed (below).
- `assets/`: icons and the knob as SVGs, the fonts, the README's screenshots.
- `docs/plans/`: work planned but not started, and the plans behind what
  was built.

## Rules

- **Nothing panics on purpose** outside tests: the workspace denies
  `unwrap`, `expect`, `panic!`, `unreachable!`, `todo!` and
  `unimplemented!` through clippy (`clippy.toml` exempts tests). Whatever
  goes wrong at run time ends in the error banner, which lists every
  problem until dismissed.
- **The launcher stores no secrets.** A GitHub login comes from the
  environment or, when the setting is on, from `gh auth token`, and stays
  in memory.
- **The game's own files are the player's.** `WTF`, `Interface/AddOns`,
  any `realmlist.wtf`, and what the game writes (`Cache`, `Logs`,
  `Screenshots`, `Errors`) are never compared with a torrent nor replaced
  from one; a mod's `settings` are kept by a reinstall; and the files of a
  mod installed are the mod's: a repair from a torrent leaves them alone;
  and while a client's game runs, nothing the game writes back on exit is
  touched.
- **No database.** Everything comes from the client folder: addon folders
  and their `.toc`, the `.old_launcher.toml` marker left in each folder the
  launcher installs (`core/src/marker.rs`), and one `.old_launcher.toml`
  beside `Wow.exe` for the torrent, what was verified against it, and the
  mods (`core/src/notes.rs`), with the size and mtime of each file found as
  released, so a look reads only what changed. A client forgotten and added
  back loses nothing.
- **Mods are data.** A mod is a module in `indexer/src/mods/` writing to
  `index/mods.json`; the launcher gains only generic capabilities (a file
  to put in place, a setup step to run, a setting to make), never code for
  one mod.
- **People read outcomes, not internals.** The app never says "catalog",
  "index" or "entry": it says the list of addons, what is available, what
  is up to date. Those words stay in the code, the logs and here.
- **Files are written beside and renamed over**: the notes, the settings,
  `Wow.exe`'s flag, files moved into place from a download, and the
  folders an install replaces. An install cut short leaves nothing half
  written.

## The catalog

`index/wotlk.json` is the launcher's own catalog, generated by the indexer
and committed; the launcher fetches it from `main` and keeps the last copy
it got, so the list shows instantly and offline. A weekly CI schedule
regenerates and commits it (`[skip ci]`: nothing built depends on it).

```sh
GITHUB_TOKEN=$(gh auth token) cargo run --release -p indexer
GITHUB_TOKEN=$(gh auth token) cargo run --release -p indexer -- --skip-curseforge
```

The token only raises GitHub's API limits (a run makes about two hundred
calls). CurseForge's API needs a key in `CURSEFORGE_API_KEY`: the
project's is a CI variable, and CurseForge's terms forbid sharing it.
Without one, `--skip-curseforge` leaves CurseForge out; the catalog it
writes then lacks those addons, so don't commit it. A key goes in single
quotes, since it has `$` signs.

The indexer lists every source, pins the downloads that move (a branch's
snapshot) to what they serve now, downloads every archive but those it
inspected before that haven't changed since (`index/archives.json`, by
URL; after changing how archives are inspected, remove the entries the
change affects, not the whole file, or CurseForge's archives are all
downloaded again), records the
folders each one installs, splits the archives in `split::ARCHIVES`,
compilations of unrelated addons, into one listing per addon by what their
`.toc` files require (`indexer/src/split.rs`), merges the listings of the
same addon, and orders each entry's downloads newest first.
`indexer/src/main.rs` and `source.rs` say how; each source's module says
what it takes and leaves out.

**Adding a source** is a module in `indexer/src/source/` implementing
`Source` (`info` describes it, `list` returns its listings) and a line in
the ordered list in `main.rs`; the first source listing an addon describes
it. A GitHub maintainer is one more name in `github::MAINTAINERS`, a single
repository one more `owner/name` in `github::REPOSITORIES` (plus a line in
`split::ARCHIVES` when it is a compilation of unrelated addons), a GitLab
project one more path in `gitlab::AUTHORS`.

## The list of mods

`index/mods.json` lists the mods: what each is, where it comes from, and
its releases, each with every file it puts in the client folder and its
SHA-256, what else it needs (settings, setup steps, `Config.wtf`
settings, platforms, where its files say their version), which clients it
suits, the files it can't be beside, and the mods it is an alternative
to. A path may name the client's language as `{locale}`, and a release
whose files differ by language has one download per language; a client
in a language it lacks gets another's, as the player picks. The list has
a format version: a change older launchers would misread raises it, and
they refuse the list. The same
indexer run writes it (`--mods-only` writes it alone); `core/src/mods.rs`
is the schema and what the launcher does with it, `src/clients/mods.rs`
the dialog section and the checks before the game starts.

**Adding a mod** is a module in `indexer/src/mods/` called from
`mods::gather`, which finds the releases and writes down what can't be
read from them (Lumenveil's knows its setup scripts by checksum; DXVK's
knows where its DLL is in the tarball; Lau's knows where each archive of
its zips goes, and checks their manifests say the same; Dungeon Maps'
takes each language's patch, with the checksum GitHub gives). A release the
module doesn't understand is left out rather than guessed at, and the
launcher goes on offering the last one listed; when that leaves a mod's
newest release out, the indexer exits with code 3 after writing, and the
schedule commits what it wrote and then fails, so its owner is mailed the
reason, which says what to teach the module.

## Starting the game

On Windows, `Wow.exe` from the client folder. Elsewhere, the client's
command run by `sh`, the way Steam runs launch options: `%command%` stands
for `Wow.exe`, the environment variables come first, and the default is
`umu-run` (Proton) or `wine`, under `gamemoderun` when installed, in a
prefix of the client's own in the user's data folder, never in the client
folder: `Wow.exe` goes through every file there as it starts and quits
(`core/src/launch.rs`). Mods' setups run
through the same command, which is why it must have `%command%`; the
module says what differs for them. With the Discord setting on, the game
runs as a child of the launcher's own binary started with `--discord`
(`src/discord.rs`).

## Clients from a torrent

`core/src/torrent.rs` says what a torrent holds, how a client folder is
compared with it and how a download's files get into it; `src/download.rs`
is the dialog that downloads a new client, resumes one, repairs one's
files and downloads or deletes its languages; `src/torrent.rs` the engine.
Downloads stage in a `.download` folder in the folder the files go in and
move into place once whole.

## Adding a screen or a setting

A screen is a module with its own `State`, `Message`, `update` returning
an `Action` (a `Task` for the runtime and an `Instruction` for the parent)
and `view`; `lib.rs` maps its messages and acts on its instructions. See
`src/settings.rs` for the smallest one. A setting is a field of
`core::Config` with a serde default, so settings files from before it load.

## The look

`src/style.rs` draws the game's login screens with iced's primitives:
colors read off the game's textures, Cinzel (OFL) for headings, Fira Sans
(OFL), regular and bold, for the body, own SVGs for the icons and the
dialogs' corner knobs. Nothing of Blizzard's is shipped.
`docs/plans/wotlk-look.md` has what each texture shows and where it went.
Screenshot renders must match across systems, so the fonts are bundled
and loaded by the app and the screenshot test alike.

## Tests

```sh
cargo clippy --workspace --all-targets   # also enforces the no-panic rule
cargo test --workspace
# Installs real catalog entries into a client, then uninstalls them
# (entries whose folders the client already has are skipped):
OLD_LAUNCHER_TEST_CLIENT="/path/to/World of Warcraft 3.3.5a" \
    cargo test --lib addons -- --ignored
# Downloads one file of each kind from GitHub, signed in with GH_TOKEN,
# GITHUB_TOKEN or the GitHub CLI's login:
cargo test --lib github -- --ignored
# Installs the newest Lumenveil index/mods.json lists into a temporary
# client:
cargo test --lib mods -- --ignored
# Joins the swarm of the torrent of a 3.3.5a client (a magnet link, or a
# .torrent's address or file), downloads its Wow.exe alone, reads its
# version, and turns the client down:
OLD_LAUNCHER_TEST_TORRENT='magnet:?xt=…' \
    cargo test --lib download::tests::a_real_torrent -- --ignored --nocapture
# Checks the folder that torrent downloaded into against it, twice, and
# says what a repair would download. The check notes what it found in
# that folder: point it at a folder of links (cp -rs) to keep a client
# as it is.
OLD_LAUNCHER_TEST_TORRENT='magnet:?xt=…' OLD_LAUNCHER_TEST_TORRENT_CLIENT=/tmp/links \
    cargo test --release --lib download::tests::a_real_client_is_checked \
    -- --ignored --nocapture
# Repairs a copy of that client, changed (a file removed, a byte changed:
# cp -a --reflink=auto makes one at no cost on btrfs and XFS), from the
# swarm, through a staging folder, then checks it again:
OLD_LAUNCHER_TEST_TORRENT='magnet:?xt=…' OLD_LAUNCHER_TEST_TORRENT_CLIENT=/tmp/copy \
    cargo test --release --lib download::tests::a_real_client_is_repaired \
    -- --ignored --nocapture
# Moves a download's files from the temporary folder to a folder on
# another disk, which renaming can't do (a tmpfs /tmp and /home will do):
OLD_LAUNCHER_TEST_OTHER_DISK=~/somewhere \
    cargo test -p old_launcher_core torrent::tests::files_move_across_disks -- --ignored
# Copies a Wine prefix, only read, into the temporary folder, moves the
# copy to that other disk, and compares it with the prefix:
OLD_LAUNCHER_TEST_PREFIX="/path/to/World of Warcraft 3.3.5a/.prefix" \
    OLD_LAUNCHER_TEST_OTHER_DISK=~/somewhere \
    cargo test -p old_launcher_core launch::tests::a_real_prefix -- --ignored --nocapture
# Makes a torrent of a client folder, only read, and checks a folder of
# links to it against it, twice:
OLD_LAUNCHER_TEST_CLIENT_FILES="/path/to/World of Warcraft 3.3.5a" \
    cargo test --release -p old_launcher_core torrent::tests::a_real_client \
    -- --ignored --nocapture
# Opens every MPQ archive of a client folder, only reading, and reads each
# listfile and the first and last file it names:
OLD_LAUNCHER_TEST_CLIENT="/path/to/World of Warcraft 3.3.5a" \
    cargo test --release -p old_launcher_core mpq::tests::every_archive \
    -- --ignored --nocapture
# Prints the order the game loads a client's archives in, reads their
# facts and builds the graph, only reading (/usr/bin/time -v shows the
# memory), with OLD_LAUNCHER_TEST_LOADED optionally the number of archives
# it should load:
OLD_LAUNCHER_TEST_CLIENT="/path/to/World of Warcraft 3.3.5a" \
    cargo test --release -p old_launcher_core patches::graph::tests::a_real_client \
    -- --ignored --nocapture
# Looks at a client's patches the way the launcher does, twice, with a cache
# in a temporary folder (OLD_LAUNCHER_TEST_CACHE keeps it, to time a start
# on its own), and prints the times and the memory, only reading:
OLD_LAUNCHER_TEST_CLIENT="/path/to/World of Warcraft 3.3.5a" \
    cargo test --release --lib patches::tests::a_real_clients_patches \
    -- --ignored --nocapture
# Prints what the file integrity check says of a client's patches without a
# torrent (twins, needs a backup would meet), only reading:
OLD_LAUNCHER_TEST_CLIENT="/path/to/World of Warcraft 3.3.5a" \
    cargo test --release --lib patches::tests::a_real_clients_check \
    -- --ignored --nocapture
# Downloads Lau's Patch Y and prints what installing it would do to a
# client's patches, with the list's rules and with the ones that crashed
# Dalaran; the folder is only read:
OLD_LAUNCHER_TEST_CLIENT="/path/to/World of Warcraft 3.3.5a" \
    cargo test --release --lib clients::mods::tests::the_gate_on_a_real_client \
    -- --ignored --nocapture
# Renders the README's screenshots from a demo client:
SCREENSHOT_CLIENT=/tmp/demo SCREENSHOT_OUT=/tmp/shots \
    cargo test --lib screenshots -- --ignored
```

The committed screenshots are rendered inside a container, so the demo
client sits at `/home/player/Games/World of Warcraft 3.3.5a`, its Wine
prefix under `/home/player`, and the text is drawn with the bundled fonts
alone. Render into an empty folder: the test only writes an image that
isn't there.

```sh
podman run --rm -v "$PWD:/src:ro" -v old_launcher-target:/target \
    -v ~/.cargo/registry:/usr/local/cargo/registry -v ~/.cargo/git:/usr/local/cargo/git \
    -v /tmp/shots:/out -w /src -e CARGO_TARGET_DIR=/target -e ICED_TEST_BACKEND=tiny-skia \
    -e HOME=/home/player \
    -e "SCREENSHOT_CLIENT=/home/player/Games/World of Warcraft 3.3.5a" -e SCREENSHOT_OUT=/out \
    docker.io/library/rust:1.98.1 cargo test --locked --lib screenshots -- --ignored
```

## CI and releasing

Every push is checked (fmt, clippy, tests on Linux and under Wine for
Windows). A release is a push to `main` that changes the version in
`Cargo.toml`, with the screenshots rendered again: its pipeline builds the
binaries and publishes them with the project page and the `release.json`
the launchers check for a newer version, in one Pages deployment that
stays until the next release. If that pipeline fails, push the fix and run
a pipeline on `main` by hand (Build > Pipelines > Run pipeline): a version
not published yet is released then. The `aur` job publishes
`aur/PKGBUILD` as `old-launcher-git` on every green pipeline of `main`.
