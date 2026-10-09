use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, State};
use tauri_plugin_dialog::DialogExt;
use url::Url;
use uuid::Uuid;

use crate::state::{emit_snapshot, AppState, Service, Settings, SharedState};
use crate::webviews;

#[derive(Debug, Clone, Serialize)]
pub struct Recipe {
    pub name: String,
    pub url: String,
    pub icon: String,
}

/// Two-letter lettermark for a service name: first letter of the first two
/// words ("Google Chat" → "GC"), or the first two letters of a single word
/// ("Telegram" → "TE").
pub fn initials(name: &str) -> String {
    let letters: Vec<char> = name
        .split_whitespace()
        .filter_map(|w| w.chars().next())
        .map(|c| c.to_uppercase().next().unwrap_or(c))
        .collect();
    let picked: Vec<char> = if letters.len() >= 2 {
        letters[..2].to_vec()
    } else {
        name.chars()
            .filter(|c| c.is_alphabetic())
            .take(2)
            .map(|c| c.to_uppercase().next().unwrap_or(c))
            .collect()
    };
    picked.into_iter().collect()
}

/// Built-in recipe catalog (normative list, spec section 6): the popular
/// slice of Ferdium's recipe store that works as a plain web client
/// (no per-service JS hacks — those are deliberately not ported).
pub fn recipes() -> Vec<Recipe> {
    [
        // messaging
        ("WhatsApp", "https://web.whatsapp.com"),
        ("Telegram", "https://web.telegram.org"),
        ("Discord", "https://discord.com/app"),
        ("Slack", "https://app.slack.com"),
        ("Messenger", "https://www.messenger.com"),
        ("Google Chat", "https://chat.google.com"),
        ("Teams", "https://teams.microsoft.com"),
        ("Teams Personal", "https://teams.live.com"),
        ("Element", "https://app.element.io"),
        ("Mattermost", "https://chat.mattermost.com"),
        ("Rocket.Chat", "https://open.rocket.chat"),
        ("Zulip", "https://chat.zulip.org"),
        ("Wire", "https://app.wire.com"),
        ("Threema Web", "https://web.threema.ch"),
        ("WeChat", "https://web.wechat.com"),
        ("GroupMe", "https://web.groupme.com"),
        ("IRCCloud", "https://www.irccloud.com"),
        ("Skype", "https://web.skype.com"),
        ("Steam Chat", "https://steamcommunity.com/chat"),
        ("Zoom", "https://zoom.us/join"),
        // mail
        ("Gmail", "https://mail.google.com"),
        ("Outlook", "https://outlook.live.com"),
        ("Outlook Work", "https://outlook.office.com"),
        ("Proton Mail", "https://mail.proton.me"),
        ("Proton Calendar", "https://calendar.proton.me"),
        ("Tuta", "https://app.tuta.com"),
        ("Fastmail", "https://app.fastmail.com"),
        ("Hey", "https://app.hey.com"),
        ("Yahoo Mail", "https://mail.yahoo.com"),
        // social
        ("X", "https://x.com"),
        ("Instagram", "https://www.instagram.com"),
        ("LinkedIn", "https://www.linkedin.com"),
        ("Facebook", "https://www.facebook.com"),
        ("Reddit", "https://old.reddit.com"),
        ("Mastodon", "https://mastodon.social"),
        ("Bluesky", "https://bsky.app"),
        ("TikTok", "https://www.tiktok.com"),
        ("Pinterest", "https://www.pinterest.com"),
        ("Twitch", "https://www.twitch.tv"),
        ("YouTube", "https://www.youtube.com"),
        // AI
        ("ChatGPT", "https://chatgpt.com"),
        ("Claude", "https://claude.ai"),
        ("Gemini", "https://gemini.google.com"),
        ("Copilot", "https://copilot.microsoft.com"),
        ("Perplexity", "https://www.perplexity.ai"),
        // productivity
        ("Notion", "https://www.notion.so"),
        ("Google Calendar", "https://calendar.google.com"),
        ("Google Keep", "https://keep.google.com"),
        ("Google Drive", "https://drive.google.com"),
        ("Google Docs", "https://docs.google.com"),
        ("Google Photos", "https://photos.google.com"),
        ("Google Voice", "https://voice.google.com"),
        ("OneDrive", "https://onedrive.live.com"),
        ("Dropbox", "https://www.dropbox.com"),
        ("Trello", "https://trello.com"),
        ("Asana", "https://app.asana.com"),
        ("Todoist", "https://app.todoist.com"),
        ("TickTick", "https://ticktick.com"),
        ("Feedly", "https://feedly.com"),
        ("Bitwarden Vault", "https://vault.bitwarden.com"),
    ]
    .into_iter()
    .map(|(name, url)| Recipe {
        name: name.to_string(),
        url: url.to_string(),
        icon: initials(name),
    })
    .collect()
}

/// Validates that `raw` is a well-formed https URL, returning the normalized form.
pub fn validate_https_url(raw: &str) -> Result<String, String> {
    let parsed = Url::parse(raw.trim()).map_err(|_| format!("invalid URL: {raw:?}"))?;
    if parsed.scheme() != "https" {
        return Err("only https:// URLs are allowed".to_string());
    }
    Ok(parsed.to_string())
}

#[tauri::command]
pub fn get_recipes() -> Vec<Recipe> {
    recipes()
}

// NOTE: commands that (may) create webviews MUST be `async`: on Windows,
// WebView2 controller creation needs the event loop to pump, and a sync
// command blocks the main thread, wedging all IPC for the window
// (https://docs.rs/tauri/latest/tauri/webview/struct.WebviewBuilder.html).
#[tauri::command]
pub async fn add_service(
    app: AppHandle,
    state: State<'_, SharedState>,
    name: String,
    url: String,
    icon: String,
) -> Result<AppState, String> {
    let url = validate_https_url(&url)?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("service name must not be empty".to_string());
    }
    let mut inner = state.lock().map_err(|e| e.to_string())?;
    let order = inner.services.iter().map(|s| s.order).max().map_or(0, |m| m + 1);
    let service = Service {
        id: Uuid::new_v4().to_string(),
        name,
        url,
        icon,
        enabled: true,
        order,
        hibernated: false,
        badge_count: 0,
    };
    inner.services.push(service.clone());
    inner.save_services()?;
    let id = service.id;
    drop(inner);
    // Webview creation happens off-lock (see webviews::activate).
    webviews::activate(&app, &state, &id)?;
    let snapshot = state.lock().map_err(|e| e.to_string())?.snapshot();
    emit_snapshot(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
pub async fn update_service(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
    name: String,
    url: String,
    icon: String,
    enabled: bool,
) -> Result<AppState, String> {
    let url = validate_https_url(&url)?;
    let (url_changed, was_hibernated, service, data_dir, is_active, snapshot) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let (url_changed, was_hibernated) = {
            let service = inner
                .service_mut(&id)
                .ok_or_else(|| format!("unknown service id {id}"))?;
            let url_changed = service.url != url;
            service.name = name.trim().to_string();
            service.url = url;
            service.icon = icon;
            service.enabled = enabled;
            (url_changed, service.hibernated)
        };
        inner.save_services()?;
        let service = inner.service(&id).cloned().expect("checked above");
        let data_dir = inner.data_dir.clone();
        let is_active = inner.active_service_id.as_deref() == Some(id.as_str());
        let snapshot = inner.snapshot();
        (url_changed, was_hibernated, service, data_dir, is_active, snapshot)
    };
    if url_changed && !was_hibernated {
        // Recreate the webview at the new URL (off-lock).
        webviews::close_webview(&app, &id)?;
        webviews::create_service_webview(&app, &service, &data_dir, is_active)?;
        if is_active {
            webviews::show_only(&app, Some(&id))?;
        }
    }
    emit_snapshot(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
pub async fn remove_service(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
) -> Result<AppState, String> {
    let snapshot = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let pos = inner
            .services
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| format!("unknown service id {id}"))?;
        // NOTE: the session folder under sessions/<id> is deliberately kept (spec).
        inner.services.remove(pos);
        inner.last_active.remove(&id);
        if inner.active_service_id.as_deref() == Some(id.as_str()) {
            inner.active_service_id = None;
        }
        inner.save_services()?;
        inner.snapshot()
    };
    webviews::close_webview(&app, &id)?;
    emit_snapshot(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
pub fn reorder_services(
    app: AppHandle,
    state: State<'_, SharedState>,
    ordered_ids: Vec<String>,
) -> Result<AppState, String> {
    let mut inner = state.lock().map_err(|e| e.to_string())?;
    for (index, id) in ordered_ids.iter().enumerate() {
        if let Some(service) = inner.service_mut(id) {
            service.order = index as u32;
        }
    }
    inner.services.sort_by_key(|s| s.order);
    inner.save_services()?;
    let snapshot = inner.snapshot();
    drop(inner);
    emit_snapshot(&app, &snapshot);
    Ok(snapshot)
}

/// A service as stored in an exported config file — no runtime fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigService {
    pub id: String,
    pub name: String,
    pub url: String,
    pub icon: String,
    pub enabled: bool,
    pub order: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigFile {
    #[serde(rename = "type")]
    pub kind: String,
    pub version: u32,
    pub exported_at: String,
    pub settings: Settings,
    pub services: Vec<ConfigService>,
}

pub const CONFIG_TYPE: &str = "bzdium-config";
pub const CONFIG_VERSION: u32 = 1;

/// Result of a successful import: the new app state plus the names of
/// entries that were rejected (invalid URL/name) and skipped.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportOutcome {
    pub state: AppState,
    pub skipped: Vec<String>,
}

fn default_config_file_name() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("bzdium-config-{secs}.json")
}

fn chrono_like_now() -> String {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}", d.as_secs())
}

/// Exports settings + services to a JSON file chosen in a native save
/// dialog. `Ok(None)` means the user cancelled. Session folders (WebView2
/// profiles) are deliberately not part of the export.
#[tauri::command]
pub async fn export_config(app: AppHandle, state: State<'_, SharedState>) -> Result<Option<String>, String> {
    let config = {
        let inner = state.lock().map_err(|e| e.to_string())?;
        let mut services: Vec<ConfigService> = inner
            .services
            .iter()
            .map(|s| ConfigService {
                id: s.id.clone(),
                name: s.name.clone(),
                url: s.url.clone(),
                icon: s.icon.clone(),
                enabled: s.enabled,
                order: s.order,
            })
            .collect();
        services.sort_by_key(|s| s.order);
        ConfigFile {
            kind: CONFIG_TYPE.to_string(),
            version: CONFIG_VERSION,
            exported_at: chrono_like_now(),
            settings: inner.settings.clone(),
            services,
        }
    };
    let file = app
        .dialog()
        .file()
        .add_filter("bzdium config", &["json"])
        .set_file_name(default_config_file_name())
        .blocking_save_file();
    let Some(file) = file else {
        return Ok(None);
    };
    let path = file.into_path().map_err(|e| format!("invalid save path: {e}"))?;
    let json = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    crate::storage::atomic_write(&path, &json)
        .map_err(|e| format!("failed to write {}: {e}", path.display()))?;
    Ok(Some(path.to_string_lossy().into_owned()))
}

/// Imports a config file, REPLACING all current services and settings.
/// Service ids are preserved so existing local sessions reconnect.
/// `Ok(None)` means the user cancelled the file picker.
#[tauri::command]
pub async fn import_config(app: AppHandle, state: State<'_, SharedState>) -> Result<Option<ImportOutcome>, String> {
    let file = app
        .dialog()
        .file()
        .add_filter("bzdium config", &["json"])
        .blocking_pick_file();
    let Some(file) = file else {
        return Ok(None);
    };
    let path = file.into_path().map_err(|e| format!("invalid file: {e}"))?;
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let config: ConfigFile =
        serde_json::from_str(&text).map_err(|e| format!("not a bzdium config file: {e}"))?;
    if config.kind != CONFIG_TYPE || config.version != CONFIG_VERSION {
        return Err(format!(
            "unsupported config file (type {}/{})",
            config.kind, config.version
        ));
    }

    let mut skipped = Vec::new();
    let mut services = Vec::new();
    for s in &config.services {
        let url = match validate_https_url(&s.url) {
            Ok(u) => u,
            Err(_) => {
                skipped.push(s.name.clone());
                continue;
            }
        };
        let name = s.name.trim().to_string();
        if name.is_empty() {
            skipped.push(format!("(unnamed: {url})"));
            continue;
        }
        services.push(Service {
            id: if s.id.trim().is_empty() { Uuid::new_v4().to_string() } else { s.id.clone() },
            name,
            url,
            icon: s.icon.clone(),
            enabled: s.enabled,
            order: s.order,
            hibernated: false,
            badge_count: 0,
        });
    }
    services.sort_by_key(|s| s.order);

    // Phase 1 (locked): replace state + persist.
    let (first_id, start_with_windows) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        inner.settings = config.settings;
        inner.services = services;
        inner.last_active.clear();
        inner.active_service_id = None;
        inner.save_settings()?;
        inner.save_services()?;
        (
            inner.services.first().map(|s| s.id.clone()),
            inner.settings.start_with_windows,
        )
    };

    // Phase 2 (unlocked): side effects.
    let _ = crate::autostart::apply(start_with_windows);
    for (_label, webview) in app.webviews() {
        if webview.label().starts_with("service-") {
            let _ = webview.close();
        }
    }
    if let Some(id) = &first_id {
        webviews::activate(&app, &state, id)?;
    }
    let snapshot = state.lock().map_err(|e| e.to_string())?.snapshot();
    emit_snapshot(&app, &snapshot);
    Ok(Some(ImportOutcome { state: snapshot, skipped }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipe_catalog_matches_spec() {
        let recipes = recipes();
        assert_eq!(recipes.len(), 60);
        assert_eq!(recipes[0].name, "WhatsApp");
        assert_eq!(recipes[0].url, "https://web.whatsapp.com");
        assert_eq!(recipes[0].icon, "WH");
        assert_eq!(recipes[1].name, "Telegram");
        assert_eq!(recipes[1].icon, "TE");
        assert_eq!(recipes[2].name, "Discord");
        assert_eq!(recipes[5].name, "Google Chat");
        assert_eq!(recipes[5].icon, "GC");
        assert_eq!(recipes[7].name, "Teams Personal");
        assert_eq!(recipes[7].icon, "TP");
        assert_eq!(recipes[10].name, "Rocket.Chat");
        assert_eq!(recipes[11].name, "Zulip");
        assert_eq!(recipes[11].icon, "ZU");
        assert_eq!(recipes[20].name, "Gmail");
        assert_eq!(recipes[20].icon, "GM");
        assert_eq!(recipes[28].name, "Yahoo Mail");
        assert_eq!(recipes[28].icon, "YM");
        assert_eq!(recipes[29].name, "X");
        assert_eq!(recipes[29].icon, "X");
        assert_eq!(recipes[39].name, "YouTube");
        assert_eq!(recipes[39].icon, "YO");
        assert_eq!(recipes[41].name, "Claude");
        assert_eq!(recipes[41].icon, "CL");
        assert_eq!(recipes[59].name, "Bitwarden Vault");
        assert_eq!(recipes[59].icon, "BV");
        // every entry has a https URL and a 1-2 letter icon
        for r in &recipes {
            assert!(r.url.starts_with("https://"), "{}: {}", r.name, r.url);
            assert!((1..=2).contains(&r.icon.chars().count()), "{}: {:?}", r.name, r.icon);
        }
    }

    #[test]
    fn initials_rules() {
        assert_eq!(initials("Google Chat"), "GC");
        assert_eq!(initials("Teams Personal"), "TP");
        assert_eq!(initials("Telegram"), "TE");
        assert_eq!(initials("X"), "X");
        assert_eq!(initials("Zoom"), "ZO");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn https_url_validation() {
        assert!(validate_https_url("https://web.whatsapp.com").is_ok());
        assert!(validate_https_url("https://example.com/path?q=1").is_ok());
        assert!(validate_https_url("http://example.com").is_err());
        assert!(validate_https_url("ftp://example.com").is_err());
        assert!(validate_https_url("not a url").is_err());
        assert!(validate_https_url("").is_err());
    }

    #[test]
    fn config_file_round_trip() {
        let config = ConfigFile {
            kind: CONFIG_TYPE.to_string(),
            version: CONFIG_VERSION,
            exported_at: "1770000000".to_string(),
            settings: Settings::default(),
            services: vec![ConfigService {
                id: "seed-1".to_string(),
                name: "Telegram".to_string(),
                url: "https://web.telegram.org/".to_string(),
                icon: "TE".to_string(),
                enabled: true,
                order: 0,
            }],
        };
        let json = serde_json::to_string_pretty(&config).unwrap();
        assert!(json.contains("\"type\": \"bzdium-config\""));
        assert!(json.contains("\"showUrlBar\": true"));
        let back: ConfigFile = serde_json::from_str(&json).unwrap();
        assert_eq!(back.kind, CONFIG_TYPE);
        assert_eq!(back.version, CONFIG_VERSION);
        assert_eq!(back.services.len(), 1);
        assert_eq!(back.services[0].id, "seed-1");

        // Wrong kind/version rejected.
        let bad = json.replace("bzdium-config", "something-else");
        let parsed: Result<ConfigFile, _> = serde_json::from_str(&bad);
        assert!(parsed.is_ok()); // parse ok; rejection happens in import_config's explicit check
    }

    #[test]
    fn settings_default_show_url_bar_true_and_backfills_old_files() {
        // Old settings.json without showUrlBar must deserialize with true.
        let old = r#"{"hibernationMinutes":30,"minimizeToTray":true,"startWithWindows":false,"darkUi":true}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert!(s.show_url_bar);
        assert_eq!(Settings::default().show_url_bar, true);
    }
}
