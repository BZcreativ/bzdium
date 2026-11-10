#!/usr/bin/env bash
# One-time setup of the `bzdium-deb12` WSL distro used to build bzdium's
# Linux artifacts. Debian 12 pins the toolchain to glibc 2.36, the oldest
# baseline we support: artifacts built here run on Debian 12 and every newer
# distro. The day-to-day build script is build-linux-deb12.sh.
#
# Run from Git Bash on the dev machine (MSYS_NO_PATHCONV=1 for any wsl.exe
# call that passes /mnt/... paths through). Requires ~5 GB free on E:.
# To start over:  wsl.exe --unregister bzdium-deb12  then re-run.
#
# Why not the Ubuntu WSL distro? It is Ubuntu 26.04 (glibc 2.43); anything
# linked there needs GLIBC_2.38+ symbols and dies on Debian 12 with
# "version `GLIBC_2.38' not found" (v0.2.17 regression, reported by a user
# on Debian 12 / glibc 2.36).

set -euo pipefail

DISTRO=bzdium-deb12
INSTALL_ROOT=/e/wsl/$DISTRO
DOWNLOAD_DIR=$INSTALL_ROOT-download

if wsl.exe -l -q 2>/dev/null | tr -d '\0' | grep -qx "$DISTRO"; then
    echo ">>> $DISTRO already registered; verifying toolchain"
    wsl.exe -d "$DISTRO" -u root -e bash -c \
        "cat /etc/debian_version; ldd --version | head -1; \
         \$HOME/.cargo/bin/rustc --version; cargo tauri --version"
    exit 0
fi

mkdir -p "$INSTALL_ROOT" "$DOWNLOAD_DIR"
cd "$DOWNLOAD_DIR"

# Newest daily Debian 12 (bookworm) rootfs from the Linux Containers image
# server -- official Debian bits, rebuilt daily, no Docker needed.
LISTING=$(curl -sL https://images.linuxcontainers.org/images/debian/bookworm/amd64/default/)
BUILD=$(grep -oE 'href="[0-9]{8}_[0-9:]+/"' <<<"$LISTING" | tail -1 \
        | grep -oE '[0-9]{8}_[0-9:]+')
[ -n "$BUILD" ] || { echo "could not find a bookworm build in the listing"; exit 1; }
BASE="https://images.linuxcontainers.org/images/debian/bookworm/amd64/default/${BUILD//:/%3A}"
echo ">>> downloading bookworm rootfs $BUILD"
curl -sL -O "$BASE/rootfs.tar.xz" -O "$BASE/SHA256SUMS"
grep ' rootfs.tar.xz$' SHA256SUMS | sha256sum -c -

xz -d rootfs.tar.xz
echo ">>> importing into WSL as $DISTRO"
wsl.exe --import "$DISTRO" "$(cygpath -w "$INSTALL_ROOT")" \
        "$(cygpath -w "$DOWNLOAD_DIR/rootfs.tar")" --version 2

wsl.exe -d "$DISTRO" -u root -e bash -c '
    set -e
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq libwebkit2gtk-4.1-dev build-essential curl wget \
        file pkg-config patchelf xz-utils git libxdo-dev libssl-dev \
        libayatana-appindicator3-dev librsvg2-dev

    cd /tmp
    curl -sL -o rustup-init https://static.rust-lang.org/rustup/dist/x86_64-unknown-linux-gnu/rustup-init
    chmod +x rustup-init
    ./rustup-init -y --default-toolchain 1.98.0 --profile minimal

    curl -sL -o ct.tgz https://github.com/tauri-apps/tauri/releases/download/tauri-cli-v2.12.3/cargo-tauri-x86_64-unknown-linux-gnu.tgz
    tar xzf ct.tgz && cp cargo-tauri /usr/local/bin/

    echo "=== $DISTRO ready ==="
    cat /etc/debian_version
    ldd --version | head -1
    $HOME/.cargo/bin/rustc --version
    cargo tauri --version
' | tr -d "\0"

rm -f "$DOWNLOAD_DIR/rootfs.tar"
