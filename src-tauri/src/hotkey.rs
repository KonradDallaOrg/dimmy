//! Global hotkey via low-level keyboard hook (Win+Alt / Cmd+Alt).
//!
//! `RegisterHotKey` cannot register modifier-only combos, so we use
//! `WH_KEYBOARD_LL` to track key states and detect when both Alt and
//! Win/Cmd are held simultaneously.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

const EVENT_NONE: u8 = 0;
const EVENT_PRESSED: u8 = 1;
const EVENT_RELEASED: u8 = 2;

static KEY1_DOWN: AtomicBool = AtomicBool::new(false);
static KEY2_DOWN: AtomicBool = AtomicBool::new(false);
static COMBO_ACTIVE: AtomicBool = AtomicBool::new(false);
static HOTKEY_EVENT: AtomicU8 = AtomicU8::new(EVENT_NONE);

/// Configurable key group 1 and 2 (each group can match multiple VK codes).
/// Format: up to 2 VK codes packed as (vk1, vk2) in a u32: high 16 = vk1, low 16 = vk2.
/// If vk2 is 0, only vk1 is matched.
static KEY1_CODES: AtomicU32 = AtomicU32::new(0);
static KEY2_CODES: AtomicU32 = AtomicU32::new(0);

/// Shortcut presets: (name, key1_left, key1_right, key2_left, key2_right)
pub const SHORTCUT_PRESETS: &[(&str, &str)] = &[
    ("win+alt", "Win + Alt"),
    ("ctrl+alt", "Ctrl + Alt"),
    ("ctrl+shift", "Ctrl + Shift"),
];

/// Set the shortcut combo by preset name. Must be called before install().
pub fn set_shortcut(preset: &str) {
    let (k1, k2) = match preset {
        "ctrl+alt" => (
            pack_keys(0xA2, 0xA3),  // VK_LCONTROL, VK_RCONTROL
            pack_keys(0xA4, 0xA5),  // VK_LMENU, VK_RMENU
        ),
        "ctrl+shift" => (
            pack_keys(0xA2, 0xA3),  // VK_LCONTROL, VK_RCONTROL
            pack_keys(0xA0, 0xA1),  // VK_LSHIFT, VK_RSHIFT
        ),
        _ => (
            // Default: win+alt
            pack_keys(0x5B, 0x5C),  // VK_LWIN, VK_RWIN
            pack_keys(0xA4, 0xA5),  // VK_LMENU, VK_RMENU
        ),
    };
    KEY1_CODES.store(k1, Ordering::SeqCst);
    KEY2_CODES.store(k2, Ordering::SeqCst);
}

fn pack_keys(left: u32, right: u32) -> u32 {
    (left << 16) | (right & 0xFFFF)
}

fn matches_key_group(vk: u32, packed: u32) -> bool {
    let left = packed >> 16;
    let right = packed & 0xFFFF;
    vk == left || (right != 0 && vk == right)
}

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

            let k1 = KEY1_CODES.load(Ordering::SeqCst);
            let k2 = KEY2_CODES.load(Ordering::SeqCst);

            if matches_key_group(vk, k1) {
                if is_down {
                    KEY1_DOWN.store(true, Ordering::SeqCst);
                    if KEY2_DOWN.load(Ordering::SeqCst)
                        && !COMBO_ACTIVE.swap(true, Ordering::SeqCst)
                    {
                        HOTKEY_EVENT.store(EVENT_PRESSED, Ordering::SeqCst);
                    }
                } else if is_up {
                    KEY1_DOWN.store(false, Ordering::SeqCst);
                    if COMBO_ACTIVE.swap(false, Ordering::SeqCst) {
                        HOTKEY_EVENT.store(EVENT_RELEASED, Ordering::SeqCst);
                    }
                }
            } else if matches_key_group(vk, k2) {
                if is_down {
                    KEY2_DOWN.store(true, Ordering::SeqCst);
                    if KEY1_DOWN.load(Ordering::SeqCst)
                        && !COMBO_ACTIVE.swap(true, Ordering::SeqCst)
                    {
                        HOTKEY_EVENT.store(EVENT_PRESSED, Ordering::SeqCst);
                    }
                } else if is_up {
                    KEY2_DOWN.store(false, Ordering::SeqCst);
                    if COMBO_ACTIVE.swap(false, Ordering::SeqCst) {
                        HOTKEY_EVENT.store(EVENT_RELEASED, Ordering::SeqCst);
                    }
                }
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
