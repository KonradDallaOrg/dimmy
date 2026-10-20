//! Settings the app itself stores, outside `config.json`, for `dimmy-setup`.
//!
//! Windows keeps them in `<config_dir>/ui_prefs.json` (C# property names,
//! read case-sensitively at launch); macOS in the app's UserDefaults. Both
//! are read once at launch, so they are only written while the app is closed
//! (the CLI holds the same write lock as for `config.json`).
//!
//! The agent sees one name per setting and one hotkey grammar
//! (`ctrl+alt+m`) on both systems; this module translates.

use std::path::Path;

use serde_json::{json, Map, Value};

use crate::setup::{FieldError, Kind};

#[derive(Debug, Clone)]
pub struct AppPref {
    pub key: &'static str,
    pub kind: Kind,
    pub help: &'static str,
    /// Windows: the `ui_prefs.json` property. macOS: the UserDefaults key.
    pub store_key: &'static str,
    pub default: Value,
}

fn pref(
    key: &'static str,
    kind: Kind,
    store_key: &'static str,
    default: Value,
    help: &'static str,
) -> AppPref {
    AppPref {
        key,
        kind,
        help,
        store_key,
        default,
    }
}

fn hotkey(needs_key: bool, may_be_off: bool) -> Kind {
    Kind::AppHotkey {
        needs_key,
        may_be_off,
    }
}

fn channel() -> Kind {
    Kind::Enum(vec!["stable".into(), "prerelease".into()])
}

/// Linux keeps every setting in `config.json`.
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn app_prefs() -> Vec<AppPref> {
    Vec::new()
}

#[cfg(target_os = "windows")]
pub fn app_prefs() -> Vec<AppPref> {
    vec![
        pref("dict_add_hotkey", hotkey(true, false), "DictHotkey", json!("ctrl+shift+d"),
            "Shortcut that adds the selected word to the personal dictionary."),
        pref("command_hotkey", hotkey(false, true), "CommandHotkey", json!(""),
            "Shortcut that records a spoken instruction and applies it to the selected text; empty means off."),
        pref("meeting_hotkey", hotkey(false, true), "MeetingHotkey", json!("ctrl+alt+m"),
            "Shortcut that starts and stops a meeting recording; empty means off."),
        pref("update_channel", channel(), "UpdateChannel", json!("stable"),
            "Which updates Dimmy installs: finished releases only, or test releases too."),
        pref("recap_export_folder", Kind::Text { max_len: 1024 }, "RecapExportFolder", json!(""),
            "Folder that also receives a copy of every meeting recap, for example a notes vault; empty means off."),
        pref("theme", Kind::Enum(vec!["Default".into(), "Light".into(), "Dark".into()]), "Theme", json!("Default"),
            "Light or dark look; Default follows Windows."),
        pref("pill_show_on_startup", Kind::Bool, "PillShowOnStartup", json!(true),
            "Show the recording pill when Dimmy starts."),
        pref("pill_show_on_hotkey", Kind::Bool, "PillShowOnHotkey", json!(true),
            "Bring back a hidden recording pill when a shortcut is pressed."),
        pref("show_taskbar_icon", Kind::Bool, "ShowTaskbarIcon", json!(true),
            "Show Dimmy's button on the taskbar."),
        pref("tray_icon_always_visible", Kind::Bool, "TrayIconAlwaysVisible", json!(false),
            "Keep Dimmy's icon visible next to the clock instead of in the hidden icons."),
    ]
}

#[cfg(target_os = "macos")]
pub fn app_prefs() -> Vec<AppPref> {
    vec![
        pref("shortcut", Kind::AppHotkey { needs_key: false, may_be_off: false }, "shortcutEncoded", json!("fn"),
            "The dictation shortcut: fn alone, two or more modifiers, or modifiers plus a key, joined with +."),
        pref("dict_add_hotkey", hotkey(true, false), "dictHotkeyEncoded", json!("cmd+shift+d"),
            "Shortcut that adds the selected word to the personal dictionary."),
        pref("command_hotkey", hotkey(false, true), "commandHotkeyEncoded", json!(""),
            "Shortcut that records a spoken instruction and applies it to the selected text; empty means off."),
        pref("meeting_hotkey", hotkey(false, true), "meetingHotkeyEncoded", json!("ctrl+opt+m"),
            "Shortcut that starts and stops a meeting recording; empty means off."),
        pref("update_channel", channel(), "dimmy.update_channel", json!("stable"),
            "Which updates Dimmy installs: finished releases only, or test releases too."),
        pref("recap_export_folder", Kind::Text { max_len: 1024 }, "recapExportFolder", json!(""),
            "Folder that also receives a copy of every meeting recap, for example a notes vault; empty means off."),
        pref("pill_visible", Kind::Bool, "pillVisible", json!(true),
            "Show the floating recording pill."),
        pref("show_in_dock", Kind::Bool, "showInDock", json!(false),
            "Show Dimmy's icon in the Dock."),
        pref("show_in_menu_bar", Kind::Bool, "showInMenuBar", json!(true),
            "Show Dimmy's icon in the menu bar. It cannot be off together with show_in_dock."),
    ]
}

pub fn app_pref(key: &str) -> Option<AppPref> {
    app_prefs().into_iter().find(|p| p.key == key)
}

// ── Hotkey grammar ──────────────────────────────────────────────────

/// macOS virtual key codes (ANSI layout), the table `HotkeyCombo` uses.
const MAC_KEYS: &[(&str, u16)] = &[
    ("a", 0x00),
    ("s", 0x01),
    ("d", 0x02),
    ("f", 0x03),
    ("h", 0x04),
    ("g", 0x05),
    ("z", 0x06),
    ("x", 0x07),
    ("c", 0x08),
    ("v", 0x09),
    ("b", 0x0B),
    ("q", 0x0C),
    ("w", 0x0D),
    ("e", 0x0E),
    ("r", 0x0F),
    ("y", 0x10),
    ("t", 0x11),
    ("1", 0x12),
    ("2", 0x13),
    ("3", 0x14),
    ("4", 0x15),
    ("6", 0x16),
    ("5", 0x17),
    ("=", 0x18),
    ("9", 0x19),
    ("7", 0x1A),
    ("8", 0x1C),
    ("0", 0x1D),
    ("o", 0x1F),
    ("u", 0x20),
    ("i", 0x22),
    ("p", 0x23),
    ("l", 0x25),
    ("j", 0x26),
    ("k", 0x28),
    ("n", 0x2D),
    ("m", 0x2E),
    ("enter", 0x24),
    ("tab", 0x30),
    ("space", 0x31),
    ("backspace", 0x33),
    ("esc", 0x35),
    ("f1", 0x7A),
    ("f2", 0x78),
    ("f3", 0x63),
    ("f4", 0x76),
    ("f5", 0x60),
    ("f6", 0x61),
    ("f7", 0x62),
    ("f8", 0x64),
    ("f9", 0x65),
    ("f10", 0x6D),
    ("f11", 0x67),
    ("f12", 0x6F),
];

/// A combo split into modifiers and an optional key.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Combo {
    pub ctrl: bool,
    pub alt: bool,
    pub win: bool,
    pub shift: bool,
    pub fn_key: bool,
    pub key: Option<String>,
}

impl Combo {
    fn modifiers(&self) -> usize {
        [self.ctrl, self.alt, self.win, self.shift]
            .iter()
            .filter(|b| **b)
            .count()
    }
}

/// Split `ctrl+alt+m` into its parts. `alt`/`opt`/`option` and
/// `win`/`cmd`/`command` are the same key on the two systems. `None` for an
/// unknown word, a repeated modifier or two keys.
pub fn split_combo(combo: &str) -> Option<Combo> {
    let mut c = Combo::default();
    for raw in combo.split('+') {
        let t = raw.trim().to_ascii_lowercase();
        let slot = match t.as_str() {
            "ctrl" | "control" => &mut c.ctrl,
            "alt" | "opt" | "option" => &mut c.alt,
            "win" | "cmd" | "command" => &mut c.win,
            "shift" => &mut c.shift,
            "fn" => &mut c.fn_key,
            "" => return None,
            _ => {
                if c.key.is_some() {
                    return None;
                }
                c.key = Some(t);
                continue;
            }
        };
        if *slot {
            return None;
        }
        *slot = true;
    }
    Some(c)
}

fn mac_key_code(name: &str) -> Option<u16> {
    MAC_KEYS.iter().find(|(n, _)| *n == name).map(|(_, c)| *c)
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn mac_key_name(code: u16) -> Option<&'static str> {
    MAC_KEYS.iter().find(|(_, c)| *c == code).map(|(n, _)| *n)
}

/// Whether this system can bind `c`. `needs_key`: a modifier plus a key.
/// Otherwise two modifiers, or one plus a key; on macOS also fn alone.
fn combo_allowed(c: &Combo, needs_key: bool, mac: bool) -> bool {
    let key_known = match &c.key {
        None => true,
        Some(k) if mac => mac_key_code(k).is_some(),
        Some(k) => crate::hotkey::is_key_name(k),
    };
    if !key_known || (c.fn_key && !mac) {
        return false;
    }
    let mods = c.modifiers();
    if c.fn_key {
        return mods == 0 && c.key.is_none() && !needs_key;
    }
    if needs_key {
        return c.key.is_some() && mods >= 1;
    }
    // The Windows hook binds at most two modifier groups.
    let max_mods = if mac { 4 } else { 2 };
    mods <= max_mods && (mods >= 2 || (mods == 1 && c.key.is_some()))
}

/// macOS `HotkeyCombo.encoded`: `ctrl+opt+cmd+shift+2e:M`, or modifiers only.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn mac_combo_encoded(c: &Combo) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (on, name) in [
        (c.ctrl, "ctrl"),
        (c.alt, "opt"),
        (c.win, "cmd"),
        (c.shift, "shift"),
    ] {
        if on {
            parts.push(name.into());
        }
    }
    if let Some(k) = &c.key {
        let code = mac_key_code(k).expect("validated key");
        parts.push(format!("{code:02x}:{}", k.to_ascii_uppercase()));
    }
    parts.join("+")
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn mac_combo_decoded(s: &str) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    for t in s.split('+') {
        match t {
            "ctrl" | "shift" => out.push(t.into()),
            "opt" => out.push("opt".into()),
            "cmd" => out.push("cmd".into()),
            _ => {
                let (hex, _) = t.split_once(':')?;
                let code = u16::from_str_radix(hex, 16).ok()?;
                out.push(mac_key_name(code)?.into());
            }
        }
    }
    Some(out.join("+"))
}

/// macOS `ModifierShortcut.encoded`: ctrl=1 opt=2 cmd=4 shift=8 fn=16, a key
/// flagged at bit 7 with its code from bit 8.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn mac_shortcut_encoded(c: &Combo) -> i64 {
    let mut v = 0i64;
    for (on, bit) in [
        (c.ctrl, 1),
        (c.alt, 2),
        (c.win, 4),
        (c.shift, 8),
        (c.fn_key, 16),
    ] {
        if on {
            v |= bit;
        }
    }
    if let Some(k) = &c.key {
        v |= 1 << 7;
        v |= i64::from(mac_key_code(k).expect("validated key")) << 8;
    }
    assert!(v != 0, "a shortcut has at least one key");
    v
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn mac_shortcut_decoded(v: i64) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    for (bit, name) in [
        (1, "ctrl"),
        (2, "opt"),
        (4, "cmd"),
        (8, "shift"),
        (16, "fn"),
    ] {
        if v & bit != 0 {
            out.push(name.into());
        }
    }
    if v & (1 << 7) != 0 {
        out.push(mac_key_name(u16::try_from(v >> 8).ok()?)?.into());
    }
    (!out.is_empty()).then(|| out.join("+"))
}

// ── Validation ──────────────────────────────────────────────────────

pub fn check_hotkey(key: &str, v: &Value, needs_key: bool, may_be_off: bool) -> Option<FieldError> {
    let bad = |m: String| {
        Some(FieldError::new(
            key,
            "not_allowed",
            m,
            Some(json!(if cfg!(target_os = "macos") {
                ["cmd+shift+d", "ctrl+opt+m", "fn"]
            } else {
                ["ctrl+shift+d", "ctrl+alt+m", "win+alt"]
            })),
        ))
    };
    let Some(s) = v.as_str() else {
        return Some(FieldError::new(
            key,
            "wrong_type",
            format!("{key} must be a string"),
            None,
        ));
    };
    if s.is_empty() {
        return (!may_be_off)
            .then(|| bad(format!("{key} cannot be off")))
            .flatten();
    }
    let mac = cfg!(target_os = "macos");
    match split_combo(s) {
        Some(c) if combo_allowed(&c, needs_key, mac) => None,
        _ if needs_key => bad(format!(
            "{key} must be one or more modifiers plus a key, joined with +"
        )),
        _ => bad(format!(
            "{key} must be two modifiers, or modifiers plus a key, joined with +{}",
            if mac { ", or fn alone" } else { "" }
        )),
    }
}

/// Cross-setting rule the app enforces silently.
pub fn cross_check(m: &Map<String, Value>, current: &Value) -> Vec<FieldError> {
    let get = |k: &str| m.get(k).or(current.get(k)).cloned();
    if app_pref("show_in_dock").is_some()
        && get("show_in_dock") == Some(json!(false))
        && get("show_in_menu_bar") == Some(json!(false))
    {
        return vec![FieldError::new(
            "show_in_menu_bar",
            "conflict",
            "show_in_dock and show_in_menu_bar cannot both be off; Dimmy would switch one back on"
                .into(),
            None,
        )];
    }
    Vec::new()
}

// ── Storage ─────────────────────────────────────────────────────────

/// Windows: the stored value as the agent sees it, default when absent.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn read_ui_prefs(path: &Path) -> Value {
    let stored: Map<String, Value> = std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut out = Map::new();
    for p in app_prefs() {
        out.insert(
            p.key.into(),
            stored.get(p.store_key).cloned().unwrap_or(p.default),
        );
    }
    Value::Object(out)
}

/// Windows: merge `patch` into `ui_prefs.json`, keeping every other property
/// (the app owns the file and may have keys this build does not know).
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub fn write_ui_prefs(path: &Path, patch: &Map<String, Value>) -> Result<(), String> {
    assert!(!patch.is_empty(), "write_ui_prefs needs something to write");
    let mut stored: Map<String, Value> = match std::fs::read_to_string(path) {
        Ok(s) => {
            serde_json::from_str(&s).map_err(|e| format!("ui_prefs.json is not valid JSON: {e}"))?
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(e) => return Err(e.to_string()),
    };
    for (k, v) in patch {
        let p = app_pref(k).expect("validated app setting");
        stored.insert(p.store_key.into(), v.clone());
    }
    let text = serde_json::to_string_pretty(&Value::Object(stored)).expect("JSON");
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// macOS: one `defaults write` argument list per stored key. An empty
/// optional hotkey deletes its key, which is how the app stores "off".
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn mac_defaults_ops(patch: &Map<String, Value>) -> Vec<Vec<String>> {
    let mut ops: Vec<Vec<String>> = Vec::new();
    for (k, v) in patch {
        let p = app_pref(k).expect("validated app setting");
        let sk = p.store_key.to_string();
        let op = match (&p.kind, v) {
            (Kind::AppHotkey { .. }, Value::String(s)) if s.is_empty() => vec!["delete".into(), sk],
            (Kind::AppHotkey { .. }, Value::String(s)) => {
                let c = split_combo(s).expect("validated combo");
                if p.key == "shortcut" {
                    vec![
                        "write".into(),
                        sk,
                        "-int".into(),
                        mac_shortcut_encoded(&c).to_string(),
                    ]
                } else {
                    vec!["write".into(), sk, "-string".into(), mac_combo_encoded(&c)]
                }
            }
            (_, Value::Bool(b)) => vec!["write".into(), sk, "-bool".into(), b.to_string()],
            (_, Value::String(s)) => vec!["write".into(), sk, "-string".into(), s.clone()],
            _ => unreachable!("validated value"),
        };
        ops.push(op);
        if p.key == "meeting_hotkey" {
            // Without it the app puts its default meeting shortcut back.
            ops.push(vec![
                "write".into(),
                "meetingHotkeyDefaultApplied".into(),
                "-bool".into(),
                "true".into(),
            ]);
        }
    }
    ops
}

/// macOS: the agent-facing value of one setting from `defaults read` output
/// (`None` when the key is absent).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn mac_value(p: &AppPref, raw: Option<&str>) -> Value {
    let Some(raw) = raw.map(str::trim) else {
        return match p.key {
            // Absent means off for these two; the app writes a default
            // meeting shortcut on its first launch.
            "command_hotkey" => json!(""),
            _ => p.default.clone(),
        };
    };
    match &p.kind {
        Kind::Bool => json!(raw == "1" || raw == "true"),
        Kind::AppHotkey { .. } if p.key == "shortcut" => raw
            .parse::<i64>()
            .ok()
            .filter(|v| *v != 0)
            .and_then(mac_shortcut_decoded)
            .map_or(p.default.clone(), Value::from),
        Kind::AppHotkey { .. } => mac_combo_decoded(raw).map_or(p.default.clone(), Value::from),
        _ => json!(raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(s: &str) -> Combo {
        split_combo(s).expect(s)
    }

    #[test]
    fn split_accepts_both_systems_names() {
        assert_eq!(c("Ctrl+Option+M"), c("ctrl+alt+m"));
        assert_eq!(c("cmd+shift+d"), c("win+shift+d"));
        assert!(split_combo("ctrl+ctrl+m").is_none());
        assert!(split_combo("ctrl+m+n").is_none());
        assert!(split_combo("ctrl++m").is_none());
    }

    #[test]
    fn allowed_combos_match_each_system() {
        assert!(combo_allowed(&c("ctrl+alt+m"), false, false));
        assert!(combo_allowed(&c("win+alt"), false, false));
        assert!(!combo_allowed(&c("ctrl"), false, false));
        assert!(
            !combo_allowed(&c("ctrl+alt+shift+m"), false, false),
            "Windows hook: two groups"
        );
        assert!(combo_allowed(&c("ctrl+alt+shift+m"), false, true));
        assert!(!combo_allowed(&c("fn"), false, false));
        assert!(combo_allowed(&c("fn"), false, true));
        assert!(!combo_allowed(&c("fn"), true, true));
        assert!(
            !combo_allowed(&c("ctrl+shift"), true, false),
            "dictionary needs a key"
        );
        assert!(combo_allowed(&c("ctrl+shift+d"), true, false));
        assert!(!combo_allowed(&c("cmd+shift+ù"), true, true));
    }

    #[test]
    fn mac_combo_round_trips_the_apps_format() {
        // The app's own default meeting shortcut: ⌃⌥M, keyCode 0x2E.
        assert_eq!(mac_combo_encoded(&c("ctrl+opt+m")), "ctrl+opt+2e:M");
        assert_eq!(mac_combo_encoded(&c("cmd+shift+d")), "cmd+shift+02:D");
        assert_eq!(mac_combo_encoded(&c("cmd+opt")), "opt+cmd");
        assert_eq!(mac_combo_decoded("ctrl+opt+2e:M").unwrap(), "ctrl+opt+m");
        assert_eq!(mac_combo_decoded("cmd+opt").unwrap(), "cmd+opt");
        assert!(mac_combo_decoded("ctrl+zz:M").is_none());
    }

    #[test]
    fn mac_shortcut_matches_the_apps_bitmask() {
        assert_eq!(mac_shortcut_encoded(&c("fn")), 16);
        assert_eq!(mac_shortcut_encoded(&c("ctrl+opt")), 3);
        // cmd + key m: 4 | 1<<7 | 0x2E<<8
        assert_eq!(mac_shortcut_encoded(&c("cmd+m")), 4 | 128 | (0x2E << 8));
        assert_eq!(mac_shortcut_decoded(16).unwrap(), "fn");
        assert_eq!(
            mac_shortcut_decoded(4 | 128 | (0x2E << 8)).unwrap(),
            "cmd+m"
        );
    }

    #[test]
    fn hotkey_values_are_checked() {
        assert!(check_hotkey("command_hotkey", &json!(""), false, true).is_none());
        assert!(check_hotkey("dict_add_hotkey", &json!(""), true, false).is_some());
        assert!(check_hotkey("meeting_hotkey", &json!(3), false, true).is_some());
        assert!(check_hotkey("meeting_hotkey", &json!("ctrl+alt+m"), false, true).is_none());
        assert!(check_hotkey("meeting_hotkey", &json!("ctrl+alt+nope"), false, true).is_some());
    }

    #[test]
    fn ui_prefs_merge_keeps_unknown_properties() {
        let dir = std::env::temp_dir().join(format!("dimmy-ui-prefs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ui_prefs.json");
        std::fs::write(&path, r#"{"BuyerEmail":"x@y.z","Theme":"Dark"}"#).unwrap();
        let Some(first) = app_prefs().into_iter().find(|p| p.key == "dict_add_hotkey") else {
            return;
        };
        let mut patch = Map::new();
        patch.insert(first.key.into(), json!("ctrl+shift+k"));
        write_ui_prefs(&path, &patch).unwrap();
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["BuyerEmail"], json!("x@y.z"));
        assert_eq!(raw[first.store_key], json!("ctrl+shift+k"));
        assert_eq!(read_ui_prefs(&path)[first.key], json!("ctrl+shift+k"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn missing_file_reads_as_defaults() {
        let v = read_ui_prefs(Path::new("/definitely/not/here/ui_prefs.json"));
        for p in app_prefs() {
            assert_eq!(v[p.key], p.default, "{}", p.key);
        }
    }

    #[test]
    fn every_default_validates() {
        for p in app_prefs() {
            if let Kind::AppHotkey {
                needs_key,
                may_be_off,
            } = p.kind
            {
                assert!(
                    check_hotkey(p.key, &p.default, needs_key, may_be_off).is_none(),
                    "{}",
                    p.key
                );
            }
            assert!(p.help.ends_with('.'), "{}", p.key);
        }
    }

    #[test]
    fn mac_ops_turn_a_meeting_hotkey_off_for_good() {
        if app_pref("meeting_hotkey").is_none() {
            return;
        }
        let mut patch = Map::new();
        patch.insert("meeting_hotkey".into(), json!(""));
        let ops = mac_defaults_ops(&patch);
        assert!(ops
            .iter()
            .any(|o| o[0] == "write" && o[1] == "meetingHotkeyDefaultApplied"));
    }
}
