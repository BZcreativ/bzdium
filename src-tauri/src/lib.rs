mod autostart;
mod badges;
mod hibernate;
mod services;
mod state;
mod storage;
mod tray;
mod webviews;

use std::sync::Mutex;

use tauri::{AppHandle, Manager, State, WindowEvent};

use state::{emit_snapshot, AppState, Settings, SharedState, StateInner};

#[tauri::command]
fn get_state(state: State<'_, SharedState>) -> Result<AppState, String> {
    let inner = state.lock().map_err(|e| e.to_string())?;
    Ok(inner.snapshot())
}

#[tauri::command]
fn get_settings(state: State<'_, SharedState>) -> Result<Settings, String> {
    let inner = state.lock().map_err(|e| e.to_string())?;
    Ok(inner.settings.clone())
}

#[tauri::command]
fn update_settings(
    app: AppHandle,
    state: State<'_, SharedState>,
    settings: Settings,
) -> Result<AppState, String> {
    let mut inner = state.lock().map_err(|e| e.to_string())?;
    inner.settings = settings;
    inner.save_settings()?;
    autostart::apply(inner.settings.start_with_windows)?;
    let snapshot = inner.snapshot();
    drop(inner);
    // The URL-bar toggle changes service webview geometry — re-apply it.
    webviews::sync_bounds(&app);
    emit_snapshot(&app, &snapshot);
    Ok(snapshot)
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
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .manage(Mutex::new(initial_state))
        .invoke_handler(tauri::generate_handler![
            get_state,
            get_settings,
            update_settings,
            services::get_recipes,
            services::add_service,
            services::update_service,
            services::remove_service,
            services::reorder_services,
            services::export_config,
            services::import_config,
            webviews::set_active_service,
            webviews::reload_service,
            webviews::navigate,
            webviews::navigate_url,
            webviews::hibernate_service,
            webviews::wake_service,
            webviews::set_overlay_mode,
            badges::report_title,
        ])
        .setup(|app| {
            tray::setup_tray(app)?;
            hibernate::start_hibernation_task(app.handle().clone());

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
            #[cfg(debug_assertions)]
            if let Some(ui) = app.get_webview(state::UI_WEBVIEW_LABEL) {
                ui.open_devtools();
            }
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
                services.first().map(|s| s.id.clone())
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
