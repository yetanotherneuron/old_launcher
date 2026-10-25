# Old Launcher

A launcher and addon manager for World of Warcraft 3.3.5a clients.

**[Download for Linux](https://old-launcher.icarns.xyz/old_launcher-linux-x86_64)**
**[Download for Windows](https://old-launcher.icarns.xyz/old_launcher.exe)**

[Linux on ARM64](https://old-launcher.icarns.xyz/old_launcher-linux-arm64)
· [Arch Linux (AUR)](https://aur.archlinux.org/packages/old-launcher-git)

It's a portable program: nothing to install, just run it.

- **Windows** (64-bit, Windows 10 or newer): run `old_launcher.exe`. It isn't
  signed, so Windows may warn about an unknown publisher: choose *More info*,
  then *Run anyway*.
- **Linux** (64-bit, any distribution from 2018 or later, because of glibc):
  make the file executable (`chmod +x` on it), then run it. Or install it
  from the [AUR](https://aur.archlinux.org/packages/old-launcher-git).

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
- **Switch the language** of a client that has more than one.
- **Show on Discord** that you're playing, if you want.

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
- Addon authors on GitLab: juxuanu (Kekator).
- [WoWInterface](https://www.wowinterface.com), for the addons whose last
  version was made for patch 3.3.
- [Rising Gods](https://addons.rising-gods.de/addons), a server's collection
  (descriptions in German).

When several of them offer the same addon, the newest version is installed;
you can also pick another source instead.

Missing an addon? Ask for it on
[GitLab](https://gitlab.com/juxuanu/old_launcher/-/issues/new?issuable_template=Addon%20request)
or on [Discord](https://discord.gg/tS35paspnf).

## Good to know

- Outside your client folders, the launcher writes only its settings file,
  `~/.config/old_launcher/config.toml` on Linux and
  `%APPDATA%\old_launcher\config.toml` on Windows, and a copy of the addon
  list in `~/.cache/old_launcher` or `%LOCALAPPDATA%\old_launcher`, so the
  list doesn't need to be downloaded every time you open the launcher.
- Every addon folder it installs gets a small `.old_launcher.toml` file
  saying where it came from. The game ignores it; the launcher uses it to
  tell what can be updated.
- Downloads from GitHub, which limits anonymous ones, can sign in with
  `GH_TOKEN`, `GITHUB_TOKEN` or your GitHub CLI login. The launcher never
  stores a token.

Developers: see
[DEVELOPMENT.md](https://gitlab.com/juxuanu/old_launcher/-/blob/main/DEVELOPMENT.md).
