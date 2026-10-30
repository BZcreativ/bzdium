//! Per-app font customization: cross-module persistence and boundary tests.
//!
//! WHY these live in `tests/` and not in `fonts.rs`: the unit tests inside the
//! module cover the pure helpers (clamping, CSS generation, payload shape,
//! shortcut parsing). They cannot cover the seams BETWEEN modules — a font
//! written by `storage::save_services` read back by `storage::load_services`, a
//! config document exported and imported later, a `services.json` written
//! before the feature existed. Those seams are exactly where a per-app setting
//! silently loses data, so they are pinned here through the public module
//! surface (`bzdium_lib::{fonts, state, storage, services}`).
//!
//! Everything is filesystem-only: temp dirs under `std::env::temp_dir()`
//! (UUID-suffixed, so parallel test runs cannot collide), no Tauri app handle,
//! no network. The one gap is stated where it matters: the real
//! `export_config_to_path`/`import_config_from_path` need an `AppHandle` +
//! `State`, so the export test covers the on-disk document contract (serde),
//! not the dialog commands themselves.

use std::path::{Path, PathBuf};

use bzdium_lib::fonts::{
    canonical_accel, page_payload, style_css, validate_shortcuts, FontConfig, ShortcutBindings,
    DEFAULT_SIZE_PX, MAX_SIZE_PX, MIN_SIZE_PX, STEP_PX,
};
use bzdium_lib::services::{ConfigFile, ConfigService, CONFIG_TYPE, CONFIG_VERSION};
use bzdium_lib::state::{Service, ServiceKind, Settings};
use bzdium_lib::storage::{load_services, save_services, SERVICES_FILE};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// The same convention the existing integration tests use: a UUID-suffixed
/// directory under the OS temp dir, so two `cargo test` runs (or two tests in
/// this file) never share state.
fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bzdium-font-it-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// A minimal, valid `Service` (all fields that predate this feature are set
/// explicitly; the font is the only thing under test).
fn service(id: &str, name: &str, order: u32, font: FontConfig) -> Service {
    Service {
        id: id.to_string(),
        name: name.to_string(),
        url: format!("https://{order}.example.com/"),
        icon: name.chars().take(2).collect::<String>().to_uppercase(),
        enabled: true,
        order,
        kind: ServiceKind::Web,
        icon_id: None,
        custom_icon: None,
        hibernated: false,
        badge_count: 0,
        ignore_tls_errors: false,
        font,
    }
}

/// A non-default binding set that is already canonical, so a round trip proves
/// the values travel rather than being re-derived: every slot differs from
/// `ShortcutBindings::default()` and from each other.
fn custom_shortcuts() -> ShortcutBindings {
    ShortcutBindings {
        increase: "Ctrl+Shift+U".to_string(),
        decrease: "Ctrl+Shift+D".to_string(),
        reset: "Ctrl+Shift+0".to_string(),
        open_settings: "Ctrl+Alt+F".to_string(),
    }
}

const ID_A: &str = "11111111-1111-4111-8111-111111111111";
const ID_B: &str = "22222222-2222-4222-8222-222222222222";

// ---------------------------------------------------------------------------
// 1. Backward compatibility: a pre-feature services.json still opens
// ---------------------------------------------------------------------------

#[test]
fn services_json_without_font_opens_with_defaults() {
    let dir = temp_dir("legacy");
    // Exactly what bzdium wrote before per-app fonts existed (0.2.16 shape):
    // no "font" key. Every field the loader backfills is deliberately absent,
    // because a real old file would not have it either.
    let legacy = r#"[
        {
            "id": "0b6d9a9e-2f4f-4a09-9c1e-1d2c3b4a5f60",
            "name": "Chat",
            "url": "https://chat.example.com/",
            "icon": "CH",
            "enabled": true,
            "order": 0
        }
    ]"#;
    std::fs::write(dir.join(SERVICES_FILE), legacy).expect("write legacy services.json");

    let services = load_services(&dir);
    assert_eq!(services.len(), 1, "the legacy entry must not be dropped");
    assert_eq!(services[0].name, "Chat");
    // `#[serde(default)]` on Service::font is what makes the upgrade silent;
    // assert the whole default, not just size, so a newly added FontConfig
    // field with a non-default fallback would fail here.
    assert_eq!(services[0].font, FontConfig::default());
    assert_eq!(services[0].font.size_px, DEFAULT_SIZE_PX);
    assert_eq!(services[0].font.default_size_px, DEFAULT_SIZE_PX);
    assert_eq!(services[0].font.min_size_px, MIN_SIZE_PX);
    assert_eq!(services[0].font.max_size_px, MAX_SIZE_PX);
    assert_eq!(services[0].font.step_px, STEP_PX);
    assert!(services[0].font.enabled, "new services opt into overrides");
    assert!(services[0].font.scale_px_text, "the px pass is on by default");
    assert_eq!(services[0].font.family, None, "system font stack is preserved");
    assert_eq!(
        services[0].font.shortcuts,
        ShortcutBindings::default(),
        "the default accelerators must be usable out of the box"
    );

    // The next save materialises the defaults (this is what makes a UI edit
    // or a plain restart write the new field), and the value survives it.
    save_services(&dir, &services).expect("save services");
    let raw = std::fs::read_to_string(dir.join(SERVICES_FILE)).expect("read back services.json");
    let saved: serde_json::Value = serde_json::from_str(&raw).expect("services.json is JSON");
    assert!(
        saved[0].get("font").is_some(),
        "new saves carry the font field: {raw}"
    );
    assert_eq!(
        saved[0]["font"]["scalePxText"], true,
        "camelCase on disk: {raw}"
    );
    assert_eq!(load_services(&dir)[0].font, FontConfig::default());

    cleanup(&dir);
}

// ---------------------------------------------------------------------------
// 2. Per-app isolation + persistence round trip
// ---------------------------------------------------------------------------

#[test]
fn per_app_font_configs_survive_save_and_load_unchanged() {
    let dir = temp_dir("round-trip");
    let font_a = FontConfig {
        family: Some("Inter".to_string()),
        size_px: 18.5,
        enabled: true,
        scale_px_text: false,
        shortcuts: custom_shortcuts(),
        ..FontConfig::default()
    };
    let font_b = FontConfig {
        family: None,
        size_px: 12.0,
        enabled: false,
        scale_px_text: true,
        shortcuts: ShortcutBindings::default(),
        ..FontConfig::default()
    };
    save_services(
        &dir,
        &[
            service(ID_A, "Chat", 0, font_a.clone()),
            service(ID_B, "Mail", 1, font_b.clone()),
        ],
    )
    .expect("save services");

    let loaded = load_services(&dir);
    assert_eq!(loaded.len(), 2);
    // load_services sorts by `order`; the names prove the fonts stayed paired
    // with their own service instead of being shared or swapped.
    assert_eq!(loaded[0].id, ID_A);
    assert_eq!(loaded[0].name, "Chat");
    assert_eq!(loaded[1].id, ID_B);
    assert_eq!(loaded[1].name, "Mail");

    // Whole-struct equality: every field the UI edits, including all four
    // shortcuts, must be byte-identical after a disk round trip.
    assert_eq!(loaded[0].font, font_a);
    assert_eq!(loaded[1].font, font_b);
    assert_eq!(loaded[0].font.family.as_deref(), Some("Inter"));
    assert_eq!(loaded[0].font.size_px, 18.5, "fractional sizes survive");
    assert!(!loaded[0].font.scale_px_text);
    assert_eq!(loaded[0].font.shortcuts.increase, "Ctrl+Shift+U");
    assert_eq!(loaded[0].font.shortcuts.decrease, "Ctrl+Shift+D");
    assert_eq!(loaded[0].font.shortcuts.reset, "Ctrl+Shift+0");
    assert_eq!(loaded[0].font.shortcuts.open_settings, "Ctrl+Alt+F");
    assert_eq!(loaded[1].font.family, None);
    assert!(!loaded[1].font.enabled);

    // Editing one service and saving again must leave the other untouched:
    // this is the per-app isolation promise, not just a serialization one.
    let mut services = load_services(&dir);
    services[0].font.size_px = 22.0;
    services[0].font.family = Some("Georgia".to_string());
    services[0].font.shortcuts.increase = "Ctrl+Alt+Up".to_string();
    save_services(&dir, &services).expect("save edited services");

    let reloaded = load_services(&dir);
    assert_eq!(reloaded[0].font.size_px, 22.0);
    assert_eq!(reloaded[0].font.family.as_deref(), Some("Georgia"));
    assert_eq!(reloaded[0].font.shortcuts.increase, "Ctrl+Alt+Up");
    assert_eq!(reloaded[1].font, font_b, "the second app's font must not move");

    // On-disk shape is the wire contract the UI reads back (camelCase keys).
    let raw = std::fs::read_to_string(dir.join(SERVICES_FILE)).expect("read services.json");
    let saved: serde_json::Value = serde_json::from_str(&raw).expect("services.json is JSON");
    assert_eq!(saved[0]["font"]["sizePx"], 22.0);
    assert_eq!(saved[0]["font"]["scalePxText"], false);
    assert_eq!(saved[0]["font"]["shortcuts"]["openSettings"], "Ctrl+Alt+F");
    assert_eq!(saved[0]["font"]["shortcuts"]["increase"], "Ctrl+Alt+Up");

    cleanup(&dir);
}

// ---------------------------------------------------------------------------
// 3. Clamping at the load/deserialize boundary
// ---------------------------------------------------------------------------

#[test]
fn out_of_range_stored_values_never_reach_generated_css() {
    let dir = temp_dir("clamp");
    // A hand-edited services.json: size above the ceiling, an inverted/
    // overflowing bounds pair, a dead step, and a hostile family string. This
    // must parse (it is valid JSON) — the guarantee is that the CONSUMER
    // clamps it, so no NaN/huge value can reach generated CSS.
    let hostile = r#"[{
        "id": "33333333-3333-4333-8333-333333333333",
        "name": "Hostile",
        "url": "https://hostile.example.com/",
        "icon": "HO",
        "enabled": true,
        "order": 0,
        "font": {
            "family": "Bad } .x{",
            "sizePx": 9999.0,
            "defaultSizePx": 9999.0,
            "minSizePx": 0.1,
            "maxSizePx": 500.0,
            "stepPx": 0.0,
            "enabled": true,
            "scalePxText": true,
            "shortcuts": {
                "increase": "Ctrl+=",
                "decrease": "Ctrl+-",
                "reset": "Ctrl+0",
                "openSettings": "Ctrl+Shift+F"
            }
        }
    }]"#;
    std::fs::write(dir.join(SERVICES_FILE), hostile).expect("write hostile services.json");

    let loaded = load_services(&dir);
    assert_eq!(loaded.len(), 1);
    // Contract: storage does NOT rewrite the stored numbers (they are kept as
    // written so a user's out-of-range value is not silently destroyed); the
    // clamping boundary is `clamped()`, used by every consumer. Pin both
    // halves so a "helpful" clamp added to load_services is a deliberate
    // decision, not an accident.
    assert_eq!(loaded[0].font.size_px, 9999.0);
    assert_eq!(loaded[0].font.max_size_px, 500.0);

    let clamped = loaded[0].font.clone().clamped();
    // Exact values from FontConfig::clamped's documented ranges:
    // min clamps into [1, 200], max into [1, 400], step into [0.5, 20], and a
    // family that cannot be embedded safely becomes None.
    assert_eq!(clamped.min_size_px, 1.0);
    assert_eq!(clamped.max_size_px, 400.0);
    assert_eq!(clamped.size_px, 400.0);
    assert_eq!(clamped.default_size_px, 400.0);
    assert_eq!(clamped.step_px, 0.5);
    assert_eq!(clamped.min_size_px <= clamped.default_size_px, true);
    assert!(clamped.default_size_px <= clamped.max_size_px);
    assert_eq!(clamped.family, None, "hostile family is dropped, not escaped");
    assert!(clamped.size_px.is_finite() && clamped.step_px.is_finite());

    // The generated CSS is the actual security/robustness boundary: it must
    // carry the clamped numbers and none of the hostile text.
    let css = style_css(&loaded[0].font);
    assert!(css.contains("font-size: 400px !important"), "css: {css}");
    assert!(!css.contains("9999"), "raw stored size leaked into CSS: {css}");
    assert!(!css.contains("Bad"), "hostile family leaked into CSS: {css}");

    // Same guarantee for the payload the page bundle embeds.
    let payload = page_payload(&loaded[0].font, "https://hostile.example.com/");
    let value: serde_json::Value = serde_json::from_str(&payload).expect("payload must be JSON");
    assert_eq!(value["cfg"]["sizePx"], 400.0);
    assert_eq!(value["cfg"]["maxSizePx"], 400.0);
    assert!(value["cfg"]["family"].is_null());
    assert!(!payload.contains("Bad"));

    cleanup(&dir);
}

#[test]
fn stored_size_below_min_and_inverted_bounds_are_repaired() {
    let dir = temp_dir("clamp-bounds");
    let json = r#"[
        {
            "id": "44444444-4444-4444-8444-444444444444",
            "name": "Too small",
            "url": "https://small.example.com/",
            "icon": "SM",
            "enabled": true,
            "order": 0,
            "font": {
                "sizePx": 2.0,
                "defaultSizePx": 2.0,
                "minSizePx": 8.0,
                "maxSizePx": 40.0,
                "stepPx": 1.0,
                "enabled": true,
                "scalePxText": true,
                "shortcuts": {
                    "increase": "Ctrl+=",
                    "decrease": "Ctrl+-",
                    "reset": "Ctrl+0",
                    "openSettings": "Ctrl+Shift+F"
                }
            }
        },
        {
            "id": "55555555-5555-4555-8555-555555555555",
            "name": "Inverted bounds",
            "url": "https://inverted.example.com/",
            "icon": "IN",
            "enabled": true,
            "order": 1,
            "font": {
                "sizePx": 20.0,
                "defaultSizePx": 20.0,
                "minSizePx": 30.0,
                "maxSizePx": 10.0,
                "stepPx": 1.0,
                "enabled": true,
                "scalePxText": true,
                "shortcuts": {
                    "increase": "Ctrl+=",
                    "decrease": "Ctrl+-",
                    "reset": "Ctrl+0",
                    "openSettings": "Ctrl+Shift+F"
                }
            }
        }
    ]"#;
    std::fs::write(dir.join(SERVICES_FILE), json).expect("write services.json");
    let loaded = load_services(&dir);
    assert_eq!(loaded.len(), 2);

    // Below min: lifted exactly to min, never below it.
    let small = loaded[0].font.clone().clamped();
    assert_eq!(small.min_size_px, 8.0);
    assert_eq!(small.size_px, 8.0, "size must not stay below min");
    assert!(style_css(&loaded[0].font).contains("font-size: 8px !important"));

    // Inverted bounds: max is repaired strictly above min, and both the
    // current size and the default are pulled inside the repaired range.
    let inverted = loaded[1].font.clone().clamped();
    assert!(
        inverted.max_size_px > inverted.min_size_px,
        "min {} max {} must be a usable range",
        inverted.min_size_px,
        inverted.max_size_px
    );
    assert!(inverted.size_px >= inverted.min_size_px);
    assert!(inverted.size_px <= inverted.max_size_px);
    assert!(inverted.default_size_px >= inverted.min_size_px);
    assert!(inverted.default_size_px <= inverted.max_size_px);

    cleanup(&dir);
}

#[test]
fn a_services_json_with_non_finite_numbers_is_backed_up_not_loaded() {
    let dir = temp_dir("nan");
    // JSON has no NaN literal, so a hand-edited "NaN" makes the whole file
    // unparseable. The honest contract (storage::load_or_default) is: keep the
    // bad file as `services.json.broken-<ts>` and start from defaults — a
    // non-finite number must never produce a half-loaded service.
    let path = dir.join(SERVICES_FILE);
    let nan_json = r#"[{
        "id": "66666666-6666-4666-8666-666666666666",
        "name": "NaN",
        "url": "https://nan.example.com/",
        "icon": "NA",
        "enabled": true,
        "order": 0,
        "font": {
            "sizePx": NaN,
            "defaultSizePx": 16.0,
            "minSizePx": 8.0,
            "maxSizePx": 40.0,
            "stepPx": 1.0,
            "enabled": true,
            "scalePxText": true,
            "shortcuts": {
                "increase": "Ctrl+=",
                "decrease": "Ctrl+-",
                "reset": "Ctrl+0",
                "openSettings": "Ctrl+Shift+F"
            }
        }
    }]"#;
    std::fs::write(&path, nan_json).expect("write services.json");

    let services = load_services(&dir);
    assert!(services.is_empty(), "a corrupt file must not half-load");
    assert!(!path.exists(), "the bad file is moved aside, not deleted");
    let backups: Vec<_> = std::fs::read_dir(&dir)
        .expect("read temp dir")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry.file_name().to_string_lossy().starts_with("services.json.broken-")
        })
        .collect();
    assert_eq!(backups.len(), 1, "the tampered file is preserved for inspection");
    assert!(std::fs::read_to_string(backups[0].path())
        .expect("read backup")
        .contains("NaN"));

    // And an empty (default) state still saves and loads cleanly.
    save_services(&dir, &[]).expect("save empty services");
    assert!(load_services(&dir).is_empty());

    cleanup(&dir);
}

// ---------------------------------------------------------------------------
// 4. Shortcut canonicalization, conflicts and reserved accelerators
// ---------------------------------------------------------------------------

#[test]
fn conflicting_shortcuts_are_rejected_naming_both_actions() {
    // Same physical key spelled differently: default increase is "Ctrl+=",
    // and "Ctrl++" canonicalizes to "Ctrl+Plus" — the equivalence folding in
    // validate_shortcuts must still catch them as one shortcut.
    let mut bindings = ShortcutBindings::default();
    bindings.decrease = "ctrl++".to_string();
    let error = validate_shortcuts(&bindings).expect_err("Ctrl+= and Ctrl++ collide");
    assert!(error.contains("Increase font size"), "must name the first action: {error}");
    assert!(error.contains("Decrease font size"), "must name the second action: {error}");
    assert!(error.contains("Ctrl+="), "must name the shared shortcut: {error}");

    // Identical after canonicalization (spacing/case) also collides.
    let mut bindings = ShortcutBindings::default();
    bindings.increase = "  ctrl + shift + u ".to_string();
    bindings.reset = "Ctrl+Shift+U".to_string();
    let error = validate_shortcuts(&bindings).expect_err("same accelerator twice");
    assert!(error.contains("Increase font size"), "got: {error}");
    assert!(error.contains("Reset font size"), "got: {error}");

    // Reserved accelerators are refused for every slot that is checked, and
    // the message names both the action and the accelerator so the toast is
    // actionable.
    let mut bindings = ShortcutBindings::default();
    bindings.open_settings = "ctrl+r".to_string();
    let error = validate_shortcuts(&bindings).expect_err("Ctrl+R is bzdium's reload");
    assert!(error.contains("Font settings"), "got: {error}");
    assert!(error.contains("Ctrl+R"), "got: {error}");

    let mut bindings = ShortcutBindings::default();
    bindings.increase = "f5".to_string();
    let error = validate_shortcuts(&bindings).expect_err("F5 is reserved");
    assert!(error.contains("Increase font size"), "got: {error}");
    assert!(error.contains("F5"), "got: {error}");
}

#[test]
fn shortcut_canonicalization_is_stable_and_platform_mapped() {
    // Canonical spelling: modifiers in Ctrl, Alt, Shift order, then one key.
    assert_eq!(canonical_accel("ctrl + alt + k").unwrap(), "Ctrl+Alt+K");
    assert_eq!(canonical_accel("Shift+Ctrl+F12").unwrap(), "Ctrl+Shift+F12");
    // Cmd/Command/Meta/Super are all accepted as the primary modifier and
    // normalized to "Ctrl": the token is what the settings UI stores, while the
    // page script maps it to the physical Cmd key on macOS.
    assert_eq!(canonical_accel("Cmd+Shift+F").unwrap(), "Ctrl+Shift+F");
    assert_eq!(canonical_accel("Control+F").unwrap(), "Ctrl+F");
    assert_eq!(canonical_accel("Meta+R").unwrap(), "Ctrl+R");
    // '+', '-' and '=' are expressible (they double as separators / shifted
    // spellings) and the named keys are accepted.
    assert_eq!(canonical_accel("Ctrl++").unwrap(), "Ctrl+Plus");
    assert_eq!(canonical_accel("Ctrl+0").unwrap(), "Ctrl+0");
    assert_eq!(canonical_accel("Alt+Space").unwrap(), "Alt+Space");
    // Malformed input is reported, not silently accepted.
    assert!(canonical_accel("").is_err());
    assert!(canonical_accel("Ctrl+Shift").is_err(), "modifiers only");
    assert!(canonical_accel("Ctrl+A+B").is_err(), "more than one key");
    assert!(canonical_accel("Ctrl+Shift+Ctrl+X").is_err(), "duplicate modifier");
    assert!(canonical_accel("Hyper+F").is_err(), "unknown token");

    // A valid custom set is returned canonicalized and unchanged afterwards:
    // this is what set_font_config persists, so the page script can compare
    // key events against it directly.
    let canonical = validate_shortcuts(&custom_shortcuts()).expect("custom set is valid");
    assert_eq!(canonical, custom_shortcuts());
    assert_eq!(
        validate_shortcuts(&ShortcutBindings::default()).expect("defaults are valid"),
        ShortcutBindings::default()
    );
}

// ---------------------------------------------------------------------------
// 5. Config export/import document carries fonts
// ---------------------------------------------------------------------------

#[test]
fn exported_config_document_carries_each_services_font() {
    let dir = temp_dir("export");
    let font_a = FontConfig {
        family: Some("Noto Sans".to_string()),
        size_px: 17.5,
        enabled: true,
        scale_px_text: true,
        shortcuts: custom_shortcuts(),
        ..FontConfig::default()
    };
    let font_b = FontConfig {
        family: None,
        size_px: 11.0,
        enabled: false,
        scale_px_text: false,
        shortcuts: ShortcutBindings::default(),
        ..FontConfig::default()
    };
    // NOTE: built with serde directly because the real commands
    // (services::export_config_to_path / import_config_from_path) need an
    // AppHandle + State, which an integration test cannot construct. This
    // pins the on-disk DOCUMENT contract those commands read and write.
    let document = ConfigFile {
        kind: CONFIG_TYPE.to_string(),
        version: CONFIG_VERSION,
        exported_at: "0".to_string(),
        settings: Settings::default(),
        services: vec![
            ConfigService {
                id: ID_A.to_string(),
                name: "Chat".to_string(),
                url: "https://chat.example.com/".to_string(),
                icon: "CH".to_string(),
                enabled: true,
                order: 0,
                icon_id: None,
                ignore_tls_errors: false,
                font: font_a.clone(),
                kind: ServiceKind::Web,
            },
            ConfigService {
                id: ID_B.to_string(),
                name: "Mail".to_string(),
                url: "https://mail.example.com/".to_string(),
                icon: "MA".to_string(),
                enabled: true,
                order: 1,
                icon_id: None,
                ignore_tls_errors: false,
                font: font_b.clone(),
                kind: ServiceKind::Web,
            },
        ],
    };
    let path = dir.join("bzdium-config.json");
    let json = serde_json::to_string_pretty(&document).expect("serialize config document");
    std::fs::write(&path, &json).expect("write config document");
    assert!(json.contains("\"type\": \"bzdium-config\""), "export type tag: {json}");
    assert!(json.contains("\"font\""), "every service carries its font: {json}");
    assert!(json.contains("\"scalePxText\""), "camelCase font fields: {json}");
    assert!(json.contains("\"openSettings\""), "shortcuts travel too: {json}");

    // Round trip: an export and a later import restore each service's exact
    // font next to its own service.
    let text = std::fs::read_to_string(&path).expect("read config document");
    let imported: ConfigFile = serde_json::from_str(&text).expect("parse config document");
    assert_eq!(imported.kind, CONFIG_TYPE);
    assert_eq!(imported.version, CONFIG_VERSION);
    assert_eq!(imported.settings, Settings::default());
    assert_eq!(imported.services.len(), 2);
    assert_eq!(imported.services[0].font, font_a);
    assert_eq!(imported.services[1].font, font_b);
    assert_eq!(imported.services[0].font.shortcuts.open_settings, "Ctrl+Alt+F");

    // Older export files (written before per-app fonts) have no "font" key;
    // `#[serde(default)]` on ConfigService::font keeps them importable with
    // defaults, and CONFIG_VERSION was deliberately not bumped for this.
    let mut legacy: serde_json::Value = serde_json::from_str(&text).expect("parse as Value");
    for service in legacy["services"].as_array_mut().expect("services array").iter_mut() {
        service.as_object_mut().expect("service object").remove("font");
    }
    let legacy_text = serde_json::to_string(&legacy).expect("serialize legacy document");
    let imported: ConfigFile = serde_json::from_str(&legacy_text).expect("legacy export still imports");
    assert_eq!(imported.services.len(), 2);
    for service in &imported.services {
        assert_eq!(service.font, FontConfig::default());
    }

    cleanup(&dir);
}
