use serde::Serialize;
use tauri::{AppHandle, State};
use url::Url;
use uuid::Uuid;

use crate::state::{emit_snapshot, AppState, Service, SharedState};
use crate::webviews;

#[derive(Debug, Clone, Serialize)]
pub struct Recipe {
    pub name: String,
    pub url: String,
    pub icon: String,
}

/// Built-in recipe catalog (normative list, spec section 6).
/// Icon = first letter of the name.
pub fn recipes() -> Vec<Recipe> {
    [
        ("WhatsApp", "https://web.whatsapp.com"),
        ("Telegram", "https://web.telegram.org"),
        ("Discord", "https://discord.com/app"),
        ("Slack", "https://app.slack.com"),
        ("Messenger", "https://www.messenger.com"),
        ("Gmail", "https://mail.google.com"),
        ("Outlook", "https://outlook.live.com"),
        ("Google Chat", "https://chat.google.com"),
        ("Teams", "https://teams.microsoft.com"),
        ("Element", "https://app.element.io"),
        ("X", "https://x.com"),
        ("Instagram", "https://www.instagram.com"),
        ("LinkedIn", "https://www.linkedin.com"),
    ]
    .into_iter()
    .map(|(name, url)| Recipe {
        name: name.to_string(),
        url: url.to_string(),
        icon: name.chars().next().unwrap_or('?').to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recipe_catalog_matches_spec() {
        let recipes = recipes();
        assert_eq!(recipes.len(), 13);
        assert_eq!(recipes[0].name, "WhatsApp");
        assert_eq!(recipes[0].url, "https://web.whatsapp.com");
        assert_eq!(recipes[0].icon, "W");
        assert_eq!(recipes[1].name, "Telegram");
        assert_eq!(recipes[2].name, "Discord");
        assert_eq!(recipes[3].name, "Slack");
        assert_eq!(recipes[4].name, "Messenger");
        assert_eq!(recipes[5].name, "Gmail");
        assert_eq!(recipes[6].name, "Outlook");
        assert_eq!(recipes[7].name, "Google Chat");
        assert_eq!(recipes[7].icon, "G");
        assert_eq!(recipes[8].name, "Teams");
        assert_eq!(recipes[9].name, "Element");
        assert_eq!(recipes[10].name, "X");
        assert_eq!(recipes[10].icon, "X");
        assert_eq!(recipes[11].name, "Instagram");
        assert_eq!(recipes[12].name, "LinkedIn");
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
}
