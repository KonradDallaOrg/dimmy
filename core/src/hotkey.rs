//! Global hotkey via low-level keyboard hook.
//!
//! Supports two shortcut formats:
//!   - 2 modifiers: e.g. Win+Alt, Ctrl+Shift
//!   - 2 modifiers + 1 key: e.g. Ctrl+Shift+Space, Win+Alt+N
//!
//! Keys are configured at runtime via `set_shortcut()`.
//! Recording mode uses GetAsyncKeyState polling to capture the combo.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU8, Ordering};

const EVENT_NONE: u8 = 0;
const EVENT_PRESSED: u8 = 1;
const EVENT_RELEASED: u8 = 2;

static KEY1_DOWN: AtomicBool = AtomicBool::new(false);
static KEY2_DOWN: AtomicBool = AtomicBool::new(false);
static KEY3_DOWN: AtomicBool = AtomicBool::new(false);
static COMBO_ACTIVE: AtomicBool = AtomicBool::new(false);
static HOTKEY_EVENT: AtomicU8 = AtomicU8::new(EVENT_NONE);

/// When `true`, the LL keyboard hook consumes (returns 1 for) every event
/// matching the configured shortcut keys, instead of forwarding to the OS
/// via `CallNextHookEx`. Flipped to `true` the instant the combo is fully
/// pressed; cleared back to `false` only when every shortcut key is
/// physically released. While true, the hook also flushes the kernel's
/// modifier state by injecting synthetic UPs for the combo keys (since
/// the first modifier already passed through to the kernel before we
/// knew it was a combo). Net effect: during a dictation hold the OS
/// shell + focused app never see orphan Win/Alt up events, so they can't
/// open Start Menu, activate Alt-menu-mode in Notepad++, or otherwise
/// hijack the input stream that our subsequent synthetic Ctrl+V would
/// then collide with. Event-driven, no timing.
static MODIFIER_SUPPRESS: AtomicBool = AtomicBool::new(false);

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
        GROUP_WIN => {
            if cfg!(target_os = "macos") {
                "cmd"
            } else {
                "win"
            }
        }
        GROUP_ALT => {
            if cfg!(target_os = "macos") {
                "option"
            } else {
                "alt"
            }
        }
        GROUP_CTRL => "ctrl",
        GROUP_SHIFT => "shift",
        _ => "?",
    }
}

fn group_to_label(group: u8) -> &'static str {
    match group {
        GROUP_WIN => {
            if cfg!(target_os = "macos") {
                "Cmd"
            } else {
                "Win"
            }
        }
        GROUP_ALT => {
            if cfg!(target_os = "macos") {
                "Option"
            } else {
                "Alt"
            }
        }
        GROUP_CTRL => "Ctrl",
        GROUP_SHIFT => "Shift",
        _ => "?",
    }
}

fn name_to_group(name: &str) -> u8 {
    match name {
        "win" | "cmd" => GROUP_WIN,
        "alt" | "option" => GROUP_ALT,
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
                if c.is_ascii_lowercase() {
                    return (c - b'a' + 0x41) as u32;
                }
                if c.is_ascii_digit() {
                    return c as u32;
                }
            }
            // f1-f12
            if let Some(suffix) = name.strip_prefix('f') {
                if let Ok(n) = suffix.parse::<u32>() {
                    if (1..=12).contains(&n) {
                        return 0x6F + n;
                    }
                }
            }
            // num0-num9
            if name.starts_with("num") && name.len() == 4 {
                let d = name.as_bytes()[3];
                if d.is_ascii_digit() {
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

/// Set the shortcut combo from a string like "Win+Alt", "Alt+X", or "Ctrl+Shift+Space".
///
/// Supported formats (case insensitive):
/// - 2 modifiers: "Win+Alt", "Ctrl+Shift"
/// - 1 modifier + 1 key: "Alt+X", "Ctrl+Space"
/// - 2 modifiers + 1 key: "Ctrl+Shift+X", "Win+Alt+N"
pub fn set_shortcut(combo: &str) {
    assert!(!combo.is_empty(), "shortcut combo must not be empty");
    assert!(
        combo.contains('+'),
        "shortcut must contain '+' separator: {}",
        combo
    );

    // Normalize to lowercase for matching
    let lower = combo.to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('+').map(|s| s.trim()).collect();

    // Classify each part as modifier group or VK key
    let mut groups: Vec<u8> = Vec::new();
    let mut vk: u32 = 0;
    for part in &parts {
        let g = name_to_group(part);
        if g != 0 {
            groups.push(g);
        } else {
            let k = name_to_vk(part);
            if k != 0 {
                vk = k;
            }
        }
    }

    if groups.len() == 2 {
        // 2 modifiers (+ optional key)
        KEY1_CODES.store(group_to_packed(groups[0]), Ordering::SeqCst);
        KEY2_CODES.store(group_to_packed(groups[1]), Ordering::SeqCst);
        KEY3_CODE.store(vk, Ordering::SeqCst);
        KEY3_DOWN.store(false, Ordering::SeqCst);
        return;
    }

    if groups.len() == 1 && vk != 0 {
        // 1 modifier + 1 key: use the modifier as both KEY1 and KEY2
        // so the hook fires when this single modifier + key are both down.
        // We store the modifier in KEY1, VK=0 in KEY2 (unused), and the key in KEY3.
        KEY1_CODES.store(group_to_packed(groups[0]), Ordering::SeqCst);
        KEY2_CODES.store(0, Ordering::SeqCst);
        KEY3_CODE.store(vk, Ordering::SeqCst);
        KEY3_DOWN.store(false, Ordering::SeqCst);
        return;
    }

    // Fallback: cmd+option+D on macOS, win+alt on Windows/Linux
    KEY1_CODES.store(group_to_packed(GROUP_WIN), Ordering::SeqCst);
    KEY2_CODES.store(group_to_packed(GROUP_ALT), Ordering::SeqCst);
    #[cfg(target_os = "macos")]
    {
        KEY3_CODE.store(name_to_vk("d"), Ordering::SeqCst);
    }
    #[cfg(not(target_os = "macos"))]
    {
        KEY3_CODE.store(0, Ordering::SeqCst);
    }
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
        format!(
            "{} + {} + {}",
            group_to_label(g1),
            group_to_label(g2),
            vk_to_label(k3)
        )
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
            let name = format!(
                "{}+{}+{}",
                group_to_name(g1),
                group_to_name(g2),
                vk_to_name(k3)
            );
            let label = format!(
                "{} + {} + {}",
                group_to_label(g1),
                group_to_label(g2),
                vk_to_label(k3)
            );
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

    #[allow(non_snake_case, clippy::upper_case_acronyms)]
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
    #[allow(clippy::upper_case_acronyms)]
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
        fn GetMessageW(lpMsg: *mut MSG, hWnd: isize, wMsgFilterMin: u32, wMsgFilterMax: u32)
            -> i32;
        fn GetAsyncKeyState(vKey: i32) -> i16;
        fn SendInput(cInputs: u32, pInputs: *const INPUT, cbSize: i32) -> u32;
    }

    /// `LLKHF_INJECTED` (`0x10`) — set on any keyboard event injected by
    /// `SendInput` (ours or another process). The hook recognises its own
    /// synthetic UP burst by this bit and passes it straight through
    /// without re-running combo state machine logic. Without this guard
    /// the suppression flag would flap on every injected event.
    const LLKHF_INJECTED: u32 = 0x10;
    const INPUT_KEYBOARD: u32 = 1;
    const KEYEVENTF_KEYUP: u32 = 0x0002;
    /// `VK_NONAME` (`0xFC`) — Windows VK code reserved by Microsoft, no
    /// shipping app reacts to it. We inject a down+up burst of it as a
    /// "chord-buster": the Windows shell only treats NON-MODIFIER key
    /// input as the chord that prevents Start Menu opening on Win
    /// release. Other modifiers (Alt, Ctrl, Shift) don't count — that's
    /// why Win+E suppresses Start Menu but Win+Alt doesn't (empirically
    /// confirmed 2026-05-18: `f69d3db` let Alt down reach the shell and
    /// Start Menu still opened on the subsequent synthetic Win UP).
    /// AutoHotkey + Talon Voice use the same VK_NONAME trick for the
    /// same reason.
    const VK_NONAME: u16 = 0xFC;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    #[allow(non_snake_case, clippy::upper_case_acronyms)]
    struct KEYBDINPUT {
        wVk: u16,
        wScan: u16,
        dwFlags: u32,
        time: u32,
        dwExtraInfo: usize,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    #[allow(non_snake_case, clippy::upper_case_acronyms)]
    struct MOUSEINPUT {
        dx: i32,
        dy: i32,
        mouseData: u32,
        dwFlags: u32,
        time: u32,
        dwExtraInfo: usize,
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    #[allow(non_snake_case, clippy::upper_case_acronyms)]
    struct HARDWAREINPUT {
        uMsg: u32,
        wParamL: u16,
        wParamH: u16,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    #[allow(non_snake_case, dead_code)]
    union INPUT_U {
        mi: MOUSEINPUT,
        ki: KEYBDINPUT,
        hi: HARDWAREINPUT,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    #[allow(non_snake_case, clippy::upper_case_acronyms)]
    struct INPUT {
        r#type: u32,
        u: INPUT_U,
    }

    /// Inject a single KEYUP for the given virtual-key, marked INJECTED so
    /// our own hook recognises and ignores it on its way through the chain.
    /// Used to flush the kernel's modifier state right after we decide a
    /// shortcut chord is active — the first modifier reached the OS before
    /// we knew it was part of a combo, so without this flush the OS would
    /// still consider it held throughout the dictation.
    fn emit_synthetic_keyup(vk: u16) {
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            u: INPUT_U {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
        }
    }

    /// Inject a single KEYDOWN for the given virtual-key, marked INJECTED
    /// so our own hook recognises and ignores it. Paired with
    /// `emit_synthetic_keyup` for the VK_NONAME chord-buster.
    fn emit_synthetic_keydown(vk: u16) {
        let input = INPUT {
            r#type: INPUT_KEYBOARD,
            u: INPUT_U {
                ki: KEYBDINPUT {
                    wVk: vk,
                    wScan: 0,
                    dwFlags: 0,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };
        unsafe {
            SendInput(1, &input, std::mem::size_of::<INPUT>() as i32);
        }
    }

    /// Synthetic UP burst for every shortcut key — both sides of each
    /// modifier group (LWin + RWin, LMENU + RMENU, …) so the burst is
    /// idempotent regardless of which side the user actually pressed.
    /// Spurious UPs for keys the kernel doesn't track as down are no-ops.
    ///
    /// When the combo includes the WIN modifier group, the burst is
    /// prefixed by a VK_NONAME down+up "chord-buster" so that the
    /// Windows shell classifies the upcoming synthetic Win UP as
    /// "released-after-chord" instead of "solo Win press-release". See
    /// VK_NONAME doc comment for the full incident note.
    fn emit_synthetic_combo_release() {
        let k1 = KEY1_CODES.load(Ordering::SeqCst);
        let k2 = KEY2_CODES.load(Ordering::SeqCst);
        let k3 = KEY3_CODE.load(Ordering::SeqCst);

        let win_packed = pack_keys(VK_LWIN, VK_RWIN);
        if k1 == win_packed || k2 == win_packed {
            emit_synthetic_keydown(VK_NONAME);
            emit_synthetic_keyup(VK_NONAME);
        }

        for packed in [k1, k2] {
            let left = packed >> 16;
            let right = packed & 0xFFFF;
            if left != 0 {
                emit_synthetic_keyup(left as u16);
            }
            if right != 0 {
                emit_synthetic_keyup(right as u16);
            }
        }
        if k3 != 0 {
            emit_synthetic_keyup(k3 as u16);
        }
    }

    fn is_key_pressed(vk: u32) -> bool {
        unsafe { GetAsyncKeyState(vk as i32) & (0x8000u16 as i16) != 0 }
    }

    /// Scan for any pressed non-modifier key. Returns VK code or 0.
    fn scan_non_modifier_key() -> u32 {
        const SCAN: &[u32] = &[
            0x08, 0x09, 0x0D, 0x1B, 0x20, // Bksp Tab Enter Esc Space
            0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, // 0-9
            0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, // A-Z
            0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56,
            0x57, 0x58, 0x59, 0x5A, 0x60, 0x61, 0x62, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
            0x69, // Numpad 0-9
            0x6A, 0x6B, 0x6D, 0x6E, 0x6F, // Numpad ops
            0x70, 0x71, 0x72, 0x73, 0x74, 0x75, // F1-F12
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
            pressed[count] = GROUP_WIN;
            count += 1;
        }
        if is_key_pressed(VK_LMENU) || is_key_pressed(VK_RMENU) {
            pressed[count] = GROUP_ALT;
            count += 1;
        }
        if is_key_pressed(VK_LCONTROL) || is_key_pressed(VK_RCONTROL) {
            pressed[count] = GROUP_CTRL;
            count += 1;
        }
        if is_key_pressed(VK_LSHIFT) || is_key_pressed(VK_RSHIFT) {
            pressed[count] = GROUP_SHIFT;
            count += 1;
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

    /// Keyboard hook callback — combo detection + modifier suppression.
    ///
    /// Suppression contract (the fix landed 2026-05-18, refined twice same evening):
    /// - When the configured combo (KEY1+KEY2[+KEY3]) becomes fully pressed,
    ///   the hook (a) emits a synthetic UP for every shortcut key to flush
    ///   the kernel's modifier state — preceded by a VK_NONAME down+up
    ///   "chord-buster" if WIN is one of the modifiers, so the shell
    ///   classifies the upcoming Win UP as released-after-chord (Alt does
    ///   not count as chord input for the shell's Start-Menu logic — only
    ///   non-modifier keys do). Then (b) sets `MODIFIER_SUPPRESS=true`.
    /// - While `MODIFIER_SUPPRESS=true`, every event matching a
    ///   shortcut key is consumed (return 1) instead of forwarded. The OS
    ///   shell therefore never sees an orphan Win-alone-release (no Start
    ///   Menu) or Alt-alone-release (no Notepad++ menu-mode activation).
    /// - The flag clears only when every shortcut key has been physically
    ///   released, so the second key's UP is suppressed too.
    /// - Injected events (LLKHF_INJECTED set, i.e. our own synthetic UPs
    ///   or another app's SendInput) bypass the state machine entirely.
    /// - Suppression is SCOPED to the configured shortcut keys — pressing
    ///   any other Win-prefixed shortcut (Win+E, Win+L, Win+Tab, …)
    ///   continues to reach the OS as normal because COMBO_ACTIVE never
    ///   activates without both configured keys held.
    unsafe extern "system" fn keyboard_proc(code: i32, wparam: usize, lparam: isize) -> isize {
        if code < 0 || lparam == 0 {
            return unsafe {
                CallNextHookEx(HOOK_HANDLE.load(Ordering::SeqCst), code, wparam, lparam)
            };
        }
        let kb = unsafe { &*(lparam as *const KBDLLHOOKSTRUCT) };

        // Skip injected events (including our own synthetic UPs) so the
        // suppression flag doesn't flap and KEY*_DOWN reflects only
        // physical key state.
        if kb.flags & LLKHF_INJECTED != 0 {
            return unsafe {
                CallNextHookEx(HOOK_HANDLE.load(Ordering::SeqCst), code, wparam, lparam)
            };
        }

        let vk = kb.vkCode;
        let is_down = wparam == WM_KEYDOWN || wparam == WM_SYSKEYDOWN;
        let is_up = wparam == WM_KEYUP || wparam == WM_SYSKEYUP;

        // Skip hotkey detection while recording a new shortcut. Suppression
        // also stays off — the user needs to actually press combos to
        // configure them.
        if RECORDING.load(Ordering::SeqCst) {
            return unsafe {
                CallNextHookEx(HOOK_HANDLE.load(Ordering::SeqCst), code, wparam, lparam)
            };
        }

        let k1 = KEY1_CODES.load(Ordering::SeqCst);
        let k2 = KEY2_CODES.load(Ordering::SeqCst);
        let k3 = KEY3_CODE.load(Ordering::SeqCst);

        let is_combo_key =
            matches_key_group(vk, k1) || matches_key_group(vk, k2) || (k3 != 0 && vk == k3);

        if k3 == 0 {
            // ── 2-modifier combo ──
            if matches_key_group(vk, k1) {
                if is_down {
                    KEY1_DOWN.store(true, Ordering::SeqCst);
                    if KEY2_DOWN.load(Ordering::SeqCst)
                        && !COMBO_ACTIVE.swap(true, Ordering::SeqCst)
                    {
                        HOTKEY_EVENT.store(EVENT_PRESSED, Ordering::SeqCst);
                        MODIFIER_SUPPRESS.store(true, Ordering::SeqCst);
                        emit_synthetic_combo_release();
                    }
                } else if is_up {
                    KEY1_DOWN.store(false, Ordering::SeqCst);
                    if COMBO_ACTIVE.swap(false, Ordering::SeqCst) {
                        HOTKEY_EVENT.store(EVENT_RELEASED, Ordering::SeqCst);
                    }
                    if !KEY1_DOWN.load(Ordering::SeqCst) && !KEY2_DOWN.load(Ordering::SeqCst) {
                        MODIFIER_SUPPRESS.store(false, Ordering::SeqCst);
                    }
                }
            } else if matches_key_group(vk, k2) {
                if is_down {
                    KEY2_DOWN.store(true, Ordering::SeqCst);
                    if KEY1_DOWN.load(Ordering::SeqCst)
                        && !COMBO_ACTIVE.swap(true, Ordering::SeqCst)
                    {
                        HOTKEY_EVENT.store(EVENT_PRESSED, Ordering::SeqCst);
                        MODIFIER_SUPPRESS.store(true, Ordering::SeqCst);
                        emit_synthetic_combo_release();
                    }
                } else if is_up {
                    KEY2_DOWN.store(false, Ordering::SeqCst);
                    if COMBO_ACTIVE.swap(false, Ordering::SeqCst) {
                        HOTKEY_EVENT.store(EVENT_RELEASED, Ordering::SeqCst);
                    }
                    if !KEY1_DOWN.load(Ordering::SeqCst) && !KEY2_DOWN.load(Ordering::SeqCst) {
                        MODIFIER_SUPPRESS.store(false, Ordering::SeqCst);
                    }
                }
            }
        } else {
            // ── 2-modifier + 1-key combo ──
            let mut changed = false;
            if matches_key_group(vk, k1) {
                if is_down {
                    KEY1_DOWN.store(true, Ordering::SeqCst);
                } else if is_up {
                    KEY1_DOWN.store(false, Ordering::SeqCst);
                }
                changed = true;
            } else if matches_key_group(vk, k2) {
                if is_down {
                    KEY2_DOWN.store(true, Ordering::SeqCst);
                } else if is_up {
                    KEY2_DOWN.store(false, Ordering::SeqCst);
                }
                changed = true;
            } else if vk == k3 {
                if is_down {
                    KEY3_DOWN.store(true, Ordering::SeqCst);
                } else if is_up {
                    KEY3_DOWN.store(false, Ordering::SeqCst);
                }
                changed = true;
            }

            if changed {
                let all = KEY1_DOWN.load(Ordering::SeqCst)
                    && (k2 == 0 || KEY2_DOWN.load(Ordering::SeqCst))
                    && KEY3_DOWN.load(Ordering::SeqCst);
                if all && !COMBO_ACTIVE.swap(true, Ordering::SeqCst) {
                    HOTKEY_EVENT.store(EVENT_PRESSED, Ordering::SeqCst);
                    MODIFIER_SUPPRESS.store(true, Ordering::SeqCst);
                    emit_synthetic_combo_release();
                } else if !all && COMBO_ACTIVE.swap(false, Ordering::SeqCst) {
                    HOTKEY_EVENT.store(EVENT_RELEASED, Ordering::SeqCst);
                }
                if !KEY1_DOWN.load(Ordering::SeqCst)
                    && !KEY2_DOWN.load(Ordering::SeqCst)
                    && !KEY3_DOWN.load(Ordering::SeqCst)
                {
                    MODIFIER_SUPPRESS.store(false, Ordering::SeqCst);
                }
            }
        }

        if is_combo_key && MODIFIER_SUPPRESS.load(Ordering::SeqCst) {
            return 1;
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

// ── macOS implementation ─────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod platform {
    use super::*;

    // macOS keycodes for modifier keys
    const KC_LEFT_CMD: u16 = 0x37;
    const KC_RIGHT_CMD: u16 = 0x36;
    const KC_LEFT_OPTION: u16 = 0x3A;
    const KC_RIGHT_OPTION: u16 = 0x3D;
    const KC_LEFT_CTRL: u16 = 0x3B;
    const KC_RIGHT_CTRL: u16 = 0x3E;
    const KC_LEFT_SHIFT: u16 = 0x38;
    const KC_RIGHT_SHIFT: u16 = 0x3C;

    fn keycode_to_group(kc: u16) -> u8 {
        match kc {
            KC_LEFT_CMD | KC_RIGHT_CMD => GROUP_WIN,
            KC_LEFT_OPTION | KC_RIGHT_OPTION => GROUP_ALT,
            KC_LEFT_CTRL | KC_RIGHT_CTRL => GROUP_CTRL,
            KC_LEFT_SHIFT | KC_RIGHT_SHIFT => GROUP_SHIFT,
            _ => 0,
        }
    }

    /// Map macOS keycode to the Windows-compatible VK code used by the shared logic.
    /// Modifier keys use the platform VK constants; non-modifier keys are mapped
    /// to the same codes that vk_to_name/vk_to_label/name_to_vk understand.
    fn keycode_to_vk(kc: u16) -> u32 {
        match kc {
            // Modifiers — mapped to Windows VK codes so matches_key_group works
            KC_LEFT_CMD => VK_LWIN,
            KC_RIGHT_CMD => VK_RWIN,
            KC_LEFT_OPTION => VK_LMENU,
            KC_RIGHT_OPTION => VK_RMENU,
            KC_LEFT_CTRL => VK_LCONTROL,
            KC_RIGHT_CTRL => VK_RCONTROL,
            KC_LEFT_SHIFT => VK_LSHIFT,
            KC_RIGHT_SHIFT => VK_RSHIFT,
            // Letters (macOS 0x00-0x1F map to ANSI layout)
            0x00 => 0x41, // A
            0x0B => 0x42, // B
            0x08 => 0x43, // C
            0x02 => 0x44, // D
            0x0E => 0x45, // E
            0x03 => 0x46, // F
            0x05 => 0x47, // G
            0x04 => 0x48, // H
            0x22 => 0x49, // I
            0x26 => 0x4A, // J
            0x28 => 0x4B, // K
            0x25 => 0x4C, // L
            0x2E => 0x4D, // M
            0x2D => 0x4E, // N
            0x1F => 0x4F, // O
            0x23 => 0x50, // P
            0x0C => 0x51, // Q
            0x0F => 0x52, // R
            0x01 => 0x53, // S
            0x11 => 0x54, // T
            0x20 => 0x55, // U
            0x09 => 0x56, // V
            0x0D => 0x57, // W
            0x07 => 0x58, // X
            0x10 => 0x59, // Y
            0x06 => 0x5A, // Z
            // Digits
            0x12 => 0x31, // 1
            0x13 => 0x32, // 2
            0x14 => 0x33, // 3
            0x15 => 0x34, // 4
            0x17 => 0x35, // 5
            0x16 => 0x36, // 6
            0x1A => 0x37, // 7
            0x1C => 0x38, // 8
            0x19 => 0x39, // 9
            0x1D => 0x30, // 0
            // Function keys
            0x7A => 0x70, // F1
            0x78 => 0x71, // F2
            0x63 => 0x72, // F3
            0x76 => 0x73, // F4
            0x60 => 0x74, // F5
            0x61 => 0x75, // F6
            0x62 => 0x76, // F7
            0x64 => 0x77, // F8
            0x65 => 0x78, // F9
            0x6D => 0x79, // F10
            0x67 => 0x7A, // F11
            0x6F => 0x7B, // F12
            // Special keys
            0x33 => 0x08, // Backspace (Delete)
            0x30 => 0x09, // Tab
            0x24 => 0x0D, // Return/Enter
            0x35 => 0x1B, // Escape
            0x31 => 0x20, // Space
            // Numpad
            0x52 => 0x60, // Numpad 0
            0x53 => 0x61, // Numpad 1
            0x54 => 0x62, // Numpad 2
            0x55 => 0x63, // Numpad 3
            0x56 => 0x64, // Numpad 4
            0x57 => 0x65, // Numpad 5
            0x58 => 0x66, // Numpad 6
            0x59 => 0x67, // Numpad 7
            0x5B => 0x68, // Numpad 8
            0x5C => 0x69, // Numpad 9
            0x43 => 0x6A, // Numpad *
            0x45 => 0x6B, // Numpad +
            0x4E => 0x6D, // Numpad -
            0x41 => 0x6E, // Numpad .
            0x4B => 0x6F, // Numpad /
            _ => 0,
        }
    }

    // ── CoreGraphics / CoreFoundation FFI ────────────────────────────

    type CGEventTapLocation = u32;
    type CGEventTapPlacement = u32;
    type CGEventTapOptions = u32;
    type CGEventMask = u64;
    type CGEventType = u32;
    type CGEventField = u32;
    type CGEventFlags = u64;
    type CGEventSourceStateID = u32;

    const K_CG_SESSION_EVENT_TAP: CGEventTapLocation = 1;
    const K_CG_HEAD_INSERT_EVENT_TAP: CGEventTapPlacement = 0;
    const K_CG_EVENT_TAP_OPTION_LISTEN_ONLY: CGEventTapOptions = 1;

    const K_CG_EVENT_KEY_DOWN: CGEventType = 10;
    const K_CG_EVENT_KEY_UP: CGEventType = 11;
    const K_CG_EVENT_FLAGS_CHANGED: CGEventType = 12;

    const K_CG_KEYBOARD_EVENT_KEYCODE: CGEventField = 9;

    const K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION: CGEventSourceStateID = 0;

    const K_CG_EVENT_FLAG_MASK_COMMAND: CGEventFlags = 0x00100000;
    const K_CG_EVENT_FLAG_MASK_SHIFT: CGEventFlags = 0x00020000;
    const K_CG_EVENT_FLAG_MASK_ALTERNATE: CGEventFlags = 0x00080000;
    const K_CG_EVENT_FLAG_MASK_CONTROL: CGEventFlags = 0x00040000;

    // Opaque types
    #[repr(C)]
    struct __CGEvent {
        _private: [u8; 0],
    }
    type CGEventRef = *mut __CGEvent;

    #[repr(C)]
    struct __CFMachPort {
        _private: [u8; 0],
    }
    type CFMachPortRef = *mut __CFMachPort;

    #[repr(C)]
    struct __CFRunLoopSource {
        _private: [u8; 0],
    }
    type CFRunLoopSourceRef = *mut __CFRunLoopSource;

    #[repr(C)]
    struct __CFRunLoop {
        _private: [u8; 0],
    }
    type CFRunLoopRef = *mut __CFRunLoop;

    type CFIndex = isize;
    type CFAllocatorRef = *const std::ffi::c_void;
    type CFStringRef = *const std::ffi::c_void;
    type CFRunLoopMode = CFStringRef;

    type CGEventTapCallBack = unsafe extern "C" fn(
        proxy: *const std::ffi::c_void,
        event_type: CGEventType,
        event: CGEventRef,
        user_info: *mut std::ffi::c_void,
    ) -> CGEventRef;

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGEventTapCreate(
            tap: CGEventTapLocation,
            place: CGEventTapPlacement,
            options: CGEventTapOptions,
            events_of_interest: CGEventMask,
            callback: CGEventTapCallBack,
            user_info: *mut std::ffi::c_void,
        ) -> CFMachPortRef;

        fn CGEventGetIntegerValueField(event: CGEventRef, field: CGEventField) -> i64;

        fn CGEventSourceFlagsState(state_id: CGEventSourceStateID) -> CGEventFlags;

        fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFMachPortCreateRunLoopSource(
            allocator: CFAllocatorRef,
            port: CFMachPortRef,
            order: CFIndex,
        ) -> CFRunLoopSourceRef;

        fn CFRunLoopGetCurrent() -> CFRunLoopRef;

        fn CFRunLoopAddSource(rl: CFRunLoopRef, source: CFRunLoopSourceRef, mode: CFRunLoopMode);

        fn CFRunLoopRun();

        static kCFRunLoopCommonModes: CFRunLoopMode;
    }

    fn event_mask(ty: CGEventType) -> CGEventMask {
        1u64 << (ty as u64)
    }

    // Track the tap for re-enabling if macOS disables it
    static TAP_PORT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    unsafe extern "C" fn tap_callback(
        _proxy: *const std::ffi::c_void,
        event_type: CGEventType,
        event: CGEventRef,
        _user_info: *mut std::ffi::c_void,
    ) -> CGEventRef {
        // macOS may send a special event when the tap is disabled; re-enable it
        if event_type == 0xFFFFFFFF {
            // kCGEventTapDisabledByTimeout or kCGEventTapDisabledByUserInput
            let port_addr = TAP_PORT.load(Ordering::SeqCst);
            if port_addr != 0 {
                CGEventTapEnable(port_addr as CFMachPortRef, true);
            }
            return event;
        }

        let keycode =
            unsafe { CGEventGetIntegerValueField(event, K_CG_KEYBOARD_EVENT_KEYCODE) } as u16;

        // Determine key-down / key-up
        // For kCGEventFlagsChanged we look at the current modifier flags to decide
        let (is_down, is_up) = match event_type {
            K_CG_EVENT_KEY_DOWN => (true, false),
            K_CG_EVENT_KEY_UP => (false, true),
            K_CG_EVENT_FLAGS_CHANGED => {
                // For modifier keys, flagsChanged fires on both press and release.
                // Check the current flags to determine direction.
                let flags =
                    unsafe { CGEventSourceFlagsState(K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION) };
                let group = keycode_to_group(keycode);
                let is_pressed = match group {
                    GROUP_WIN => flags & K_CG_EVENT_FLAG_MASK_COMMAND != 0,
                    GROUP_ALT => flags & K_CG_EVENT_FLAG_MASK_ALTERNATE != 0,
                    GROUP_CTRL => flags & K_CG_EVENT_FLAG_MASK_CONTROL != 0,
                    GROUP_SHIFT => flags & K_CG_EVENT_FLAG_MASK_SHIFT != 0,
                    _ => return event,
                };
                (is_pressed, !is_pressed)
            }
            _ => return event,
        };

        let vk = keycode_to_vk(keycode);
        if vk == 0 {
            return event;
        }

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
                    if is_down {
                        KEY1_DOWN.store(true, Ordering::SeqCst);
                    } else if is_up {
                        KEY1_DOWN.store(false, Ordering::SeqCst);
                    }
                    changed = true;
                } else if matches_key_group(vk, k2) {
                    if is_down {
                        KEY2_DOWN.store(true, Ordering::SeqCst);
                    } else if is_up {
                        KEY2_DOWN.store(false, Ordering::SeqCst);
                    }
                    changed = true;
                } else if vk == k3 {
                    if is_down {
                        KEY3_DOWN.store(true, Ordering::SeqCst);
                    } else if is_up {
                        KEY3_DOWN.store(false, Ordering::SeqCst);
                    }
                    changed = true;
                }

                if changed {
                    let all = KEY1_DOWN.load(Ordering::SeqCst)
                        && (k2 == 0 || KEY2_DOWN.load(Ordering::SeqCst))
                        && KEY3_DOWN.load(Ordering::SeqCst);
                    if all && !COMBO_ACTIVE.swap(true, Ordering::SeqCst) {
                        HOTKEY_EVENT.store(EVENT_PRESSED, Ordering::SeqCst);
                    } else if !all && COMBO_ACTIVE.swap(false, Ordering::SeqCst) {
                        HOTKEY_EVENT.store(EVENT_RELEASED, Ordering::SeqCst);
                    }
                }
            }
        }

        event
    }

    /// Poll modifier key state using CGEventSourceFlagsState (for recording mode).
    pub fn poll_async_key_state() {
        let flags = unsafe { CGEventSourceFlagsState(K_CG_EVENT_SOURCE_STATE_COMBINED_SESSION) };

        let mut pressed: [u8; 4] = [0; 4];
        let mut count = 0usize;

        if flags & K_CG_EVENT_FLAG_MASK_COMMAND != 0 {
            pressed[count] = GROUP_WIN;
            count += 1;
        }
        if flags & K_CG_EVENT_FLAG_MASK_ALTERNATE != 0 {
            pressed[count] = GROUP_ALT;
            count += 1;
        }
        if flags & K_CG_EVENT_FLAG_MASK_CONTROL != 0 {
            pressed[count] = GROUP_CTRL;
            count += 1;
        }
        if flags & K_CG_EVENT_FLAG_MASK_SHIFT != 0 {
            pressed[count] = GROUP_SHIFT;
            count += 1;
        }

        if count < 2 {
            REC_WAIT.store(0, Ordering::SeqCst);
            return;
        }

        // On macOS we cannot scan non-modifier keys via flags alone, so
        // we only support 2-modifier combos in recording mode (matching
        // typical macOS usage patterns like Cmd+Option).
        let wait = REC_WAIT.load(Ordering::SeqCst);
        if wait == 0 {
            REC_GROUP1.store(pressed[0], Ordering::SeqCst);
            REC_GROUP2.store(pressed[1], Ordering::SeqCst);
            REC_KEY3.store(0, Ordering::SeqCst);
            REC_WAIT.store(3, Ordering::SeqCst);
        } else if wait == 1 {
            REC_WAIT.store(0, Ordering::SeqCst);
            REC_DONE.store(true, Ordering::SeqCst);
        } else {
            REC_WAIT.store(wait - 1, Ordering::SeqCst);
        }
    }

    pub fn install_hook(log_fn: fn(&str)) {
        std::thread::spawn(move || unsafe {
            let mask = event_mask(K_CG_EVENT_KEY_DOWN)
                | event_mask(K_CG_EVENT_KEY_UP)
                | event_mask(K_CG_EVENT_FLAGS_CHANGED);

            let tap = CGEventTapCreate(
                K_CG_SESSION_EVENT_TAP,
                K_CG_HEAD_INSERT_EVENT_TAP,
                K_CG_EVENT_TAP_OPTION_LISTEN_ONLY,
                mask,
                tap_callback,
                std::ptr::null_mut(),
            );

            if tap.is_null() {
                log_fn("ERROR: Failed to create CGEventTap — check Accessibility permissions");
                return;
            }

            TAP_PORT.store(tap as usize, Ordering::SeqCst);

            let source = CFMachPortCreateRunLoopSource(std::ptr::null(), tap, 0);
            if source.is_null() {
                log_fn("ERROR: Failed to create run loop source for CGEventTap");
                return;
            }

            let run_loop = CFRunLoopGetCurrent();
            CFRunLoopAddSource(run_loop, source, kCFRunLoopCommonModes);
            CGEventTapEnable(tap, true);

            log_fn("CGEventTap installed — shortcut active");

            CFRunLoopRun();
        });
    }
}

// ── Linux stub ──────────────────────────────────────────────────────

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod platform {
    pub fn install_hook(_log_fn: fn(&str)) {}
    pub fn poll_async_key_state() {}
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Helper: read back the stored key config after set_shortcut.
    fn stored_keys() -> (u32, u32, u32) {
        (
            KEY1_CODES.load(Ordering::SeqCst),
            KEY2_CODES.load(Ordering::SeqCst),
            KEY3_CODE.load(Ordering::SeqCst),
        )
    }

    #[test]
    fn set_shortcut_two_modifiers_lowercase() {
        set_shortcut("win+alt");
        let (k1, k2, k3) = stored_keys();
        assert_eq!(k1, group_to_packed(GROUP_WIN));
        assert_eq!(k2, group_to_packed(GROUP_ALT));
        assert_eq!(k3, 0, "2-mod combo should have no VK key");
    }

    #[test]
    fn set_shortcut_two_modifiers_mixed_case() {
        set_shortcut("Win+Alt");
        let (k1, k2, k3) = stored_keys();
        assert_eq!(k1, group_to_packed(GROUP_WIN));
        assert_eq!(k2, group_to_packed(GROUP_ALT));
        assert_eq!(k3, 0);
    }

    #[test]
    fn set_shortcut_two_modifiers_uppercase() {
        set_shortcut("CTRL+SHIFT");
        let (k1, k2, k3) = stored_keys();
        assert_eq!(k1, group_to_packed(GROUP_CTRL));
        assert_eq!(k2, group_to_packed(GROUP_SHIFT));
        assert_eq!(k3, 0);
    }

    #[test]
    fn set_shortcut_one_mod_one_key() {
        set_shortcut("Alt+X");
        let (k1, k2, k3) = stored_keys();
        assert_eq!(k1, group_to_packed(GROUP_ALT), "modifier should be Alt");
        assert_eq!(k2, 0, "no second modifier");
        assert_eq!(k3, 0x58, "VK for X = 0x58");
    }

    #[test]
    fn set_shortcut_one_mod_one_key_lowercase() {
        set_shortcut("ctrl+z");
        let (k1, k2, k3) = stored_keys();
        assert_eq!(k1, group_to_packed(GROUP_CTRL));
        assert_eq!(k2, 0);
        assert_eq!(k3, 0x5A, "VK for Z = 0x5A");
    }

    #[test]
    fn set_shortcut_two_mods_one_key() {
        set_shortcut("Ctrl+Shift+Space");
        let (k1, k2, k3) = stored_keys();
        assert_eq!(k1, group_to_packed(GROUP_CTRL));
        assert_eq!(k2, group_to_packed(GROUP_SHIFT));
        assert_eq!(k3, 0x20, "VK for Space = 0x20");
    }

    #[test]
    fn set_shortcut_two_mods_fkey() {
        set_shortcut("win+alt+f1");
        let (k1, k2, k3) = stored_keys();
        assert_eq!(k1, group_to_packed(GROUP_WIN));
        assert_eq!(k2, group_to_packed(GROUP_ALT));
        assert_eq!(k3, 0x70, "VK for F1 = 0x70");
    }

    #[test]
    fn set_shortcut_invalid_falls_back() {
        set_shortcut("garbage+nonsense");
        let (k1, k2, _k3) = stored_keys();
        // Fallback is Win+Alt
        assert_eq!(k1, group_to_packed(GROUP_WIN), "fallback should be Win");
        assert_eq!(k2, group_to_packed(GROUP_ALT), "fallback should be Alt");
    }

    #[test]
    #[should_panic(expected = "shortcut combo must not be empty")]
    fn set_shortcut_empty_panics() {
        set_shortcut("");
    }

    #[test]
    #[should_panic(expected = "shortcut must contain '+' separator")]
    fn set_shortcut_no_separator_panics() {
        set_shortcut("winalt");
    }

    #[test]
    fn name_to_group_case_insensitive() {
        // name_to_group works on already-lowercased input (set_shortcut lowercases)
        assert_eq!(name_to_group("win"), GROUP_WIN);
        assert_eq!(name_to_group("alt"), GROUP_ALT);
        assert_eq!(name_to_group("ctrl"), GROUP_CTRL);
        assert_eq!(name_to_group("shift"), GROUP_SHIFT);
        assert_eq!(name_to_group("cmd"), GROUP_WIN);
        assert_eq!(name_to_group("option"), GROUP_ALT);
    }

    #[test]
    fn name_to_vk_letters_and_fkeys() {
        assert_eq!(name_to_vk("a"), 0x41);
        assert_eq!(name_to_vk("z"), 0x5A);
        assert_eq!(name_to_vk("0"), 0x30);
        assert_eq!(name_to_vk("9"), 0x39);
        assert_eq!(name_to_vk("f1"), 0x70);
        assert_eq!(name_to_vk("f12"), 0x7B);
        assert_eq!(name_to_vk("space"), 0x20);
        assert_eq!(name_to_vk("enter"), 0x0D);
        assert_eq!(name_to_vk("tab"), 0x09);
    }

    #[test]
    fn take_event_returns_none_by_default() {
        // Clear any pending event
        take_event();
        assert_eq!(take_event(), EVENT_NONE);
    }

    #[test]
    fn current_label_reflects_set_shortcut() {
        set_shortcut("Ctrl+Shift+X");
        let label = current_label();
        assert!(
            label.contains("Ctrl") || label.contains("ctrl"),
            "label should contain Ctrl: {}",
            label
        );
    }
}
