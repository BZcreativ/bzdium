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
    /// 1-2 letter fallback lettermark.
    pub icon: String,
    /// Brand icon slug (dist/assets/icons/<slug>.svg); null -> monogram.
    #[serde(rename = "iconId")]
    pub icon_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Custom service icons
// ---------------------------------------------------------------------------

/// Uploaded icons live as files in <dataDir>/icons/{serviceId}.{ext}.
/// Service configuration stores only the file name — never base64 blobs.
pub const ICONS_DIR: &str = "icons";
/// Upload size limit (bytes). Sidebar icons render at 44 logical px; even
/// a 4x retina raster is a few KB.
pub const MAX_ICON_BYTES: u64 = 256 * 1024;

/// Sniffs the image type from magic bytes (never trusts the picked file
/// name). Returns the canonical extension. SVG acceptance is strict
/// (round-2 N7): the document must START with `<svg` (after BOM/whitespace)
/// or with an `<?xml` declaration followed by `<svg` — an HTML page that
/// merely mentions `<svg` is rejected, and any DOCTYPE/ENTITY is rejected
/// outright (also satisfies the XXE constraint: we never parse, but we
/// refuse to store documents that carry entity machinery).
/// SVGs are only ever rendered through <img>, where scripts cannot execute.
fn sniff_image_type(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.len() >= 8 && bytes[..8] == [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a] {
        return Some(("png", "image/png"));
    }
    if bytes.len() >= 3 && bytes[..3] == [0xff, 0xd8, 0xff] {
        return Some(("jpg", "image/jpeg"));
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some(("webp", "image/webp"));
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
    if head.contains("<!doctype") || head.contains("<!entity") {
        return None;
    }
    let trimmed = head.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']);
    if trimmed.starts_with("<svg") {
        return Some(("svg", "image/svg+xml"));
    }
    if trimmed.starts_with("<?xml") && head.contains("<svg") {
        return Some(("svg", "image/svg+xml"));
    }
    None
}

fn icon_file_dir(data_dir: &std::path::Path) -> std::path::PathBuf {
    data_dir.join(ICONS_DIR)
}

/// Local invariant for every id that reaches a filesystem path or an
/// injected script: service ids are UUIDs. Callers upstream guarantee it
/// (add_service generates, import normalizes); this makes it true locally.
fn uuid_or_reject(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok()
}

/// Removes any stored icon file for the service (any extension), so a
/// replacement with a different format does not leave stale files.
/// No-ops for non-UUID ids (defense in depth, N13).
fn remove_icon_files(data_dir: &std::path::Path, id: &str) {
    if !uuid_or_reject(id) {
        return;
    }
    for ext in ["png", "jpg", "webp", "svg"] {
        let _ = std::fs::remove_file(icon_file_dir(data_dir).join(format!("{id}.{ext}")));
    }
}

/// Uploads a custom icon for a service via a native file picker: validates
/// type + size, stores it as a file, records the file name in the service
/// config. `Err` messages are user-facing (toasted by the UI).
#[tauri::command]
pub async fn pick_service_icon(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
) -> Result<AppState, String> {
    let file = app
        .dialog()
        .file()
        .add_filter("Image (png, jpg, webp, svg)", &["png", "jpg", "jpeg", "webp", "svg"])
        .blocking_pick_file();
    let Some(file) = file else {
        // user cancelled — return current state unchanged
        return Ok(state.lock().map_err(|e| e.to_string())?.snapshot());
    };
    let path = file.into_path().map_err(|e| format!("invalid file: {e}"))?;
    let meta = std::fs::metadata(&path).map_err(|e| format!("cannot read file: {e}"))?;
    if meta.len() > MAX_ICON_BYTES {
        return Err(format!(
            "Icon is {} KB; the limit is {} KB.",
            meta.len() / 1024,
            MAX_ICON_BYTES / 1024
        ));
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read file: {e}"))?;
    let (ext, _mime) =
        sniff_image_type(&bytes).ok_or_else(|| "Not a supported image (png, jpg, webp, svg).".to_string())?;

    let (snapshot, data_dir) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        if inner.service(&id).is_none() {
            return Err(format!("unknown service id {id}"));
        }
        let data_dir = inner.data_dir.clone();
        if !uuid_or_reject(&id) {
            return Err(format!("invalid service id {id}"));
        }
        let dir = icon_file_dir(&data_dir);
        std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create icons dir: {e}"))?;
        remove_icon_files(&data_dir, &id);
        let dest = dir.join(format!("{id}.{ext}"));
        crate::storage::atomic_write_bytes(&dest, &bytes)
            .map_err(|e| format!("cannot store icon: {e}"))?;
        let service = inner.service_mut(&id).expect("checked above");
        service.custom_icon = Some(format!("{id}.{ext}"));
        inner.save_services()?;
        (inner.snapshot(), data_dir)
    };
    let _ = data_dir;
    emit_snapshot(&app, &snapshot);
    Ok(snapshot)
}

/// Clears a service's custom icon. The icon resolver then falls back to the
/// brand icon (or monogram) automatically.
#[tauri::command]
pub async fn reset_service_icon(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
) -> Result<AppState, String> {
    let (snapshot, data_dir) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let service = inner
            .service_mut(&id)
            .ok_or_else(|| format!("unknown service id {id}"))?;
        let had = service.custom_icon.take().is_some();
        let data_dir = inner.data_dir.clone();
        if had {
            inner.save_services()?;
        }
        (inner.snapshot(), data_dir)
    };
    remove_icon_files(&data_dir, &id);
    emit_snapshot(&app, &snapshot);
    Ok(snapshot)
}

/// Returns the custom icon as a data URL for <img> rendering, or null when
/// the service has no (readable) custom icon file.
#[tauri::command]
pub fn get_service_icon(
    state: State<'_, SharedState>,
    id: String,
) -> Result<Option<String>, String> {
    use base64::Engine as _;
    let (file_name, data_dir) = {
        let inner = state.lock().map_err(|e| e.to_string())?;
        let service = inner
            .service(&id)
            .ok_or_else(|| format!("unknown service id {id}"))?;
        (service.custom_icon.clone(), inner.data_dir.clone())
    };
    let Some(file_name) = file_name else {
        return Ok(None);
    };
    // Defense in depth: `custom_icon` could be hand-edited in services.json
    // (e.g. "..\\..\\secrets.txt"). Only ever read back files we named
    // ourselves: exactly "{uuid}.{known-ext}".
    let ext = file_name.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    let stem = &file_name[..file_name.len().saturating_sub(ext.len() + 1)];
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" => "image/jpeg",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => return Ok(None),
    };
    if Uuid::parse_str(stem).is_err() {
        return Ok(None);
    }
    let bytes = std::fs::read(icon_file_dir(&data_dir).join(&file_name))
        .map_err(|e| format!("cannot read icon: {e}"))
        .or_else(|e| {
            // missing/corrupt file: graceful fallback, not an error toast
            eprintln!("{e}");
            Ok::<Vec<u8>, String>(Vec::new())
        })?;
    if bytes.is_empty() {
        return Ok(None);
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(Some(format!("data:{mime};base64,{b64}")))
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
/// (per-service JS hacks are deliberately not ported). Third tuple element:
/// brand icon slug ("" = none bundled -> monogram fallback). Slugs reference
/// dist/assets/icons/<slug>.svg (Simple Icons, CC0).
pub fn recipes() -> Vec<Recipe> {
    [
        // messaging
        ("WhatsApp", "https://web.whatsapp.com", "whatsapp"),
        ("Telegram", "https://web.telegram.org", "telegram"),
        ("Discord", "https://discord.com/app", "discord"),
        ("Slack", "https://app.slack.com", ""),
        ("Messenger", "https://www.messenger.com", "messenger"),
        ("Google Chat", "https://chat.google.com", "googlechat"),
        ("Teams", "https://teams.microsoft.com", ""),
        ("Teams Personal", "https://teams.live.com", ""),
        ("Element", "https://app.element.io", "element"),
        ("Mattermost", "https://chat.mattermost.com", "mattermost"),
        ("Rocket.Chat", "https://open.rocket.chat", ""),
        ("Zulip", "https://chat.zulip.org", "zulip"),
        ("Wire", "https://app.wire.com", "wire"),
        ("Threema Web", "https://web.threema.ch", "threema"),
        ("WeChat", "https://web.wechat.com", "wechat"),
        ("GroupMe", "https://web.groupme.com", "groupme"),
        ("IRCCloud", "https://www.irccloud.com", ""),
        ("Skype", "https://web.skype.com", ""),
        ("Steam Chat", "https://steamcommunity.com/chat", "steam"),
        ("Zoom", "https://zoom.us/join", "zoom"),
        // mail
        ("Gmail", "https://mail.google.com", "gmail"),
        ("Outlook", "https://outlook.live.com", ""),
        ("Outlook Work", "https://outlook.office.com", ""),
        ("Proton Mail", "https://mail.proton.me", "protonmail"),
        ("Proton Calendar", "https://calendar.proton.me", ""),
        ("Tuta", "https://app.tuta.com", "tuta"),
        ("Fastmail", "https://app.fastmail.com", ""),
        ("Hey", "https://app.hey.com", ""),
        ("Yahoo Mail", "https://mail.yahoo.com", ""),
        // social
        ("X", "https://x.com", "x"),
        ("Instagram", "https://www.instagram.com", "instagram"),
        ("LinkedIn", "https://www.linkedin.com", ""),
        ("Facebook", "https://www.facebook.com", "facebook"),
        ("Reddit", "https://old.reddit.com", "reddit"),
        ("Mastodon", "https://mastodon.social", "mastodon"),
        ("Bluesky", "https://bsky.app", "bluesky"),
        ("TikTok", "https://www.tiktok.com", "tiktok"),
        ("Pinterest", "https://www.pinterest.com", "pinterest"),
        ("Twitch", "https://www.twitch.tv", "twitch"),
        ("YouTube", "https://www.youtube.com", "youtube"),
        // AI
        ("ChatGPT", "https://chatgpt.com", ""),
        ("Claude", "https://claude.ai", "anthropic"),
        ("Gemini", "https://gemini.google.com", "googlegemini"),
        ("Copilot", "https://copilot.microsoft.com", ""),
        ("Perplexity", "https://www.perplexity.ai", "perplexity"),
        // productivity
        ("Notion", "https://www.notion.so", "notion"),
        ("Google Calendar", "https://calendar.google.com", "googlecalendar"),
        ("Google Keep", "https://keep.google.com", "googlekeep"),
        ("Google Drive", "https://drive.google.com", "googledrive"),
        ("Google Docs", "https://docs.google.com", "googledocs"),
        ("Google Photos", "https://photos.google.com", "googlephotos"),
        ("Google Voice", "https://voice.google.com", ""),
        ("OneDrive", "https://onedrive.live.com", ""),
        ("Dropbox", "https://www.dropbox.com", "dropbox"),
        ("Trello", "https://trello.com", "trello"),
        ("Asana", "https://app.asana.com", "asana"),
        ("Todoist", "https://app.todoist.com", "todoist"),
        ("TickTick", "https://ticktick.com", "ticktick"),
        ("Feedly", "https://feedly.com", ""),
        ("Bitwarden Vault", "https://vault.bitwarden.com", "bitwarden"),
    ]
    .into_iter()
    .map(|(name, url, slug)| Recipe {
        name: name.to_string(),
        url: url.to_string(),
        icon: initials(name),
        icon_id: if slug.is_empty() { None } else { Some(slug.to_string()) },
    })
    .collect()
}

/// Resolves a brand icon slug from a service URL's host (used for custom
/// URLs and when a service's URL is edited). Hosts without a bundled icon
/// return None -> monogram fallback.
pub fn brand_slug_for_url(raw: &str) -> Option<String> {
    let host = Url::parse(raw.trim())
        .ok()?
        .host_str()?
        .trim_start_matches("www.")
        .to_ascii_lowercase();
    let slug = match host.as_str() {
        // messaging
        "web.whatsapp.com" => "whatsapp",
        "web.telegram.org" => "telegram",
        "discord.com" => "discord",
        "messenger.com" => "messenger",
        "chat.google.com" => "googlechat",
        "app.element.io" => "element",
        "chat.mattermost.com" => "mattermost",
        "chat.zulip.org" => "zulip",
        "app.wire.com" => "wire",
        "web.threema.ch" => "threema",
        "web.wechat.com" => "wechat",
        "web.groupme.com" => "groupme",
        "steamcommunity.com" => "steam",
        "zoom.us" => "zoom",
        // mail
        "mail.google.com" => "gmail",
        "mail.proton.me" => "protonmail",
        "app.tuta.com" => "tuta",
        // social
        "x.com" | "twitter.com" => "x",
        "instagram.com" => "instagram",
        "facebook.com" => "facebook",
        "reddit.com" | "old.reddit.com" => "reddit",
        "mastodon.social" => "mastodon",
        "bsky.app" => "bluesky",
        "tiktok.com" | "www.tiktok.com" => "tiktok",
        "pinterest.com" => "pinterest",
        "twitch.tv" | "www.twitch.tv" => "twitch",
        "youtube.com" | "www.youtube.com" | "m.youtube.com" => "youtube",
        // ai
        "claude.ai" => "anthropic",
        "gemini.google.com" => "googlegemini",
        "www.perplexity.ai" | "perplexity.ai" => "perplexity",
        // productivity
        "notion.so" | "www.notion.so" => "notion",
        "calendar.google.com" => "googlecalendar",
        "keep.google.com" => "googlekeep",
        "drive.google.com" => "googledrive",
        "docs.google.com" => "googledocs",
        "photos.google.com" => "googlephotos",
        "dropbox.com" | "www.dropbox.com" => "dropbox",
        "trello.com" => "trello",
        "app.asana.com" => "asana",
        "todoist.com" | "app.todoist.com" => "todoist",
        "ticktick.com" => "ticktick",
        "vault.bitwarden.com" => "bitwarden",
        // url-match only
        "github.com" => "github",
        "gitlab.com" => "gitlab",
        _ => return None,
    };
    Some(slug.to_string())
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
    icon_id: Option<String>,
) -> Result<AppState, String> {
    let url = validate_https_url(&url)?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("service name must not be empty".to_string());
    }
    // Brand icon: explicit (recipe) or resolved from the URL host.
    let icon_id = match icon_id {
        Some(slug) if is_valid_slug(&slug) => Some(slug),
        _ => brand_slug_for_url(&url),
    };
    let mut inner = state.lock().map_err(|e| e.to_string())?;
    let order = inner.services.iter().map(|s| s.order).max().map_or(0, |m| m + 1);
    let service = Service {
        id: Uuid::new_v4().to_string(),
        name,
        url,
        icon,
        enabled: true,
        order,
        icon_id,
        custom_icon: None,
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

/// Brand-icon slugs become file paths (dist/assets/icons/<slug>.svg is
/// bundled; URL-derived slugs go through the same map), so accept strictly
/// lowercase [a-z0-9-].
fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug.len() <= 32
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
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
    let (url_changed, was_hibernated, now_disabled, service, data_dir, is_active, snapshot) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let (url_changed, was_hibernated, now_disabled) = {
            let service = inner
                .service_mut(&id)
                .ok_or_else(|| format!("unknown service id {id}"))?;
            let url_changed = service.url != url;
            service.name = name.trim().to_string();
            service.url = url;
            service.icon = icon;
            let was_enabled = service.enabled;
            service.enabled = enabled;
            // A URL change re-identifies the service: refresh the default
            // brand icon — but never clobber a user-uploaded custom icon.
            if url_changed && service.custom_icon.is_none() {
                service.icon_id = brand_slug_for_url(&service.url);
            }
            (url_changed, service.hibernated, was_enabled && !enabled)
        };
        if now_disabled {
            // Disabling closes the webview and releases the active tab.
            inner.last_active.remove(&id);
            if inner.active_service_id.as_deref() == Some(id.as_str()) {
                inner.active_service_id = None;
            }
        }
        inner.save_services()?;
        let service = inner.service(&id).cloned().expect("checked above");
        let data_dir = inner.data_dir.clone();
        let is_active = inner.active_service_id.as_deref() == Some(id.as_str());
        let snapshot = inner.snapshot();
        (url_changed, was_hibernated, now_disabled, service, data_dir, is_active, snapshot)
    };
    if now_disabled {
        // Disabled services keep no webview.
        if let Err(e) = webviews::close_webview(&app, &id) {
            eprintln!("update_service: webview close failed: {e}");
            emit_snapshot(&app, &snapshot);
            return Err(e);
        }
    } else if url_changed && !was_hibernated {
        // Recreate the webview at the new URL (off-lock). State is already
        // persisted — on failure still emit so the UI matches disk (N10).
        if let Err(e) = (|| {
            webviews::close_webview(&app, &id)?;
            webviews::create_service_webview(&app, &service, &data_dir, is_active)?;
            if is_active {
                webviews::show_only(&app, Some(&id))?;
            }
            Ok::<(), String>(())
        })() {
            eprintln!("update_service: webview recreate failed: {e}");
            emit_snapshot(&app, &snapshot);
            return Err(e);
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
    let (snapshot, data_dir) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let pos = inner
            .services
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| format!("unknown service id {id}"))?;
        inner.services.remove(pos);
        inner.last_active.remove(&id);
        if inner.active_service_id.as_deref() == Some(id.as_str()) {
            inner.active_service_id = None;
        }
        inner.save_services()?;
        let data_dir = inner.data_dir.clone();
        (inner.snapshot(), data_dir)
    };
    if let Err(e) = webviews::close_webview(&app, &id) {
        eprintln!("remove_service: webview close failed: {e}");
        emit_snapshot(&app, &snapshot);
        return Err(e);
    }
    // The session profile is unrecoverable garbage after removal (a re-added
    // service always gets a fresh UUID), so reclaim it. Best-effort: WebView2
    // may hold locks on the folder briefly after close.
    let session_dir = data_dir.join(crate::storage::SESSIONS_DIR).join(&id);
    if let Err(e) = std::fs::remove_dir_all(&session_dir) {
        if session_dir.exists() {
            eprintln!("remove_service: could not reclaim session dir {}: {e}", session_dir.display());
        }
    }
    // Custom icon file goes with the service.
    remove_icon_files(&data_dir, &id);
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
/// Brand icon slug travels with the export; uploaded custom icons are local
/// files and deliberately do not.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ConfigService {
    pub id: String,
    pub name: String,
    pub url: String,
    pub icon: String,
    pub enabled: bool,
    pub order: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_id: Option<String>,
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

/// Service ids become directory names (`sessions/<id>`) and webview labels,
/// so ids from an imported file must be strictly controlled: accept ONLY
/// well-formed hyphenated UUIDs that are not already taken. Anything else
/// (path separators, `..`, absolute paths, junk, duplicates) is replaced
/// with a fresh UUID — this kills path traversal via `Path::join` and
/// session-profile sharing between duplicated services.
fn normalize_imported_id(raw: &str, seen: &mut std::collections::HashSet<String>) -> String {
    let t = raw.trim();
    if t.len() == 36 && Uuid::parse_str(t).is_ok() && seen.insert(t.to_lowercase()) {
        return t.to_lowercase();
    }
    loop {
        let fresh = Uuid::new_v4().to_string();
        if seen.insert(fresh.clone()) {
            return fresh;
        }
    }
}

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
                icon_id: s.icon_id.clone(),
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
    let mut seen_ids = std::collections::HashSet::new();
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
            id: normalize_imported_id(&s.id, &mut seen_ids),
            name,
            url,
            icon: s.icon.clone(),
            enabled: s.enabled,
            order: s.order,
            // brand slug travels with the export; validate strictly since
            // slugs become asset file paths
            icon_id: s.icon_id.as_deref().filter(|s| is_valid_slug(s)).map(str::to_string),
            // uploaded custom icons are local files — they do not travel
            custom_icon: None,
            hibernated: false,
            badge_count: 0,
        });
    }
    services.sort_by_key(|s| s.order);

    // Phase 1 (locked): replace state + persist. Imported `startWithWindows`
    // is deliberately ignored (a config file must not silently flip the
    // HKCU Run key) — the current value is kept.
    let (first_id, keep_ids, data_dir) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let mut new_settings = config.settings;
        new_settings.start_with_windows = inner.settings.start_with_windows;
        inner.settings = new_settings;
        inner.services = services;
        inner.last_active.clear();
        inner.active_service_id = None;
        inner.save_settings()?;
        inner.save_services()?;
        let first_id = inner
            .services
            .iter()
            .find(|s| s.enabled)
            .map(|s| s.id.clone());
        let keep_ids: std::collections::HashSet<String> =
            inner.services.iter().map(|s| s.id.clone()).collect();
        let data_dir = inner.data_dir.clone();
        (first_id, keep_ids, data_dir)
    };

    // Phase 2 (unlocked): side effects. Activation failure must NOT abort
    // the import (state was already replaced) — log and still notify.
    for (_label, webview) in app.webviews() {
        if webview.label().starts_with("service-") {
            let _ = webview.close();
        }
    }
    // Webviews are closed, so their profile dirs are unlockable: reclaim
    // sessions and icons of services that no longer exist (round-2 N5).
    reclaim_orphans(&data_dir, &keep_ids);
    if let Some(id) = &first_id {
        if let Err(e) = webviews::activate(&app, &state, id) {
            eprintln!("import: failed to activate {id}: {e}");
        }
    }
    let snapshot = state.lock().map_err(|e| e.to_string())?.snapshot();
    emit_snapshot(&app, &snapshot);
    Ok(Some(ImportOutcome { state: snapshot, skipped }))
}

/// Deletes session dirs and icon files whose service id is not in `keep`.
/// Replacing services via import must not leave old profiles behind
/// forever (round-2 N5). Never touches dirs for ids still configured.
fn reclaim_orphans(data_dir: &std::path::Path, keep: &std::collections::HashSet<String>) {
    let sessions = data_dir.join(crate::storage::SESSIONS_DIR);
    if let Ok(rd) = std::fs::read_dir(&sessions) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !keep.contains(&name) {
                if let Err(e) = std::fs::remove_dir_all(entry.path()) {
                    eprintln!("import: could not reclaim session dir {}: {e}", entry.path().display());
                }
            }
        }
    }
    let icons = icon_file_dir(data_dir);
    if let Ok(rd) = std::fs::read_dir(&icons) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let stem = name.split('.').next().unwrap_or("").to_string();
            if !keep.contains(&stem) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
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
    fn brand_slug_url_matching() {
        assert_eq!(brand_slug_for_url("https://web.whatsapp.com/").as_deref(), Some("whatsapp"));
        assert_eq!(brand_slug_for_url("https://www.instagram.com/").as_deref(), Some("instagram"));
        assert_eq!(brand_slug_for_url("https://github.com/org/repo").as_deref(), Some("github"));
        assert_eq!(brand_slug_for_url("https://x.com/home").as_deref(), Some("x"));
        assert_eq!(brand_slug_for_url("https://claude.ai/chat").as_deref(), Some("anthropic"));
        // no bundled icon -> monogram
        assert_eq!(brand_slug_for_url("https://app.slack.com/"), None);
        assert_eq!(brand_slug_for_url("https://example.com/"), None);
        assert_eq!(brand_slug_for_url("not a url"), None);
        // every catalog recipe with a slug resolves back through its URL
        for r in recipes() {
            if let Some(slug) = &r.icon_id {
                assert_eq!(brand_slug_for_url(&r.url).as_deref(), Some(slug.as_str()),
                    "recipe {} url/host mismatch", r.name);
                assert!(is_valid_slug(slug), "invalid slug {slug}");
            }
        }
    }

    #[test]
    fn bundled_brand_slugs_have_assets() {
        // The icon files are part of the repo; this pins catalog slugs to
        // actually-bundled assets so nothing silently falls back.
        let icons_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("dist")
            .join("assets")
            .join("icons");
        let manifest: std::collections::HashMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(icons_dir.join("manifest.json")).unwrap())
                .unwrap();
        assert!(manifest.len() >= 40, "expected the bundled icon set, got {}", manifest.len());
        for r in recipes() {
            if let Some(slug) = &r.icon_id {
                assert!(manifest.contains_key(slug), "manifest missing {slug}");
                assert!(icons_dir.join(format!("{slug}.svg")).exists(), "svg missing for {slug}");
            }
        }
    }

    #[test]
    fn image_type_sniffing() {
        assert_eq!(sniff_image_type(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).map(|t| t.0), Some("png"));
        assert_eq!(sniff_image_type(&[0xff, 0xd8, 0xff, 0xe0]).map(|t| t.0), Some("jpg"));
        assert_eq!(sniff_image_type(b"RIFF\x00\x00\x00\x00WEBPVP8 ").map(|t| t.0), Some("webp"));
        assert_eq!(sniff_image_type(b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>").map(|t| t.0), Some("svg"));
        assert_eq!(sniff_image_type(b"<?xml version=\"1.0\"?><svg/>").map(|t| t.0), Some("svg"));
        assert_eq!(sniff_image_type(b"not an image at all"), None);
        assert_eq!(sniff_image_type(&[]), None);
    }

    #[test]
    fn svg_sniffing_is_strict() {
        // HTML that merely mentions <svg must NOT pass as SVG (round-2 N7)
        assert_eq!(sniff_image_type(b"<!DOCTYPE html><html><body><svg>hi</svg></body></html>").map(|t| t.0), None);
        assert_eq!(sniff_image_type(b"<html><body><p>about <svg> tags</p></body></html>").map(|t| t.0), None);
        // DOCTYPE/ENTITY rejected outright (never store entity machinery)
        assert_eq!(sniff_image_type(b"<?xml version=\"1.0\"?><!DOCTYPE svg [<!ENTITY x \"y\">]><svg/>").map(|t| t.0), None);
        assert_eq!(sniff_image_type(b"<!DOCTYPE svg PUBLIC \"-//W3C//DTD SVG 1.1//EN\"><svg/>").map(|t| t.0), None);
        // BOM + leading whitespace before <svg is fine
        assert_eq!(sniff_image_type(b"\xef\xbb\xbf  <svg/>").map(|t| t.0), Some("svg"));
        // xml declaration WITHOUT svg in head is not an image
        assert_eq!(sniff_image_type(b"<?xml version=\"1.0\"?><rss/>").map(|t| t.0), None);
    }

    #[test]
    fn reclaim_orphans_removes_only_unknown_ids() {
        let dir = std::env::temp_dir().join(format!(
            "bzdium-reclaim-{}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let sessions = dir.join(crate::storage::SESSIONS_DIR);
        let icons = icon_file_dir(&dir);
        std::fs::create_dir_all(&icons).unwrap();
        std::fs::create_dir_all(sessions.join("11111111-1111-4111-8111-111111111111")).unwrap();
        std::fs::create_dir_all(sessions.join("22222222-2222-4222-8222-222222222222")).unwrap();
        std::fs::write(icons.join("11111111-1111-4111-8111-111111111111.png"), b"x").unwrap();
        std::fs::write(icons.join("33333333-3333-4333-8333-333333333333.png"), b"x").unwrap();

        let keep: std::collections::HashSet<String> =
            ["11111111-1111-4111-8111-111111111111".to_string()].into_iter().collect();
        reclaim_orphans(&dir, &keep);

        assert!(sessions.join("11111111-1111-4111-8111-111111111111").exists());
        assert!(!sessions.join("22222222-2222-4222-8222-222222222222").exists());
        assert!(icons.join("11111111-1111-4111-8111-111111111111.png").exists());
        assert!(!icons.join("33333333-3333-4333-8333-333333333333.png").exists());
        std::fs::remove_dir_all(&dir).ok();
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
                icon_id: Some("telegram".to_string()),
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

    #[test]
    fn imported_ids_are_uuids_only_and_deduped() {
        let mut seen = std::collections::HashSet::new();
        // Well-formed UUID passes through (lowercased).
        let good = "3F6B2C5E-7A1D-4C9F-9B2E-1D0F8A7C6B5A";
        let kept = normalize_imported_id(good, &mut seen);
        assert_eq!(kept, "3f6b2c5e-7a1d-4c9f-9b2e-1d0f8a7c6b5a");
        // Same id again → deduped to a DIFFERENT uuid.
        let dup = normalize_imported_id(good, &mut seen);
        assert_ne!(dup, kept);
        assert!(Uuid::parse_str(&dup).is_ok());
        // Traversal / absolute / junk ids are replaced with fresh UUIDs.
        for evil in [
            "..\\..\\..\\Users\\me\\AppData",
            "../../etc",
            "C:\\Windows\\System32",
            "",
            "seed-telegram-0001",
            "service-x",
        ] {
            let id = normalize_imported_id(evil, &mut seen);
            assert!(Uuid::parse_str(&id).is_ok(), "{evil:?} -> {id}");
            assert_eq!(id.len(), 36);
            assert!(!id.contains('\\') && !id.contains('/') && !id.contains(".."));
            assert_eq!(id.chars().filter(|c| *c == '-').count(), 4);
        }
    }
}
