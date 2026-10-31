# Controlling the music player from the game

Status: a plan, not decided on (2026-10-31). Would fold the WoW Music
Bridge tray app (https://gitlab.com/juxuanu/wow-music-bridge, AGPL-3.0, Go)
into the launcher, so a player needs no second program running.

## What the bridge is

The game's addons can't talk to the system, so the bridge speaks to its
companion addon through files in the addon's folder, which the addon reads
with `Texture:SetTexture`: a valid 8×8 TGA loads (a 1 bit), a 1×1 TGA
doesn't (a 0 bit). The game indexes loose files once, at start, so the file
names never change; only their contents do.

- App → addon: `state/<slot>/b0000.tga…` are the bits of one message each
  (`now` 16 bytes: status, cover counter, volume, repeat, shuffle, speed,
  position, length, abilities; `track` 219: title, artist, album; `players`
  179: the players and which is shown), each as a length byte, the payload
  and a Fletcher-16 checksum, with a 4-bit version in `v0..v3.tga` bumped
  only once the message is fully written. `hb.tga` flips every second: the
  heartbeat the addon calls the bridge offline without. `one.tga`,
  `zero.tga` (also the hard-link templates for writes) and `files4.tga`
  (the marker telling the addon the game saw this install's files).
- Addon → app: the addon loads `cmd/<command>_<0|1>.blp` (alternating, so
  two presses in a row are two events); the app notices the open with
  inotify (`IN_OPEN` on the folder) on Linux and an exclusive oplock per
  file on Windows. 90 commands: toggle, stop, next, prev, volup, voldown,
  mute, shuffle, loop0-2, raise, quit, fullscreen, auto, rate{10,15,20,25,
  30,40}, vol00-20, seek00-39, player0-7.
- Cover art: `art.tga` rewritten whole (128×128, centre-cropped), the `now`
  message's counter telling the addon to reload it; `note.tga` when none.
- Players: MPRIS over the session D-Bus on Linux (every `org.mpris.
  MediaPlayer2.*` but playerctld; follows whatever plays unless one is
  pinned; mute remembers the volume, since MPRIS has none); the system media
  sessions (SMTC, `Windows.Media.Control`) on Windows, which can't set the
  volume nor reach the window: those stay disabled in the addon.

The addon folder holds about 3,600 files, which `Wow.exe` walks at start
and quit like every other loose file (see `client-folder-is-walked`).

## Where it lives in the launcher

In the wrapper the game already runs under for Discord (`src/discord.rs`):
the launcher's own binary, holding the game as its child and exiting with
its code. The bridge needs to run exactly while the game does, whether or
not the launcher stays open, and that is what the wrapper is; no daemon,
no tray. The wrapper generalizes from `--discord <command>` to options
before the command, say `--discord --music <client folder> -- <command>`,
each feature a task raced against the child's exit as `follow` does now.

The game started from Steam or Lutris, or found already running, gets no
bridge, as it gets no Discord status: accepted there, accepted here.

Install happens in the launcher, never in the wrapper: the game isn't
running when Play writes, so no half-written file is seen, and the
command watcher (which would take the install's own file opens for button
presses) runs only in the wrapper. The addon's source (`WoWMusicBridge.toc`
and `.lua`) is embedded with `include_bytes!`, versioned with the launcher:
the protocol binds the two sides byte for byte (slot sizes, flags, the
files marker), so they ship together. On Play with the bridge on, the
launcher rewrites the addon files when they differ from the embedded ones,
before starting the game, the way it moves an old prefix out first: the
"restart WoW to finish installing" state never arises.

Alternative weighed: the addon as a catalog entry from the bridge's repo,
with the state files inside the archive, and the launcher only writing
bits. Rejected for now: the protocol is this addon's, not generic, and the
marker's record of installed files would flag every bit as changed.

## Modules

All under `src/music/`, mirroring the Go files, plus the addon's sources
under `assets/music/`:

- `channel.rs`: TGA and BLP generation, Fletcher-16, the three encoders,
  the file layout, `install` (write every file, drop strays, a tempdir
  test), `publish` (write bits by hard link to `one`/`zero`, falling back
  to a write; rename over the old; bump the version) and `heartbeat`. Pure
  functions with the Go code's test values as fixtures: the Lua side never
  changes, so the bytes must not either.
- `player.rs`: the `Media` trait of the Go code (`state`, `command`,
  `select`, `cover`, `changed`), `Snapshot`, `Track`, `Player`, `stepVolume`.
- `mpris.rs` (unix): zbus. Already compiled into the launcher through
  iced's `mundy`, with its `async-io` features: use it as is, no `tokio`
  feature, which would switch mundy's zbus too and risk a panic outside a
  runtime. zbus's internal executor runs its own thread, so its futures
  await fine under the wrapper's tokio runtime. Proxies for
  `org.mpris.MediaPlayer2` and `.Player`, `ListNames`, `NameOwnerChanged`
  and `PropertiesChanged`/`Seeked` signals for `changed`.
- `smtc.rs` (windows): the `windows` crate, already in the Windows build
  through wgpu; add `Media`, `Media_Control`, `Foundation`,
  `Foundation_Collections`, `Storage_Streams`. One thread, `RoInitialize`
  MTA, a `std::sync::mpsc` of closures like the Go `requests` channel; the
  crate's `.get()` on async operations replaces the hand-rolled vtable
  waits; `TypedEventHandler::new` for the change events; the thumbnail
  through `OpenReadAsync` and `DataReader`. When the API is missing (Wine,
  under CI), the game runs with the bridge offline.
- `watch.rs`: inotify through `libc` (already in the tree) on an
  `AsyncFd`, or the `inotify` crate; the oplock loop with `windows-sys`
  (`Win32_System_Ioctl`, `Win32_System_IO`, `Win32_Storage_FileSystem`:
  `FSCTL_REQUEST_OPLOCK_LEVEL_1`, overlapped, `CancelIoEx`). Both yield
  the file names opened. Wine grants no oplocks: that test is `#[ignore]`
  there, the watcher logs once and keeps retrying, as the Go code does.
- `art.rs`: `reqwest` (already there) for `http(s)` covers, `fs` for
  `file`, the player's own stream otherwise; decoding with `image`
  (`default-features = false`, `png`, `jpeg`, `webp`, `gif`: all pure
  Rust), centre crop and resize to 128, TGA out.
- `mod.rs`: the Go `app.loop` as a tokio task: a tick a second and on
  `changed` (coalesced at 30 ms), the `publish` order (`track`, `players`,
  then `now`, so a change is complete when the addon reads `now`),
  `nowChanged`'s 2 s drift rule, the art request/result pair, commands
  resolved against the last snapshot, a second tick 300 ms after a track
  command.

Launcher side:

- `core`: `Installation::music_addon()` → `addons_directory().join(
  "WoWMusicBridge")`, case-insensitive like the rest; nothing else, the
  protocol stays in `src/`.
- `src/game.rs`: `play` takes `music: bool` beside `discord`; with it on,
  the Play task refreshes the addon (blocking, before the prefix move) and
  the wrapper gets `--music <root>`.
- `src/settings.rs`: a "Music" section like Discord's: one checkbox,
  "Control my music player from the game", with a line saying what it
  does: "The launcher adds a small addon to the client when you press Play,
  and the game shows and controls what plays on this computer." Saved as
  `Config::music`, off by default, left out of the file while off.
- Addons tab: the folder shows up like any addon installed by hand. The
  scan knows the launcher's own folder by name and labels it "Comes with
  the launcher" instead of offering to recognize it; Remove works as for
  any folder, and leaves the setting on (Play puts it back).
- The addon's own UI strings stay the addon's; the launcher's never say
  "bridge", "slot", "bit" or "heartbeat".

## Dependencies

New: `image` (pure Rust decoders) and maybe `inotify`. Already compiled,
now direct: `zbus` (unix), `windows` with the Media and Storage features
(windows), `windows-sys` features for the oplock. No C toolchain, no new
system libraries: zbus's D-Bus is pure Rust, and the Windows APIs are the
system's.

## Tests

- `channel`: every encoder against bytes computed by the Go code (record
  them once), the TGA and BLP headers byte for byte, install into a
  tempdir (file count, strays removed, reinstall keeps the version files),
  publish then read back through a Rust port of the addon's `readByte` and
  `payload` checks.
- `player`: `stepVolume` edge cases (0.15 up and down, the ends).
- `mpris`: a fake player on a private bus (`zbus` can serve one) answering
  `GetAll`, `PlayPause`, `SetPosition`; the follow-what-plays rule.
- `watch`: inotify on a tempdir, a file opened → its name; `#[ignore]`d
  oplock test run only on real Windows.
- The wrapper: a fake command that touches `cmd/toggle_0.blp` and exits,
  with the fake player, ends with the player paused.
- Simulator tests for the Settings checkbox, and `clients`/`game` tests
  that the wrapper's arguments carry `--music`.
- Windows tests under Wine before the release, as always; the SMTC and
  oplock paths need a real Windows run once, by hand.

## Order

1. `channel.rs` with its tests and the embedded addon: the bytes first.
2. `mpris.rs` and `watch.rs` for Linux; the wrapper task; run it against
   the test client's folder with a real player, no game, then with the
   game.
3. Settings, Play wiring, the Addons tab label.
4. `smtc.rs` and the oplock watcher; one run on a Windows machine.
5. README line, DEVELOPMENT.md paragraph on the wrapper, release.

Open: whether the Go app is retired, kept for people without the launcher,
or made to install the same addon files so the two stay interchangeable.
