//! Global hotkey via low-level keyboard hook (Win+Alt / Cmd+Alt).
//!
//! `RegisterHotKey` cannot register modifier-only combos, so we use
//! `WH_KEYBOARD_LL` to track key states and detect when both Alt and
//! Win/Cmd are held simultaneously.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

const EVENT_NONE: u8 = 0;
const EVENT_PRESSED: u8 = 1;
const EVENT_RELEASED: u8 = 2;

static ALT_DOWN: AtomicBool = AtomicBool::new(false);
static WIN_DOWN: AtomicBool = AtomicBool::new(false);
static COMBO_ACTIVE: AtomicBool = AtomicBool::new(false);
static HOTKEY_EVENT: AtomicU8 = AtomicU8::new(EVENT_NONE);

/// Take the latest hotkey event: 0=none, 1=pressed, 2=released.
pub fn take_event() -> u8 {
    HOTKEY_EVENT.swap(EVENT_NONE, Ordering::SeqCst)
}

/// Install the global keyboard hook. Must be called from the main thread context.
pub fn install(log_fn: fn(&str)) {
    platform::install_hook(log_fn);
}

// ── Windows implementation ──────────────────────────────────────────

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::sync::atomic::AtomicIsize;

    static HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);

    // Virtual key codes
    const VK_LWIN: u32 = 0x5B;
    const VK_RWIN: u32 = 0x5C;
    const VK_LMENU: u32 = 0xA4;
    const VK_RMENU: u32 = 0xA5;

    // Window messages
    const WM_KEYDOWN: usize = 0x0100;
    const WM_KEYUP: usize = 0x0101;
    const WM_SYSKEYDOWN: usize = 0x0104;
    const WM_SYSKEYUP: usize = 0x0105;

    const WH_KEYBOARD_LL: i32 = 13;

    #[allow(non_snake_case)]
    #[repr(C)]
    struct KBDLLHOOKSTRUCT {
        vkCode: u32,
        scanCode: u32,
        flags: u32,
        time: u32,
        dwExtraInfo: usize,
    }

    #[repr(C)]
    #[derive(Default)]
    struct MSG {
        hwnd: isize,
        message: u32,
        wparam: usize,
        lparam: isize,
        time: u32,
        pt_x: i32,
        pt_y: i32,
    }

    extern "system" {
        fn SetWindowsHookExW(
            idHook: i32,
            lpfn: Option<unsafe extern "system" fn(i32, usize, isize) -> isize>,
            hmod: isize,
            dwThreadId: u32,
        ) -> isize;
        fn CallNextHookEx(hhk: isize, nCode: i32, wParam: usize, lParam: isize) -> isize;
        fn GetMessageW(lpMsg: *mut MSG, hWnd: isize, wMsgFilterMin: u32, wMsgFilterMax: u32) -> i32;
    }

    unsafe extern "system" fn keyboard_proc(code: i32, wparam: usize, lparam: isize) -> isize {
        if code >= 0 && lparam != 0 {
            let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            let vk = kb.vkCode;
            let is_down = wparam == WM_KEYDOWN || wparam == WM_SYSKEYDOWN;
            let is_up = wparam == WM_KEYUP || wparam == WM_SYSKEYUP;

            match vk {
                VK_LWIN | VK_RWIN => {
                    if is_down {
                        WIN_DOWN.store(true, Ordering::SeqCst);
                        if ALT_DOWN.load(Ordering::SeqCst)
                            && !COMBO_ACTIVE.swap(true, Ordering::SeqCst)
                        {
                            HOTKEY_EVENT.store(EVENT_PRESSED, Ordering::SeqCst);
                        }
                    } else if is_up {
                        WIN_DOWN.store(false, Ordering::SeqCst);
                        if COMBO_ACTIVE.swap(false, Ordering::SeqCst) {
                            HOTKEY_EVENT.store(EVENT_RELEASED, Ordering::SeqCst);
                        }
                    }
                }
                VK_LMENU | VK_RMENU => {
                    if is_down {
                        ALT_DOWN.store(true, Ordering::SeqCst);
                        if WIN_DOWN.load(Ordering::SeqCst)
                            && !COMBO_ACTIVE.swap(true, Ordering::SeqCst)
                        {
                            HOTKEY_EVENT.store(EVENT_PRESSED, Ordering::SeqCst);
                        }
                    } else if is_up {
                        ALT_DOWN.store(false, Ordering::SeqCst);
                        if COMBO_ACTIVE.swap(false, Ordering::SeqCst) {
                            HOTKEY_EVENT.store(EVENT_RELEASED, Ordering::SeqCst);
                        }
                    }
                }
                _ => {}
            }
        }
        unsafe { CallNextHookEx(HOOK_HANDLE.load(Ordering::SeqCst), code, wparam, lparam) }
    }

    pub fn install_hook(log_fn: fn(&str)) {
        std::thread::spawn(move || unsafe {
            let hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), 0, 0);
            if hook == 0 {
                log_fn("ERROR: Failed to install keyboard hook for Win+Alt");
                return;
            }
            HOOK_HANDLE.store(hook, Ordering::SeqCst);
            log_fn("Keyboard hook installed — Win+Alt shortcut active");

            // Low-level hooks require a message pump on the installing thread
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, 0, 0, 0) > 0 {}
        });
    }
}

// ── Non-Windows stub ────────────────────────────────────────────────

#[cfg(not(target_os = "windows"))]
mod platform {
    pub fn install_hook(_log_fn: fn(&str)) {
        // TODO: macOS CGEventTap implementation
    }
}
