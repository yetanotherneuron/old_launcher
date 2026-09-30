# Downloading a client through BitTorrent

Status: assessed, not planned in detail. Written on 2026-09-30 against
commit b1ebe73 (version 0.5.0). Names refer to the code as it was then.

## The ask

A community modifies the 3.3.5a client with modern character models,
assets, effects and sound, and distributes it through a torrent announced
on Discord. Offer, under Manage clients, to download that client through
the torrent into a folder, add it as a client, and offer its updates.

## Open questions, to answer before planning

- What the torrent holds: a complete client with `Wow.exe`, or patch MPQs
  meant for an existing 3.3.5a folder. The second case is a different
  feature, closer to installing files into `Data`.
- How updates are published: a new complete torrent, patch torrents, or
  something else. That decides whether the update flow below works.
- Whether the torrent is public, with trackers and DHT, or private.
- Its size, for the free-space check.

## Decisions taken in the assessment

- **Keep the community's link out of the repository.** The client is
  Blizzard's copyrighted game data with modifications. A GitLab
  repository, a Pages site and an AUR package that ship a magnet link and
  a downloader for it are exposed to a takedown, and every user seeding it
  exposes their address in the swarm. The link comes from the user: paste
  a magnet or drop a `.torrent` file under Manage clients, choose a folder.
  The launcher stays a generic "install a client from a torrent" tool.
- **The list of published versions lives with the community.** Discord is
  not machine-readable, so someone maintains a small JSON listing each
  client's name, version, magnet link and size. The user enters its
  address once; the launcher fetches it like the catalog, with ETag
  caching, and compares it with what each client folder came from.
- **Stop seeding when the download completes, by default**, with an opt-in
  to keep seeding. librqbit's `disable-upload` build feature would make
  every user a leech, which swarms punish.

## What it takes

1. **A torrent engine.** `librqbit`, the library behind the rqbit client:
   pure Rust on tokio, builds for Windows, Linux and macOS. Its `rust-tls`
   feature uses rustls and ring, which the launcher already builds, so the
   mingw and zig cross-builds should stay as they are. Use it with default
   features off, so the HTTP API, web UI and UPnP server stay out.
   `AddTorrentOptions` has what is needed: `overwrite` to resume into an
   existing folder, `only_files` to skip files, `paused`, `initial_peers`
   for a localhost test swarm, `ratelimits`, `list_only`. Session
   persistence resumes without rehashing. It is a heavy dependency: DHT,
   trackers, the peer protocol and its own reqwest come along; expect a few
   megabytes more per binary.

2. **A downloads state in the app**, like `game::State` in `src/game.rs`:
   one librqbit session for the launcher, the torrents in flight, a look at
   their stats every second for the dialog. The progress plumbing follows
   `install` in `src/addons.rs`: a sipper reporting stages, a cancel flag, a
   finished message. Downloads run only while the launcher is open; the
   session's persistence resumes them on the next start.

3. **The clients dialog** (`src/clients.rs`). A "Download a client…" entry
   beside the Add button, then a destination picker and a free-space check
   (sysinfo's `disk` feature, or a small crate). Each download is a row
   with progress, speed, pause and cancel. On completion the folder goes
   through `Installation::open` and the same `Instruction::Added` path the
   setup form uses in `src/lib.rs`, so it lands in the config, is selected
   and scanned. Play stays disabled until then.

4. **Updates.** A torrent is immutable, so an update is a new torrent added
   onto the same folder: the engine verifies the pieces on disk and fetches
   only the changed ones, as long as the new torrent has the same files at
   the same offsets. A marker in the client root, like the addon markers in
   `core/src/marker.rs`, records which client and version the folder came
   from; the dialog compares it with the newest published version.
   Protected on an update, through `only_files`: `realmlist.wtf`,
   `WTF/`, `Interface/AddOns/`, `Screenshots/`, `Logs/`. The launcher never
   deletes files a new torrent dropped.

5. **Tests.** Two librqbit sessions on localhost, one seeding a temporary
   folder, one downloading it through `initial_peers`, cover the engine
   wrapper. The catalog tests already run a local TCP server under Wine in
   CI, so sockets there are proven. The dialog is tested with the
   simulator like the rest.

Altogether the largest feature since the catalog: about the size of the
addons screen and its install pipeline together.
