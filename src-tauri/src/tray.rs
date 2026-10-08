use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, Manager};

pub const TRAY_ID: &str = "bzdium-tray";
const MENU_QUIT_ID: &str = "quit";

/// System tray: left-click toggles the window, menu offers Quit.
pub fn setup_tray(app: &mut App) -> tauri::Result<()> {
    let quit = MenuItem::with_id(app, MENU_QUIT_ID, "Quit bzdium", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&quit])?;
    let icon = tauri::image::Image::from_bytes(include_bytes!("../icons/icon-32.png"))?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("bzdium")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| {
            if event.id.as_ref() == MENU_QUIT_ID {
                app.exit(0);
            }
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_window(crate::state::UI_WEBVIEW_LABEL) {
                    if window.is_visible().unwrap_or(false) {
                        let _ = window.hide();
                    } else {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
            }
        })
        .build(app)?;
    Ok(())
}
