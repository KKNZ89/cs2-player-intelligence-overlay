//! The notification-area icon: open the dashboard (left click), pin the overlay, or quit.

use crate::engine::{show_main, Engine};
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{App, Manager};

pub fn create(app: &App) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open dashboard", true, None::<&str>)?;
    let pin = MenuItem::with_id(app, "pin", "Pin or unpin overlay", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit CS2 Player Intel", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &pin, &PredefinedMenuItem::separator(app)?, &quit])?;
    let mut builder = TrayIconBuilder::with_id("main").tooltip("CS2 Player Intel").menu(&menu).show_menu_on_left_click(false);
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "pin" => {
                if let Some(engine) = app.try_state::<Arc<Engine>>() {
                    let engine = engine.inner().clone();
                    // Menu events arrive on the UI thread; the overlay work runs on the async runtime.
                    tauri::async_runtime::spawn(async move {
                        engine.toggle_pinned();
                    });
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}
