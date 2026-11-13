//! Shortcut recorder: captures a new shortcut while the settings dialog is open.
//!
//! The host calls `start` when its dialog opens and `stop` when it closes.
//! Every key pressed in between goes through `feed`. On Windows the keyboard
//! hook calls it and swallows the key, so Win+… never reaches the shell and
//! the current shortcuts can't fire; on macOS the sheet's event monitors call
//! it through FFI. Each key-down that changes the held keys produces ONE
//! `hotkey_recorder` event with the keys to draw and the verdict.
//!
//! The verdict is computed on a worker thread, never inside the hook: Windows
//! silently removes a low-level hook that runs slow, and that would kill every
//! shortcut until Dimmy restarts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Mutex;

use crate::hotkey::{self, GROUP_ALT, GROUP_CTRL, GROUP_SHIFT, GROUP_WIN};

const VK_TAB: u32 = 0x09;
const VK_ENTER: u32 = 0x0D;
const VK_ESC: u32 = 0x1B;
/// The macOS Fn key, outside the Windows VK range the tables use.
const VK_FN: u32 = 0x1_0000;
/// A key the shortcut grammar has no name for (macOS keycodes we can't map).
const VK_UNKNOWN: u32 = 0x2_0000;

/// The Windows hook binds at most two modifier groups (`Binding` has two
/// slots); the macOS event tap compares the full modifier set.
#[cfg(target_os = "macos")]
const MAX_MODIFIERS: usize = 4;
#[cfg(not(target_os = "macos"))]
const MAX_MODIFIERS: usize = 2;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static SESSION: Mutex<Option<Session>> = Mutex::new(None);

/// Whether a settings dialog is recording a shortcut right now.
pub(crate) fn is_active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Dictation,
    Command,
    Meeting,
    Dictionary,
    /// EXPERIMENT (exp/pc-actions): the shortcut that drives the PC.
    Execute,
}

const TARGETS: [Target; 5] = [
    Target::Dictation,
    Target::Command,
    Target::Meeting,
    Target::Dictionary,
    Target::Execute,
];

impl Target {
    pub fn parse(s: &str) -> Option<Self> {
        TARGETS.into_iter().find(|t| t.id() == s)
    }

    pub fn id(self) -> &'static str {
        match self {
            Target::Dictation => "dictation",
            Target::Command => "command",
            Target::Meeting => "meeting",
            Target::Dictionary => "dictionary",
            Target::Execute => "execute",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Target::Dictation => "the dictation shortcut",
            Target::Command => "the command shortcut",
            Target::Meeting => "the meeting shortcut",
            Target::Dictionary => "the add-to-dictionary shortcut",
            Target::Execute => "the execute shortcut",
        }
    }

    /// The dictionary hotkey is bound with RegisterHotKey (Windows) and a
    /// key-down monitor (macOS): both need a real key. The other three run on
    /// the keyboard hook / event tap, which also fires on two modifiers alone.
    fn allows_modifier_only(self) -> bool {
        self != Target::Dictionary
    }
}

/// The shortcuts configured right now, one per target, in the core grammar
/// (`"win+alt"`, `"ctrl+shift+d"`, `""` = not set).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Current {
    combos: [String; 5],
}

impl Current {
    /// `{"dictation":"win+alt","command":"","meeting":"","dictionary":"ctrl+shift+d"}`.
    /// Missing fields count as "not set".
    pub fn from_json(s: &str) -> Option<Self> {
        let v: serde_json::Value = serde_json::from_str(s).ok()?;
        let obj = v.as_object()?;
        let mut c = Current::default();
        for t in TARGETS {
            c.combos[t as usize] = obj
                .get(t.id())
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
        }
        Some(c)
    }

    fn get(&self, t: Target) -> &str {
        &self.combos[t as usize]
    }
}

/// The keys held down at the moment of a key-down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    pub win: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub fn_key: bool,
    /// VK of the one non-modifier key (0 = none).
    pub key: u32,
    /// The platform's own code for `key` (macOS keycode), for the host to store.
    pub native_key: u32,
}

impl Held {
    fn modifiers(&self) -> usize {
        [self.win, self.ctrl, self.alt, self.shift]
            .into_iter()
            .filter(|&b| b)
            .count()
    }

    fn is_empty(&self) -> bool {
        self.modifiers() == 0 && !self.fn_key && self.key == 0
    }
}

/// What the keyboard hook does with the key it just handed to the recorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decision {
    /// The key is part of the shortcut being recorded: nobody else sees it.
    Swallow,
    /// The key is for the dialog itself (Tab, Enter, Esc) or was already down
    /// when the recorder opened: let it through.
    Pass,
    /// Shift+Tab, but the Shift down was swallowed: send Shift (this VK) then
    /// Tab, and swallow the physical Tab. Windows only.
    ForwardShiftTab(u32),
}

/// Live key state of one recording session.
#[derive(Debug, Default)]
struct Capture {
    held: Held,
    last_sent: Option<Held>,
    /// Keys that were already down when the recorder opened: their key-up
    /// must reach the OS, which saw their key-down.
    pass_up: Vec<u32>,
    /// A Shift down was sent to the OS for Shift+Tab, so its key-up must go too.
    shift_forwarded: bool,
    shift_vk: u32,
}

impl Capture {
    fn new(pass_up: Vec<u32>) -> Self {
        Capture {
            pass_up,
            ..Capture::default()
        }
    }

    /// Feed one key event. Returns what to do with it, and the snapshot to
    /// publish when this key-down changed the held keys.
    fn key(
        &mut self,
        vk: u32,
        native: u32,
        down: bool,
        forward_shift_tab: bool,
    ) -> (Decision, Option<Held>) {
        if !down {
            if let Some(i) = self.pass_up.iter().position(|&k| k == vk) {
                self.pass_up.swap_remove(i);
                return (Decision::Pass, None);
            }
        }
        let h = self.held;
        let no_modifier = h.modifiers() == 0 && !h.fn_key;
        if matches!(vk, VK_TAB | VK_ENTER | VK_ESC) && no_modifier {
            return (Decision::Pass, None);
        }
        let only_shift = h.shift && !h.win && !h.ctrl && !h.alt && !h.fn_key;
        if vk == VK_TAB && only_shift {
            if down && forward_shift_tab && !self.shift_forwarded {
                self.shift_forwarded = true;
                return (Decision::ForwardShiftTab(self.shift_vk), None);
            }
            return (Decision::Pass, None);
        }

        let is_shift = hotkey::vk_to_group(vk) == GROUP_SHIFT;
        if is_shift && down {
            self.shift_vk = vk;
        }
        apply(&mut self.held, vk, native, down);
        if is_shift && !down && self.shift_forwarded {
            self.shift_forwarded = false;
            return (Decision::Pass, None);
        }
        if !down || self.last_sent == Some(self.held) {
            return (Decision::Swallow, None);
        }
        self.last_sent = Some(self.held);
        (Decision::Swallow, Some(self.held))
    }
}

fn apply(held: &mut Held, vk: u32, native: u32, down: bool) {
    match hotkey::vk_to_group(vk) {
        GROUP_WIN => held.win = down,
        GROUP_CTRL => held.ctrl = down,
        GROUP_ALT => held.alt = down,
        GROUP_SHIFT => held.shift = down,
        _ if vk == VK_FN => held.fn_key = down,
        _ if down => {
            held.key = vk;
            held.native_key = native;
        }
        _ if held.key == vk => {
            held.key = 0;
            held.native_key = 0;
        }
        _ => {}
    }
}

/// The verdict on one snapshot, as the dialog shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    /// One label per key, in the platform's usual order.
    pub keys: Vec<String>,
    /// The shortcut in the core grammar, what the host stores.
    pub combo: String,
    pub can_save: bool,
    /// Why it can't be saved. Empty when it can, and for the empty snapshot.
    pub error: String,
    /// Things worth knowing that don't block saving.
    pub warnings: Vec<String>,
}

pub fn evaluate(
    target: Target,
    held: &Held,
    current: &Current,
    system_taken: impl Fn(&Held) -> bool,
) -> Verdict {
    let keys = labels(held);
    let combo = grammar(held);
    let mut error = problem(target, held).unwrap_or_default();

    if error.is_empty() {
        if let Some(other) = TARGETS
            .into_iter()
            .filter(|&t| t != target)
            .find(|&t| hotkey::combos_conflict(&combo, current.get(t)))
        {
            error = format!("Already used by {}.", other.label());
        }
    }

    let can_save = !held.is_empty() && error.is_empty();
    let mut warnings = Vec::new();
    if can_save {
        let ours = TARGETS
            .into_iter()
            .any(|t| same_combo(&combo, current.get(t)));
        if !ours && system_taken(held) {
            warnings.push(SYSTEM_TAKEN.to_string());
        }
        if types_characters(held) {
            warnings.push(TYPES_CHARACTERS.to_string());
        }
    }

    assert!(keys.len() <= 6, "at most fn + 4 modifiers + 1 key");
    assert!(
        !can_save || !combo.is_empty() || held.fn_key,
        "a savable shortcut has a spelling"
    );
    Verdict {
        keys,
        combo,
        can_save,
        error,
        warnings,
    }
}

/// Why `held` can't be this target's shortcut, before looking at the others.
fn problem(target: Target, held: &Held) -> Option<String> {
    if held.is_empty() {
        return None;
    }
    let mods = held.modifiers();
    if held.fn_key {
        // macOS: the tap matches Fn (alone or with modifiers) on the flags it
        // carries; there is no Fn in the grammar the other shortcuts use.
        if target == Target::Dictation && held.key == 0 && mods <= MAX_MODIFIERS {
            return None;
        }
        return Some("Fn works only for the dictation shortcut, and without a key.".into());
    }
    if held.key != 0 && !is_known_key(held.key) {
        return Some(
            "This key can't be used in a shortcut. Try a letter, a number, F1 to F12 or Space."
                .into(),
        );
    }
    if mods == 0 {
        return Some(START_WITH_MODIFIER.into());
    }
    if mods > MAX_MODIFIERS {
        return Some("Use at most two modifier keys.".into());
    }
    if held.key == 0 && (mods == 1 || !target.allows_modifier_only()) {
        return Some(if target.allows_modifier_only() {
            "Add a key or a second modifier.".into()
        } else {
            "Add a key, for example D.".into()
        });
    }
    None
}

fn same_combo(a: &str, b: &str) -> bool {
    match (hotkey::combo_keyset(a), hotkey::combo_keyset(b)) {
        (Some(x), Some(y)) => x.len() == y.len() && x.iter().all(|k| y.contains(k)),
        _ => false,
    }
}

fn key_label(vk: u32) -> String {
    if is_known_key(vk) {
        hotkey::vk_to_label(vk)
    } else {
        "?".into()
    }
}

/// Core grammar, modifiers first: `win+ctrl+alt+shift+key`.
fn grammar(held: &Held) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (on, group) in [
        (held.win, GROUP_WIN),
        (held.ctrl, GROUP_CTRL),
        (held.alt, GROUP_ALT),
        (held.shift, GROUP_SHIFT),
    ] {
        if on {
            parts.push(hotkey::group_to_name(group).into());
        }
    }
    if is_known_key(held.key) {
        parts.push(hotkey::vk_to_name(held.key));
    }
    parts.join("+")
}

#[cfg(target_os = "macos")]
fn labels(held: &Held) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for (on, glyph) in [
        (held.fn_key, "fn"),
        (held.ctrl, "⌃"),
        (held.alt, "⌥"),
        (held.shift, "⇧"),
        (held.win, "⌘"),
    ] {
        if on {
            v.push(glyph.into());
        }
    }
    if held.key != 0 {
        v.push(key_label(held.key));
    }
    v
}

#[cfg(not(target_os = "macos"))]
fn labels(held: &Held) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for (on, name) in [
        (held.win, "Win"),
        (held.ctrl, "Ctrl"),
        (held.alt, "Alt"),
        (held.shift, "Shift"),
    ] {
        if on {
            v.push(name.into());
        }
    }
    if held.key != 0 {
        v.push(key_label(held.key));
    }
    v
}

#[cfg(target_os = "macos")]
const START_WITH_MODIFIER: &str = "Start with a modifier: ⌃, ⌥, ⇧ or ⌘.";
#[cfg(not(target_os = "macos"))]
const START_WITH_MODIFIER: &str = "Start with a modifier: Ctrl, Alt, Shift or Win.";

#[cfg(target_os = "macos")]
const SYSTEM_TAKEN: &str = "macOS already uses this shortcut, so one of the two may stop working.";
#[cfg(not(target_os = "macos"))]
const SYSTEM_TAKEN: &str =
    "Windows or another app already uses this shortcut, so one of the two may stop working.";

/// macOS: Option+key types special characters (Option+E starts an accent).
#[cfg(target_os = "macos")]
fn types_characters(held: &Held) -> bool {
    held.alt && !held.ctrl && !held.win && held.key != 0
}
#[cfg(target_os = "macos")]
const TYPES_CHARACTERS: &str =
    "Option with a key types special characters, so this shortcut can get in the way of typing them.";

/// Windows: Ctrl+Alt is how many layouts spell AltGr (Italian @ is Ctrl+Alt+ò).
#[cfg(not(target_os = "macos"))]
fn types_characters(held: &Held) -> bool {
    held.ctrl && held.alt && !held.win && held.key != 0
}
#[cfg(not(target_os = "macos"))]
const TYPES_CHARACTERS: &str =
    "On many keyboards Ctrl+Alt works as AltGr, so this shortcut can get in the way of typing characters like @ or €.";

impl Verdict {
    fn to_json(&self, target: Target, held: &Held) -> String {
        let key_name = if is_known_key(held.key) {
            hotkey::vk_to_name(held.key)
        } else {
            String::new()
        };
        serde_json::json!({
            "target": target.id(),
            "keys": self.keys,
            "combo": self.combo,
            "can_save": self.can_save,
            "error": self.error,
            "warnings": self.warnings,
            "mods": {
                "win": held.win,
                "ctrl": held.ctrl,
                "alt": held.alt,
                "shift": held.shift,
                "fn": held.fn_key,
            },
            "key": key_name,
            "native_key": held.native_key,
        })
        .to_string()
    }
}

fn is_known_key(vk: u32) -> bool {
    vk != 0 && vk != VK_FN && vk != VK_UNKNOWN && hotkey::is_key_name(&hotkey::vk_to_name(vk))
}

struct Session {
    capture: Capture,
    /// Snapshots go to the worker; the hook never waits on it.
    tx: SyncSender<Held>,
}

/// Open a recording session. Replaces one that is still open.
pub fn start(target: Target, current: Current) {
    let (tx, rx) = sync_channel::<Held>(16);
    std::thread::Builder::new()
        .name("dimmy-hotkey-recorder".into())
        .spawn(move || worker(target, current, rx))
        .expect("spawn the hotkey recorder thread");
    let session = Session {
        capture: Capture::new(platform::held_keys()),
        tx,
    };
    *SESSION.lock().unwrap_or_else(|e| e.into_inner()) = Some(session);
    ACTIVE.store(true, Ordering::SeqCst);
}

/// Close the session. Returns whether one was open. Dropping the sender ends
/// the worker once it has published what was already queued.
pub fn stop() -> bool {
    ACTIVE.store(false, Ordering::SeqCst);
    SESSION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .is_some()
}

/// One key event from the hook (Windows) or the sheet (macOS).
/// `forward_shift_tab` is true only on the Windows hook, where a swallowed
/// Shift has to be replayed for Shift+Tab to move the focus back.
pub(crate) fn feed(native: u32, down: bool, forward_shift_tab: bool) -> Decision {
    let vk = platform::native_to_vk(native);
    let mut guard = SESSION.lock().unwrap_or_else(|e| e.into_inner());
    let Some(session) = guard.as_mut() else {
        return Decision::Pass;
    };
    let (decision, snapshot) = session.capture.key(vk, native, down, forward_shift_tab);
    if let Some(held) = snapshot {
        // A full queue drops this frame; the next key-down publishes again.
        let _ = session.tx.try_send(held);
    }
    decision
}

fn worker(target: Target, current: Current, rx: Receiver<Held>) {
    for held in rx {
        let verdict = evaluate(target, &held, &current, platform::system_taken);
        crate::ffi::emit_event("hotkey_recorder", &verdict.to_json(target, &held));
    }
}

#[cfg(target_os = "windows")]
mod platform {
    use super::{is_known_key, Held};
    use crate::hotkey::{
        VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RCONTROL, VK_RMENU, VK_RSHIFT, VK_RWIN,
    };

    const MOD_ALT: u32 = 0x0001;
    const MOD_CONTROL: u32 = 0x0002;
    const MOD_SHIFT: u32 = 0x0004;
    const MOD_WIN: u32 = 0x0008;
    const MOD_NOREPEAT: u32 = 0x4000;
    const ERROR_HOTKEY_ALREADY_REGISTERED: i32 = 1409;
    /// Arbitrary, only ever registered for the instant of the probe.
    const PROBE_ID: i32 = 0x0D1E;

    extern "system" {
        fn GetAsyncKeyState(vKey: i32) -> i16;
        fn RegisterHotKey(hWnd: isize, id: i32, fsModifiers: u32, vk: u32) -> i32;
        fn UnregisterHotKey(hWnd: isize, id: i32) -> i32;
    }

    pub fn native_to_vk(native: u32) -> u32 {
        native
    }

    pub fn held_keys() -> Vec<u32> {
        [
            VK_LWIN,
            VK_RWIN,
            VK_LMENU,
            VK_RMENU,
            VK_LCONTROL,
            VK_RCONTROL,
            VK_LSHIFT,
            VK_RSHIFT,
        ]
        .into_iter()
        .filter(|&vk| unsafe { GetAsyncKeyState(vk as i32) } as u16 & 0x8000 != 0)
        .collect()
    }

    /// Windows has no list of who owns which shortcut. What it does have:
    /// RegisterHotKey refuses a combination someone already registered, so
    /// try to take it and give it straight back. Shortcuts an app handles
    /// only while it's focused (Teams' Ctrl+Shift+M) stay invisible.
    pub fn system_taken(held: &Held) -> bool {
        if !is_known_key(held.key) {
            return false;
        }
        let mut mods = MOD_NOREPEAT;
        for (on, m) in [
            (held.alt, MOD_ALT),
            (held.ctrl, MOD_CONTROL),
            (held.shift, MOD_SHIFT),
            (held.win, MOD_WIN),
        ] {
            if on {
                mods |= m;
            }
        }
        unsafe {
            if RegisterHotKey(0, PROBE_ID, mods, held.key) != 0 {
                UnregisterHotKey(0, PROBE_ID);
                return false;
            }
        }
        std::io::Error::last_os_error().raw_os_error() == Some(ERROR_HOTKEY_ALREADY_REGISTERED)
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{Held, VK_FN, VK_UNKNOWN};

    const KC_FUNCTION: u32 = 0x3F;

    pub fn native_to_vk(native: u32) -> u32 {
        if native == KC_FUNCTION {
            return VK_FN;
        }
        match crate::hotkey::mac_keycode_to_vk(native as u16) {
            0 => VK_UNKNOWN,
            vk => vk,
        }
    }

    /// The sheet's monitors only see keys pressed after it opened.
    pub fn held_keys() -> Vec<u32> {
        Vec::new()
    }

    type CFTypeRef = *const c_void;
    use std::ffi::{c_char, c_void};

    // Carbon is already linked: DictHotkeyManager uses RegisterEventHotKey.
    #[link(name = "Carbon", kind = "framework")]
    extern "C" {
        fn CopySymbolicHotKeys(out: *mut CFTypeRef) -> i32;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFArrayGetCount(array: CFTypeRef) -> isize;
        fn CFArrayGetValueAtIndex(array: CFTypeRef, idx: isize) -> CFTypeRef;
        fn CFDictionaryGetValue(dict: CFTypeRef, key: CFTypeRef) -> CFTypeRef;
        fn CFNumberGetValue(number: CFTypeRef, the_type: isize, out: *mut c_void) -> bool;
        fn CFBooleanGetValue(boolean: CFTypeRef) -> bool;
        fn CFStringCreateWithCString(
            alloc: CFTypeRef,
            s: *const c_char,
            encoding: u32,
        ) -> CFTypeRef;
        fn CFRelease(cf: CFTypeRef);
    }

    const K_CF_NUMBER_SINT64: isize = 4;
    const K_CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;
    // Carbon modifier bits (Events.h).
    const CMD_KEY: i64 = 1 << 8;
    const SHIFT_KEY: i64 = 1 << 9;
    const OPTION_KEY: i64 = 1 << 11;
    const CONTROL_KEY: i64 = 1 << 12;
    const MODIFIER_MASK: i64 = CMD_KEY | SHIFT_KEY | OPTION_KEY | CONTROL_KEY;

    fn cf_string(s: &std::ffi::CStr) -> CFTypeRef {
        unsafe {
            CFStringCreateWithCString(std::ptr::null(), s.as_ptr(), K_CF_STRING_ENCODING_UTF8)
        }
    }

    fn number(dict: CFTypeRef, key: CFTypeRef) -> Option<i64> {
        let n = unsafe { CFDictionaryGetValue(dict, key) };
        if n.is_null() {
            return None;
        }
        let mut v: i64 = 0;
        let ok =
            unsafe { CFNumberGetValue(n, K_CF_NUMBER_SINT64, &mut v as *mut i64 as *mut c_void) };
        ok.then_some(v)
    }

    /// The system's own shortcuts (Spotlight, screenshots, Mission Control,
    /// input sources), as System Settings > Keyboard > Shortcuts lists them.
    /// Another app's shortcuts stay invisible, as on Windows.
    pub fn system_taken(held: &Held) -> bool {
        if held.key == 0 || held.native_key == 0 {
            return false;
        }
        let mut want: i64 = 0;
        for (on, bit) in [
            (held.win, CMD_KEY),
            (held.shift, SHIFT_KEY),
            (held.alt, OPTION_KEY),
            (held.ctrl, CONTROL_KEY),
        ] {
            if on {
                want |= bit;
            }
        }
        let mut array: CFTypeRef = std::ptr::null();
        if unsafe { CopySymbolicHotKeys(&mut array) } != 0 || array.is_null() {
            return false;
        }
        let k_code = cf_string(c"kHISymbolicHotKeyCode");
        let k_mods = cf_string(c"kHISymbolicHotKeyModifiers");
        let k_enabled = cf_string(c"kHISymbolicHotKeyEnabled");
        let mut taken = false;
        for i in 0..unsafe { CFArrayGetCount(array) } {
            let entry = unsafe { CFArrayGetValueAtIndex(array, i) };
            let enabled = unsafe { CFDictionaryGetValue(entry, k_enabled) };
            if enabled.is_null() || !unsafe { CFBooleanGetValue(enabled) } {
                continue;
            }
            if number(entry, k_code) == Some(held.native_key as i64)
                && number(entry, k_mods).map(|m| m & MODIFIER_MASK) == Some(want)
            {
                taken = true;
                break;
            }
        }
        unsafe {
            CFRelease(k_code);
            CFRelease(k_mods);
            CFRelease(k_enabled);
            CFRelease(array);
        }
        taken
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
mod platform {
    use super::Held;

    pub fn native_to_vk(native: u32) -> u32 {
        native
    }

    pub fn held_keys() -> Vec<u32> {
        Vec::new()
    }

    pub fn system_taken(_held: &Held) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hotkey::{VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_RSHIFT};

    const D: u32 = 0x44;
    const F5: u32 = 0x74;
    const ARROW_UP: u32 = 0x26;

    // The labels each platform draws.
    #[cfg(target_os = "macos")]
    const CTRL: &str = "⌃";
    #[cfg(not(target_os = "macos"))]
    const CTRL: &str = "Ctrl";
    #[cfg(target_os = "macos")]
    const SHIFT: &str = "⇧";
    #[cfg(not(target_os = "macos"))]
    const SHIFT: &str = "Shift";

    fn current(d: &str, c: &str, m: &str, k: &str) -> Current {
        Current::from_json(&format!(
            r#"{{"dictation":"{d}","command":"{c}","meeting":"{m}","dictionary":"{k}"}}"#
        ))
        .unwrap()
    }

    fn none_taken(_: &Held) -> bool {
        false
    }

    fn held(win: bool, ctrl: bool, alt: bool, shift: bool, key: u32) -> Held {
        Held {
            win,
            ctrl,
            alt,
            shift,
            fn_key: false,
            key,
            native_key: key,
        }
    }

    /// Feed a key and return only the snapshot.
    fn press(c: &mut Capture, vk: u32) -> Option<Held> {
        c.key(vk, vk, true, true).1
    }

    fn release(c: &mut Capture, vk: u32) -> Option<Held> {
        c.key(vk, vk, false, true).1
    }

    // ── Capture ──────────────────────────────────────────────────────

    #[test]
    fn keys_appear_one_by_one_as_they_go_down() {
        let mut c = Capture::new(vec![]);
        assert_eq!(
            press(&mut c, VK_LCONTROL),
            Some(held(false, true, false, false, 0))
        );
        assert_eq!(
            press(&mut c, VK_LSHIFT),
            Some(held(false, true, false, true, 0))
        );
        assert_eq!(press(&mut c, D), Some(held(false, true, false, true, D)));
    }

    #[test]
    fn releasing_keeps_the_last_snapshot_and_sends_nothing() {
        let mut c = Capture::new(vec![]);
        press(&mut c, VK_LCONTROL);
        press(&mut c, D);
        assert_eq!(release(&mut c, D), None);
        assert_eq!(release(&mut c, VK_LCONTROL), None);
        assert_eq!(c.last_sent, Some(held(false, true, false, false, D)));
    }

    #[test]
    fn auto_repeat_sends_one_snapshot() {
        let mut c = Capture::new(vec![]);
        press(&mut c, VK_LCONTROL);
        assert!(press(&mut c, D).is_some());
        assert_eq!(press(&mut c, D), None);
        assert_eq!(press(&mut c, D), None);
    }

    #[test]
    fn a_fresh_press_after_letting_go_starts_over() {
        let mut c = Capture::new(vec![]);
        press(&mut c, VK_LCONTROL);
        press(&mut c, D);
        release(&mut c, D);
        release(&mut c, VK_LCONTROL);
        assert_eq!(
            press(&mut c, VK_LSHIFT),
            Some(held(false, false, false, true, 0))
        );
    }

    #[test]
    fn a_second_key_replaces_the_first() {
        let mut c = Capture::new(vec![]);
        press(&mut c, VK_LCONTROL);
        press(&mut c, D);
        assert_eq!(press(&mut c, F5), Some(held(false, true, false, false, F5)));
    }

    #[test]
    fn shortcut_keys_are_swallowed() {
        let mut c = Capture::new(vec![]);
        assert_eq!(c.key(VK_LWIN, VK_LWIN, true, true).0, Decision::Swallow);
        assert_eq!(c.key(VK_LMENU, VK_LMENU, true, true).0, Decision::Swallow);
        assert_eq!(c.key(VK_LMENU, VK_LMENU, false, true).0, Decision::Swallow);
        assert_eq!(c.key(VK_LWIN, VK_LWIN, false, true).0, Decision::Swallow);
    }

    #[test]
    fn tab_enter_esc_alone_reach_the_dialog_and_record_nothing() {
        let mut c = Capture::new(vec![]);
        for vk in [VK_TAB, VK_ENTER, VK_ESC] {
            assert_eq!(c.key(vk, vk, true, true), (Decision::Pass, None));
            assert_eq!(c.key(vk, vk, false, true), (Decision::Pass, None));
        }
        assert_eq!(c.last_sent, None);
    }

    #[test]
    fn enter_with_a_modifier_is_a_shortcut() {
        let mut c = Capture::new(vec![]);
        press(&mut c, VK_LCONTROL);
        let (d, snap) = c.key(VK_ENTER, VK_ENTER, true, true);
        assert_eq!(d, Decision::Swallow);
        assert_eq!(snap, Some(held(false, true, false, false, VK_ENTER)));
    }

    #[test]
    fn shift_tab_moves_focus_back_on_windows() {
        let mut c = Capture::new(vec![]);
        press(&mut c, VK_RSHIFT);
        assert_eq!(
            c.key(VK_TAB, VK_TAB, true, true).0,
            Decision::ForwardShiftTab(VK_RSHIFT)
        );
        // Auto-repeat: the OS already holds Shift now.
        assert_eq!(c.key(VK_TAB, VK_TAB, true, true).0, Decision::Pass);
        assert_eq!(c.key(VK_TAB, VK_TAB, false, true).0, Decision::Pass);
        // The OS saw our Shift down, so it must see the up.
        assert_eq!(c.key(VK_RSHIFT, VK_RSHIFT, false, true).0, Decision::Pass);
        // The next Shift is the recorder's again.
        assert_eq!(c.key(VK_LSHIFT, VK_LSHIFT, true, true).0, Decision::Swallow);
    }

    #[test]
    fn shift_tab_passes_untouched_where_no_forwarding_is_needed() {
        let mut c = Capture::new(vec![]);
        c.key(VK_LSHIFT, VK_LSHIFT, true, false);
        assert_eq!(c.key(VK_TAB, VK_TAB, true, false).0, Decision::Pass);
        assert_eq!(
            c.key(VK_LSHIFT, VK_LSHIFT, false, false).0,
            Decision::Swallow
        );
    }

    #[test]
    fn a_key_held_when_recording_starts_gets_its_key_up() {
        let mut c = Capture::new(vec![VK_LCONTROL]);
        assert_eq!(
            c.key(VK_LCONTROL, VK_LCONTROL, false, true),
            (Decision::Pass, None)
        );
        // Only once: the next Ctrl is part of the new shortcut.
        assert_eq!(
            c.key(VK_LCONTROL, VK_LCONTROL, true, true).0,
            Decision::Swallow
        );
        assert_eq!(
            c.key(VK_LCONTROL, VK_LCONTROL, false, true).0,
            Decision::Swallow
        );
    }

    // ── Verdict ──────────────────────────────────────────────────────

    #[test]
    fn modifier_and_key_is_valid_and_spelled_in_the_core_grammar() {
        let v = evaluate(
            Target::Command,
            &held(false, true, false, true, D),
            &current("win+alt", "", "", "ctrl+shift+k"),
            none_taken,
        );
        assert!(v.can_save, "{v:?}");
        assert_eq!(v.combo, "ctrl+shift+d");
        assert_eq!(v.keys, [CTRL, SHIFT, "D"]);
        assert_eq!(v.error, "");
        assert!(v.warnings.is_empty());
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn labels_follow_the_windows_order() {
        let v = evaluate(
            Target::Dictation,
            &held(true, true, false, false, D),
            &Current::default(),
            none_taken,
        );
        assert_eq!(v.keys, ["Win", "Ctrl", "D"]);
        assert_eq!(v.combo, "win+ctrl+d");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn labels_follow_the_mac_order_and_glyphs() {
        let v = evaluate(
            Target::Dictation,
            &held(true, true, true, true, D),
            &Current::default(),
            none_taken,
        );
        assert_eq!(v.keys, ["⌃", "⌥", "⇧", "⌘", "D"]);
        assert_eq!(v.combo, "cmd+ctrl+option+shift+d");
    }

    #[test]
    fn the_empty_snapshot_has_no_error_and_cannot_be_saved() {
        let v = evaluate(
            Target::Command,
            &Held::default(),
            &Current::default(),
            none_taken,
        );
        assert!(!v.can_save);
        assert_eq!(v.error, "");
        assert!(v.keys.is_empty());
    }

    #[test]
    fn a_key_without_modifier_is_refused() {
        let v = evaluate(
            Target::Dictation,
            &held(false, false, false, false, F5),
            &Current::default(),
            none_taken,
        );
        assert!(!v.can_save);
        assert!(v.error.starts_with("Start with a modifier"), "{}", v.error);
        assert_eq!(v.keys, ["F5"]);
    }

    #[test]
    fn one_modifier_alone_asks_for_more() {
        let v = evaluate(
            Target::Dictation,
            &held(false, true, false, false, 0),
            &Current::default(),
            none_taken,
        );
        assert!(!v.can_save);
        assert!(v.error.starts_with("Add a key"), "{}", v.error);
    }

    #[test]
    fn two_modifiers_alone_work_on_the_hook_but_not_for_the_dictionary() {
        let win_alt = held(true, false, true, false, 0);
        for t in [Target::Dictation, Target::Command, Target::Meeting] {
            let v = evaluate(t, &win_alt, &Current::default(), none_taken);
            assert!(v.can_save, "{t:?}: {v:?}");
            assert_eq!(
                v.combo,
                format!(
                    "{}+{}",
                    hotkey::group_to_name(GROUP_WIN),
                    hotkey::group_to_name(GROUP_ALT)
                )
            );
        }
        let v = evaluate(
            Target::Dictionary,
            &win_alt,
            &Current::default(),
            none_taken,
        );
        assert!(!v.can_save);
        assert!(v.error.starts_with("Add a key"), "{}", v.error);
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn three_modifiers_are_more_than_the_hook_binds() {
        let v = evaluate(
            Target::Command,
            &held(false, true, true, true, D),
            &Current::default(),
            none_taken,
        );
        assert!(!v.can_save);
        assert!(v.error.contains("at most two"), "{}", v.error);
    }

    #[test]
    fn a_key_without_a_name_is_refused_and_shown_as_a_question_mark() {
        let v = evaluate(
            Target::Command,
            &held(false, true, false, false, ARROW_UP),
            &Current::default(),
            none_taken,
        );
        assert!(!v.can_save);
        assert!(v.error.contains("can't be used"), "{}", v.error);
        assert_eq!(v.keys, [CTRL, "?"]);
    }

    #[test]
    fn fn_is_for_the_dictation_shortcut_without_a_key() {
        let fn_only = Held {
            fn_key: true,
            ..Held::default()
        };
        let v = evaluate(Target::Dictation, &fn_only, &Current::default(), none_taken);
        assert!(v.can_save, "{v:?}");
        let v = evaluate(Target::Command, &fn_only, &Current::default(), none_taken);
        assert!(!v.can_save);
        // The macOS tap matches Fn with other modifiers as a chord.
        let fn_ctrl = Held {
            fn_key: true,
            ctrl: true,
            ..Held::default()
        };
        let v = evaluate(Target::Dictation, &fn_ctrl, &Current::default(), none_taken);
        assert!(v.can_save, "{v:?}");
        let fn_key = Held {
            fn_key: true,
            key: D,
            native_key: D,
            ..Held::default()
        };
        let v = evaluate(Target::Dictation, &fn_key, &Current::default(), none_taken);
        assert!(!v.can_save);
        assert!(v.error.starts_with("Fn works only"), "{}", v.error);
    }

    #[test]
    fn the_execute_shortcut_takes_part_in_conflicts_both_ways() {
        const X: u32 = 0x58;
        let cur = Current::from_json(
            r#"{"dictation":"win+alt","command":"ctrl+alt+x","execute":"ctrl+shift+d"}"#,
        )
        .unwrap();
        let v = evaluate(
            Target::Execute,
            &held(false, true, true, false, X),
            &cur,
            none_taken,
        );
        assert_eq!(v.error, "Already used by the command shortcut.");
        let v = evaluate(
            Target::Dictionary,
            &held(false, true, false, true, D),
            &cur,
            none_taken,
        );
        assert_eq!(v.error, "Already used by the execute shortcut.");
    }

    #[test]
    fn a_shortcut_another_target_uses_is_blocked_and_named() {
        let cur = current("win+alt", "", "", "ctrl+shift+d");
        let v = evaluate(
            Target::Command,
            &held(false, true, false, true, D),
            &cur,
            none_taken,
        );
        assert!(!v.can_save);
        assert_eq!(v.error, "Already used by the add-to-dictionary shortcut.");
    }

    #[test]
    fn overlap_counts_as_a_conflict_too() {
        // Win+Alt fires whenever Win+Alt+N is pressed.
        let cur = current("win+alt", "", "", "");
        let v = evaluate(
            Target::Meeting,
            &held(true, false, true, false, 0x4E),
            &cur,
            none_taken,
        );
        assert!(!v.can_save);
        assert_eq!(v.error, "Already used by the dictation shortcut.");
    }

    #[test]
    fn a_target_never_conflicts_with_itself() {
        let cur = current("", "", "", "ctrl+shift+d");
        let v = evaluate(
            Target::Dictionary,
            &held(false, true, false, true, D),
            &cur,
            none_taken,
        );
        assert!(v.can_save, "{v:?}");
    }

    #[test]
    fn a_shortcut_taken_by_the_system_is_a_warning_not_a_block() {
        let v = evaluate(
            Target::Command,
            &held(false, true, false, true, D),
            &Current::default(),
            |_| true,
        );
        assert!(v.can_save);
        assert_eq!(v.warnings.len(), 1);
        assert!(v.warnings[0].contains("already uses this shortcut"));
    }

    #[test]
    fn our_own_shortcuts_are_not_probed_against_the_system() {
        // The dictionary hotkey is registered by Dimmy itself, so the probe
        // would always say "taken".
        let cur = current("", "", "", "Ctrl+Shift+D");
        let probed = std::cell::Cell::new(false);
        evaluate(
            Target::Dictionary,
            &held(false, true, false, true, D),
            &cur,
            |_| {
                probed.set(true);
                true
            },
        );
        assert!(!probed.get());
    }

    #[test]
    #[cfg(not(target_os = "macos"))]
    fn ctrl_alt_with_a_key_warns_about_altgr() {
        let v = evaluate(
            Target::Command,
            &held(false, true, true, false, 0x45),
            &Current::default(),
            none_taken,
        );
        assert!(v.can_save);
        assert!(v.warnings.iter().any(|w| w.contains("AltGr")), "{v:?}");
        // With Win it's not AltGr any more.
        let v = evaluate(
            Target::Command,
            &held(true, true, false, false, 0x45),
            &Current::default(),
            none_taken,
        );
        assert!(v.warnings.is_empty());
    }

    #[test]
    fn current_json_tolerates_missing_fields_and_rejects_garbage() {
        let c = Current::from_json(r#"{"dictation":"win+alt"}"#).unwrap();
        assert_eq!(c.get(Target::Dictation), "win+alt");
        assert_eq!(c.get(Target::Meeting), "");
        assert!(Current::from_json("not json").is_none());
        assert!(Current::from_json("[1,2]").is_none());
    }

    #[test]
    fn targets_round_trip_through_their_id() {
        for t in TARGETS {
            assert_eq!(Target::parse(t.id()), Some(t));
        }
        assert_eq!(Target::parse("pill"), None);
    }
}
