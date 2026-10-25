#!/bin/sh
# Describes what a release publishes, for the launcher's update check: the
# version given, and the address on the page given (the binaries are published
# beside it), checksum and size of each binary in the working directory.
# Prints release.json.
set -eu

version=$1
page=$2

# Each binary is keyed by its platform as Rust names it (std::env::consts::OS
# and ARCH), so a launcher finds its own.
binary() {
    printf '    "%s": {\n      "url": "%s%s",\n      "sha256": "%s",\n      "size": %s\n    }' \
        "$1" "$page" "$2" "$(sha256sum "$2" | cut -d ' ' -f 1)" "$(stat -c %s "$2")"
}

printf '{\n  "version": "%s",\n  "binaries": {\n' "$version"
binary windows-x86_64 old_launcher.exe
printf ',\n'
binary linux-x86_64 old_launcher-linux-x86_64
printf ',\n'
binary linux-aarch64 old_launcher-linux-arm64
printf '\n  }\n}\n'
