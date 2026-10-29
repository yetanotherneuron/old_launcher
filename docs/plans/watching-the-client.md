# Watching the client's folder

Status: an idea, not started (2026-10-30).

Once the launcher has started, it could watch the client's folder, and get
told when a file in it changes: inotify on Linux, `ReadDirectoryChangesW`
on Windows, FSEvents on macOS. A crate covering all three is the kind of
complex problem a dependency is worth taking for. Today the launcher reads
the folder again when asked to: on F5, after its own work, and when the
window regains the focus. The patches' scan compares the `Data` listings,
and the mods' look compares each file's size and modification time (see
`mpq-dependencies.md`). With a watcher, a change made while the launcher
runs would be known as it happens. Only what changed would be read again:
one archive's facts, one mod's files, one addon's folder, the realmlist.

Startup still needs the stamps, since a watcher can't see what changed
while the launcher was closed. Watchers drop events too: Windows' buffer
overflows, a network or Wine-mapped drive may send nothing, and Linux
limits how many folders can be watched. So a dropped or overflowed watch
falls back to reading again, the way regaining the focus does now. Windows
is the environment to design for: names compared case-insensitively, and a
rename seen as one file leaving and another arriving.
