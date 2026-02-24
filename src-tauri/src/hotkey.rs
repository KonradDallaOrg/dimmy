//! Global hotkey via low-level keyboard hook.
//!
//! Supports two shortcut formats:
//!   - 2 modifiers: e.g. Win+Alt, Ctrl+Shift
//!   - 2 modifiers + 1 key: e.g. Ctrl+Shift+Space, Win+Alt+N
//!
//! Keys are configured at runtime via `set_shortcut()`.
//! Recording mode uses GetAsyncKeyState polling to capture the combo.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

const EVENT_NONE: u8 = 0;
const EVENT_PRESSED: u8 = 1;
const EVENT_RELEASED: u8 = 2;

static KEY1_DOWN: AtomicBool = AtomicBool::new(false);
static KEY2_DOWN: AtomicBool = AtomicBool::new(false);
static KEY3_DOWN: AtomicBool = AtomicBool::new(false);
static COMBO_ACTIVE: AtomicBool = AtomicBool::new(false);
static HOTKEY_EVENT: AtomicU8 = AtomicU8::new(EVENT_NONE);

/// Modifier group 1 and 2 (packed L/R VK codes).
static KEY1_CODES: AtomicU32 = AtomicU32::new(0);
static KEY2_CODES: AtomicU32 = AtomicU32::new(0);
/// Non-modifier key VK code (0 = 2-mod-only combo).
static KEY3_CODE: AtomicU32 = AtomicU32::new(0);

/// Recording mode flag.
static RECORDING: AtomicBool = AtomicBool::new(false);
static REC_GROUP1: AtomicU8 = AtomicU8::new(0);
static REC_GROUP2: AtomicU8 = AtomicU8::new(0);
static REC_KEY3: AtomicU32 = AtomicU32::new(0);
static REC_DONE: AtomicBool = AtomicBool::new(false);
/// Countdown: when 2 mods detected without a key, wait before confirming 2-mod combo.
static REC_WAIT: AtomicU8 = AtomicU8::new(0);

// ── Modifier groups ──────────────────────────────────────────────────

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

// ── Non-modifier key names ──────────────────────────────────────────

fn vk_to_name(vk: u32) -> String {
    match vk {
        0x08 => "backspace".into(),
        0x09 => "tab".into(),
        0x0D => "enter".into(),
        0x1B => "esc".into(),
        0x20 => "space".into(),
        0x30..=0x39 => String::from(char::from(vk as u8)),
        0x41..=0x5A => String::from(char::from(vk as u8).to_ascii_lowercase()),
        0x60..=0x69 => format!("num{}", vk - 0x60),
        0x6A => "num*".into(),
        0x6B => "num+".into(),
        0x6D => "num-".into(),
        0x6E => "num.".into(),
        0x6F => "num/".into(),
        0x70..=0x7B => format!("f{}", vk - 0x6F),
        _ => format!("0x{:02X}", vk),
    }
}

fn vk_to_label(vk: u32) -> String {
    match vk {
        0x08 => "Backspace".into(),
        0x09 => "Tab".into(),
        0x0D => "Enter".into(),
        0x1B => "Esc".into(),
        0x20 => "Space".into(),
        0x30..=0x39 => String::from(char::from(vk as u8)),
        0x41..=0x5A => String::from(char::from(vk as u8)),
        0x60..=0x69 => format!("Num{}", vk - 0x60),
        0x6A => "Num*".into(),
        0x6B => "Num+".into(),
        0x6D => "Num-".into(),
        0x6E => "Num.".into(),
        0x6F => "Num/".into(),
        0x70..=0x7B => format!("F{}", vk - 0x6F),
        _ => format!("0x{:02X}", vk),
    }
}

fn name_to_vk(name: &str) -> u32 {
    match name {
        "backspace" => 0x08,
        "tab" => 0x09,
        "enter" => 0x0D,
        "esc" => 0x1B,
        "space" => 0x20,
        "num*" => 0x6A,
        "num+" => 0x6B,
        "num-" => 0x6D,
        "num." => 0x6E,
        "num/" => 0x6F,
        _ => {
            let bytes = name.as_bytes();
            if bytes.len() == 1 {
                let c = bytes[0];
                if c >= b'a' && c <= b'z' {
                    return (c - b'a' + 0x41) as u32;
                }
                if c >= b'0' && c <= b'9' {
                    return c as u32;
                }
            }
            // f1-f12
            if name.starts_with('f') {
                if let Ok(n) = name[1..].parse::<u32>() {
                    if n >= 1 && n <= 12 {
                        return 0x6F + n;
                    }
                }
            }
            // num0-num9
            if name.starts_with("num") && name.len() == 4 {
                let d = name.as_bytes()[3];
                if d >= b'0' && d <= b'9' {
                    return 0x60 + (d - b'0') as u32;
                }
            }
            0
        }
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

/// Set the shortcut combo from a string like "win+alt" or "ctrl+shift+space".
pub fn set_shortcut(combo: &str) {
    let parts: Vec<&str> = combo.split('+').collect();
    if parts.len() == 2 {
        let g1 = name_to_group(parts[0]);
        let g2 = name_to_group(parts[1]);
        if g1 != 0 && g2 != 0 {
            KEY1_CODES.store(group_to_packed(g1), Ordering::SeqCst);
            KEY2_CODES.store(group_to_packed(g2), Ordering::SeqCst);
            KEY3_CODE.store(0, Ordering::SeqCst);
            KEY3_DOWN.store(false, Ordering::SeqCst);
            return;
        }
    } else if parts.len() == 3 {
        let g1 = name_to_group(parts[0]);
        let g2 = name_to_group(parts[1]);
        let vk = name_to_vk(parts[2]);
        if g1 != 0 && g2 != 0 && vk != 0 {
            KEY1_CODES.store(group_to_packed(g1), Ordering::SeqCst);
            KEY2_CODES.store(group_to_packed(g2), Ordering::SeqCst);
            KEY3_CODE.store(vk, Ordering::SeqCst);
            KEY3_DOWN.store(false, Ordering::SeqCst);
            return;
        }
    }
    // Fallback: win+alt
    KEY1_CODES.store(group_to_packed(GROUP_WIN), Ordering::SeqCst);
    KEY2_CODES.store(group_to_packed(GROUP_ALT), Ordering::SeqCst);
    KEY3_CODE.store(0, Ordering::SeqCst);
    KEY3_DOWN.store(false, Ordering::SeqCst);
}

/// Get the human-readable label for the current shortcut.
pub fn current_label() -> String {
    let k1 = KEY1_CODES.load(Ordering::SeqCst);
    let k2 = KEY2_CODES.load(Ordering::SeqCst);
    let k3 = KEY3_CODE.load(Ordering::SeqCst);
    let g1 = vk_to_group(k1 >> 16);
    let g2 = vk_to_group(k2 >> 16);
    if k3 != 0 {
        format!("{} + {} + {}", group_to_label(g1), group_to_label(g2), vk_to_label(k3))
    } else {
        format!("{} + {}", group_to_label(g1), group_to_label(g2))
    }
}

/// Start recording mode — enables GetAsyncKeyState polling.
pub fn start_recording() {
    REC_GROUP1.store(0, Ordering::SeqCst);
    REC_GROUP2.store(0, Ordering::SeqCst);
    REC_KEY3.store(0, Ordering::SeqCst);
    REC_WAIT.store(0, Ordering::SeqCst);
    REC_DONE.store(false, Ordering::SeqCst);
    RECORDING.store(true, Ordering::SeqCst);
}

/// Poll modifier key states using GetAsyncKeyState.
/// Called from poll_shortcut_recording at ~100ms intervals.
/// Detects 2 modifiers (+ optional non-modifier key).
pub fn poll_recording_keys() {
    if !RECORDING.load(Ordering::SeqCst) || REC_DONE.load(Ordering::SeqCst) {
        return;
    }
    platform::poll_async_key_state();
}

/// Check if recording captured a combo. Returns Some((name, label)) or None.
pub fn take_recorded() -> Option<(String, String)> {
    if !REC_DONE.load(Ordering::SeqCst) {
        return None;
    }
    let g1 = REC_GROUP1.load(Ordering::SeqCst);
    let g2 = REC_GROUP2.load(Ordering::SeqCst);
    let k3 = REC_KEY3.load(Ordering::SeqCst);

    if g1 != 0 && g2 != 0 && g1 != g2 {
        // Valid combo — clear recording state
        RECORDING.store(false, Ordering::SeqCst);
        REC_DONE.store(false, Ordering::SeqCst);
        REC_WAIT.store(0, Ordering::SeqCst);

        if k3 != 0 {
            let name = format!("{}+{}+{}", group_to_name(g1), group_to_name(g2), vk_to_name(k3));
            let label = format!("{} + {} + {}", group_to_label(g1), group_to_label(g2), vk_to_label(k3));
            Some((name, label))
        } else {
            let name = format!("{}+{}", group_to_name(g1), group_to_name(g2));
            let label = format!("{} + {}", group_to_label(g1), group_to_label(g2));
            Some((name, label))
        }
    } else {
        // Invalid — reset but keep recording active
        REC_GROUP1.store(0, Ordering::SeqCst);
        REC_GROUP2.store(0, Ordering::SeqCst);
        REC_KEY3.store(0, Ordering::SeqCst);
        REC_WAIT.store(0, Ordering::SeqCst);
        REC_DONE.store(false, Ordering::SeqCst);
        None
    }
}

/// Cancel recording mode — fully resets all recording state.
pub fn stop_recording() {
    RECORDING.store(false, Ordering::SeqCst);
    REC_GROUP1.store(0, Ordering::SeqCst);
    REC_GROUP2.store(0, Ordering::SeqCst);
    REC_KEY3.store(0, Ordering::SeqCst);
    REC_WAIT.store(0, Ordering::SeqCst);
    REC_DONE.store(false, Ordering::SeqCst);
}

/// Returns true if currently in recording mode.
pub fn is_recording() -> bool {
    RECORDING.load(Ordering::SeqCst)
}

/// Take the latest hotkey event: 0=none, 1=pressed, 2=released.
pub fn take_event() -> u8 {
    HOTKEY_EVENT.swap(EVENT_NONE, Ordering::SeqCst)
}

/// Install the global keyboard hook.
pub fn install(log_fn: fn(&str)) {
    platform::install_hook(log_fn);
}

// ── Windows implementation ──────────────────────────────────────────

#[cfg(target_os = "windows")]
mod platform {
    use super::*;
    use std::sync::atomic::AtomicIsize;

    static HOOK_HANDLE: AtomicIsize = AtomicIsize::new(0);

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
        fn GetAsyncKeyState(vKey: i32) -> i16;
    }

    fn is_key_pressed(vk: u32) -> bool {
        unsafe { GetAsyncKeyState(vk as i32) & (0x8000u16 as i16) != 0 }
    }

    /// Scan for any pressed non-modifier key. Returns VK code or 0.
    fn scan_non_modifier_key() -> u32 {
        const SCAN: &[u32] = &[
            0x08, 0x09, 0x0D, 0x1B, 0x20,                         // Bksp Tab Enter Esc Space
            0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, // 0-9
            0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,       // A-Z
            0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x50,
            0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58,
            0x59, 0x5A,
            0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, // Numpad 0-9
            0x6A, 0x6B, 0x6D, 0x6E, 0x6F,                         // Numpad ops
            0x70, 0x71, 0x72, 0x73, 0x74, 0x75,                   // F1-F12
            0x76, 0x77, 0x78, 0x79, 0x7A, 0x7B,
        ];
        for &vk in SCAN {
            if is_key_pressed(vk) {
                return vk;
            }
        }
        0
    }

    /// Poll all modifier keys + non-modifier keys via GetAsyncKeyState.
    /// When 2+ mod groups pressed:
    ///   - If a non-mod key is also pressed → record 3-key combo immediately.
    ///   - Otherwise, wait 3 polls (~300ms) then confirm 2-mod combo.
    pub fn poll_async_key_state() {
        let mut pressed: [u8; 4] = [0; 4];
        let mut count = 0usize;

        if is_key_pressed(VK_LWIN) || is_key_pressed(VK_RWIN) {
            pressed[count] = GROUP_WIN; count += 1;
        }
        if is_key_pressed(VK_LMENU) || is_key_pressed(VK_RMENU) {
            pressed[count] = GROUP_ALT; count += 1;
        }
        if is_key_pressed(VK_LCONTROL) || is_key_pressed(VK_RCONTROL) {
            pressed[count] = GROUP_CTRL; count += 1;
        }
        if is_key_pressed(VK_LSHIFT) || is_key_pressed(VK_RSHIFT) {
            pressed[count] = GROUP_SHIFT; count += 1;
        }

        if count < 2 {
            // Not enough modifiers — reset wait
            REC_WAIT.store(0, Ordering::SeqCst);
            return;
        }

        // 2+ mods detected — check for non-modifier key
        let non_mod = scan_non_modifier_key();

        if non_mod != 0 {
            // 3-key combo — record immediately
            REC_GROUP1.store(pressed[0], Ordering::SeqCst);
            REC_GROUP2.store(pressed[1], Ordering::SeqCst);
            REC_KEY3.store(non_mod, Ordering::SeqCst);
            REC_DONE.store(true, Ordering::SeqCst);
            return;
        }

        // No non-mod key — countdown for 2-mod confirmation
        let wait = REC_WAIT.load(Ordering::SeqCst);
        if wait == 0 {
            // First detection — store groups and start countdown
            REC_GROUP1.store(pressed[0], Ordering::SeqCst);
            REC_GROUP2.store(pressed[1], Ordering::SeqCst);
            REC_KEY3.store(0, Ordering::SeqCst);
            REC_WAIT.store(3, Ordering::SeqCst);
        } else if wait == 1 {
            // Countdown done — confirm 2-mod combo
            REC_WAIT.store(0, Ordering::SeqCst);
            REC_DONE.store(true, Ordering::SeqCst);
        } else {
            REC_WAIT.store(wait - 1, Ordering::SeqCst);
        }
    }

    /// Keyboard hook callback — handles normal hotkey detection only.
    unsafe extern "system" fn keyboard_proc(code: i32, wparam: usize, lparam: isize) -> isize {
        if code >= 0 && lparam != 0 {
            let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };
            let vk = kb.vkCode;
            let is_down = wparam == WM_KEYDOWN || wparam == WM_SYSKEYDOWN;
            let is_up = wparam == WM_KEYUP || wparam == WM_SYSKEYUP;

            // Skip hotkey detection while recording a new shortcut
            if !RECORDING.load(Ordering::SeqCst) {
                let k1 = KEY1_CODES.load(Ordering::SeqCst);
                let k2 = KEY2_CODES.load(Ordering::SeqCst);
                let k3 = KEY3_CODE.load(Ordering::SeqCst);

                if k3 == 0 {
                    // ── 2-modifier combo ──
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
                } else {
                    // ── 2-modifier + 1-key combo ──
                    let mut changed = false;
                    if matches_key_group(vk, k1) {
                        if is_down { KEY1_DOWN.store(true, Ordering::SeqCst); }
                        else if is_up { KEY1_DOWN.store(false, Ordering::SeqCst); }
                        changed = true;
                    } else if matches_key_group(vk, k2) {
                        if is_down { KEY2_DOWN.store(true, Ordering::SeqCst); }
                        else if is_up { KEY2_DOWN.store(false, Ordering::SeqCst); }
                        changed = true;
                    } else if vk == k3 {
                        if is_down { KEY3_DOWN.store(true, Ordering::SeqCst); }
                        else if is_up { KEY3_DOWN.store(false, Ordering::SeqCst); }
                        changed = true;
                    }

                    if changed {
                        let all = KEY1_DOWN.load(Ordering::SeqCst)
                            && KEY2_DOWN.load(Ordering::SeqCst)
                            && KEY3_DOWN.load(Ordering::SeqCst);
                        if all && !COMBO_ACTIVE.swap(true, Ordering::SeqCst) {
                            HOTKEY_EVENT.store(EVENT_PRESSED, Ordering::SeqCst);
                        } else if !all && COMBO_ACTIVE.swap(false, Ordering::SeqCst) {
                            HOTKEY_EVENT.store(EVENT_RELEASED, Ordering::SeqCst);
                        }
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
    pub fn install_hook(_log_fn: fn(&str)) {}
    pub fn poll_async_key_state() {}
}
