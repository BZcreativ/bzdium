//! Per-app font customization.
//!
//! WHY a module of its own: every bzdium service is an independent embedded
//! site with its own typography, and users routinely need a different text
//! size per app (a dense chat client next to a reading-heavy mail client).
//! The preference therefore lives on `state::Service`, persists in
//! services.json with the rest of that service, and is delivered to the page
//! as an injected *stylesheet* rather than browser zoom: zoom would also scale
//! images, video and icons — exactly what users do NOT want when they only
//! need bigger text.
//!
//! Delivery follows the same page-side pattern as `badges.rs` and
//! `watchdog.rs`: one bundled initialization script does the work inside the
//! webview and talks back through exactly ONE IPC command
//! (`service_menu_action`), so remote content gains no new attack surface
//! beyond "act on my own service". The CSS is applied through a *constructed*
//! stylesheet, which is exempt from the app's strict `style-src 'self'` CSP and
//! leaves no DOM node for a page to remove; a reused `<style>` element covers
//! engines without constructed stylesheets (older WebKitGTK).
//!
//! The px-scaling pass exists because `:root { font-size }` only affects
//! rem/em-driven text, and most real sites pin text in px. The script measures
//! each text-bearing element with and without a 16 px baseline sheet and only
//! rewrites the elements whose size does NOT react to the baseline (i.e. px
//! declared). Elements already driven by rem are left alone — that is what
//! keeps repeated passes from compounding.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State};

use crate::state::{SharedState, UI_WEBVIEW_LABEL};

/// Default text size for a fresh service.
pub const DEFAULT_SIZE_PX: f64 = 16.0;
/// Lower bound the UI/shortcuts may reach.
pub const MIN_SIZE_PX: f64 = 8.0;
/// Upper bound the UI/shortcuts may reach.
pub const MAX_SIZE_PX: f64 = 40.0;
/// One keyboard step.
pub const STEP_PX: f64 = 1.0;

/// Emitted to the UI webview after a service's config changed.
/// Payload: `{ id, config }`.
pub const EVENT_FONT_CHANGED: &str = "font-changed";
/// Asks the UI webview to open the font settings panel for a service.
/// Payload: `{ id }`.
pub const EVENT_OPEN_FONT_SETTINGS: &str = "open-font-settings";

/// Hard caps used only by [`FontConfig::clamped`] — beyond these a hand-edited
/// services.json would produce unusable CSS (and an unusable UI slider).
const MIN_SIZE_FLOOR: f64 = 1.0;
const MIN_SIZE_CEIL: f64 = 200.0;
const MAX_SIZE_CEIL: f64 = 400.0;
const MAX_STEP_PX: f64 = 20.0;
const MIN_STEP_PX: f64 = 0.5;
/// Guard against a `family` value that is technically safe but absurd; real
/// font family names are far shorter.
const MAX_FAMILY_CHARS: usize = 64;

/// Keyboard accelerators for the per-app font bundle. Format is the canonical
/// accelerator string produced by [`canonical_accel`] (`"Ctrl+Shift+F"`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutBindings {
    pub increase: String,
    pub decrease: String,
    pub reset: String,
    pub open_settings: String,
}

impl Default for ShortcutBindings {
    fn default() -> Self {
        Self {
            increase: "Ctrl+=".to_string(),
            decrease: "Ctrl+-".to_string(),
            reset: "Ctrl+0".to_string(),
            open_settings: "Ctrl+Shift+F".to_string(),
        }
    }
}

/// Per-app typography preference.
///
/// `family = None` means "leave the site's own font stack alone" (only size is
/// enforced). `default_size_px` / `min_size_px` / `max_size_px` / `step_px`
/// travel with the config so the page-side bundle can clamp locally and the UI
/// shows the same numbers the Rust side enforces.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct FontConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    pub size_px: f64,
    pub default_size_px: f64,
    pub min_size_px: f64,
    pub max_size_px: f64,
    pub step_px: f64,
    pub enabled: bool,
    /// Best-effort pass that rescales text sites declare in `px` (see module
    /// docs). ON by default: without it the size setting does nothing on most
    /// real services.
    pub scale_px_text: bool,
    pub shortcuts: ShortcutBindings,
}

impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: None,
            size_px: DEFAULT_SIZE_PX,
            default_size_px: DEFAULT_SIZE_PX,
            min_size_px: MIN_SIZE_PX,
            max_size_px: MAX_SIZE_PX,
            step_px: STEP_PX,
            enabled: true,
            scale_px_text: true,
            shortcuts: ShortcutBindings::default(),
        }
    }
}

impl FontConfig {
    /// Returns a copy whose numbers are inside the supported ranges and whose
    /// family is either safe to embed in CSS or `None`.
    ///
    /// Everything here is defensive: values arrive over IPC from the UI (or
    /// from a hand-edited services.json), so NaN, inverted bounds and huge
    /// steps must not be able to reach generated CSS or the page bundle.
    pub fn clamped(mut self) -> Self {
        self.min_size_px = finite_or(self.min_size_px, MIN_SIZE_PX).clamp(MIN_SIZE_FLOOR, MIN_SIZE_CEIL);
        self.max_size_px = finite_or(self.max_size_px, MAX_SIZE_PX).clamp(MIN_SIZE_FLOOR, MAX_SIZE_CEIL);
        // max must stay strictly above min; nudge whichever side is possible.
        if self.max_size_px <= self.min_size_px {
            self.max_size_px = (self.min_size_px + 1.0).min(MAX_SIZE_CEIL);
        }
        if self.max_size_px <= self.min_size_px {
            self.min_size_px = (self.max_size_px - 1.0).max(MIN_SIZE_FLOOR);
        }
        self.default_size_px =
            finite_or(self.default_size_px, DEFAULT_SIZE_PX).clamp(self.min_size_px, self.max_size_px);
        self.size_px = finite_or(self.size_px, self.default_size_px).clamp(self.min_size_px, self.max_size_px);
        self.step_px = finite_or(self.step_px, STEP_PX).clamp(MIN_STEP_PX, MAX_STEP_PX);
        self.family = self
            .family
            .as_deref()
            .and_then(sanitize_family)
            .map(str::to_string);
        self
    }

    /// One step up; `true` when the value actually moved.
    pub fn increase(&mut self) -> bool {
        if self.is_at_max() {
            return false;
        }
        let next = (self.size_px + self.effective_step()).min(self.max_size_px);
        if next > self.size_px {
            self.size_px = next;
            true
        } else {
            false
        }
    }

    /// One step down; `true` when the value actually moved.
    pub fn decrease(&mut self) -> bool {
        if self.is_at_min() {
            return false;
        }
        let next = (self.size_px - self.effective_step()).max(self.min_size_px);
        if next < self.size_px {
            self.size_px = next;
            true
        } else {
            false
        }
    }

    /// Returns to `default_size_px`; `true` when the value actually moved.
    pub fn reset_size(&mut self) -> bool {
        if (self.size_px - self.default_size_px).abs() < 1e-9 {
            return false;
        }
        self.size_px = self.default_size_px.clamp(self.min_size_px, self.max_size_px);
        true
    }

    pub fn is_at_min(&self) -> bool {
        self.size_px <= self.min_size_px + 1e-9
    }

    pub fn is_at_max(&self) -> bool {
        self.size_px >= self.max_size_px - 1e-9
    }

    /// Serialized form for logs/UI helpers. Serialization of this struct is
    /// infallible in practice; `"{}"` keeps the helper total anyway.
    #[allow(dead_code)] // part of the documented surface; exercised by tests
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// A stored step of 0/NaN would make the buttons dead; the step used for
    /// arithmetic always has a floor, independent of deserialization.
    fn effective_step(&self) -> f64 {
        finite_or(self.step_px, STEP_PX).clamp(MIN_STEP_PX, MAX_STEP_PX)
    }
}

/// The actions the per-app bundle can trigger, and the actions the settings UI
/// edits. Kept in one place so ids cannot drift between Rust, the payload and
/// the page script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutAction {
    Increase,
    Decrease,
    Reset,
    OpenSettings,
}

impl ShortcutAction {
    pub const ALL: [ShortcutAction; 4] = [
        ShortcutAction::Increase,
        ShortcutAction::Decrease,
        ShortcutAction::Reset,
        ShortcutAction::OpenSettings,
    ];

    /// Wire id used by the UI and the page script.
    ///
    /// The Rust side dispatches page actions by their literal names, so this
    /// pair is part of the cross-language vocabulary rather than an internal
    /// helper: kept (and exercised by the tests below) so the two sides cannot
    /// silently drift apart.
    #[allow(dead_code)]
    pub fn id(self) -> &'static str {
        match self {
            ShortcutAction::Increase => "increase",
            ShortcutAction::Decrease => "decrease",
            ShortcutAction::Reset => "reset",
            ShortcutAction::OpenSettings => "openSettings",
        }
    }

    /// Human label used in validation errors (and by the settings UI).
    pub fn label(self) -> &'static str {
        match self {
            ShortcutAction::Increase => "Increase font size",
            ShortcutAction::Decrease => "Decrease font size",
            ShortcutAction::Reset => "Reset font size",
            ShortcutAction::OpenSettings => "Font settings",
        }
    }

    #[allow(dead_code)]
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "increase" => Some(ShortcutAction::Increase),
            "decrease" => Some(ShortcutAction::Decrease),
            "reset" => Some(ShortcutAction::Reset),
            "openSettings" => Some(ShortcutAction::OpenSettings),
            _ => None,
        }
    }

    pub fn binding<'a>(self, bindings: &'a ShortcutBindings) -> &'a str {
        match self {
            ShortcutAction::Increase => &bindings.increase,
            ShortcutAction::Decrease => &bindings.decrease,
            ShortcutAction::Reset => &bindings.reset,
            ShortcutAction::OpenSettings => &bindings.open_settings,
        }
    }
}

// -----------------------------------------------------------------------------
// Value sanitizing helpers
// -----------------------------------------------------------------------------

fn finite_or(value: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        fallback
    }
}

/// Accepts a family only if it is composed of characters that are meaningful
/// inside a font-family name and are harmless in the generated CSS. Braces,
/// semicolons, quotes, slashes, comments/star and control characters are all
/// rejected — they are the pieces a hostile services.json would need to break
/// out of the quoted family or start a new rule.
fn sanitize_family(raw: &str) -> Option<&str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_FAMILY_CHARS {
        return None;
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '.' | '-'))
    {
        return None;
    }
    Some(trimmed)
}

/// CSS number formatting: integers lose their `.0`, fractions keep up to two
/// decimals (`18` / `17.5`). NaN never reaches here (callers clamp first), but
/// the fallback is cheap insurance.
fn format_css_number(value: f64) -> String {
    let value = if value.is_finite() { value } else { DEFAULT_SIZE_PX };
    let rounded = (value * 100.0).round() / 100.0;
    if (rounded - rounded.trunc()).abs() < 1e-9 {
        format!("{}", rounded as i64)
    } else {
        let text = format!("{rounded:.2}");
        text.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

// -----------------------------------------------------------------------------
// Shortcut parsing / validation
// -----------------------------------------------------------------------------

const RESERVED_ACCELS: [&str; 7] = [
    "Ctrl+R",
    "Ctrl+W",
    "Ctrl+Q",
    "Ctrl+Shift+I",
    "Ctrl+Shift+J",
    "Ctrl+F",
    "F5",
];

/// Modifier token normalization. `Ctrl` covers every platform's primary
/// modifier spelling; the page script maps `Ctrl` to Cmd/Meta on macOS.
fn normalize_modifier(token: &str) -> Option<&'static str> {
    match token.to_ascii_lowercase().as_str() {
        "ctrl" | "control" | "cmd" | "command" | "meta" | "super" => Some("Ctrl"),
        "alt" | "option" => Some("Alt"),
        "shift" => Some("Shift"),
        _ => None,
    }
}

/// Parses one non-modifier token into its canonical spelling. The accepted set
/// is deliberately small: letters, digits, F1-F24, the common punctuation keys
/// and a handful of named keys. Anything else is a typo the user should see
/// reported, not silently accepted.
fn parse_key_token(token: &str) -> Result<String, String> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return Err("a shortcut is missing its key".to_string());
    }
    let lower = trimmed.to_ascii_lowercase();
    let named = match lower.as_str() {
        "plus" | "add" => Some("Plus"),
        "minus" | "subtract" => Some("Minus"),
        "space" | "spacebar" => Some("Space"),
        "enter" | "return" => Some("Enter"),
        "escape" | "esc" => Some("Escape"),
        _ => None,
    };
    if let Some(named) = named {
        return Ok(named.to_string());
    }
    if let Some(number) = lower.strip_prefix('f') {
        if !number.is_empty() && number.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(n) = number.parse::<u8>() {
                if (1..=24).contains(&n) {
                    return Ok(format!("F{n}"));
                }
            }
        }
    }
    let mut chars = trimmed.chars();
    let first = chars.next().expect("non-empty token");
    if chars.next().is_some() {
        return Err(format!(
            "\"{trimmed}\" is not a key bzdium recognizes — use a letter, a digit, F1-F24 or a named key"
        ));
    }
    if first.is_ascii_alphabetic() {
        return Ok(first.to_ascii_uppercase().to_string());
    }
    if first.is_ascii_digit() {
        return Ok(first.to_string());
    }
    if "=-+_.,/;'[]\\`".contains(first) {
        return Ok(first.to_string());
    }
    Err(format!(
        "\"{trimmed}\" is not a key bzdium recognizes — use a letter, a digit, F1-F24 or a named key"
    ))
}

/// Normalizes a user-typed accelerator into the canonical form stored in
/// services.json and compared against page key events.
///
/// Canonical form: modifiers in the order Ctrl, Alt, Shift, then one key, all
/// joined with `+` (e.g. `"ctrl + shift + f"` -> `"Ctrl+Shift+F"`). A trailing
/// `+` is read as the plus key itself (`"Ctrl++"` -> `"Ctrl+Plus"`), because
/// `+` is also the separator.
pub fn canonical_accel(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("a shortcut cannot be empty".to_string());
    }

    // Pull trailing '+' characters off as "the plus key". This is the only way
    // to express '+' because it doubles as the separator.
    let mut body = trimmed;
    let mut plus_key = false;
    while body.ends_with('+') {
        body = body[..body.len() - 1].trim_end();
        plus_key = true;
    }

    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut key: Option<String> = None;
    for token in body.split('+').map(str::trim).filter(|t| !t.is_empty()) {
        if let Some(modifier) = normalize_modifier(token) {
            let seen = match modifier {
                "Ctrl" => ctrl,
                "Alt" => alt,
                _ => shift,
            };
            if seen {
                return Err(format!(
                    "the {modifier} modifier appears more than once in \"{trimmed}\""
                ));
            }
            match modifier {
                "Ctrl" => ctrl = true,
                "Alt" => alt = true,
                _ => shift = true,
            }
            continue;
        }
        if key.is_some() {
            return Err(format!(
                "\"{trimmed}\" has more than one key — a shortcut is modifiers plus a single key"
            ));
        }
        key = Some(parse_key_token(token)?);
    }
    if plus_key {
        if key.is_some() {
            return Err(format!(
                "\"{trimmed}\" has more than one key — a shortcut is modifiers plus a single key"
            ));
        }
        key = Some("Plus".to_string());
    }
    let Some(key) = key else {
        return Err(format!(
            "\"{trimmed}\" is only modifiers — add the key that should trigger it"
        ));
    };

    let mut canonical = String::new();
    if ctrl {
        canonical.push_str("Ctrl+");
    }
    if alt {
        canonical.push_str("Alt+");
    }
    if shift {
        canonical.push_str("Shift+");
    }
    canonical.push_str(&key);
    Ok(canonical)
}

/// Equivalence key for duplicate detection: `Ctrl+=` and `Ctrl+Plus` are the
/// same physical shortcut, as are `-`/`Minus`. Canonical strings stay as the
/// user typed them; only conflict detection folds them together.
fn accel_equivalence(canonical: &str) -> String {
    let mut parts: Vec<&str> = canonical.split('+').collect();
    let key = parts.pop().unwrap_or_default();
    let folded = match key {
        "=" | "Plus" => "Plus".to_string(),
        "-" | "Minus" => "Minus".to_string(),
        "Space" => "Space".to_string(),
        other => other.to_string(),
    };
    format!("{}+{}", parts.join("+"), folded)
}

/// Canonicalizes every binding, rejects duplicates and bzdium's own reserved
/// accelerators. Returns the canonicalized copy — callers should persist what
/// this returns, not the raw input, so the page script can compare directly.
pub fn validate_shortcuts(bindings: &ShortcutBindings) -> Result<ShortcutBindings, String> {
    let mut canonical = ShortcutBindings::default();
    for action in ShortcutAction::ALL {
        let raw = action.binding(bindings);
        let value = canonical_accel(raw).map_err(|e| format!("{} shortcut: {e}", action.label()))?;
        if RESERVED_ACCELS.contains(&value.as_str()) {
            return Err(format!(
                "{} cannot use {value} — bzdium reserves that shortcut for the app itself",
                action.label()
            ));
        }
        match action {
            ShortcutAction::Increase => canonical.increase = value,
            ShortcutAction::Decrease => canonical.decrease = value,
            ShortcutAction::Reset => canonical.reset = value,
            ShortcutAction::OpenSettings => canonical.open_settings = value,
        }
    }
    for (index, first) in ShortcutAction::ALL.iter().enumerate() {
        for second in &ShortcutAction::ALL[index + 1..] {
            let first_value = first.binding(&canonical);
            let second_value = second.binding(&canonical);
            if accel_equivalence(first_value) == accel_equivalence(second_value) {
                return Err(format!(
                    "{} and {} are both bound to {first_value} — give them different shortcuts",
                    first.label(),
                    second.label()
                ));
            }
        }
    }
    Ok(canonical)
}

// -----------------------------------------------------------------------------
// CSS generation
// -----------------------------------------------------------------------------

/// Selector for the family pass. Icon fonts live in `::before`/`::after` and in
/// `<i>` elements, so those are excluded — as are code blocks, where a user
/// family would destroy alignment. No blanket `*` rule: that would also hit
/// SVG-internal text, canvas fallbacks and pseudo-elements.
const FAMILY_SELECTOR: &str = "body *:not(i):not(svg):not(svg *):not([class*=\"icon\" i]):not([class*=\"fa-\" i]):not([class*=\"fa \" i]):not([class*=\"material-icons\" i]):not([class*=\"glyph\" i]):not([class*=\"emoji\" i]):not(code):not(pre):not(kbd):not(samp):not(tt)";

/// The stylesheet injected into the service page. Empty when the feature is
/// disabled for this service (the page script then removes whatever it had
/// applied earlier).
pub fn style_css(cfg: &FontConfig) -> String {
    if !cfg.enabled {
        return String::new();
    }
    // Clamp a defensive copy: style_css is also called with configs read
    // straight out of a (possibly hand-edited) services.json.
    let cfg = cfg.clone().clamped();
    let size = format_css_number(cfg.size_px);
    let mut css = format!(
        ":root {{ font-size: {size}px !important; }}\nbody {{ font-size: {size}px !important; }}\n"
    );
    if let Some(family) = cfg.family.as_deref().and_then(sanitize_family) {
        let stack = format!("\"{family}\", -apple-system, \"Segoe UI\", Roboto, Arial, sans-serif");
        css.push_str(&format!(
            ":root, body {{ font-family: {stack} !important; }}\n"
        ));
        css.push_str(&format!(
            "{FAMILY_SELECTOR} {{ font-family: {stack} !important; }}\n"
        ));
    }
    css
}

// -----------------------------------------------------------------------------
// Page payload and scripts
// -----------------------------------------------------------------------------

/// Payload `cfg` object. A dedicated struct (rather than serializing
/// `FontConfig`) keeps the wire shape stable for the page script and always
/// includes `family` (as `null`) so JS never has to distinguish "absent".
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PageCfg<'a> {
    family: Option<&'a str>,
    size_px: f64,
    default_size_px: f64,
    min_size_px: f64,
    max_size_px: f64,
    step_px: f64,
    enabled: bool,
    scale_px_text: bool,
    shortcuts: &'a ShortcutBindings,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PagePayload<'a> {
    css: String,
    home: &'a str,
    cfg: PageCfg<'a>,
}

/// JSON object embedded in the bundle: `{ css, home, cfg }`. Built through
/// serde so nothing user-controlled is ever string-concatenated into JS.
pub fn page_payload(cfg: &FontConfig, home_url: &str) -> String {
    // Sanitize once: even a hand-edited services.json must never put
    // out-of-range numbers into the page bundle.
    let safe = cfg.clone().clamped();
    let payload = PagePayload {
        css: style_css(&safe),
        home: home_url,
        cfg: PageCfg {
            family: safe.family.as_deref().and_then(sanitize_family),
            size_px: safe.size_px,
            default_size_px: safe.default_size_px,
            min_size_px: safe.min_size_px,
            max_size_px: safe.max_size_px,
            step_px: safe.step_px,
            enabled: safe.enabled,
            scale_px_text: safe.scale_px_text,
            shortcuts: &safe.shortcuts,
        },
    };
    serde_json::to_string(&payload)
        .unwrap_or_else(|_| r#"{"css":"","home":"","cfg":{"family":null,"sizePx":16,"enabled":true}}"#.to_string())
}

/// The full document-start bundle: stylesheet application (all frames), plus —
/// top frame only — keyboard shortcuts, the right-click menu and the px-scaling
/// pass. Idempotent by design; see the script's own comments.
pub fn init_script(service_id: &str, cfg: &FontConfig, home_url: &str) -> String {
    debug_assert!(
        uuid::Uuid::parse_str(service_id).is_ok(),
        "service id must be a UUID"
    );
    let id_literal = serde_json::to_string(service_id).unwrap_or_else(|_| "\"\"".to_string());
    let script = FONT_SCRIPT.replace("/*__BZDIUM_ID__*/", &id_literal);
    script.replace("/*__BZDIUM_PAYLOAD__*/", &page_payload(cfg, home_url))
}

/// Live-update script: calls into the already-installed bundle. This is what
/// `apply_live` evals after a config change or a navigation.
pub fn apply_script(cfg: &FontConfig, home_url: &str) -> String {
    format!(
        "window.__bzdiumFont && window.__bzdiumFont.apply({});",
        page_payload(cfg, home_url)
    )
}

/// Evals the live-update script into a service webview. A missing webview is
/// expected (hibernated service): the config is already persisted and the
/// creation-time initialization script applies it when the page comes back.
///
/// Deliberately takes no state lock — callers either hold it or pass values in.
pub fn apply_live(app: &AppHandle, id: &str, cfg: &FontConfig, home_url: &str) {
    let label = crate::webviews::label_for(id);
    let Some(webview) = app.get_webview(&label) else {
        return;
    };
    if let Err(e) = webview.eval(&apply_script(cfg, home_url)) {
        eprintln!("fonts: could not update {label}: {e}");
    }
}

// -----------------------------------------------------------------------------
// Lock-free config registry
// -----------------------------------------------------------------------------

/// Webview label -> (last known font config, service home URL). Mirrors
/// `webviews::WEBVIEW_URLS`, for the same reason: `on_page_load` runs on the
/// event loop and must stay lock-free of the app state mutex (a sync command
/// processed in a nested message pump while that lock is held deadlocks the
/// app). The home URL is cached alongside the config because the "Home" menu
/// entry and re-applying after a navigation both need it, and `on_page_load`
/// must not take the state lock to look it up. It is only a cache —
/// services.json is the source of truth — and is re-primed when a webview is
/// created.
static FONT_CONFIGS: LazyLock<Mutex<HashMap<String, (FontConfig, String)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Remembers the config + home URL for a service (used at webview creation and
/// by every command that changes it). Lock poisoning is ignored, like
/// `record_webview_url`: a poisoned registry only costs one re-application.
pub fn remember(id: &str, cfg: &FontConfig, home_url: &str) {
    if let Ok(mut map) = FONT_CONFIGS.lock() {
        map.insert(id.to_string(), (cfg.clone(), home_url.to_string()));
    }
}

/// The last config + home URL remembered for a service, if any.
pub fn cached(id: &str) -> Option<(FontConfig, String)> {
    FONT_CONFIGS.lock().ok().and_then(|map| map.get(id).cloned())
}

/// Drops a service's cached config (service removal).
pub fn forget(id: &str) {
    if let Ok(mut map) = FONT_CONFIGS.lock() {
        map.remove(id);
    }
}

// -----------------------------------------------------------------------------
// Events
// -----------------------------------------------------------------------------

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FontChangedPayload<'a> {
    id: &'a str,
    config: &'a FontConfig,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct OpenFontSettingsPayload<'a> {
    id: &'a str,
}

fn emit_font_changed(app: &AppHandle, id: &str, cfg: &FontConfig) {
    if let Err(e) = app.emit_to(
        UI_WEBVIEW_LABEL,
        EVENT_FONT_CHANGED,
        FontChangedPayload { id, config: cfg },
    ) {
        eprintln!("failed to emit {EVENT_FONT_CHANGED}: {e}");
    }
}

/// The one fan-out used by every mutating command: remember for
/// `on_page_load`, push to the page, tell the UI, refresh its snapshot.
fn commit(app: &AppHandle, id: &str, cfg: &FontConfig, home_url: &str, snapshot: &crate::state::AppState) {
    remember(id, cfg, home_url);
    apply_live(app, id, cfg, home_url);
    emit_font_changed(app, id, cfg);
    crate::state::emit_snapshot(app, snapshot);
}

// -----------------------------------------------------------------------------
// Commands — settings UI (local webview only)
// -----------------------------------------------------------------------------

/// Current config for a service; defaults for a service that has none.
#[tauri::command]
pub fn get_font_config(state: State<'_, SharedState>, id: String) -> Result<FontConfig, String> {
    let inner = state.lock().map_err(|e| e.to_string())?;
    let service = inner
        .service(&id)
        .ok_or_else(|| format!("unknown service id {id}"))?;
    // Clamp on read: a hand-edited services.json must not leak NaN/huge values
    // into the settings UI.
    Ok(service.font.clone().clamped())
}

/// Replaces a service's config: validates shortcuts, clamps ranges, persists,
/// pushes to the page and notifies the UI.
#[tauri::command]
pub fn set_font_config(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
    config: FontConfig,
) -> Result<crate::state::AppState, String> {
    let shortcuts = validate_shortcuts(&config.shortcuts)?;
    let mut config = config.clamped();
    config.shortcuts = shortcuts;

    let (home_url, snapshot) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let service = inner
            .service_mut(&id)
            .ok_or_else(|| format!("unknown service id {id}"))?;
        service.font = config.clone();
        let home_url = service.url.clone();
        inner.save_services()?;
        (home_url, inner.snapshot())
    };
    commit(&app, &id, &config, &home_url, &snapshot);
    Ok(snapshot)
}

/// Shared increase/decrease/reset path. Persists and applies only when the
/// value actually moved (a key held at a bound must not rewrite disk).
fn adjust_and_apply(
    app: &AppHandle,
    state: &State<'_, SharedState>,
    id: &str,
    action: &str,
) -> Result<FontConfig, String> {
    let (config, home_url, snapshot) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let service = inner
            .service_mut(id)
            .ok_or_else(|| format!("unknown service id {id}"))?;
        let changed = match action {
            "increase" => service.font.increase(),
            "decrease" => service.font.decrease(),
            "reset" => service.font.reset_size(),
            other => return Err(format!("unknown font action {other:?}")),
        };
        if !changed {
            return Ok(service.font.clone());
        }
        let config = service.font.clone();
        let home_url = service.url.clone();
        inner.save_services()?;
        (config, home_url, inner.snapshot())
    };
    commit(app, id, &config, &home_url, &snapshot);
    Ok(config)
}

/// Keyboard shortcut target (UI webview sends the accelerator; the page-side
/// bundle sends the same command after normalizing the key event).
///
/// SECURITY: a webview may only ever change its own service's font. In
/// practice this command is only exposed to the local UI webview, but the
/// guard is kept as defence in depth and mirrors `badges::report_title`.
#[tauri::command]
pub fn adjust_font_size(
    app: AppHandle,
    state: State<'_, SharedState>,
    webview: tauri::Webview<tauri::Wry>,
    id: String,
    action: String,
) -> Result<FontConfig, String> {
    if webview.label() != UI_WEBVIEW_LABEL {
        if webview.label() != crate::webviews::label_for(&id) {
            return Err(
                "adjust_font_size called for a different service than the calling webview".to_string(),
            );
        }
    }
    adjust_and_apply(&app, &state, &id, &action)
}

/// Restores the appearance defaults but KEEPS the user's shortcuts: shortcut
/// layout is muscle memory, not part of "this page looks wrong, reset it".
#[tauri::command]
pub fn reset_font_config(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
) -> Result<crate::state::AppState, String> {
    let (config, home_url, snapshot) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let service = inner
            .service_mut(&id)
            .ok_or_else(|| format!("unknown service id {id}"))?;
        let shortcuts = service.font.shortcuts.clone();
        let config = FontConfig {
            shortcuts,
            ..FontConfig::default()
        };
        service.font = config.clone();
        let home_url = service.url.clone();
        inner.save_services()?;
        (config, home_url, inner.snapshot())
    };
    commit(&app, &id, &config, &home_url, &snapshot);
    Ok(snapshot)
}

/// Restores the default accelerators, leaving appearance untouched.
#[tauri::command]
pub fn reset_font_shortcuts(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
) -> Result<crate::state::AppState, String> {
    let (config, home_url, snapshot) = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let service = inner
            .service_mut(&id)
            .ok_or_else(|| format!("unknown service id {id}"))?;
        service.font.shortcuts = ShortcutBindings::default();
        let config = service.font.clone();
        let home_url = service.url.clone();
        inner.save_services()?;
        (config, home_url, inner.snapshot())
    };
    commit(&app, &id, &config, &home_url, &snapshot);
    Ok(snapshot)
}

/// Copies one service's appearance settings to every other service.
///
/// Only the user-facing knobs travel (family, size, enabled, px scaling,
/// shortcuts). Each service keeps its own bounds/step and URL — bounds are
/// per-config policy, not appearance, and a service's own home URL must never
/// be replaced by another service's.
#[tauri::command]
pub fn apply_font_to_all(
    app: AppHandle,
    state: State<'_, SharedState>,
    id: String,
) -> Result<crate::state::AppState, String> {
    let mut targets: Vec<(String, FontConfig, String)> = Vec::new();
    let snapshot = {
        let mut inner = state.lock().map_err(|e| e.to_string())?;
        let source = inner
            .service(&id)
            .map(|service| service.font.clone().clamped())
            .ok_or_else(|| format!("unknown service id {id}"))?;
        for service in inner.services.iter_mut() {
            if service.id == id {
                continue;
            }
            service.font.family = source.family.clone();
            service.font.size_px = source.size_px;
            service.font.enabled = source.enabled;
            service.font.scale_px_text = source.scale_px_text;
            service.font.shortcuts = source.shortcuts.clone();
            targets.push((service.id.clone(), service.font.clone(), service.url.clone()));
        }
        inner.save_services()?;
        inner.snapshot()
    };
    // Webview evals happen off-lock (webview calls must not hold the state
    // mutex — see the locking contract in webviews.rs).
    for (target_id, config, home_url) in &targets {
        remember(target_id, config, home_url);
        apply_live(&app, target_id, config, home_url);
        emit_font_changed(&app, target_id, config);
    }
    crate::state::emit_snapshot(&app, &snapshot);
    Ok(snapshot)
}

/// Curated font families offered by the settings UI. The UI adds its own
/// "System default" entry (which maps to `family = None`); this list is only
/// families that can actually be requested by name.
#[tauri::command]
pub fn list_font_families() -> Vec<String> {
    [
        "Arial",
        "Roboto",
        "Inter",
        "Open Sans",
        "Lato",
        "Noto Sans",
        "Segoe UI",
        "Helvetica Neue",
        "Verdana",
        "Tahoma",
        "Trebuchet MS",
        "Georgia",
        "Times New Roman",
        "Courier New",
    ]
    .iter()
    .map(|family| family.to_string())
    .collect()
}

// -----------------------------------------------------------------------------
// Commands — page side (the ONLY command remote content may call)
// -----------------------------------------------------------------------------

/// The single page-side entry point for the right-click menu and the keyboard
/// shortcuts. Remote pages can invoke it — so it may only ever act on the
/// service that owns the calling webview, and every action routes through
/// existing, already-guarded code.
#[tauri::command]
pub async fn service_menu_action(
    app: AppHandle,
    state: State<'_, SharedState>,
    webview: tauri::Webview<tauri::Wry>,
    id: String,
    action: String,
) -> Result<(), String> {
    if webview.label() != crate::webviews::label_for(&id) {
        return Err(
            "service_menu_action called for a different service than the calling webview".to_string(),
        );
    }
    match action.as_str() {
        "font-increase" => {
            adjust_and_apply(&app, &state, &id, "increase")?;
            Ok(())
        }
        "font-decrease" => {
            adjust_and_apply(&app, &state, &id, "decrease")?;
            Ok(())
        }
        "font-reset" => {
            adjust_and_apply(&app, &state, &id, "reset")?;
            Ok(())
        }
        "font-settings" => {
            // UI-only event: the page never learns anything back.
            if let Err(e) = app.emit_to(
                UI_WEBVIEW_LABEL,
                EVENT_OPEN_FONT_SETTINGS,
                OpenFontSettingsPayload { id: id.as_str() },
            ) {
                eprintln!("failed to emit {EVENT_OPEN_FONT_SETTINGS}: {e}");
            }
            Ok(())
        }
        // Reuse the existing recovery paths instead of reimplementing them:
        // a hard reload is the only reload that clears a wedged WebView2
        // network stack, and open_service_external already validates schemes.
        "reload" => crate::webviews::schedule_hard_reload(&app, &id).map(|_| ()),
        "open-external" => crate::webviews::open_service_external(state, id),
        other => Err(format!("unknown service menu action {other:?}")),
    }
}

// -----------------------------------------------------------------------------
// The page-side bundle
// -----------------------------------------------------------------------------

/// The JavaScript injected as an initialization script. Kept as a const with
/// two textual placeholders (`/*__BZDIUM_ID__*/` and `/*__BZDIUM_PAYLOAD__*/`)
/// so the script can live as readable JS while the id and the serde-built
/// payload are substituted at runtime. `id` is a UUID and the payload is JSON,
/// so substitution cannot break out of an expression.
///
/// Contract highlights:
/// - CSS runs in EVERY frame (iframes are content); UI runs top-frame only.
/// - One constructed stylesheet, adopted (CSP-exempt, no DOM node); a single
///   reused `<style>` element is the fallback for older WebKitGTK.
/// - One `keydown` listener, one `contextmenu` listener — flagged so a second
///   injection can never double them.
/// - The px pass rewrites only elements the baseline sheet proves are px-pinned,
///   tracks what it touched and restores it when the feature is disabled.
/// - No `innerHTML`, no `eval`, no unbounded scans; all IPC through the
///   captured `__TAURI__.core.invoke`.
const FONT_SCRIPT: &str = r##"(function () {
  'use strict';
  var PAYLOAD = /*__BZDIUM_PAYLOAD__*/;
  var ID = /*__BZDIUM_ID__*/;
  var isTop = !(window.top !== window.self);
  var cfg = PAYLOAD.cfg || {};
  var state = { css: '', home: '', degraded: false, bindings: {} };
  var isMac = /Mac|iPhone|iPad/.test(navigator.platform || '');
  var nativeComputedStyle = window.getComputedStyle;
  var invoke = null;

  // Re-entrancy guard: an initialization script can run more than once for a
  // document (webview re-created, SPA calling document.open). The first copy
  // owns every listener and observer; later injections only refresh its
  // payload, so nothing is ever installed twice.
  if (window.__bzdiumFont && typeof window.__bzdiumFont.apply === 'function') {
    try { window.__bzdiumFont.apply(PAYLOAD); } catch (e) {}
    return;
  }

  // ---------------------------------------------------------------------------
  // Stylesheet application (runs in ALL frames — iframes are content too)
  // ---------------------------------------------------------------------------
  var sheet = null;
  var styleEl = null;
  var BASE_CSS = ':root, body { font-size: 16px !important; }';

  // Splits the generated (flat) stylesheet into individual rules so the
  // fallback <style> element can be filled through CSSOM. Quote-aware, so a
  // font name containing a brace can never desynchronize the split.
  function splitRules(css) {
    var rules = [];
    var start = 0;
    var depth = 0;
    var quote = null;
    for (var i = 0; i < css.length; i++) {
      var ch = css.charAt(i);
      if (quote) {
        if (ch === '\\') { i++; continue; }
        if (ch === quote) { quote = null; }
        continue;
      }
      if (ch === '"' || ch === "'") { quote = ch; continue; }
      if (ch === '{') { depth++; }
      else if (ch === '}') {
        depth--;
        if (depth === 0) {
          var rule = css.slice(start, i + 1).replace(/^\s+|\s+$/g, '');
          if (rule) { rules.push(rule); }
          start = i + 1;
        }
      }
    }
    return rules;
  }

  // Creates/adopts the single stylesheet we own. Constructed stylesheets are
  // exempt from the app's `style-src 'self'` CSP and leave no DOM node, so they
  // are preferred; the <style> fallback is for engines without them.
  function ensureSheet() {
    if (sheet) {
      var adopted = document.adoptedStyleSheets || [];
      for (var i = 0; i < adopted.length; i++) { if (adopted[i] === sheet) { return; } }
      try { document.adoptedStyleSheets = adopted.concat([sheet]); } catch (e) {}
      return;
    }
    try {
      var nativeSheet = typeof CSSStyleSheet === 'function' &&
        typeof CSSStyleSheet.prototype.replaceSync === 'function' &&
        'adoptedStyleSheets' in Document.prototype;
      if (nativeSheet) {
        sheet = new CSSStyleSheet();
        document.adoptedStyleSheets = (document.adoptedStyleSheets || []).concat([sheet]);
        return;
      }
    } catch (e) { sheet = null; }
    if (styleEl && styleEl.parentNode) { return; }
    var id = '__bzdium-font';
    styleEl = document.getElementById(id);
    if (!styleEl) {
      styleEl = document.createElement('style');
      styleEl.id = id;
    }
    var root = document.head || document.documentElement || document.body;
    if (!root) { return; } // too early; retried on readystatechange
    root.appendChild(styleEl);
  }

  function applyCss(css) {
    state.css = css || '';
    try {
      ensureSheet();
      if (sheet) {
        sheet.replaceSync(state.css);
        return;
      }
      if (!styleEl) { return; }
      var elementSheet = styleEl.sheet;
      if (elementSheet) {
        while (elementSheet.cssRules.length > 0) { elementSheet.deleteRule(0); }
        var rules = splitRules(state.css);
        for (var i = 0; i < rules.length; i++) {
          try { elementSheet.insertRule(rules[i], elementSheet.cssRules.length); }
          catch (e) { break; }
        }
      } else {
        // CSP refused to give the element a sheet: try textContent (honoured
        // by some engines) and record that we are degraded — never throw.
        styleEl.textContent = state.css;
        state.degraded = true;
      }
    } catch (e) {
      state.degraded = true;
    }
  }

  function ensureAttached() {
    try { applyCss(state.css); } catch (e) {}
  }

  // Re-attaches after SPA route swaps (documentElement replaced) and after a
  // hostile page removes our <style> element. Deliberately observes only
  // document/documentElement/head direct children — a cheap check, not a
  // subtree scan on every mutation.
  var attachmentObserver = null;
  function installAttachmentObservers() {
    if (attachmentObserver || typeof MutationObserver !== 'function') { return; }
    try {
      attachmentObserver = new MutationObserver(function () {
        try {
          ensureAttached();
          if (document.documentElement) { attachmentObserver.observe(document.documentElement, { childList: true }); }
          if (document.head) { attachmentObserver.observe(document.head, { childList: true }); }
        } catch (e) {}
      });
      attachmentObserver.observe(document, { childList: true });
      if (document.documentElement) { attachmentObserver.observe(document.documentElement, { childList: true }); }
      if (document.head) { attachmentObserver.observe(document.head, { childList: true }); }
    } catch (e) { attachmentObserver = null; }
  }

  function onReady() {
    ensureAttached();
    installAttachmentObservers();
    installPassObserver();
    if (isTop && cfg && cfg.enabled && cfg.scalePxText) { requestPass(); }
  }

  function installReadyHooks() {
    if (window.__bzdiumReadyHooks) { return; }
    window.__bzdiumReadyHooks = true;
    document.addEventListener('readystatechange', onReady);
    document.addEventListener('DOMContentLoaded', onReady);
    if (document.readyState !== 'loading') { onReady(); }
  }

  // ---------------------------------------------------------------------------
  // IPC — capture the invoke function in this closure so a page clobbering
  // window.__TAURI__ later cannot redirect our calls (and a missing global is
  // simply a no-op, never an exception).
  // ---------------------------------------------------------------------------
  function captureInvoke() {
    try {
      var tauri = window.__TAURI__;
      if (tauri && tauri.core && tauri.core.invoke) { invoke = tauri.core.invoke; }
    } catch (e) {}
  }
  captureInvoke();

  function sendAction(action) {
    try {
      if (!invoke) { captureInvoke(); } // late injection (`withGlobalTauri` timing)
      if (!invoke) { return; }
      invoke('service_menu_action', { id: ID, action: action });
    } catch (e) {}
  }

  // ---------------------------------------------------------------------------
  // Accelerator normalization (must match fonts::canonical_accel on the Rust
  // side: modifiers Ctrl, Alt, Shift in that order, then one key token).
  // ---------------------------------------------------------------------------
  function normKey(raw) {
    var key = String(raw || '').toLowerCase();
    if (key === 'plus' || key === '+' || key === '=') { return '+'; }
    if (key === 'minus' || key === '-' || key === '_') { return '-'; }
    if (key === 'space' || key === 'spacebar') { return ' '; }
    if (key === 'enter' || key === 'return') { return 'enter'; }
    if (key === 'escape' || key === 'esc') { return 'escape'; }
    return key;
  }

  function parseAccel(accel) {
    if (typeof accel !== 'string' || !accel) { return null; }
    var parts = accel.split('+');
    var mods = { ctrl: false, alt: false, shift: false };
    var key = null;
    for (var i = 0; i < parts.length; i++) {
      var token = parts[i].replace(/^\s+|\s+$/g, '');
      if (!token) { continue; }
      var lower = token.toLowerCase();
      if (lower === 'ctrl' || lower === 'control' || lower === 'cmd' || lower === 'command' || lower === 'meta' || lower === 'super') { mods.ctrl = true; continue; }
      if (lower === 'alt' || lower === 'option') { mods.alt = true; continue; }
      if (lower === 'shift') { mods.shift = true; continue; }
      key = normKey(lower);
    }
    if (!key) { return null; }
    return { ctrl: mods.ctrl, alt: mods.alt, shift: mods.shift, key: key };
  }

  function eventAccel(event) {
    var key = event.key;
    if (!key || key === 'Unidentified' || key === 'Dead') { return null; }
    var shift = !!event.shiftKey;
    if (key === '+') {
      shift = false; // Shift+= types '+' but it is the same physical key as '='
    } else if (key === '_') {
      key = '-';     // Shift+- types '_' but it is the same physical key as '-'
      shift = false;
    }
    return {
      ctrl: isMac ? !!event.metaKey : !!event.ctrlKey,
      alt: !!event.altKey,
      shift: shift,
      key: normKey(key)
    };
  }

  function sameAccel(a, b) {
    return !!a && !!b && a.ctrl === b.ctrl && a.alt === b.alt && a.shift === b.shift && a.key === b.key;
  }

  function installShortcuts() {
    if (window.__bzdiumKeysInstalled) { return; }
    window.__bzdiumKeysInstalled = true;
    window.addEventListener('keydown', function (event) {
      try {
        if (event.isComposing) { return; } // IME composition owns the key
        if (!event.key) { return; }
        var pressed = eventAccel(event);
        if (!pressed) { return; }
        var actions = ['font-increase', 'font-decrease', 'font-reset', 'font-settings'];
        var editable = isEditableTarget(event.target);
        for (var i = 0; i < actions.length; i++) {
          if (!sameAccel(pressed, state.bindings[actions[i]])) { continue; }
          var isSettings = actions[i] === 'font-settings';
          // Overrides off: no appearance actions, but settings stays a way
          // back in (the menu rows are greyed; this shortcut is the escape).
          if (!isSettings && (!cfg || cfg.enabled === false)) { return; }
          // A focused text field keeps its own editing shortcuts (Ctrl+B,
          // Ctrl+-, …): the site's handling must win there. Opening settings
          // is navigation, not editing, and stays available.
          if (editable && !isSettings) { return; }
          event.preventDefault();
          event.stopPropagation();
          sendAction(actions[i]);
          return;
        }
      } catch (e) {}
    }, true);
  }

  // ---------------------------------------------------------------------------
  // Right-click menu (top frame only)
  // ---------------------------------------------------------------------------
  var menuEl = null;

  function isEditableTarget(target) {
    if (!target || !target.tagName) { return false; }
    var tag = target.tagName.toLowerCase();
    if (tag === 'input' || tag === 'textarea' || tag === 'select') { return true; }
    if (target.isContentEditable) { return true; }
    var editable = null;
    try { editable = target.getAttribute ? target.getAttribute('contenteditable') : null; } catch (e) {}
    return editable === '' || editable === 'true' || editable === 'plaintext-only';
  }

  function closeMenu() {
    if (!menuEl) { return; }
    try { if (menuEl.parentNode) { menuEl.parentNode.removeChild(menuEl); } } catch (e) {}
    menuEl = null;
  }

  function separator() {
    var line = document.createElement('div');
    line.style.height = '1px';
    line.style.margin = '4px 2px';
    line.style.background = '#2c3150';
    return line;
  }

  // bzdium dark palette; per-element style assignments are CSP-safe (no
  // inline <style> element, and no markup injection anywhere).
  function menuRow(label, hint, disabled, run) {
    var row = document.createElement('div');
    row.setAttribute('role', 'menuitem');
    row.style.display = 'flex';
    row.style.alignItems = 'center';
    row.style.justifyContent = 'space-between';
    row.style.padding = '5px 10px';
    row.style.borderRadius = '4px';
    row.style.cursor = disabled ? 'default' : 'pointer';
    row.style.opacity = disabled ? '0.45' : '1';
    var name = document.createElement('span');
    name.textContent = label;
    row.appendChild(name);
    if (hint) {
      var meta = document.createElement('span');
      meta.textContent = hint;
      meta.style.marginLeft = '28px';
      meta.style.fontSize = '11px';
      meta.style.opacity = '0.7';
      meta.style.whiteSpace = 'nowrap';
      row.appendChild(meta);
    }
    if (!disabled) {
      row.addEventListener('mouseenter', function () { row.style.background = '#232743'; });
      row.addEventListener('mouseleave', function () { row.style.background = 'transparent'; });
      row.addEventListener('click', function (event) {
        if (event) { event.preventDefault(); event.stopPropagation(); }
        closeMenu();
        try { run(); } catch (e) {}
      });
    }
    return row;
  }

  function openMenu(x, y) {
    closeMenu(); // only ONE menu exists at a time
    try {
      var enabled = !!cfg.enabled;
      var offHint = enabled ? '' : 'overrides off';
      var size = Math.round(Number(cfg.sizePx || 16) * 100) / 100;
      var sizeText = size + 'px';
      var atMax = Number(cfg.sizePx) >= Number(cfg.maxSizePx) - 0.001;
      var atMin = Number(cfg.sizePx) <= Number(cfg.minSizePx) + 0.001;
      var shortcuts = cfg.shortcuts || {};

      function fontHint(accel) {
        if (!enabled) { return offHint; }
        return sizeText + (accel ? '  ' + accel : '');
      }

      var menu = document.createElement('div');
      menu.id = '__bzdium-menu';
      menu.setAttribute('role', 'menu');
      var style = menu.style;
      style.position = 'fixed';
      style.zIndex = '2147483647';
      style.minWidth = '230px';
      style.padding = '4px';
      style.background = '#15172b';
      style.color = '#e7e9f3';
      style.border = '1px solid #2c3150';
      style.borderRadius = '6px';
      style.boxShadow = '0 8px 24px rgba(0, 0, 0, 0.45)';
      style.font = '13px system-ui, -apple-system, "Segoe UI", Roboto, Arial, sans-serif';
      style.userSelect = 'none';

      var rows = [];
      rows.push(menuRow('Increase font size', fontHint(shortcuts.increase), !enabled || atMax, function () { sendAction('font-increase'); }));
      rows.push(menuRow('Decrease font size', fontHint(shortcuts.decrease), !enabled || atMin, function () { sendAction('font-decrease'); }));
      rows.push(menuRow('Reset font size', enabled ? (shortcuts.reset || '') : offHint, !enabled, function () { sendAction('font-reset'); }));
      rows.push(menuRow('Font settings…', enabled ? (shortcuts.openSettings || '') : offHint, !enabled, function () { sendAction('font-settings'); }));
      rows.push(separator());
      rows.push(menuRow('Reload', '', false, function () { sendAction('reload'); }));
      rows.push(menuRow('Back', '', false, function () { history.back(); }));
      rows.push(menuRow('Forward', '', false, function () { history.forward(); }));
      rows.push(menuRow('Home', '', false, function () { if (state.home) { location.assign(state.home); } }));
      rows.push(menuRow('Open in browser', '', false, function () { sendAction('open-external'); }));
      rows.push(separator());
      var selection = '';
      try { selection = window.getSelection ? String(window.getSelection() || '') : ''; } catch (e) {}
      if (selection.replace(/\s+/g, '') !== '') {
        rows.push(menuRow('Copy', '', false, function () { try { document.execCommand('copy'); } catch (e) {} }));
      }
      rows.push(menuRow('Select all', '', false, function () { try { document.execCommand('selectAll'); } catch (e) {} }));

      for (var i = 0; i < rows.length; i++) { menu.appendChild(rows[i]); }
      menu.style.left = Math.max(4, x) + 'px';
      menu.style.top = Math.max(4, y) + 'px';
      (document.body || document.documentElement).appendChild(menu);
      try {
        var rect = menu.getBoundingClientRect();
        var maxLeft = (window.innerWidth || 0) - rect.width - 4;
        var maxTop = (window.innerHeight || 0) - rect.height - 4;
        if (rect.left > maxLeft) { menu.style.left = Math.max(4, maxLeft) + 'px'; }
        if (rect.top > maxTop) { menu.style.top = Math.max(4, maxTop) + 'px'; }
      } catch (e) {}
      menuEl = menu;
    } catch (e) {
      closeMenu();
    }
  }

  function installMenu() {
    if (window.__bzdiumMenuInstalled) { return; }
    window.__bzdiumMenuInstalled = true;
    window.addEventListener('contextmenu', function (event) {
      try {
        // Editable fields keep WebView2's native menu (Paste/undo must work).
        if (isEditableTarget(event.target)) { return; }
        event.preventDefault();
        openMenu(event.clientX || 0, event.clientY || 0);
      } catch (e) {}
    }, true);
    window.addEventListener('keydown', function (event) {
      if (menuEl && (event.key === 'Escape' || event.key === 'Esc')) {
        closeMenu();
        event.preventDefault();
        event.stopPropagation();
      }
    }, true);
    document.addEventListener('mousedown', function (event) {
      if (menuEl && !(menuEl === event.target || menuEl.contains(event.target))) { closeMenu(); }
    }, true);
    window.addEventListener('blur', closeMenu);
    window.addEventListener('scroll', closeMenu, true);
    window.addEventListener('resize', closeMenu);
  }

  // ---------------------------------------------------------------------------
  // px-text scaling pass (top frame only)
  // ---------------------------------------------------------------------------
  var touched = new Map(); // element -> [previous inline value, priority]
  var passTimer = null;
  var lastPassAt = 0;
  var mutationTimer = null;
  var mutationPassTimer = null;
  var lastMutationPassAt = 0;
  var passObserver = null;

  var SKIP_SELECTOR = 'script,style,noscript,template,svg,img,video,canvas,iframe,object,embed,audio,source,track,#__bzdium-menu,#__bzdium-menu *';
  var ICON_SELECTOR = '[class*="icon" i],[class*="fa-" i],[class*="material-icons" i],[class*="glyph" i],[class*="emoji" i]';

  function restoreTouched() {
    try {
      touched.forEach(function (previous, element) {
        try {
          if (previous[0]) { element.style.setProperty('font-size', previous[0], previous[1] || ''); }
          else { element.style.removeProperty('font-size'); }
        } catch (e) {}
      });
    } finally {
      touched.clear();
    }
  }

  function computedFontSize(element) {
    try {
      var computed = nativeComputedStyle ? nativeComputedStyle.call(window, element) : window.getComputedStyle(element);
      return parseFloat(computed.fontSize) || 0;
    } catch (e) { return 0; }
  }

  function hasDirectText(element) {
    var nodes = element.childNodes;
    for (var i = 0; i < nodes.length; i++) {
      if (nodes[i].nodeType === 3 && /\S/.test(nodes[i].nodeValue || '')) { return true; }
    }
    return false;
  }

  function collectCandidates() {
    var out = [];
    var body = document.body;
    if (!body || typeof document.createTreeWalker !== 'function') { return out; }
    var walker = document.createTreeWalker(body, 1, null); // SHOW_ELEMENT
    var element = walker.nextNode();
    // Two budgets: candidates we keep, and nodes we are willing to visit.
    // The visit budget is what protects huge SPAs (Gmail/Twitter-class DOMs):
    // without it a pass walks the whole tree with a closest() call per node.
    var visited = 0;
    while (element && out.length < 1200 && visited < 6000) {
      visited++;
      var skip = false;
      try {
        if (typeof element.closest === 'function') {
          if (element.closest(SKIP_SELECTOR) || element.closest(ICON_SELECTOR)) { skip = true; }
        } else if (element.tagName && /^(SCRIPT|STYLE|NOSCRIPT|SVG|IMG|VIDEO|CANVAS|IFRAME|OBJECT|EMBED)$/.test(element.tagName)) {
          skip = true;
        }
      } catch (e) { skip = true; }
      if (!skip && hasDirectText(element)) { out.push(element); }
      element = walker.nextNode();
    }
    return out;
  }

  function runPxPass() {
    if (!cfg || !cfg.enabled || !cfg.scalePxText) { return; }
    var body = document.body;
    if (!body) { return; }
    var ratio = Number(cfg.sizePx) / (Number(cfg.defaultSizePx) || 16);
    if (!(ratio > 0) || !(ratio < 1000)) { return; }
    restoreTouched(); // never compound a previous factor
    if (Math.abs(ratio - 1) < 0.001) { return; }
    var candidates = collectCandidates();
    if (candidates.length === 0) { return; }
    var i;
    var before = [];
    var baseline = [];
    for (i = 0; i < candidates.length; i++) { before[i] = computedFontSize(candidates[i]); }
    // Measure with a baseline sheet: elements whose size follows the baseline
    // are rem/em-driven (already handled by the :root rule); elements whose
    // size ignores it are px-declared and need the explicit override.
    var userCss = state.css;
    try {
      applyCss(BASE_CSS);
      for (i = 0; i < candidates.length; i++) { baseline[i] = computedFontSize(candidates[i]); }
    } finally {
      applyCss(userCss);
    }
    for (i = 0; i < candidates.length; i++) {
      var element = candidates[i];
      var sizeA = before[i];
      var sizeB = baseline[i];
      if (!(sizeB > 0)) { continue; }
      if (Math.abs(sizeA - sizeB) > 0.5) { continue; } // rem-driven: leave alone
      var next = sizeB * ratio;
      if (!(next > 0)) { continue; }
      try {
        var previousValue = element.style.getPropertyValue('font-size');
        var previousPriority = element.style.getPropertyPriority('font-size');
        touched.set(element, [previousValue, previousPriority]);
        element.style.setProperty('font-size', next + 'px', 'important');
      } catch (e) {}
    }
  }

  // User-initiated passes (config change, shortcut) run at most once per
  // 150 ms; a queued request coalesces into one timer.
  function requestPass() {
    if (passTimer) { return; }
    var wait = 150 - (Date.now() - lastPassAt);
    if (wait < 0) { wait = 0; }
    passTimer = window.setTimeout(function () {
      passTimer = null;
      lastPassAt = Date.now();
      try { runPxPass(); } catch (e) {}
    }, wait);
  }

  // Mutation-driven passes are rate-limited much harder than user ones: chat
  // and mail pages mutate continuously, and a 1200-element double-reflow pass
  // must never run back to back. At most one per 1200 ms, and none while the
  // page is hidden (a human cannot see the layout we would be measuring).
  function scheduleMutationPass() {
    if (mutationPassTimer) { return; }
    var wait = 1200 - (Date.now() - lastMutationPassAt);
    if (wait < 250) { wait = 250; }
    mutationPassTimer = window.setTimeout(function () {
      mutationPassTimer = null;
      if (!cfg || !cfg.enabled || !cfg.scalePxText) { return; }
      if (document.hidden) { return; }
      lastMutationPassAt = Date.now();
      lastPassAt = Date.now();
      try { runPxPass(); } catch (e) {}
    }, wait);
  }

  function installPassObserver() {
    // No px scaling requested: never pay for an observer that cannot pass.
    if (!cfg || !cfg.enabled || !cfg.scalePxText) { return; }
    if (passObserver || typeof MutationObserver !== 'function') { return; }
    var root = document.body || document.documentElement;
    if (!root) { return; }
    try {
      // 250 ms debounce after the last mutation, then the 1200 ms mutation
      // budget: dynamic content (chat messages, virtual lists) is covered
      // without a pass per mutation. One observer for the page's lifetime.
      passObserver = new MutationObserver(function () {
        if (mutationTimer) { return; }
        mutationTimer = window.setTimeout(function () {
          mutationTimer = null;
          scheduleMutationPass();
        }, 250);
      });
      passObserver.observe(root, { childList: true, subtree: true });
    } catch (e) { passObserver = null; }
  }

  // ---------------------------------------------------------------------------
  // Public API + first application
  // ---------------------------------------------------------------------------
  function updatePayload(payload) {
    if (!payload || typeof payload !== 'object') { return; }
    cfg = payload.cfg || { enabled: false };
    state.home = (typeof payload.home === 'string') ? payload.home : '';
    state.css = (typeof payload.css === 'string') ? payload.css : '';
    var shortcuts = cfg.shortcuts || {};
    state.bindings = {
      'font-increase': parseAccel(shortcuts.increase),
      'font-decrease': parseAccel(shortcuts.decrease),
      'font-reset': parseAccel(shortcuts.reset),
      'font-settings': parseAccel(shortcuts.openSettings)
    };
  }

  function apply(payload) {
    updatePayload(payload);
    applyCss(state.css); // EVERY frame, top or not
    installReadyHooks();
    installAttachmentObservers();
    if (!isTop) { return; } // everything below is top-frame UI only
    installShortcuts();
    installMenu();
    if (cfg.enabled && cfg.scalePxText) {
      installPassObserver();
      requestPass();
    } else {
      restoreTouched();
    }
  }

  window.__bzdiumFont = {
    apply: function (payload) { try { apply(payload); } catch (e) {} },
    config: function () { return cfg; }
  };
  window.__bzdiumFont.apply(PAYLOAD);
})();"##;

// -----------------------------------------------------------------------------
// Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_ids() -> (&'static str, &'static str) {
        (
            "00000000-0000-4000-8000-00000000aaa1",
            "00000000-0000-4000-8000-00000000aaa2",
        )
    }

    #[test]
    fn service_json_without_font_gets_defaults() {
        // services.json files written before this feature existed must load
        // with the default config, including the default shortcuts.
        let json = r#"{"id":"00000000-0000-4000-8000-00000000aaa1","name":"Chat","url":"https://example.com/","icon":"CH","enabled":true,"order":0}"#;
        let service: crate::state::Service = serde_json::from_str(json).unwrap();
        assert_eq!(service.font, FontConfig::default());
        assert_eq!(service.font.size_px, DEFAULT_SIZE_PX);
        assert!(service.font.enabled);
        assert!(service.font.scale_px_text);
        assert!(service.font.family.is_none());
    }

    #[test]
    fn clamping_brings_every_value_into_range() {
        // Below the floor.
        let cfg = FontConfig {
            size_px: 2.0,
            min_size_px: 0.2,
            ..FontConfig::default()
        }
        .clamped();
        assert!(cfg.min_size_px >= 1.0);
        assert!(cfg.size_px >= cfg.min_size_px);

        // Above the ceiling.
        let cfg = FontConfig {
            size_px: 9999.0,
            ..FontConfig::default()
        }
        .clamped();
        assert!(cfg.size_px <= MAX_SIZE_PX);
        assert!(cfg.size_px <= cfg.max_size_px);

        // NaN everywhere must not survive.
        let cfg = FontConfig {
            size_px: f64::NAN,
            default_size_px: f64::NAN,
            min_size_px: f64::NAN,
            max_size_px: f64::NAN,
            step_px: f64::NAN,
            ..FontConfig::default()
        }
        .clamped();
        assert!(cfg.size_px.is_finite());
        assert!(cfg.default_size_px.is_finite());
        assert!(cfg.min_size_px.is_finite());
        assert!(cfg.max_size_px.is_finite());
        assert!(cfg.step_px.is_finite());
        assert!(cfg.min_size_px <= cfg.default_size_px);
        assert!(cfg.default_size_px <= cfg.max_size_px);

        // Inverted bounds are repaired.
        let cfg = FontConfig {
            size_px: 20.0,
            default_size_px: 20.0,
            min_size_px: 30.0,
            max_size_px: 10.0,
            ..FontConfig::default()
        }
        .clamped();
        assert!(cfg.max_size_px > cfg.min_size_px);
        assert!(cfg.size_px >= cfg.min_size_px && cfg.size_px <= cfg.max_size_px);
        assert!(cfg.default_size_px >= cfg.min_size_px && cfg.default_size_px <= cfg.max_size_px);

        // A huge step is capped; a tiny one is lifted.
        let cfg = FontConfig {
            step_px: 1e9,
            ..FontConfig::default()
        }
        .clamped();
        assert!(cfg.step_px >= MIN_STEP_PX && cfg.step_px <= MAX_STEP_PX);
        let cfg = FontConfig {
            step_px: 0.01,
            ..FontConfig::default()
        }
        .clamped();
        assert!(cfg.step_px >= MIN_STEP_PX);
    }

    #[test]
    fn stepping_stops_at_both_bounds_and_reset_works() {
        let mut cfg = FontConfig::default();
        assert!(cfg.increase());
        assert_eq!(cfg.size_px, DEFAULT_SIZE_PX + STEP_PX);
        while cfg.increase() {}
        assert_eq!(cfg.size_px, MAX_SIZE_PX);
        assert!(cfg.is_at_max());
        assert!(!cfg.increase());
        assert!(cfg.decrease());
        assert_eq!(cfg.size_px, MAX_SIZE_PX - STEP_PX);
        while cfg.decrease() {}
        assert_eq!(cfg.size_px, MIN_SIZE_PX);
        assert!(cfg.is_at_min());
        assert!(!cfg.decrease());
        assert!(cfg.reset_size());
        assert_eq!(cfg.size_px, DEFAULT_SIZE_PX);
        assert!(!cfg.reset_size());
    }

    #[test]
    fn family_sanitization_accepts_real_names_and_rejects_hostile_ones() {
        let accepted = FontConfig {
            family: Some("  Inter  ".to_string()),
            ..FontConfig::default()
        }
        .clamped();
        assert_eq!(accepted.family.as_deref(), Some("Inter"));

        for evil in [
            "Bad } .x{",
            "a\"b",
            "x;color:red",
            "a\\b",
            "a/b",
            "a<b",
            "line\nbreak",
        ] {
            let cfg = FontConfig {
                family: Some(evil.to_string()),
                ..FontConfig::default()
            }
            .clamped();
            assert_eq!(cfg.family, None, "{evil:?} must be rejected");
        }

        // Empty / whitespace-only family means "system default".
        for empty in ["", "   ", "\t\n"] {
            let cfg = FontConfig {
                family: Some(empty.to_string()),
                ..FontConfig::default()
            }
            .clamped();
            assert_eq!(cfg.family, None);
        }

        // The generated CSS must never contain the raw hostile text.
        let hostile = FontConfig {
            family: Some("Bad } .x{".to_string()),
            ..FontConfig::default()
        };
        let css = style_css(&hostile);
        assert!(!css.contains("Bad"));
        assert!(!css.contains("} .x{"));
    }

    #[test]
    fn style_css_disabled_is_empty_and_enabled_is_expected() {
        let disabled = FontConfig {
            enabled: false,
            ..FontConfig::default()
        };
        assert_eq!(style_css(&disabled), "");

        let enabled = FontConfig {
            size_px: 18.0,
            ..FontConfig::default()
        };
        let css = style_css(&enabled);
        assert!(css.contains("font-size: 18px !important"), "{css}");
        assert!(css.contains(":root"));
        assert!(css.contains("body"));
        // Icon fonts live in pseudo-elements and must never be restyled.
        assert!(!css.contains("::before"));
        assert!(!css.contains("::after"));

        let family = FontConfig {
            size_px: 18.0,
            family: Some("Inter".to_string()),
            ..FontConfig::default()
        };
        let css = style_css(&family);
        assert!(css.contains("\"Inter\""), "{css}");
        assert!(css.contains("font-family: \"Inter\", -apple-system, \"Segoe UI\", Roboto, Arial, sans-serif"));
        assert!(css.contains(":not(code)"));
        assert!(!css.contains("::before") && !css.contains("::after"));

        // Fractional sizes stay readable, not "17.500000".
        let fraction = FontConfig {
            size_px: 17.5,
            ..FontConfig::default()
        };
        assert!(style_css(&fraction).contains("font-size: 17.5px !important"));
    }

    #[test]
    fn canonical_accel_normalizes_and_reports_errors() {
        assert_eq!(canonical_accel("ctrl + shift + f").unwrap(), "Ctrl+Shift+F");
        assert_eq!(canonical_accel("Control+Alt+Shift+X").unwrap(), "Ctrl+Alt+Shift+X");
        assert_eq!(canonical_accel("cmd+f").unwrap(), "Ctrl+F");
        assert_eq!(canonical_accel("option+shift+2").unwrap(), "Alt+Shift+2");
        assert_eq!(canonical_accel("Ctrl+=").unwrap(), "Ctrl+=");
        assert_eq!(canonical_accel("Ctrl++").unwrap(), "Ctrl+Plus");
        assert_eq!(canonical_accel("f5").unwrap(), "F5");
        assert_eq!(canonical_accel("ctrl+0").unwrap(), "Ctrl+0");
        assert_eq!(canonical_accel("Ctrl+/").unwrap(), "Ctrl+/");
        assert_eq!(canonical_accel("Shift+F12").unwrap(), "Shift+F12");

        assert!(canonical_accel("").is_err());
        assert!(canonical_accel("   ").is_err());
        // Only modifiers, no key.
        assert!(canonical_accel("Ctrl+Shift").is_err());
        // Duplicate modifier.
        assert!(canonical_accel("Ctrl+Shift+Ctrl+X").is_err());
        // Two keys.
        assert!(canonical_accel("Ctrl+A+B").is_err());
        // Unknown token / function key out of range.
        assert!(canonical_accel("Hyper+F").is_err());
        assert!(canonical_accel("Ctrl+F25").is_err());
        assert!(canonical_accel("Ctrl+ä").is_err());
    }

    #[test]
    fn validate_shortcuts_canonicalizes_rejects_duplicates_and_reserved() {
        assert_eq!(
            validate_shortcuts(&ShortcutBindings::default()).unwrap(),
            ShortcutBindings::default()
        );

        let mut bindings = ShortcutBindings::default();
        bindings.increase = " ctrl + shift + u ".to_string();
        assert_eq!(validate_shortcuts(&bindings).unwrap().increase, "Ctrl+Shift+U");

        // Duplicates name both actions (the toast must be actionable).
        let mut bindings = ShortcutBindings::default();
        bindings.decrease = "ctrl+=".to_string();
        let error = validate_shortcuts(&bindings).unwrap_err();
        assert!(error.contains("Increase font size"), "{error}");
        assert!(error.contains("Decrease font size"), "{error}");

        // '=' and 'Plus' are the same physical key.
        let mut bindings = ShortcutBindings::default();
        bindings.reset = "Ctrl+Plus".to_string();
        bindings.increase = "Ctrl+=".to_string();
        assert!(validate_shortcuts(&bindings).is_err());

        // Reserved accelerators are refused for every slot.
        for (slot, accel) in [
            (ShortcutAction::Increase, "Ctrl+R"),
            (ShortcutAction::Decrease, "ctrL+w"),
            (ShortcutAction::Reset, "Ctrl+Shift+I"),
            (ShortcutAction::OpenSettings, "F5"),
        ] {
            let mut bindings = ShortcutBindings::default();
            match slot {
                ShortcutAction::Increase => bindings.increase = accel.to_string(),
                ShortcutAction::Decrease => bindings.decrease = accel.to_string(),
                ShortcutAction::Reset => bindings.reset = accel.to_string(),
                ShortcutAction::OpenSettings => bindings.open_settings = accel.to_string(),
            }
            let error = validate_shortcuts(&bindings).unwrap_err();
            assert!(error.contains(slot.label()), "{error}");
            assert!(error.contains(&canonical_accel(accel).unwrap()), "{error}");
        }
    }

    #[test]
    fn shortcut_actions_round_trip_ids_and_bindings() {
        assert_eq!(ShortcutAction::ALL.len(), 4);
        for action in ShortcutAction::ALL {
            assert_eq!(ShortcutAction::from_id(action.id()), Some(action));
            assert!(!action.label().is_empty());
            assert!(!action.binding(&ShortcutBindings::default()).is_empty());
        }
        assert_eq!(ShortcutAction::from_id("nope"), None);
    }

    #[test]
    fn page_payload_is_valid_json_with_css_and_cfg() {
        let cfg = FontConfig {
            size_px: 18.0,
            family: Some("Inter".to_string()),
            ..FontConfig::default()
        };
        let payload = page_payload(&cfg, "https://example.com/start");
        let value: serde_json::Value = serde_json::from_str(&payload).expect("payload must be JSON");
        assert!(value["css"]
            .as_str()
            .unwrap()
            .contains("font-size: 18px !important"));
        assert_eq!(value["home"], "https://example.com/start");
        assert_eq!(value["cfg"]["family"], "Inter");
        assert_eq!(value["cfg"]["sizePx"], 18.0);
        assert_eq!(value["cfg"]["defaultSizePx"], 16.0);
        assert_eq!(value["cfg"]["minSizePx"], 8.0);
        assert_eq!(value["cfg"]["maxSizePx"], 40.0);
        assert_eq!(value["cfg"]["stepPx"], 1.0);
        assert_eq!(value["cfg"]["enabled"], true);
        assert_eq!(value["cfg"]["scalePxText"], true);
        assert_eq!(value["cfg"]["shortcuts"]["increase"], "Ctrl+=");
        assert_eq!(value["cfg"]["shortcuts"]["openSettings"], "Ctrl+Shift+F");

        // A hostile family never reaches the payload (it is clamped away).
        let evil = FontConfig {
            family: Some("Bad } .x{".to_string()),
            ..FontConfig::default()
        };
        let value: serde_json::Value =
            serde_json::from_str(&page_payload(&evil, "https://example.com/")).unwrap();
        assert!(value["cfg"]["family"].is_null());
        assert!(!value["css"].as_str().unwrap().contains("Bad"));

        // Quotes are escaped by serde, not concatenated: the payload always
        // parses, including with a family that contains spaces.
        let spaced = FontConfig {
            family: Some("Noto Sans".to_string()),
            ..FontConfig::default()
        };
        serde_json::from_str::<serde_json::Value>(&page_payload(&spaced, "https://x.example/"))
            .expect("serialized payload stays valid JSON");
    }

    #[test]
    fn init_script_contains_the_bundle() {
        let cfg = FontConfig {
            size_px: 18.0,
            family: Some("Inter".to_string()),
            ..FontConfig::default()
        };
        let script = init_script("00000000-0000-4000-8000-00000000abc3", &cfg, "https://example.com/");
        assert!(script.contains("\"00000000-0000-4000-8000-00000000abc3\""));
        assert!(script.contains("font-size: 18px"));
        assert!(script.contains("\"family\":\"Inter\""));
        assert!(script.contains("https://example.com/"));
        // Required script mechanics.
        assert!(script.contains("window.top !== window.self"));
        assert!(script.contains("contextmenu"));
        assert!(script.contains("adoptedStyleSheets"));
        assert!(script.contains("service_menu_action"));
        assert!(script.contains("__bzdiumFont"));
        assert!(script.contains("__bzdiumKeysInstalled"));
        assert!(script.contains("CSSStyleSheet"));
        assert!(script.contains("MutationObserver"));
        // Performance guards the review pinned: mutation passes are budgeted
        // (1200 ms) and the candidate walk is node-bounded (6000), and no pass
        // runs while the page is hidden.
        assert!(script.contains("lastMutationPassAt"));
        assert!(script.contains("1200"));
        assert!(script.contains("visited < 6000"));
        assert!(script.contains("document.hidden"));
        // Shortcut scope: editing fields keep their own size shortcuts, but
        // settings remains reachable even when overrides are disabled.
        assert!(script.contains("isEditableTarget(event.target)"));
        assert!(script.contains("isSettings"));
        // Safety properties: no HTML injection, no eval.
        assert!(!script.contains("innerHTML"));
        assert!(!script.contains("eval("));

        // Disabled config still yields a valid bundle whose css is empty.
        let disabled = FontConfig {
            enabled: false,
            ..FontConfig::default()
        };
        let script = init_script("00000000-0000-4000-8000-00000000abc3", &disabled, "https://example.com/");
        assert!(script.contains("\"css\":\"\""));
        assert!(script.contains("\"enabled\":false"));
    }

    #[test]
    fn apply_script_calls_the_installed_api() {
        let script = apply_script(&FontConfig::default(), "https://example.com/");
        assert!(script.starts_with("window.__bzdiumFont"));
        assert!(script.contains(".apply("));
        assert!(script.contains("\"sizePx\":16.0"));
        let value_part = script
            .trim_start_matches("window.__bzdiumFont && window.__bzdiumFont.apply(")
            .trim_end_matches(");");
        serde_json::from_str::<serde_json::Value>(value_part).expect("embedded payload is JSON");
    }

    #[test]
    fn font_registry_is_per_service_and_overwrites() {
        let (a, b) = default_ids();
        forget(a);
        forget(b);
        assert!(cached(a).is_none());

        let config_a = FontConfig {
            size_px: 20.0,
            ..FontConfig::default()
        };
        let config_b = FontConfig {
            size_px: 12.0,
            ..FontConfig::default()
        };
        let home_a = "https://a.example/";
        let home_b = "https://b.example/";
        remember(a, &config_a, home_a);
        remember(b, &config_b, home_b);
        assert_eq!(cached(a), Some((config_a.clone(), home_a.to_string())));
        assert_eq!(cached(b), Some((config_b.clone(), home_b.to_string())));

        // Overwriting one service must not leak into the other — neither the
        // config nor its home URL.
        let config_a2 = FontConfig {
            size_px: 22.0,
            family: Some("Inter".to_string()),
            ..FontConfig::default()
        };
        remember(a, &config_a2, "https://a.example/next");
        let (cached_a, cached_home_a) = cached(a).unwrap();
        assert_eq!(cached_a.size_px, 22.0);
        assert_eq!(cached_home_a, "https://a.example/next");
        let (cached_b, cached_home_b) = cached(b).unwrap();
        assert_eq!(cached_b.size_px, 12.0);
        assert_eq!(cached_home_b, home_b);
        assert!(cached_b.family.is_none());

        forget(a);
        assert!(cached(a).is_none());
        assert!(cached(b).is_some());
        forget(b);
        assert!(cached(b).is_none());
    }

    #[test]
    fn font_family_list_is_curated_and_excludes_the_system_entry() {
        let families = list_font_families();
        assert_eq!(families.first().map(String::as_str), Some("Arial"));
        assert!(families.contains(&"Segoe UI".to_string()));
        assert!(families.contains(&"Noto Sans".to_string()));
        // The UI adds "System default" itself (family = None).
        assert!(!families.iter().any(|family| family == "System default"));
        assert!(families.len() >= 10);
    }

    #[test]
    fn to_json_is_camel_case_and_round_trips() {
        let json = FontConfig::default().to_json();
        assert!(json.contains("\"sizePx\":16.0"), "{json}");
        assert!(json.contains("\"scalePxText\":true"));
        assert!(json.contains("\"minSizePx\":8.0"));
        assert!(!json.contains("\"family\"")); // None is skipped
        let back: FontConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, FontConfig::default());
    }
}
