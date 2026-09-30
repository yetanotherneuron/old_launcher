# Vanilla and TBC clients

Status: planned, not started. Written on 2026-09-30 against commit b1ebe73
(version 0.5.0). Names refer to the code as it was then.

## Goal

Open World of Warcraft 1.12 (Vanilla) and 2.4.3 (The Burning Crusade)
clients beside 3.3.5a ones, each with a catalog of its own, without breaking
the launchers already out there.

## What is Wrath-specific today

Only four things in the workspace assume 3.3.5a:

- **The version gate.** `Version::is_wotlk` in `core/src/version.rs` accepts
  only 3.3.5, and `Installation::open` in `core/src/installation.rs` rejects
  everything else with `Error::UnsupportedVersion`, whose message says "only
  3.3.5a is".
- **The realmlist location.** `Realmlist::of` in `core/src/installation.rs`
  looks in `Data/<locale>/realmlist.wtf`. Vanilla and TBC keep
  `realmlist.wtf` next to the executable; it moved into the locale folder
  with 3.0.
- **The interface number 30300.** `Toc::WOTLK_INTERFACE` in
  `core/src/toc.rs`, used by `toc::made_for` (which names the game version
  an interface number belongs to, and answers nothing for 30300), by the
  "Made for …" badge hint `made_for_hint` in `src/addons/card.rs`, and by the
  indexer's filter in `indexer/src/main.rs` (listings with `require_wotlk`
  are dropped unless a folder declares 30300).
- **One catalog.** `PUBLISHED` in `src/catalog.rs` points at
  `index/wotlk.json` in the repository, and the cache is `wotlk.json` plus
  `wotlk.etag` under the cache directory. The indexer writes
  `index/wotlk.json` by default and the `index` CI job commits that file.

Everything else already fits the three clients. Verified while reading:

- `Interface/AddOns`, `WTF/Account/<account>/SavedVariables` and the
  per-character folders, the `.toc` format, `Blizzard_*` skipping, markers,
  adoption, the library and the export layout are the same in 1.12, 2.4.3
  and 3.3.5.
- The executable lookup, `child(&root, "Wow.exe")` in
  `core/src/installation.rs`, ignores case, so the older clients' `WoW.exe`
  is found. `launch::running` compares process names ignoring case, and
  `launch::folder` lowercases the command line, so the Play button follows
  those games too.
- `Data/<locale>` folders and `SET locale` in `WTF/Config.wtf` exist in all
  three, so `Installation::locale` and `Details::locales` keep working.
- `Catalog` derives no `deny_unknown_fields`, so a new field in the catalog
  does not break 0.5.0 launchers.

## Decisions

- **A type named `Expansion`** in `core/src/version.rs`, beside `Version`:
  `Vanilla`, `BurningCrusade`, `Wrath`. It knows its interface number
  (11200, 20400, 30300), its display name ("Vanilla", "The Burning Crusade",
  "Wrath of the Lich King"), its file id (`vanilla`, `tbc`, `wotlk`) and
  where its realmlist lives. Not `Game`: the app already has a `game` module
  about the running process.
- **Which versions count.** Any 1.12.x is Vanilla: 1.12.2 exists and shares
  the 11200 interface. TBC is exactly 2.4.3. Wrath stays exactly 3.3.5.
  Turtle WoW's modified client reports 1.17 and stays out of scope.
- **One catalog file per expansion**: `index/vanilla.json`, `index/tbc.json`
  and `index/wotlk.json`. The Wrath file keeps its name and URL, so 0.5.0
  launchers keep working. `Catalog` gains an `expansion` field with a serde
  default of Wrath; no schema version bump.
- **The app shows the catalog of the selected client's expansion.** Without
  an open client it shows the previous one, and Wrath on a fresh start. No
  expansion picker: the clients decide.
- **`OLD_LAUNCHER_INDEX` overrides whatever expansion is shown.** It is a
  developer setting.
- **Empty catalogs for Vanilla and TBC until they have sources**, written by
  hand. The indexer keeps refusing to write an empty catalog.
- **Importing an export made on another expansion warns** in the import
  dialog rather than refusing.

## Steps

Steps 1 and 2 change nothing users see. Steps 3 and 4 make the feature
visible. All four go out in one push, as 0.6.0. Each step passes
`cargo clippy --workspace --all-targets` and `cargo test --workspace`
locally, and the Windows target through CI.

### 1. `Expansion` in core

- `core/src/version.rs`: the enum, with `interface()`, `name()`, `id()` and
  the realmlist rule. `Version::expansion() -> Option<Expansion>` replaces
  `is_wotlk`.
- `core/src/installation.rs`: `Installation` stores the expansion and
  exposes `expansion()`; `open` rejects versions without one.
  `Realmlist::of(root, expansion)`: the client root for Vanilla and TBC,
  `Data/<locale>` for Wrath. `Details::read` reads the executable's version
  itself and stores `expansion: Option<Expansion>` in `Details`, so the
  clients dialog keeps working on folders that never opened, and the focus
  re-read in `src/clients.rs` (`Message::Focused`) passes it on.
- `core/src/error.rs`: `UnsupportedVersion` names the three supported
  versions.
- `core/src/toc.rs`: `WOTLK_INTERFACE` goes. `made_for(interface,
  expansion)` answers nothing for the client's own interface, names 3.3 as
  "Wrath of the Lich King" and 2.4 or 1.12 as their expansion when the
  client is another one, and keeps naming 3.0 to 3.2, 2.0 to 2.3 and 1.0 to
  1.11 by patch. `Toc::targets_other_client` goes: only its own tests call
  it.
- `core/src/testing.rs`: `VANILLA` (1.12.1.5875) and `TBC` (2.4.3.8606)
  beside `WOTLK`, and a client builder that takes the expansion.
- App call sites the compiler points at: `made_for` and `made_for_hint` in
  `src/addons/card.rs` take `Expansion::Wrath` for now; `Realmlist::of` in
  `src/clients.rs`.
- Tests: version to expansion mapping; a Vanilla client with `WoW.EXE` opens
  (the existing `other_client_versions_are_rejected` flips into this);
  realmlist at the root for Vanilla and TBC and in `Data/<locale>` for
  Wrath; `made_for` per expansion; `Details` carries the expansion.

### 2. One catalog per expansion

- `core/src/catalog.rs`: the `expansion` field, defaulting to Wrath.
- `indexer/src/main.rs`: an `--expansion` flag (default `wotlk`); `--output`
  defaults to `index/<id>.json`; the source list is built per expansion (the
  current four sources are Wrath's); the empty-result refusal stays.
- `indexer/src/source.rs`: `require_wotlk` becomes `require_interface`,
  checked against the expansion's number.
- `index/vanilla.json` and `index/tbc.json`: hand-written, with the field,
  no sources, no entries.
- `.gitlab-ci.yml`, job `index`: run the indexer once per expansion and
  `git add` the three files.
- `src/catalog.rs`: the `repository()` test helper takes the expansion; the
  `the_repository_catalog_parses` test covers the three files and checks
  the field matches the file name.
- Tests: indexer filter per expansion; catalogs without the field parse as
  Wrath.

### 3. The app follows the selected client

- `src/catalog.rs`: `PUBLISHED` and the cache names become functions of the
  expansion (`<id>.json`, `<id>.etag`; the Wrath names are unchanged, so
  existing caches carry over). `Location::configured(expansion)` and
  `is_published` compare against that expansion's URL.
- `src/lib.rs`: the fields `catalog`, `refresh`, `refreshed` and `tag` become
  one record per expansion in a map, loaded lazily from the cache, plus the
  expansion shown. `Message::CatalogLoaded` carries the expansion so a late
  reply lands in the right slot. The `Opened` arm switches the shown
  expansion and refreshes when it changed. `refresh_catalog`, `propose` and
  every `&self.catalog` use the shown one.
- `src/addons.rs` and `src/addons/card.rs`: `view` and `card` get the
  expansion for the badge hint. The empty-list text for `Status::All`
  distinguishes "not fetched yet" from "fetched, and it has nothing for
  Vanilla yet", which needs the record to tell a missing catalog from an
  empty one.
- Tests: opening clients of two expansions swaps the catalog; a stale
  `CatalogLoaded` for another expansion does not replace the shown one; the
  empty text per case.

### 4. Texts, docs, screenshots, version

- `src/setup.rs`: title "Add a WoW client", the folder picker title, the
  explanation naming the three versions, the log lines.
- `src/lib.rs`: the header badge shows the expansion name before the version
  badge; the "Opened" log line says the expansion.
- `src/clients.rs`: an "Expansion" fact beside "Version".
- `core/src/transfer.rs`: `Manifest` records the expansion, defaulting to
  Wrath for old exports; `src/transfer.rs` shows a warning line when it
  differs from the client's.
- `README.md` (the intro, the "Several clients" line, the catalog section,
  the layout section), `assets/old_launcher.desktop` (Comment and
  Keywords), `aur/PKGBUILD` (pkgdesc), `pages/head.html` (meta description),
  the crate docs of `core/src/lib.rs` and `src/lib.rs`, `core/src/testing.rs`
  docs.
- Regenerate the README screenshots, since the header changes:
  `SCREENSHOT_CLIENT=/tmp/demo SCREENSHOT_OUT=/tmp/shots cargo test --lib screenshots -- --ignored`,
  then copy the `*-tiny-skia.png` files into `assets/screenshots/` under
  their names.
- Bump the workspace version to 0.6.0.

### 5. Sources for Vanilla and TBC

The user chooses the sources. The `Maintainer` source in
`indexer/src/source/github.rs` works unchanged for any GitHub account,
filtered by the expansion's interface. A curated site or collection needs a
module of its own, like `locus313.rs` or `rising_gods.rs`, implementing the
`Source` trait. Verify each candidate before wiring it in: what it lists,
how its downloads are addressed, whether its archives declare the interface.
Then run the indexer for the expansion and commit its catalog, or let the
schedule do it.

## Open points

- **The real executables.** The fixture fakes the version block. Whether a
  1.12.1 and a 2.4.3 `WoW.exe` carry one that `Version::from_executable`
  reads as 1.12.1.5875 and 2.4.3.8606 needs a real client: after step 1,
  the launcher's "Opened …: version …" log line settles it.
- **Sources** (step 5): none chosen yet.
