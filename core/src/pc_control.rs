//! Voice PC control — EXPERIMENT (branch `exp/pc-actions`, not for release).
//!
//! The "execute" shortcut sends the spoken words here instead of typing them:
//! "apri Teams", "affianca Teams e Outlook", "metti Notepad++ a sinistra",
//! "trova i file di ieri", "scrivi in fondo a Notepad++ ciao".
//!
//! Two halves:
//! - the **hands** (Windows only for now): open windows, Start-menu apps,
//!   bring to front, snap left / right / full / other monitor, tile two,
//!   open an app, Explorer search by date, cursor to the end;
//! - the **brain**: a closed list of actions. `plan_with_llm` asks a fast model, or Haiku
//!   (prewarmed, no thinking) for a JSON plan; a local router is the next step.
//!
//! Nothing here deletes, sends or buys: the action list has no such verb.

use serde::Deserialize;

/// One step of a plan. The model can only pick from these.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case")]
pub enum Step {
    /// Open an app (or bring it forward when it is already open).
    Open { app: String },
    /// Bring an open app's window to the front.
    Focus { app: String },
    /// Move one window: left, right, full, other_monitor.
    Place { app: String, side: Side },
    /// Two windows side by side: `left` on the left half, `right` on the right.
    Tile { left: String, right: String },
    /// Explorer search in the user's folders, by words and/or modified date.
    FindFiles {
        #[serde(default)]
        query: String,
        #[serde(default)]
        when: When,
    },
    /// Send a window to the taskbar.
    Minimize { app: String },
    /// Bring a minimised window back, as it was.
    Restore { app: String },
    /// Minimise everything (Win+D).
    ShowDesktop,
    /// Open a site, or search on one, in the default browser.
    Web {
        #[serde(default)]
        site: String,
        #[serde(default)]
        query: String,
    },
    /// A key for the front-most browser: new_tab, close_tab, reopen_tab,
    /// back, forward, reload, next_tab, previous_tab.
    Browser { action: String },
    /// A media key: play_pause, next, previous, volume_up, volume_down, mute.
    Media { action: String },
    /// Keys for the named (or front) app: a named action (new_line,
    /// select_all, copy, cut, paste, undo, redo, top, bottom, save, find,
    /// enter, escape, tab, backtab, space, backspace, delete, up, down, left,
    /// right, page_up, page_down, home, end) or a chord like "ctrl+shift+t".
    Key {
        #[serde(default)]
        app: String,
        action: String,
    },
    /// A user folder in Explorer: downloads, documents, desktop, pictures,
    /// music, videos; latest_download SELECTS the newest download, never
    /// opens it (a downloaded program must not start by voice).
    OpenFolder { folder: String },
    /// A Windows Settings page: bluetooth, wifi, sound, display, notifications.
    Settings { page: String },
    /// Windows' screen snip (Win+Shift+S).
    Screenshot,
    /// One of the commands the app in front offers (its menu or its controls),
    /// by the label listed to the planner.
    AppCommand { label: String },
    /// Put the cursor at the end of the app's document and paste `text` there.
    TypeAtEnd {
        #[serde(default)]
        app: String,
        text: String,
    },
    /// Write `text` where the cursor is.
    Type { text: String },
    /// Close a window as its X button does: the app still asks about
    /// unsaved work. An empty `app` is the window in front.
    Close {
        #[serde(default)]
        app: String,
    },
    /// Back to the window that was in front before this one.
    SwitchWindow,
    /// Click something the window in front shows: its number in the list
    /// of what is on the screen. `double` opens a file or an icon, which a
    /// single click only selects.
    Click {
        item: usize,
        #[serde(default)]
        double: bool,
    },
}

impl Step {
    /// The step's name, as the planner writes it.
    fn kind(&self) -> &'static str {
        match self {
            Step::Open { .. } => "open",
            Step::Focus { .. } => "focus",
            Step::Place { .. } => "place",
            Step::Tile { .. } => "tile",
            Step::FindFiles { .. } => "find_files",
            Step::Minimize { .. } => "minimize",
            Step::Restore { .. } => "restore",
            Step::ShowDesktop => "show_desktop",
            Step::Web { .. } => "web",
            Step::Browser { .. } => "browser",
            Step::Media { .. } => "media",
            Step::Key { .. } => "key",
            Step::OpenFolder { .. } => "open_folder",
            Step::Settings { .. } => "settings",
            Step::Screenshot => "screenshot",
            Step::AppCommand { .. } => "app_command",
            Step::TypeAtEnd { .. } => "type_at_end",
            Step::Type { .. } => "type",
            Step::Close { .. } => "close",
            Step::SwitchWindow => "switch_window",
            Step::Click { .. } => "click",
        }
    }
}

/// A plan for the log: the names of its steps. A step holds what the user
/// said (a search, a text to type, a file name), and `dimmy.log` holds none
/// of it.
pub fn kinds(steps: &[Step]) -> String {
    let names: Vec<&str> = steps.iter().map(Step::kind).collect();
    format!("[{}]", names.join(","))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Left,
    Right,
    Full,
    OtherMonitor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum When {
    #[default]
    Any,
    Today,
    Yesterday,
    ThisWeek,
    LastWeek,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Plan {
    #[serde(default)]
    pub steps: Vec<Step>,
    /// One short sentence for the pill, in the user's language.
    #[serde(default)]
    pub say: String,
}

/// What the host gets back: a line for the pill and, for `type_at_end`,
/// the text to paste (the host pastes; it owns the clipboard dance).
#[derive(Debug, Default, serde::Serialize)]
pub struct Outcome {
    pub ok: bool,
    pub said: String,
    pub paste: Option<String>,
    pub engine: String,
    /// Why the local router answered or passed (intent, margin, slots).
    pub router: Option<String>,
    /// "Cosa posso dire?": the examples to show; nothing was run.
    pub help: Option<String>,
    pub plan_ms: u64,
    pub act_ms: u64,
    pub error: Option<String>,
}

/// Steps harmless enough to run on a live partial, before the user is done:
/// each one is undone with a click. Writing text is not one of them.
pub fn live_safe(plan: &Plan) -> bool {
    !plan.steps.is_empty()
        && plan.steps.iter().all(|s| match s {
            Step::TypeAtEnd { .. }
            | Step::Type { .. }
            | Step::AppCommand { .. }
            | Step::Click { .. }
            | Step::Close { .. } => false,
            Step::Key { action, .. } => LIVE_KEYS.contains(&action.as_str()),
            Step::Browser { action } => action != "close_tab",
            _ => true,
        })
}

/// Keys that only move or copy: a wrong one changes nothing in a document.
const LIVE_KEYS: &[&str] = &[
    "copy",
    "select_all",
    "top",
    "bottom",
    "page_up",
    "page_down",
    "up",
    "down",
    "left",
    "right",
];

/// The virtual keys of one key name, as people and the planner write them.
fn key_code(name: &str) -> Option<u8> {
    Some(match name {
        "ctrl" | "control" => 0x11,
        "alt" => 0x12,
        "shift" | "maiusc" => 0x10,
        "win" | "windows" => 0x5B,
        "enter" | "invio" | "return" => 0x0D,
        "escape" | "esc" => 0x1B,
        "tab" => 0x09,
        "space" | "spazio" => 0x20,
        "backspace" => 0x08,
        "delete" | "canc" | "del" => 0x2E,
        "up" | "su" => 0x26,
        "down" | "giu" | "giù" => 0x28,
        "left" | "sinistra" => 0x25,
        "right" | "destra" => 0x27,
        "page_up" | "pageup" | "pagsu" => 0x21,
        "page_down" | "pagedown" | "paggiu" => 0x22,
        "home" | "inizio" => 0x24,
        "end" | "fine" => 0x23,
        _ => {
            let b = name.as_bytes();
            match b {
                [c @ b'a'..=b'z'] => c.to_ascii_uppercase(),
                [c @ b'0'..=b'9'] => *c,
                [b'f', d @ b'1'..=b'9'] => 0x70 + (d - b'1'),
                [b'f', b'1', d @ b'0'..=b'2'] => 0x79 + (d - b'0'),
                _ => return None,
            }
        }
    })
}

/// The keys of a `key` step, in press order: a named action, or a chord of
/// up to four keys ("ctrl+shift+t"). None for anything else: an unknown
/// name is refused, never guessed.
pub fn key_chord(action: &str) -> Option<Vec<u8>> {
    let named: &[u8] = match action {
        "new_line" => &[0x0D],
        "select_all" => &[0x11, 0x41],
        "copy" => &[0x11, 0x43],
        "cut" => &[0x11, 0x58],
        "paste" => &[0x11, 0x56],
        "undo" => &[0x11, 0x5A],
        "redo" => &[0x11, 0x59],
        "top" => &[0x11, 0x24],
        "bottom" => &[0x11, 0x23],
        "save" => &[0x11, 0x53],
        "find" => &[0x11, 0x46],
        "backtab" => &[0x10, 0x09],
        _ => &[],
    };
    if !named.is_empty() {
        return Some(named.to_vec());
    }
    let keys: Vec<u8> = action
        .split('+')
        .map(|k| key_code(k.trim().to_lowercase().as_str()))
        .collect::<Option<_>>()?;
    let modifier = |k: &u8| matches!(k, 0x10 | 0x11 | 0x12 | 0x5B);
    // Modifiers first, one last key that is not one: "s+ctrl" is not a chord.
    let ok = keys.len() == 1
        || (2..=4).contains(&keys.len())
            && keys[..keys.len() - 1].iter().all(modifier)
            && !modifier(&keys[keys.len() - 1]);
    ok.then_some(keys)
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// The address for a `web` step. Known sites search on themselves; any
/// other words become a Google search.
pub fn web_url(site: &str, query: &str) -> Option<String> {
    let site = norm(site);
    let q = percent_encode(query.trim());
    let has_q = !query.trim().is_empty();
    let url = match site.as_str() {
        "youtube" if has_q => format!("https://www.youtube.com/results?search_query={q}"),
        "youtube" => "https://www.youtube.com".into(),
        "amazon" if has_q => format!("https://www.amazon.it/s?k={q}"),
        "amazon" => "https://www.amazon.it".into(),
        "wikipedia" if has_q => format!("https://it.wikipedia.org/w/index.php?search={q}"),
        "wikipedia" => "https://it.wikipedia.org".into(),
        "gmail" => "https://mail.google.com".into(),
        "maps" | "mappe" if has_q => format!("https://www.google.com/maps/search/{q}"),
        "maps" | "mappe" => "https://www.google.com/maps".into(),
        "netflix" | "github" | "linkedin" | "facebook" | "instagram" | "spotify" | "reddit"
            if !has_q =>
        {
            format!("https://www.{site}.com")
        }
        "google" | "" if has_q => format!("https://www.google.com/search?q={q}"),
        "google" => "https://www.google.com".into(),
        _ if has_q => format!(
            "https://www.google.com/search?q={}",
            percent_encode(&format!("{site} {}", query.trim()))
        ),
        _ => format!("https://www.google.com/search?q={}", percent_encode(&site)),
    };
    Some(url)
}

/// Parse the model's answer: a JSON object, possibly wrapped in a code fence.
pub fn parse_plan(raw: &str) -> Result<Plan, String> {
    let s = raw.trim();
    let start = s.find('{').ok_or("no JSON object in the answer")?;
    let end = s.rfind('}').ok_or("no JSON object in the answer")?;
    if end < start {
        return Err("no JSON object in the answer".into());
    }
    serde_json::from_str::<Plan>(&s[start..=end]).map_err(|e| format!("bad plan: {e}"))
}

// ── App names ────────────────────────────────────────────────────────

/// Lowercase letters and digits only: "Notepad++" and "notepad" compare.
pub fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

/// Spoken names that are not the app's name.
const ALIASES: &[(&str, &[&str])] = &[
    ("mail", &["outlook", "olk", "thunderbird"]),
    ("posta", &["outlook", "olk", "thunderbird"]),
    ("email", &["outlook", "olk", "thunderbird"]),
    ("calendario", &["outlook", "olk"]),
    ("calendar", &["outlook", "olk"]),
    ("browser", &["chrome", "msedge", "firefox", "brave"]),
    ("bloccodinote", &["notepad"]),
    ("blocconote", &["notepad"]),
    ("terminale", &["windowsterminal", "terminal"]),
    ("terminal", &["windowsterminal", "terminal"]),
    ("esplora", &["explorer"]),
    ("files", &["explorer"]),
];

/// A spoken word that stands for an app ("posta", "browser").
pub fn is_alias(word: &str) -> bool {
    ALIASES.iter().any(|(alias, _)| *alias == word)
}

fn similarity(a: &str, b: &str) -> f32 {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    1.0 - prev[b.len()] as f32 / a.len().max(b.len()) as f32
}

/// How well a spoken app name fits a candidate (process name or app name +
/// window title). 0 = no match.
pub fn name_score(spoken: &str, name: &str, title: &str) -> f32 {
    let s = norm(spoken);
    let n = norm(name);
    let t = norm(title);
    if s.is_empty() {
        return 0.0;
    }
    for (alias, targets) in ALIASES {
        if s == *alias && targets.iter().any(|x| n.contains(x)) {
            return 0.95;
        }
    }
    if n == s {
        return 1.0;
    }
    // A prefix or substring needs 4 letters: on a live partial, "Apri no"
    // must not already mean Notepad.
    if s.len() >= 4 && n.starts_with(&s) || n.len() >= 4 && s.starts_with(&n) {
        return 0.9;
    }
    if s.len() >= 4 && n.contains(&s) {
        return 0.8;
    }
    if !t.is_empty() && s.len() >= 4 && t.contains(&s) {
        return 0.7;
    }
    let sim = similarity(&s, &n);
    if s.len() >= 4 && sim >= 0.6 {
        return 0.6 * sim;
    }
    0.0
}

fn best<'a, T>(spoken: &str, items: &'a [T], key: impl Fn(&T) -> (&str, &str)) -> Option<&'a T> {
    items
        .iter()
        .map(|it| {
            let (n, t) = key(it);
            (name_score(spoken, n, t), it)
        })
        .filter(|(s, _)| *s > 0.0)
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, it)| it)
}

// ── Brain: Haiku ─────────────────────────────────────────────────────

fn planner_prompt(
    command: &str,
    windows: &[String],
    app: &str,
    app_cmds: &[String],
    in_order: bool,
    history: &[(String, String)],
) -> String {
    let history_part = if history.is_empty() {
        String::new()
    } else {
        format!(
            "\nEarlier in this session, oldest first (said -> done). The new command continues from there: \
             a search that names no site goes to the site already open, \"it\" is the app just named, \
             words that are no command after a site was opened are a search on it.\n{}\n",
            history
                .iter()
                .map(|(said, did)| format!("\"{said}\" -> {did}"))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    let app_part = if app_cmds.is_empty() {
        String::new()
    } else if in_order {
        format!(
            "\nWhat the window in front ({app}) shows on the screen now, in reading order. To click one, use \
             {{\"do\":\"click\",\"item\":<its number>,\"double\":<true to open a file or an icon>}}. \"The third video\" is the third item that reads like \
             a video title (skip menus, buttons, filters, channel names); \"the video by X\" is the title \
             that names X. Click what is shown rather than searching for it again:\n{}\n",
            app_cmds.join("\n")
        )
    } else {
        format!(
            "\nCommands the app in front ({app}) offers, most likely first. To run one, use \
             {{\"do\":\"app_command\",\"label\":\"<the label exactly as listed, without the shortcut>\"}}:\n{}\n",
            app_cmds.join("\n")
        )
    };
    format!(
        "You turn a spoken command into actions on a Windows PC. Reply with ONE JSON object and nothing else.\n\
         Schema: {{\"steps\": [...], \"say\": \"<one short sentence in the command's language saying what you did>\"}}\n\
         Each step is one of:\n\
         {{\"do\":\"open\",\"app\":\"<app name>\"}}  open it, or bring it forward if already open\n\
         {{\"do\":\"focus\",\"app\":\"<app>\"}}  bring an open window to the front\n\
         {{\"do\":\"place\",\"app\":\"<app>\",\"side\":\"left|right|full|other_monitor\"}}\n\
         {{\"do\":\"tile\",\"left\":\"<app>\",\"right\":\"<app>\"}}  two windows side by side\n\
         {{\"do\":\"minimize\",\"app\":\"<app>\"}}  send it to the taskbar (also: hide, lower, abbassa, nascondi)\n\
         {{\"do\":\"restore\",\"app\":\"<app>\"}}  bring a minimised window back\n\
         {{\"do\":\"show_desktop\"}}  minimise everything\n\
         {{\"do\":\"web\",\"site\":\"youtube|google|amazon|wikipedia|maps|gmail|<site>\",\"query\":\"<search words, may be empty>\"}}  open or search a site in the browser\n\
         {{\"do\":\"browser\",\"action\":\"new_tab|close_tab|reopen_tab|back|forward|reload|next_tab|previous_tab\"}}\n\
         {{\"do\":\"media\",\"action\":\"play_pause|next|previous|volume_up|volume_down|mute\"}}\n\
         {{\"do\":\"key\",\"app\":\"<app, may be empty>\",\"action\":\"new_line|select_all|copy|cut|paste|undo|redo|top|bottom|save|find|enter|escape|tab|backtab|space|backspace|delete|up|down|left|right|page_up|page_down|home|end, or a chord like ctrl+shift+t\"}}  press keys in the app (scrolling is page_down / page_up)\n\
         {{\"do\":\"close\",\"app\":\"<app, may be empty for the window in front>\"}}  close a window\n\
         {{\"do\":\"switch_window\"}}  back to the previous window\n\
         {{\"do\":\"type\",\"text\":\"<text to write>\"}}  write the text where the cursor is\n\
         {{\"do\":\"open_folder\",\"folder\":\"downloads|documents|desktop|pictures|music|videos|latest_download\"}}\n\
         {{\"do\":\"settings\",\"page\":\"bluetooth|wifi|sound|display|notifications\"}}\n\
         {{\"do\":\"screenshot\"}}\n\
         {{\"do\":\"find_files\",\"query\":\"<words, may be empty>\",\"when\":\"any|today|yesterday|this_week|last_week\"}}\n\
         {{\"do\":\"type_at_end\",\"app\":\"<app>\",\"text\":\"<text to write>\"}}  cursor to the end of the document, write the text\n\
         Rules: only these actions. Use the app names as the user said them, or the name from the open windows list. \
         \"this window\" / \"questa finestra\" / \"it\" means the first window in the list. The words come from speech \
         recognition: read through spelling mistakes (\"aprint browser\" is \"apri il browser\"). If the command asks for something \
         outside these actions, reply {{\"steps\": [], \"say\": \"<what you cannot do>\"}}.\n\
         Open windows, front-most first (process: title):\n{}\n{}{}\n\
         Command: {}",
        windows.join("\n"),
        app_part,
        history_part,
        command
    )
}

/// Ask a model for a plan. `fast` is an HTTPS provider that answers in well
/// under a second, when the user has a key for one; the Claude CLI (the
/// user's subscription, prewarmed, no thinking) is the fallback: its API
/// call alone measured 2.0-2.4 s per command (2026-11-08, ~6k tokens in).
/// Returns the plan and the engine that made it.
pub fn plan_with_llm(
    command: &str,
    model: &str,
    fast: impl Fn(&str) -> Option<String>,
) -> Result<(Plan, &'static str), String> {
    let windows: Vec<String> = hands::open_windows()
        .iter()
        .take(25)
        .map(|w| format!("{}: {}", w.process, w.title))
        .collect();
    // The planner sees the screen whenever there is one to see: what is
    // shown is the context of what is said. A menu app has only commands.
    let (shown_app, shown) = crate::pc_appcmds::screen_lines(60);
    let in_order = !shown.is_empty();
    let (app, app_cmds) = if in_order {
        (shown_app, shown)
    } else {
        crate::pc_appcmds::candidates(command, 12)
    };
    let history = context().history.clone();
    let prompt = planner_prompt(command, &windows, &app, &app_cmds, in_order, &history);
    if let Some(raw) = fast(&prompt) {
        match parse_plan(&raw) {
            Ok(plan) => return Ok((plan, "fast")),
            Err(e) => crate::log(&format!(
                "[PcControl] fast planner answer unusable ({e}), asking Claude"
            )),
        }
    }
    let raw =
        crate::claude_code::run_blocking(&prompt, model, std::time::Duration::from_secs(30), true)
            .map_err(|e| e.to_string());
    // A started CLI answers one prompt: start the next one now, so the next
    // command does not pay its half second of start-up.
    crate::claude_code::prewarm(model);
    parse_plan(&raw?).map(|p| (p, "claude"))
}

/// Start the Claude CLI, load the local router and read the Start menu
/// while the user talks.
pub fn prepare(model: &str) {
    reset_context();
    crate::claude_code::prewarm(model);
    crate::pc_appcmds::capture_foreground();
    #[cfg(feature = "pc-router")]
    {
        crate::pc_router::warm();
        crate::pc_router::shadow_begin();
    }
    std::thread::spawn(|| {
        let _ = hands::start_apps();
    });
}

/// Names a command can mention: open windows (process, title), then the
/// Start-menu apps (name, "").
pub fn known_names() -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = hands::open_windows()
        .into_iter()
        .map(|w| (w.process, w.title))
        .collect();
    v.extend(
        hands::start_app_names()
            .into_iter()
            .map(|n| (n, String::new())),
    );
    v
}

/// Words an execute session is likely to hear: the commands' own words and
/// the names of the open apps. The recognizer leans toward them, so "apri"
/// is not heard as "April" and an app's name is spelled as the app spells it.
pub fn bias_words() -> Vec<String> {
    const COMMAND_WORDS: &[&str] = &[
        "apri",
        "chiudi",
        "affianca",
        "riduci a icona",
        "ingrandisci",
        "massimizza",
        "ripristina",
        "sposta",
        "mettilo",
        "a sinistra",
        "a destra",
        "schermo intero",
        "altro monitor",
        "nuova scheda",
        "chiudi la scheda",
        "finestra",
        "cartella",
        "download",
        "impostazioni",
        "cerca",
        "vai su",
        "scorri",
        "premi",
        "invio",
        "copia",
        "incolla",
        "annulla",
        "salva",
        "seleziona tutto",
        "volume",
        "pausa",
        "screenshot",
        "desktop",
        "browser",
        "scrivi",
        "clicca",
        "YouTube",
        "Google",
    ];
    let mut v: Vec<String> = COMMAND_WORDS.iter().map(|s| s.to_string()).collect();
    for w in hands::open_windows().iter().take(25) {
        for name in [say_name(&w.process, &w.title), w.process.clone()] {
            if name.chars().count() >= 3 && !v.iter().any(|x| x.eq_ignore_ascii_case(&name)) {
                v.push(name);
            }
        }
    }
    v
}

/// The open apps by the name a person would say, once each, Dimmy left out.
pub fn open_app_names() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for w in hands::open_windows() {
        let name = say_name(&w.process, &w.title);
        if !name.is_empty()
            && !w.process.eq_ignore_ascii_case("Dimmy.Windows")
            && !v.iter().any(|n| n.eq_ignore_ascii_case(&name))
        {
            v.push(name);
        }
    }
    v
}

/// "Documento1 - Word" says "Word": the last part of a title is usually the
/// app. A long last part is a page or a file, so the process name is used.
pub(crate) fn say_name(process: &str, title: &str) -> String {
    let last = title.rsplit(" - ").next().unwrap_or("").trim();
    if title.contains(" - ") && (2..=20).contains(&last.chars().count()) {
        last.to_string()
    } else {
        process.to_string()
    }
}

/// Every open window (handle, process, title), front-most first: for the
/// read-only check of what the screen layer sees.
#[cfg(all(test, target_os = "windows"))]
pub(crate) fn window_handles() -> Vec<(isize, String, String)> {
    hands::open_windows()
        .into_iter()
        .map(|w| (w.hwnd, w.process, w.title))
        .collect()
}

/// The executable name (no extension) of a process, for the app-commands log.
#[cfg(target_os = "windows")]
pub fn process_name_of(pid: u32) -> String {
    hands::process_name_of(pid)
}

/// What the session's earlier commands leave for the next one. People talk
/// to it as to a person: "vai su YouTube", then only the words to search,
/// then "clicca il terzo video", then "mettilo a destra".
struct Context {
    /// The app the last command named: what "mettilo" and "ripristinalo" mean.
    last_app: String,
    /// The site of the last `web` step: where a bare "cerca ..." searches.
    site: String,
    /// The last `web` step opened a site with nothing to search: words that
    /// are not a command are the search.
    awaiting_query: bool,
    /// The browser tab in front is one this session opened or filled: the
    /// next `web` step goes into it instead of opening another.
    own_tab: bool,
    /// The last commands, oldest first: (what was said, what was done).
    history: Vec<(String, String)>,
}

static CONTEXT: std::sync::Mutex<Context> = std::sync::Mutex::new(Context {
    last_app: String::new(),
    site: String::new(),
    awaiting_query: false,
    own_tab: false,
    history: Vec::new(),
});

fn context() -> std::sync::MutexGuard<'static, Context> {
    CONTEXT.lock().unwrap_or_else(|e| e.into_inner())
}

/// A new session starts with no context.
pub(crate) fn reset_context() {
    let mut c = context();
    c.last_app.clear();
    c.site.clear();
    c.awaiting_query = false;
    c.own_tab = false;
    c.history.clear();
}

pub fn last_app() -> String {
    context().last_app.clone()
}

#[cfg(test)]
pub(crate) fn set_last_app(app: &str) {
    context().last_app = app.to_string();
}

/// The site of the session's last `web` step, or "".
pub fn last_site() -> String {
    context().site.clone()
}

fn own_tab() -> bool {
    context().own_tab
}

/// Keep what a command did, for the planner of the next one.
pub fn remember(said: &str, did: &str) {
    let mut c = context();
    c.history.push((
        crate::truncate_utf8(said, 120).to_string(),
        crate::truncate_utf8(did, 120).to_string(),
    ));
    if c.history.len() > 6 {
        c.history.remove(0);
    }
}

/// Sites that take a search: after "vai su YouTube", words are a search.
const SEARCHABLE: &[&str] = &["youtube", "google", "amazon", "wikipedia", "maps", "mappe"];

/// What a step that ran leaves behind.
fn note_step(step: &Step) {
    let mut c = context();
    match step {
        Step::Web { site, query } => {
            let site = norm(site);
            c.site = if site.is_empty() {
                "google".into()
            } else {
                site
            };
            c.awaiting_query = query.trim().is_empty() && SEARCHABLE.contains(&c.site.as_str());
            c.own_tab = true;
        }
        Step::Browser { action } => {
            c.awaiting_query = false;
            c.own_tab = action == "new_tab"
                || (c.own_tab && matches!(action.as_str(), "back" | "forward" | "reload"));
        }
        // These leave the window in front where it is.
        Step::Key { app, .. } if app.trim().is_empty() => c.awaiting_query = false,
        Step::Type { .. } | Step::AppCommand { .. } | Step::Click { .. } | Step::Media { .. } => {
            c.awaiting_query = false
        }
        _ => {
            c.awaiting_query = false;
            c.own_tab = false;
        }
    }
    if let Some(app) = named_app(step) {
        c.last_app = app.to_string();
    }
}

/// Words that start a command, not a search: "clicca il primo", "apri ...".
const COMMAND_STARTS: &[&str] = &[
    "clicca",
    "click",
    "premi",
    "press",
    "apri",
    "aprimi",
    "open",
    "chiudi",
    "close",
    "vai",
    "go",
    "torna",
    "metti",
    "mettilo",
    "sposta",
    "scrivi",
    "type",
    "seleziona",
    "scegli",
    "vediamo",
    "guarda",
    "riproduci",
    "play",
    "fammi",
    "mostra",
    "mostrami",
    "show",
    "passa",
    "riduci",
    "ingrandisci",
    "nuova",
    "new",
    "scorri",
    "scroll",
    "alza",
    "abbassa",
    "cosa",
    "what",
];

const SEARCH_VERBS: &[&str] = &["cerca", "cercami", "cerco", "search"];

/// A search that leans on the session: "cerca gatti" goes to the site
/// already open, and after "cerca su YouTube" (the site opened, nothing
/// searched yet) the words alone are the search. None when the session has
/// no site, or the words start like a command.
pub fn search_in_context(said: &str) -> Option<Plan> {
    let (site, awaiting) = {
        let c = context();
        (c.site.clone(), c.awaiting_query)
    };
    if site.is_empty() {
        return None;
    }
    let edge = |c: char| !c.is_alphanumeric();
    let said = said.trim().trim_matches(edge);
    let first = said
        .split_whitespace()
        .next()?
        .trim_matches(edge)
        .to_lowercase();
    let query = if SEARCH_VERBS.contains(&first.as_str()) {
        said.split_whitespace()
            .skip(1)
            .collect::<Vec<_>>()
            .join(" ")
    } else if awaiting && !COMMAND_STARTS.contains(&first.as_str()) {
        said.to_string()
    } else {
        return None;
    };
    if query.is_empty() {
        return None;
    }
    Some(Plan {
        say: format!("Cerco \"{query}\" su {site}"),
        steps: vec![Step::Web { site, query }],
    })
}

/// Words that count or name the things a page shows.
#[cfg(test)]
const LISTING_WORDS: &[&str] = &[
    "primo",
    "prima",
    "secondo",
    "seconda",
    "terzo",
    "terza",
    "quarto",
    "quarta",
    "quinto",
    "quinta",
    "sesto",
    "sesta",
    "ultimo",
    "ultima",
    "first",
    "second",
    "third",
    "fourth",
    "fifth",
    "last",
    "video",
    "risultato",
    "risultati",
    "link",
    "result",
    "results",
];

#[cfg(test)]
fn wants_listing(command: &str) -> bool {
    command
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| LISTING_WORDS.contains(&w.to_lowercase().as_str()))
}

fn named_app(step: &Step) -> Option<&str> {
    match step {
        Step::Open { app }
        | Step::Focus { app }
        | Step::Place { app, .. }
        | Step::Minimize { app }
        | Step::Restore { app }
        | Step::Tile { right: app, .. } => Some(app.as_str()).filter(|a| !a.trim().is_empty()),
        _ => None,
    }
}

/// Run a plan, step by step; stop at the first step that fails.
pub fn run(plan: &Plan) -> Result<Option<String>, String> {
    let mut paste = None;
    for step in &plan.steps {
        crate::log(&format!("[PcControl] step {:?}", step));
        if let Some(text) = hands::act(step)? {
            paste = Some(text);
        }
        note_step(step);
    }
    Ok(paste)
}

#[cfg(target_os = "windows")]
mod hands {
    use super::{best, Side, Step, When};
    use std::sync::Mutex;

    type Hwnd = isize;

    #[repr(C)]
    #[derive(Default, Clone, Copy)]
    struct Rect {
        l: i32,
        t: i32,
        r: i32,
        b: i32,
    }

    #[repr(C)]
    struct MonitorInfo {
        cb: u32,
        monitor: Rect,
        work: Rect,
        flags: u32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(cb: unsafe extern "system" fn(Hwnd, isize) -> i32, lp: isize) -> i32;
        fn IsWindowVisible(h: Hwnd) -> i32;
        fn GetWindowTextW(h: Hwnd, buf: *mut u16, max: i32) -> i32;
        fn GetWindowThreadProcessId(h: Hwnd, pid: *mut u32) -> u32;
        fn GetWindow(h: Hwnd, cmd: u32) -> Hwnd;
        fn ShowWindow(h: Hwnd, cmd: i32) -> i32;
        fn IsIconic(h: Hwnd) -> i32;
        fn SetForegroundWindow(h: Hwnd) -> i32;
        fn MonitorFromWindow(h: Hwnd, flags: u32) -> isize;
        fn GetMonitorInfoW(m: isize, mi: *mut MonitorInfo) -> i32;
        fn EnumDisplayMonitors(
            hdc: isize,
            clip: *const Rect,
            cb: unsafe extern "system" fn(isize, isize, *mut Rect, isize) -> i32,
            lp: isize,
        ) -> i32;
        fn SetWindowPos(h: Hwnd, after: Hwnd, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> i32;
        fn keybd_event(vk: u8, scan: u8, flags: u32, extra: usize);
        fn MapVirtualKeyW(code: u32, map: u32) -> u32;
        fn GetForegroundWindow() -> Hwnd;
        fn PostMessageW(h: Hwnd, msg: u32, wp: usize, lp: isize) -> i32;
    }

    const BROWSERS: [&str; 6] = ["chrome", "msedge", "firefox", "brave", "opera", "vivaldi"];

    fn front_is_browser() -> bool {
        let mut pid = 0u32;
        unsafe { GetWindowThreadProcessId(GetForegroundWindow(), &mut pid) };
        let process = process_name(pid);
        BROWSERS.iter().any(|b| process.eq_ignore_ascii_case(b))
    }

    /// Load `url` in the browser tab in front: address bar, the address,
    /// Enter. Delete first drops what the bar completed by itself.
    fn navigate_tab(url: &str) {
        chord(&[0x11, 0x4C]); // Ctrl+L
        std::thread::sleep(std::time::Duration::from_millis(80));
        crate::hotkey::type_text(url);
        std::thread::sleep(std::time::Duration::from_millis(40));
        chord(&[0x2E]);
        chord(&[0x0D]);
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn QueryFullProcessImageNameW(h: isize, flags: u32, buf: *mut u16, len: *mut u32) -> i32;
        fn CloseHandle(h: isize) -> i32;
    }
    #[link(name = "shell32")]
    extern "system" {
        fn ShellExecuteW(
            hwnd: isize,
            op: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> isize;
    }
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmGetWindowAttribute(h: Hwnd, attr: u32, out: *mut u32, size: u32) -> i32;
    }

    pub struct Win {
        pub hwnd: Hwnd,
        pub process: String,
        pub title: String,
    }

    fn process_name(pid: u32) -> String {
        unsafe {
            let h = OpenProcess(0x1000, 0, pid); // PROCESS_QUERY_LIMITED_INFORMATION
            if h == 0 {
                return String::new();
            }
            let mut buf = [0u16; 520];
            let mut len = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
            CloseHandle(h);
            if ok == 0 {
                return String::new();
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            let file = path.rsplit('\\').next().unwrap_or("");
            file.trim_end_matches(".exe")
                .trim_end_matches(".EXE")
                .to_string()
        }
    }

    unsafe extern "system" fn collect(h: Hwnd, lp: isize) -> i32 {
        let out = &mut *(lp as *mut Vec<Win>);
        if IsWindowVisible(h) == 0 || GetWindow(h, 4) != 0 {
            return 1; // hidden, or an owned popup (GW_OWNER)
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(h, 14, &mut cloaked, 4) == 0 && cloaked != 0 {
            return 1; // DWMWA_CLOAKED: a suspended UWP shell window
        }
        let mut buf = [0u16; 256];
        let n = GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
        if n <= 0 {
            return 1;
        }
        let mut pid = 0u32;
        GetWindowThreadProcessId(h, &mut pid);
        let process = process_name(pid);
        if process.is_empty()
            || matches!(process.as_str(), "TextInputHost" | "ApplicationFrameHost")
        {
            return 1;
        }
        out.push(Win {
            hwnd: h,
            process,
            title: String::from_utf16_lossy(&buf[..n as usize]),
        });
        1
    }

    pub fn process_name_of(pid: u32) -> String {
        process_name(pid)
    }

    /// Top-level windows a user would call by name, front-most first.
    pub fn open_windows() -> Vec<Win> {
        let mut v: Vec<Win> = Vec::new();
        unsafe { EnumWindows(collect, &mut v as *mut Vec<Win> as isize) };
        v.retain(|w| !w.process.eq_ignore_ascii_case("Dimmy.Windows"));
        v
    }

    pub struct StartApp {
        pub name: String,
        pub app_id: String,
    }

    pub fn start_app_names() -> Vec<String> {
        start_apps().into_iter().map(|a| a.name).collect()
    }

    static START_APPS: Mutex<Vec<(String, String)>> = Mutex::new(Vec::new());

    /// The Start menu's apps (name, AppUserModelID). Read once, ~1 s.
    pub fn start_apps() -> Vec<StartApp> {
        let mut cache = START_APPS.lock().unwrap_or_else(|e| e.into_inner());
        if cache.is_empty() {
            use std::os::windows::process::CommandExt;
            let out = std::process::Command::new("powershell")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-StartApps | ConvertTo-Json -Compress",
                ])
                .creation_flags(0x0800_0000)
                .output();
            if let Ok(o) = out {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&o.stdout) {
                    for a in v.as_array().into_iter().flatten() {
                        if let (Some(n), Some(id)) = (a["Name"].as_str(), a["AppID"].as_str()) {
                            cache.push((n.to_string(), id.to_string()));
                        }
                    }
                }
            }
            crate::log(&format!("[PcControl] start apps: {}", cache.len()));
        }
        cache
            .iter()
            .map(|(n, id)| StartApp {
                name: n.clone(),
                app_id: id.clone(),
            })
            .collect()
    }

    fn find_window(app: &str) -> Option<Win> {
        let wins = open_windows();
        if app.trim().is_empty() {
            return wins.into_iter().next();
        }
        let hit = best(app, &wins, |w| (w.process.as_str(), w.title.as_str()))?;
        Some(Win {
            hwnd: hit.hwnd,
            process: hit.process.clone(),
            title: hit.title.clone(),
        })
    }

    fn tap(vk: u8, up: bool) {
        unsafe {
            let scan = MapVirtualKeyW(vk as u32, 0) as u8;
            keybd_event(vk, scan, if up { 2 } else { 0 }, 0);
        }
    }

    /// Arrows, Home/End, Insert/Delete and Win are extended keys.
    fn key_flags(vk: u8) -> u32 {
        u32::from(matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B))
    }

    /// Press `keys` in order, release them in reverse.
    fn chord(keys: &[u8]) {
        unsafe {
            for &k in keys {
                keybd_event(k, MapVirtualKeyW(k as u32, 0) as u8, key_flags(k), 0);
            }
            for &k in keys.iter().rev() {
                keybd_event(k, MapVirtualKeyW(k as u32, 0) as u8, key_flags(k) | 2, 0);
            }
        }
    }

    /// Hand a URL, a shell: folder or a ms-settings: page to Windows.
    /// ShellExecute "open": the default browser for a URL, Explorer for a
    /// shell: folder, Settings for ms-settings:. (explorer.exe with a URL
    /// that has a query string opened Explorer instead of the browser.)
    fn shell_open(target: &str) -> Result<Option<String>, String> {
        let wide = |s: &str| {
            s.encode_utf16()
                .chain(std::iter::once(0))
                .collect::<Vec<u16>>()
        };
        let (verb, file) = (wide("open"), wide(target));
        let rc = unsafe {
            ShellExecuteW(
                0,
                verb.as_ptr(),
                file.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            )
        };
        // Per the docs, a value above 32 means success.
        if rc > 32 {
            Ok(None)
        } else {
            Err(format!("ShellExecute failed ({rc}) for {target}"))
        }
    }

    fn bring_forward(h: Hwnd) {
        unsafe {
            if IsIconic(h) != 0 {
                ShowWindow(h, 9); // SW_RESTORE
            }
            // An Alt tap lifts the foreground lock for this process.
            tap(0x12, false);
            tap(0x12, true);
            SetForegroundWindow(h);
        }
    }

    fn monitors() -> Vec<Rect> {
        unsafe extern "system" fn each(m: isize, _: isize, _: *mut Rect, lp: isize) -> i32 {
            let out = &mut *(lp as *mut Vec<Rect>);
            let mut mi = MonitorInfo {
                cb: std::mem::size_of::<MonitorInfo>() as u32,
                monitor: Rect::default(),
                work: Rect::default(),
                flags: 0,
            };
            if GetMonitorInfoW(m, &mut mi) != 0 {
                out.push(mi.work);
            }
            1
        }
        let mut v: Vec<Rect> = Vec::new();
        unsafe {
            EnumDisplayMonitors(0, std::ptr::null(), each, &mut v as *mut Vec<Rect> as isize)
        };
        v
    }

    fn work_area_of(h: Hwnd) -> Rect {
        unsafe {
            let mut mi = MonitorInfo {
                cb: std::mem::size_of::<MonitorInfo>() as u32,
                monitor: Rect::default(),
                work: Rect::default(),
                flags: 0,
            };
            GetMonitorInfoW(MonitorFromWindow(h, 2), &mut mi); // NEAREST
            mi.work
        }
    }

    fn place(h: Hwnd, side: Side) {
        let mut wa = work_area_of(h);
        if side == Side::OtherMonitor {
            if let Some(other) = monitors().into_iter().find(|m| m.l != wa.l || m.t != wa.t) {
                wa = other;
            }
        }
        let (w, ht) = (wa.r - wa.l, wa.b - wa.t);
        let (x, cx) = match side {
            Side::Left => (wa.l, w / 2),
            Side::Right => (wa.l + w / 2, w - w / 2),
            Side::Full | Side::OtherMonitor => (wa.l, w),
        };
        unsafe {
            ShowWindow(h, 9); // a maximised window ignores SetWindowPos
            SetWindowPos(h, 0, x, wa.t, cx, ht, 0x0040); // SWP_SHOWWINDOW
            if matches!(side, Side::Full | Side::OtherMonitor) {
                ShowWindow(h, 3); // SW_MAXIMIZE on the monitor it now sits on
            }
        }
        bring_forward(h);
    }

    /// Wait for a just-launched app's window to show up.
    fn wait_window(app: &str) -> Option<Win> {
        for _ in 0..120 {
            if let Some(w) = find_window(app) {
                return Some(w);
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        None
    }

    fn open(app: &str) -> Result<Win, String> {
        if let Some(w) = find_window(app) {
            bring_forward(w.hwnd);
            return Ok(w);
        }
        let apps = start_apps();
        let hit =
            best(app, &apps, |a| (a.name.as_str(), "")).ok_or(format!("app non trovata: {app}"))?;
        crate::log(&format!(
            "[PcControl] launch {:?} ({})",
            hit.name, hit.app_id
        ));
        std::process::Command::new("explorer.exe")
            .arg(format!("shell:AppsFolder\\{}", hit.app_id))
            .spawn()
            .map_err(|e| e.to_string())?;
        wait_window(app)
            .or_else(|| wait_window(&hit.name))
            .ok_or(format!("{} non si è aperta in 6 s", hit.name))
    }

    /// Put `h` in front and give it time to take the keyboard. A window
    /// already in front gets its keys at once.
    fn front_for_keys(h: Hwnd) {
        if unsafe { GetForegroundWindow() } == h {
            return;
        }
        bring_forward(h);
        std::thread::sleep(std::time::Duration::from_millis(120));
    }

    fn need(app: &str) -> Result<Win, String> {
        find_window(app).ok_or(format!("nessuna finestra aperta per \"{app}\""))
    }

    fn aqs_date(when: When) -> &'static str {
        match when {
            When::Any => "",
            When::Today => "datemodified:today",
            When::Yesterday => "datemodified:yesterday",
            When::ThisWeek => "datemodified:this week",
            When::LastWeek => "datemodified:last week",
        }
    }

    fn find_files(query: &str, when: When) -> Result<(), String> {
        let q = format!("{} {}", query.trim(), aqs_date(when))
            .trim()
            .to_string();
        let home = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into());
        let enc = |s: &str| {
            s.bytes()
                .map(|b| match b {
                    b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b':' | b'\\' => {
                        (b as char).to_string()
                    }
                    _ => format!("%{b:02X}"),
                })
                .collect::<String>()
        };
        let uri = format!("search-ms:query={}&crumb=location:{}", enc(&q), enc(&home));
        crate::log(&format!("[PcControl] explorer {uri}"));
        std::process::Command::new("explorer.exe")
            .arg(uri)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    /// Do one step. `Ok(Some(text))` = the host should paste `text` now.
    pub fn act(step: &Step) -> Result<Option<String>, String> {
        match step {
            Step::Open { app } => open(app).map(|_| None),
            Step::Focus { app } => need(app).map(|w| {
                bring_forward(w.hwnd);
                None
            }),
            Step::Place { app, side } => {
                let w = need(app).or_else(|_| open(app))?;
                place(w.hwnd, *side);
                Ok(None)
            }
            Step::Tile { left, right } => {
                let l = need(left).or_else(|_| open(left))?;
                let r = need(right).or_else(|_| open(right))?;
                place(r.hwnd, Side::Right);
                place(l.hwnd, Side::Left);
                Ok(None)
            }
            Step::FindFiles { query, when } => find_files(query, *when).map(|_| None),
            Step::Minimize { app } => need(app).map(|w| {
                unsafe { ShowWindow(w.hwnd, 6) }; // SW_MINIMIZE
                None
            }),
            Step::Restore { app } => need(app).map(|w| {
                unsafe { ShowWindow(w.hwnd, 9) }; // SW_RESTORE
                bring_forward(w.hwnd);
                None
            }),
            Step::ShowDesktop => {
                chord(&[0x5B, 0x44]); // Win+D
                Ok(None)
            }
            Step::Web { site, query } => {
                let url = super::web_url(site, query).ok_or("niente da aprire")?;
                // The tab this session opened takes the next address: "cerca
                // su YouTube", then the words, is one tab, not two.
                let same_tab = super::own_tab() && front_is_browser();
                crate::log(&format!("[PcControl] web {url} same_tab={same_tab}"));
                if same_tab {
                    navigate_tab(&url);
                    Ok(None)
                } else {
                    shell_open(&url)
                }
            }
            Step::Browser { action } => {
                let keys: &[u8] = match action.as_str() {
                    "new_tab" => &[0x11, 0x54],
                    "close_tab" => &[0x11, 0x57],
                    "reopen_tab" => &[0x11, 0x10, 0x54],
                    "back" => &[0x12, 0x25],
                    "forward" => &[0x12, 0x27],
                    "reload" => &[0x74],
                    "next_tab" => &[0x11, 0x09],
                    "previous_tab" => &[0x11, 0x10, 0x09],
                    other => return Err(format!("azione browser sconosciuta: {other}")),
                };
                let w = open_windows()
                    .into_iter()
                    .find(|w| BROWSERS.iter().any(|b| w.process.eq_ignore_ascii_case(b)))
                    .ok_or("nessun browser aperto")?;
                front_for_keys(w.hwnd);
                chord(keys);
                Ok(None)
            }
            Step::Media { action } => {
                let (vk, times) = match action.as_str() {
                    "play_pause" => (0xB3, 1),
                    "next" => (0xB0, 1),
                    "previous" => (0xB1, 1),
                    "volume_up" => (0xAF, 5),
                    "volume_down" => (0xAE, 5),
                    "mute" => (0xAD, 1),
                    other => return Err(format!("azione media sconosciuta: {other}")),
                };
                for _ in 0..times {
                    chord(&[vk]);
                }
                Ok(None)
            }
            Step::Key { app, action } => {
                let keys = super::key_chord(action)
                    .ok_or_else(|| format!("tasto sconosciuto: {action}"))?;
                if !app.trim().is_empty() {
                    front_for_keys(need(app)?.hwnd);
                }
                chord(&keys);
                Ok(None)
            }
            Step::Type { text } => Ok(Some(text.clone())),
            Step::Click { item, double } => {
                crate::pc_appcmds::click_listed(*item, *double).map(|_| None)
            }
            Step::Close { app } => need(app).map(|w| {
                unsafe { PostMessageW(w.hwnd, 0x0010, 0, 0) }; // WM_CLOSE
                None
            }),
            Step::SwitchWindow => {
                let w = open_windows()
                    .into_iter()
                    .nth(1)
                    .ok_or("non c'è un'altra finestra")?;
                bring_forward(w.hwnd);
                Ok(None)
            }
            Step::OpenFolder { folder } => {
                if folder == "latest_download" {
                    let dir =
                        std::path::PathBuf::from(std::env::var("USERPROFILE").unwrap_or_default())
                            .join("Downloads");
                    let newest = std::fs::read_dir(&dir)
                        .map_err(|e| e.to_string())?
                        .filter_map(Result::ok)
                        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                        .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
                        .ok_or("nessun file in Download")?;
                    return std::process::Command::new("explorer.exe")
                        .arg(format!("/select,{}", newest.path().display()))
                        .spawn()
                        .map(|_| None)
                        .map_err(|e| e.to_string());
                }
                let shell = match folder.as_str() {
                    "downloads" => "shell:Downloads",
                    "documents" => "shell:Personal",
                    "desktop" => "shell:Desktop",
                    "pictures" => "shell:My Pictures",
                    "music" => "shell:My Music",
                    "videos" => "shell:My Video",
                    other => return Err(format!("cartella sconosciuta: {other}")),
                };
                shell_open(shell)
            }
            Step::Settings { page } => {
                let uri = match page.as_str() {
                    "bluetooth" => "ms-settings:bluetooth",
                    "wifi" => "ms-settings:network-wifi",
                    "sound" => "ms-settings:sound",
                    "display" => "ms-settings:display",
                    "notifications" => "ms-settings:notifications",
                    other => return Err(format!("impostazione sconosciuta: {other}")),
                };
                shell_open(uri)
            }
            Step::Screenshot => {
                chord(&[0x5B, 0x10, 0x53]); // Win+Shift+S
                Ok(None)
            }
            Step::AppCommand { label } => {
                let cmd = crate::pc_appcmds::find(label)
                    .ok_or(format!("l'app non ha il comando \"{label}\""))?;
                if cmd.risky {
                    return Err(format!(
                        "\"{}\" è un comando delicato: non lo eseguo a voce",
                        cmd.name
                    ));
                }
                crate::pc_appcmds::run(&cmd).map(|_| None)
            }
            Step::TypeAtEnd { app, text } => {
                let w = need(app)?;
                front_for_keys(w.hwnd);
                tap(0x11, false); // Ctrl
                tap(0x23, false); // End
                tap(0x23, true);
                tap(0x11, true);
                Ok(Some(text.clone()))
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
mod hands {
    use super::Step;

    pub struct Win {
        pub process: String,
        pub title: String,
    }

    pub fn open_windows() -> Vec<Win> {
        Vec::new()
    }

    pub fn start_apps() -> Vec<()> {
        Vec::new()
    }

    pub fn start_app_names() -> Vec<String> {
        Vec::new()
    }

    pub fn act(_step: &Step) -> Result<Option<String>, String> {
        Err("PC actions are Windows-only in this experiment".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // PRIVACY.md: the log holds "technical messages, not your transcripts".
    // A plan carries what was said: the log gets the names of its steps.
    #[test]
    fn a_plan_in_the_log_names_its_steps_and_none_of_the_words() {
        let plan = vec![
            Step::Web {
                site: "youtube".into(),
                query: "gatti che suonano il pianoforte".into(),
            },
            Step::Type {
                text: "caro Mario ti scrivo".into(),
            },
            Step::Click {
                item: 3,
                double: false,
            },
        ];
        let logged = kinds(&plan);
        assert_eq!(logged, "[web,type,click]");
        for word in ["gatti", "pianoforte", "Mario", "youtube"] {
            assert!(!logged.contains(word), "{word} is in the log: {logged}");
        }
    }

    #[test]
    fn an_app_is_named_as_a_person_says_it() {
        assert_eq!(say_name("WINWORD", "Documento1 - Word"), "Word");
        assert_eq!(say_name("Spotify", "Spotify Premium"), "Spotify");
        assert_eq!(
            say_name("chrome", "Una pagina - con un titolo molto lungo davvero"),
            "chrome"
        );
    }

    #[test]
    fn a_plan_parses_even_inside_a_code_fence() {
        let raw =
            "```json\n{\"steps\":[{\"do\":\"tile\",\"left\":\"Teams\",\"right\":\"Outlook\"},\
                   {\"do\":\"place\",\"app\":\"notepad++\",\"side\":\"other_monitor\"},\
                   {\"do\":\"find_files\",\"when\":\"yesterday\"}],\"say\":\"Fatto\"}\n```";
        let p = parse_plan(raw).unwrap();
        assert_eq!(p.say, "Fatto");
        assert_eq!(
            p.steps,
            vec![
                Step::Tile {
                    left: "Teams".into(),
                    right: "Outlook".into()
                },
                Step::Place {
                    app: "notepad++".into(),
                    side: Side::OtherMonitor
                },
                Step::FindFiles {
                    query: String::new(),
                    when: When::Yesterday
                },
            ]
        );
    }

    #[test]
    fn web_steps_build_the_right_address() {
        assert_eq!(
            web_url("youtube", "lo-fi beats").unwrap(),
            "https://www.youtube.com/results?search_query=lo-fi%20beats"
        );
        assert_eq!(web_url("YouTube", "").unwrap(), "https://www.youtube.com");
        assert_eq!(
            web_url("", "meteo milano").unwrap(),
            "https://www.google.com/search?q=meteo%20milano"
        );
        assert_eq!(web_url("gmail", "").unwrap(), "https://mail.google.com");
    }

    #[test]
    fn live_runs_only_what_a_click_undoes() {
        let one = |s: Step| Plan {
            say: String::new(),
            steps: vec![s],
        };
        assert!(live_safe(&one(Step::Media {
            action: "play_pause".into()
        })));
        assert!(live_safe(&one(Step::Browser {
            action: "new_tab".into()
        })));
        assert!(!live_safe(&one(Step::Browser {
            action: "close_tab".into()
        })));
        assert!(!live_safe(&one(Step::Key {
            app: String::new(),
            action: "paste".into()
        })));
    }

    #[test]
    fn an_unknown_action_is_refused_not_guessed() {
        assert!(parse_plan(r#"{"steps":[{"do":"delete","path":"C:\\"}]}"#).is_err());
        assert!(parse_plan("sorry, I can't").is_err());
    }

    #[test]
    fn spoken_names_find_their_window() {
        assert!(name_score("notepad", "notepad++", "nuovo 2 - Notepad++") >= 0.9);
        assert!(name_score("Notepad++", "notepad++", "") >= 0.9);
        assert!(
            name_score("la mail", "olk", "Mail - Outlook") > 0.0
                || name_score("mail", "olk", "") > 0.9
        );
        assert!(name_score("teams", "ms-teams", "Chat | Microsoft Teams") >= 0.8);
        assert!(
            name_score("tims", "teams", "") > 0.0,
            "a close mishearing still matches"
        );
        assert_eq!(name_score("excel", "notepad++", "nuovo 2"), 0.0);
        // A live partial cut mid-word ("Apri no…") must not name an app yet.
        assert_eq!(name_score("no", "notepad++", "nuovo 2 - Notepad++"), 0.0);
        assert_eq!(name_score("not", "notepad++", ""), 0.0);
    }
    #[test]
    fn key_chords_are_named_keys_or_refused() {
        assert_eq!(key_chord("copy"), Some(vec![0x11, 0x43]));
        assert_eq!(key_chord("enter"), Some(vec![0x0D]));
        assert_eq!(key_chord("ctrl+shift+t"), Some(vec![0x11, 0x10, 0x54]));
        assert_eq!(key_chord("alt+f4"), Some(vec![0x12, 0x73]));
        assert_eq!(key_chord("f12"), Some(vec![0x7B]));
        assert_eq!(key_chord("win"), Some(vec![0x5B]));
        assert_eq!(key_chord("Ctrl+S"), Some(vec![0x11, 0x53]));
        for bad in [
            "",
            "banana",
            "ctrl+banana",
            "s+ctrl",
            "ctrl+alt",
            "a+b",
            "ctrl+alt+shift+win+s",
            "f13",
        ] {
            assert_eq!(key_chord(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_planner_can_close_switch_and_type() {
        let plan = parse_plan(
            r#"{"steps":[{"do":"close","app":"notepad"},{"do":"close"},{"do":"switch_window"},{"do":"type","text":"ciao"},{"do":"key","action":"ctrl+shift+t"}],"say":"ok"}"#,
        )
        .unwrap();
        assert_eq!(
            plan.steps,
            vec![
                Step::Close {
                    app: "notepad".into()
                },
                Step::Close { app: String::new() },
                Step::SwitchWindow,
                Step::Type {
                    text: "ciao".into()
                },
                Step::Key {
                    app: String::new(),
                    action: "ctrl+shift+t".into()
                },
            ]
        );
    }

    #[test]
    fn only_harmless_steps_run_before_the_user_is_done() {
        let live = |s: Step| {
            live_safe(&Plan {
                steps: vec![s],
                say: String::new(),
            })
        };
        let key = |a: &str| Step::Key {
            app: String::new(),
            action: a.into(),
        };
        assert!(live(key("copy")));
        assert!(live(key("page_down")));
        assert!(live(Step::SwitchWindow));
        for step in [
            key("save"),
            key("enter"),
            key("delete"),
            key("alt+f4"),
            Step::Close { app: String::new() },
            Step::Type { text: "x".into() },
        ] {
            assert!(!live(step.clone()), "{step:?}");
        }
    }
    // The user's own case (2026-11-10): "cerca su YouTube" opened YouTube,
    // and the words said next opened a second tab, on Google.
    #[test]
    #[serial_test::serial]
    fn words_after_a_site_are_a_search_on_it() {
        reset_context();
        assert!(
            search_in_context("gatti che suonano").is_none(),
            "no site yet"
        );
        note_step(&Step::Web {
            site: "YouTube".into(),
            query: String::new(),
        });
        assert_eq!(last_site(), "youtube");
        let steps = |said: &str| search_in_context(said).map(|p| p.steps);
        let search = |q: &str| {
            Some(vec![Step::Web {
                site: "youtube".into(),
                query: q.into(),
            }])
        };
        assert_eq!(
            steps("Gatti che suonano il piano."),
            search("Gatti che suonano il piano")
        );
        assert_eq!(steps("cerca gatti"), search("gatti"));
        assert_eq!(steps("clicca il terzo video"), None);
        assert_eq!(steps("apri Notepad"), None);
        // The search ran: the tab is ours, and words alone are no search now.
        note_step(&Step::Web {
            site: "youtube".into(),
            query: "gatti".into(),
        });
        assert!(own_tab());
        assert_eq!(steps("cani"), None);
        assert_eq!(steps("cerca cani"), search("cani"));
        // A click on the page keeps the tab; another app in front does not.
        note_step(&Step::AppCommand { label: "x".into() });
        assert!(own_tab());
        note_step(&Step::Open {
            app: "notepad".into(),
        });
        assert!(!own_tab());
        assert_eq!(last_app(), "notepad");
        reset_context();
    }

    #[test]
    #[serial_test::serial]
    fn the_planner_gets_the_session_and_the_page_in_order() {
        reset_context();
        remember("vai su YouTube", "Apro youtube");
        let history = context().history.clone();
        let prompt = planner_prompt(
            "clicca il terzo video",
            &[],
            "chrome",
            &["Home".to_string(), "Gatti buffi".to_string()],
            true,
            &history,
        );
        assert!(prompt.contains("\"vai su YouTube\" -> Apro youtube"));
        assert!(prompt.contains("in reading order"));
        assert!(prompt.contains("Gatti buffi"));
        assert!(wants_listing("clicca il terzo video"));
        assert!(wants_listing("apri l'ultimo risultato"));
        assert!(!wants_listing("apri Notepad"));
        for i in 0..10 {
            remember(&format!("comando {i}"), "ok");
        }
        assert_eq!(context().history.len(), 6);
        reset_context();
    }
}
