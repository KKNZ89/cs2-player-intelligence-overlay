//! The overlay window over CS2 (borderless windowed). It is shown while the hotkey has pinned it or while
//! Tab is held in CS2 (Ctrl+Tab: expanded view), and becomes clickable while CS2's Esc menu is open.
//! Only the explicit mouse-navigation shortcut allows it to take focus from the game.

use crate::settings::Settings;
use tauri::{AppHandle, Manager, Monitor, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

pub const LABEL: &str = "overlay";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyView {
    Hidden,
    Compact,
    Expanded,
}

#[derive(Clone, Debug)]
pub struct OverlayState {
    pub pinned: bool,
    pub key_view: KeyView,
    /// CS2's Esc menu is open: the cursor is free, so the overlay accepts clicks.
    pub menu_open: bool,
    /// Explicitly requested mouse navigation, independent of CS2's menu reports.
    pub mouse_navigation: bool,
    pub content_height: f64,
    /// Width the side panel needs for the chosen columns plus readable names (reported by the page).
    pub side_width: f64,
}

impl Default for OverlayState {
    fn default() -> Self {
        Self { pinned: false, key_view: KeyView::Hidden, menu_open: false, mouse_navigation: false, content_height: 560.0, side_width: 500.0 }
    }
}

impl OverlayState {
    pub fn view(&self) -> &'static str {
        if self.mouse_navigation || self.menu_open {
            "interactive"
        } else if self.key_view == KeyView::Expanded {
            "expanded"
        } else if self.pinned || self.key_view == KeyView::Compact {
            "compact"
        } else {
            "hidden"
        }
    }

    pub fn end_mouse_navigation(&mut self) -> bool {
        if !self.mouse_navigation {
            return false;
        }
        self.mouse_navigation = false;
        self.menu_open = false;
        self.key_view = KeyView::Hidden;
        true
    }
}

/// A rectangle in logical (DPI-independent) pixels relative to a monitor's work area.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// Where the overlay goes inside a work area of `area_width` x `area_height` logical pixels.
pub fn layout(view: &str, settings: &Settings, content_height: f64, side_width: f64, area_width: f64, area_height: f64) -> Bounds {
    let wide = matches!(view, "expanded" | "interactive");
    let layout = if wide { "top" } else { settings.overlay_layout.as_str() };
    let side = matches!(layout, "left" | "right");
    let base = if side {
        side_width.clamp(500.0, 1000.0)
    } else {
        match view {
            "expanded" => 1480.0,
            "interactive" => 1280.0,
            _ => 1180.0,
        }
    };
    let width = (base * settings.overlay_scale).round().min(area_width);
    let height = (content_height * settings.overlay_scale).ceil().min(area_height);
    let clamp_x = |x: f64| x.round().min(area_width - width).max(0.0);
    // Side panels use the horizontal offset as the distance from their screen edge.
    let x = match layout {
        "left" => clamp_x(settings.overlay_x),
        "right" => clamp_x(area_width - width - settings.overlay_x),
        _ => clamp_x((area_width - width) / 2.0 + settings.overlay_x),
    };
    let y = settings.overlay_y.round().min(area_height - height).max(0.0);
    Bounds { x, y, width, height }
}

/// `data_dir` overrides the browser profile folder (smoke tests); it must match the dashboard's.
pub fn create(app: &AppHandle, data_dir: Option<std::path::PathBuf>) -> tauri::Result<WebviewWindow> {
    let mut builder = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("overlay.html".into()));
    if let Some(dir) = data_dir {
        builder = builder.data_directory(dir);
    }
    // The window layer's own show, hide, click-through and always-on-top calls would activate the
    // overlay (taking focus from CS2) or, mixed with native calls, hide it again. After creation, the
    // overlay is therefore only placed, shown and hidden through `native::place`.
    builder
        .initialization_script(crate::FREEZE_PROTOTYPE)
        .title("CS2 Player Intel overlay")
        .inner_size(1180.0, 560.0)
        .transparent(true)
        .decorations(false)
        .shadow(false)
        .resizable(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(false)
        .focusable(false)
        .visible(false)
        .build()
}

/// A window rectangle in physical pixels: x, y, width, height.
type Rect = (i32, i32, i32, i32);

#[cfg(windows)]
mod native {
    use super::Rect;
    use tauri::WebviewWindow;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowLongPtrW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow, GWL_EXSTYLE, HWND_TOPMOST, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_SHOWWINDOW, SW_HIDE, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
    };

    pub(super) fn extended_style(existing: u32, click_through: bool, focusable: bool) -> u32 {
        let mut style = existing | WS_EX_TOOLWINDOW.0;
        style = if focusable { style & !WS_EX_NOACTIVATE.0 } else { style | WS_EX_NOACTIVATE.0 };
        let pass_through = WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0;
        if click_through {
            style | pass_through
        } else {
            style & !pass_through
        }
    }

    pub(super) fn focus(window: &WebviewWindow) -> Result<(), String> {
        let handle = window.hwnd().map_err(|error| error.to_string())?;
        let hwnd = HWND(handle.0 as _);
        // SAFETY: only this app's live overlay is activated, following an explicit user shortcut.
        unsafe {
            // Do not use Tauri's focus fallback: on Windows it can synthesize an Alt keypress.
            if !SetForegroundWindow(hwnd).as_bool() || GetForegroundWindow() != hwnd {
                return Err("Windows did not give the overlay focus. Press the mouse-navigation shortcut while CS2 is in front.".into());
            }
        }
        Ok(())
    }

    /// Moves and sizes the overlay; `show` also shows (topmost, never activated) or hides it, and sets
    /// click-through: the overlay only takes clicks in CS2's Esc menu.
    pub fn place(window: &WebviewWindow, rect: Option<Rect>, show: Option<bool>, click_through: bool, focusable: bool) {
        let Ok(handle) = window.hwnd() else { return };
        let hwnd = HWND(handle.0 as _);
        // SAFETY: hwnd is this app's live overlay window; only its extended style, position and
        // visibility change.
        unsafe {
            if show == Some(false) {
                let _ = ShowWindow(hwnd, SW_HIDE);
                return;
            }
            let mut flags = SWP_NOACTIVATE;
            if show == Some(true) {
                let style = extended_style(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32, click_through, focusable);
                SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style as isize);
                flags |= SWP_SHOWWINDOW | SWP_FRAMECHANGED;
            }
            let (x, y, width, height) = rect.unwrap_or_default();
            if rect.is_none() {
                flags |= SWP_NOMOVE | SWP_NOSIZE;
            }
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, width, height, flags);
        }
    }
}

pub fn monitor_id(monitor: &Monitor) -> String {
    monitor.name().cloned().unwrap_or_default()
}

/// Positions, sizes and scales the overlay for its current view; shows or hides it.
pub fn apply(app: &AppHandle, state: &OverlayState, settings: &Settings, show: bool) -> tauri::Result<()> {
    let Some(window) = app.get_webview_window(LABEL) else { return Ok(()) };
    let monitors = window.available_monitors().unwrap_or_default();
    let monitor = monitors
        .into_iter()
        .find(|m| !settings.overlay_display.is_empty() && monitor_id(m) == settings.overlay_display)
        .or_else(|| window.primary_monitor().ok().flatten());
    let view = state.view();
    let rect: Option<Rect> = monitor.map(|monitor| {
        let area = monitor.work_area();
        let scale = monitor.scale_factor();
        let bounds = layout(view, settings, state.content_height, state.side_width, area.size.width as f64 / scale, area.size.height as f64 / scale);
        let px = |value: f64| (value * scale).round() as i32;
        (area.position.x + px(bounds.x), area.position.y + px(bounds.y), px(bounds.width), px(bounds.height))
    });
    window.set_zoom(settings.overlay_scale)?;
    let show = show.then_some(view != "hidden");
    #[cfg(windows)]
    native::place(&window, rect, show, view != "interactive", state.mouse_navigation);
    #[cfg(not(windows))]
    let _ = (rect, show);
    Ok(())
}

pub fn is_visible(app: &AppHandle) -> bool {
    app.get_webview_window(LABEL).and_then(|w| w.is_visible().ok()).unwrap_or(false)
}

pub fn focus_mouse_navigation(window: &WebviewWindow) -> Result<(), String> {
    // Keep show/hide and activation native: Tauri's cached window flags can hide the overlay again.
    window.set_cursor_visible(true).map_err(|error| error.to_string())?;
    #[cfg(windows)]
    {
        native::focus(window)
    }
    #[cfg(not(windows))]
    {
        Err("Overlay mouse navigation is supported on Windows.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(layout: &str, x: f64, scale: f64) -> Settings {
        Settings {
            csrep_api_key: String::new(),
            steam_web_api_key: String::new(),
            overlay_opacity: 0.92,
            overlay_trigger: "hold-tab".into(),
            overlay_hotkey: "F8".into(),
            interact_hotkey: "Shift+F8".into(),
            faceit_api_key: String::new(),
            theme: "dark".into(),
            overlay_style: "standard".into(),
            overlay_columns: Vec::new(),
            privacy_mode: false,
            show_win_estimate: false,
            profile_indicators: true,
            min_sample_matches: 30.0,
            notifications: true,
            notify_types: Vec::new(),
            retention_days: 0.0,
            source_priority: Vec::new(),
            cache_minutes: 30.0,
            esc_interactive: true,
            expanded_groups: Vec::new(),
            auto_retry_minutes: 2.0,
            steamworks_sdk_path: String::new(),
            cs2_cfg_path: String::new(),
            cs2_console_log_path: String::new(),
            update_source: String::new(),
            csstats_enabled: true,
            csrep_pages_enabled: true,
            scoreboard_colours: false,
            auto_install_updates: true,
            launch_cs2_on_start: true,
            close_to_tray: true,
            start_with_windows: false,
            overlay_display: String::new(),
            overlay_layout: layout.into(),
            overlay_x: x,
            overlay_y: 36.0,
            overlay_scale: scale,
        }
    }

    #[test]
    fn views_follow_keys_pin_and_menu() {
        let mut state = OverlayState::default();
        assert_eq!(state.view(), "hidden");
        state.pinned = true;
        assert_eq!(state.view(), "compact");
        state.key_view = KeyView::Expanded;
        assert_eq!(state.view(), "expanded");
        state.menu_open = true;
        assert_eq!(state.view(), "interactive");
    }

    #[test]
    fn layouts_place_panels_on_the_chosen_edge() {
        let right = layout("compact", &settings("right", 20.0, 1.0), 500.0, 500.0, 1920.0, 1040.0);
        assert_eq!(right, Bounds { x: 1920.0 - 500.0 - 20.0, y: 36.0, width: 500.0, height: 500.0 });
        let left = layout("compact", &settings("left", 20.0, 1.0), 500.0, 500.0, 1920.0, 1040.0);
        assert_eq!(left.x, 20.0);
        let top = layout("expanded", &settings("right", 0.0, 1.0), 2000.0, 500.0, 1920.0, 1040.0);
        assert_eq!((top.width, top.height, top.x, top.y), (1480.0, 1040.0, 220.0, 0.0), "wide views go on top and fit the screen");
        let scaled = layout("compact", &settings("top", 0.0, 1.5), 400.0, 500.0, 1280.0, 720.0);
        let wide_side = layout("compact", &settings("right", 0.0, 1.0), 400.0, 640.0, 1920.0, 1040.0);
        assert_eq!((wide_side.width, wide_side.x), (640.0, 1280.0), "many columns widen the side panel, names keep their room");
        assert_eq!(layout("compact", &settings("right", 0.0, 1.0), 400.0, 5000.0, 1920.0, 1040.0).width, 1000.0);
        assert_eq!((scaled.width, scaled.height), (1280.0, 600.0));
    }

    #[test]
    fn explicit_mouse_navigation_survives_game_menu_and_tab_changes() {
        let mut state = OverlayState { mouse_navigation: true, ..OverlayState::default() };
        assert_eq!(state.view(), "interactive");
        state.menu_open = false;
        state.key_view = KeyView::Expanded;
        assert_eq!(state.view(), "interactive");
        assert!(state.end_mouse_navigation());
        assert_eq!(state.view(), "hidden");
        assert!(!state.end_mouse_navigation());
    }

    #[test]
    fn leaving_mouse_navigation_keeps_a_pinned_overlay_click_through() {
        let mut state = OverlayState { pinned: true, menu_open: true, mouse_navigation: true, ..OverlayState::default() };
        assert!(state.end_mouse_navigation());
        assert_eq!(state.view(), "compact");
        assert!(!state.menu_open);
    }

    #[test]
    fn automatic_views_never_request_mouse_navigation() {
        let mut state = OverlayState { menu_open: true, ..OverlayState::default() };
        assert_eq!(state.view(), "interactive");
        assert!(!state.mouse_navigation);
        state.menu_open = false;
        state.key_view = KeyView::Compact;
        assert_eq!(state.view(), "compact");
        assert!(!state.mouse_navigation);
    }

    #[cfg(windows)]
    #[test]
    fn windows_styles_allow_focus_only_for_explicit_mouse_navigation() {
        use windows::Win32::UI::WindowsAndMessaging::{WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT};
        let passive = native::extended_style(0, true, false);
        assert_ne!(passive & WS_EX_NOACTIVATE.0, 0);
        assert_ne!(passive & WS_EX_TRANSPARENT.0, 0);
        let automatic = native::extended_style(passive, false, false);
        assert_ne!(automatic & WS_EX_NOACTIVATE.0, 0);
        assert_eq!(automatic & (WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0), 0);
        let manual = native::extended_style(passive, false, true);
        assert_eq!(manual & (WS_EX_NOACTIVATE.0 | WS_EX_TRANSPARENT.0 | WS_EX_LAYERED.0), 0);
        assert_ne!(manual & WS_EX_TOOLWINDOW.0, 0);
        assert_eq!(native::extended_style(manual, true, false), passive);
    }
}
