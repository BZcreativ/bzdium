//! Append-only audit log for every agent-initiated action.
//!
//! One JSON object per line, written to agent/audit.jsonl inside the data
//! dir. Arguments are redacted before they are written: tokens must never
//! reach the log, and service names or URLs are the only user data kept.

use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{agent_dir, AUDIT_FILE};

/// Keep the log bounded: rotate to audit.1.jsonl past this size.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuditEntry {
    /// Unix seconds.
    pub ts: u64,
    /// Tool name, or the HTTP route for non-tool requests.
    pub action: String,
    /// Transport that carried the call: rest, mcp, stdio, ui.
    pub transport: String,
    /// Token label, never the token itself.
    pub actor: String,
    pub scope: String,
    pub outcome: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// Redacted, size-capped arguments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Removes anything secret-shaped and caps the rendered size.
pub fn redact(value: &Value) -> Value {
    const SECRET_KEYS: [&str; 6] =
        ["token", "authorization", "password", "secret", "apiKey", "api_key"];
    fn walk(v: &Value) -> Value {
        match v {
            Value::Object(map) => {
                let mut out = serde_json::Map::new();
                for (k, val) in map {
                    if SECRET_KEYS.iter().any(|s| k.eq_ignore_ascii_case(s)) {
                        out.insert(k.clone(), Value::String("[redacted]".into()));
                    } else {
                        out.insert(k.clone(), walk(val));
                    }
                }
                Value::Object(out)
            }
            Value::Array(items) => Value::Array(items.iter().map(walk).collect()),
            other => other.clone(),
        }
    }
    let walked = walk(value);
    // Cap serialised size so a huge import_config payload cannot bloat the log.
    // `get(..2048)` returns None when the cut lands inside a multi-byte UTF-8
    // character — back off to the previous boundary instead of slicing at a
    // byte offset (which would panic, and this runs on the audit path AFTER a
    // mutation may already have committed).
    let rendered = walked.to_string();
    if rendered.len() > 2048 {
        let mut cut = 2048;
        while !rendered.is_char_boundary(cut) {
            cut -= 1;
        }
        serde_json::json!({
            "truncated": true,
            "bytes": rendered.len(),
            "preview": &rendered[..cut],
        })
    } else {
        walked
    }
}

/// Appends one entry. Audit failures are reported on stderr but never fail
/// the agent call that produced them: losing a log line must not break work.
pub fn record(data_dir: &Path, entry: &AuditEntry) {
    let dir = agent_dir(data_dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(AUDIT_FILE);
    rotate_if_needed(&path);
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        Ok(mut f) => {
            if let Err(e) = writeln!(f, "{line}") {
                eprintln!("agent audit: write failed: {e}");
            }
        }
        Err(e) => eprintln!("agent audit: cannot open {}: {e}", path.display()),
    }
}

fn rotate_if_needed(path: &Path) {
    let too_big = std::fs::metadata(path).map(|m| m.len() > MAX_BYTES).unwrap_or(false);
    if too_big {
        let rotated = path.with_file_name("audit.1.jsonl");
        let _ = std::fs::remove_file(&rotated);
        let _ = std::fs::rename(path, rotated);
    }
}

/// Reads the most recent entries, newest last. Used by the audit resource.
pub fn read_recent(data_dir: &Path, limit: usize) -> Vec<AuditEntry> {
    let path = agent_dir(data_dir).join(AUDIT_FILE);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let mut entries: Vec<AuditEntry> = text
        .lines()
        .rev()
        .take(limit)
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    entries.reverse();
    entries
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_removes_secret_shaped_keys() {
        let v = serde_json::json!({
            "id": "keep",
            "token": "bzd_agt_secret",
            "nested": { "authorization": "Bearer x", "url": "https://a.example" }
        });
        let r = redact(&v);
        assert_eq!(r["id"], "keep");
        assert_eq!(r["token"], "[redacted]");
        assert_eq!(r["nested"]["authorization"], "[redacted]");
        assert_eq!(r["nested"]["url"], "https://a.example");
    }

    #[test]
    fn redact_truncates_oversized_payloads() {
        let big = serde_json::json!({ "blob": "x".repeat(10_000) });
        let r = redact(&big);
        assert_eq!(r["truncated"], true);
        assert!(r["preview"].as_str().unwrap().len() <= 2048);
    }

    #[test]
    fn redact_never_panics_on_multibyte_at_the_cut() {
        // A payload whose 2048th byte falls inside a multi-byte character
        // must truncate at the previous boundary, not panic.
        let mut blob = "é".repeat(1200); // 2 bytes per char
        blob.push_str(&"x".repeat(100));
        let big = serde_json::json!({ "blob": blob });
        let r = redact(&big);
        assert_eq!(r["truncated"], true);
        let preview = r["preview"].as_str().unwrap();
        assert!(preview.len() <= 2048);
        assert!(std::str::from_utf8(preview.as_bytes()).is_ok());
    }

    #[test]
    fn record_and_read_back() {
        let dir = std::env::temp_dir().join(format!("bzdium-audit-{}", uuid::Uuid::new_v4()));
        for i in 0..3 {
            record(
                &dir,
                &AuditEntry {
                    ts: 1000 + i,
                    action: format!("tool_{i}"),
                    transport: "rest".into(),
                    actor: "test".into(),
                    scope: "read".into(),
                    outcome: "ok".into(),
                    duration_ms: Some(1),
                    dry_run: None,
                    error_code: None,
                    args: None,
                },
            );
        }
        let recent = read_recent(&dir, 10);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[2].action, "tool_2");
        assert_eq!(recent[0].action, "tool_0");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_recent_returns_newest_tail_in_order() {
        let dir = std::env::temp_dir().join(format!("bzdium-audit2-{}", uuid::Uuid::new_v4()));
        for i in 0..10 {
            record(
                &dir,
                &AuditEntry {
                    ts: i,
                    action: format!("a{i}"),
                    transport: "mcp".into(),
                    actor: "t".into(),
                    scope: "read".into(),
                    outcome: "ok".into(),
                    duration_ms: None,
                    dry_run: None,
                    error_code: None,
                    args: None,
                },
            );
        }
        let recent = read_recent(&dir, 3);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].action, "a7");
        assert_eq!(recent[2].action, "a9");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
