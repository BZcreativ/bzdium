// Prevents an additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // --mcp-stdio runs the MCP stdio bridge and MUST NOT start Tauri: agent
    // clients (Claude Desktop, Cursor) spawn this as a subprocess, and a
    // second Tauri instance would fight the single-instance lock. The bridge
    // talks to the already-running bzdium over loopback HTTP instead.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--mcp-stdio") {
        std::process::exit(bzdium_lib::agent::stdio::run_stdio_bridge());
    }
    bzdium_lib::run();
}
