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

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[derive(Clone, Debug, PartialEq)]
enum Target {
    Menu {
        hwnd: isize,
        id: u32,
    },
    /// `raw` is the name exactly as UI Automation gave it: the control is
    /// found again by it, and a trimmed name would not match.
    Control {
        hwnd: isize,
        raw: String,
    },
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
struct Snapshot {
    hwnd: isize,
    /// The app as a person names it ("Notepad++", "Word").
    app: String,
    cmds: Vec<AppCommand>,
    /// What the window shows, in reading order (`pc_screen`). Empty for an
    /// app read through its menu.
    items: Vec<crate::pc_screen::Item>,
}

/// The items the last pointer or planner listing was made from: a `click`
/// step names one by its place in THIS list, whatever was captured since.
static LISTED: Mutex<Vec<crate::pc_screen::Item>> = Mutex::new(Vec::new());

fn list(items: &[crate::pc_screen::Item]) {
    *LISTED.lock().unwrap_or_else(|e| e.into_inner()) = items.to_vec();
}

/// What the words point at in the window in front ("il primo video", "il
/// video di ...", a title read aloud), as a step. None when they point at
/// nothing on the screen.
pub fn point(spoken: &str) -> Option<(crate::pc_control::Step, String)> {
    with_front(|snap| {
        let items = &snap?.items;
        match crate::pc_screen::point(spoken, items)? {
            crate::pc_screen::Pointed::Item(k) => {
                list(items);
                let label: String = items[k].text.chars().take(60).collect();
                let double = crate::pc_screen::opens_with_two_clicks(spoken, &items[k]);
                let step = crate::pc_control::Step::Click {
                    item: k + 1,
                    double,
                };
                Some((step, label))
            }
            crate::pc_screen::Pointed::Keys(action) => Some((
                crate::pc_control::Step::Key {
                    app: String::new(),
                    action: action.clone(),
                },
                action,
            )),
        }
    })
}

/// What the window in front shows, numbered, for the planner: a `click`
/// step answers with a number of this list.
pub fn screen_lines(max: usize) -> (String, Vec<String>) {
    with_front(|snap| match snap {
        Some(snap) if !snap.items.is_empty() => {
            list(&snap.items);
            (snap.app.clone(), crate::pc_screen::lines(&snap.items, max))
        }
        _ => (String::new(), Vec::new()),
    })
}

/// Click the item with this number (from 1) in the list last shown.
pub fn click_listed(number: usize, double: bool) -> Result<String, String> {
    let item = LISTED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(number.wrapping_sub(1))
        .cloned()
        .ok_or_else(|| format!("non c'è un elemento numero {number} a schermo"))?;
    click(&item, double)?;
    Ok(item.text.chars().take(60).collect())
}

static SNAPSHOT: Mutex<Option<Snapshot>> = Mutex::new(None);

/// A word of the name that starts with one of these.
const RISKY_STEMS: &[&str] = &[
    "elimin",
    "cancell",
    "cestino",
    "chiud",
    "inoltra",
    "compra",
    "acquist",
    "paga",
    "disinstall",
    "svuota",
    "disconnett",
    "rimuov",
    "spegn",
    "riavvi",
    "sovrascriv",
    "abbandon",
    "riagganc",
    "reimpost",
    "delete",
    "remove",
    "uninstall",
    "purchase",
    "pay",
    "buy",
    "empty",
    "logout",
    "logoff",
    "restart",
    "trash",
    "discard",
    "overwrit",
    "checkout",
];

/// A whole word of the name. A stem here would refuse harmless names:
/// "invia" is in "Posta inviata", "termina" in "Terminale", "close" in
/// "Closed captions", "post" in "Posta".
const RISKY_WORDS: &[&str] = &[
    "esci",
    "uscire",
    "invia",
    "inviare",
    "formatta",
    "formattare",
    "pubblica",
    "pubblicare",
    "pulisci",
    "ripristina",
    "ripristinare",
    "scarta",
    "termina",
    "terminare",
    "rispondi",
    "exit",
    "quit",
    "close",
    "send",
    "shut",
    "shutdown",
    "clear",
    "reset",
    "leave",
    "stop",
    "post",
    "publish",
    "reply",
];

/// Two words in a row; the second may go on ("sostituisci tutto / tutti").
const RISKY_PHRASES: &[&str] = &[
    "sostituisci tutt",
    "conferma ordine",
    "ordina ora",
    "sign out",
    "sign off",
    "log out",
    "log off",
    "replace all",
    "hang up",
    "place order",
    "format disk",
    "format drive",
];

/// `name` is a command name or its whole menu path ("Elimina > Riga").
pub fn is_risky(name: &str) -> bool {
    let w = words(name);
    let line = format!(" {}", w.join(" "));
    w.iter()
        .any(|t| RISKY_WORDS.contains(&t.as_str()) || RISKY_STEMS.iter().any(|r| t.starts_with(r)))
        || RISKY_PHRASES
            .iter()
            .any(|p| line.contains(&format!(" {p}")))
}

const STOP: &[&str] = &[
    "il", "lo", "la", "i", "gli", "le", "un", "uno", "una", "di", "del", "della", "dello", "dei",
    "delle", "da", "dal", "a", "al", "alla", "allo", "ai", "in", "nel", "nella", "su", "sul",
    "sulla", "per", "con", "e", "ed", "o", "mi", "me", "ti", "puoi", "fai", "fammi", "dai", "the",
    "an", "of", "to", "on", "for", "and", "or", "my", "me", "please", "can", "you", "app",
];

/// What a person says around the name of a control: "clicca su Salva",
/// "premi il pulsante Invia". Taken off the spoken side only, never off a
/// command name ("Apri...", "Vai alla riga").
const CARRIER_VERBS: &[&str] = &[
    "clicca",
    "click",
    "premi",
    "press",
    "tocca",
    "seleziona",
    "select",
    "scegli",
    "vai",
    "go",
    "apri",
    "open",
];
const CARRIER_NOUNS: &[&str] = &[
    "pulsante", "bottone", "tasto", "button", "menu", "voce", "link", "scheda",
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

fn words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .chars()
        .map(fold)
        .collect::<String>()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

fn tokens(s: &str) -> Vec<String> {
    let mut w = words(s);
    w.retain(|t| !STOP.contains(&t.as_str()));
    w
}

/// The name without what is in parentheses ("Ricarica (Ctrl+R)"), unless
/// nothing else is left.
fn bare(name: &str) -> String {
    let mut out = String::new();
    let mut depth = 0u32;
    for c in name.chars() {
        match c {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            c if depth == 0 => out.push(c),
            _ => {}
        }
    }
    if tokens(&out).is_empty() {
        name.to_string()
    } else {
        out
    }
}

/// The spoken words without the carrier around them; all of them when
/// nothing else was said ("apri" alone names "Apri...").
fn uncarried(s: &[String]) -> &[String] {
    let is = |list: &[&str], t: &String| list.contains(&t.as_str());
    let lead = s
        .iter()
        .take_while(|t| is(CARRIER_VERBS, t) || is(CARRIER_NOUNS, t))
        .count();
    if lead == s.len() {
        return s;
    }
    let tail = s.iter().rev().take_while(|t| is(CARRIER_NOUNS, t)).count();
    assert!(lead + tail < s.len());
    &s[lead..s.len() - tail]
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

/// F1 of the words they share: the command's words found in the speech, and
/// the speech's words that belong to the command. 0..=1.
fn f1(s: &[String], n: &[String]) -> f32 {
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
    let f = if recall + precision == 0.0 {
        0.0
    } else {
        2.0 * recall * precision / (recall + precision)
    };
    assert!((0.0..=1.0).contains(&f));
    f
}

/// The words exactly as said against the name.
fn score_as_said(spoken: &str, name: &str) -> f32 {
    f1(&tokens(spoken), &tokens(&bare(name)))
}

/// How well the spoken words name this command, with or without the carrier
/// around the name ("clicca su Salva"): the better of the two. 0..=1.
pub fn score(spoken: &str, name: &str) -> f32 {
    let (s, n) = (tokens(spoken), tokens(&bare(name)));
    f1(&s, &n).max(f1(uncarried(&s), &n))
}

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
#[derive(Debug, PartialEq)]
enum Freshness {
    Use,
    Recapture,
    Nothing,
}

/// May the snapshot answer for the window in front? Dimmy's own windows (the
/// pill, the help) do not count as "in front": the commands are for the app
/// behind them.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn freshness(snapshot_hwnd: Option<isize>, front: isize, front_is_dimmy: bool) -> Freshness {
    if front == 0 {
        Freshness::Nothing
    } else if front_is_dimmy {
        match snapshot_hwnd {
            Some(_) => Freshness::Use,
            None => Freshness::Nothing,
        }
    } else if snapshot_hwnd == Some(front) {
        Freshness::Use
    } else {
        Freshness::Recapture
    }
}

/// The snapshot, only when it is of the window in front (read again first
/// when it is not).
fn with_front<R>(f: impl FnOnce(Option<&Snapshot>) -> R) -> R {
    let ready = front_ready();
    let g = SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner());
    f(g.as_ref().filter(|_| ready))
}

#[cfg(target_os = "windows")]
use win::front_ready;

#[cfg(not(target_os = "windows"))]
fn front_ready() -> bool {
    true
}

/// A clear match: high, and clearly ahead of the next different command.
const RUN_SCORE: f32 = 0.85;
const RUN_LEAD: f32 = 0.15;

fn ranked_by(
    spoken: &str,
    cmds: &[AppCommand],
    score: fn(&str, &str) -> f32,
) -> Vec<(f32, AppCommand)> {
    let mut v: Vec<(f32, AppCommand)> = cmds
        .iter()
        .map(|c| (score(spoken, &c.name), c.clone()))
        .filter(|(s, _)| *s > 0.0)
        .collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0));
    v
}

fn ranked(spoken: &str, cmds: &[AppCommand]) -> Vec<(f32, AppCommand)> {
    ranked_by(spoken, cmds, score)
}

fn clear(r: &[(f32, AppCommand)]) -> Option<&AppCommand> {
    let (top, cmd) = r.first()?;
    let next = r
        .iter()
        .find(|(_, c)| c.name.to_lowercase() != cmd.name.to_lowercase())
        .map(|(s, _)| *s)
        .unwrap_or(0.0);
    (*top >= RUN_SCORE && top - next >= RUN_LEAD).then_some(cmd)
}

/// What the app in front offers for these words.
pub enum Match {
    /// Clear and safe: run it.
    Run(AppCommand),
    /// Clear, but its name says it is dangerous.
    Refused(AppCommand),
    None,
}

/// The words as said go first: with "Chiudi" and "Chiudi scheda" both there,
/// "chiudi scheda" without its carrier noun would name both.
fn best_of(spoken: &str, cmds: &[AppCommand]) -> Match {
    let as_said = ranked_by(spoken, cmds, score_as_said);
    let carried = ranked(spoken, cmds);
    let Some(cmd) = clear(&as_said).or_else(|| clear(&carried)) else {
        return Match::None;
    };
    if cmd.risky {
        Match::Refused(cmd.clone())
    } else {
        Match::Run(cmd.clone())
    }
}

pub fn best(spoken: &str) -> Match {
    with_front(|snap| match snap {
        Some(snap) => best_of(spoken, &snap.cmds),
        None => Match::None,
    })
}

fn candidates_of(spoken: &str, cmds: &[AppCommand], n: usize) -> Vec<String> {
    ranked(spoken, cmds)
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
        .collect()
}

/// The safe commands most like the words, for Claude to choose from.
pub fn candidates(spoken: &str, n: usize) -> (String, Vec<String>) {
    with_front(|snap| match snap {
        Some(snap) => (snap.app.clone(), candidates_of(spoken, &snap.cmds, n)),
        None => (String::new(), Vec::new()),
    })
}

/// The safe commands of the window in front in the order the app lists them
/// (page order for a web page): what "il terzo video" counts in.
pub fn listing(n: usize) -> (String, Vec<String>) {
    with_front(|snap| match snap {
        Some(snap) => (
            snap.app.clone(),
            snap.cmds
                .iter()
                .filter(|c| !c.risky)
                .take(n)
                .map(|c| c.label.clone())
                .collect(),
        ),
        None => (String::new(), Vec::new()),
    })
}

/// The front app and a few of its safe commands, short names first, to show
/// as examples of what can be said.
pub fn examples(n: usize) -> Option<(String, Vec<String>)> {
    with_front(|snap| {
        let snap = snap?;
        let names = examples_of(&snap.cmds, n);
        (!names.is_empty()).then(|| (snap.app.clone(), names))
    })
}

/// A top menu ("File", or "" for the buttons) and the names under it.
pub type Menu = (String, Vec<String>);

/// Every safe command of the window in front, by menu: the full list
/// "cosa posso dire qui?" shows.
pub fn groups() -> Option<(String, Vec<Menu>)> {
    with_front(|snap| {
        let snap = snap?;
        let groups = groups_of(&snap.cmds);
        (!groups.is_empty()).then(|| (snap.app.clone(), groups))
    })
}

/// Grouped by the top menu ("File", "Modifica"), in the app's order; a
/// control has no menu and goes in the group "". No name twice in a group.
fn groups_of(cmds: &[AppCommand]) -> Vec<Menu> {
    let mut out: Vec<Menu> = Vec::new();
    for c in cmds.iter().filter(|c| !c.risky) {
        let name = c.name.trim();
        if !name.chars().any(char::is_alphabetic) {
            continue;
        }
        let top = match c.label.split_once(" > ") {
            Some((top, _)) => top.trim().to_string(),
            None => String::new(),
        };
        let i = match out.iter().position(|(g, _)| *g == top) {
            Some(i) => i,
            None => {
                out.push((top, Vec::new()));
                out.len() - 1
            }
        };
        let names = &mut out[i].1;
        if !names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            names.push(name.to_string());
        }
    }
    assert!(out.iter().all(|(_, n)| !n.is_empty()));
    out
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
    // `label` is as listed to the planner: the shortcut may follow in
    // parentheses, and a control's own name may hold parentheses too.
    let whole = label.trim();
    let want = label.split(" (").next().unwrap_or(label).trim();
    with_front(|snap| {
        let cmds = &snap?.cmds;
        cmds.iter()
            .find(|c| c.label == whole || c.label == want || c.name == want)
            // A long title may come back cut short: its start is enough.
            .or_else(|| {
                cmds.iter().find(|c| {
                    want.chars().count() >= 12
                        && (c.label.starts_with(want) || c.name.starts_with(want))
                })
            })
            .cloned()
    })
}

/// Longest text of an error or a log line that came from outside.
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
const TEXT_MAX: usize = 200;

#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn split_label(raw: &str) -> (String, String) {
    let mut it = raw.splitn(2, '\t');
    let name = it.next().unwrap_or("").replace('&', "").trim().to_string();
    let shortcut = it.next().unwrap_or("").trim().to_string();
    (name, shortcut)
}

#[cfg(target_os = "windows")]
mod win {
    use super::{freshness, is_risky, split_label, AppCommand, Freshness, Snapshot, Target};
    use super::{SNAPSHOT, TEXT_MAX};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use windows::core::Interface;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Variant::VARIANT;
    use windows::Win32::UI::Accessibility::{
        AutomationElementMode_None, CUIAutomation, CUIAutomation8, IUIAutomation, IUIAutomation2,
        IUIAutomationCondition, IUIAutomationElement, IUIAutomationExpandCollapsePattern,
        IUIAutomationInvokePattern, IUIAutomationLegacyIAccessiblePattern,
        IUIAutomationSelectionItemPattern, IUIAutomationTogglePattern, TreeScope_Descendants,
        UIA_BoundingRectanglePropertyId, UIA_ButtonControlTypeId, UIA_CheckBoxControlTypeId,
        UIA_ComboBoxControlTypeId, UIA_ControlTypePropertyId, UIA_DataItemControlTypeId,
        UIA_ExpandCollapsePatternId, UIA_HyperlinkControlTypeId, UIA_InvokePatternId,
        UIA_IsEnabledPropertyId, UIA_IsInvokePatternAvailablePropertyId, UIA_IsOffscreenPropertyId,
        UIA_LegacyIAccessiblePatternId, UIA_ListItemControlTypeId, UIA_MenuItemControlTypeId,
        UIA_NamePropertyId, UIA_RadioButtonControlTypeId, UIA_SelectionItemPatternId,
        UIA_SplitButtonControlTypeId, UIA_TabItemControlTypeId, UIA_TogglePatternId,
        UIA_TreeItemControlTypeId,
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
        fn GetWindowTextW(h: isize, buf: *mut u16, max: i32) -> i32;
        fn IsWindow(h: isize) -> i32;
        fn GetWindowRect(h: isize, rect: *mut Rect4) -> i32;
        fn SetCursorPos(x: i32, y: i32) -> i32;
        fn SetThreadDpiAwarenessContext(context: isize) -> isize;
        fn mouse_event(flags: u32, dx: u32, dy: u32, data: u32, extra: usize);
        fn GetClassNameW(h: isize, buf: *mut u16, max: i32) -> i32;
        fn EnumChildWindows(
            parent: isize,
            cb: unsafe extern "system" fn(isize, isize) -> i32,
            lp: isize,
        ) -> i32;
        fn SendMessageTimeoutW(
            h: isize,
            msg: u32,
            w: usize,
            l: isize,
            flags: u32,
            timeout_ms: u32,
            result: *mut usize,
        ) -> isize;
    }

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    pub(super) struct Rect4 {
        l: i32,
        t: i32,
        r: i32,
        b: i32,
    }

    /// While this lives, the thread measures the screen in real pixels,
    /// whatever the host process declared. UI Automation hands rectangles
    /// to a DPI-aware caller in pixels and to an unaware one scaled; a
    /// window's own rectangle and the mouse must be in the same unit, or at
    /// 150% scaling most of a page counts as outside its window.
    struct RealPixels(isize);

    impl RealPixels {
        fn new() -> Self {
            // DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2
            Self(unsafe { SetThreadDpiAwarenessContext(-4) })
        }
    }

    impl Drop for RealPixels {
        fn drop(&mut self) {
            if self.0 != 0 {
                unsafe { SetThreadDpiAwarenessContext(self.0) };
            }
        }
    }

    /// A real click in the middle of the item: what the user's hand would
    /// do, so it works on everything that is on the screen.
    pub fn click(item: &crate::pc_screen::Item, double: bool) -> Result<(), String> {
        let _pixels = RealPixels::new();
        let front = unsafe { GetForegroundWindow() };
        let mut window = Rect4::default();
        unsafe { GetWindowRect(front, &mut window) };
        let (x, y) = item.center();
        if x <= window.l || x >= window.r || y <= window.t || y >= window.b {
            return Err(short(format!(
                "\"{}\" non è più a schermo",
                item.text.chars().take(40).collect::<String>()
            )));
        }
        unsafe {
            SetCursorPos(x, y);
            std::thread::sleep(Duration::from_millis(30));
            for _ in 0..if double { 2 } else { 1 } {
                mouse_event(0x0002, 0, 0, 0, 0); // MOUSEEVENTF_LEFTDOWN
                mouse_event(0x0004, 0, 0, 0, 0); // MOUSEEVENTF_LEFTUP
                std::thread::sleep(Duration::from_millis(40));
            }
        }
        Ok(())
    }

    const MF_BYPOSITION: u32 = 0x400;
    const MAX_ITEMS: usize = 800;

    // One call into another process: a hung app costs this much, not the
    // 20 s UI Automation waits by default.
    const UIA_CONNECT_MS: u32 = 500;
    const UIA_CALL_MS: u32 = 1500;
    const UIA_E_TIMEOUT: i32 = 0x8013_1505_u32 as i32;
    /// Finding the control again is at most three such calls.
    const FIND_WAIT: Duration = Duration::from_millis(4000);
    /// A control that opens a modal dialog answers only when the dialog
    /// closes: past this, the action counts as started.
    const ACT_WAIT: Duration = Duration::from_millis(1500);

    /// A web app in a Chromium window (WebView2, Electron) builds its
    /// accessibility tree only once something asks for it: this few controls
    /// in a window that holds a web view means it was not asked yet.
    const FEW_CONTROLS: usize = 10;
    const WAKE_MAX_VIEWS: u32 = 4;
    const WAKE_SEND_MS: u32 = 200;
    const WAKE_SETTLE: Duration = Duration::from_millis(300);

    /// The newest capture asked for; an older one never overwrites it.
    static GEN: AtomicU64 = AtomicU64::new(0);
    /// One capture at a time: a reader that finds the snapshot stale waits
    /// for the capture already running instead of reading the window twice.
    static CAPTURE: Mutex<()> = Mutex::new(());

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
            let label = format!("{path}{name}");
            out.push(AppCommand {
                risky: is_risky(&label),
                label,
                name,
                shortcut,
                target: Target::Menu { hwnd, id },
            });
        }
    }

    fn automation() -> Result<IUIAutomation, String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let bounded: windows::core::Result<IUIAutomation2> =
                CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER);
            if let Ok(ua) = bounded {
                let _ = ua.SetConnectionTimeout(UIA_CONNECT_MS);
                let _ = ua.SetTransactionTimeout(UIA_CALL_MS);
                if let Ok(ua) = ua.cast::<IUIAutomation>() {
                    return Ok(ua);
                }
            }
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).map_err(|e| e.to_string())
        }
    }

    /// What a person can name and click: anything invokable, plus the kinds
    /// that act through other patterns (tabs, check boxes, list and tree
    /// items); on screen and enabled.
    unsafe fn clickable(ua: &IUIAutomation) -> windows::core::Result<IUIAutomationCondition> {
        let mut any = ua.CreatePropertyCondition(
            UIA_IsInvokePatternAvailablePropertyId,
            &VARIANT::from(true),
        )?;
        for kind in [
            UIA_ButtonControlTypeId,
            UIA_HyperlinkControlTypeId,
            UIA_MenuItemControlTypeId,
            UIA_TabItemControlTypeId,
            UIA_SplitButtonControlTypeId,
            UIA_CheckBoxControlTypeId,
            UIA_RadioButtonControlTypeId,
            UIA_ListItemControlTypeId,
            UIA_TreeItemControlTypeId,
            UIA_ComboBoxControlTypeId,
            UIA_DataItemControlTypeId,
        ] {
            let is_kind =
                ua.CreatePropertyCondition(UIA_ControlTypePropertyId, &VARIANT::from(kind.0))?;
            any = ua.CreateOrCondition(&any, &is_kind)?;
        }
        let on_screen =
            ua.CreatePropertyCondition(UIA_IsOffscreenPropertyId, &VARIANT::from(false))?;
        let enabled = ua.CreatePropertyCondition(UIA_IsEnabledPropertyId, &VARIANT::from(true))?;
        ua.CreateAndCondition(&ua.CreateAndCondition(&any, &on_screen)?, &enabled)
    }

    /// The window's controls, as commands to say (`out`) and as things on
    /// the screen to point at (`items`, in reading order).
    pub(super) fn controls(
        hwnd: isize,
        out: &mut Vec<AppCommand>,
        items: &mut Vec<crate::pc_screen::Item>,
    ) -> Result<(), String> {
        use crate::pc_screen::{Item, Kind};
        let _pixels = RealPixels::new();
        let ua = automation()?;
        let found = unsafe {
            (|| {
                let root = ua.ElementFromHandle(HWND(hwnd as _))?;
                // Name, kind and place come back with the search: one call
                // into the app, not one per control.
                let cache = ua.CreateCacheRequest()?;
                cache.AddProperty(UIA_NamePropertyId)?;
                cache.AddProperty(UIA_ControlTypePropertyId)?;
                cache.AddProperty(UIA_BoundingRectanglePropertyId)?;
                cache.SetAutomationElementMode(AutomationElementMode_None)?;
                let all =
                    root.FindAllBuildCache(TreeScope_Descendants, &clickable(&ua)?, &cache)?;
                let mut found = Vec::new();
                for i in 0..all.Length()? {
                    let Ok(el) = all.GetElement(i) else { continue };
                    let Ok(name) = el.CachedName() else { continue };
                    let kind = el.CachedControlType().map(|k| k.0).unwrap_or(0);
                    let rect = el.CachedBoundingRectangle().unwrap_or_default();
                    found.push((name.to_string(), kind, rect));
                }
                windows::core::Result::Ok(found)
            })()
        }
        .map_err(|e| e.to_string())?;
        let mut window = Rect4::default();
        unsafe { GetWindowRect(hwnd, &mut window) };
        let mut shown = Vec::new();
        let mut dropped = 0;
        for (raw, kind, r) in &found {
            let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
            let (cx, cy) = ((r.left + r.right) / 2, (r.top + r.bottom) / 2);
            // On the screen means inside the window: a page keeps what is
            // scrolled away in its tree, with a rectangle outside it.
            let inside = cx > window.l && cx < window.r && cy > window.t && cy < window.b;
            if text.is_empty() || r.right - r.left < 4 || r.bottom - r.top < 4 || !inside {
                dropped += 1;
                continue;
            }
            let kind = match *kind {
                k if k == UIA_HyperlinkControlTypeId.0 => Kind::Link,
                k if k == UIA_ButtonControlTypeId.0 || k == UIA_SplitButtonControlTypeId.0 => {
                    Kind::Button
                }
                k if k == UIA_TabItemControlTypeId.0 => Kind::Tab,
                k if k == UIA_ListItemControlTypeId.0
                    || k == UIA_TreeItemControlTypeId.0
                    || k == UIA_DataItemControlTypeId.0 =>
                {
                    Kind::ListItem
                }
                _ => Kind::Other,
            };
            shown.push(Item {
                text: text.chars().take(200).collect(),
                kind,
                left: r.left,
                top: r.top,
                right: r.right,
                bottom: r.bottom,
            });
            if shown.len() >= MAX_ITEMS {
                break;
            }
        }
        if dropped > shown.len() {
            // Most of the window is not where a click can reach it: say so,
            // or "il primo video" fails with nothing to explain it.
            crate::log(&format!(
                "[PcAppCmds] {} of {} controls are outside the window ({},{} to {},{}) or have no size",
                dropped,
                found.len(),
                window.l,
                window.t,
                window.r,
                window.b
            ));
        }
        *items = crate::pc_screen::in_reading_order(shown);
        let mut seen = std::collections::HashSet::new();
        for (raw, _, _) in found {
            if out.len() >= MAX_ITEMS {
                break;
            }
            let name = raw.trim().to_string();
            if name.is_empty() || name.chars().count() > 60 || !seen.insert(name.to_lowercase()) {
                continue;
            }
            out.push(AppCommand {
                label: name.clone(),
                risky: is_risky(&name),
                name,
                shortcut: String::new(),
                target: Target::Control { hwnd, raw },
            });
        }
        Ok(())
    }

    fn process_of(hwnd: isize) -> String {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
        crate::pc_control::process_name_of(pid)
    }

    fn is_dimmy(hwnd: isize) -> bool {
        process_of(hwnd).eq_ignore_ascii_case("Dimmy.Windows")
    }

    unsafe extern "system" fn wake_view(h: isize, woken: isize) -> i32 {
        let mut buf = [0u16; 64];
        let n = GetClassNameW(h, buf.as_mut_ptr(), buf.len() as i32).clamp(0, 64) as usize;
        if String::from_utf16_lossy(&buf[..n]) != "Chrome_RenderWidgetHostHWND" {
            return 1;
        }
        // WM_GETOBJECT with object id 1 is the probe Chromium takes as "an
        // assistive tool is here" and answers by building the page's tree.
        let mut reply = 0usize;
        SendMessageTimeoutW(h, 0x003D, 0, 1, 0x2, WAKE_SEND_MS, &mut reply);
        let woken = &mut *(woken as *mut u32);
        *woken += 1;
        (*woken < WAKE_MAX_VIEWS) as i32
    }

    /// How many web views of the window were asked to build their tree.
    fn wake_chromium(hwnd: isize) -> u32 {
        let mut woken = 0u32;
        unsafe { EnumChildWindows(hwnd, wake_view, &mut woken as *mut u32 as isize) };
        assert!(woken <= WAKE_MAX_VIEWS);
        woken
    }

    fn short(e: String) -> String {
        e.chars().take(TEXT_MAX).collect()
    }

    fn held() -> Option<isize> {
        SNAPSHOT
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .map(|s| s.hwnd)
    }

    /// Read the window's commands and keep them, unless a newer capture was
    /// asked for meanwhile. The caller holds `CAPTURE`. True when web views
    /// were woken: their controls are there a moment later.
    fn capture_locked(hwnd: isize, gen: u64, may_wake: bool) -> bool {
        assert!(hwnd != 0 && gen != 0);
        let t = Instant::now();
        let process = process_of(hwnd);
        let mut cmds = Vec::new();
        let mut items = Vec::new();
        walk(unsafe { GetMenu(hwnd) }, hwnd, "", 0, &mut cmds);
        let source = if cmds.is_empty() {
            if let Err(e) = controls(hwnd, &mut cmds, &mut items) {
                crate::log(&format!("[PcAppCmds] UI Automation failed: {}", short(e)));
            }
            "controls"
        } else {
            // The menu names the commands; what is on the screen (toolbar,
            // tabs, lists) still comes from the controls, for "il secondo
            // file" and "clicca su ..." in an app with a menu.
            let mut unused = Vec::new();
            let _ = controls(hwnd, &mut unused, &mut items);
            "menu"
        };
        crate::log(&format!(
            "[PcAppCmds] {process}: {} commands, {} items on screen, from its {source} in {} ms ({} risky)",
            cmds.len(),
            items.len(),
            t.elapsed().as_millis(),
            cmds.iter().filter(|c| c.risky).count()
        ));
        let woken = if may_wake && source == "controls" && cmds.len() < FEW_CONTROLS {
            wake_chromium(hwnd)
        } else {
            0
        };
        if woken > 0 {
            crate::log(&format!(
                "[PcAppCmds] {process}: {woken} web view(s) asked for their controls, reading again in {} ms",
                WAKE_SETTLE.as_millis()
            ));
        }
        let mut buf = [0u16; 256];
        let n = unsafe { GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32) };
        let title = String::from_utf16_lossy(&buf[..n.clamp(0, 256) as usize]);
        let app = crate::pc_control::say_name(&process, &title);
        if GEN.load(Ordering::SeqCst) == gen {
            *SNAPSHOT.lock().unwrap_or_else(|e| e.into_inner()) = Some(Snapshot {
                hwnd,
                app,
                cmds,
                items,
            });
        }
        woken > 0
    }

    fn capture_again_after_wake(hwnd: isize, gen: u64) {
        std::thread::sleep(WAKE_SETTLE);
        let _one = CAPTURE.lock().unwrap_or_else(|e| e.into_inner());
        if GEN.load(Ordering::SeqCst) == gen && unsafe { IsWindow(hwnd) } != 0 {
            capture_locked(hwnd, gen, false);
        }
    }

    /// Read the commands of the window in front, off the caller thread.
    pub fn capture_foreground() {
        let hwnd = unsafe { GetForegroundWindow() };
        if hwnd == 0 {
            return;
        }
        std::thread::spawn(move || {
            if is_dimmy(hwnd) {
                return;
            }
            let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
            let woke = {
                let _one = CAPTURE.lock().unwrap_or_else(|e| e.into_inner());
                GEN.load(Ordering::SeqCst) == gen && capture_locked(hwnd, gen, true)
            };
            if woke {
                capture_again_after_wake(hwnd, gen);
            }
        });
    }

    /// True when the snapshot may answer for the window in front. A stale
    /// one is read again here, on the caller thread: about 1 ms for a menu,
    /// one bounded UI Automation search otherwise.
    pub(super) fn front_ready() -> bool {
        let front = unsafe { GetForegroundWindow() };
        let have = held();
        if front != 0 && have == Some(front) {
            return true;
        }
        match freshness(have, front, front != 0 && is_dimmy(front)) {
            Freshness::Use => true,
            Freshness::Nothing => false,
            Freshness::Recapture => {
                let woke = {
                    let _one = CAPTURE.lock().unwrap_or_else(|e| e.into_inner());
                    if held() == Some(front) {
                        return true;
                    }
                    let gen = GEN.fetch_add(1, Ordering::SeqCst) + 1;
                    capture_locked(front, gen, true).then_some(gen)
                };
                if let Some(gen) = woke {
                    std::thread::spawn(move || capture_again_after_wake(front, gen));
                }
                held() == Some(front)
            }
        }
    }

    unsafe fn locate(hwnd: isize, raw: &str) -> windows::core::Result<IUIAutomationElement> {
        let ua = automation().map_err(|_| windows::core::Error::empty())?;
        let root = ua.ElementFromHandle(HWND(hwnd as _))?;
        let named = ua.CreatePropertyCondition(UIA_NamePropertyId, &VARIANT::from(raw))?;
        // The name alone also finds the text inside a button, which does
        // nothing when pressed.
        let same = ua.CreateAndCondition(&named, &clickable(&ua)?)?;
        root.FindFirst(TreeScope_Descendants, &same)
    }

    /// The first way of pressing the control that it accepts.
    unsafe fn act(el: &IUIAutomationElement) -> Result<(), String> {
        type Try<'a> = &'a dyn Fn() -> windows::core::Result<()>;
        let ways: [Try; 5] = [
            &|| unsafe {
                el.GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)?
                    .Invoke()
            },
            &|| unsafe {
                el.GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                    UIA_SelectionItemPatternId,
                )?
                .Select()
            },
            &|| unsafe {
                el.GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                    UIA_ExpandCollapsePatternId,
                )?
                .Expand()
            },
            &|| unsafe {
                el.GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)?
                    .Toggle()
            },
            &|| unsafe {
                el.GetCurrentPatternAs::<IUIAutomationLegacyIAccessiblePattern>(
                    UIA_LegacyIAccessiblePatternId,
                )?
                .DoDefaultAction()
            },
        ];
        let mut last = String::new();
        for way in ways {
            match way() {
                Ok(()) => return Ok(()),
                // The app took the action and is busy with it (a dialog).
                Err(e) if e.code().0 == UIA_E_TIMEOUT => return Ok(()),
                Err(e) => last = e.to_string(),
            }
        }
        Err(last)
    }

    fn press(hwnd: isize, raw: String, name: &str) -> Result<(), String> {
        // Found, then pressed. On its own thread: the press can stay in the
        // app for as long as a dialog it opened is up.
        let (tx, rx) = std::sync::mpsc::channel::<Result<(), String>>();
        std::thread::spawn(move || unsafe {
            let el = match locate(hwnd, &raw) {
                Ok(el) => el,
                Err(e) => {
                    let _ = tx.send(Err(e.to_string()));
                    return;
                }
            };
            let _ = tx.send(Ok(()));
            SetForegroundWindow(hwnd);
            let _ = tx.send(act(&el));
        });
        match rx.recv_timeout(FIND_WAIT) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                crate::log(&format!("[PcAppCmds] control not found: {}", short(e)));
                return Err(short(format!("\"{name}\" non c'è più")));
            }
            Err(_) => {
                return Err(short(format!(
                    "l'app non risponde: \"{name}\" non eseguito"
                )))
            }
        }
        match rx.recv_timeout(ACT_WAIT) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => Err(short(format!("non riesco a premere \"{name}\": {e}"))),
            Err(_) => {
                crate::log("[PcAppCmds] the control has not answered yet: counted as started");
                Ok(())
            }
        }
    }

    pub fn run(cmd: &AppCommand) -> Result<(), String> {
        assert!(!cmd.risky, "a risky command never reaches run()");
        let (Target::Menu { hwnd, .. } | Target::Control { hwnd, .. }) = &cmd.target;
        if unsafe { IsWindow(*hwnd) } == 0 {
            return Err(short(format!(
                "la finestra di \"{}\" non c'è più",
                cmd.name
            )));
        }
        match &cmd.target {
            Target::Menu { hwnd, id } => unsafe {
                SetForegroundWindow(*hwnd);
                if PostMessageW(*hwnd, 0x0111, *id as usize, 0) == 0 {
                    return Err(short(format!("could not send \"{}\"", cmd.label)));
                }
                Ok(())
            },
            Target::Control { hwnd, raw } => press(*hwnd, raw.clone(), &cmd.name),
        }
    }
}

#[cfg(target_os = "windows")]
pub use win::{capture_foreground, click, run};

#[cfg(not(target_os = "windows"))]
pub fn capture_foreground() {}

#[cfg(not(target_os = "windows"))]
pub fn click(_item: &crate::pc_screen::Item, _double: bool) -> Result<(), String> {
    Err("app commands are Windows-only in this experiment".into())
}

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
    fn a_spoken_carrier_does_not_hide_the_command() {
        for (spoken, name) in [
            ("clicca su Salva", "Salva"),
            ("premi il pulsante Invia", "Invia"),
            ("vai su Impostazioni", "Impostazioni"),
            ("click the Save button", "Save"),
            ("apri il menu Visualizza", "Visualizza"),
        ] {
            assert!(
                score(spoken, name) >= RUN_SCORE,
                "{spoken} -> {name}: {}",
                score(spoken, name)
            );
        }
        assert!(score("vai alla riga", "Vai alla riga...") > 0.99);
        assert!(score("apri", "Apri...") > 0.99, "a bare carrier is a name");
        assert!(score("apri Teams", "Apri...") < RUN_SCORE);
        assert_eq!(score("clicca su stampa", "Copia"), 0.0);
    }

    #[test]
    fn a_shortcut_in_the_name_does_not_lower_the_match() {
        assert!(score("ricarica", "Ricarica (Ctrl+R)") > 0.99);
        assert!(score("nuova scheda", "Nuova scheda (Ctrl+T)") > 0.99);
        assert!(score("aiuto", "(?) Aiuto") > 0.99);
        assert!(
            score("beta", "(beta)") > 0.99,
            "nothing left: keep the name"
        );
    }

    #[test]
    fn harmless_names_are_not_flagged() {
        for name in [
            "Formato",
            "Formattazione condizionale",
            "Information",
            "Display",
            "Posta inviata",
            "Enclose",
            "Back",
            "Forward",
            "Indietro",
            "Avanti",
            "Rovescia",
            "Ordina per nome",
            "Nuovo terminale",
            "Format > Font",
            "Pagina successiva",
        ] {
            assert!(!is_risky(name), "{name}");
        }
    }

    #[test]
    fn dangerous_names_are_flagged_by_word_and_by_path() {
        for name in [
            "Elimina > Riga",
            "File > Chiudi tutto",
            "Sign out",
            "Log Out",
            "Sostituisci tutto",
            "Replace All",
            "Clear history",
            "Ripristina impostazioni",
            "Reset",
            "Discard changes",
            "Sovrascrivi",
            "Leave",
            "Hang up",
            "Pubblica",
            "Checkout",
            "Termina",
            "Formatta...",
            "Invia",
            "Pagamento",
            "Rispondi",
        ] {
            assert!(is_risky(name), "{name}");
        }
    }

    #[test]
    fn only_the_window_in_front_is_answered_for() {
        use Freshness::*;
        assert_eq!(freshness(Some(7), 7, false), Use);
        assert_eq!(freshness(Some(7), 9, false), Recapture);
        assert_eq!(freshness(None, 9, false), Recapture);
        assert_eq!(
            freshness(Some(7), 9, true),
            Use,
            "Dimmy's own window in front"
        );
        assert_eq!(freshness(None, 9, true), Nothing);
        assert_eq!(freshness(Some(7), 0, false), Nothing, "no window in front");
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
            target: Target::Control {
                hwnd: 0,
                raw: String::new(),
            },
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
    fn the_full_list_is_by_menu_and_safe() {
        let mk = |label: &str| {
            let name = label.rsplit(" > ").next().unwrap().to_string();
            AppCommand {
                label: label.into(),
                risky: is_risky(&name),
                name,
                shortcut: String::new(),
                target: Target::Control {
                    hwnd: 0,
                    raw: String::new(),
                },
            }
        };
        let cmds: Vec<AppCommand> = [
            "File > Nuovo",
            "File > Salva tutto",
            "File > Esci",
            "Modifica > Copia",
            "File > Recenti > nuovo",
            "Indietro",
        ]
        .iter()
        .map(|l| mk(l))
        .collect();
        assert_eq!(
            groups_of(&cmds),
            vec![
                (
                    "File".to_string(),
                    vec!["Nuovo".to_string(), "Salva tutto".into()]
                ),
                ("Modifica".into(), vec!["Copia".into()]),
                ("".into(), vec!["Indietro".into()]),
            ]
        );
    }

    #[test]
    fn a_clear_lead_is_needed_to_run() {
        let mk = |name: &str| AppCommand {
            label: name.into(),
            name: name.into(),
            shortcut: String::new(),
            risky: is_risky(name),
            target: Target::Control {
                hwnd: 0,
                raw: String::new(),
            },
        };
        let cmds = vec![
            mk("Salva tutto"),
            mk("Salva"),
            mk("Sposta nel cestino"),
            mk("Stampa"),
            mk("Chiudi"),
            mk("Chiudi scheda"),
            mk("Ricarica (Ctrl+R)"),
        ];
        assert!(matches!(best_of("salva tutto", &cmds), Match::Run(c) if c.name == "Salva tutto"));
        assert!(matches!(best_of("clicca su salva", &cmds), Match::Run(c) if c.name == "Salva"));
        assert!(
            matches!(best_of("premi il pulsante ricarica", &cmds), Match::Run(c) if c.name == "Ricarica (Ctrl+R)")
        );
        assert!(matches!(
            best_of("sposta nel cestino", &cmds),
            Match::Refused(_)
        ));
        assert!(
            matches!(best_of("chiudi scheda", &cmds), Match::Refused(c) if c.name == "Chiudi scheda"),
            "the words as said win over the words without their carrier"
        );
        assert!(matches!(best_of("apri il meteo", &cmds), Match::None));
        let c = candidates_of("salva", &cmds, 5);
        assert!(!c.is_empty());
        assert!(
            !candidates_of("cestino", &cmds, 5)
                .iter()
                .any(|l| l.contains("cestino")),
            "risky commands never reach Claude"
        );
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
            let mut items = Vec::new();
            let _ = win::controls(chrome, &mut cmds, &mut items);
            println!(
                "Chrome: {} controls, {} items on screen in {} ms",
                cmds.len(),
                items.len(),
                t.elapsed().as_millis()
            );
            for line in crate::pc_screen::lines(&items, 200) {
                println!("  {line}");
            }
            for said in [
                "apri il primo video",
                "apri il secondo video",
                "apri il terzo risultato",
                "apri l'ultimo video",
            ] {
                let hit = crate::pc_screen::point(said, &items).map(|p| match p {
                    crate::pc_screen::Pointed::Item(k) => items[k].text.clone(),
                    crate::pc_screen::Pointed::Keys(k) => k,
                });
                println!("  {said:?} -> {hit:?}");
            }
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

    /// What the screen layer sees in every window that is open and not
    /// minimized, read-only:
    /// `cargo test --lib --features pc-router every_open_window -- --ignored --nocapture`
    #[test]
    #[ignore]
    #[cfg(target_os = "windows")]
    fn every_open_window_shows_its_items() {
        use crate::pc_screen::Kind;
        for (hwnd, process, title) in crate::pc_control::window_handles() {
            let t = std::time::Instant::now();
            let (mut cmds, mut items) = (Vec::new(), Vec::new());
            let _ = win::controls(hwnd, &mut cmds, &mut items);
            let n = |k: Kind| items.iter().filter(|i| i.kind == k).count();
            println!(
                "== {process} | {} | {} items in {} ms: {} links, {} buttons, {} tabs, {} rows, {} other",
                title.chars().take(40).collect::<String>(),
                items.len(),
                t.elapsed().as_millis(),
                n(Kind::Link),
                n(Kind::Button),
                n(Kind::Tab),
                n(Kind::ListItem),
                n(Kind::Other)
            );
            for said in [
                "apri il primo",
                "apri il secondo file",
                "apri il primo risultato",
            ] {
                if let Some(crate::pc_screen::Pointed::Item(k)) =
                    crate::pc_screen::point(said, &items)
                {
                    println!(
                        "   {said:?} -> {} {:?}",
                        items[k].kind.label(),
                        items[k].text.chars().take(50).collect::<String>()
                    );
                }
            }
        }
    }
}
