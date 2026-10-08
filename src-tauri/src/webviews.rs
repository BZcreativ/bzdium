use std::fs;
use std::path::Path;

use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, State, WebviewUrl, Wry};

use crate::badges;
use crate::state::{emit_snapshot, AppState, Service, SharedState};

/// Sidebar width in logical pixels (window geometry contract, spec section 6).
pub const SIDEBAR_WIDTH: f64 = 72.0;

pub const WEBVIEW2_DOWNLOAD_URL: &str =
    "https://developer.microsoft.com/en-us/microsoft-edge/webview2/";

pub fn label_for(service_id: &str) -> String {
    format!("service-{service_id}")
}

fn webview_error(e: impl std::fmt::Display) -> String {
    format!(
        "failed to create service webview: {e}. If the WebView2 runtime is missing, install it from {WEBVIEW2_DOWNLOAD_URL}"
    )
}

/// (Re)creates the webview for a service. No-op if it already exists.
/// The webview is created at the correct bounds and then hidden unless
/// `visible` is requested.
pub fn create_service_webview(
    app: &AppHandle,
    service: &Service,
    data_dir: &Path,
    visible: bool,
) -> Result<(), String> {
    let label = label_for(&service.id);
    if app.get_webview(&label).is_some() {
        if visible {
            return show_only(app, Some(&service.id));
        }
        return Ok(());
    }
    let window = app
        .get_window(crate::state::UI_WEBVIEW_LABEL)
        .ok_or("main window not found")?;
    let url = service
        .url
        .parse()
        .map_err(|e| format!("invalid service URL {}: {e}", service.url))?;
    let session_dir = data_dir
        .join(crate::storage::SESSIONS_DIR)
        .join(&service.id);
    fs::create_dir_all(&session_dir).map_err(|e| format!("cannot create session dir: {e}"))?;

    let (width, height) = content_size_logical(&window)?;
    let builder = tauri::webview::WebviewBuilder::<Wry>::new(label, WebviewUrl::External(url))
        .data_directory(session_dir)
        .initialization_script(badges::init_script(&service.id))
        .focused(visible);
    let webview = window
        .add_child(
            builder,
            LogicalPosition::new(SIDEBAR_WIDTH, 0.0),
            LogicalSize::new((width - SIDEBAR_WIDTH).max(0.0), height),
        )
        .map_err(webview_error)?;
    if visible {
        let _ = webview.set_focus();
    } else {
        let _ = webview.hide();
    }
    Ok(())
}

fn content_size_logical(window: &tauri::Window) -> Result<(f64, f64), String> {
    let size = window.inner_size().map_err(|e| e.to_string())?;
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    Ok((
        size.width as f64 / scale,
        size.height as f64 / scale,
    ))
}

/// Closes a service webview if it exists.
pub fn close_webview(app: &AppHandle, service_id: &str) -> Result<(), String> {
    if let Some(webview) = app.get_webview(&label_for(service_id)) {
        webview.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Shows the webview of `active_id` (if any) and hides all other service
/// webviews. Bounds of the shown webview are resynced.
pub fn show_only(app: &AppHandle, active_id: Option<&str>) -> Result<(), String> {
    let window = app
        .get_window(crate::state::UI_WEBVIEW_LABEL)
        .ok_or("main window not found")?;
    for webview in window.webviews() {
        let label = webview.label().to_string();
        if !label.starts_with("service-") {
            continue;
        }
        let is_active = active_id == Some(&label["service-".len()..]);
        if is_active {
            let (width, height) = content_size_logical(&window)?;
            webview
                .set_position(LogicalPosition::new(SIDEBAR_WIDTH, 0.0))
                .map_err(|e| e.to_string())?;
            webview
                .set_size(LogicalSize::new((width - SIDEBAR_WIDTH).max(0.0), height))
                .map_err(|e| e.to_string())?;
            webview.show().map_err(|e| e.to_string())?;
            let _ = webview.set_focus();
        } else {
            webview.hide().map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Re-applies the geometry contract to every open service webview
/// (called on window resize).
pub fn sync_bounds(app: &AppHandle) {
    let Some(window) = app.get_window(crate::state::UI_WEBVIEW_LABEL) else {
        return;
    };
    let Ok((width, height)) = content_size_logical(&window) else {
        return;
    };
    for webview in window.webviews() {
        if !webview.label().starts_with("service-") {
            continue;
        }
        let _ = webview.set_position(LogicalPosition::new(SIDEBAR_WIDTH, 0.0));
        let _ = webview.set_size(LogicalSize::new((width - SIDEBAR_WIDTH).max(0.0), height));
    }
}

/// Activates a service: wakes it if hibernated, shows its webview, hides
/// the others, resets its hibernation timer. Manages the state lock itself
/// and NEVER holds it across webview operations — webview creation runs on
/// the event loop with a nested message pump, and a sync command processed
/// inside that pump while the lock is held would deadlock the whole app.
pub fn activate(app: &AppHandle, state: &SharedState, id: &str) -> Result<(), String> {
    let (service, data_dir) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let service = inner
            .service(id)
            .cloned()
            .ok_or_else(|| format!("unknown service id {id}"))?;
        let data_dir = inner.data_dir.clone();
        inner.touch(id);
        if service.hibernated {
            if let Some(s) = inner.service_mut(id) {
                s.hibernated = false;
            }
            inner.save_services()?;
        }
        inner.active_service_id = Some(id.to_string());
        (service, data_dir)
    };
    create_service_webview(app, &service, &data_dir, true)?;
    show_only(app, Some(id))
}

#[tauri::command]
pub async fn set_active_service(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: Option<String>,
) -> Result<AppState, String> {
    match id {
        Some(id) => {
            activate(&app, &state, &id)?;
            let snapshot = state.lock().map_err(|e| e.to_string())?.snapshot();
            emit_snapshot(&app, &snapshot);
            Ok(snapshot)
        }
        None => {
            let (snapshot, _) = {
                let mut inner = state.lock().map_err(|e| e.to_string())?;
                inner.active_service_id = None;
                let s = inner.snapshot();
                (s, ())
            };
            show_only(&app, None)?;
            emit_snapshot(&app, &snapshot);
            Ok(snapshot)
        }
    }
}

#[tauri::command]
pub async fn reload_service(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
) -> Result<AppState, String> {
    let (service, data_dir, snapshot) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let service = inner
            .service(&id)
            .cloned()
            .ok_or_else(|| format!("unknown service id {id}"))?;
        let data_dir = inner.data_dir.clone();
        if service.hibernated {
            if let Some(s) = inner.service_mut(&id) {
                s.hibernated = false;
            }
            inner.save_services()?;
        }
        inner.touch(&id);
        let snapshot = inner.snapshot();
        (service, data_dir, snapshot)
    };
    if app.get_webview(&label_for(&id)).is_none() {
        create_service_webview(&app, &service, &data_dir, true)?;
    }
    if let Some(webview) = app.get_webview(&label_for(&id)) {
        webview
            .eval("location.reload()")
            .map_err(|e| e.to_string())?;
    }
    emit_snapshot(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
pub async fn navigate(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
    action: String,
) -> Result<AppState, String> {
    let (service, data_dir, snapshot) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let service = inner
            .service(&id)
            .cloned()
            .ok_or_else(|| format!("unknown service id {id}"))?;
        let data_dir = inner.data_dir.clone();
        if service.hibernated {
            if let Some(s) = inner.service_mut(&id) {
                s.hibernated = false;
            }
            inner.save_services()?;
        }
        inner.touch(&id);
        let snapshot = inner.snapshot();
        (service, data_dir, snapshot)
    };
    if app.get_webview(&label_for(&id)).is_none() {
        create_service_webview(&app, &service, &data_dir, true)?;
    }
    if let Some(webview) = app.get_webview(&label_for(&id)) {
        match action.as_str() {
            "back" => webview.eval("history.back()").map_err(|e| e.to_string())?,
            "forward" => webview.eval("history.forward()").map_err(|e| e.to_string())?,
            "home" => {
                let url = service.url.parse().map_err(|e| format!("{e}"))?;
                webview.navigate(url).map_err(|e| e.to_string())?;
            }
            other => return Err(format!("unknown navigate action {other:?}")),
        }
    }
    emit_snapshot(&app, &snapshot);
    Ok(snapshot)
}

/// UI overlays (modals, context menu) live in the UI webview, which sits
/// below service webviews in z-order — an open dialog over the content area
/// would be occluded by the active service webview. While an overlay is
/// open the UI asks us to hide all service webviews; on close we restore
/// the active one. Pure view operation: no state change, no event.
#[tauri::command]
pub fn set_overlay_mode(
    app: AppHandle,
    state: State<'_, SharedState>,
    open: bool,
) -> Result<(), String> {
    let active = {
        let inner = state.lock().map_err(|e| e.to_string())?;
        inner.active_service_id.clone()
    };
    if open {
        show_only(&app, None)?;
    } else {
        show_only(&app, active.as_deref())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn hibernate_service(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
) -> Result<AppState, String> {
    let snapshot = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        if inner.service(&id).is_none() {
            return Err(format!("unknown service id {id}"));
        }
        if let Some(s) = inner.service_mut(&id) {
            s.hibernated = true;
            s.badge_count = 0;
        }
        inner.last_active.remove(&id);
        inner.save_services()?;
        inner.snapshot()
    };
    close_webview(&app, &id)?;
    emit_snapshot(&app, &snapshot);
    Ok(snapshot)
}

#[tauri::command]
pub async fn wake_service(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
) -> Result<AppState, String> {
    let (service, data_dir, is_active, snapshot) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let service = inner
            .service(&id)
            .cloned()
            .ok_or_else(|| format!("unknown service id {id}"))?;
        let data_dir = inner.data_dir.clone();
        inner.touch(&id);
        if let Some(s) = inner.service_mut(&id) {
            s.hibernated = false;
        }
        inner.save_services()?;
        let is_active = inner.active_service_id.as_deref() == Some(id.as_str());
        let snapshot = inner.snapshot();
        (service, data_dir, is_active, snapshot)
    };
    create_service_webview(&app, &service, &data_dir, is_active)?;
    if is_active {
        show_only(&app, Some(&id))?;
    }
    emit_snapshot(&app, &snapshot);
    Ok(snapshot)
}
