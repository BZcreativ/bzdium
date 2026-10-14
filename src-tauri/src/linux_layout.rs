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

use gtk::prelude::*;
use tauri::{LogicalPosition, LogicalSize, Manager, Webview, Window};

fn find_fixed(vbox: &gtk::Box) -> Option<gtk::Fixed> {
    vbox.children()
        .into_iter()
        .find_map(|w| w.downcast::<gtk::Fixed>().ok())
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
        fixed.put(&child, 0, 0);
        child.set_size_request(win_w, win_h);
    }

    vbox.pack_start(&fixed, true, true, 0);
    fixed.show_all();
    Ok(())
}

/// Moves a newly created service webview from vbox into the GtkFixed container
/// at the requested (x, y) coordinates and size.
pub fn attach_service(
    window: &Window,
    webview: &Webview,
    pos: LogicalPosition<f64>,
    size: LogicalSize<f64>,
) -> Result<(), String> {
    let window_ = window.clone();
    let x = pos.x.round() as i32;
    let y = pos.y.round() as i32;
    let w = size.width.round() as i32;
    let h = size.height.round() as i32;

    webview
        .with_webview(move |platform_wv| {
            let Ok(vbox) = window_.default_vbox() else { return; };
            let Some(fixed) = find_fixed(&vbox) else { return; };
            let widget = platform_wv.inner();

            if let Some(parent) = widget.parent() {
                if let Ok(container) = parent.downcast::<gtk::Container>() {
                    container.remove(&widget);
                }
            }

            fixed.put(&widget, x, y);
            widget.set_size_request(w, h);
            widget.show();
            fixed.queue_resize();
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Updates the bounds of all webviews in the GtkFixed container.
/// The main UI webview is expanded to full window dimensions (0, 0, win_w, win_h).
/// Active service webview is placed at (pos, size), and inactive ones are hidden.
pub fn sync_all_bounds(
    window: &Window,
    pos: LogicalPosition<f64>,
    size: LogicalSize<f64>,
    active_service_id: Option<&str>,
) -> Result<(), String> {
    let Ok(win_size) = window.inner_size() else { return Ok(()); };
    let scale = window.scale_factor().unwrap_or(1.0);
    let win_w = (win_size.width as f64 / scale).round() as i32;
    let win_h = (win_size.height as f64 / scale).round() as i32;

    let x = pos.x.round() as i32;
    let y = pos.y.round() as i32;
    let w = size.width.round() as i32;
    let h = size.height.round() as i32;

    let window_ = window.clone();
    let active_id = active_service_id.map(|s| s.to_string());

    // Main UI webview always spans the whole window
    if let Some(main_wv) = window.get_webview(crate::state::UI_WEBVIEW_LABEL) {
        let win_clone = window_.clone();
        let _ = main_wv.with_webview(move |platform_wv| {
            let Ok(vbox) = win_clone.default_vbox() else { return; };
            let Some(fixed) = find_fixed(&vbox) else { return; };
            let widget = platform_wv.inner();
            fixed.move_(&widget, 0, 0);
            widget.set_size_request(win_w, win_h);
        });
    }

    // Service webviews
    for webview in window.webviews() {
        let label = webview.label().to_string();
        if !label.starts_with("service-") {
            continue;
        }
        let s_id = label["service-".len()..].to_string();
        let is_active = active_id.as_deref() == Some(&s_id);
        let win_clone = window_.clone();

        let _ = webview.with_webview(move |platform_wv| {
            let Ok(vbox) = win_clone.default_vbox() else { return; };
            let Some(fixed) = find_fixed(&vbox) else { return; };
            let widget = platform_wv.inner();
            fixed.move_(&widget, x, y);
            widget.set_size_request(w, h);
            if is_active {
                widget.show();
            } else {
                widget.hide();
            }
        });
    }

    Ok(())
}
