//! Declares bzdium's own commands in the app ACL manifest.
//!
//! This is required for two things:
//! 1. Generating `allow-<command>` permissions so capabilities can reference them.
//! 2. Authorizing the *remote* service webviews (https pages) to invoke
//!    `report_title` — Tauri v2 denies every custom command from a non-local
//!    origin unless a capability with a `remote` context grants it.
fn main() {
    // The dialog plugin's Windows backend calls comctl32!TaskDialogIndirect, an
    // entry point that exists only in the Common Controls v6 side-by-side
    // assembly. A process must declare the v6 dependency in its application
    // manifest or the loader refuses to start it at all, failing with
    // STATUS_ENTRYPOINT_NOT_FOUND (0xc0000139) before main() -- which looks
    // exactly like a corrupt build.
    //
    // tauri-build embeds such a manifest, but only into the application binary.
    // Test harnesses are separate executables that link the same crate and so
    // import the same symbol, and they would get only the manifest link.exe
    // synthesises, which does not name v6. So: stop tauri-build from embedding
    // its copy, and embed one manifest ourselves into every linked executable.
    // Doing it in one place means the app binary and the test harnesses cannot
    // disagree, and there is never more than one manifest resource per image
    // (two make link.exe fail with LNK1123).
    let windows_attributes = if cfg!(windows) {
        let manifest = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("windows-app.manifest");
        println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg=/MANIFESTINPUT:{}", manifest.display());
        println!("cargo:rerun-if-changed={}", manifest.display());
        tauri_build::WindowsAttributes::new_without_app_manifest()
    } else {
        tauri_build::WindowsAttributes::new()
    };

    tauri_build::try_build(
        tauri_build::Attributes::new()
            .windows_attributes(windows_attributes)
            .app_manifest(tauri_build::AppManifest::new().commands(&[
            "get_state",
            "update_settings",
            "get_recipes",
            "add_service",
            "update_service",
            "remove_service",
            "reorder_services",
            "set_active_service",
            "reload_service",
            "clear_service_data",
            "navigate",
            "hibernate_service",
            "wake_service",
            "set_overlay_mode",
            "navigate_url",
            "export_config",
            "import_config",
            "open_devtools",
            "open_service_external",
            "open_url_external",
            "grok_key_status",
            "grok_save_key",
            "grok_delete_key",
            "grok_test_connection",
            "grok_send_message",
            "grok_cancel_request",
            "grok_list_conversations",
            "grok_save_conversation",
            "grok_clear_history",
            "pick_service_icon",
            "reset_service_icon",
            "get_service_icon",
            "report_title",
            "report_probe",
            "get_font_config",
            "set_font_config",
            "adjust_font_size",
            "reset_font_config",
            "reset_font_shortcuts",
            "apply_font_to_all",
            "list_font_families",
            "service_menu_action",
            "list_webview2_runtimes",
            "get_agent_status",
            "set_agent_admin_scope",
            "mint_agent_token",
            "revoke_agent_tokens",
        ])),
    )
    .expect("failed to run tauri-build");
}
