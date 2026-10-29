//! WebView2 runtime selection (Windows).
//!
//! The Evergreen WebView2 runtime auto-updates itself under running apps.
//! Runtime 156.0.4314.x introduced a regression that wedges service webviews'
//! network stacks after minutes of use (see `watchdog.rs`); the only real
//! cure is running on a different runtime build. WebView2's loader honours
//! the `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER` variable when it resolves the
//! runtime, so bzdium sets it — from Settings — before the first webview is
//! created. Because the loader reads it once per process, a change takes
//! effect on the next app start; the Settings UI says so.
//!
//! On Linux this module's functions are inert: `apply_override` is a no-op
//! and `list_runtimes` returns an empty list (WebKitGTK has no equivalent).

use std::path::{Path, PathBuf};

use serde::Serialize;

/// One installed WebView2 runtime the user can pin to.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeInfo {
    /// Dotted version, e.g. "155.0.4283.62".
    pub version: String,
    /// Absolute folder containing msedgewebview2.exe.
    pub path: String,
}

/// Standard Evergreen install roots, machine-wide and per-user.
fn search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Ok(dir) = std::env::var("ProgramFiles(x86)") {
        roots.push(PathBuf::from(dir).join("Microsoft").join("EdgeWebView").join("Application"));
    }
    if let Ok(dir) = std::env::var("ProgramFiles") {
        roots.push(PathBuf::from(dir).join("Microsoft").join("EdgeWebView").join("Application"));
    }
    if let Ok(dir) = std::env::var("LOCALAPPDATA") {
        roots.push(PathBuf::from(dir).join("Microsoft").join("EdgeWebView").join("Application"));
    }
    roots
}

/// Directory name inside the bzdium data dir where a user can side-load a
/// Microsoft "Fixed Version" runtime (`<dataDir>/webview2-runtimes/<version>/`).
/// The Evergreen updater deletes superseded runtimes from its own roots —
/// often within days — so holding a known-good build requires a place the
/// updater never touches.
pub const SIDECAR_DIR: &str = "webview2-runtimes";

fn sidecar_root(data_dir: &Path) -> PathBuf {
    data_dir.join(SIDECAR_DIR)
}

fn allowed_roots(data_dir: &Path) -> Vec<PathBuf> {
    let mut roots = search_roots();
    roots.push(sidecar_root(data_dir));
    roots
}

/// A candidate folder is a runtime iff it contains msedgewebview2.exe. This
/// is the base validation; anything that can be *persisted* or *applied*
/// must additionally pass [`folder_is_allowed`].
pub fn folder_is_valid(folder: &Path) -> bool {
    folder.is_dir() && folder.join("msedgewebview2.exe").is_file()
}

/// True when `folder` lies under one of the allowed roots (Evergreen install
/// roots plus the bzdium sidecar dir). The pin reaches
/// `WEBVIEW2_BROWSER_EXECUTABLE_FOLDER`, a folder the loader executes
/// binaries from — so a value that arrived through the agent API or an
/// imported config must never point outside the runtimes bzdium itself
/// discovered. Pure path-prefix check; testable with synthetic roots.
fn path_under_roots(folder: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| folder.starts_with(root))
}

/// Validation gate for values entering Settings or applied at startup:
/// must be an existing runtime folder under a known install root or the
/// data-dir sidecar root.
pub fn folder_is_allowed(folder: &str, data_dir: &Path) -> bool {
    if !cfg!(windows) {
        return false;
    }
    let path = Path::new(folder);
    folder_is_valid(path) && path_under_roots(path, &allowed_roots(data_dir))
}

/// Version sort key: "155.0.4283.62" -> (155, 0, 4283, 62). Unparsable
/// components count as 0 so the sort never panics on odd folder names.
fn version_key(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

/// Enumerates installed runtimes, newest first: everything under the
/// Evergreen roots plus anything side-loaded into
/// `<dataDir>/webview2-runtimes/<version>/`. Purely filesystem-based, so it
/// is safe to call from any thread and needs no WebView2 interaction.
pub fn list_runtimes(data_dir: &Path) -> Vec<RuntimeInfo> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let mut found: Vec<RuntimeInfo> = Vec::new();
    for root in allowed_roots(data_dir) {
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !folder_is_valid(&path) {
                continue;
            }
            if found.iter().any(|r| r.path == path.to_string_lossy()) {
                continue;
            }
            found.push(RuntimeInfo {
                version: name.to_string(),
                path: path.to_string_lossy().into_owned(),
            });
        }
    }
    found.sort_by(|a, b| version_key(&b.version).cmp(&version_key(&a.version)));
    found
}

/// Applies the pin for this process. Must run before the first webview is
/// created. A folder outside the allowed roots, or one that has vanished
/// (e.g. an Evergreen cleanup), is refused — the process falls back to the
/// system default instead of executing from an arbitrary path.
pub fn apply_override(folder: &str, data_dir: &Path) -> bool {
    if !folder_is_allowed(folder, data_dir) {
        return false;
    }
    // The loader only re-reads this per environment creation, and every
    // webview in the process must agree on one runtime, so this is
    // process-global by design.
    std::env::set_var("WEBVIEW2_BROWSER_EXECUTABLE_FOLDER", folder);
    true
}

/// Settings panel: runtimes available to pin to, newest first.
#[tauri::command]
pub fn list_webview2_runtimes(
    state: tauri::State<'_, crate::state::SharedState>,
) -> Result<Vec<RuntimeInfo>, String> {
    let data_dir = state.lock().map_err(|e| e.to_string())?.data_dir.clone();
    Ok(list_runtimes(&data_dir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_ordering_is_newest_first() {
        let mut v = vec![
            RuntimeInfo { version: "155.0.4283.62".into(), path: "a".into() },
            RuntimeInfo { version: "156.0.4314.50".into(), path: "b".into() },
            RuntimeInfo { version: "99.1".into(), path: "c".into() },
        ];
        v.sort_by(|a, b| version_key(&b.version).cmp(&version_key(&a.version)));
        let versions: Vec<&str> = v.iter().map(|r| r.version.as_str()).collect();
        assert_eq!(versions, vec!["156.0.4314.50", "155.0.4283.62", "99.1"]);
    }

    #[test]
    fn validation_requires_the_loader_binary() {
        let dir = std::env::temp_dir().join(format!(
            "bzdium-wv2-test-{}-{:x}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // empty dir: not a runtime
        assert!(!folder_is_valid(&dir));
        // with the binary: valid (name alone is enough — never executed here)
        std::fs::write(dir.join("msedgewebview2.exe"), b"").unwrap();
        assert!(folder_is_valid(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn apply_override_rejects_missing_folders() {
        let data_dir = std::env::temp_dir();
        assert!(!apply_override("Z:\\definitely\\not\\here", &data_dir));
    }

    #[test]
    fn paths_outside_the_roots_are_not_allowed() {
        let roots = vec![PathBuf::from("/rt/a"), PathBuf::from("/rt/b")];
        assert!(path_under_roots(Path::new("/rt/a/155.0"), &roots));
        assert!(path_under_roots(Path::new("/rt/b/x"), &roots));
        assert!(!path_under_roots(Path::new("/elsewhere/155.0"), &roots));
        assert!(!path_under_roots(Path::new("/rt"), &roots), "the root itself is not a runtime");
    }

    #[test]
    fn sidecar_runtimes_are_discovered_and_allowed() {
        // A Fixed Version runtime extracted into <dataDir>/webview2-runtimes/
        // must be listed by list_runtimes and accepted by folder_is_allowed —
        // this is the supported way to hold a build the Evergreen updater
        // would otherwise delete from its own roots.
        let data_dir = std::env::temp_dir().join(format!(
            "bzdium-wv2-sidecar-{}-{:x}",
            std::process::id(),
            line!()
        ));
        let runtime = data_dir.join(SIDECAR_DIR).join("155.0.4283.62");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::write(runtime.join("msedgewebview2.exe"), b"").unwrap();

        let listed = list_runtimes(&data_dir);
        assert!(
            listed.iter().any(|r| r.version == "155.0.4283.62"),
            "sidecar runtime must appear in the Settings list"
        );
        assert!(folder_is_allowed(
            &runtime.to_string_lossy(),
            &data_dir
        ));
        // ...but a folder OUTSIDE the roots still is not, even with the exe.
        let rogue = data_dir.join("rogue-rt");
        std::fs::create_dir_all(&rogue).unwrap();
        std::fs::write(rogue.join("msedgewebview2.exe"), b"").unwrap();
        assert!(!folder_is_allowed(&rogue.to_string_lossy(), &data_dir));

        std::fs::remove_dir_all(&data_dir).ok();
    }
}
