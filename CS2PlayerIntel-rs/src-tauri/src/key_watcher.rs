//! Watches the foreground window while CS2 runs and, when a feature needs keys, polls the keyboard state
//! (GetAsyncKeyState). Nothing is hooked, injected or consumed: CS2 still receives every key. It reports:
//! - which window is in front: CS2, one of this app's own, or another app's (about ten times a second);
//! - with keys: the view (compact while Tab is held, expanded while Ctrl+Tab is held, otherwise hidden),
//!   Esc presses while CS2 is in front (CS2's pause menu opens or closes), and whether Tab is held with
//!   CS2 in front (the scoreboard is open), for reading teammate colours.

use crate::overlay::KeyView;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

const INTERVAL: Duration = Duration::from_millis(33);
const FOREGROUND_EVERY: u32 = 3;
const VK_TAB: i32 = 0x09;
const VK_CONTROL: i32 = 0x11;
const VK_ESCAPE: i32 = 0x1b;

/// The window in front.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Front {
    Cs2,
    /// One of this app's windows (the overlay with mouse navigation, a profile opened over the game).
    Own,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    View(KeyView),
    Escape,
    Foreground(Front),
    /// CS2's scoreboard is open (Tab held with CS2 in front) or closed again.
    Scoreboard(bool),
}

/// The keyboard as the watcher sees it; tests use a scripted stand-in.
pub trait Keyboard {
    fn is_down(&self, key: i32) -> bool;
    fn foreground_title(&self) -> String;
    /// The window in front belongs to this app.
    fn foreground_is_own(&self) -> bool;
}

pub fn is_cs2_title(title: &str) -> bool {
    title.starts_with("Counter-Strike 2")
}

pub fn is_cs2_foreground() -> bool {
    #[cfg(windows)]
    {
        is_cs2_title(&Win32Keyboard.foreground_title())
    }
    #[cfg(not(windows))]
    {
        false
    }
}

#[cfg(windows)]
pub struct Win32Keyboard;

#[cfg(windows)]
impl Keyboard for Win32Keyboard {
    fn is_down(&self, key: i32) -> bool {
        // SAFETY: GetAsyncKeyState only reads the key state for a virtual-key code.
        (unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(key) } as u16 & 0x8000) != 0
    }

    fn foreground_title(&self) -> String {
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};
        let mut buffer = [0u16; 256];
        // SAFETY: the buffer outlives the call and its length is passed with it.
        let length = unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_invalid() {
                return String::new();
            }
            GetWindowTextW(hwnd, &mut buffer)
        };
        String::from_utf16_lossy(&buffer[..length.max(0) as usize])
    }

    fn foreground_is_own(&self) -> bool {
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};
        let mut process = 0u32;
        // SAFETY: GetWindowThreadProcessId only writes the process id into the local variable.
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.is_invalid() {
                return false;
            }
            GetWindowThreadProcessId(hwnd, Some(&mut process));
        }
        process == std::process::id()
    }
}

/// One polling step's state, separate from the thread so it can be tested.
pub struct Watch {
    pub hold_tab: bool,
    /// Keys are read only when a feature needs them (hold Tab, the Esc-menu overlay, scoreboard colours).
    pub keys: bool,
    view: KeyView,
    esc_down: bool,
    ticks: u32,
    front: Option<Front>,
    scoreboard: bool,
}

impl Watch {
    pub fn new(hold_tab: bool, keys: bool) -> Self {
        Self { hold_tab, keys, view: KeyView::Hidden, esc_down: false, ticks: 0, front: None, scoreboard: false }
    }

    pub fn tick(&mut self, keyboard: &impl Keyboard, events: &mut Vec<KeyEvent>) {
        if self.ticks % FOREGROUND_EVERY == 0 {
            let front = if is_cs2_title(&keyboard.foreground_title()) {
                Front::Cs2
            } else if keyboard.foreground_is_own() {
                Front::Own
            } else {
                Front::Other
            };
            if Some(front) != self.front {
                self.front = Some(front);
                events.push(KeyEvent::Foreground(front));
            }
        }
        self.ticks = self.ticks.wrapping_add(1);
        let mut view = KeyView::Hidden;
        let scoreboard = self.keys && keyboard.is_down(VK_TAB) && is_cs2_title(&keyboard.foreground_title());
        if self.hold_tab && scoreboard {
            view = if keyboard.is_down(VK_CONTROL) { KeyView::Expanded } else { KeyView::Compact };
        }
        if scoreboard != self.scoreboard {
            self.scoreboard = scoreboard;
            events.push(KeyEvent::Scoreboard(scoreboard));
        }
        let esc_down = self.keys && keyboard.is_down(VK_ESCAPE);
        if esc_down && !self.esc_down && is_cs2_title(&keyboard.foreground_title()) {
            events.push(KeyEvent::Escape);
        }
        self.esc_down = esc_down;
        if view != self.view {
            self.view = view;
            events.push(KeyEvent::View(view));
        }
    }
}

/// The polling thread. Dropping it stops the thread.
pub struct KeyWatcher {
    stop: Arc<AtomicBool>,
    hold_tab: Arc<AtomicBool>,
    keys: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl KeyWatcher {
    #[cfg(windows)]
    pub fn start(hold_tab: bool, keys: bool, on_event: impl Fn(KeyEvent) + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let hold = Arc::new(AtomicBool::new(hold_tab));
        let read_keys = Arc::new(AtomicBool::new(keys));
        let (thread_stop, thread_hold, thread_keys) = (stop.clone(), hold.clone(), read_keys.clone());
        let thread = std::thread::Builder::new()
            .name("key-watcher".into())
            .spawn(move || {
                let keyboard = Win32Keyboard;
                let mut watch = Watch::new(hold_tab, keys);
                let mut events = Vec::new();
                while !thread_stop.load(Ordering::Relaxed) {
                    watch.hold_tab = thread_hold.load(Ordering::Relaxed);
                    watch.keys = thread_keys.load(Ordering::Relaxed);
                    watch.tick(&keyboard, &mut events);
                    for event in events.drain(..) {
                        on_event(event);
                    }
                    std::thread::sleep(INTERVAL);
                }
                if watch.view != KeyView::Hidden {
                    on_event(KeyEvent::View(KeyView::Hidden));
                }
            })
            .ok();
        Self { stop, hold_tab: hold, keys: read_keys, thread }
    }

    pub fn set_hold_tab(&self, hold_tab: bool, keys: bool) {
        self.hold_tab.store(hold_tab, Ordering::Relaxed);
        self.keys.store(keys, Ordering::Relaxed);
    }
}

impl Drop for KeyWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Scripted {
        down: RefCell<Vec<i32>>,
        title: RefCell<String>,
        own: RefCell<bool>,
    }

    impl Keyboard for Scripted {
        fn is_down(&self, key: i32) -> bool {
            self.down.borrow().contains(&key)
        }
        fn foreground_title(&self) -> String {
            self.title.borrow().clone()
        }
        fn foreground_is_own(&self) -> bool {
            *self.own.borrow()
        }
    }

    fn scripted(down: Vec<i32>, title: &str) -> Scripted {
        Scripted { down: RefCell::new(down), title: RefCell::new(title.into()), own: RefCell::new(false) }
    }

    #[test]
    fn reports_views_escape_and_foreground() {
        let keyboard = scripted(vec![VK_TAB], "Counter-Strike 2");
        let mut watch = Watch::new(true, true);
        let mut events = Vec::new();
        watch.tick(&keyboard, &mut events);
        assert_eq!(events, [KeyEvent::Foreground(Front::Cs2), KeyEvent::Scoreboard(true), KeyEvent::View(KeyView::Compact)]);
        events.clear();
        keyboard.down.borrow_mut().extend([VK_CONTROL, VK_ESCAPE]);
        watch.tick(&keyboard, &mut events);
        assert_eq!(events, [KeyEvent::Escape, KeyEvent::View(KeyView::Expanded)]);
        events.clear();
        watch.tick(&keyboard, &mut events);
        assert!(events.is_empty(), "a held Esc is one press");
        *keyboard.title.borrow_mut() = "Discord".into();
        watch.hold_tab = false;
        watch.tick(&keyboard, &mut events);
        assert_eq!(events, [KeyEvent::Foreground(Front::Other), KeyEvent::Scoreboard(false), KeyEvent::View(KeyView::Hidden)]);
        assert!(!is_cs2_title("Notepad") && is_cs2_title("Counter-Strike 2 - Direct3D 11"));
    }

    #[test]
    fn tells_cs2_this_app_and_other_windows_apart() {
        let keyboard = scripted(vec![], "Counter-Strike 2");
        let mut watch = Watch::new(false, false);
        let mut events = Vec::new();
        let mut front_after = |title: &str, own: bool, events: &mut Vec<KeyEvent>| {
            *keyboard.title.borrow_mut() = title.into();
            *keyboard.own.borrow_mut() = own;
            events.clear();
            for _ in 0..FOREGROUND_EVERY {
                watch.tick(&keyboard, events);
            }
            events.clone()
        };
        assert_eq!(front_after("Counter-Strike 2", false, &mut events), [KeyEvent::Foreground(Front::Cs2)]);
        assert_eq!(front_after("Discord", false, &mut events), [KeyEvent::Foreground(Front::Other)]);
        assert_eq!(front_after("CS2 Player Intel overlay", true, &mut events), [KeyEvent::Foreground(Front::Own)]);
        assert_eq!(front_after("CS2 Player Intel overlay", true, &mut events), [], "only changes are reported");
    }

    #[test]
    fn keys_are_not_read_unless_a_feature_needs_them() {
        let keyboard = scripted(vec![VK_TAB, VK_ESCAPE], "Counter-Strike 2");
        let mut watch = Watch::new(true, false);
        let mut events = Vec::new();
        watch.tick(&keyboard, &mut events);
        assert_eq!(events, [KeyEvent::Foreground(Front::Cs2)], "the foreground only");
    }
}
