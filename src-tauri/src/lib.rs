pub mod agent;
mod autostart;
mod badges;
mod hibernate;
mod services;
mod state;
mod storage;
mod tray;
mod webviews;
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
    autostart::apply(inner.settings.start_with_windows)?;
    let agent_access = inner.settings.agent_access;
    let data_dir = inner.data_dir.clone();
    let snapshot = inner.snapshot();
    drop(inner);
    // The URL-bar toggle changes service webview geometry — re-apply it.
    webviews::sync_bounds(&app);
    emit_snapshot(&app, &snapshot);

    // Mirror the checkbox into the layer's own config and mint a first token
    // when the layer is switched on without one. Without this the checkbox
    // would write settings.json while startup reads agent/config.json, and the
    // listener would never bind.
    let current_admin = agent::load_config(&data_dir).allow_admin_scope;
    if let Err(e) = agent::wire::sync_from_settings(&data_dir, agent_access, current_admin) {
        eprintln!("agent: cannot persist agent config: {e}");
    }
    if agent_access {
        match agent::wire::ensure_first_token(&data_dir, current_admin) {
            Ok(Some(token)) => {
                // Shown once in the UI; never logged, never retrievable again.
                use tauri::Emitter as _;
                app.emit("agent-token-created", token).ok();
            }
            Ok(None) => {}
            Err(e) => eprintln!("agent: cannot mint the first token: {e}"),
        }
    }
    agent::startup::apply_enabled(&app, agent_access);

    Ok(snapshot)
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
    let initial_state = StateInner::new(data_dir);

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
            webviews::navigate,
            webviews::navigate_url,
            webviews::hibernate_service,
            webviews::wake_service,
            webviews::set_overlay_mode,
            webviews::open_devtools,
            webviews::open_service_external,
            get_agent_status,
            set_agent_admin_scope,
            mint_agent_token,
            revoke_agent_tokens,
            badges::report_title,
        ])
        .setup(|app| {
            tray::setup_tray(app)?;
            hibernate::start_hibernation_task(app.handle().clone());

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
