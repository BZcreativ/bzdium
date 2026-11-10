//! Linux layout manager (GTK multi-webview positioning).
//!
//! On Linux, Tauri v2 defaults to packing all child webviews into a vertical
//! `GtkBox` (`default_vbox`). When a service webview is added via `window.add_child`,
//! GTK stacks the main UI chrome webview and the service webview vertically,
//! crushing the sidebar and placing the service underneath it across the entire
//! window width.
//!
//! This module resolves the layout on Linux by:
//! 1. Creating a `GtkFixed` container within `default_vbox` during setup.
//! 2. Moving the main UI webview into `GtkFixed` at `(0, 0)` spanning full window dimensions.
//! 3. Moving newly created service webviews into `GtkFixed` at `(SIDEBAR_WIDTH, offset_y)`.
//! 4. Keeping positions and sizes synchronized on resize, service switch, and URL bar toggles.
//!
//! Sizing contract: a `GtkFixed` sizes its children by their size request, and
//! GTK3 folds children's requests into the toplevel's minimum size. Requesting
//! the current window size on every sync therefore pins the window minimum at
//! its current size — the window can grow but never shrink, which is how
//! v0.2.17 ballooned past the screen with no edge or title bar left to grab
//! (user report on Debian 12; GNOME offers no visible resize borders). To keep
//! the minimum at the configured 480x400 floor, children here keep a 1x1
//! request and real geometry is applied by allocating them manually whenever
//! the fixed container is allocated — synchronous with the window resize, and
//! without the per-event `with_webview` round-trips that made resizing janky.

use std::sync::{Mutex, OnceLock};

use gtk::prelude::*;
use tauri::{LogicalPosition, Manager, Webview, Window};
use webkit2gtk::{WebContextExt, WebViewExt, WebsiteDataManagerExt};

/// gtk widget names used to tell the webviews apart inside the fixed.
const MAIN_WV_NAME: &str = "bzdium-wv-main";
const SERVICE_WV_PREFIX: &str = "bzdium-wv-service-";

/// Vertical offset (gtk px) of the service area — 0, or the URL bar height
/// when that setting is on — and which service is active. Written from
/// `sync_all_bounds`, read on every allocation of the fixed container. All
/// access happens on the GTK main thread, but the Mutex keeps the static
/// reachable from any tauri thread safely.
fn layout_state() -> &'static Mutex<(i32, Option<String>)> {
    static STATE: OnceLock<Mutex<(i32, Option<String>)>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new((0, None)))
}

fn find_fixed(vbox: &gtk::Box) -> Option<gtk::Fixed> {
    vbox.children()
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Fixed>().ok())
}

/// Allocates every child of `fixed` for a container of `cw` x `ch`: the main
/// UI webview spans the whole container, the active service webview gets the
/// rect right of the sidebar and below the stored top offset. Inactive
/// services keep their allocation — visibility is driven by `show_only` via
/// show()/hide(). Deriving the rect from the container (instead of storing a
/// fixed one) is what keeps services glued to the window during resizes.
fn apply_with(fixed: &gtk::Fixed, cw: i32, ch: i32) {
    let (top, active) = layout_state()
        .lock()
        .map(|s| (s.0, s.1.clone()))
        .unwrap_or((0, None));
    let sidebar = crate::webviews::SIDEBAR_WIDTH.round() as i32;
    for child in fixed.children() {
        let name = child.widget_name().to_string();
        let rect = if name == MAIN_WV_NAME {
            Some((0, 0, cw, ch))
        } else if let Some(id) = name.strip_prefix(SERVICE_WV_PREFIX) {
            if active.as_deref() == Some(id) {
                Some((
                    sidebar,
                    top,
                    (cw - sidebar).max(1),
                    (ch - top).max(1),
                ))
            } else {
                None
            }
        } else {
            None
        };
        if let Some((x, y, w, h)) = rect {
            child.size_allocate(&gtk::gdk::Rectangle::new(x, y, w, h));
        }
    }
}

/// `apply_with` using the fixed container's current allocation.
fn apply(fixed: &gtk::Fixed) {
    let alloc = fixed.allocation();
    apply_with(fixed, alloc.width(), alloc.height());
}

/// Initializes the Linux GTK layout inside the window's default vbox.
/// Replaces direct vbox vertical stacking with a single `GtkFixed` container
/// that hosts both the root UI webview and dynamic service webviews.
pub fn init_layout(window: &Window) -> Result<(), String> {
    let vbox = window.default_vbox().map_err(|e| e.to_string())?;

    let Ok(size) = window.inner_size() else { return Ok(()); };
    let scale = window.scale_factor().unwrap_or(1.0);
    let win_w = (size.width as f64 / scale).round() as i32;
    let win_h = (size.height as f64 / scale).round() as i32;

    let fixed = gtk::Fixed::new();
    fixed.set_widget_name("bzdium-fixed-container");

    for child in vbox.children() {
        vbox.remove(&child);
        child.set_widget_name(MAIN_WV_NAME);
        // Keep the request minimal — see the module docs: a child request the
        // size of the window would pin the toplevel minimum at that size.
        child.set_size_request(1, 1);
        fixed.put(&child, 0, 0);
    }

    vbox.pack_start(&fixed, true, true, 0);
    // Re-layout on every allocation change (i.e. every window resize) — this
    // replaces the per-Resized-event IPC sync used before v0.2.18.
    fixed.connect_size_allocate(|fixed, _| apply(fixed));
    fixed.show_all();
    // First frame before the window is mapped: allocate at the requested
    // window size so the UI does not flash at 1x1.
    apply_with(&fixed, win_w, win_h);
    Ok(())
}

/// Shrinks the main window to the monitor if the configured default
/// (1280x800) does not fit — a window that opens larger than the screen
/// cannot be resized back by the user once its decorations land off-screen.
/// Leaves a margin so panels and the WM title bar stay reachable.
pub fn clamp_window_to_monitor(window: &Window) {
    let (Ok(size), Ok(scale)) = (window.inner_size(), window.scale_factor()) else {
        return;
    };
    let Ok(Some(monitor)) = window.current_monitor() else {
        return;
    };
    let mon = monitor.size();
    let max_w = (mon.width as f64 * 0.9) as u32;
    let max_h = (mon.height as f64 * 0.85) as u32;
    if size.width <= max_w && size.height <= max_h {
        return;
    }
    let new_w = size.width.min(max_w).max(480 * scale as u32);
    let new_h = size.height.min(max_h).max(400 * scale as u32);
    let _ = window.set_size(tauri::PhysicalSize::new(new_w, new_h));
    let pos = monitor.position();
    let _ = window.set_position(tauri::PhysicalPosition::new(
        pos.x + (mon.width.saturating_sub(new_w) / 2) as i32,
        pos.y + (mon.height.saturating_sub(new_h) / 2) as i32,
    ));
}

/// Moves a newly created service webview from vbox into the GtkFixed container
/// at the requested (x, y) position, and configures TLS error policy. Sizing
/// is handled by the allocation pass (see `apply_with`).
pub fn attach_service(
    window: &Window,
    webview: &Webview,
    pos: LogicalPosition<f64>,
    ignore_tls_errors: bool,
) -> Result<(), String> {
    let window_ = window.clone();
    let label = webview.label().to_string();
    let Some(service_id) = label.strip_prefix("service-") else {
        return Ok(());
    };
    let widget_name = format!("{SERVICE_WV_PREFIX}{service_id}");
    let x = pos.x.round() as i32;
    let y = pos.y.round() as i32;

    webview
        .with_webview(move |platform_wv| {
            let Ok(vbox) = window_.default_vbox() else { return; };
            let Some(fixed) = find_fixed(&vbox) else { return; };
            let widget = platform_wv.inner();

            if ignore_tls_errors {
                if let Some(context) = widget.web_context() {
                    if let Some(data_manager) = context.website_data_manager() {
                        data_manager.set_tls_errors_policy(webkit2gtk::TLSErrorsPolicy::Ignore);
                    }
                    #[allow(deprecated)]
                    context.set_tls_errors_policy(webkit2gtk::TLSErrorsPolicy::Ignore);
                }
                widget.connect_load_failed_with_tls_errors(move |_wv, failing_uri, _cert, _flags| {
                    eprintln!("[bzdium] ignoring TLS certificate error for {failing_uri}");
                    true
                });
            } else if let Some(context) = widget.web_context() {
                if let Some(data_manager) = context.website_data_manager() {
                    data_manager.set_tls_errors_policy(webkit2gtk::TLSErrorsPolicy::Fail);
                }
                #[allow(deprecated)]
                context.set_tls_errors_policy(webkit2gtk::TLSErrorsPolicy::Fail);
            }

            if let Some(parent) = widget.parent() {
                if let Ok(container) = parent.downcast::<gtk::Container>() {
                    container.remove(&widget);
                }
            }

            widget.set_widget_name(&widget_name);
            // Minimal request (module docs); real geometry comes from apply().
            widget.set_size_request(1, 1);
            fixed.put(&widget, x, y);
            widget.show();
            fixed.queue_resize();
            apply(&fixed);
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Records the service-area top offset (URL bar height or 0) and which
/// service is active, then re-applies the layout: the main UI webview spans
/// the whole window and the active service webview is sized from the
/// container (see `apply_with`). Called on service switch, URL-bar toggle
/// and overlay toggles — window resizes are handled by the container's
/// size-allocate handler and need no explicit sync.
pub fn sync_all_bounds(
    window: &Window,
    top_offset: f64,
    active_service_id: Option<&str>,
) -> Result<(), String> {
    let Some(main_wv) = window.get_webview(crate::state::UI_WEBVIEW_LABEL) else {
        return Ok(());
    };
    let window_ = window.clone();
    let active = active_service_id.map(|s| s.to_string());

    main_wv
        .with_webview(move |_| {
            let Ok(vbox) = window_.default_vbox() else { return; };
            let Some(fixed) = find_fixed(&vbox) else { return; };

            let top = top_offset.round() as i32;
            if let Ok(mut state) = layout_state().lock() {
                *state = (top, active);
            }
            apply(&fixed);
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}
