pub mod agent;
mod autostart;
mod badges;
pub mod grok;
mod hibernate;
mod services;
mod state;
mod storage;
mod tray;
mod webviews;
mod watchdog;
mod webview2_runtime;
#[cfg(target_os = "linux")]
pub mod linux_layout;

use std::sync::Mutex;

use tauri::{AppHandle, Manager, State, WindowEvent};

use state::{emit_snapshot, AppState, Settings, SharedState, StateInner};

#[tauri::command]
fn get_state(state: State<'_, SharedState>) -> Result<AppState, String> {
    let inner = state.lock().map_err(|e| e.to_string())?;
    Ok(inner.snapshot())
}

/// True when running in portable layout (data dir sits next to the exe).
/// Autostart is refused in that mode: the HKCU Run entry would point at a
/// movable, user-replaceable binary (round-2 N4).
fn is_portable_layout(data_dir: &std::path::Path) -> bool {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|p| p.join(storage::PORTABLE_DIR_NAME)))
        .is_some_and(|portable| portable == data_dir)
}

#[tauri::command]
fn update_settings(
    app: AppHandle,
    state: State<'_, SharedState>,
    settings: Settings,
) -> Result<AppState, String> {
    // A pinned WebView2 runtime must be a real runtime folder under a known
    // Evergreen root or the data-dir sidecar root; refuse to persist anything
    // the startup path would silently ignore (see run()) — the loader
    // executes from this folder.
    if let Some(folder) = settings.webview2_runtime_folder.as_deref() {
        let data_dir = state
            .lock()
            .map_err(|e| e.to_string())?
            .data_dir
            .clone();
        if !webview2_runtime::folder_is_allowed(folder, &data_dir) {
            return Err(format!(
                "'{folder}' is not a WebView2 runtime folder (msedgewebview2.exe not found under a known install root or {}/).",
                webview2_runtime::SIDECAR_DIR
            ));
        }
    }
    let mut inner = state.lock().map_err(|e| e.to_string())?;
    let portable = is_portable_layout(&inner.data_dir);
    if settings.start_with_windows && portable {
        // Do not persist an unfulfillable promise: clear the flag before saving.
        let mut settings = settings;
        settings.start_with_windows = false;
        inner.settings = settings;
        inner.save_settings()?;
        let snapshot = inner.snapshot();
        drop(inner);
        webviews::sync_bounds(&app);
        emit_snapshot(&app, &snapshot);
        return Err(
            "Start with Windows is only available for installed copies — a portable exe is movable and user-replaceable.".to_string(),
        );
    }
    inner.settings = settings;
    inner.save_settings()?;
    let agent_access = inner.settings.agent_access;
    let start_with_windows = inner.settings.start_with_windows;
    let data_dir = inner.data_dir.clone();
    let snapshot = inner.snapshot();
    drop(inner);
    // The URL-bar toggle changes service webview geometry — re-apply it, and
    // emit BEFORE any fallible post-persist step (autostart's registry write
    // below): the settings are already on disk, so an error after them must
    // not leave the UI showing stale state.
    webviews::sync_bounds(&app);
    emit_snapshot(&app, &snapshot);

    sync_agent_layer(&app, &data_dir, agent_access);

    // Registry write goes last: failure here is reported to the caller, but
    // the snapshot above already reflects the saved settings.
    autostart::apply(start_with_windows)?;

    Ok(snapshot)
}

/// Post-persist side effects that must follow EVERY change of
/// `settings.agent_access` (update_settings and import_config). Mirrors the
/// checkbox into the layer's own config, mints a first token when the layer
/// is switched on without one, and opens/closes the runtime gate. Without
/// this, an imported config could show Agent Access off while the loopback
/// listener keeps serving existing bearer tokens (or vice versa).
fn sync_agent_layer(app: &AppHandle, data_dir: &std::path::Path, agent_access: bool) {
    let current_admin = agent::load_config(data_dir).allow_admin_scope;
    if let Err(e) = agent::wire::sync_from_settings(data_dir, agent_access, current_admin) {
        eprintln!("agent: cannot persist agent config: {e}");
    }
    if agent_access {
        match agent::wire::ensure_first_token(data_dir, current_admin) {
            Ok(Some(token)) => {
                // Shown once in the UI; never logged, never retrievable again.
                use tauri::Emitter as _;
                app.emit("agent-token-created", token).ok();
            }
            Ok(None) => {}
            Err(e) => eprintln!("agent: cannot mint the first token: {e}"),
        }
    }
    agent::startup::apply_enabled(app, agent_access);
}

// ---------------------------------------------------------------------------
// Agent Access commands
//
// These are the only commands that touch the layer's own credential store.
// They are deliberately separate from update_settings: minting a credential and
// granting admin scope are privilege operations, not preferences, and the UI
// asks for them explicitly.
// ---------------------------------------------------------------------------

/// Current layer state for the Settings panel. Never contains a token value.
#[tauri::command]
fn get_agent_status(state: State<'_, SharedState>) -> Result<agent::wire::AgentStatus, String> {
    let inner = state.lock().map_err(|e| e.to_string())?;
    Ok(agent::wire::status(&inner.data_dir))
}

/// Grants or revokes the admin ceiling. Admin scope unlocks the three
/// irreversible tools, so this is a distinct, explicit switch.
#[tauri::command]
fn set_agent_admin_scope(
    state: State<'_, SharedState>,
    allow: bool,
) -> Result<agent::wire::AgentStatus, String> {
    let (data_dir, agent_access) = {
        let inner = state.lock().map_err(|e| e.to_string())?;
        (inner.data_dir.clone(), inner.settings.agent_access)
    };
    agent::wire::sync_from_settings(&data_dir, agent_access, allow)?;
    Ok(agent::wire::status(&data_dir))
}

/// Mints an extra token and returns it once. The token is persisted before it
/// is returned, so a crash between the two cannot hand out a token the layer
/// will later reject.
#[tauri::command]
fn mint_agent_token(
    state: State<'_, SharedState>,
    scope: String,
    label: String,
) -> Result<String, String> {
    let parsed = agent::scope::Scope::parse(&scope)
        .ok_or_else(|| format!("unknown scope '{scope}': use read, write or admin"))?;
    let data_dir = {
        let inner = state.lock().map_err(|e| e.to_string())?;
        inner.data_dir.clone()
    };
    let label = if label.trim().is_empty() {
        "Created from Settings".to_string()
    } else {
        label.trim().to_string()
    };
    agent::wire::mint_additional_token(&data_dir, parsed, &label)
}

/// Deletes every token. The layer stays bound but authenticates nobody.
#[tauri::command]
fn revoke_agent_tokens(state: State<'_, SharedState>) -> Result<usize, String> {
    let data_dir = {
        let inner = state.lock().map_err(|e| e.to_string())?;
        inner.data_dir.clone()
    };
    agent::wire::revoke_all_tokens(&data_dir)
}

pub fn run() {
    let data_dir = storage::resolve_data_dir();
    let initial_state = StateInner::new(data_dir.clone());

    // WebView2 runtime pin: must happen before the first webview exists,
    // i.e. before the Builder creates the main window. An invalid stored
    // folder (e.g. removed by the Evergreen updater) degrades to the system
    // runtime rather than failing startup.
    if let Some(folder) = initial_state.settings.webview2_runtime_folder.clone() {
        if webview2_runtime::apply_override(&folder, &data_dir) {
            eprintln!("webview2: pinning runtime folder {folder}");
        } else {
            eprintln!("webview2: ignoring invalid runtime folder {folder:?}");
        }
    }

    tauri::Builder::default()
        // Must stay the first plugin: a second launch (e.g. clicking the exe
        // again, or a second copy of the portable folder) must never run a
        // concurrent instance — two instances would share the UI webview's
        // WebView2 profile and corrupt each other's IPC (observed as a
        // full app hang). Instead: surface the existing window.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_window(state::UI_WEBVIEW_LABEL) {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
                webviews::restore_link_windows(app);
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .manage(Mutex::new(initial_state))
        .invoke_handler(tauri::generate_handler![
            get_state,
            update_settings,
            services::get_recipes,
            services::add_service,
            services::update_service,
            services::remove_service,
            services::reorder_services,
            services::export_config,
            services::import_config,
            services::pick_service_icon,
            services::reset_service_icon,
            services::get_service_icon,
            webviews::set_active_service,
            webviews::reload_service,
            webviews::clear_service_data,
            webviews::navigate,
            webviews::navigate_url,
            webviews::hibernate_service,
            webviews::wake_service,
            webviews::set_overlay_mode,
            webviews::open_devtools,
            webviews::open_service_external,
            webviews::open_url_external,
            get_agent_status,
            set_agent_admin_scope,
            mint_agent_token,
            revoke_agent_tokens,
            badges::report_title,
            watchdog::report_probe,
            webview2_runtime::list_webview2_runtimes,
            grok::grok_key_status,
            grok::grok_save_key,
            grok::grok_delete_key,
            grok::grok_test_connection,
            grok::grok_send_message,
            grok::grok_cancel_request,
            grok::grok_list_conversations,
            grok::grok_save_conversation,
            grok::grok_clear_history,
        ])
        .setup(|app| {
            tray::setup_tray(app)?;
            hibernate::start_hibernation_task(app.handle().clone());
            watchdog::start_watchdog_task(app.handle().clone());

            // Agent Access Layer: opt-in, loopback-only. Starts nothing unless
            // the user enabled it; see agent::startup for the security rules.
            agent::startup::start_if_enabled(app.handle());

            // Keep the registry Run key consistent with the persisted setting.
            let start_with_windows = {
                let state = app.state::<SharedState>();
                state
                    .lock()
                    .map(|inner| inner.settings.start_with_windows)
                    .unwrap_or(false)
            };
            if start_with_windows {
                let _ = autostart::apply(true);
            }

            // Window events: minimize-to-tray on close, webview bounds on resize.
            let window = app
                .get_window(state::UI_WEBVIEW_LABEL)
                .expect("main window must exist");

            #[cfg(target_os = "linux")]
            linux_layout::init_layout(&window).map_err(|e| e.to_string())?;

            let handle = app.handle().clone();
            window.on_window_event(move |event| match event {
                WindowEvent::CloseRequested { api, .. } => {
                    let minimize_to_tray = {
                        let state = handle.state::<SharedState>();
                        state
                            .lock()
                            .map(|inner| inner.settings.minimize_to_tray)
                            .unwrap_or(false)
                    };
                    if minimize_to_tray {
                        api.prevent_close();
                        if let Some(window) = handle.get_window(state::UI_WEBVIEW_LABEL) {
                            let _ = window.hide();
                        }
                        // Link windows are top-level: hide them too, or a
                        // "minimize to tray" would leave orphan windows on
                        // the desktop (round-3 finding 2).
                        webviews::hide_link_windows(&handle);
                    }
                }
                WindowEvent::Resized(_) => webviews::sync_bounds(&handle),
                _ => {}
            });

            // Activate the first service (by order) if any are configured.
            // This must NOT run on the main thread: webview creation inside
            // setup would block the event loop (same WebView2 constraint as
            // async commands). A plain thread dispatches the creation to the
            // event loop instead of blocking it.
            let first_id = {
                let state = app.state::<SharedState>();
                let inner = state.lock().map_err(|e| e.to_string())?;
                let mut services = inner.services.clone();
                services.sort_by_key(|s| s.order);
                services.iter().find(|s| s.enabled).map(|s| s.id.clone())
            };
            if let Some(id) = first_id {
                let app_handle = app.handle().clone();
                std::thread::spawn(move || {
                    let state = app_handle.state::<SharedState>();
                    // activate manages the state lock itself and keeps it
                    // off the webview-creation path.
                    if let Err(e) = webviews::activate(&app_handle, &state, &id) {
                        eprintln!("startup activation failed for {id}: {e}");
                    }
                });
            }

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running bzdium");
}
