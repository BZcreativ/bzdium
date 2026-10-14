use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::{LazyLock, Mutex};

use tauri::{AppHandle, LogicalPosition, LogicalSize, Manager, State, WebviewUrl, Wry};

use crate::badges;
use crate::state::{emit_snapshot, AppState, Service, SharedState};

/// Sidebar width in logical pixels (window geometry contract, spec section 6).
pub const SIDEBAR_WIDTH: f64 = 72.0;

/// Height of the URL bar strip in logical pixels when visible.
pub const URL_BAR_HEIGHT: f64 = 40.0;

pub const WEBVIEW2_DOWNLOAD_URL: &str =
    "https://developer.microsoft.com/en-us/microsoft-edge/webview2/";

pub fn label_for(service_id: &str) -> String {
    format!("service-{service_id}")
}

// ---------------------------------------------------------------------------
// Last-loaded-URL registry (badge origin gate, round-2 finding N1)
// ---------------------------------------------------------------------------

/// Webview label -> last URL reported by its `on_page_load` hook. Written on
/// the event loop (where the hook runs with the URL in hand), read from
/// `report_title`. This exists because `Webview::url()` is a BLOCKING
/// channel getter — calling it from a sync command on the main thread
/// would self-deadlock — and because the webview LABEL never changes when
/// a page navigates, so the label check alone cannot stop off-origin badge
/// forgery.
static WEBVIEW_URLS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub fn record_webview_url(label: &str, url: &str) {
    WEBVIEW_URLS
        .lock()
        .map(|mut m| m.insert(label.to_string(), url.to_string()))
        .ok();
}

/// True when the webview's last loaded URL is on the service's home host
/// (www-insensitive). Unknown webview or unparsable URLs fail CLOSED.
pub fn webview_on_home_origin(label: &str, home_url: &str) -> bool {
    let current = match WEBVIEW_URLS.lock() {
        Ok(m) => match m.get(label) {
            Some(u) => u.clone(),
            None => return false,
        },
        Err(_) => return false,
    };
    hosts_match(&current, home_url)
}

fn strip_www(host: &str) -> &str {
    host.strip_prefix("www.").unwrap_or(host)
}

/// Page-initiated URLs may only ever reach the user's browser over http(s).
/// Everything else (file:, custom schemes, javascript:, about:) is dropped —
/// web content must not be able to launch protocol handlers.
fn external_open_allowed(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Opens a URL with the shell's default handler (the user's browser for
/// http/https). Direct ShellExecuteW FFI — see Cargo.toml note on why the
/// opener plugin was dropped.
#[cfg(windows)]
fn open_in_default_browser(url: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    // Safety: null parent HWND; wide-string args are valid for the call's
    // lifetime; "open" verb + http(s) URL validated by the caller.
    let verb: Vec<u16> = "open\0".encode_utf16().collect();
    let wide: Vec<u16> = format!("{url}\0").encode_utf16().collect();
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecuteW returns >32 on success.
    if result as usize > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW failed with code {}", result as isize))
    }
}

#[cfg(target_os = "linux")]
fn open_in_default_browser(url: &str) -> Result<(), String> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("failed to run xdg-open: {e}"))
}

#[cfg(all(not(windows), not(target_os = "linux")))]
fn open_in_default_browser(url: &str) -> Result<(), String> {
    let _ = url;
    Err("external links unsupported on this platform".to_string())
}

/// Shared profile for in-app link windows (`settings.linkTarget == "bzdium"`).
/// wry requires distinct `data_directory`s only when the WebView2 environment
/// *options* differ (see wry's `web_context.rs`); ours are identical for every
/// webview, so a single shared profile is safe, keeps logins across link
/// windows, and cannot grow without bound the way a per-window profile would.
const LINKS_DIR: &str = "links";

/// Last URL a service webview reported through `on_page_load`.
fn current_webview_url(label: &str) -> Option<String> {
    WEBVIEW_URLS.lock().ok().and_then(|m| m.get(label).cloned())
}

/// What to do with a page-initiated new window. `allow_in_app` is false for
/// link windows themselves, so a page opened *inside* bzdium always sends its
/// own popups to the real browser rather than nesting more windows.
fn handle_new_window(app: &AppHandle, url: &tauri::Url, allow_in_app: bool) {
    if !external_open_allowed(url.as_str()) {
        return;
    }
    let in_app = allow_in_app
        && app
            .state::<SharedState>()
            .lock()
            .map(|inner| inner.settings.link_target == crate::state::LinkTarget::Bzdium)
            .unwrap_or(false);
    if in_app {
        open_link_window(app, url);
    } else if let Err(e) = open_in_default_browser(url.as_str()) {
        crate::state::emit_notice(app, &format!("Could not open this link in your browser: {e}"));
    }
}

/// Opens `url` in its own bzdium window. The webview is created on a plain
/// thread rather than the message loop: webview creation pumps a nested
/// message loop, and this runs inside wry's `NewWindowRequested` dispatch
/// (see the threading contract in the design spec).
fn open_link_window(app: &AppHandle, url: &tauri::Url) {
    let handle = app.clone();
    let target = url.clone();
    std::thread::spawn(move || {
        let profile = match handle.state::<SharedState>().lock() {
            Ok(inner) => inner.data_dir.join(LINKS_DIR),
            Err(_) => {
                crate::state::emit_notice(
                    &handle,
                    "Could not open this link in bzdium: app state unavailable.",
                );
                return;
            }
        };
        let _ = fs::create_dir_all(&profile);
        let title = target.as_str().to_string();
        let sink = handle.clone();
        let built = tauri::WebviewWindowBuilder::new(
            &handle,
            format!("link-{}", uuid::Uuid::new_v4()),
            WebviewUrl::External(target),
        )
        .title(title)
        .inner_size(1024.0, 768.0)
        .data_directory(profile)
        .on_new_window(move |url, _features| {
            handle_new_window(&sink, &url, false);
            tauri::webview::NewWindowResponse::Deny
        })
        .build();
        if let Err(e) = built {
            crate::state::emit_notice(&handle, &format!("Could not open this link in bzdium: {e}"));
        }
    });
}

/// Opens the page a service is currently showing in the OS default browser
/// (context menu → "Open in browser"). Falls back to the service's configured
/// URL when its webview has not reported a load yet.
#[tauri::command]
pub fn open_service_external(state: State<'_, SharedState>, id: String) -> Result<(), String> {
    let (configured, label) = {
        let inner = state.lock().map_err(|e| e.to_string())?;
        let service = inner
            .service(&id)
            .ok_or_else(|| format!("unknown service id {id}"))?;
        (service.url.clone(), label_for(&id))
    };
    let url = current_webview_url(&label).unwrap_or(configured);
    if !external_open_allowed(&url) {
        return Err("only http(s) pages can be opened in the browser".to_string());
    }
    open_in_default_browser(&url)
}

fn hosts_match(a: &str, b: &str) -> bool {
    match (
        tauri::Url::parse(a.trim()),
        tauri::Url::parse(b.trim()),
    ) {
        (Ok(a), Ok(b)) => match (a.host_str(), b.host_str()) {
            (Some(a), Some(b)) => {
                strip_www(a).eq_ignore_ascii_case(strip_www(b))
            }
            _ => false,
        },
        _ => false,
    }
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

    let (position, size) = service_bounds(app, &window)?;
    // `on_new_window` runs inside a wry callback, so it has no command return
    // value to carry an error back to the UI. Clone the handle to report a
    // failed hand-off as a toast — release builds have no console, where an
    // `eprintln!` would be invisible.
    let notice_sink = app.clone();
    let builder = tauri::webview::WebviewBuilder::<Wry>::new(label, WebviewUrl::External(url))
        .data_directory(session_dir)
        .initialization_script(badges::init_script(&service.id))
        .on_page_load(|webview, payload| {
            // Drives the URL bar and feeds the badge origin gate.
            // Deliberately lock-free: label + url only.
            let owner = webview.app_handle().clone();
            let label = webview.label().to_string();
            record_webview_url(&label, payload.url().as_str());
            if let Some(id) = label.strip_prefix("service-") {
                crate::state::emit_url_changed(&owner, id, payload.url().as_str());
            }
        })
        .on_new_window(move |url, _features| {
            // External links (target=_blank / window.open): http(s) goes to
            // the default browser, or to a bzdium window when the user chose
            // that in Settings; every other scheme is dropped and the page
            // itself can never create an in-app popup.
            handle_new_window(&notice_sink, &url, true);
            tauri::webview::NewWindowResponse::Deny
        })
        .focused(visible);

    // Windows equivalent of Linux's TlsErrorsPolicy::Ignore (linux_layout.rs).
    // wry REPLACES its default WebView2 args when additional_browser_args is
    // set, so its defaults are repeated here. Scope is safe: WebView2 runs one
    // browser process per data directory, every service already gets its own
    // session dir, and link windows share a different profile — the switch
    // can only relax TLS for services that explicitly opted in.
    #[cfg(windows)]
    let builder = if service.ignore_tls_errors {
        builder.additional_browser_args(
            "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection \
             --ignore-certificate-errors",
        )
    } else {
        builder
    };
    let webview = window
        .add_child(builder, position, size)
        .map_err(webview_error)?;

    #[cfg(target_os = "linux")]
    {
        crate::linux_layout::attach_service(&window, &webview, position, size, service.ignore_tls_errors)?;
    }

    if visible {
        let _ = webview.set_focus();
    } else {
        let _ = webview.hide();
    }
    Ok(())
}

/// Top offset of the service webview area: below the URL bar when the
/// setting is on, flush otherwise. Brief state read — safe on any thread.
fn content_top_offset(app: &AppHandle) -> f64 {
    let state: tauri::State<'_, SharedState> = app.state();
    state
        .lock()
        .map(|inner| if inner.settings.show_url_bar { URL_BAR_HEIGHT } else { 0.0 })
        .unwrap_or(0.0)
}

/// Position and size (logical px) a service webview should occupy:
/// right of the sidebar and, when the URL bar is visible, below it.
fn service_bounds(app: &AppHandle, window: &tauri::Window) -> Result<(LogicalPosition<f64>, LogicalSize<f64>), String> {
    let (width, height) = content_size_logical(window)?;
    let offset_y = content_top_offset(app);
    Ok((
        LogicalPosition::new(SIDEBAR_WIDTH, offset_y),
        LogicalSize::new(
            (width - SIDEBAR_WIDTH).max(1.0),
            (height - offset_y).max(1.0),
        ),
    ))
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
    let (position, size) = service_bounds(app, &window)?;
    for webview in window.webviews() {
        let label = webview.label().to_string();
        if !label.starts_with("service-") {
            continue;
        }
        let is_active = active_id == Some(&label["service-".len()..]);
        if is_active {
            webview.set_position(position).map_err(|e| e.to_string())?;
            webview.set_size(size).map_err(|e| e.to_string())?;
            webview.show().map_err(|e| e.to_string())?;
            let _ = webview.set_focus();
        } else {
            webview.hide().map_err(|e| e.to_string())?;
        }
    }
    #[cfg(target_os = "linux")]
    crate::linux_layout::sync_all_bounds(&window, position, size, active_id)?;
    Ok(())
}

/// Re-applies the geometry contract to every open service webview
/// (called on window resize and after URL-bar toggles).
pub fn sync_bounds(app: &AppHandle) {
    let Some(window) = app.get_window(crate::state::UI_WEBVIEW_LABEL) else {
        return;
    };
    let Ok((position, size)) = service_bounds(app, &window) else {
        return;
    };
    for webview in window.webviews() {
        if !webview.label().starts_with("service-") {
            continue;
        }
        let _ = webview.set_position(position);
        let _ = webview.set_size(size);
    }
    #[cfg(target_os = "linux")]
    {
        let active_id = {
            let state: tauri::State<'_, SharedState> = app.state();
            state.lock().ok().and_then(|i| i.active_service_id.clone())
        };
        let _ = crate::linux_layout::sync_all_bounds(&window, position, size, active_id.as_deref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_matching_rules() {
        assert!(hosts_match("https://web.whatsapp.com/", "https://web.whatsapp.com"));
        assert!(hosts_match("https://www.instagram.com/x", "https://instagram.com"));
        assert!(hosts_match(
            "https://web.telegram.org/k/",
            "https://web.telegram.org/"
        ));
        // off-origin navigation is rejected (round-2 N1 core case)
        assert!(!hosts_match("https://evil.example/", "https://web.whatsapp.com/"));
        // lookalike suffixes must not pass
        assert!(!hosts_match("https://web.whatsapp.com.evil.example/", "https://web.whatsapp.com/"));
        assert!(!hosts_match("not a url", "https://web.whatsapp.com/"));
        assert!(!hosts_match("https://web.whatsapp.com/", "not a url"));
    }

    #[test]
    fn external_open_scheme_guard() {
        assert!(external_open_allowed("https://github.com/BZcreativ/bzdium"));
        assert!(external_open_allowed("http://example.com/x"));
        // never hand page-supplied non-http schemes to the OS
        assert!(!external_open_allowed("file:///C:/Windows/system32/evil.exe"));
        assert!(!external_open_allowed("ms-settings:privacy"));
        assert!(!external_open_allowed("javascript:alert(1)"));
        assert!(!external_open_allowed("about:blank"));
        assert!(!external_open_allowed(""));
    }

    #[test]
    fn origin_gate_fails_closed_for_unknown_webviews() {
        assert!(!webview_on_home_origin("service-unknown", "https://web.whatsapp.com/"));
    }

    #[test]
    fn origin_gate_tracks_recorded_urls() {
        record_webview_url("service-test-a", "https://web.whatsapp.com/");
        assert!(webview_on_home_origin("service-test-a", "https://web.whatsapp.com/"));
        assert!(webview_on_home_origin("service-test-a", "https://www.web.whatsapp.com/"));
        // navigation away from the service host stops badge reporting
        record_webview_url("service-test-a", "https://accounts.google.com/");
        assert!(webview_on_home_origin("service-test-a", "https://accounts.google.com/"));
        assert!(!webview_on_home_origin("service-test-a", "https://web.whatsapp.com/"));
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
        if !service.enabled {
            return Err(format!("{} is disabled — enable it in Edit", service.name));
        }
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
            // State may be persisted by activate before its webview op
            // fails — emit the fresh state either way so the UI matches
            // disk, then surface the error as a toast (N10 pattern).
            if let Err(e) = activate(&app, &state, &id) {
                eprintln!("set_active_service: activation failed: {e}");
                if let Ok(inner) = state.lock() {
                    let snapshot = inner.snapshot();
                    drop(inner);
                    emit_snapshot(&app, &snapshot);
                }
                return Err(e);
            }
            let snapshot = state.lock().map_err(|e| e.to_string())?.snapshot();
            emit_snapshot(&app, &snapshot);
            Ok(snapshot)
        }
        None => {
            let snapshot = {
                let mut inner = state.lock().map_err(|e| e.to_string())?;
                inner.active_service_id = None;
                inner.snapshot()
            };
            if let Err(e) = show_only(&app, None) {
                eprintln!("set_active_service: hide-all failed: {e}");
                emit_snapshot(&app, &snapshot);
                return Err(e);
            }
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

/// Developer mode: opens DevTools on a webview — `Some(service id)` for that
/// service's page, `None` for the UI webview itself. Async per the locking
/// contract (webview op); requires the tauri `devtools` feature, which also
/// enables it in release builds.
#[tauri::command]
pub async fn open_devtools(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: Option<String>,
) -> Result<(), String> {
    let dev_mode = state
        .lock()
        .map_err(|e| e.to_string())?
        .settings
        .dev_mode;
    if !dev_mode {
        return Err("developer mode is disabled".to_string());
    }
    let label = match id {
        Some(id) => label_for(&id),
        None => crate::state::UI_WEBVIEW_LABEL.to_string(),
    };
    let webview = app
        .get_webview(&label)
        .ok_or_else(|| format!("webview {label} not found"))?;
    webview.open_devtools();
    Ok(())
}

/// Navigates a service webview to an explicit https URL (URL bar "Go").
#[tauri::command]
pub async fn navigate_url(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
    url: String,
) -> Result<(), String> {
    let url = crate::services::validate_https_url(&url)?;
    let (service, data_dir, is_active) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let service = inner
            .service(&id)
            .cloned()
            .ok_or_else(|| format!("unknown service id {id}"))?;
        let data_dir = inner.data_dir.clone();
        inner.touch(&id);
        let is_active = inner.active_service_id.as_deref() == Some(id.as_str());
        (service, data_dir, is_active)
    };
    if app.get_webview(&label_for(&id)).is_none() {
        create_service_webview(&app, &service, &data_dir, is_active)?;
    }
    if let Some(webview) = app.get_webview(&label_for(&id)) {
        let parsed = url.parse().map_err(|e| format!("invalid URL: {e}"))?;
        webview.navigate(parsed).map_err(|e| e.to_string())?;
    }
    Ok(())
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
    if let Err(e) = close_webview(&app, &id) {
        eprintln!("hibernate_service: webview close failed: {e}");
        emit_snapshot(&app, &snapshot);
        return Err(e);
    }
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
    if let Err(e) = create_service_webview(&app, &service, &data_dir, is_active) {
        eprintln!("wake_service: webview create failed: {e}");
        emit_snapshot(&app, &snapshot);
        return Err(e);
    }
    if is_active {
        if let Err(e) = show_only(&app, Some(&id)) {
            eprintln!("wake_service: show failed: {e}");
            emit_snapshot(&app, &snapshot);
            return Err(e);
        }
    }
    emit_snapshot(&app, &snapshot);
    Ok(snapshot)
}
