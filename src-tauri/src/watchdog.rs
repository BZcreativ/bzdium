//! Watchdog: automatic recovery for wedged service webviews.
//!
//! Failure mode (observed with Gmail on WebView2 156.0.4314.50, 2026-10-28):
//! a service webview's network stack dies while the page keeps rendering its
//! last frame. Gmail then sits on "Not connected. Connecting in 3:26…" with
//! every data request rejected (`Uncaught (in promise) _.Ltb` in the console)
//! and never recovers on its own. Only replacing the browser process helps —
//! which is exactly what `reload_service`'s hard reload does; a human just
//! has to notice first. This module notices: it probes each running service
//! webview on a timer and hard-reloads one that has stopped answering while
//! the machine is still online.
//!
//! Design mirrors `badges.rs`: a small script injected into every service
//! webview (top frame only) does the probing, because only code *inside* the
//! page exercises that webview's own network stack; a Rust-side timer
//! triggers it and counts results. A wedged page either reports `ok:false`
//! or (more often) never reports at all — both count as misses.

use std::collections::HashMap;
use std::net::TcpStream;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};

use crate::state::SharedState;

/// How often each running service webview is probed.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(45);

/// A webview is declared wedged after this many consecutive probe rounds
/// without a healthy answer (~3 minutes at the default interval — long
/// enough that a slow page or a short network blip never trips it).
pub const MISS_THRESHOLD: u64 = 4;

/// Freshly created webviews are not judged for this long: a page that is
/// still loading may legitimately miss a round or two.
pub const GRACE_PERIOD: Duration = Duration::from_secs(2 * PROBE_INTERVAL.as_secs());

/// Upper bound on watchdog interventions: at most [`MAX_AUTO_RELOADS`]
/// hard reloads per service within [`RELOAD_BUDGET_WINDOW`]. A site that is
/// genuinely down must not be reload-spammed; beyond the budget the service
/// waits for a human.
pub const MAX_AUTO_RELOADS: usize = 2;
pub const RELOAD_BUDGET_WINDOW: Duration = Duration::from_secs(30 * 60);

/// Time the page-side probe gives its fetch before declaring it hung. Must
/// stay well below [`PROBE_INTERVAL`] so a hung fetch cannot overlap the
/// next round.
const PROBE_TIMEOUT_MS: u64 = 10_000;

/// Per-webview probe bookkeeping. Keyed by webview label.
#[derive(Debug, Clone, PartialEq)]
struct Track {
    /// Probe round counter; incremented every time a probe is triggered.
    expected: u64,
    /// Highest round this webview answered with `ok: true`.
    last_ok: u64,
    /// When the webview (re)appeared; probes inside the grace period are
    /// not counted against it.
    first_seen: Instant,
    /// Instants of watchdog-initiated hard reloads (bounded history).
    reloads: Vec<Instant>,
}

impl Track {
    fn new(now: Instant) -> Self {
        Self {
            expected: 0,
            last_ok: 0,
            first_seen: now,
            reloads: Vec::new(),
        }
    }

    fn misses(&self) -> u64 {
        self.expected.saturating_sub(self.last_ok)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    /// Answering normally (or not enough evidence yet) — nothing to do.
    Waiting,
    /// No answers and out of intervention budget — a human must look.
    GivenUp,
    /// Wedged: no answers past the threshold, machine online, budget left.
    Reload,
}

/// Pure decision: is this webview wedged, and may we still intervene?
fn judge(track: &Track, now: Instant) -> Verdict {
    if now.duration_since(track.first_seen) < GRACE_PERIOD {
        return Verdict::Waiting;
    }
    if track.misses() < MISS_THRESHOLD {
        return Verdict::Waiting;
    }
    if reload_budget_available(&track.reloads, now) {
        Verdict::Reload
    } else {
        Verdict::GivenUp
    }
}

/// True when another watchdog reload is within budget: fewer than
/// [`MAX_AUTO_RELOADS`] recent reloads, or the oldest of them is older than
/// [`RELOAD_BUDGET_WINDOW`].
fn reload_budget_available(reloads: &[Instant], now: Instant) -> bool {
    let recent: Vec<&Instant> = reloads
        .iter()
        .filter(|t| now.duration_since(**t) < RELOAD_BUDGET_WINDOW)
        .collect();
    recent.len() < MAX_AUTO_RELOADS
}

/// Records a watchdog reload and resets the probe accounting: the webview is
/// about to be replaced, so rounds and answers start over.
fn record_reload(track: &mut Track, now: Instant) {
    track.reloads.push(now);
    track.reloads.sort();
    let keep = track.reloads.len().saturating_sub(MAX_AUTO_RELOADS);
    track.reloads.drain(..keep);
    track.expected = 0;
    track.last_ok = 0;
    track.first_seen = now;
}

static TRACKS: LazyLock<Mutex<HashMap<String, Track>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// JavaScript injected into every service webview (top frame only — same
/// rationale as `badges::init_script`). Defines `window.__bzdiumProbe(seq)`,
/// which fetches the page's own origin with `cache: 'no-store'` and reports
/// whether that request settled. A wedged network stack never settles, so
/// the report never arrives: silence is the signal.
pub fn probe_script(service_id: &str) -> String {
    debug_assert!(
        uuid::Uuid::parse_str(service_id).is_ok(),
        "service id must be a UUID"
    );
    format!(
        r#"(function () {{
  if (window.top !== window.self) {{ return; }}
  var inflight = false;
  window.__bzdiumProbe = function (seq) {{
    if (inflight || !window.__TAURI__) {{ return; }}
    inflight = true;
    var done = false;
    var finish = function (ok) {{
      if (done) {{ return; }}
      done = true;
      inflight = false;
      try {{
        window.__TAURI__.core.invoke('report_probe', {{ id: {service_id:?}, seq: seq, ok: ok ? 1 : 0 }});
      }} catch (e) {{}}
    }};
    try {{
      var url = location.origin + '/favicon.ico?__bzdium_probe=' + seq;
      var signal = (typeof AbortSignal !== 'undefined' && AbortSignal.timeout)
        ? AbortSignal.timeout({PROBE_TIMEOUT_MS})
        : undefined;
      fetch(url, {{ cache: 'no-store', signal: signal }})
        .then(function () {{ finish(true); }}, function () {{ finish(false); }});
      // Belt and braces for engines without AbortSignal.timeout: the fetch
      // promise must settle one way or the other within the timeout.
      window.setTimeout(function () {{ finish(false); }}, {PROBE_TIMEOUT_MS} + 2000);
    }} catch (e) {{ finish(false); }}
  }};
}})();"#
    )
}

/// Conservative machine-online gate: the watchdog must never reload a page
/// just because the network went down — that punishes the page for the
/// machine's sake. A plain TCP connect to a well-known anycast resolver
/// (no DNS, std-only). If this check itself cannot run, fail closed.
fn network_up() -> bool {
    let addr: std::net::SocketAddr = "1.1.1.1:443".parse().expect("static socket address");
    TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok()
}

/// Starts the watchdog loop. One thread, one pass every [`PROBE_INTERVAL`]:
/// trigger a probe in every live service webview, then act on the verdicts.
/// The setting is re-read every pass, so toggling it in Settings takes
/// effect within one interval.
pub fn start_watchdog_task(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(PROBE_INTERVAL);
        tick(&app);
    });
}

fn tick(app: &AppHandle) {
    let enabled = app
        .state::<SharedState>()
        .lock()
        .map(|inner| inner.settings.auto_recover)
        .unwrap_or(false);
    let now = Instant::now();

    // Phase 1: bookkeeping + probe triggers. `eval` is fire-and-forget; the
    // answer (if any) arrives asynchronously through `report_probe`.
    let live_labels: Vec<String> = app
        .webviews()
        .iter()
        .map(|(label, _)| label.to_string())
        .filter(|label| label.starts_with("service-"))
        .collect();
    {
        let Ok(mut tracks) = TRACKS.lock() else {
            return;
        };
        if !enabled {
            tracks.clear();
            return;
        }
        // Forget webviews that are gone (hibernated, closed, reloaded).
        tracks.retain(|label, _| live_labels.contains(label));
        for label in &live_labels {
            let track = tracks
                .entry(label.clone())
                .or_insert_with(|| Track::new(now));
            track.expected += 1;
            if let Some(webview) = app.get_webview(label) {
                let seq = track.expected;
                let _ = webview.eval(&format!(
                    "window.__bzdiumProbe && window.__bzdiumProbe({seq})"
                ));
            }
        }
    }

    // Phase 2: act on verdicts. The reload path manages its own locking and
    // runs its webview work on a separate thread (see webviews.rs), so no
    // locks are held across it.
    let mut reloads: Vec<String> = Vec::new();
    {
        let Ok(tracks) = TRACKS.lock() else { return };
        for (label, track) in tracks.iter() {
            if judge(track, now) != Verdict::Reload {
                continue;
            }
            // Cheap local check, run once per candidate only.
            if !network_up() {
                continue;
            }
            if let Some(id) = label.strip_prefix("service-") {
                reloads.push(id.to_string());
            }
        }
    }
    for id in reloads {
        {
            let Ok(mut tracks) = TRACKS.lock() else {
                continue;
            };
            if let Some(track) = tracks.get_mut(&crate::webviews::label_for(&id)) {
                record_reload(track, now);
            }
        }
        let name = app
            .state::<SharedState>()
            .lock()
            .ok()
            .and_then(|inner| inner.service(&id).map(|s| s.name.clone()))
            .unwrap_or_else(|| "This page".to_string());
        crate::state::emit_notice(
            app,
            &format!("{name} stopped responding — reloading it automatically."),
        );
        if let Err(e) = crate::webviews::schedule_hard_reload(app, &id) {
            eprintln!("watchdog: hard reload of {id} failed: {e}");
        }
    }
}

/// Command invoked BY service webviews (injected probe script). Records the
/// answer for the calling webview. Like `report_title`, the `id` must match
/// the calling webview's label — a webview can only ever speak for itself.
/// A page spoofing `ok` merely suppresses its own reload; a page spoofing
/// failure merely reloads itself. Neither harms anything else.
#[tauri::command]
pub fn report_probe(
    webview: tauri::Webview<tauri::Wry>,
    id: String,
    seq: u64,
    ok: u8,
) -> Result<(), String> {
    if webview.label() != crate::webviews::label_for(&id) {
        return Err(
            "report_probe called for a different service than the calling webview".to_string(),
        );
    }
    if ok == 0 {
        return Ok(());
    }
    let mut tracks = TRACKS
        .lock()
        .map_err(|e| format!("watchdog state poisoned: {e}"))?;
    if let Some(track) = tracks.get_mut(webview.label()) {
        // Clamp: a page cannot fast-forward its own liveness into future
        // rounds it has not been asked about.
        track.last_ok = seq.min(track.expected);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track_with_misses(misses: u64, now: Instant) -> Track {
        Track {
            expected: misses,
            last_ok: 0,
            first_seen: now - GRACE_PERIOD - Duration::from_secs(1),
            reloads: Vec::new(),
        }
    }

    #[test]
    fn fresh_webviews_get_grace() {
        let now = Instant::now();
        let mut track = Track::new(now);
        track.expected = MISS_THRESHOLD + 2; // silent since creation
        assert_eq!(judge(&track, now), Verdict::Waiting);
        assert_eq!(
            judge(&track, now + GRACE_PERIOD + Duration::from_secs(1)),
            Verdict::Reload
        );
    }

    #[test]
    fn threshold_must_be_reached() {
        let now = Instant::now();
        assert_eq!(
            judge(&track_with_misses(MISS_THRESHOLD - 1, now), now),
            Verdict::Waiting
        );
        assert_eq!(
            judge(&track_with_misses(MISS_THRESHOLD, now), now),
            Verdict::Reload
        );
    }

    #[test]
    fn answered_probes_reset_misses() {
        let now = Instant::now();
        let mut track = track_with_misses(0, now);
        track.expected = 10;
        track.last_ok = 9; // answered everything but the last round
        assert_eq!(track.misses(), 1);
        assert_eq!(judge(&track, now), Verdict::Waiting);
    }

    #[test]
    fn reload_budget_caps_interventions() {
        let now = Instant::now();
        // A webview that has been silent past grace and threshold, with the
        // given watchdog-reload history.
        fn wedged(now: Instant, reloads: Vec<Instant>) -> Track {
            Track {
                expected: MISS_THRESHOLD,
                last_ok: 0,
                first_seen: now - GRACE_PERIOD - Duration::from_secs(1),
                reloads,
            }
        }

        // no intervention yet → reload
        assert_eq!(judge(&wedged(now, vec![]), now), Verdict::Reload);
        // one recent reload, wedged again → a second one is still in budget
        assert_eq!(
            judge(&wedged(now, vec![now - Duration::from_secs(60)]), now),
            Verdict::Reload
        );
        // two recent reloads, wedged again → budget exhausted, wait for a human
        assert_eq!(
            judge(
                &wedged(
                    now,
                    vec![
                        now - Duration::from_secs(60),
                        now - Duration::from_secs(120)
                    ]
                ),
                now
            ),
            Verdict::GivenUp
        );
        // …but a reload that aged out of the window frees the budget again
        assert_eq!(
            judge(
                &wedged(
                    now,
                    vec![
                        now - RELOAD_BUDGET_WINDOW - Duration::from_secs(60),
                        now - Duration::from_secs(60)
                    ],
                ),
                now
            ),
            Verdict::Reload
        );
    }

    #[test]
    fn record_reload_resets_accounting() {
        let now = Instant::now();
        let mut track = track_with_misses(MISS_THRESHOLD, now);
        record_reload(&mut track, now);
        assert_eq!(track.expected, 0);
        assert_eq!(track.last_ok, 0);
        assert_eq!(track.reloads.len(), 1);
        // and it is back inside the grace period
        assert_eq!(judge(&track, now), Verdict::Waiting);
    }

    #[test]
    fn seq_is_clamped_to_expected() {
        // Direct exercise of the clamp logic used by report_probe.
        let mut track = Track::new(Instant::now());
        track.expected = 5;
        // a hostile page reporting a future round must not gain credit for it
        track.last_ok = 99u64.min(track.expected);
        assert_eq!(track.last_ok, 5);
    }

    #[test]
    fn probe_script_shape() {
        let script = probe_script("00000000-0000-4000-8000-00000000abc2");
        assert!(
            script.contains("window.top !== window.self"),
            "top frame only"
        );
        assert!(script.contains("report_probe"));
        assert!(script.contains("\"00000000-0000-4000-8000-00000000abc2\""));
        assert!(
            script.contains("no-store"),
            "probe must bypass the HTTP cache"
        );
        assert!(script.contains("AbortSignal.timeout"));
    }

    #[test]
    fn budget_helper_boundaries() {
        let now = Instant::now();
        assert!(reload_budget_available(&[], now));
        assert!(reload_budget_available(
            &[now - Duration::from_secs(1)],
            now
        ));
        // two recent reloads: out of budget…
        assert!(!reload_budget_available(
            &[
                now - Duration::from_secs(60),
                now - Duration::from_secs(120)
            ],
            now
        ));
        // …unless the older one has aged out of the window
        assert!(reload_budget_available(
            &[
                now - RELOAD_BUDGET_WINDOW - Duration::from_secs(60),
                now - Duration::from_secs(60)
            ],
            now
        ));
    }
}
