//! Guards the release download links in README.md.
//!
//! GitHub and Forgejo asset URLs embed both a release tag and a versioned file
//! name, and the bundler derives those names from the package version. Bumping
//! one half of the pair is invisible until someone clicks a 404: commit
//! a8148d0 moved the README tag to v0.2.14 but left six of seven asset names
//! at 0.2.13, so every Linux install link and the Windows installer link were
//! dead. These tests pin the invariants so that release-day drift cannot be
//! committed again.
//!
//! Companion tool: build-helpers/update-release-links.ps1 rewrites the links
//! for a new version and refuses to write a README with mismatched pairs.

use regex::Regex;
use std::collections::BTreeSet;

const VERSION: &str = r"(?P<version>\d+\.\d+\.\d+)";

fn readme() -> String {
    std::fs::read_to_string("../README.md").expect("../README.md must be readable")
}

/// Every (tag, file) pair in a GitHub/Forgejo download URL.
fn download_links(text: &str) -> Vec<(String, String)> {
    let re = Regex::new(&format!(r"releases/download/v{VERSION}/(?P<file>[^)\s\x22]+)"))
        .expect("download URL regex must compile");
    re.captures_iter(text)
        .map(|c| (c["version"].to_string(), c["file"].to_string()))
        .collect()
}

/// A release tag must never be paired with an asset whose name carries a
/// different version. Versionless assets (the demo video) are exempt: their
/// names say nothing to contradict.
#[test]
fn download_tag_and_asset_name_agree_on_the_version() {
    let links = download_links(&readme());
    assert!(
        links.len() >= 8,
        "expected the README's download links, parsed only {}",
        links.len()
    );
    let file_version = Regex::new(VERSION).expect("version regex must compile");
    for (tag, file) in links {
        let Some(found) = file_version.captures(&file) else {
            continue;
        };
        assert_eq!(
            found.name("version").unwrap().as_str(),
            tag,
            "download URL mixes tag v{tag} with file '{file}'"
        );
    }
}

/// The install snippets must name files the page actually links, so a bump
/// cannot leave "apt install ./bzdium_0.2.13_amd64.deb" under a v0.2.14 link.
#[test]
fn install_commands_reference_linked_assets() {
    let text = readme();
    let linked: BTreeSet<String> = download_links(&text).into_iter().map(|(_, f)| f).collect();
    // Only the install snippets: the debugging docs also invoke a relative
    // `./bzdium.exe` that is built locally, not a downloadable release asset.
    let command = Regex::new(
        r#"(?m)^.*(?:apt install|dpkg -i|dnf install|rpm -i|pacman -U|chmod \+x).*?(?:\./)?(?P<file>bzdium[^\s\x60\x22']*\.(?:deb|rpm|AppImage|tgz|tar\.gz|pkg\.tar\.zst|exe))"#,
    )
    .expect("install-command regex must compile");
    let mut checked = 0;
    for capture in command.captures_iter(&text) {
        let file = capture["file"].to_string();
        assert!(
            linked.contains(&file),
            "README tells the reader to install './{file}', but no download link names that file"
        );
        checked += 1;
    }
    assert!(
        checked >= 4,
        "expected the apt/dnf/rpm/pacman/AppImage install commands, saw {checked}"
    );
}

/// The "Latest Release (vX)" label and the release-tag links must name the
/// version every current-version asset uses. The pinned Arch package is a
/// deliberate exception and stays on its own tag.
#[test]
fn latest_release_label_matches_current_links() {
    let text = readme();
    let label = Regex::new(r"Latest Release \(v(?P<version>\d+\.\d+\.\d+)\)")
        .expect("label regex must compile")
        .captures(&text)
        .expect("README must name the latest release version");
    let label_version = label["version"].to_string();

    let tag_link = Regex::new(r"releases/tag/v(?P<version>\d+\.\d+\.\d+)")
        .expect("tag-link regex must compile");
    let mut tag_links = 0;
    for capture in tag_link.captures_iter(&text) {
        assert_eq!(
            &capture["version"], &label_version,
            "a release-tag link points at a different version than the label"
        );
        tag_links += 1;
    }
    assert!(
        tag_links >= 2,
        "expected the GitHub and Forgejo release-tag links, saw {tag_links}"
    );

    let version_re = Regex::new(VERSION).expect("version regex must compile");
    for (tag, file) in download_links(&text) {
        if file.starts_with("bzdium-bin-") || !version_re.is_match(&file) {
            continue;
        }
        assert_eq!(
            tag, label_version,
            "asset '{file}' points at v{tag} but the README documents v{label_version}"
        );
    }
}
