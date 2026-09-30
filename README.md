# Old Launcher

A launcher and addon manager for World of Warcraft 3.3.5a clients, built
with [iced](https://github.com/iced-rs/iced).

**[Download for Linux](https://gitlab.com/juxuanu/old_launcher/-/jobs/artifacts/main/raw/old_launcher-linux-x86_64?job=linux-x86_64)**
**[Download for Windows](https://gitlab.com/juxuanu/old_launcher/-/jobs/artifacts/main/raw/old_launcher.exe?job=windows)**

[Linux on ARM64](https://gitlab.com/juxuanu/old_launcher/-/jobs/artifacts/main/raw/old_launcher-linux-arm64?job=linux-arm64)
· [Arch Linux (AUR)](https://aur.archlinux.org/packages/old-launcher-git)

Each is a single file built from the latest `main`; nothing to install.

- **Linux** (64-bit, any distribution from 2018 on): make the file
  executable, then run it: `chmod +x old_launcher-linux-x86_64` (or
  `old_launcher-linux-arm64`).
- **Windows** (64-bit, Windows 10 or newer): run `old_launcher.exe`. It isn't
  signed, so Windows may warn about an unknown publisher: choose *More info*,
  then *Run anyway*.
- **Arch Linux**: install
  [`old-launcher-git`](https://aur.archlinux.org/packages/old-launcher-git)
  from the AUR, which adds it to your applications menu.

## What it does

![Every addon in one list, with the search box and the filters above it](assets/screenshots/addons.png)

- **Play**: on Linux through Wine, Proton or any runner, set up per client.
- **One list** of every addon, available and installed, to search and filter.
- **Install** or reinstall with one click, from the freshest source or the one you pick.
- **Update** addons that have a newer version available, and the launcher itself.
- **Manage** addons: uninstall, clean saved settings, repair broken folders.
- **Recognizes** the addons you installed before the launcher.
- **Export** addons and settings to a zip; **import** it on another computer.
- **Several clients** side by side (3.3.5a only).

![The installed addons, among them a manual install and a broken folder to repair](assets/screenshots/installed.png)

![A client's details, and how it starts on Linux: Proton through umu-launcher](assets/screenshots/clients.png)

![Importing addons and saved settings exported on another computer](assets/screenshots/import.png)

Shortcuts: <kbd>Ctrl</kbd>+<kbd>F</kbd> search, <kbd>F5</kbd> refresh,
<kbd>Esc</kbd> close dialogs, <kbd>Ctrl</kbd>+<kbd>Q</kbd> quit.

## Where the addons come from

The list of addons is put together from these collections, and refreshed
every week:

- [NoM0Re/WoW-3.3.5a-Addons](https://github.com/NoM0Re/WoW-3.3.5a-Addons),
  a curated collection, together with NoM0Re's own addon repositories.
- [locus313/WoW-3.3.5a-Addons](https://github.com/locus313/WoW-3.3.5a-Addons),
  a curated collection of backports.
- Backport maintainers on GitHub: ElvUI-WotLK, bkader, Bunny67, Zidras,
  s0h2x, KhalGH, Sattva-108, Xurkon.
- [Rising Gods](https://addons.rising-gods.de/addons), a server's collection
  (descriptions in German).

When several of them offer the same addon, the newest version is installed,
and the addon's card lets you pick another source instead. The card links
to its page at every source that has it.

## Good to know

- Outside your client folders, the launcher writes only its settings file,
  `~/.config/old_launcher/config.toml` (the platform's config directory
  elsewhere), and a copy of the addon list in your cache directory, so the
  list shows at once and offline.
- Every addon folder it installs gets a small `.old_launcher.toml` file
  saying where it came from. The game ignores it; the launcher uses it to
  tell what can be updated.

Developers: see
[DEVELOPMENT.md](https://gitlab.com/juxuanu/old_launcher/-/blob/main/DEVELOPMENT.md).
