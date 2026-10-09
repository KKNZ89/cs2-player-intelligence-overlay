// No console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod analysis;
mod autostart;
mod capture;
mod commands;
mod console_roster;
mod diagnostics;
mod engine;
mod game;
mod gsi;
mod gsi_config;
mod key_watcher;
mod lifecycle;
mod match_controller;
mod match_history;
mod model;
mod modes;
mod notifications;
mod overlay;
mod player_data;
mod prediction;
mod providers;
mod roster;
mod roster_input;
mod scoreboard;
mod settings;
mod steam;
mod team_inference;
mod tray;
mod updater;

use engine::Engine;
use std::sync::Arc;
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder, WindowEvent};

/// Hardening for the app's own two pages only. Tauri's global `freezePrototype` setting would also apply
/// to the provider windows, and sites such as Steam's sign-in page stop working with it.
pub const FREEZE_PROTOTYPE: &str = "Object.freeze(Object.prototype);";

fn main() {
    // The Steam helper is this executable in a second role; it must start before anything else.
    if std::env::args().nth(1).as_deref() == Some(steam::HELPER_ARG) {
        std::process::exit(steam::run_helper());
    }
    // Development check of window capture: --capture-window "<title prefix>" <out.png>
    #[cfg(all(debug_assertions, windows))]
    if std::env::args().nth(1).as_deref() == Some("--capture-window") {
        let args: Vec<String> = std::env::args().collect();
        let result = capture::find_window(&args[2])
            .ok_or_else(|| format!("No window titled {}", args[2]))
            .and_then(|hwnd| capture::capture_window(hwnd, std::time::Duration::from_secs(3)))
            .and_then(|frame| frame.save_png(std::path::Path::new(&args[3])).map(|_| (frame.width, frame.height)));
        println!("{result:?}");
        std::process::exit(if result.is_ok() { 0 } else { 1 });
    }

    // A smoke test runs beside an installed copy without contacting it (no single-instance hand-off).
    let mut builder = tauri::Builder::default();
    if std::env::var_os("CS2INTEL_SMOKE").is_none() {
        builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| engine::show_main(app)));
    }
    let app = builder
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(tauri::generate_handler![
            commands::state_get,
            commands::settings_save,
            commands::path_pick,
            commands::gsi_install,
            commands::csstats_login,
            commands::profile_open,
            commands::link_open,
            commands::player_add,
            commands::players_select,
            commands::players_automatic,
            commands::player_remove,
            commands::player_side,
            commands::players_refresh,
            commands::player_refresh,
            commands::overlay_toggle,
            commands::overlay_interaction_end,
            commands::overlay_fit,
            commands::update_check,
            commands::update_install,
            commands::game_launch,
            commands::diagnostics_recent,
            commands::diagnostics_open,
            commands::player_history,
            commands::note_add,
            commands::note_delete,
            commands::history_stats,
            commands::history_matches,
            commands::history_match,
            commands::history_export,
            commands::history_clear,
            commands::history_import,
        ])
        .setup(|app| {
            let handle = app.handle().clone();
            // A smoke test runs against its own folder and never imports the real settings.
            let smoke = std::env::var_os("CS2INTEL_SMOKE").map(std::path::PathBuf::from);
            // Started with Windows: only the tray icon until the dashboard is opened.
            let data_dir = match &smoke {
                Some(dir) => dir.clone(),
                None => app.path().app_data_dir()?,
            };
            // Started with Windows, or restarted by an automatic update: tray only until opened.
            let minimized = std::env::args().any(|arg| arg == autostart::MINIMIZED_ARG) || updater::take_quiet_restart(&data_dir);
            #[cfg(windows)]
            let protector = Box::new(settings::Dpapi);
            let settings = settings::SettingsStore::open(&data_dir, protector);
            let history = match_history::MatchHistoryStore::open(&data_dir);
            let engine = Engine::new(handle.clone(), data_dir, settings, match_controller::MatchState::new(history), smoke.is_some(), minimized);
            app.manage(engine.clone());

            // Every handler and the state exist before either page loads.
            let mut main = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()));
            if let Some(dir) = &smoke {
                main = main.data_directory(dir.join("webview"));
            }
            let main = main
                .initialization_script(FREEZE_PROTOTYPE)
                .title("CS2 Player Intel")
                .inner_size(1280.0, 820.0)
                .min_inner_size(900.0, 620.0)
                // Ctrl + plus / minus / 0 scale the dashboard's text.
                .zoom_hotkeys_enabled(true)
                .background_color(tauri::window::Color(13, 15, 18, 255))
                .visible(smoke.is_none() && !minimized)
                .build()?;
            let weak = Arc::downgrade(&engine);
            let window = main.clone();
            main.on_window_event(move |event| {
                let Some(engine) = weak.upgrade() else { return };
                match event {
                    // Closing hides the dashboard while the tray icon keeps the app (and the overlay)
                    // running; otherwise the hidden overlay would keep the process alive with no window.
                    WindowEvent::CloseRequested { api, .. } => {
                        if engine.settings().close_to_tray && !engine.smoke {
                            api.prevent_close();
                            let _ = window.hide();
                        } else {
                            engine.app.exit(0);
                        }
                    }
                    // A minimised dashboard receives nothing; it catches up when it is shown again. Window
                    // events arrive on the UI thread, so the work runs on the async runtime.
                    WindowEvent::Focused(true) => {
                        tauri::async_runtime::spawn(async move { engine.flush(&["main"]) });
                    }
                    _ => {}
                }
            });
            let overlay = overlay::create(&handle, smoke.as_ref().map(|dir| dir.join("webview")))?;
            let weak = Arc::downgrade(&engine);
            overlay.on_window_event(move |event| {
                if matches!(event, WindowEvent::Focused(false)) {
                    if let Some(engine) = weak.upgrade() {
                        tauri::async_runtime::spawn(async move { engine.end_mouse_navigation() });
                    }
                }
            });
            if smoke.is_none() {
                tray::create(app)?;
            }
            engine.start();
            Ok(())
        })
        .build(tauri::generate_context!())
        .unwrap_or_else(|error| {
            eprintln!("CS2 Player Intel could not start: {error}");
            std::process::exit(1);
        });

    app.run(|app, event| {
        if let RunEvent::Exit = event {
            if let Some(engine) = app.try_state::<Arc<Engine>>() {
                engine.kill_steam();
                engine.updates.install_on_quit();
            }
        }
    });
}
