//! The tool registry: the single source of truth for every agent capability.
//!
//! ZERO DUPLICATION. Each handler calls the same Rust function the UI invokes
//! over Tauri IPC. No domain rule is restated here: validation, the TLS-bypass
//! policy, id checks, persistence and webview orchestration all stay in
//! services.rs / webviews.rs / state.rs, so an agent and the UI can never
//! disagree about what is legal.
//!
//! Commands that take tauri::State are reached through the AppHandle via
//! Manager::state(), which yields exactly the State the command expects.

use serde_json::{json, Value};

use super::error::{AgentError, ErrorCode};
use super::guard::project_service_list;
use super::schema::{boolean, dry_run_prop, integer, object, props, string, string_array};
use super::scope::Scope;
use super::{AgentCtx, BoxFuture, ToolSpec};

use tauri::Manager as _;

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn arg_str(args: &Value, key: &str) -> Result<String, AgentError> {
    args.get(key)
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        .ok_or_else(|| {
            AgentError::invalid_parameter(
                key,
                format!("Missing or non-string parameter '{key}'."),
                format!("Provide '{key}' as a string. Call tools/list to see the schema for this tool."),
            )
        })
}

fn arg_opt_str(args: &Value, key: &str) -> Option<String> {
    args.get(key).and_then(Value::as_str).map(|s| s.to_string())
}

fn arg_bool(args: &Value, key: &str, default: bool) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn state_of(ctx: &AgentCtx) -> tauri::State<'_, crate::state::SharedState> {
    ctx.app.state::<crate::state::SharedState>()
}

fn snapshot_json(ctx: &AgentCtx) -> Result<Value, AgentError> {
    let state = state_of(ctx);
    let inner = state.lock().map_err(|e| AgentError::from_command(e.to_string()))?;
    serde_json::to_value(inner.snapshot())
        .map_err(|e| AgentError::new(ErrorCode::Internal, e.to_string(), "Retry the call."))
}

fn map_err(e: String) -> AgentError {
    AgentError::from_command(e)
}

// ---------------------------------------------------------------------------
// Tool handlers: read scope
// ---------------------------------------------------------------------------

fn h_list_services<'a>(ctx: &'a AgentCtx, _args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let state = state_of(ctx);
        let inner = state.lock().map_err(|e| map_err(e.to_string()))?;
        let mut services = inner.services.clone();
        services.sort_by_key(|s| s.order);
        Ok(json!({
            "services": services,
            "count": services.len(),
            "activeServiceId": inner.active_service_id,
        }))
    })
}

fn h_get_service<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let id = arg_str(&args, "id")?;
        let state = state_of(ctx);
        let inner = state.lock().map_err(|e| map_err(e.to_string()))?;
        let svc = inner
            .service(&id)
            .ok_or_else(|| AgentError::from_command(format!("unknown service id {id}")))?;
        Ok(serde_json::to_value(svc).unwrap_or(Value::Null))
    })
}

fn h_get_settings<'a>(ctx: &'a AgentCtx, _args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let state = state_of(ctx);
        let inner = state.lock().map_err(|e| map_err(e.to_string()))?;
        Ok(serde_json::to_value(&inner.settings).unwrap_or(Value::Null))
    })
}

fn h_get_state<'a>(ctx: &'a AgentCtx, _args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move { snapshot_json(ctx) })
}

fn h_list_recipes<'a>(_ctx: &'a AgentCtx, _args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let recipes = crate::services::recipes();
        Ok(json!({ "recipes": recipes, "count": recipes.len() }))
    })
}

fn h_get_service_icon<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let id = arg_str(&args, "id")?;
        let state = state_of(ctx);
        let data_url = crate::services::get_service_icon(state, id.clone()).map_err(map_err)?;
        Ok(json!({
            "id": id,
            // A custom icon is at most 256 KiB, so the data URL stays small.
            // The agent gets the same value the UI renders.
            "dataUrl": data_url,
            "hasCustomIcon": data_url.is_some(),
        }))
    })
}

fn h_list_sessions<'a>(ctx: &'a AgentCtx, _args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        // SECURITY: directory names and aggregate sizes ONLY. Session profiles
        // hold decrypted cookies and logins; their contents must never cross
        // this boundary, not even to a read-scoped token.
        let sessions_dir = ctx.data_dir.join(crate::storage::SESSIONS_DIR);
        let mut rows = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
            let mut items: Vec<_> = entries.flatten().collect();
            items.sort_by_key(|e| e.file_name());
            for entry in items {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                rows.push(json!({
                    "serviceId": entry.file_name().to_string_lossy(),
                    "bytesOnDisk": dir_size(&path),
                }));
            }
        }
        Ok(json!({ "sessions": rows, "count": rows.len() }))
    })
}

/// Sums file sizes in a directory tree. Skips anything unreadable rather than
/// failing the whole listing.
fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        match entry.file_type() {
            Ok(t) if t.is_dir() => total = total.saturating_add(dir_size(&p)),
            Ok(t) if t.is_file() => {
                total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0))
            }
            _ => {}
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Tool handlers: write scope
// ---------------------------------------------------------------------------

fn h_add_service<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let name = arg_str(&args, "name")?;
        let url = arg_str(&args, "url")?;
        let icon = arg_opt_str(&args, "icon").unwrap_or_default();
        let icon_id = arg_opt_str(&args, "iconId");
        let ignore_tls = args.get("ignoreTlsErrors").and_then(Value::as_bool);
        let state = state_of(ctx);
        let snap = crate::services::add_service(ctx.app.clone(), state, name, url, icon, icon_id, ignore_tls)
            .await
            .map_err(map_err)?;
        serde_json::to_value(snap)
            .map_err(|e| AgentError::new(ErrorCode::Internal, e.to_string(), "Retry the call."))
    })
}

fn h_update_service<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let id = arg_str(&args, "id")?;
        let name = arg_str(&args, "name")?;
        let url = arg_str(&args, "url")?;
        let icon = arg_opt_str(&args, "icon").unwrap_or_default();
        let enabled = arg_bool(&args, "enabled", true);
        let ignore_tls = args.get("ignoreTlsErrors").and_then(Value::as_bool);
        let state = state_of(ctx);
        let snap = crate::services::update_service(
            ctx.app.clone(), state, id, name, url, icon, enabled, ignore_tls,
        )
        .await
        .map_err(map_err)?;
        serde_json::to_value(snap)
            .map_err(|e| AgentError::new(ErrorCode::Internal, e.to_string(), "Retry the call."))
    })
}

/// Field-level settings merge.
///
/// The existing update_settings command replaces the whole struct, so an agent
/// that supplied one field would silently reset every other field to a
/// default. This reads the current settings and applies only the keys the
/// agent actually sent.
fn h_patch_settings<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let state = state_of(ctx);
        let current = {
            let inner = state.lock().map_err(|e| map_err(e.to_string()))?;
            inner.settings.clone()
        };
        let mut next = current;
        let mut changed: Vec<&str> = Vec::new();

        if let Some(v) = args.get("hibernationMinutes").and_then(Value::as_u64) {
            next.hibernation_minutes = v;
            changed.push("hibernationMinutes");
        }
        for (key, field) in [
            ("minimizeToTray", &mut next.minimize_to_tray),
            ("startWithWindows", &mut next.start_with_windows),
            ("darkUi", &mut next.dark_ui),
            ("showUrlBar", &mut next.show_url_bar),
            ("devMode", &mut next.dev_mode),
            ("autoRecover", &mut next.auto_recover),
        ] {
            if let Some(v) = args.get(key).and_then(Value::as_bool) {
                *field = v;
                changed.push(key);
            }
        }
        if let Some(v) = args.get("linkTarget").and_then(Value::as_str) {
            let target = match v {
                "browser" => crate::state::LinkTarget::Browser,
                "bzdium" => crate::state::LinkTarget::Bzdium,
                other => {
                    return Err(AgentError::invalid_parameter(
                        "linkTarget",
                        format!("Unknown linkTarget '{other}'."),
                        "Use \"browser\" to open links in the OS browser, or \"bzdium\" for an in-app window.",
                    ))
                }
            };
            next.link_target = target;
            changed.push("linkTarget");
        }
        if args.get("webview2RuntimeFolder").is_some() {
            // String pins a runtime folder (validated by update_settings),
            // explicit null clears the pin back to the system runtime.
            let folder = match args.get("webview2RuntimeFolder") {
                Some(Value::String(v)) => Some(v.clone()),
                Some(Value::Null) => None,
                _ => {
                    return Err(AgentError::invalid_parameter(
                        "webview2RuntimeFolder",
                        "webview2RuntimeFolder must be a runtime folder path or null.",
                        "Call list_webview2_runtimes from the UI capability, or pass null to clear the pin.",
                    ))
                }
            };
            if let Some(f) = folder.as_deref() {
                if !crate::webview2_runtime::folder_is_allowed(f) {
                    return Err(AgentError::invalid_parameter(
                        "webview2RuntimeFolder",
                        format!("'{f}' is not an installed WebView2 runtime under a known install root."),
                        "Use a path returned by the Settings 'WebView2 runtime' list; it takes effect on the next app start.",
                    ));
                }
            }
            next.webview2_runtime_folder = folder;
            changed.push("webview2RuntimeFolder");
        }
        if changed.is_empty() {
            return Err(AgentError::invalid_parameter(
                "settings",
                "No recognised settings fields were supplied.",
                "Supply at least one of: hibernationMinutes, minimizeToTray, startWithWindows, darkUi, showUrlBar, devMode, autoRecover, linkTarget, webview2RuntimeFolder.",
            ));
        }

        // update_settings is a synchronous command: it persists and emits
        // inline, so there is nothing to await.
        let snap = crate::update_settings(ctx.app.clone(), state, next).map_err(map_err)?;
        Ok(json!({ "settings": snap.settings, "patched": changed }))
    })
}

fn h_reorder_services<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let ids: Vec<String> = args
            .get("orderedIds")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default();
        if ids.is_empty() {
            return Err(AgentError::invalid_parameter(
                "orderedIds",
                "orderedIds must be a non-empty array of service ids.",
                "Call list_services to get the current ids, then send them in the order you want.",
            ));
        }
        let state = state_of(ctx);
        let snap = crate::services::reorder_services(ctx.app.clone(), state, ids).map_err(map_err)?;
        serde_json::to_value(snap)
            .map_err(|e| AgentError::new(ErrorCode::Internal, e.to_string(), "Retry the call."))
    })
}

fn h_set_active_service<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let id = arg_str(&args, "id")?;
        let state = state_of(ctx);
        let snap = crate::webviews::set_active_service(ctx.app.clone(), state, Some(id))
            .await
            .map_err(map_err)?;
        serde_json::to_value(snap)
            .map_err(|e| AgentError::new(ErrorCode::Internal, e.to_string(), "Retry the call."))
    })
}

fn h_reload_service<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let id = arg_str(&args, "id")?;
        let state = state_of(ctx);
        let snap = crate::webviews::reload_service(ctx.app.clone(), state, id)
            .await
            .map_err(map_err)?;
        serde_json::to_value(snap)
            .map_err(|e| AgentError::new(ErrorCode::Internal, e.to_string(), "Retry the call."))
    })
}

fn h_navigate<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let id = arg_str(&args, "id")?;
        let action = arg_str(&args, "action")?;
        let state = state_of(ctx);
        let snap = crate::webviews::navigate(ctx.app.clone(), state, id, action)
            .await
            .map_err(map_err)?;
        serde_json::to_value(snap)
            .map_err(|e| AgentError::new(ErrorCode::Internal, e.to_string(), "Retry the call."))
    })
}

fn h_navigate_url<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let id = arg_str(&args, "id")?;
        let url = arg_str(&args, "url")?;
        let state = state_of(ctx);
        crate::webviews::navigate_url(ctx.app.clone(), state, id.clone(), url)
            .await
            .map_err(map_err)?;
        Ok(json!({ "id": id, "status": "navigated" }))
    })
}

fn h_hibernate_service<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let id = arg_str(&args, "id")?;
        let state = state_of(ctx);
        let snap = crate::webviews::hibernate_service(ctx.app.clone(), state, id)
            .await
            .map_err(map_err)?;
        serde_json::to_value(snap)
            .map_err(|e| AgentError::new(ErrorCode::Internal, e.to_string(), "Retry the call."))
    })
}

fn h_wake_service<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let id = arg_str(&args, "id")?;
        let state = state_of(ctx);
        let snap = crate::webviews::wake_service(ctx.app.clone(), state, id)
            .await
            .map_err(map_err)?;
        serde_json::to_value(snap)
            .map_err(|e| AgentError::new(ErrorCode::Internal, e.to_string(), "Retry the call."))
    })
}

fn h_reset_service_icon<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let id = arg_str(&args, "id")?;
        let state = state_of(ctx);
        let snap = crate::services::reset_service_icon(ctx.app.clone(), state, id)
            .await
            .map_err(map_err)?;
        serde_json::to_value(snap)
            .map_err(|e| AgentError::new(ErrorCode::Internal, e.to_string(), "Retry the call."))
    })
}

// ---------------------------------------------------------------------------
// Tool handlers: admin scope
// ---------------------------------------------------------------------------

fn h_remove_service<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let id = arg_str(&args, "id")?;
        let state = state_of(ctx);
        let snap = crate::services::remove_service(ctx.app.clone(), state, id.clone())
            .await
            .map_err(map_err)?;
        Ok(json!({
            "removed": id,
            "services": snap.services,
            // State the irreversible part explicitly so an agent's transcript
            // records what it destroyed.
            "sessionProfileDeleted": true,
        }))
    })
}

fn h_export_config<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let path = arg_str(&args, "path")?;
        let state = state_of(ctx);
        let p = std::path::PathBuf::from(&path);
        let token = crate::services::export_config_to_path(&ctx.app, state, &p).map_err(map_err)?;
        Ok(json!({
            "path": path,
            "confirmationToken": token,
            "expiresInSeconds": crate::services::IMPORT_CONFIRM_WINDOW_SECS,
            "note": "Keep the confirmationToken and pass it to import_config within the window.",
        }))
    })
}

fn h_import_config<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let path = arg_str(&args, "path")?;
        let token = arg_str(&args, "confirmationToken")?;
        let state = state_of(ctx);
        let p = std::path::PathBuf::from(&path);
        // import_config_from_path is synchronous: it performs all reads and
        // persistence inline and only closes/recreates webviews afterwards.
        let outcome = crate::services::import_config_from_path(&ctx.app, state, &p, Some(&token))
            .map_err(map_err)?;
        serde_json::to_value(outcome)
            .map_err(|e| AgentError::new(ErrorCode::Internal, e.to_string(), "Retry the call."))
    })
}

// ---------------------------------------------------------------------------
// Dry-run previews
//
// Each preview runs the SAME validators the real handler does and then reports
// the projected effect without touching state.
// ---------------------------------------------------------------------------

fn p_add_service<'a>(_ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let name = arg_str(&args, "name")?;
        let url = arg_str(&args, "url")?;
        let validated = crate::services::validate_https_url(&url).map_err(map_err)?;
        let ignore_tls = arg_bool(&args, "ignoreTlsErrors", false);
        if ignore_tls && !crate::services::tls_bypass_allowed_for_url(&validated) {
            return Err(AgentError::from_command(
                "TLS verification can only be disabled for self-hosted services, not for a built-in public service".into(),
            ));
        }
        if name.trim().is_empty() {
            return Err(AgentError::invalid_parameter(
                "name",
                "The service name must not be empty.",
                "Provide a non-empty name, e.g. Work Slack.",
            ));
        }
        Ok(json!({
            "wouldAdd": {
                "name": name,
                "url": validated,
                "ignoreTlsErrors": ignore_tls,
                "brandIcon": crate::services::brand_slug_for_url(&validated),
            },
            "note": "Validation passed. The service and its webview were NOT created."
        }))
    })
}

fn p_update_service<'a>(_ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let url = arg_str(&args, "url")?;
        crate::services::validate_https_url(&url).map_err(map_err)?;
        Ok(json!({ "wouldUpdate": { "url": url }, "validated": true }))
    })
}

fn p_patch_settings<'a>(_ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let mut keys = Vec::new();
        for k in [
            "hibernationMinutes",
            "minimizeToTray",
            "startWithWindows",
            "darkUi",
            "showUrlBar",
            "devMode",
            "autoRecover",
            "linkTarget",
            "webview2RuntimeFolder",
        ] {
            if args.get(k).is_some() {
                keys.push(k);
            }
        }
        if keys.is_empty() {
            return Err(AgentError::invalid_parameter(
                "settings",
                "No recognised settings fields were supplied.",
                "Supply at least one of: hibernationMinutes, minimizeToTray, startWithWindows, darkUi, showUrlBar, devMode, autoRecover, linkTarget, webview2RuntimeFolder.",
            ));
        }
        Ok(json!({ "wouldPatch": keys }))
    })
}

fn p_reorder<'a>(_ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let ids = args.get("orderedIds").cloned().unwrap_or(Value::Null);
        Ok(json!({ "wouldReorderTo": ids }))
    })
}

fn p_remove_service<'a>(ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let id = arg_str(&args, "id")?;
        let projected = project_service_list(ctx, |services| {
            if let Some(pos) = services.iter().position(|s| s.id == id) {
                services.remove(pos);
            }
        });
        Ok(json!({
            "wouldRemove": id,
            "remaining": projected["count"],
            "irreversible": "The service session profile (saved logins) would be permanently deleted.",
        }))
    })
}

fn p_navigate<'a>(_ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let action = arg_str(&args, "action")?;
        if !matches!(action.as_str(), "back" | "forward" | "home") {
            return Err(AgentError::invalid_parameter(
                "action",
                format!("Unknown navigate action '{action}'."),
                "Use one of: back, forward, home.",
            ));
        }
        Ok(json!({ "wouldNavigate": action }))
    })
}

fn p_navigate_url<'a>(_ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let url = arg_str(&args, "url")?;
        let validated = crate::services::validate_https_url(&url).map_err(map_err)?;
        Ok(json!({ "wouldNavigateTo": validated }))
    })
}

/// Tools that only touch a webview's lifecycle share one trivial preview.
fn p_service_only<'a>(_ctx: &'a AgentCtx, args: Value) -> BoxFuture<'a, Result<Value, AgentError>> {
    Box::pin(async move {
        let id = arg_str(&args, "id")?;
        Ok(json!({ "wouldApplyToService": id }))
    })
}

// ---------------------------------------------------------------------------
// Schema builders
// ---------------------------------------------------------------------------

fn schema_id() -> Value {
    string("Service id (a UUID). Call list_services to discover valid ids.")
}

fn s_list_services() -> Value {
    object(props(vec![]), &[])
}
fn s_get_service() -> Value {
    object(props(vec![("id", schema_id())]), &["id"])
}
fn s_get_settings() -> Value {
    object(props(vec![]), &[])
}
fn s_get_state() -> Value {
    object(props(vec![]), &[])
}
fn s_list_recipes() -> Value {
    object(props(vec![]), &[])
}
fn s_get_service_icon() -> Value {
    object(props(vec![("id", schema_id())]), &["id"])
}
fn s_list_sessions() -> Value {
    object(props(vec![]), &[])
}

fn s_add_service() -> Value {
    object(
        props(vec![
            ("name", string("Display name for the service, e.g. \"Work Slack\". Must not be empty.")),
            (
                "url",
                string("Absolute https URL of the web app, e.g. https://web.whatsapp.com. Only https is accepted."),
            ),
            (
                "icon",
                string("1-2 character lettermark used when no brand icon exists, e.g. \"WS\". Optional."),
            ),
            (
                "iconId",
                string("Brand icon slug from list_recipes (e.g. \"whatsapp\"), or omit to auto-detect from the URL host."),
            ),
            (
                "ignoreTlsErrors",
                boolean("Trust invalid or self-signed TLS certificates. Only permitted for self-hosted hosts; refused for built-in public services."),
            ),
            ("dryRun", dry_run_prop()),
        ]),
        &["name", "url"],
    )
}

fn s_update_service() -> Value {
    object(
        props(vec![
            ("id", schema_id()),
            ("name", string("New display name. Must not be empty.")),
            ("url", string("New absolute https URL. Changing it rebuilds the webview.")),
            ("icon", string("1-2 character lettermark. Optional.")),
            ("enabled", boolean("Whether the service is enabled. Disabling closes its webview and clears the active tab. Defaults to true.")),
            (
                "ignoreTlsErrors",
                boolean("Trust invalid or self-signed TLS certificates. Only permitted for self-hosted hosts."),
            ),
            ("dryRun", dry_run_prop()),
        ]),
        &["id", "name", "url"],
    )
}

fn s_patch_settings() -> Value {
    object(
        props(vec![
            (
                "hibernationMinutes",
                integer("Minutes of inactivity before a service's webview is closed to free RAM. 0 disables hibernation.", Some(0), None),
            ),
            ("minimizeToTray", boolean("Minimize to the system tray instead of exiting on close.")),
            ("startWithWindows", boolean("Launch at login. Refused for portable copies, where the exe path is movable.")),
            ("darkUi", boolean("Use the dark theme.")),
            ("showUrlBar", boolean("Show the address bar above the service content.")),
            ("devMode", boolean("Surface DevTools entries in the UI for troubleshooting.")),
            ("autoRecover", boolean("Watchdog: hard-reload a service page that stopped responding while the machine is online.")),
            (
                "linkTarget",
                super::schema::string_enum(
                    "Where new-window links from service pages open.",
                    &["browser", "bzdium"],
                ),
            ),
            (
                "webview2RuntimeFolder",
                json!({
                    "description": "Windows: absolute path of a WebView2 runtime folder (must contain msedgewebview2.exe) to pin every webview to; null clears the pin. Takes effect on the next app start.",
                    "anyOf": [
                        { "type": "string" },
                        { "type": "null" }
                    ]
                }),
            ),
            ("dryRun", dry_run_prop()),
        ]),
        &[],
    )
}

fn s_reorder_services() -> Value {
    object(
        props(vec![
            (
                "orderedIds",
                string_array("All service ids in the desired order. Ids not listed keep their relative order."),
            ),
            ("dryRun", dry_run_prop()),
        ]),
        &["orderedIds"],
    )
}

fn s_set_active_service() -> Value {
    object(props(vec![("id", schema_id()), ("dryRun", dry_run_prop())]), &["id"])
}

fn s_reload_service() -> Value {
    object(props(vec![("id", schema_id()), ("dryRun", dry_run_prop())]), &["id"])
}

fn s_navigate() -> Value {
    object(
        props(vec![
            ("id", schema_id()),
            (
                "action",
                super::schema::string_enum(
                    "Navigation action: back, forward, or home (the service's configured URL).",
                    &["back", "forward", "home"],
                ),
            ),
            ("dryRun", dry_run_prop()),
        ]),
        &["id", "action"],
    )
}

fn s_navigate_url() -> Value {
    object(
        props(vec![
            ("id", schema_id()),
            ("url", string("Absolute https URL to load inside that service's isolated session.")),
            ("dryRun", dry_run_prop()),
        ]),
        &["id", "url"],
    )
}

fn s_hibernate_service() -> Value {
    object(props(vec![("id", schema_id()), ("dryRun", dry_run_prop())]), &["id"])
}

fn s_wake_service() -> Value {
    object(props(vec![("id", schema_id()), ("dryRun", dry_run_prop())]), &["id"])
}

fn s_reset_service_icon() -> Value {
    object(props(vec![("id", schema_id()), ("dryRun", dry_run_prop())]), &["id"])
}

fn s_remove_service() -> Value {
    object(props(vec![("id", schema_id()), ("dryRun", dry_run_prop())]), &["id"])
}

fn s_export_config() -> Value {
    object(
        props(vec![
            (
                "path",
                string("Absolute path to write the JSON config to. Contains services and settings only - never sessions or credentials."),
            ),
            ("dryRun", dry_run_prop()),
        ]),
        &["path"],
    )
}

fn s_import_config() -> Value {
    object(
        props(vec![
            ("path", string("Absolute path of a bzdium config JSON file to import. Maximum 2 MiB.")),
            (
                "confirmationToken",
                string("Token returned by export_config. Required: an import REPLACES all services and settings."),
            ),
            ("dryRun", dry_run_prop()),
        ]),
        &["path", "confirmationToken"],
    )
}

// ---------------------------------------------------------------------------
// The registry
// ---------------------------------------------------------------------------

pub static TOOLS: &[ToolSpec] = &[
    // ---- read -------------------------------------------------------------
    ToolSpec {
        name: "list_services",
        description: "List every configured service with its id, name, url, enabled/hibernated state and unread badge count, plus which service is currently active.",
        scope: Scope::Read,
        mutating: false,
        input_schema: s_list_services,
        handler: h_list_services,
        preview: None,
    },
    ToolSpec {
        name: "get_service",
        description: "Get one configured service by id.",
        scope: Scope::Read,
        mutating: false,
        input_schema: s_get_service,
        handler: h_get_service,
        preview: None,
    },
    ToolSpec {
        name: "get_settings",
        description: "Read the current application settings (hibernation, tray, theme, URL bar, developer mode, link target).",
        scope: Scope::Read,
        mutating: false,
        input_schema: s_get_settings,
        handler: h_get_settings,
        preview: None,
    },
    ToolSpec {
        name: "get_state",
        description: "Get the complete application snapshot: services, active service id, settings, total badge count and the data directory path.",
        scope: Scope::Read,
        mutating: false,
        input_schema: s_get_state,
        handler: h_get_state,
        preview: None,
    },
    ToolSpec {
        name: "list_recipes",
        description: "List the 60 built-in service recipes (name, url, lettermark, brand icon slug). Use these when adding a well-known messenger rather than typing a URL.",
        scope: Scope::Read,
        mutating: false,
        input_schema: s_list_recipes,
        handler: h_list_recipes,
        preview: None,
    },
    ToolSpec {
        name: "get_service_icon",
        description: "Get a service's custom icon as a data URL, or null when it has none and renders a lettermark instead.",
        scope: Scope::Read,
        mutating: false,
        input_schema: s_get_service_icon,
        handler: h_get_service_icon,
        preview: None,
    },
    ToolSpec {
        name: "list_sessions",
        description: "List each service's on-disk session profile size in bytes. Names and sizes only: profile contents (cookies, logins) are never exposed.",
        scope: Scope::Read,
        mutating: false,
        input_schema: s_list_sessions,
        handler: h_list_sessions,
        preview: None,
    },
    // ---- write ------------------------------------------------------------
    ToolSpec {
        name: "add_service",
        description: "Add a new service and open its isolated session webview. The URL must be https. TLS bypass is refused for built-in public services.",
        scope: Scope::Write,
        mutating: true,
        input_schema: s_add_service,
        handler: h_add_service,
        preview: Some(p_add_service),
    },
    ToolSpec {
        name: "update_service",
        description: "Update a service's name, url, icon or enabled flag. Changing the url rebuilds its webview; disabling closes it and clears the active tab.",
        scope: Scope::Write,
        mutating: true,
        input_schema: s_update_service,
        handler: h_update_service,
        preview: Some(p_update_service),
    },
    ToolSpec {
        name: "patch_settings",
        description: "Update only the settings fields you supply. Unlike a full replacement, unspecified fields keep their current values.",
        scope: Scope::Write,
        mutating: true,
        input_schema: s_patch_settings,
        handler: h_patch_settings,
        preview: Some(p_patch_settings),
    },
    ToolSpec {
        name: "reorder_services",
        description: "Set the sidebar order of services by supplying their ids in the desired order.",
        scope: Scope::Write,
        mutating: true,
        input_schema: s_reorder_services,
        handler: h_reorder_services,
        preview: Some(p_reorder),
    },
    ToolSpec {
        name: "set_active_service",
        description: "Make a service the active tab, creating or waking its webview as needed.",
        scope: Scope::Write,
        mutating: true,
        input_schema: s_set_active_service,
        handler: h_set_active_service,
        preview: Some(p_service_only),
    },
    ToolSpec {
        name: "reload_service",
        description: "Hard-reload a service by closing and recreating its webview. This is the only reliable recovery when a page's network stack has wedged; it returns to the configured URL, not the current deep link.",
        scope: Scope::Write,
        mutating: true,
        input_schema: s_reload_service,
        handler: h_reload_service,
        preview: Some(p_service_only),
    },
    ToolSpec {
        name: "navigate",
        description: "Navigate a service's webview back, forward, or home (to its configured URL).",
        scope: Scope::Write,
        mutating: true,
        input_schema: s_navigate,
        handler: h_navigate,
        preview: Some(p_navigate),
    },
    ToolSpec {
        name: "navigate_url",
        description: "Load an explicit https URL inside a service's isolated session.",
        scope: Scope::Write,
        mutating: true,
        input_schema: s_navigate_url,
        handler: h_navigate_url,
        preview: Some(p_navigate_url),
    },
    ToolSpec {
        name: "hibernate_service",
        description: "Close a service's webview to free RAM while keeping its session login intact.",
        scope: Scope::Write,
        mutating: true,
        input_schema: s_hibernate_service,
        handler: h_hibernate_service,
        preview: Some(p_service_only),
    },
    ToolSpec {
        name: "wake_service",
        description: "Recreate a hibernated service's webview at its configured URL.",
        scope: Scope::Write,
        mutating: true,
        input_schema: s_wake_service,
        handler: h_wake_service,
        preview: Some(p_service_only),
    },
    ToolSpec {
        name: "reset_service_icon",
        description: "Remove a service's uploaded custom icon so it falls back to its brand icon or lettermark.",
        scope: Scope::Write,
        mutating: true,
        input_schema: s_reset_service_icon,
        handler: h_reset_service_icon,
        preview: Some(p_service_only),
    },
    // ---- admin ------------------------------------------------------------
    ToolSpec {
        name: "remove_service",
        description: "Permanently remove a service, delete its saved session (logins are unrecoverable) and its custom icon. Irreversible.",
        scope: Scope::Admin,
        mutating: true,
        input_schema: s_remove_service,
        handler: h_remove_service,
        preview: Some(p_remove_service),
    },
    ToolSpec {
        name: "export_config",
        description: "Write services and settings to a JSON file at the path you choose. Returns a confirmationToken needed to import. Sessions and credentials are never included.",
        scope: Scope::Admin,
        mutating: true,
        input_schema: s_export_config,
        handler: h_export_config,
        preview: None,
    },
    ToolSpec {
        name: "import_config",
        description: "REPLACE all services and settings from a config JSON file. Requires the confirmationToken from export_config. Saved sessions are kept where ids match.",
        scope: Scope::Admin,
        mutating: true,
        input_schema: s_import_config,
        handler: h_import_config,
        preview: None,
    },
];

/// Looks up a tool by exact name.
pub fn find_tool(name: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|t| t.name == name)
}

pub fn all_tools() -> &'static [ToolSpec] {
    TOOLS
}

/// The MCP tools/list payload and the REST /tools payload share this shape.
pub fn tools_schema_list() -> Value {
    Value::Array(
        TOOLS
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "scope": t.scope.as_str(),
                    "mutating": t.mutating,
                    "supportsDryRun": t.mutating && t.preview.is_some(),
                    "inputSchema": (t.input_schema)(),
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for t in TOOLS {
            assert!(seen.insert(t.name), "duplicate tool name: {}", t.name);
        }
    }

    #[test]
    fn every_tool_has_a_useful_description() {
        for t in TOOLS {
            assert!(!t.description.is_empty(), "{} has no description", t.name);
            assert!(
                t.description.len() > 30,
                "{} description is too terse for an LLM: {:?}",
                t.name,
                t.description
            );
        }
    }

    #[test]
    fn every_schema_is_an_object_with_described_properties() {
        for t in TOOLS {
            let schema = (t.input_schema)();
            assert_eq!(schema["type"], "object", "{} schema must be an object", t.name);
            assert_eq!(schema["additionalProperties"], false, "{} must reject unknown props", t.name);
            if let Some(props) = schema["properties"].as_object() {
                for (k, v) in props {
                    let d = v.get("description").and_then(|d| d.as_str()).unwrap_or("");
                    assert!(
                        !d.is_empty(),
                        "{} property '{}' has no description",
                        t.name,
                        k
                    );
                }
            }
        }
    }

    #[test]
    fn required_properties_are_declared_in_the_schema() {
        for t in TOOLS {
            let schema = (t.input_schema)();
            if let Some(required) = schema["required"].as_array() {
                for r in required {
                    let name = r.as_str().unwrap();
                    assert!(
                        schema["properties"].get(name).is_some(),
                        "{} requires '{}' but does not declare it",
                        t.name,
                        name
                    );
                }
            }
        }
    }

    #[test]
    fn every_mutating_tool_supports_dry_run() {
        for t in TOOLS {
            if t.mutating {
                // export/import touch the filesystem only on an explicit path
                // and are admin-gated; everything else must preview.
                if t.name == "export_config" || t.name == "import_config" {
                    continue;
                }
                assert!(
                    t.preview.is_some(),
                    "mutating tool {} must provide a dryRun preview",
                    t.name
                );
            } else {
                assert!(t.preview.is_none(), "read-only tool {} should not preview", t.name);
            }
        }
    }

    #[test]
    fn mutating_tools_expose_dry_run_parameter() {
        for t in TOOLS {
            if t.mutating && t.preview.is_some() {
                let schema = (t.input_schema)();
                assert!(
                    schema["properties"].get("dryRun").is_some(),
                    "{} is mutating but its schema has no dryRun property",
                    t.name
                );
            }
        }
    }

    #[test]
    fn scopes_match_the_documented_matrix() {
        let read = ["list_services", "get_service", "get_settings", "get_state",
                    "list_recipes", "get_service_icon", "list_sessions"];
        let admin = ["remove_service", "export_config", "import_config"];
        for t in TOOLS {
            let expected = if read.contains(&t.name) {
                Scope::Read
            } else if admin.contains(&t.name) {
                Scope::Admin
            } else {
                Scope::Write
            };
            assert_eq!(t.scope, expected, "{} has the wrong scope", t.name);
        }
    }

    #[test]
    fn ui_only_and_gated_commands_are_not_exposed() {
        for forbidden in [
            "set_overlay_mode",
            "open_devtools",
            "open_service_external",
            "report_title",
            "pick_service_icon",
        ] {
            assert!(
                find_tool(forbidden).is_none(),
                "{forbidden} must not be reachable by an agent"
            );
        }
    }

    #[test]
    fn find_tool_is_exact_match() {
        assert!(find_tool("list_services").is_some());
        assert!(find_tool("list_service").is_none());
        assert!(find_tool("").is_none());
    }

    #[test]
    fn schema_list_serialises_every_tool() {
        let list = tools_schema_list();
        let arr = list.as_array().unwrap();
        assert_eq!(arr.len(), TOOLS.len());
        for row in arr {
            assert!(row["name"].is_string());
            assert!(row["inputSchema"].is_object());
            assert!(row.get("scope").is_some());
        }
    }
}
