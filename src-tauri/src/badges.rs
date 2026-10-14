use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager, State};

use crate::state::SharedState;

/// Maximum accepted title length (bytes). Titles are only parsed for a
/// `(N)` badge prefix; anything longer is a hostile payload trying to force
/// multi-megabyte IPC deserialization and byte scanning (round-3 finding 1).
pub const MAX_TITLE_BYTES: usize = 512;

/// Minimum interval between the expensive fan-outs (state snapshot + IPC
/// emit + window retitle) triggered by one webview. The in-memory badge
/// count always updates, so a page flipping titles still converges to the
/// right number — it just cannot force lock/clone/emit churn in a loop.
const MIN_UPDATE_INTERVAL: Duration = Duration::from_millis(1000);

/// Webview label -> last accepted update time.
static LAST_UPDATE: LazyLock<Mutex<HashMap<String, Instant>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Records the attempt and reports whether the fan-out may proceed.
fn throttle_allows(map: &mut HashMap<String, Instant>, label: &str, now: Instant) -> bool {
    match map.get(label) {
        Some(last) if now.duration_since(*last) < MIN_UPDATE_INTERVAL => false,
        _ => {
            map.insert(label.to_string(), now);
            true
        }
    }
}

/// JavaScript injected into every service webview. Polls `document.title`
/// every 3 s and reports it to Rust so badges can be derived.
///
/// Top frame only: WebView2 runs initialization scripts in ALL frames
/// (AddScriptToExecuteOnDocumentCreated). Inside iframes (OAuth, reCAPTCHA,
/// about:srcdoc) the invoke would carry an opaque `Origin: null` header that
/// the Tauri IPC endpoint rejects with HTTP 500, and the `window.__TAURI__`
/// global would leak into third-party frames — extra embedded-browser
/// fingerprint surface.
///
/// Local invariant: service ids are UUIDs by construction (add_service,
/// import) — assert it here so this stays true even if callers change.
pub fn init_script(service_id: &str) -> String {
    debug_assert!(uuid::Uuid::parse_str(service_id).is_ok(), "service id must be a UUID");
    format!(
        r#"(function () {{
  if (window.top !== window.self) {{ return; }}
  var last = null;
  function report() {{
    try {{
      var title = document.title || '';
      if (title !== last) {{
        last = title;
        window.__TAURI__.core.invoke('report_title', {{ id: {service_id:?}, title: title }});
      }}
    }} catch (e) {{}}
  }}
  setInterval(report, 3000);
  if (document.readyState === 'complete') {{ report(); }}
  else {{ window.addEventListener('load', report); }}
}})();"#
    )
}

/// Badge parsing per spec section 6:
/// first match of `\((\d+)\)` → that number; a title starting with `•` or
/// containing `(•)` → 1; otherwise 0.
/// Counts are capped at 999 (the UI renders "99+" anyway) so per-service
/// values and their sum can never overflow, in any build profile.
pub fn parse_badge_count(title: &str) -> u32 {
    const MAX_BADGE: u32 = 999;
    let bytes = title.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'(' {
            let mut j = i + 1;
            let start = j;
            let mut value: u32 = 0;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                value = value.saturating_mul(10).saturating_add((bytes[j] - b'0') as u32);
                j += 1;
            }
            if j > start && j < bytes.len() && bytes[j] == b')' {
                return value.min(MAX_BADGE);
            }
        }
        i += 1;
    }
    if title.starts_with('•') || title.contains("(•)") {
        return 1;
    }
    0
}

/// Command invoked BY service webviews (injected script). Updates the badge
/// count; emits `state-changed` and retitles the window only when the count
/// actually changed.
///
/// Two gates (round-1 + round-2 review):
/// 1. The reported `id` must match the CALLING webview's label.
/// 2. The webview's last loaded URL must be on the service's home host —
///    the label never changes when a page navigates, so without this a
///    service tab walked to another origin could still forge badges.
///    Unknown webviews fail closed.
#[tauri::command]
pub fn report_title(
    app: AppHandle,
    webview: tauri::Webview<tauri::Wry>,
    state: State<'_, SharedState>,
    id: String,
    title: String,
) -> Result<(), String> {
    if webview.label() != crate::webviews::label_for(&id) {
        return Err("report_title called for a different service than the calling webview".to_string());
    }
    if title.len() > MAX_TITLE_BYTES {
        return Ok(());
    }
    let count = parse_badge_count(&title);
    let (total, snapshot) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let home_url = match inner.service(&id) {
            Some(service) => service.url.clone(),
            None => return Err(format!("unknown service id {id}")),
        };
        if !crate::webviews::webview_on_home_origin(webview.label(), &home_url) {
            // Off-origin (e.g. mid-OAuth redirect) — ignore silently; badge
            // reporting resumes when the page returns to the service host.
            return Ok(());
        }
        let changed = match inner.service_mut(&id) {
            Some(service) if service.badge_count != count => {
                service.badge_count = count;
                true
            }
            _ => false,
        };
        if !changed {
            return Ok(());
        }
        // The count is already right; only the fan-out is throttled.
        let label = webview.label().to_string();
        let allowed = LAST_UPDATE
            .lock()
            .map(|mut m| throttle_allows(&mut m, &label, Instant::now()))
            .unwrap_or(true);
        if !allowed {
            return Ok(());
        }
        let snapshot = inner.snapshot();
        (snapshot.total_badge_count, snapshot)
    };
    // Lock released: window title and emit happen from the snapshot.
    if let Some(window) = app.get_window(crate::state::UI_WEBVIEW_LABEL) {
        let title = if total > 0 {
            format!("bzdium ({total})")
        } else {
            "bzdium".to_string()
        };
        let _ = window.set_title(&title);
    }
    crate::state::emit_snapshot(&app, &snapshot);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parenthesized_number() {
        assert_eq!(parse_badge_count("(3) WhatsApp"), 3);
        assert_eq!(parse_badge_count("(12) Slack"), 12);
        assert_eq!(parse_badge_count("WhatsApp (7)"), 7);
    }

    #[test]
    fn bullet_variants() {
        assert_eq!(parse_badge_count("• Telegram"), 1);
        assert_eq!(parse_badge_count("(•) Discord"), 1);
        assert_eq!(parse_badge_count("Discord (•)"), 1);
    }

    #[test]
    fn plain_titles_are_zero() {
        assert_eq!(parse_badge_count("WhatsApp"), 0);
        assert_eq!(parse_badge_count(""), 0);
        assert_eq!(parse_badge_count("Telegram Web"), 0);
    }

    #[test]
    fn edge_cases() {
        // Regex requires digits only inside parens.
        assert_eq!(parse_badge_count("(12 new) X"), 0);
        assert_eq!(parse_badge_count("(one) X"), 0);
        assert_eq!(parse_badge_count("() X"), 0);
        assert_eq!(parse_badge_count("(12 X"), 0);
        // First matching group wins.
        assert_eq!(parse_badge_count("(12 new) (3) X"), 3);
        assert_eq!(parse_badge_count("(2) (5) X"), 2);
        // Bullet in the middle without parens doesn't count.
        assert_eq!(parse_badge_count("A • B"), 0);
        // Number beats bullet when both present.
        assert_eq!(parse_badge_count("• (4) Chat"), 4);
    }

    #[test]
    fn badge_count_is_capped() {
        // A hostile page title must not produce huge counts: per-service
        // values are capped (UI renders "99+") so the total can never
        // overflow, even in debug builds with overflow checks on.
        assert_eq!(parse_badge_count("(99999999999) X"), 999);
        assert_eq!(parse_badge_count("(1000) X"), 999);
        assert_eq!(parse_badge_count("(999) X"), 999);
        assert_eq!(parse_badge_count("(998) X"), 998);
    }

    #[test]
    fn init_script_runs_in_top_frame_only() {
        let script = init_script("00000000-0000-4000-8000-00000000abc1");
        assert!(script.contains("window.top !== window.self"));
    }

    #[test]
    fn init_script_references_report_title_and_id() {
        let script = init_script("00000000-0000-4000-8000-00000000abc1");
        assert!(script.contains("report_title"));
        assert!(script.contains("\"00000000-0000-4000-8000-00000000abc1\""));
        assert!(script.contains("3000"));
    }

    #[test]
    fn throttle_caps_fanout_per_label() {
        let mut map = HashMap::new();
        let t0 = Instant::now();
        assert!(throttle_allows(&mut map, "service-a", t0));
        // immediate second call from the same webview is throttled
        assert!(!throttle_allows(&mut map, "service-a", t0 + Duration::from_millis(10)));
        // other webviews are unaffected
        assert!(throttle_allows(&mut map, "service-b", t0 + Duration::from_millis(10)));
        // same webview again after the interval is allowed
        assert!(throttle_allows(&mut map, "service-a", t0 + Duration::from_millis(1001)));
    }
}
