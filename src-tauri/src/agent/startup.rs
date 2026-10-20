//! Agent Access Layer startup and lifecycle.
//!
//! The layer is OFF unless a human enables it. When enabled, this module
//! starts a loopback-only HTTP server on a background thread with its own
//! tokio runtime, so it can never block the Tauri event loop (the same
//! constraint the webview code documents at length).
//!
//! Two hard rules, both driven by the app's own security model:
//!   1. Never bind a routable interface. 127.0.0.1 only.
//!   2. Never start when no token exists. An unauthenticated listener would
//!      be reachable by every local process AND by remote pages running in
//!      the service webviews.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::agent::{self, AgentConfig, AgentCtx};

/// Guards against a double start when the user toggles the setting repeatedly.
static RUNNING: AtomicBool = AtomicBool::new(false);

/// Starts the agent HTTP server if (and only if) it is enabled in config.
/// Called from setup, after the window exists. Failure to start is logged but
/// never fatal: the desktop app must keep working.
pub fn start_if_enabled(app: &tauri::AppHandle) {
    let data_dir = data_dir_of(app);
    let cfg = agent::load_config(&data_dir);

    // Two independent switches must agree before anything listens:
    //   * settings.json agentAccess - what the human sees in Settings
    //   * agent/config.json enabled - what the layer obeys
    // agent::wire::sync_from_settings keeps them in step, but a config written
    // by an older build (or edited by hand) could disagree. Requiring both is
    // the conservative reading, and it fails closed on either side.
    let settings_on = {
        use tauri::Manager as _;
        let state = app.state::<crate::state::SharedState>();
        let inner = state.lock().expect("state mutex poisoned");
        inner.settings.agent_access
    };

    if !cfg.enabled || !settings_on {
        if cfg.enabled && !settings_on {
            // The layer was enabled out-of-band but the user's checkbox is off.
            // Do not listen; say why, once, so the state is diagnosable.
            eprintln!(
                "agent: config enables the layer but Settings has agent access off; not starting"
            );
        }
        agent::http::set_enabled(false);
        return;
    }

    start(app, cfg);
}

fn data_dir_of(app: &tauri::AppHandle) -> std::path::PathBuf {
    use tauri::Manager as _;
    let state = app.state::<crate::state::SharedState>();
    let inner = state.lock().expect("state mutex poisoned");
    inner.data_dir.clone()
}

/// Spawns the listener. Idempotent per process.
pub fn start(app: &tauri::AppHandle, config: AgentConfig) {
    if RUNNING.swap(true, Ordering::SeqCst) {
        // Already bound: the switch was merely flipped on again, so just
        // re-open the request gate instead of binding a second listener.
        agent::http::set_enabled(true);
        eprintln!("agent: already running");
        return;
    }
    agent::http::set_enabled(true);
    let ctx = Arc::new(AgentCtx::new(app.clone()));

    std::thread::Builder::new()
        .name("bzdium-agent-http".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    eprintln!("agent: cannot build tokio runtime: {e}");
                    RUNNING.store(false, Ordering::SeqCst);
                    return;
                }
            };
            runtime.block_on(async move {
                match agent::http::serve(ctx, config).await {
                    Ok(port) => {
                        eprintln!("agent: listening on http://127.0.0.1:{port}/api/v1/agent");
                        // serve() binds, writes the port file, and hands the
                        // accept loop to tokio::spawn, then returns. If this
                        // async block ended here, block_on would return, the
                        // Runtime would be dropped at the end of its scope, and
                        // dropping a runtime cancels every task it owns - so the
                        // listener would close microseconds after reporting
                        // success. The port file would point at a dead socket.
                        // Park instead, keeping the runtime (and the accept
                        // loop) alive for the life of the process.
                        std::future::pending::<()>().await;
                    }
                    Err(e) => {
                        eprintln!("agent: failed to start: {e}");
                        RUNNING.store(false, Ordering::SeqCst);
                    }
                }
            });
        })
        .ok();
}

/// True when the listener is up in this process.
pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

/// Applies a live enable/disable from Settings.
///
/// Enabling binds the listener the first time, then merely re-opens the request
/// gate on later toggles. Disabling leaves the socket bound but closes the gate,
/// which is instant, needs no runtime teardown, and cannot leave the app in a
/// half-shut state. The port is released when the process exits.
pub fn apply_enabled(app: &tauri::AppHandle, enabled: bool) {
    if enabled {
        let data_dir = data_dir_of(app);
        let cfg = agent::load_config(&data_dir);
        if cfg.enabled {
            start(app, cfg);
        } else {
            // Settings said on but config says off: refuse to listen rather
            // than start a layer whose own master switch is false.
            eprintln!("agent: enable requested but config is disabled; not starting");
            agent::http::set_enabled(false);
        }
    } else {
        agent::http::set_enabled(false);
    }
}
