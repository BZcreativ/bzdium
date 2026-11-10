#!/usr/bin/env bash
# Build bzdium's Linux artifacts (deb, rpm, AppImage, tarballs) inside the
# `bzdium-deb12` WSL distro so every artifact is linked against Debian 12's
# glibc 2.36 -- the oldest baseline we support (Tauri v2 needs webkit2gtk-4.1,
# which rules out Debian 11). Anything built in the Ubuntu 26.04 WSL distro
# requires GLIBC_2.38+ and dies on Debian 12 / Ubuntu 22.04-24.04.
#
# Run from Git Bash on the dev machine (MSYS_NO_PATHCONV stops Git Bash from
# rewriting the /mnt/e path before wsl.exe sees it; the distro's default user
# is root):
#
#   MSYS_NO_PATHCONV=1 wsl.exe -d bzdium-deb12 -u root -e bash \
#     /mnt/e/dev/bzdium/build-helpers/build-linux-deb12.sh \
#     /mnt/e/dev/bzdium/dist-release-0.2.17
#
# The only argument is the staging directory for the artifacts (created if
# missing). Version numbers are read from src-tauri/tauri.conf.json, so run
# this after the release version bump, like the checklist's WSL-Ubuntu step.

set -euo pipefail

STAGE_DIR="${1:?usage: build-linux-deb12.sh <staging-dir>}"
REPO=/mnt/e/dev/bzdium
SRC="$REPO/src-tauri"
TARGET_DIR=/root/bzdium-target

export PATH="$HOME/.cargo/bin:/usr/local/bin:$PATH"
export CARGO_TARGET_DIR="$TARGET_DIR"
# No FUSE inside WSL: make AppImage tooling extract-and-run.
export APPIMAGE_EXTRACT_AND_RUN=1

cd "$SRC"
VERSION=$(grep -oP '(?<="version": ")[^"]+' tauri.conf.json | head -1)
echo ">>> building bzdium v$VERSION (Debian $(cat /etc/debian_version), $(ldd --version | head -1 | grep -oP '2\.\d+')))"

cargo tauri build --bundles deb,rpm,appimage

mkdir -p "$STAGE_DIR"
BUNDLE_DIR="$TARGET_DIR/release/bundle"
cp "$BUNDLE_DIR/deb/bzdium_${VERSION}_amd64.deb" \
   "$BUNDLE_DIR/rpm/bzdium-${VERSION}-1.x86_64.rpm" \
   "$BUNDLE_DIR/appimage/bzdium_${VERSION}_amd64.AppImage" \
   "$STAGE_DIR/"
tar -czf "$STAGE_DIR/bzdium-${VERSION}-linux-x64.tar.gz" \
    -C "$TARGET_DIR/release" bzdium
cp "$STAGE_DIR/bzdium-${VERSION}-linux-x64.tar.gz" \
   "$STAGE_DIR/bzdium-${VERSION}-linux-x64.tgz"

# ---------------------------------------------------------------------------
# Compatibility gate: nothing we ship may require symbol versions newer than
# what a stock Debian 12 offers (GLIBC 2.36, GLIBCXX 3.4.30, CXXABI 1.3.13).
# ---------------------------------------------------------------------------
check_max() { # $1=file $2=prefix $3=allowed-max (versions, prefix stripped)
    local max highest
    max=$(objdump -T "$1" 2>/dev/null | grep -oE "${2}_[0-9]+(\.[0-9]+)*" \
          | sed "s/^${2}_//" | sort -uV | tail -1)
    [ -n "$max" ] || return 0          # no requirement of this symbol class
    GMAX[$2]=$(printf '%s\n' "${GMAX[$2]}" "$max" | grep -v '^$' | sort -V | tail -1)
    highest=$(printf '%s\n' "$3" "$max" | sort -V | tail -1)
    if [ "$highest" = "$3" ]; then     # max <= allowed
        echo "ok: $(basename "$1") max ${2} = $max"
    else
        echo "FAIL: $1 needs ${2}_$max > $3"
        return 1
    fi
}

FAILED=0
declare -A GMAX=([GLIBC]= [GLIBCXX]= [CXXABI]=)
echo ">>> artifact compatibility check"
check_max "$TARGET_DIR/release/bzdium" GLIBC 2.36 || FAILED=1

EXTRACT=$(mktemp -d)
cd "$EXTRACT"
"$STAGE_DIR/bzdium_${VERSION}_amd64.AppImage" --appimage-extract >/dev/null 2>&1
while IFS= read -r -d '' lib; do
    check_max "$lib" GLIBC 2.36 || FAILED=1
    check_max "$lib" GLIBCXX 3.4.30 || FAILED=1
    check_max "$lib" CXXABI 1.3.13 || FAILED=1
done < <(find squashfs-root -name '*.so*' -print0)
cd /
rm -rf "$EXTRACT"

if [ "$FAILED" -ne 0 ]; then
    echo ">>> COMPATIBILITY GATE FAILED -- do not publish these artifacts"
    exit 1
fi
echo ">>> compatibility gate PASSED (highest symbols seen vs allowed):"
echo "    GLIBC ${GMAX[GLIBC]:-none} <= 2.36, GLIBCXX ${GMAX[GLIBCXX]:-none} <= 3.4.30, CXXABI ${GMAX[CXXABI]:-none} <= 1.3.13"
echo ">>> all Linux artifacts are Debian 12 (glibc 2.36) compatible:"
ls -la "$STAGE_DIR"
