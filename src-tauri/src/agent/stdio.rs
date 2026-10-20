//! stdio bridge for the --mcp-stdio subcommand.
//!
//! Design constraint that shapes this whole file: bzdium is a single-instance
//! desktop app, and the state the tools mutate (webviews, the active tab, the
//! in-memory service list) exists only inside the running GUI process. A stdio
//! MCP server therefore cannot be a second implementation of the tools; it is
//! a PROXY that forwards JSON-RPC to the loopback listener of the one process
//! that owns the state.
//!
//! This file deliberately does NOT initialise Tauri. It must start in
//! milliseconds from an editor or agent launch, with no window, no webview, no
//! single-instance race, and no second writer for the same JSON files.
//!
//! TRANSPORT DETAIL THAT IS A SECURITY REQUIREMENT
//! ----------------------------------------------
//! The bridge speaks HTTP/1.1 to 127.0.0.1 BY HAND rather than through a
//! general-purpose HTTP client. A client that honours HTTP_PROXY /
//! ALL_PROXY would send a bearer token, and the whole privileged API surface,
//! to whatever proxy the environment names. Environment variables are attacker
//! influenced in exactly the scenarios this layer defends against, so no
//! proxy-aware path is reachable here: the socket is opened to a hard-coded
//! loopback address and nothing else.

use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{json, Value};

use super::http::read_port;
use super::openapi::route;
use super::PORT_FILE;

/// The absolute path the stdio bridge must POST to.
///
/// MCP_PATH is a route FRAGMENT, and every route is mounted under API_PREFIX.
/// Sending the fragment alone made the bridge request /mcp, which the server
/// correctly answered with 404 - so --mcp-stdio failed on every call, which is
/// the one transport every desktop MCP client actually uses. Compose the two
/// consts here so they cannot drift apart.
fn full_mcp_path() -> String {
    route(super::openapi::MCP_PATH)
}

/// Environment variable carrying the bearer token. Deliberately an env var
/// rather than argv: a command line is visible to every process on the machine
/// through the process list.
pub const TOKEN_ENV: &str = "BZDIUM_AGENT_API_KEY";

/// Optional override for the port, so a user running two portable copies can
/// point the bridge at the right instance without editing files.
pub const PORT_ENV: &str = "BZDIUM_AGENT_PORT";

/// Deliberately short: this is a local socket, and a hung bzdium must not hang
/// the editor that launched the bridge.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// The JSON-RPC error code used when bzdium is not reachable, per the task
/// contract. Callers branch on this rather than parsing text.
pub const NOT_RUNNING_CODE: &str = "BZDIUM_NOT_RUNNING";

fn not_running(suggestion: &str, details: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": Value::Null,
        "error": {
            "code": -32000,
            "message": "bzdium is not running, or the Agent Access API is disabled.",
            "data": {
                "agentCode": NOT_RUNNING_CODE,
                "suggestion": suggestion,
                "details": details,
                "httpStatus": 500,
            }
        }
    })
}

/// Resolves the data directory exactly as the GUI does, so the port file is
/// found in the same place.
///
/// This calls the same resolver the app uses rather than re-deriving paths:
/// two independent path derivations is precisely how a bridge ends up reading
/// a stale port file from the wrong profile.
pub fn data_dir() -> std::path::PathBuf {
    crate::storage::resolve_data_dir()
}

/// Where the bridge looks for the port.
pub fn port_file(data_dir: &std::path::Path) -> std::path::PathBuf {
    super::agent_dir(data_dir).join(PORT_FILE)
}

/// Resolves the port: the environment override wins, then the port file.
pub fn resolve_port() -> Result<u16, String> {
    if let Ok(raw) = std::env::var(PORT_ENV) {
        if let Ok(port) = raw.trim().parse::<u16>() {
            if port != 0 {
                return Ok(port);
            }
        }
        return Err(format!(
            "{PORT_ENV} is set to '{raw}', which is not a valid TCP port."
        ));
    }
    let dir = data_dir();
    read_port(&dir).ok_or_else(|| {
        format!(
            "No running bzdium instance was found: {} is missing or unreadable.",
            port_file(&dir).display()
        )
    })
}

/// Reads the bearer token from the environment.
pub fn resolve_token() -> Result<String, String> {
    match std::env::var(TOKEN_ENV) {
        Ok(t) if !t.trim().is_empty() => Ok(t.trim().to_string()),
        _ => Err(format!(
            "{TOKEN_ENV} is not set. Mint an agent token in bzdium Settings, then export it as {TOKEN_ENV}."
        )),
    }
}

/// The HTTP response the bridge got back.
pub struct BridgeResponse {
    pub status: u16,
    pub body: String,
}

/// POSTs one JSON-RPC payload to the running instance over a hand-rolled
/// HTTP/1.1 exchange on a loopback socket.
///
/// Canonical request form is used (Host then Content-Length then the rest) and
/// the Host header names loopback exactly, so the server's DNS-rebinding check
/// passes for a legitimate client. No Origin header is ever sent: this is not
/// a browser.
pub fn post_jsonrpc(port: u16, token: &str, payload: &str) -> Result<BridgeResponse, String> {
    use std::net::TcpStream;

    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
        .map_err(|e| format!("cannot connect to 127.0.0.1:{port}: {e}"))?;
    stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .and_then(|_| stream.set_write_timeout(Some(IO_TIMEOUT)))
        .map_err(|e| format!("cannot configure the socket: {e}"))?;

    let request = format!(
        "POST {path} HTTP/1.1\r\n\
         Host: 127.0.0.1:{port}\r\n\
         Authorization: Bearer {token}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {len}\r\n\
         Accept: application/json\r\n\
         Connection: close\r\n\
         \r\n\
         {payload}",
        path = full_mcp_path(),
        len = payload.len(),
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("cannot send the request: {e}"))?;
    stream.flush().map_err(|e| format!("cannot flush the request: {e}"))?;

    // Connection: close means we read to EOF, so no chunked decoding is needed.
    let mut raw = Vec::new();
    let mut reader = std::io::BufReader::new(&mut stream);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                raw.extend_from_slice(line.as_bytes());
                // Stop once the buffered connection signals the end of input.
                if raw.len() > 8 * 1024 * 1024 {
                    return Err("the response exceeded the 8 MiB bridge limit".to_string());
                }
            }
            Err(e) => return Err(format!("cannot read the response: {e}")),
        }
    }

    let text = String::from_utf8_lossy(&raw).into_owned();
    parse_http_response(&text)
}

/// Parses a raw HTTP/1.1 response. Split out so it is testable without a
/// socket, which is the only practical way to cover every framing edge.
pub fn parse_http_response(raw: &str) -> Result<BridgeResponse, String> {
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .ok_or_else(|| "malformed response: no header terminator".to_string())?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.split_whitespace();
    let _http_version = parts.next();
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("malformed status line: {status_line:?}"))?;
    Ok(BridgeResponse {
        status,
        body: body.to_string(),
    })
}

/// Turns an HTTP-level failure into a JSON-RPC error the caller can act on.
pub fn http_error_response(status: u16, body: &str) -> Value {
    // The server's own error envelope is passed through verbatim: it already
    // carries a code, a message and an actionable suggestion.
    if let Ok(parsed) = serde_json::from_str::<Value>(body) {
        if parsed.get("code").is_some() && parsed.get("suggestion").is_some() {
            let agent_code = parsed["code"].as_str().unwrap_or("INTERNAL_ERROR");
            return json!({
                "jsonrpc": "2.0",
                "id": Value::Null,
                "error": {
                    "code": -32000,
                    "message": parsed.get("message").cloned().unwrap_or(Value::Null),
                    "data": {
                        "agentCode": agent_code,
                        "suggestion": parsed.get("suggestion").cloned().unwrap_or(Value::Null),
                        "details": parsed.get("details").cloned().unwrap_or(Value::Null),
                        "httpStatus": status,
                    }
                }
            });
        }
    }
    json!({
        "jsonrpc": "2.0",
        "id": Value::Null,
        "error": {
            "code": -32000,
            "message": format!("bzdium returned HTTP {status}."),
            "data": {
                "agentCode": "INTERNAL_ERROR",
                "suggestion": "Check the bzdium log. If the Agent Access API was just disabled, re-enable it in Settings and mint a token.",
                "httpStatus": status,
                "body": body.chars().take(500).collect::<String>(),
            }
        }
    })
}

/// Handles one input line. Returns the exact line to write back, or None when
/// the line is blank (nothing to answer).
///
/// Kept separate from the I/O loop so the whole protocol mapping is testable
/// without spawning a process.
pub fn handle_line(line: &str, port: u16, token: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let response = match serde_json::from_str::<Value>(trimmed) {
        Ok(_payload) => match post_jsonrpc(port, token, trimmed) {
            Ok(resp) if (200..300).contains(&resp.status) => {
                if resp.body.trim().is_empty() {
                    // A notification: accepted with no response object.
                    return None;
                }
                resp.body
            }
            Ok(resp) => http_error_response(resp.status, &resp.body).to_string(),
            Err(e) => not_running(
                "Start the bzdium desktop app, wait for its window to appear, then retry.",
                json!({ "reason": e, "port": port }),
            )
            .to_string(),
        },
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": Value::Null,
            "error": {
                "code": -32700,
                "message": format!("The request is not valid JSON: {e}"),
                "data": {
                    "agentCode": "INVALID_PARAMETER",
                    "suggestion": "Write one JSON-RPC 2.0 request object per line.",
                }
            }
        })
        .to_string(),
    };
    Some(response)
}

/// Builds the JSON-RPC error emitted when the bridge cannot start at all.
pub fn startup_failure(reason: &str, suggestion: &str) -> Value {
    not_running(suggestion, json!({ "reason": reason }))
}

/// Runs the stdio MCP bridge. Returns the process exit code.
///
/// Never initialises Tauri. One JSON-RPC request per input line, one response
/// per output line, which is what MCP stdio transport specifies.
pub fn run_stdio_bridge() -> i32 {
    let (port, token) = match (resolve_port(), resolve_token()) {
        (Ok(p), Ok(t)) => (p, t),
        (port, token) => {
            // Both failures are reported on stdout as a valid JSON-RPC error,
            // because a client is reading stdout as a protocol stream and a
            // bare stderr message would look like a hang.
            let reason = match (port, token) {
                (Err(e), _) => e,
                (_, Err(e)) => e,
                _ => unreachable!("a failure in either arm is handled above"),
            };
            let suggestion = if reason.contains(TOKEN_ENV) {
                format!(
                    "Mint an agent token in bzdium Settings, then run the bridge with {TOKEN_ENV} set to it."
                )
            } else {
                "Start the bzdium desktop app and enable the Agent Access API in Settings, then retry."
                    .to_string()
            };
            let mut out = std::io::stdout().lock();
            let _ = writeln!(out, "{}", startup_failure(&reason, &suggestion));
            let _ = out.flush();
            return 1;
        }
    };

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        if let Some(response) = handle_line(&line, port, &token) {
            // A write failure means the client closed the pipe: stop quietly.
            if writeln!(stdout, "{response}").is_err() {
                return 0;
            }
            if stdout.flush().is_err() {
                return 0;
            }
        }
    }
    // stdin closed normally: clean exit, as MCP stdio clients expect.
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_running_error_uses_the_contracted_code_and_a_suggestion() {
        let v = startup_failure("no port file", "start bzdium");
        assert_eq!(v["error"]["data"]["agentCode"], NOT_RUNNING_CODE);
        assert_eq!(v["error"]["data"]["suggestion"], "start bzdium");
        assert_eq!(v["jsonrpc"], "2.0");
        // The code must be a JSON-RPC server error, not a success.
        let code = v["error"]["code"].as_i64().unwrap();
        assert!((-32099..=-32000).contains(&code), "got {code}");
    }

    #[test]
    fn a_missing_token_names_the_env_var_in_the_suggestion() {
        // SAFETY: this test owns the variable for its duration; no other test
        // reads TOKEN_ENV.
        std::env::remove_var(TOKEN_ENV);
        let err = resolve_token().unwrap_err();
        assert!(err.contains(TOKEN_ENV), "got: {err}");
        assert!(err.contains("Mint"), "the message must be actionable: {err}");
    }

    // Environment variables are process-global, so these tests must not run
    // concurrently: one test's remove_var is another's missing override. That
    // race made a_port_override_wins_over_the_port_file fail intermittently
    // with "No running bzdium instance" when a sibling cleared the variable
    // between its set_var and its assertion.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn env_guard() -> std::sync::MutexGuard<'static, ()> {
        match ENV_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    #[test]
    fn a_blank_token_is_rejected() {
        let _env = env_guard();
        std::env::set_var(TOKEN_ENV, "   ");
        assert!(resolve_token().is_err(), "a whitespace token must not be accepted");
        std::env::remove_var(TOKEN_ENV);
    }

    #[test]
    fn a_token_is_returned_trimmed() {
        let _env = env_guard();
        std::env::set_var(TOKEN_ENV, "  bzd_agt_abc  ");
        assert_eq!(resolve_token().unwrap(), "bzd_agt_abc");
        std::env::remove_var(TOKEN_ENV);
    }

    #[test]
    fn a_malformed_port_override_is_reported_rather_than_ignored() {
        let _env = env_guard();
        std::env::set_var(PORT_ENV, "not-a-port");
        let err = resolve_port().unwrap_err();
        assert!(err.contains(PORT_ENV), "got: {err}");
        std::env::remove_var(PORT_ENV);
    }

    #[test]
    fn a_port_override_wins_over_the_port_file() {
        let _env = env_guard();
        std::env::set_var(PORT_ENV, "48765");
        assert_eq!(resolve_port().unwrap(), 48765);
        std::env::remove_var(PORT_ENV);
    }

    #[test]
    fn responses_parse_from_raw_http() {
        let raw = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"jsonrpc\":\"2.0\",\"id\":1}";
        let resp = parse_http_response(raw).unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.body, "{\"jsonrpc\":\"2.0\",\"id\":1}");
    }

    #[test]
    fn a_malformed_response_is_rejected_not_guessed() {
        assert!(parse_http_response("garbage").is_err());
        assert!(parse_http_response("HTTP/1.1\r\n\r\n").is_err());
    }

    #[test]
    fn a_server_error_envelope_is_passed_through_verbatim() {
        let body = r#"{"code":"UNAUTHORIZED","message":"no token","suggestion":"send one"}"#;
        let v = http_error_response(401, body);
        assert_eq!(v["error"]["data"]["agentCode"], "UNAUTHORIZED");
        assert_eq!(v["error"]["data"]["suggestion"], "send one");
        assert_eq!(v["error"]["data"]["httpStatus"], 401);
    }

    #[test]
    fn a_non_envelope_body_still_yields_a_usable_error() {
        let v = http_error_response(502, "<html>nope</html>");
        assert_eq!(v["error"]["data"]["httpStatus"], 502);
        assert!(v["error"]["data"]["suggestion"].as_str().unwrap().len() > 10);
        // The raw body is retained, bounded, for diagnosis.
        assert!(v["error"]["data"]["body"].as_str().unwrap().contains("nope"));
    }

    #[test]
    fn an_invalid_json_line_produces_a_parse_error_without_contacting_the_server() {
        // Port 1 is almost certainly closed; a JSON error must short-circuit
        // before any socket work.
        let out = handle_line("this is not json", 1, "bzd_agt_x").expect("a response line");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["error"]["code"], -32700);
        assert_eq!(v["error"]["data"]["agentCode"], "INVALID_PARAMETER");
    }

    #[test]
    fn a_blank_line_produces_no_output() {
        assert!(handle_line("", 1, "t").is_none());
        assert!(handle_line("   \n", 1, "t").is_none());
    }

    #[test]
    fn an_unreachable_port_yields_not_running() {
        // Nothing is listening on port 1, so this exercises the connect-failure
        // path end to end.
        let out = handle_line("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}", 1, "bzd_agt_x")
            .expect("a response line");
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["error"]["data"]["agentCode"], NOT_RUNNING_CODE);
        assert!(v["error"]["data"]["suggestion"].as_str().unwrap().contains("Start"));
    }

    #[test]
    fn the_port_file_constant_matches_the_http_module() {
        // Guards against the bridge and the server disagreeing on the file.
        assert_eq!(PORT_FILE, "port");
    }

    /// The bridge must address the endpoint the server actually serves.
    ///
    /// MCP_PATH is the fragment "/mcp"; every route is mounted under
    /// API_PREFIX. Composing only the fragment made --mcp-stdio POST to /mcp,
    /// which the router correctly refused with 404 - so the one transport a
    /// desktop MCP client uses failed on every request, while the HTTP tests
    /// and the unit tests all stayed green because they either use the router
    /// directly or never leave the process.
    #[test]
    fn the_bridge_posts_to_the_prefixed_mcp_route() {
        let path = full_mcp_path();
        assert_eq!(path, "/api/v1/agent/mcp");
        assert!(
            path.starts_with(super::super::openapi::API_PREFIX),
            "the bridge must send a path the router recognises: {path}"
        );
        // And the server must resolve exactly that path to the MCP route.
        assert_eq!(
            super::super::http::resolve_route(&path),
            Some(super::super::http::Route::Mcp),
            "the composed path must be the route the server serves"
        );
    }
}
