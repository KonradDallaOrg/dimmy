//! Global hotkey via low-level keyboard hook.
//!
//! Supports any 2-modifier-key combo (e.g. Win+Alt, Ctrl+Shift).
//! Keys are configured at runtime via `set_shortcut()`.
//! Recording mode captures the next 2-modifier combo pressed by the user.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

const EVENT_NONE: u8 = 0;
const EVENT_PRESSED: u8 = 1;
const EVENT_RELEASED: u8 = 2;

static KEY1_DOWN: AtomicBool = AtomicBool::new(false);
static KEY2_DOWN: AtomicBool = AtomicBool::new(false);
static COMBO_ACTIVE: AtomicBool = AtomicBool::new(false);
static HOTKEY_EVENT: AtomicU8 = AtomicU8::new(EVENT_NONE);

/// Configurable key group 1 and 2 (each group can match L/R variants).
/// Format: high 16 bits = left VK, low 16 bits = right VK.
static KEY1_CODES: AtomicU32 = AtomicU32::new(0);
static KEY2_CODES: AtomicU32 = AtomicU32::new(0);

/// Recording mode: when true, the hook captures pressed modifier groups
/// instead of triggering hotkey events.
static RECORDING: AtomicBool = AtomicBool::new(false);
/// Captured groups during recording. Each stores a modifier group id (1-4), 0 = empty.
static REC_GROUP1: AtomicU8 = AtomicU8::new(0);
static REC_GROUP2: AtomicU8 = AtomicU8::new(0);
/// Set to true when recording captured a valid 2-key combo.
static REC_DONE: AtomicBool = AtomicBool::new(false);

// ── Modifier groups ──────────────────────────────────────────────────
// Each modifier key (Win, Alt, Ctrl, Shift) has left/right VK codes.
// We identify them by group id for recording.

const GROUP_WIN: u8 = 1;
const GROUP_ALT: u8 = 2;
const GROUP_CTRL: u8 = 3;
const GROUP_SHIFT: u8 = 4;

const VK_LWIN: u32 = 0x5B;
const VK_RWIN: u32 = 0x5C;
const VK_LMENU: u32 = 0xA4;
const VK_RMENU: u32 = 0xA5;
const VK_LCONTROL: u32 = 0xA2;
const VK_RCONTROL: u32 = 0xA3;
const VK_LSHIFT: u32 = 0xA0;
const VK_RSHIFT: u32 = 0xA1;

/// Map a VK code to its modifier group id, or 0 if not a modifier.
fn vk_to_group(vk: u32) -> u8 {
    match vk {
        VK_LWIN | VK_RWIN => GROUP_WIN,
        VK_LMENU | VK_RMENU => GROUP_ALT,
        VK_LCONTROL | VK_RCONTROL => GROUP_CTRL,
        VK_LSHIFT | VK_RSHIFT => GROUP_SHIFT,
        _ => 0,
    }
}

fn group_to_packed(group: u8) -> u32 {
    match group {
        GROUP_WIN => pack_keys(VK_LWIN, VK_RWIN),
        GROUP_ALT => pack_keys(VK_LMENU, VK_RMENU),
        GROUP_CTRL => pack_keys(VK_LCONTROL, VK_RCONTROL),
        GROUP_SHIFT => pack_keys(VK_LSHIFT, VK_RSHIFT),
        _ => 0,
    }
}

fn group_to_name(group: u8) -> &'static str {
    match group {
        GROUP_WIN => "win",
        GROUP_ALT => "alt",
        GROUP_CTRL => "ctrl",
        GROUP_SHIFT => "shift",
        _ => "?",
    }
}

fn group_to_label(group: u8) -> &'static str {
    match group {
        GROUP_WIN => "Win",
        GROUP_ALT => "Alt",
        GROUP_CTRL => "Ctrl",
        GROUP_SHIFT => "Shift",
        _ => "?",
    }
}

fn name_to_group(name: &str) -> u8 {
    match name {
        "win" => GROUP_WIN,
        "alt" => GROUP_ALT,
        "ctrl" => GROUP_CTRL,
        "shift" => GROUP_SHIFT,
        _ => 0,
    }
}

// ── Public API ───────────────────────────────────────────────────────

fn pack_keys(left: u32, right: u32) -> u32 {
    (left << 16) | (right & 0xFFFF)
}

fn matches_key_group(vk: u32, packed: u32) -> bool {
    let left = packed >> 16;
    let right = packed & 0xFFFF;
    vk == left || (right != 0 && vk == right)
}

/// Set the shortcut combo from a string like "win+alt", "ctrl+shift".
pub fn set_shortcut(combo: &str) {
    let parts: Vec<&str> = combo.split('+').collect();
    if parts.len() == 2 {
        let g1 = name_to_group(parts[0]);
        let g2 = name_to_group(parts[1]);
        if g1 != 0 && g2 != 0 {
            KEY1_CODES.store(group_to_packed(g1), Ordering::SeqCst);
            KEY2_CODES.store(group_to_packed(g2), Ordering::SeqCst);
            return;
        }
    }
    // Fallback: win+alt
    KEY1_CODES.store(group_to_packed(GROUP_WIN), Ordering::SeqCst);
    KEY2_CODES.store(group_to_packed(GROUP_ALT), Ordering::SeqCst);
}

/// Get the human-readable label for the current shortcut (e.g. "Win + Alt").
pub fn current_label() -> String {
    let k1 = KEY1_CODES.load(Ordering::SeqCst);
    let k2 = KEY2_CODES.load(Ordering::SeqCst);
    let g1 = vk_to_group(k1 >> 16);
    let g2 = vk_to_group(k2 >> 16);
    format!("{} + {}", group_to_label(g1), group_to_label(g2))
}

/// Start recording mode. The hook will capture the next 2-modifier combo.
pub fn start_recording() {
    REC_GROUP1.store(0, Ordering::SeqCst);
    REC_GROUP2.store(0, Ordering::SeqCst);
    REC_DONE.store(false, Ordering::SeqCst);
    RECORDING.store(true, Ordering::SeqCst);
}

/// Check if recording captured a combo. Returns Some((name, label)) or None.
pub fn take_recorded() -> Option<(String, String)> {
    if !REC_DONE.load(Ordering::SeqCst) {
        return None;
    }
    RECORDING.store(false, Ordering::SeqCst);
    REC_DONE.store(false, Ordering::SeqCst);
    let g1 = REC_GROUP1.load(Ordering::SeqCst);
    let g2 = REC_GROUP2.load(Ordering::SeqCst);
    if g1 != 0 && g2 != 0 && g1 != g2 {
        let name = format!("{}+{}", group_to_name(g1), group_to_name(g2));
        let label = format!("{} + {}", group_to_label(g1), group_to_label(g2));
        Some((name, label))
    } else {
        None
    }
}

/// Cancel recording mode.
pub fn stop_recording() {
    RECORDING.store(false, Ordering::SeqCst);
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

            // ── Recording mode: capture modifier combo ──
            if RECORDING.load(Ordering::SeqCst) {
                if is_down {
                    let group = vk_to_group(vk);
                    if group != 0 {
                        let g1 = REC_GROUP1.load(Ordering::SeqCst);
                        if g1 == 0 {
                            REC_GROUP1.store(group, Ordering::SeqCst);
                        } else if g1 != group && REC_GROUP2.load(Ordering::SeqCst) == 0 {
                            REC_GROUP2.store(group, Ordering::SeqCst);
                            REC_DONE.store(true, Ordering::SeqCst);
                        }
                    }
                } else if is_up {
                    // Reset on key release so user can retry
                    let group = vk_to_group(vk);
                    if group != 0 && !REC_DONE.load(Ordering::SeqCst) {
                        let g1 = REC_GROUP1.load(Ordering::SeqCst);
                        if g1 == group {
                            REC_GROUP1.store(0, Ordering::SeqCst);
                        }
                    }
                }
                return CallNextHookEx(HOOK_HANDLE.load(Ordering::SeqCst), code, wparam, lparam);
            }

            // ── Normal mode: detect configured combo ──
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
                log_fn("ERROR: Failed to install keyboard hook");
                return;
            }
            HOOK_HANDLE.store(hook, Ordering::SeqCst);
            log_fn("Keyboard hook installed — shortcut active");

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
