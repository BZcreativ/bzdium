//! Declares bzdium's own commands in the app ACL manifest.
//!
//! This is required for two things:
//! 1. Generating `allow-<command>` permissions so capabilities can reference them.
//! 2. Authorizing the *remote* service webviews (https pages) to invoke
//!    `report_title` — Tauri v2 denies every custom command from a non-local
//!    origin unless a capability with a `remote` context grants it.
fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(tauri_build::AppManifest::new().commands(&[
            "get_state",
            "get_settings",
            "update_settings",
            "get_recipes",
            "add_service",
            "update_service",
            "remove_service",
            "reorder_services",
            "set_active_service",
            "reload_service",
            "navigate",
            "hibernate_service",
            "wake_service",
            "set_overlay_mode",
            "report_title",
        ])),
    )
    .expect("failed to run tauri-build");
}
