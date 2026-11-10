# Release checklist

bzdium releases are cut by hand from `main`. This is the order that keeps
the version in `Cargo.toml`, the README download links and the published
assets in agreement. Replace `0.2.15` below with the version being released
(no leading `v`). Start from a clean tree and the pinned toolchain:

    git status --short          # must print nothing
    rustc --version             # must be 1.98.0 (see rust-toolchain.toml)

## 1. Version and release notes

- Add the `- **v0.2.15** (...)` entry at the top of **Release History** in
  `README.md`; newest first, user-visible changes only.
- Bump `version` in `src-tauri/Cargo.toml` and `src-tauri/tauri.conf.json`.
- Run `cargo check --manifest-path src-tauri/Cargo.toml` so `Cargo.lock`
  follows, then commit as `chore: release 0.2.15`.
- Write the published release body to
  `dist-release-0.2.15/RELEASE_NOTES.md` (this is not the README entry).

## 2. Build the artifacts

Every asset name embeds the package version, so build only after the bump.
Collect all seven in `dist-release-0.2.15/`:

| Platform | How | Asset |
|---|---|---|
| Windows installer | `npx @tauri-apps/cli build` | `bzdium_0.2.15_x64-setup.exe` |
| Windows portable | copy `src-tauri/target/release/bzdium.exe` | `bzdium-v0.2.15-x64-portable.exe` |
| Linux deb/rpm/AppImage | `npx @tauri-apps/cli build --bundles deb,rpm,appimage` | `bzdium_0.2.15_amd64.deb`, `bzdium-0.2.15-1.x86_64.rpm`, `bzdium_0.2.15_amd64.AppImage` |
| Linux portable tarball | `tar -czf bzdium-0.2.15-linux-x64.tar.gz -C src-tauri/target/release bzdium` (same bytes as `.tgz`) | `bzdium-0.2.15-linux-x64.tar.gz` / `.tgz` |

The Linux bundles are built inside the **`bzdium-deb12`** WSL distro — a
plain Debian 12 (glibc 2.36) install that pins the artifacts to the oldest
glibc we support. Do **not** build them in the Ubuntu 26.04 WSL distro: its
glibc 2.43 links GLIBC_2.38+ symbol references into the binary and into every
library the AppImage bundles, and the result dies on Debian 12 with
``version `GLIBC_2.38' not found`` (the v0.2.17 regression reported by a user
on Debian 12). The distro carries the pinned 1.98.0 toolchain and
`cargo-tauri` 2.12.3; `build-helpers/setup-debian12-build.sh` recreates it
from scratch if it is ever missing.

    MSYS_NO_PATHCONV=1 wsl.exe -d bzdium-deb12 -u root -e bash \
      /mnt/e/dev/bzdium/build-helpers/build-linux-deb12.sh \
      /mnt/e/dev/bzdium/dist-release-0.2.15

Artifacts land directly in the staging directory given as the argument (the
script also builds both tarballs and refuses to finish if any artifact needs a
symbol newer than Debian 12's glibc 2.36 / GCC 12 libstdc++). The intermediate
cargo cache is `/root/bzdium-target` inside the distro.
The build also emits an MSI; it is not linked from the README and has not
been uploaded since v0.2.10, so leave it out of the release.

Check the directory before continuing: every file name must contain
`0.2.15`. A name that still says the previous version is the failure mode
that the first v0.2.14 README edit shipped.

## 3. Point the README at the new version

    .\build-helpers\update-release-links.ps1 -Version 0.2.15          # dry run
    .\build-helpers\update-release-links.ps1 -Version 0.2.15 -Apply   # write
    cargo test --manifest-path src-tauri\Cargo.toml --locked          # guard

Commit as `docs: point download links at v0.2.15`. The guard test
(`src-tauri/tests/readme_download_links.rs`) fails when a tag and a file
name disagree -- do not bypass it.

## 4. Tag and push

    git tag v0.2.15
    git push github main --follow-tags
    git push forgejo main --follow-tags

Create the tag **after** steps 1-3 so the tagged tree has the correct
links. A tag that exists only locally makes every README download link
answer 404 (that is the v0.2.14 state as of this checklist being written).

## 5. Publish

GitHub (`gh auth login` once):

    gh release create v0.2.15 --repo BZcreativ/bzdium \
      --title "bzdium v0.2.15" \
      --notes-file dist-release-0.2.15/RELEASE_NOTES.md \
      dist-release-0.2.15/bzdium_0.2.15_x64-setup.exe \
      dist-release-0.2.15/bzdium-v0.2.15-x64-portable.exe \
      dist-release-0.2.15/bzdium_0.2.15_amd64.deb \
      dist-release-0.2.15/bzdium-0.2.15-1.x86_64.rpm \
      dist-release-0.2.15/bzdium_0.2.15_amd64.AppImage \
      dist-release-0.2.15/bzdium-0.2.15-linux-x64.tar.gz \
      dist-release-0.2.15/bzdium-0.2.15-linux-x64.tgz

Mirror the tag, body and assets on Forgejo; the README links both hosts.

## 6. Verify the published links

    gh release view v0.2.15 --repo BZcreativ/bzdium --json assets --jq '.assets[].name'
    grep -o 'https://github.com/BZcreativ/bzdium/releases/[^)]*' README.md

Every URL the second command prints must answer 200 (`curl -sI <url> | head -1`).

## Pinned entries -- leave alone

- `bzdium-bin-0.2.7-...pkg.tar.zst` (Arch): no newer Arch package has been
  published, so the bullet is deliberately outside the version bump.
- `bzdium-demo.mp4` (v0.2.1): the demo video is not rebuilt per release.
