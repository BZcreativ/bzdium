use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::state::{emit_state_changed, SharedState};
use crate::webviews;

/// How often the hibernation task scans for idle services.
const SCAN_INTERVAL: Duration = Duration::from_secs(30);

/// Starts the background hibernation loop. Every scan, services whose
/// webview has been idle for longer than `settings.hibernation_minutes`
/// (0 = disabled) and that are not the active service are hibernated:
/// their webview is closed (freeing memory) and the service is flagged.
/// Waking recreates the webview at the service URL (see webviews.rs).
///
/// The state mutex is never held across webview calls: closing a webview
/// can dispatch to the main thread, and any main-thread command waiting on
/// the same mutex would deadlock the app.
pub fn start_hibernation_task(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(SCAN_INTERVAL);

        // Phase 1 (locked): decide what to hibernate and record it.
        let idle = {
            let state = app.state::<SharedState>();
            let mut inner = match state.lock() {
                Ok(inner) => inner,
                Err(_) => continue,
            };
            let minutes = inner.settings.hibernation_minutes;
            if minutes == 0 {
                continue;
            }
            let limit = Duration::from_secs(minutes.saturating_mul(60));
            let active = inner.active_service_id.clone();
            let idle: Vec<String> = inner
                .services
                .iter()
                .filter(|s| !s.hibernated && active.as_deref() != Some(s.id.as_str()))
                .filter(|s| {
                    inner
                        .last_active
                        .get(&s.id)
                        .map(|t| t.elapsed() >= limit)
                        .unwrap_or(true)
                })
                .map(|s| s.id.clone())
                .collect();
            for id in &idle {
                if let Some(service) = inner.service_mut(id) {
                    service.hibernated = true;
                }
                inner.last_active.remove(id);
            }
            if idle.is_empty() {
                continue;
            }
            if let Err(e) = inner.save_services() {
                eprintln!("hibernation: failed to save services: {e}");
            }
            idle
        };

        // Phase 2 (unlocked): actually close the webviews.
        for id in &idle {
            if let Err(e) = webviews::close_webview(&app, id) {
                eprintln!("hibernation: failed to close webview for {id}: {e}");
            }
        }

        // Phase 3 (locked): notify the UI.
        {
            let state = app.state::<SharedState>();
            if let Ok(inner) = state.lock() {
                emit_state_changed(&app, &inner);
            };
        }
    });
}
