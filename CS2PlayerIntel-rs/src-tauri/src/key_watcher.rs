//! Polls the keyboard state (GetAsyncKeyState) while CS2 runs. Nothing is hooked, injected or consumed:
//! CS2 still receives every key. It reports:
//! - the view: compact while Tab is held, expanded while Ctrl+Tab is held, otherwise hidden;
//! - Esc presses while CS2 is in front (CS2's pause menu opens or closes);
//! - whether CS2 is the foreground window (checked about twice a second);
//! - whether Tab is held with CS2 in front (the scoreboard is open), for reading teammate colours.

use crate::overlay::KeyView;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

const INTERVAL: Duration = Duration::from_millis(33);
const FOREGROUND_EVERY: u32 = 15;
const VK_TAB: i32 = 0x09;
const VK_CONTROL: i32 = 0x11;
const VK_ESCAPE: i32 = 0x1b;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyEvent {
    View(KeyView),
    Escape,
    Foreground(bool),
    /// CS2's scoreboard is open (Tab held with CS2 in front) or closed again.
    Scoreboard(bool),
}

/// The keyboard as the watcher sees it; tests use a scripted stand-in.
pub trait Keyboard {
    fn is_down(&self, key: i32) -> bool;
    fn foreground_title(&self) -> String;
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
}

/// One polling step's state, separate from the thread so it can be tested.
pub struct Watch {
    pub hold_tab: bool,
    view: KeyView,
    esc_down: bool,
    ticks: u32,
    cs2_front: Option<bool>,
    scoreboard: bool,
}

impl Watch {
    pub fn new(hold_tab: bool) -> Self {
        Self { hold_tab, view: KeyView::Hidden, esc_down: false, ticks: 0, cs2_front: None, scoreboard: false }
    }

    pub fn tick(&mut self, keyboard: &impl Keyboard, events: &mut Vec<KeyEvent>) {
        let mut view = KeyView::Hidden;
        let scoreboard = keyboard.is_down(VK_TAB) && is_cs2_title(&keyboard.foreground_title());
        if self.hold_tab && scoreboard {
            view = if keyboard.is_down(VK_CONTROL) { KeyView::Expanded } else { KeyView::Compact };
        }
        if scoreboard != self.scoreboard {
            self.scoreboard = scoreboard;
            events.push(KeyEvent::Scoreboard(scoreboard));
        }
        let esc_down = keyboard.is_down(VK_ESCAPE);
        if esc_down && !self.esc_down && is_cs2_title(&keyboard.foreground_title()) {
            events.push(KeyEvent::Escape);
        }
        self.esc_down = esc_down;
        if self.ticks % FOREGROUND_EVERY == 0 {
            let front = is_cs2_title(&keyboard.foreground_title());
            if Some(front) != self.cs2_front {
                self.cs2_front = Some(front);
                events.push(KeyEvent::Foreground(front));
            }
        }
        self.ticks = self.ticks.wrapping_add(1);
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
    thread: Option<JoinHandle<()>>,
}

impl KeyWatcher {
    #[cfg(windows)]
    pub fn start(hold_tab: bool, on_event: impl Fn(KeyEvent) + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let hold = Arc::new(AtomicBool::new(hold_tab));
        let (thread_stop, thread_hold) = (stop.clone(), hold.clone());
        let thread = std::thread::Builder::new()
            .name("key-watcher".into())
            .spawn(move || {
                let keyboard = Win32Keyboard;
                let mut watch = Watch::new(hold_tab);
                let mut events = Vec::new();
                while !thread_stop.load(Ordering::Relaxed) {
                    watch.hold_tab = thread_hold.load(Ordering::Relaxed);
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
        Self { stop, hold_tab: hold, thread }
    }

    pub fn set_hold_tab(&self, hold_tab: bool) {
        self.hold_tab.store(hold_tab, Ordering::Relaxed);
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
    }

    impl Keyboard for Scripted {
        fn is_down(&self, key: i32) -> bool {
            self.down.borrow().contains(&key)
        }
        fn foreground_title(&self) -> String {
            self.title.borrow().clone()
        }
    }

    #[test]
    fn reports_views_escape_and_foreground() {
        let keyboard = Scripted { down: RefCell::new(vec![VK_TAB]), title: RefCell::new("Counter-Strike 2".into()) };
        let mut watch = Watch::new(true);
        let mut events = Vec::new();
        watch.tick(&keyboard, &mut events);
        assert_eq!(events, [KeyEvent::Scoreboard(true), KeyEvent::Foreground(true), KeyEvent::View(KeyView::Compact)]);
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
        assert_eq!(events, [KeyEvent::Scoreboard(false), KeyEvent::View(KeyView::Hidden)]);
        assert!(!is_cs2_title("Notepad") && is_cs2_title("Counter-Strike 2 - Direct3D 11"));
    }
}
