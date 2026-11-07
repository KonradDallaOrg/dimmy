//! Commands of the app in front — EXPERIMENT (branch `exp/pc-actions`).
//!
//! When the execute shortcut is pressed, the commands of the foreground window
//! are read while the user talks: the classic Win32 menu (Notepad++: 484 items
//! with their shortcuts in 9 ms), or, when there is none, the controls UI
//! Automation can invoke (Chrome: ~50 named buttons and links in 150-300 ms).
//! The spoken words are matched to a command name by the words they share; a
//! clear match runs it (WM_COMMAND for a menu item, Invoke for a control),
//! otherwise the best candidates go to Claude with the command.
//!
//! A command whose name says it deletes, sends, closes, quits, buys or signs
//! out is never run by voice in this version, and none of these run live.

use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq)]
pub struct AppCommand {
    /// "File > Salva tutto": the menu path, or the control name.
    pub label: String,
    /// The last part of the label, what a person would say.
    pub name: String,
    pub shortcut: String,
    pub risky: bool,
    target: Target,
}

#[derive(Clone, Debug, PartialEq)]
enum Target {
    Menu { hwnd: isize, id: u32 },
    Control { hwnd: isize },
}

struct Snapshot {
    process: String,
    cmds: Vec<AppCommand>,
}

static SNAPSHOT: Mutex<Option<Snapshot>> = Mutex::new(None);

const RISKY: &[&str] = &[
    "elimin",
    "cancell",
    "cestino",
    "esci",
    "chiud",
    "invia",
    "inoltra",
    "compra",
    "acquist",
    "paga",
    "disinstall",
    "formatta",
    "svuota",
    "disconnett",
    "rimuov",
    "sostituisci tutt",
    "spegn",
    "riavvi",
    "delete",
    "remove",
    "exit",
    "quit",
    "close",
    "send",
    "forward",
    "buy",
    "purchase",
    "pay",
    "uninstall",
    "format",
    "empty",
    "sign out",
    "log out",
    "logout",
    "replace all",
    "shut",
    "restart",
    "trash",
];

pub fn is_risky(name: &str) -> bool {
    let n = name.to_lowercase();
    RISKY.iter().any(|r| n.contains(r))
}

const STOP: &[&str] = &[
    "il", "lo", "la", "i", "gli", "le", "un", "uno", "una", "di", "del", "della", "dello", "dei",
    "delle", "da", "dal", "a", "al", "alla", "allo", "ai", "in", "nel", "nella", "su", "sul",
    "sulla", "per", "con", "e", "ed", "o", "mi", "me", "ti", "puoi", "fai", "fammi", "dai", "the",
    "an", "of", "to", "on", "for", "and", "or", "my", "me", "please", "can", "you", "app",
];

fn fold(c: char) -> char {
    match c {
        'à' | 'á' | 'â' => 'a',
        'è' | 'é' | 'ê' => 'e',
        'ì' | 'í' => 'i',
        'ò' | 'ó' | 'ô' => 'o',
        'ù' | 'ú' => 'u',
        c => c,
    }
}

fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .chars()
        .map(fold)
        .collect::<String>()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty() && !STOP.contains(t))
        .map(str::to_string)
        .collect()
}

/// Same word, or the same root ("salvare" / "salva", "chiudere" / "chiudi").
fn same_word(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let common = a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
    let shorter = a.chars().count().min(b.chars().count());
    shorter >= 4 && common >= 4.max(shorter.saturating_sub(2))
}

/// How well the spoken words name this command: F1 of the words they share
/// (the command's words found in the speech, and the speech's words that
/// belong to the command). 0..=1.
pub fn score(spoken: &str, name: &str) -> f32 {
    let (s, n) = (tokens(spoken), tokens(name));
    if s.is_empty() || n.is_empty() {
        return 0.0;
    }
    let found = n
        .iter()
        .filter(|t| s.iter().any(|w| same_word(w, t)))
        .count() as f32;
    let used = s
        .iter()
        .filter(|w| n.iter().any(|t| same_word(w, t)))
        .count() as f32;
    let (recall, precision) = (found / n.len() as f32, used / s.len() as f32);
    if recall + precision == 0.0 {
        0.0
    } else {
        2.0 * recall * precision / (recall + precision)
    }
}

/// A clear match: high, and clearly ahead of the next different command.
const RUN_SCORE: f32 = 0.85;
const RUN_LEAD: f32 = 0.15;

fn ranked(spoken: &str, cmds: &[AppCommand]) -> Vec<(f32, AppCommand)> {
    let mut v: Vec<(f32, AppCommand)> = cmds
        .iter()
        .map(|c| (score(spoken, &c.name), c.clone()))
        .filter(|(s, _)| *s > 0.0)
        .collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0));
    v
}

/// What the app in front offers for these words.
pub enum Match {
    /// Clear and safe: run it.
    Run(AppCommand),
    /// Clear, but its name says it is dangerous.
    Refused(AppCommand),
    None,
}

pub fn best(spoken: &str) -> Match {
    let g = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    let Some(snap) = g.as_ref() else {
        return Match::None;
    };
    let r = ranked(spoken, &snap.cmds);
    let Some((top, cmd)) = r.first() else {
        return Match::None;
    };
    let next = r
        .iter()
        .find(|(_, c)| c.name.to_lowercase() != cmd.name.to_lowercase())
        .map(|(s, _)| *s)
        .unwrap_or(0.0);
    if *top < RUN_SCORE || top - next < RUN_LEAD {
        return Match::None;
    }
    if cmd.risky {
        Match::Refused(cmd.clone())
    } else {
        Match::Run(cmd.clone())
    }
}

/// The safe commands most like the words, for Claude to choose from.
pub fn candidates(spoken: &str, n: usize) -> (String, Vec<String>) {
    let g = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    let Some(snap) = g.as_ref() else {
        return (String::new(), Vec::new());
    };
    let labels = ranked(spoken, &snap.cmds)
        .into_iter()
        .filter(|(_, c)| !c.risky)
        .take(n)
        .map(|(_, c)| {
            if c.shortcut.is_empty() {
                c.label
            } else {
                format!("{} ({})", c.label, c.shortcut)
            }
        })
        .collect();
    (snap.process.clone(), labels)
}

/// The front app and a few of its safe commands, short names first, to show
/// as examples of what can be said.
pub fn examples(n: usize) -> Option<(String, Vec<String>)> {
    let g = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    let snap = g.as_ref()?;
    let names = examples_of(&snap.cmds, n);
    (!names.is_empty()).then(|| (snap.process.clone(), names))
}

fn examples_of(cmds: &[AppCommand], n: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for words in 1..=3 {
        for c in cmds.iter().filter(|c| !c.risky) {
            let name = c.name.trim();
            if out.len() < n
                && name.split_whitespace().count() == words
                && name.chars().any(char::is_alphabetic)
                && !out.iter().any(|o| o.eq_ignore_ascii_case(name))
            {
                out.push(name.to_string());
            }
        }
    }
    out
}

pub fn find(label: &str) -> Option<AppCommand> {
    let g = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    let snap = g.as_ref()?;
    let want = label.split(" (").next().unwrap_or(label).trim();
    snap.cmds
        .iter()
        .find(|c| c.label == want || c.name == want)
        .cloned()
}

fn split_label(raw: &str) -> (String, String) {
    let mut it = raw.splitn(2, '\t');
    let name = it.next().unwrap_or("").replace('&', "").trim().to_string();
    let shortcut = it.next().unwrap_or("").trim().to_string();
    (name, shortcut)
}

#[cfg(target_os = "windows")]
mod win {
    use super::{is_risky, split_label, AppCommand, Snapshot, Target, SNAPSHOT};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::{
        CUIAutomation, IUIAutomation, IUIAutomationInvokePattern,
        IUIAutomationLegacyIAccessiblePattern, TreeScope_Descendants, UIA_ButtonControlTypeId,
        UIA_ControlTypePropertyId, UIA_HyperlinkControlTypeId, UIA_InvokePatternId,
        UIA_IsInvokePatternAvailablePropertyId, UIA_LegacyIAccessiblePatternId,
        UIA_MenuItemControlTypeId, UIA_NamePropertyId, UIA_SplitButtonControlTypeId,
        UIA_TabItemControlTypeId,
    };

    #[link(name = "user32")]
    extern "system" {
        fn GetForegroundWindow() -> isize;
        pub(super) fn GetMenu(h: isize) -> isize;
        fn GetMenuItemCount(m: isize) -> i32;
        fn GetSubMenu(m: isize, pos: i32) -> isize;
        fn GetMenuStringW(m: isize, item: u32, buf: *mut u16, max: i32, flags: u32) -> i32;
        fn GetMenuItemID(m: isize, pos: i32) -> u32;
        fn GetMenuState(m: isize, item: u32, flags: u32) -> u32;
        fn PostMessageW(h: isize, msg: u32, w: usize, l: isize) -> i32;
        fn SetForegroundWindow(h: isize) -> i32;
        fn GetWindowThreadProcessId(h: isize, pid: *mut u32) -> u32;
    }

    const MF_BYPOSITION: u32 = 0x400;
    const MAX_ITEMS: usize = 800;

    pub(super) fn walk(
        menu: isize,
        hwnd: isize,
        path: &str,
        depth: u32,
        out: &mut Vec<AppCommand>,
    ) {
        if menu == 0 || depth > 4 || out.len() >= MAX_ITEMS {
            return;
        }
        let n = unsafe { GetMenuItemCount(menu) };
        for pos in 0..n.max(0) {
            let state = unsafe { GetMenuState(menu, pos as u32, MF_BYPOSITION) };
            if state & 0x800 != 0 || state & 0x3 != 0 {
                continue; // separator, or greyed / disabled
            }
            let mut buf = [0u16; 256];
            let len =
                unsafe { GetMenuStringW(menu, pos as u32, buf.as_mut_ptr(), 256, MF_BYPOSITION) };
            if len <= 0 {
                continue;
            }
            let (name, shortcut) = split_label(&String::from_utf16_lossy(&buf[..len as usize]));
            if name.is_empty() {
                continue;
            }
            let sub = unsafe { GetSubMenu(menu, pos) };
            if sub != 0 {
                walk(sub, hwnd, &format!("{path}{name} > "), depth + 1, out);
                continue;
            }
            let id = unsafe { GetMenuItemID(menu, pos) };
            if id == u32::MAX || id == 0 {
                continue;
            }
            out.push(AppCommand {
                label: format!("{path}{name}"),
                risky: is_risky(&name),
                name,
                shortcut,
                target: Target::Menu { hwnd, id },
            });
        }
    }

    fn automation() -> Result<IUIAutomation, String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())
        }
    }

    pub(super) fn controls(hwnd: isize, out: &mut Vec<AppCommand>) -> Result<(), String> {
        let ua = automation()?;
        unsafe {
            let root = ua
                .ElementFromHandle(HWND(hwnd as _))
                .map_err(|e| e.to_string())?;
            // Invokable controls, plus the kinds a person names and clicks
            // that act through other patterns (Chrome's "Extensions" button,
            // bookmarks, tabs): those run through their default action.
            let mut cond = ua
                .CreatePropertyCondition(
                    UIA_IsInvokePatternAvailablePropertyId,
                    &VARIANT::from(true),
                )
                .map_err(|e| e.to_string())?;
            for kind in [
                UIA_ButtonControlTypeId,
                UIA_HyperlinkControlTypeId,
                UIA_MenuItemControlTypeId,
                UIA_TabItemControlTypeId,
                UIA_SplitButtonControlTypeId,
            ] {
                let is_kind = ua
                    .CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(kind.0))
                    .map_err(|e| e.to_string())?;
                cond = ua
                    .CreateOrCondition(&cond, &is_kind)
                    .map_err(|e| e.to_string())?;
            }
            let all = root
                .FindAll(TreeScope_Descendants, &cond)
                .map_err(|e| e.to_string())?;
            let n = all.Length().unwrap_or(0);
            let mut seen = std::collections::HashSet::new();
            for i in 0..n {
                if out.len() >= MAX_ITEMS {
                    break;
                }
                let Ok(el) = all.GetElement(i) else { continue };
                let Ok(name) = el.CurrentName() else { continue };
                let name = name.to_string().trim().to_string();
                if name.is_empty() || name.chars().count() > 60 || !seen.insert(name.to_lowercase())
                {
                    continue;
                }
                out.push(AppCommand {
                    label: name.clone(),
                    risky: is_risky(&name),
                    name,
                    shortcut: String::new(),
                    target: Target::Control { hwnd },
                });
            }
        }
        Ok(())
    }

    fn process_of(hwnd: isize) -> String {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        sysinfo_name(pid)
    }

    fn sysinfo_name(pid: u32) -> String {
        crate::pc_control::process_name_of(pid)
    }

    /// Read the commands of the window in front, off the caller thread.
    pub fn capture_foreground() {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd == 0 {
            return;
        }
        std::thread::spawn(move || {
            let t = std::time::Instant::now();
            let process = process_of(hwnd);
            if process.eq_ignore_ascii_case("Dimmy.Windows") {
                return;
            }
            let mut cmds = Vec::new();
            walk(unsafe { GetMenu(hwnd) }, hwnd, "", 0, &mut cmds);
            let source = if cmds.is_empty() {
                if let Err(e) = controls(hwnd, &mut cmds) {
                    crate::log(&format!("[PcAppCmds] UI Automation failed: {e}"));
                }
                "controls"
            } else {
                "menu"
            };
            crate::log(&format!(
                "[PcAppCmds] {process}: {} commands from its {source} in {} ms ({} risky)",
                cmds.len(),
                t.elapsed().as_millis(),
                cmds.iter().filter(|c| c.risky).count()
            ));
            *SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Snapshot { process, cmds });
        });
    }

    pub fn run(cmd: &AppCommand) -> Result<(), String> {
        assert!(!cmd.risky, "a risky command never reaches run()");
        match cmd.target {
            Target::Menu { hwnd, id } => unsafe {
                SetForegroundWindow(hwnd);
                if PostMessageW(hwnd, 0x0111, id as usize, 0) == 0 {
                    return Err(format!("could not send \"{}\"", cmd.label));
                }
                Ok(())
            },
            Target::Control { hwnd } => {
                let ua = automation()?;
                unsafe {
                    let root = ua
                        .ElementFromHandle(HWND(hwnd as _))
                        .map_err(|e| e.to_string())?;
                    let by_name = ua
                        .CreatePropertyCondition(
                            UIA_NamePropertyId,
                            &VARIANT::from(cmd.name.as_str()),
                        )
                        .map_err(|e| e.to_string())?;
                    let el = root
                        .FindFirst(TreeScope_Descendants, &by_name)
                        .map_err(|_| format!("\"{}\" is no longer there", cmd.name))?;
                    SetForegroundWindow(hwnd);
                    if let Ok(inv) =
                        el.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)
                    {
                        return inv.Invoke().map_err(|e| e.to_string());
                    }
                    // No Invoke: the control's default action, as a screen reader does it.
                    let legacy: IUIAutomationLegacyIAccessiblePattern = el
                        .GetCurrentPatternAs(UIA_LegacyIAccessiblePatternId)
                        .map_err(|e| e.to_string())?;
                    legacy.DoDefaultAction().map_err(|e| e.to_string())
                }
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub use win::{capture_foreground, run};

#[cfg(not(target_os = "windows"))]
pub fn capture_foreground() {}

#[cfg(not(target_os = "windows"))]
pub fn run(_cmd: &AppCommand) -> Result<(), String> {
    Err("app commands are Windows-only in this experiment".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spoken_words_find_the_menu_item() {
        assert!(score("salva tutto", "Salva tutto") > 0.99);
        assert!(score("salvare tutto", "Salva tutto") > 0.99, "same root");
        assert!(
            score(
                "chiudi le schede a destra",
                "Chiudi le schede a destra della corrente"
            ) > 0.85
        );
        assert!(
            score("apri Teams", "Apri...") < 0.85,
            "one shared verb is not a match"
        );
        assert_eq!(score("stampa", "Copia"), 0.0);
    }

    #[test]
    fn dangerous_names_are_flagged() {
        assert!(is_risky("Sposta nel cestino"));
        assert!(is_risky("Esci"));
        assert!(is_risky("Chiudi tutto"));
        assert!(is_risky("Send"));
        assert!(!is_risky("Salva tutto"));
        assert!(!is_risky("Nuova scheda"));
    }

    #[test]
    fn a_menu_label_splits_into_name_and_shortcut() {
        assert_eq!(
            split_label("&Salva tutto\tCtrl+Shift+S"),
            ("Salva tutto".to_string(), "Ctrl+Shift+S".to_string())
        );
        assert_eq!(
            split_label("Stampa subito!"),
            ("Stampa subito!".to_string(), String::new())
        );
    }

    #[test]
    fn the_examples_are_safe_short_and_unique() {
        let mk = |name: &str| AppCommand {
            label: format!("Menu > {name}"),
            name: name.into(),
            shortcut: String::new(),
            risky: is_risky(name),
            target: Target::Control { hwnd: 0 },
        };
        let cmds: Vec<AppCommand> = [
            "Elimina",
            "Salva tutto",
            "Trova",
            "trova",
            "…",
            "Mostra la barra degli strumenti",
            "Stampa",
        ]
        .iter()
        .map(|n| mk(n))
        .collect();
        assert_eq!(
            examples_of(&cmds, 3),
            vec!["Trova".to_string(), "Stampa".into(), "Salva tutto".into()]
        );
    }

    #[test]
    fn a_clear_lead_is_needed_to_run() {
        let mk = |name: &str| AppCommand {
            label: name.into(),
            name: name.into(),
            shortcut: String::new(),
            risky: is_risky(name),
            target: Target::Control { hwnd: 0 },
        };
        *SNAPSHOT.lock().unwrap() = Some(Snapshot {
            process: "test".into(),
            cmds: vec![
                mk("Salva tutto"),
                mk("Salva"),
                mk("Sposta nel cestino"),
                mk("Stampa"),
            ],
        });
        assert!(matches!(best("salva tutto"), Match::Run(c) if c.name == "Salva tutto"));
        assert!(matches!(best("sposta nel cestino"), Match::Refused(_)));
        assert!(matches!(best("apri il meteo"), Match::None));
        let (proc_name, c) = candidates("salva", 5);
        assert_eq!(proc_name, "test");
        assert!(
            !c.iter().any(|l| l.contains("cestino")),
            "risky commands never reach Claude"
        );
        *SNAPSHOT.lock().unwrap() = None;
    }

    /// Read-only check on the real desktop (runs nothing):
    /// `cargo test --lib --features pc-router real_apps -- --ignored --nocapture`
    #[cfg(target_os = "windows")]
    #[test]
    #[ignore]
    fn real_apps_expose_their_commands() {
        #[link(name = "user32")]
        extern "system" {
            fn FindWindowW(class: *const u16, title: *const u16) -> isize;
        }
        let find = |class: &str| {
            let w: Vec<u16> = class.encode_utf16().chain(Some(0)).collect();
            unsafe { FindWindowW(w.as_ptr(), std::ptr::null()) }
        };
        let npp = find("Notepad++");
        if npp != 0 {
            let t = std::time::Instant::now();
            let mut cmds = Vec::new();
            win::walk(unsafe { win::GetMenu(npp) }, npp, "", 0, &mut cmds);
            println!(
                "Notepad++: {} menu commands in {} ms, {} risky",
                cmds.len(),
                t.elapsed().as_millis(),
                cmds.iter().filter(|c| c.risky).count()
            );
            for phrase in [
                "salva tutto",
                "chiudi le schede a destra",
                "stampa",
                "nuovo file",
                "sposta nel cestino",
                "maiuscolo",
                "vai alla riga",
            ] {
                let r = ranked(phrase, &cmds);
                let top: Vec<String> = r
                    .iter()
                    .take(2)
                    .map(|(s, c)| {
                        format!(
                            "{:.2} {}{}",
                            s,
                            c.label,
                            if c.risky { " [RISKY]" } else { "" }
                        )
                    })
                    .collect();
                println!("  {phrase:28} -> {}", top.join("  |  "));
            }
        }
        // Chrome has many hidden windows of this class: take a visible one with a title.
        #[link(name = "user32")]
        extern "system" {
            fn FindWindowExW(
                parent: isize,
                after: isize,
                class: *const u16,
                title: *const u16,
            ) -> isize;
            fn IsWindowVisible(h: isize) -> i32;
            fn GetWindowTextLengthW(h: isize) -> i32;
        }
        let class: Vec<u16> = "Chrome_WidgetWin_1".encode_utf16().chain(Some(0)).collect();
        let mut chrome = 0isize;
        loop {
            chrome = unsafe { FindWindowExW(0, chrome, class.as_ptr(), std::ptr::null()) };
            if chrome == 0
                || (unsafe { IsWindowVisible(chrome) } != 0
                    && unsafe { GetWindowTextLengthW(chrome) } > 0)
            {
                break;
            }
        }
        if chrome != 0 {
            let t = std::time::Instant::now();
            let mut cmds = Vec::new();
            let _ = win::controls(chrome, &mut cmds);
            println!(
                "Chrome: {} controls in {} ms",
                cmds.len(),
                t.elapsed().as_millis()
            );
            for phrase in [
                "aggiungi ai preferiti",
                "estensioni",
                "ricarica",
                "indietro",
                "home",
            ] {
                let r = ranked(phrase, &cmds);
                let top: Vec<String> = r
                    .iter()
                    .take(2)
                    .map(|(s, c)| format!("{:.2} {}", s, c.label))
                    .collect();
                println!("  {phrase:28} -> {}", top.join("  |  "));
            }
        }
    }
}
