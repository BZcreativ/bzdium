//! Read-only context resources, addressed by bzdium:// URIs.
//!
//! Design invariants (mirroring mod.rs):
//!
//! 1. ZERO DUPLICATION. Every resource projects the same data its matching read
//!    tool returns: app state is read through SharedState, recipes come from
//!    services.rs, audit entries from audit.rs, and an unknown service id is
//!    classified by the very translator (AgentError::from_command) that
//!    get_service uses. There is no second source of truth here.
//! 2. SESSIONS ARE NEVER EXPOSED. bzdium://sessions reports session directory
//!    names and byte sizes only. Profile contents - cookies, logins, tokens,
//!    local storage - are never read, never serialised and never reachable
//!    through this module. See the test
//!    sessions_expose_only_directory_names_and_sizes.
//!
//! Template matching is deliberately small: a braced segment in a URI template
//! captures exactly one path segment. Literal templates are matched before
//! parameterised ones, so a future bzdium://services/health can never be
//! shadowed by bzdium://services/{id}.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Value};

use super::error::{AgentError, ErrorCode};
use super::scope::Scope;
use super::{AgentCtx, BoxFuture, ResourceSpec};

use tauri::Manager as _;

/// Scheme every resource URI must carry.
const URI_SCHEME: &str = "bzdium://";

/// How many audit entries bzdium://audit/recent returns.
const AUDIT_LIMIT: usize = 50;

pub static RESOURCES: &[ResourceSpec] = &[
    ResourceSpec {
        uri_template: "bzdium://app/state",
        name: "app_state",
        description: "The complete application snapshot: services sorted by sidebar order, the active service id, settings, the total unread badge count and the data directory path.",
        mime_type: "application/json",
        scope: Scope::Read,
        render: r_app_state,
    },
    ResourceSpec {
        uri_template: "bzdium://app/health",
        name: "app_health",
        description: "Liveness of the running instance: status, bzdium version, agent uptime in seconds, whether the agent listener is up, and counts of services and saved session directories.",
        mime_type: "application/json",
        scope: Scope::Read,
        render: r_app_health,
    },
    ResourceSpec {
        uri_template: "bzdium://services",
        name: "services",
        description: "Every configured service with its id, name, url, icon, enabled/hibernated state, unread badge count and TLS setting, sorted by sidebar order, plus the active service id.",
        mime_type: "application/json",
        scope: Scope::Read,
        render: r_services,
    },
    ResourceSpec {
        uri_template: "bzdium://services/{id}",
        name: "service",
        description: "One configured service addressed by its id. An unknown id fails with UNKNOWN_SERVICE_ID and names list_services as the way to discover valid ids.",
        mime_type: "application/json",
        scope: Scope::Read,
        render: r_service,
    },
    ResourceSpec {
        uri_template: "bzdium://settings",
        name: "settings",
        description: "The current application settings only: hibernation minutes, tray, start-with-login, theme, URL bar, developer mode, link target and agent access.",
        mime_type: "application/json",
        scope: Scope::Read,
        render: r_settings,
    },
    ResourceSpec {
        uri_template: "bzdium://recipes",
        name: "recipes",
        description: "The built-in service recipe catalog: 60 well-known web apps with a display name, an https url, a 1-2 character lettermark and a brand icon slug. Use it instead of guessing a URL.",
        mime_type: "application/json",
        scope: Scope::Read,
        render: r_recipes,
    },
    ResourceSpec {
        uri_template: "bzdium://sessions",
        name: "sessions",
        description: "On-disk session profile size per service, in bytes. Directory names and sizes only: profile contents such as cookies, logins, tokens and local storage are never exposed by this resource.",
        mime_type: "application/json",
        scope: Scope::Read,
        render: r_sessions,
    },
    ResourceSpec {
        uri_template: "bzdium://audit/recent",
        name: "audit_recent",
        description: "The 50 most recent agent audit-log entries, oldest first: timestamp, action, transport, actor label, scope, outcome, duration and any error code. Arguments are already redacted.",
        mime_type: "application/json",
        scope: Scope::Read,
        render: r_audit_recent,
    },
];

// ---------------------------------------------------------------------------
// Lookup
// ---------------------------------------------------------------------------

/// Resolves a concrete URI against the registry.
///
/// Returns the matched spec plus every captured path parameter. None means no
/// template matches, which every transport turns into UNKNOWN_RESOURCE rather
/// than guessing at a nearest neighbour.
pub fn find_resource(uri: &str) -> Option<(&'static ResourceSpec, HashMap<String, String>)> {
    find_in(RESOURCES, uri)
}

/// The MCP resources/list and REST /resources payload.
pub fn resources_list() -> Value {
    Value::Array(
        RESOURCES
            .iter()
            .map(|r| {
                json!({
                    "uri": r.uri_template,
                    "name": r.name,
                    "description": r.description,
                    "mimeType": r.mime_type,
                    "scope": r.scope.as_str(),
                })
            })
            .collect(),
    )
}

/// Lookup against an explicit table, so matching rules can be tested against a
/// fixture with a literal/parameterised collision.
fn find_in(
    table: &'static [ResourceSpec],
    uri: &str,
) -> Option<(&'static ResourceSpec, HashMap<String, String>)> {
    let segments = uri_segments(uri)?;

    // Pass 1: literal templates. An exact URI must never be intercepted by a
    // parameterised template of the same shape.
    for spec in table.iter().filter(|s| !s.uri_template.contains('{')) {
        if let Some(params) = match_template(spec.uri_template, &segments) {
            return Some((spec, params));
        }
    }
    // Pass 2: templates with braced segments.
    for spec in table.iter().filter(|s| s.uri_template.contains('{')) {
        if let Some(params) = match_template(spec.uri_template, &segments) {
            return Some((spec, params));
        }
    }
    None
}

/// Splits a bzdium:// URI into path segments.
///
/// Tolerates a trailing slash and ignores any query or fragment, because the
/// REST projection rebuilds URIs from a path fragment and clients may append
/// either. Rejects an empty path and an empty segment (a double slash), so a
/// missing path parameter can never match.
fn uri_segments(uri: &str) -> Option<Vec<&str>> {
    let rest = strip_scheme(uri)?;
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let rest = rest.trim_end_matches('/');
    if rest.is_empty() {
        return None;
    }
    let segments: Vec<&str> = rest.split('/').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return None;
    }
    Some(segments)
}

/// Requires the bzdium scheme (case-insensitively) and returns the remainder.
fn strip_scheme(uri: &str) -> Option<&str> {
    let (scheme, rest) = uri.split_once("://")?;
    let expected = URI_SCHEME.trim_end_matches("://");
    if !scheme.eq_ignore_ascii_case(expected) {
        return None;
    }
    Some(rest)
}

/// Matches one template against already-split segments.
fn match_template(template: &str, segments: &[&str]) -> Option<HashMap<String, String>> {
    let parts = uri_segments(template)?;
    if parts.len() != segments.len() {
        return None;
    }
    let mut params = HashMap::new();
    for (part, actual) in parts.iter().zip(segments.iter()) {
        match param_name(part) {
            Some(name) => {
                let value = percent_decode(actual)?;
                if value.is_empty() {
                    return None;
                }
                params.insert(name.to_string(), value);
            }
            None => {
                if part != actual {
                    return None;
                }
            }
        }
    }
    Some(params)
}

/// The parameter name of a fully braced segment, or None for a literal.
fn param_name(segment: &str) -> Option<&str> {
    let inner = segment.strip_prefix('{')?.strip_suffix('}')?;
    if inner.is_empty() || inner.contains('{') || inner.contains('}') {
        return None;
    }
    Some(inner)
}

/// Minimal percent-decoding, no dependency. Returns None on a malformed or
/// NUL-bearing escape rather than silently mangling a service id.
fn percent_decode(raw: &str) -> Option<String> {
    if !raw.contains('%') {
        return Some(raw.to_string());
    }
    let bytes = raw.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let value = u8::from_str_radix(hex, 16).ok()?;
            if value == 0 {
                return None;
            }
            out.push(value);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

// ---------------------------------------------------------------------------
// Renderers
// ---------------------------------------------------------------------------

fn r_app_state<'a>(
    ctx: &'a AgentCtx,
    _params: &'a HashMap<String, String>,
) -> BoxFuture<'a, Result<String, AgentError>> {
    Box::pin(async move {
        // Snapshot under the lock, serialise after releasing it: holding the
        // state mutex across encoding would stall the UI thread for no reason.
        let snapshot = {
            let state = ctx.app.state::<crate::state::SharedState>();
            let inner = state.lock().map_err(lock_err)?;
            inner.snapshot()
        };
        to_pretty(state_json(&snapshot))
    })
}

fn r_app_health<'a>(
    ctx: &'a AgentCtx,
    _params: &'a HashMap<String, String>,
) -> BoxFuture<'a, Result<String, AgentError>> {
    Box::pin(async move {
        let service_count = {
            let state = ctx.app.state::<crate::state::SharedState>();
            let inner = state.lock().map_err(lock_err)?;
            inner.services.len()
        };
        // Counted outside the lock: this touches the filesystem.
        let session_count = session_dir_count(&ctx.data_dir);
        to_pretty(health_json(
            env!("CARGO_PKG_VERSION"),
            ctx.uptime_secs(),
            crate::agent::startup::is_running(),
            service_count,
            session_count,
        ))
    })
}

fn r_services<'a>(
    ctx: &'a AgentCtx,
    _params: &'a HashMap<String, String>,
) -> BoxFuture<'a, Result<String, AgentError>> {
    Box::pin(async move {
        let (mut services, active) = {
            let state = ctx.app.state::<crate::state::SharedState>();
            let inner = state.lock().map_err(lock_err)?;
            (inner.services.clone(), inner.active_service_id.clone())
        };
        services.sort_by_key(|s| s.order);
        to_pretty(services_json(&services, active.as_deref()))
    })
}

fn r_service<'a>(
    ctx: &'a AgentCtx,
    params: &'a HashMap<String, String>,
) -> BoxFuture<'a, Result<String, AgentError>> {
    Box::pin(async move {
        let Some(id) = params.get("id") else {
            // Unreachable in practice: the template only matches with an id.
            return Err(AgentError::new(
                ErrorCode::Internal,
                "No 'id' path parameter was captured from the resource URI.",
                "Read bzdium://services (or call list_services) and address the service as bzdium://services/<id>.",
            ));
        };
        let service = {
            let state = ctx.app.state::<crate::state::SharedState>();
            let inner = state.lock().map_err(lock_err)?;
            inner.service(id).cloned()
        };
        let service = service.ok_or_else(|| unknown_service(id))?;
        to_pretty(service_json(&service))
    })
}

fn r_settings<'a>(
    ctx: &'a AgentCtx,
    _params: &'a HashMap<String, String>,
) -> BoxFuture<'a, Result<String, AgentError>> {
    Box::pin(async move {
        let settings = {
            let state = ctx.app.state::<crate::state::SharedState>();
            let inner = state.lock().map_err(lock_err)?;
            inner.settings.clone()
        };
        to_pretty(json!(settings))
    })
}

fn r_recipes<'a>(
    _ctx: &'a AgentCtx,
    _params: &'a HashMap<String, String>,
) -> BoxFuture<'a, Result<String, AgentError>> {
    Box::pin(async move { to_pretty(recipes_json()) })
}

fn r_sessions<'a>(
    ctx: &'a AgentCtx,
    _params: &'a HashMap<String, String>,
) -> BoxFuture<'a, Result<String, AgentError>> {
    Box::pin(async move { to_pretty(sessions_json(&ctx.data_dir)) })
}

fn r_audit_recent<'a>(
    ctx: &'a AgentCtx,
    _params: &'a HashMap<String, String>,
) -> BoxFuture<'a, Result<String, AgentError>> {
    Box::pin(async move { to_pretty(audit_json(&ctx.data_dir, AUDIT_LIMIT)) })
}

// ---------------------------------------------------------------------------
// Pure projections
//
// Kept free of Tauri types so the shapes - and above all the session-privacy
// guarantee - are testable without constructing an AppHandle.
// ---------------------------------------------------------------------------

fn state_json(snapshot: &crate::state::AppState) -> Value {
    json!({
        "services": &snapshot.services,
        "activeServiceId": &snapshot.active_service_id,
        "settings": &snapshot.settings,
        "totalBadgeCount": snapshot.total_badge_count,
        "dataDir": &snapshot.data_dir,
    })
}

fn health_json(
    version: &str,
    uptime_secs: u64,
    agent_running: bool,
    service_count: usize,
    session_count: usize,
) -> Value {
    json!({
        "status": "ok",
        "version": version,
        "uptimeSecs": uptime_secs,
        "agentRunning": agent_running,
        "serviceCount": service_count,
        "sessionCount": session_count,
    })
}

fn services_json(services: &[crate::state::Service], active: Option<&str>) -> Value {
    json!({
        "services": services,
        "count": services.len(),
        "activeServiceId": active,
    })
}

fn service_json(service: &crate::state::Service) -> Value {
    // Service holds only String/bool/u32/Option<String>: encoding cannot fail.
    json!(service)
}

fn recipes_json() -> Value {
    let recipes = crate::services::recipes();
    json!({ "count": recipes.len(), "recipes": recipes })
}

/// Session metadata only: one row per session directory holding its id and the
/// total bytes on disk. No file names, no paths, and above all no file
/// contents ever enter this projection.
fn sessions_json(data_dir: &Path) -> Value {
    let sessions_dir = data_dir.join(crate::storage::SESSIONS_DIR);
    let mut rows: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&sessions_dir) {
        let mut items: Vec<_> = entries.flatten().collect();
        items.sort_by_key(|e| e.file_name());
        for entry in items {
            // file_type() does not follow symlinks, so a symlinked entry is
            // skipped rather than measured through to its target.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            rows.push(json!({
                "serviceId": entry.file_name().to_string_lossy(),
                "bytesOnDisk": dir_size(&entry.path()),
            }));
        }
    }
    let count = rows.len();
    json!({ "sessions": rows, "count": count })
}

fn audit_json(data_dir: &Path, limit: usize) -> Value {
    let entries = crate::agent::audit::read_recent(data_dir, limit);
    json!({ "count": entries.len(), "limit": limit, "entries": entries })
}

fn session_dir_count(data_dir: &Path) -> usize {
    let dir = data_dir.join(crate::storage::SESSIONS_DIR);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .count()
}

/// Sums file sizes in a directory tree, skipping anything unreadable.
///
/// tools.rs carries an equivalent private helper for its list_sessions tool;
/// it cannot be shared without editing that file, so the twelve lines are
/// duplicated deliberately rather than growing a shared private module.
fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    for entry in entries.flatten() {
        match entry.file_type() {
            Ok(t) if t.is_dir() => total = total.saturating_add(dir_size(&entry.path())),
            Ok(t) if t.is_file() => {
                total = total.saturating_add(entry.metadata().map(|m| m.len()).unwrap_or(0))
            }
            _ => {}
        }
    }
    total
}

/// Classifies an unknown service id exactly as get_service does: one
/// translation point, so an agent sees the same code and suggestion from every
/// surface.
fn unknown_service(id: &str) -> AgentError {
    AgentError::from_command(format!("unknown service id {id}"))
}

fn lock_err<T>(_: std::sync::PoisonError<T>) -> AgentError {
    AgentError::new(
        ErrorCode::Internal,
        "The application state lock is poisoned.",
        "A previous operation panicked while holding the state lock. Restart bzdium, then retry the read.",
    )
}

fn internal(e: serde_json::Error) -> AgentError {
    AgentError::new(
        ErrorCode::Internal,
        format!("The resource could not be encoded as JSON: {e}"),
        "Retry the read once; if it persists, inspect bzdium://audit/recent.",
    )
}

fn to_pretty(value: Value) -> Result<String, AgentError> {
    serde_json::to_string_pretty(&value).map_err(internal)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Service, Settings};
    use std::collections::HashSet;

    // -- fixtures ----------------------------------------------------------

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("bzdium-res-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn service(id: &str, order: u32, badges: u32) -> Service {
        Service {
            id: id.to_string(),
            name: format!("Service {id}"),
            url: format!("https://{id}.example.com"),
            icon: "S".to_string(),
            enabled: true,
            order,
            icon_id: None,
            custom_icon: None,
            hibernated: false,
            badge_count: badges,
            ignore_tls_errors: false,
            kind: crate::state::ServiceKind::Web,
        }
    }

    fn stub_render<'a>(
        _ctx: &'a AgentCtx,
        _params: &'a HashMap<String, String>,
    ) -> BoxFuture<'a, Result<String, AgentError>> {
        Box::pin(async move { Ok(String::new()) })
    }

    /// Fixture registry with a literal/parameterised collision, which the real
    /// table cannot express.
    static FAKE: &[ResourceSpec] = &[
        ResourceSpec {
            uri_template: "bzdium://x/{id}",
            name: "fake_template",
            description: "fixture: parameterised resource used to pin matching precedence",
            mime_type: "application/json",
            scope: Scope::Read,
            render: stub_render,
        },
        ResourceSpec {
            uri_template: "bzdium://x/literal",
            name: "fake_literal",
            description: "fixture: literal resource used to pin matching precedence",
            mime_type: "application/json",
            scope: Scope::Read,
            render: stub_render,
        },
    ];

    // -- registry contract -------------------------------------------------

    #[test]
    fn every_template_is_unique_and_described() {
        let mut uris = HashSet::new();
        let mut names = HashSet::new();
        for r in RESOURCES {
            assert!(
                r.uri_template.starts_with(URI_SCHEME),
                "{} must use the bzdium scheme",
                r.uri_template
            );
            assert!(!r.mime_type.is_empty(), "{} has no mime type", r.uri_template);
            assert!(
                r.description.len() > 30,
                "{} description is too terse for an LLM: {:?}",
                r.uri_template,
                r.description
            );
            assert_eq!(
                r.scope,
                Scope::Read,
                "{} must stay read-scoped: resources are read-only context",
                r.uri_template
            );
            assert!(uris.insert(r.uri_template), "duplicate uri {}", r.uri_template);
            assert!(names.insert(r.name), "duplicate resource name {}", r.name);
            // The template itself must be matchable, or it is advertised but
            // unreachable.
            assert!(
                match_template(r.uri_template, &uri_segments(r.uri_template).unwrap()).is_some(),
                "{} does not match itself",
                r.uri_template
            );
        }
    }

    #[test]
    fn registry_contains_the_documented_resources() {
        for uri in [
            "bzdium://app/state",
            "bzdium://app/health",
            "bzdium://services",
            "bzdium://services/{id}",
            "bzdium://settings",
            "bzdium://recipes",
            "bzdium://sessions",
            "bzdium://audit/recent",
        ] {
            assert!(
                RESOURCES.iter().any(|r| r.uri_template == uri),
                "missing resource {uri}"
            );
        }
        assert_eq!(RESOURCES.len(), 8);
    }

    #[test]
    fn resources_list_serialises_the_whole_table() {
        let list = resources_list();
        let rows = list.as_array().unwrap();
        assert_eq!(rows.len(), RESOURCES.len());
        for (row, spec) in rows.iter().zip(RESOURCES.iter()) {
            assert_eq!(row["uri"], spec.uri_template);
            assert_eq!(row["name"], spec.name);
            assert_eq!(row["mimeType"], spec.mime_type);
            assert_eq!(row["scope"], "read");
            assert!(row["description"].as_str().is_some_and(|d| !d.is_empty()));
        }
    }

    // -- URI matching ------------------------------------------------------

    #[test]
    fn static_templates_match_themselves_with_no_params() {
        for spec in RESOURCES.iter().filter(|r| !r.uri_template.contains('{')) {
            let (found, params) = find_resource(spec.uri_template)
                .unwrap_or_else(|| panic!("{} did not resolve", spec.uri_template));
            assert_eq!(found.name, spec.name);
            assert!(params.is_empty(), "{} captured unexpected params", spec.uri_template);
        }
    }

    #[test]
    fn path_parameters_are_captured() {
        let id = "4f1c9a2e-0000-4000-8000-aaaaaaaaaaaa";
        let (spec, params) = find_resource(&format!("bzdium://services/{id}")).unwrap();
        assert_eq!(spec.name, "service");
        assert_eq!(params.len(), 1);
        assert_eq!(params["id"], id);
    }

    #[test]
    fn captured_parameters_are_percent_decoded() {
        let (_, params) = find_resource("bzdium://services/a%20b").unwrap();
        assert_eq!(params["id"], "a b");
        // Malformed escapes are refused rather than silently truncated.
        assert!(find_resource("bzdium://services/bad%zz").is_none());
        assert!(find_resource("bzdium://services/bad%2").is_none());
        assert!(find_resource("bzdium://services/bad%00").is_none());
    }

    #[test]
    fn unknown_uris_never_match() {
        for uri in [
            "",
            "bzdium://",
            "bzdium:///",
            "bzdium://nope",
            "bzdium://app",
            "bzdium://services/abc/extra",
            "bzdium://audit",
            "https://example.com/services",
            "bzdium:/services",
            "bdzium://services",
            "bzdium://Services",
            "bzdium://services//abc",
        ] {
            assert!(find_resource(uri).is_none(), "{uri:?} must not match");
        }
    }

    #[test]
    fn trailing_slash_and_query_are_tolerated() {
        // The REST projection rebuilds URIs from a path fragment, so a trailing
        // slash must not turn a valid resource into UNKNOWN_RESOURCE.
        let (spec, params) = find_resource("bzdium://services/").unwrap();
        assert_eq!(spec.name, "services");
        assert!(params.is_empty());
        let (spec, _) = find_resource("bzdium://app/state?pretty=1").unwrap();
        assert_eq!(spec.name, "app_state");
        let (spec, _) = find_resource("bzdium://app/state#top").unwrap();
        assert_eq!(spec.name, "app_state");
    }

    #[test]
    fn scheme_matching_is_case_insensitive() {
        let (spec, _) = find_resource("BZDIUM://services").unwrap();
        assert_eq!(spec.name, "services");
    }

    #[test]
    fn literal_templates_win_over_parameterised_ones() {
        let (spec, params) = find_in(FAKE, "bzdium://x/literal").unwrap();
        assert_eq!(spec.name, "fake_literal", "a literal URI must not be shadowed");
        assert!(params.is_empty());

        let (spec, params) = find_in(FAKE, "bzdium://x/other").unwrap();
        assert_eq!(spec.name, "fake_template");
        assert_eq!(params["id"], "other");
    }

    #[test]
    fn a_missing_path_parameter_does_not_match() {
        // bzdium://x/ keeps no segment for the id parameter, so it cannot match.
        assert!(find_in(FAKE, "bzdium://x/").is_none());
        assert!(find_in(FAKE, "bzdium://x").is_none());
    }

    #[test]
    fn brace_segments_must_be_fully_braced() {
        assert_eq!(param_name("{id}"), Some("id"));
        assert_eq!(param_name("id"), None);
        assert_eq!(param_name("{id"), None);
        assert_eq!(param_name("id}"), None);
        assert_eq!(param_name("{}"), None);
        assert_eq!(param_name("{a{b}"), None);
    }

    // -- projections -------------------------------------------------------

    #[test]
    fn state_json_carries_the_documented_snapshot_fields() {
        let snapshot = crate::state::AppState {
            services: vec![service("b", 1, 3), service("a", 0, 1)],
            active_service_id: Some("a".to_string()),
            settings: Settings::default(),
            total_badge_count: 4,
            data_dir: "C:\\data".to_string(),
        };
        let v = state_json(&snapshot);
        assert_eq!(v["totalBadgeCount"], 4);
        assert_eq!(v["dataDir"], "C:\\data");
        assert_eq!(v["activeServiceId"], "a");
        assert_eq!(v["services"].as_array().unwrap().len(), 2);
        // Settings must keep the same camelCase contract the UI receives.
        assert_eq!(v["settings"]["hibernationMinutes"], 30);
        assert_eq!(v["settings"]["minimizeToTray"], true);
        let keys: HashSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            HashSet::from([
                "services",
                "activeServiceId",
                "settings",
                "totalBadgeCount",
                "dataDir"
            ])
        );
    }

    #[test]
    fn services_json_is_sorted_by_order() {
        let mut services = vec![service("c", 2, 0), service("a", 0, 5), service("b", 1, 1)];
        services.sort_by_key(|s| s.order);
        let v = services_json(&services, Some("a"));
        let rows = v["services"].as_array().unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
        assert_eq!(v["count"], 3);
        assert_eq!(v["activeServiceId"], "a");

        // No active service is null, not a missing key.
        let v = services_json(&services, None);
        assert!(v["activeServiceId"].is_null());
    }

    #[test]
    fn service_json_uses_the_ipc_camel_case_contract() {
        let mut svc = service("x", 0, 7);
        svc.icon_id = Some("whatsapp".to_string());
        svc.ignore_tls_errors = true;
        let v = service_json(&svc);
        assert_eq!(v["badgeCount"], 7);
        assert_eq!(v["iconId"], "whatsapp");
        assert_eq!(v["ignoreTlsErrors"], true);
        assert_eq!(v["hibernated"], false);
    }

    #[test]
    fn unknown_service_ids_are_classified_with_a_suggestion() {
        let err = unknown_service("nope");
        assert_eq!(err.code, "UNKNOWN_SERVICE_ID");
        assert!(err.suggestion.contains("list_services"));
        assert!(!err.message.is_empty());
        assert_eq!(err.status(), 400);
    }

    #[test]
    fn health_json_has_exactly_the_documented_keys() {
        let v = health_json("0.2.8", 42, true, 3, 2);
        assert_eq!(v["status"], "ok");
        assert_eq!(v["version"], "0.2.8");
        assert_eq!(v["uptimeSecs"], 42);
        assert_eq!(v["agentRunning"], true);
        assert_eq!(v["serviceCount"], 3);
        assert_eq!(v["sessionCount"], 2);
        assert_eq!(v.as_object().unwrap().len(), 6);
    }

    #[test]
    fn recipes_json_lists_the_full_catalog() {
        let v = recipes_json();
        let rows = v["recipes"].as_array().unwrap();
        assert_eq!(rows.len(), 62, "the built-in catalog is 62 recipes (61 web + Grok API)");
        assert_eq!(v["count"], 62);
        for row in rows {
            let url = row["url"].as_str().unwrap();
            assert!(url.starts_with("https://"), "{url} must be https");
            assert!(!row["name"].as_str().unwrap().is_empty());
            assert!(!row["icon"].as_str().unwrap().is_empty());
            // iconId is always present, null when no brand icon is bundled.
            assert!(row.get("iconId").is_some());
        }
    }

    #[test]
    fn audit_json_returns_at_most_the_documented_window() {
        let dir = temp_dir("audit");
        for i in 0..60u64 {
            crate::agent::audit::record(
                &dir,
                &crate::agent::audit::AuditEntry {
                    ts: i,
                    action: format!("a{i}"),
                    transport: "rest".into(),
                    actor: "token-label".into(),
                    scope: "read".into(),
                    outcome: "ok".into(),
                    duration_ms: Some(1),
                    dry_run: None,
                    error_code: None,
                    args: None,
                },
            );
        }
        let v = audit_json(&dir, AUDIT_LIMIT);
        let entries = v["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 50, "the window is capped at 50");
        assert_eq!(v["count"], 50);
        assert_eq!(v["limit"], 50);
        assert_eq!(entries[0]["action"], "a10", "oldest kept entry");
        assert_eq!(entries[49]["action"], "a59", "newest entry last");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn audit_json_is_empty_when_no_log_exists() {
        let dir = temp_dir("audit-empty");
        let v = audit_json(&dir, AUDIT_LIMIT);
        assert_eq!(v["count"], 0);
        assert!(v["entries"].as_array().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    // -- session privacy ---------------------------------------------------

    #[test]
    fn sessions_expose_only_directory_names_and_sizes() {
        let root = temp_dir("sessions");
        let sessions = root.join(crate::storage::SESSIONS_DIR);
        let id = "11111111-1111-4111-8111-111111111111";
        let profile = sessions.join(id);
        std::fs::create_dir_all(profile.join("Default").join("Local Storage").join("leveldb"))
            .unwrap();

        // Profile contents that must never leave the process.
        let cookie = b"SECRET_COOKIE=session-token-value";
        let login = b"hunter2-password-blob";
        let storage = b"localstorage-secret";
        std::fs::write(profile.join("Cookies"), cookie).unwrap();
        std::fs::write(profile.join("Login Data"), login).unwrap();
        std::fs::write(
            profile.join("Default").join("Local Storage").join("leveldb").join("000003.log"),
            storage,
        )
        .unwrap();

        // A stray file next to the session directories is not a service.
        std::fs::write(sessions.join("not-a-service.txt"), b"ignore me").unwrap();

        let value = sessions_json(&root);
        let rendered = serde_json::to_string_pretty(&value).unwrap();

        let rows = value["sessions"].as_array().unwrap();
        assert_eq!(rows.len(), 1, "only directories are reported as sessions");
        assert_eq!(value["count"], 1);

        let row = &rows[0];
        assert_eq!(row["serviceId"], id);
        let expected = (cookie.len() + login.len() + storage.len()) as u64;
        assert_eq!(
            row["bytesOnDisk"].as_u64().unwrap(),
            expected,
            "bytesOnDisk sums the profile tree"
        );

        // The row carries the two documented keys and nothing else: no file
        // names, no paths, no content.
        let map = row.as_object().unwrap();
        assert_eq!(map.len(), 2);
        assert!(map.contains_key("serviceId"));
        assert!(map.contains_key("bytesOnDisk"));

        for secret in [
            "SECRET_COOKIE",
            "session-token-value",
            "hunter2",
            "localstorage-secret",
            "Cookies",
            "Login Data",
            "Local Storage",
            "leveldb",
            "Default",
            "000003.log",
            "not-a-service",
        ] {
            assert!(
                !rendered.contains(secret),
                "bzdium://sessions leaked profile detail {secret:?}: {rendered}"
            );
        }

        // And the envelope keys are exactly the two the contract advertises.
        let keys: HashSet<&str> = value.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, HashSet::from(["sessions", "count"]));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn sessions_is_empty_when_no_profile_exists() {
        let root = temp_dir("sessions-missing");
        let value = sessions_json(&root);
        assert_eq!(value["count"], 0);
        assert!(value["sessions"].as_array().unwrap().is_empty());
        assert_eq!(session_dir_count(&root), 0);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn session_dir_count_counts_directories_only() {
        let root = temp_dir("sessions-count");
        let sessions = root.join(crate::storage::SESSIONS_DIR);
        std::fs::create_dir_all(sessions.join("a")).unwrap();
        std::fs::create_dir_all(sessions.join("b")).unwrap();
        std::fs::write(sessions.join("c"), b"file").unwrap();
        assert_eq!(session_dir_count(&root), 2);
        std::fs::remove_dir_all(&root).ok();
    }
}
