use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;
use tauri::{AppHandle, Emitter};

pub const EVENT_STATE_CHANGED: &str = "state-changed";
pub const EVENT_URL_CHANGED: &str = "url-changed";
pub const UI_WEBVIEW_LABEL: &str = "main";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Service {
    pub id: String,
    pub name: String,
    pub url: String,
    pub icon: String,
    pub enabled: bool,
    pub order: u32,
    #[serde(default)]
    pub hibernated: bool,
    #[serde(default)]
    pub badge_count: u32,
}

fn default_show_url_bar() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub hibernation_minutes: u64,
    pub minimize_to_tray: bool,
    pub start_with_windows: bool,
    pub dark_ui: bool,
    /// Older settings.json files predate this field; default to visible.
    #[serde(default = "default_show_url_bar")]
    pub show_url_bar: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hibernation_minutes: 30,
            minimize_to_tray: true,
            start_with_windows: false,
            dark_ui: true,
            show_url_bar: true,
        }
    }
}

/// Serializable snapshot sent to the UI (`get_state` / `state-changed`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppState {
    pub services: Vec<Service>,
    pub active_service_id: Option<String>,
    pub settings: Settings,
    pub total_badge_count: u32,
    pub data_dir: String,
}

/// Runtime state managed by Tauri. `last_active` is intentionally not
/// serialized; entries are (re)created whenever a service is activated.
pub struct StateInner {
    pub services: Vec<Service>,
    pub active_service_id: Option<String>,
    pub settings: Settings,
    pub data_dir: PathBuf,
    pub last_active: HashMap<String, Instant>,
}

pub type SharedState = Mutex<StateInner>;

impl StateInner {
    pub fn new(data_dir: PathBuf) -> Self {
        let settings = crate::storage::load_settings(&data_dir);
        let services = crate::storage::load_services(&data_dir);
        Self {
            services,
            active_service_id: None,
            settings,
            data_dir,
            last_active: HashMap::new(),
        }
    }

    pub fn snapshot(&self) -> AppState {
        let mut services = self.services.clone();
        services.sort_by_key(|s| s.order);
        let total_badge_count = services.iter().map(|s| s.badge_count).sum();
        AppState {
            services,
            active_service_id: self.active_service_id.clone(),
            settings: self.settings.clone(),
            total_badge_count,
            data_dir: self.data_dir.to_string_lossy().into_owned(),
        }
    }

    pub fn service(&self, id: &str) -> Option<&Service> {
        self.services.iter().find(|s| s.id == id)
    }

    pub fn service_mut(&mut self, id: &str) -> Option<&mut Service> {
        self.services.iter_mut().find(|s| s.id == id)
    }

    pub fn touch(&mut self, id: &str) {
        self.last_active.insert(id.to_string(), Instant::now());
    }

    pub fn save_settings(&self) -> Result<(), String> {
        crate::storage::save_settings(&self.data_dir, &self.settings)
    }

    pub fn save_services(&self) -> Result<(), String> {
        crate::storage::save_services(&self.data_dir, &self.services)
    }
}

/// Emits the `state-changed` event carrying the full snapshot to the UI
/// webview only (spec section 6).
pub fn emit_state_changed(app: &AppHandle, inner: &StateInner) {
    emit_snapshot(app, &inner.snapshot());
}

/// Like [`emit_state_changed`] but from an already-built snapshot — lets
/// callers release the state lock before emitting.
pub fn emit_snapshot(app: &AppHandle, snapshot: &AppState) {
    if let Err(e) = app.emit_to(UI_WEBVIEW_LABEL, EVENT_STATE_CHANGED, snapshot) {
        eprintln!("failed to emit {EVENT_STATE_CHANGED}: {e}");
    }
}

/// Notifies the UI that a service webview navigated (drives the URL bar).
/// Safe to call from the `on_page_load` hook: touches no state.
pub fn emit_url_changed(app: &AppHandle, service_id: &str, url: &str) {
    use serde::Serialize;
    #[derive(Serialize, Clone)]
    #[serde(rename_all = "camelCase")]
    struct Payload<'a> {
        id: &'a str,
        url: &'a str,
    }
    if let Err(e) = app.emit_to(
        UI_WEBVIEW_LABEL,
        EVENT_URL_CHANGED,
        Payload { id: service_id, url },
    ) {
        eprintln!("failed to emit {EVENT_URL_CHANGED}: {e}");
    }
}
