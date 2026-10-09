use tauri::{AppHandle, Manager, State};

use crate::state::SharedState;

/// JavaScript injected into every service webview. Polls `document.title`
/// every 3 s and reports it to Rust so badges can be derived.
pub fn init_script(service_id: &str) -> String {
    format!(
        r#"(function () {{
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
/// The reported `id` is bound to the CALLING webview's label: a page in
/// service A's webview can never set service B's badge (the remote capability
/// grants `report_title` to every service webview, so binding is what makes
/// that grant safe).
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
    let count = parse_badge_count(&title);
    let (total, snapshot) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        match inner.service_mut(&id) {
            Some(service) if service.badge_count != count => {
                service.badge_count = count;
            }
            _ => return Ok(()),
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
    fn init_script_references_report_title_and_id() {
        let script = init_script("abc-123");
        assert!(script.contains("report_title"));
        assert!(script.contains("\"abc-123\""));
        assert!(script.contains("3000"));
    }
}
