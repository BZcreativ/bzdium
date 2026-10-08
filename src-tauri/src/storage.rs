use serde::{de::DeserializeOwned, Serialize};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::state::{Service, Settings};

pub const PORTABLE_DIR_NAME: &str = "bzdium-data";
pub const FALLBACK_DIR_NAME: &str = "bzdium";
pub const DEV_DIR_NAME: &str = "bzdium-dev-data";
pub const SETTINGS_FILE: &str = "settings.json";
pub const SERVICES_FILE: &str = "services.json";
pub const SESSIONS_DIR: &str = "sessions";

/// Resolves the data directory for this run.
///
/// - Debug builds: a dedicated dev dir next to `target/` so tests/dev runs
///   never touch real user data.
/// - Release builds: `<exe_dir>/bzdium-data` if it is creatable/writable,
///   else `%APPDATA%/bzdium`.
pub fn resolve_data_dir() -> PathBuf {
    if cfg!(debug_assertions) {
        return PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(DEV_DIR_NAME);
    }
    let portable = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join(PORTABLE_DIR_NAME)));
    let fallback = std::env::var("APPDATA")
        .ok()
        .map(|appdata| PathBuf::from(appdata).join(FALLBACK_DIR_NAME));
    choose_data_dir(portable.as_deref(), fallback.as_deref(), dir_is_writable)
}

/// Testable core of the data-dir decision: portable dir if writable, else fallback.
pub fn choose_data_dir(
    portable: Option<&Path>,
    fallback: Option<&Path>,
    writable: impl Fn(&Path) -> bool,
) -> PathBuf {
    if let Some(p) = portable {
        if writable(p) {
            return p.to_path_buf();
        }
    }
    if let Some(f) = fallback {
        return f.to_path_buf();
    }
    // Last resort (no APPDATA?): current directory.
    portable
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Checks whether `path` can be created (if missing) and written to.
pub fn dir_is_writable(path: &Path) -> bool {
    if fs::create_dir_all(path).is_err() {
        return false;
    }
    let probe = path.join(".bzdium-write-test");
    let _ = fs::remove_file(&probe);
    match fs::OpenOptions::new().write(true).create_new(true).open(&probe) {
        Ok(_) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

fn temp_path_for(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    path.with_file_name(format!("{name}.tmp"))
}

/// Atomic write: write to a temp file in the same directory, fsync, then
/// rename over the destination (on Windows std rename replaces existing files).
pub fn atomic_write(path: &Path, contents: &str) -> io::Result<()> {
    let tmp = temp_path_for(path);
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(contents.as_bytes())?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

/// Loads JSON from `path`, returning `T::default()` when the file is missing
/// or corrupt. Corrupt files are preserved as `*.broken-<unix-ts>`.
pub fn load_or_default<T>(path: &Path) -> T
where
    T: DeserializeOwned + Default,
{
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return T::default(),
    };
    match serde_json::from_str::<T>(&text) {
        Ok(v) => v,
        Err(_) => {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "file".to_string());
            let backup = path.with_file_name(format!("{name}.broken-{ts}"));
            let _ = fs::rename(path, backup);
            T::default()
        }
    }
}

pub fn save_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
    atomic_write(path, &text).map_err(|e| format!("failed to write {}: {e}", path.display()))
}

pub fn load_settings(data_dir: &Path) -> Settings {
    load_or_default(&data_dir.join(SETTINGS_FILE))
}

/// Loads configured services. Runtime fields (`badge_count`) are reset.
pub fn load_services(data_dir: &Path) -> Vec<Service> {
    let mut services: Vec<Service> = load_or_default(&data_dir.join(SERVICES_FILE));
    for s in &mut services {
        s.badge_count = 0;
    }
    services.sort_by_key(|s| s.order);
    services
}

pub fn save_settings(data_dir: &Path, settings: &Settings) -> Result<(), String> {
    fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
    save_json(&data_dir.join(SETTINGS_FILE), settings)
}

pub fn save_services(data_dir: &Path, services: &[Service]) -> Result<(), String> {
    fs::create_dir_all(data_dir).map_err(|e| e.to_string())?;
    save_json(&data_dir.join(SERVICES_FILE), services)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Service;

    fn unique_temp_dir(tag: &str) -> PathBuf {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("bzdium-test-{tag}-{ts}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn settings_serde_round_trip() {
        let settings = Settings::default();
        let json = serde_json::to_string(&settings).unwrap();
        // Field names must be camelCase per the IPC contract.
        assert!(json.contains("\"hibernationMinutes\":30"));
        assert!(json.contains("\"minimizeToTray\":true"));
        assert!(json.contains("\"startWithWindows\":false"));
        assert!(json.contains("\"darkUi\":true"));
        let back: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(settings, back);
    }

    #[test]
    fn service_serde_round_trip() {
        let service = Service {
            id: "3f6b2c5e-7a1d-4c9f-9b2e-1d0f8a7c6b5a".to_string(),
            name: "WhatsApp".to_string(),
            url: "https://web.whatsapp.com/".to_string(),
            icon: "W".to_string(),
            enabled: true,
            order: 2,
            hibernated: true,
            badge_count: 4,
        };
        let json = serde_json::to_string(&service).unwrap();
        assert!(json.contains("\"badgeCount\":4"));
        assert!(json.contains("\"hibernated\":true"));
        let back: Service = serde_json::from_str(&json).unwrap();
        assert_eq!(service, back);

        // Runtime fields must default when absent (older/hand-written files).
        let minimal = r#"{"id":"x","name":"n","url":"https://x.com","icon":"X","enabled":true,"order":0}"#;
        let parsed: Service = serde_json::from_str(minimal).unwrap();
        assert!(!parsed.hibernated);
        assert_eq!(parsed.badge_count, 0);
    }

    #[test]
    fn atomic_write_creates_and_overwrites() {
        let dir = unique_temp_dir("atomic");
        let path = dir.join("settings.json");
        atomic_write(&path, "{\"a\":1}").unwrap();
        atomic_write(&path, "{\"a\":2}").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{\"a\":2}");
        assert!(!temp_path_for(&path).exists());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn corrupt_json_is_backed_up_and_defaults_returned() {
        let dir = unique_temp_dir("corrupt");
        let path = dir.join("settings.json");
        fs::write(&path, "{ not valid json !!!").unwrap();
        let settings: Settings = load_or_default(&path);
        assert_eq!(settings, Settings::default());
        assert!(!path.exists());
        let backups: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("settings.json.broken-"))
            .collect();
        assert_eq!(backups.len(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_file_returns_default_without_backup() {
        let dir = unique_temp_dir("missing");
        let path = dir.join("services.json");
        let services: Vec<Service> = load_or_default(&path);
        assert!(services.is_empty());
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn data_dir_prefers_writable_portable() {
        let portable = PathBuf::from("/portable");
        let fallback = PathBuf::from("/fallback");
        let chosen = choose_data_dir(Some(&portable), Some(&fallback), |_| true);
        assert_eq!(chosen, portable);
    }

    #[test]
    fn data_dir_falls_back_when_portable_not_writable() {
        let portable = PathBuf::from("/portable");
        let fallback = PathBuf::from("/fallback");
        let chosen = choose_data_dir(Some(&portable), Some(&fallback), |_| false);
        assert_eq!(chosen, fallback);
    }

    #[test]
    fn data_dir_last_resort_when_no_fallback() {
        let portable = PathBuf::from("/portable");
        let chosen = choose_data_dir(Some(&portable), None, |_| false);
        assert_eq!(chosen, portable);
    }
}
