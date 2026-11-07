//! Local intent router for PC commands — EXPERIMENT (branch `exp/pc-actions`).
//!
//! multilingual-e5-small (int8 ONNX, ~118 MB) embeds the spoken command and a
//! handful of hand-written examples per action; the nearest example decides
//! the action. Measured offline on 33 Italian/English commands: 29 right at
//! ~18 ms CPU, and the 4 misses had a margin of 0.01-0.02 between the first
//! and the second action. So: below `MIN_MARGIN`, or when a slot can't be
//! filled, the router answers `None` and the command goes to Claude.
//!
//! Slots without a model: side and date from keywords, app names by fuzzy
//! match of the command's words against open windows and Start-menu apps.

use crate::pc_control::{name_score, norm, Plan, Side, Step, When};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::sync::Mutex;
use tokenizers::Tokenizer;

const MIN_MARGIN: f32 = 0.03;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Intent {
    Open,
    Tile,
    Move,
    FindFiles,
    Minimize,
    Web,
    Browser,
    Media,
    Folder,
    Settings,
}

const EXAMPLES: &[(Intent, &str)] = &[
    (Intent::Open, "apri Word"),
    (Intent::Open, "avvia Chrome"),
    (Intent::Open, "fai partire Outlook"),
    (Intent::Open, "apri la calcolatrice"),
    (Intent::Open, "apri gmail nel browser"),
    (Intent::Open, "voglio Slack"),
    (Intent::Open, "open Notepad"),
    (Intent::Open, "launch Teams"),
    (Intent::Open, "bring up the calculator"),
    (Intent::Tile, "affianca Excel e il browser"),
    (
        Intent::Tile,
        "metti una accanto all'altra la chat e la mail",
    ),
    (Intent::Tile, "dividi lo schermo in due"),
    (Intent::Tile, "Word a sinistra e Chrome a destra"),
    (Intent::Tile, "metti le due finestre affiancate"),
    (Intent::Tile, "tile Outlook and Teams"),
    (Intent::Tile, "show these two windows next to each other"),
    (Intent::Tile, "snap two windows side by side"),
    (Intent::Move, "sposta Word a sinistra"),
    (Intent::Move, "porta la finestra a destra"),
    (Intent::Move, "ingrandisci Chrome"),
    (Intent::Move, "metti Teams a tutto schermo"),
    (Intent::Move, "manda Spotify sul secondo schermo"),
    (Intent::Move, "sposta questa finestra sul portatile"),
    (Intent::Move, "maximize Excel"),
    (Intent::Move, "move Teams to the left"),
    (Intent::Move, "put this on the second monitor"),
    (Intent::FindFiles, "cerca i file che ho modificato oggi"),
    (Intent::FindFiles, "trova il contratto di Bianchi"),
    (Intent::FindFiles, "dov'è il file del bilancio"),
    (Intent::FindFiles, "cerca le foto di ieri"),
    (Intent::FindFiles, "trovami l'excel delle spese"),
    (Intent::FindFiles, "mostrami i documenti recenti"),
    (Intent::FindFiles, "search my files for the invoice"),
    (Intent::FindFiles, "find the report I saved this morning"),
    (Intent::Minimize, "riduci a icona Word"),
    (Intent::Minimize, "abbassa Outlook"),
    (Intent::Minimize, "nascondi Chrome"),
    (Intent::Minimize, "manda Teams nella barra"),
    (Intent::Minimize, "togli di mezzo Excel"),
    (Intent::Minimize, "minimize Slack"),
    (Intent::Minimize, "hide the browser"),
    (Intent::Web, "naviga su YouTube"),
    (Intent::Web, "vai su Amazon"),
    (Intent::Web, "cerca su Google il meteo di domani"),
    (Intent::Web, "cercami su YouTube un tutorial di Excel"),
    (Intent::Web, "apri Gmail"),
    (Intent::Web, "cerca su Wikipedia Leonardo da Vinci"),
    (Intent::Web, "search YouTube for lo-fi music"),
    (Intent::Web, "go to Amazon"),
    (Intent::Web, "google the weather in Milan"),
    (Intent::Browser, "apri una nuova scheda"),
    (Intent::Browser, "apri una nuova tab"),
    (Intent::Browser, "chiudi questa scheda"),
    (Intent::Browser, "torna indietro"),
    (Intent::Browser, "vai avanti"),
    (Intent::Browser, "ricarica la pagina"),
    (Intent::Browser, "riapri la scheda chiusa"),
    (Intent::Browser, "scheda successiva"),
    (Intent::Browser, "new tab"),
    (Intent::Browser, "go back"),
    (Intent::Browser, "refresh the page"),
    (Intent::Media, "metti in pausa"),
    (Intent::Media, "riprendi la musica"),
    (Intent::Media, "canzone successiva"),
    (Intent::Media, "torna alla canzone precedente"),
    (Intent::Media, "alza il volume"),
    (Intent::Media, "abbassa il volume"),
    (Intent::Media, "togli l'audio"),
    (Intent::Media, "pause the music"),
    (Intent::Media, "next song"),
    (Intent::Media, "turn the volume down"),
    (Intent::Folder, "apri la cartella download"),
    (Intent::Folder, "aprimi i documenti"),
    (Intent::Folder, "mostrami l'ultimo file scaricato"),
    (Intent::Folder, "apri la cartella immagini"),
    (Intent::Folder, "apri il desktop in esplora file"),
    (Intent::Folder, "open my downloads folder"),
    (Intent::Folder, "show me the last download"),
    (Intent::Settings, "apri le impostazioni bluetooth"),
    (Intent::Settings, "impostazioni del wifi"),
    (Intent::Settings, "apri le impostazioni audio"),
    (Intent::Settings, "impostazioni dello schermo"),
    (Intent::Settings, "open bluetooth settings"),
    (Intent::Settings, "open sound settings"),
];

const SITES: &[&str] = &[
    "youtube",
    "google",
    "amazon",
    "wikipedia",
    "gmail",
    "maps",
    "mappe",
    "netflix",
    "github",
    "linkedin",
    "facebook",
    "instagram",
    "spotify",
    "reddit",
];

const WEB_STOP: &[&str] = &[
    "cerca", "cercami", "search", "google", "su", "on", "for", "vai", "naviga", "navika", "apri",
    "open", "go", "to", "sito", "il", "la", "lo", "i", "le", "un", "una", "di", "del", "della",
    "in", "me", "mi", "the", "a", "an",
];

fn web_of(w: &[String]) -> Option<(String, String)> {
    let site = w.iter().find(|t| SITES.contains(&t.as_str())).cloned();
    let query: Vec<&str> = w
        .iter()
        .map(String::as_str)
        .filter(|t| !WEB_STOP.contains(t) && !SITES.contains(t))
        .collect();
    let searching = w
        .iter()
        .any(|t| ["cerca", "cercami", "search", "google"].contains(&t.as_str()));
    match site {
        Some(s) => Some((s, query.join(" "))),
        None if searching && !query.is_empty() => Some(("google".into(), query.join(" "))),
        None => None,
    }
}

fn browser_action_of(w: &[String]) -> Option<&'static str> {
    let has = |x: &str| w.iter().any(|t| t == x);
    let tab = has("scheda") || has("tab");
    if has("riapri") || has("reopen") {
        Some("reopen_tab")
    } else if tab && (has("nuova") || has("new")) {
        Some("new_tab")
    } else if tab && (has("chiudi") || has("close")) {
        Some("close_tab")
    } else if tab && (has("successiva") || has("prossima") || has("next")) {
        Some("next_tab")
    } else if tab && (has("precedente") || has("previous")) {
        Some("previous_tab")
    } else if has("indietro") || has("back") {
        Some("back")
    } else if has("avanti") || has("forward") {
        Some("forward")
    } else if has("ricarica") || has("aggiorna") || has("reload") || has("refresh") {
        Some("reload")
    } else {
        None
    }
}

fn media_action_of(w: &[String]) -> Option<&'static str> {
    let has = |x: &str| w.iter().any(|t| t == x);
    let volume = has("volume");
    if has("muto") || has("mute") || has("silenzia") || (has("togli") && has("audio")) {
        Some("mute")
    } else if volume && (has("alza") || has("su") || has("up") || has("aumenta")) {
        Some("volume_up")
    } else if volume && (has("abbassa") || has("giù") || has("down") || has("diminuisci")) {
        Some("volume_down")
    } else if has("pausa") || has("pause") || has("play") || has("riprendi") || has("ferma") {
        Some("play_pause")
    } else if has("successiva") || has("prossima") || has("next") || has("skip") || has("salta") {
        Some("next")
    } else if has("precedente") || has("previous") {
        Some("previous")
    } else {
        None
    }
}

fn folder_of(w: &[String]) -> Option<&'static str> {
    let has = |x: &str| w.iter().any(|t| t == x);
    let download = has("download") || has("downloads") || has("scaricato") || has("scaricati");
    if download && (has("ultimo") || has("last") || has("latest")) {
        Some("latest_download")
    } else if download {
        Some("downloads")
    } else if has("documenti") || has("documents") {
        Some("documents")
    } else if has("desktop") || has("scrivania") {
        Some("desktop")
    } else if has("immagini") || has("foto") || has("pictures") || has("photos") {
        Some("pictures")
    } else if has("musica") || has("music") {
        Some("music")
    } else if has("video") || has("videos") {
        Some("videos")
    } else {
        None
    }
}

fn settings_of(w: &[String]) -> Option<&'static str> {
    let has = |x: &str| w.iter().any(|t| t == x);
    if has("bluetooth") {
        Some("bluetooth")
    } else if has("wifi") || (has("wi") && has("fi")) || has("rete") {
        Some("wifi")
    } else if has("audio") || has("suono") || has("sound") {
        Some("sound")
    } else if has("schermo") || has("display") || has("luminosità") {
        Some("display")
    } else if has("notifiche") || has("notifications") {
        Some("notifications")
    } else {
        None
    }
}

/// Words that carry the action, place or time, never an app name or a query.
const STOP: &[&str] = &[
    "apri",
    "aprimi",
    "avvia",
    "lancia",
    "fai",
    "partire",
    "metti",
    "mettilo",
    "mettila",
    "sposta",
    "spostala",
    "spostalo",
    "porta",
    "portami",
    "affianca",
    "affiancate",
    "affiancati",
    "dividi",
    "ingrandisci",
    "massimizza",
    "manda",
    "voglio",
    "mostrami",
    "trova",
    "trovami",
    "cerca",
    "cercami",
    "dove",
    "dov",
    "ho",
    "messo",
    "lo",
    "la",
    "il",
    "i",
    "le",
    "gli",
    "l",
    "e",
    "ed",
    "a",
    "al",
    "alla",
    "allo",
    "sulla",
    "sul",
    "su",
    "sull",
    "tra",
    "fra",
    "uno",
    "una",
    "accanto",
    "all",
    "altro",
    "altra",
    "schermo",
    "intero",
    "tutto",
    "sinistra",
    "destra",
    "monitor",
    "secondo",
    "finestra",
    "finestre",
    "questa",
    "questo",
    "in",
    "di",
    "del",
    "della",
    "dei",
    "delle",
    "da",
    "che",
    "per",
    "con",
    "mi",
    "me",
    "due",
    "open",
    "start",
    "launch",
    "put",
    "move",
    "send",
    "bring",
    "show",
    "tile",
    "split",
    "find",
    "search",
    "where",
    "is",
    "the",
    "to",
    "and",
    "side",
    "by",
    "left",
    "right",
    "screen",
    "full",
    "other",
    "second",
    "window",
    "windows",
    "this",
    "my",
    "for",
    "of",
    "on",
    "up",
    "maximize",
    "next",
    "each",
    "me",
    "riduci",
    "icona",
    "abbassa",
    "abbassala",
    "abbassalo",
    "nascondi",
    "nascondila",
    "nascondilo",
    "togli",
    "mezzo",
    "barra",
    "minimize",
    "hide",
    "puoi",
    "aprirmi",
    "can",
    "you",
];

const DATE_WORDS: &[&str] = &[
    "oggi",
    "ieri",
    "stamattina",
    "settimana",
    "scorsa",
    "today",
    "yesterday",
    "week",
    "last",
    "morning",
    "this",
    "questa",
];

const FILE_WORDS: &[&str] = &[
    "file",
    "files",
    "documenti",
    "documento",
    "documents",
    "modificati",
    "modificato",
    "salvato",
    "scaricato",
    "scaricati",
    "edited",
    "saved",
];

fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric() && c != '+')
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect()
}

fn side_of(w: &[String]) -> Option<Side> {
    let has = |x: &str| w.iter().any(|t| t == x);
    let pair = |a: &str, b: &str| w.windows(2).any(|p| p[0] == a && p[1] == b);
    if has("sinistra") || has("left") {
        Some(Side::Left)
    } else if has("destra") || has("right") {
        Some(Side::Right)
    } else if pair("altro", "monitor")
        || pair("altro", "schermo")
        || pair("secondo", "schermo")
        || pair("other", "monitor")
        || pair("other", "screen")
        || pair("second", "monitor")
        || pair("second", "screen")
        || has("portatile")
        || has("laptop")
    {
        Some(Side::OtherMonitor)
    } else if pair("schermo", "intero")
        || pair("tutto", "schermo")
        || pair("full", "screen")
        || has("massimizza")
        || has("ingrandisci")
        || has("maximize")
    {
        Some(Side::Full)
    } else {
        None
    }
}

fn when_of(w: &[String]) -> When {
    let has = |x: &str| w.iter().any(|t| t == x);
    let pair = |a: &str, b: &str| w.windows(2).any(|p| p[0] == a && p[1] == b);
    if has("ieri") || has("yesterday") {
        When::Yesterday
    } else if has("oggi") || has("stamattina") || has("today") || pair("this", "morning") {
        When::Today
    } else if pair("settimana", "scorsa") || pair("last", "week") {
        When::LastWeek
    } else if pair("questa", "settimana") || pair("this", "week") {
        When::ThisWeek
    } else {
        When::Any
    }
}

/// App names said in the command, in order, matched against `names`
/// (open windows' processes and titles, Start-menu apps).
fn apps_in(w: &[String], names: &[(String, String)]) -> Vec<String> {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < w.len() {
        if STOP.contains(&w[i].as_str()) {
            i += 1;
            continue;
        }
        // Longest n-gram (up to 3 content words) that names an app.
        let mut taken = 0;
        for n in (1..=3).rev() {
            if i + n > w.len() || w[i..i + n].iter().any(|t| STOP.contains(&t.as_str())) {
                continue;
            }
            let gram = w[i..i + n].join(" ");
            let best = names
                .iter()
                .map(|(name, title)| (name_score(&gram, name, title), name))
                .max_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((score, name)) = best {
                if score >= 0.8 {
                    found.push((gram, name.to_lowercase()));
                    taken = n;
                    break;
                }
            }
        }
        i += taken.max(1);
    }
    // "Apri Chrom, apri Chrome": two spellings of one app are one app.
    let mut out: Vec<String> = Vec::new();
    let mut targets: Vec<String> = Vec::new();
    for (g, target) in found {
        if !targets.contains(&target) {
            targets.push(target);
            out.push(g);
        }
    }
    out
}

struct Router {
    session: Session,
    tok: Tokenizer,
    examples: Vec<(Intent, Vec<f32>)>,
    needs_token_types: bool,
}

static ROUTER: Mutex<Option<Router>> = Mutex::new(None);

fn model_dir() -> Option<std::path::PathBuf> {
    // The override lets the ignored accuracy test point at the real files
    // (tests run against an isolated config dir).
    if let Ok(d) = std::env::var("DIMMY_PC_ROUTER_DIR") {
        return Some(d.into());
    }
    crate::config_dir_path().map(|p| p.join("pc-router"))
}

impl Router {
    fn load() -> Result<Self, String> {
        let dir = model_dir().ok_or("no config dir")?;
        let tok = Tokenizer::from_file(dir.join("tokenizer.json"))
            .map_err(|e| format!("tokenizer: {e}"))?;
        let session = Session::builder()
            .map_err(|e| format!("ort builder: {e}"))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| format!("ort opt: {e}"))?
            .with_intra_threads(2)
            .map_err(|e| format!("ort threads: {e}"))?
            .commit_from_file(dir.join("model_quantized.onnx"))
            .map_err(|e| format!("ort load: {e}"))?;
        let needs_token_types = session.inputs.iter().any(|i| i.name == "token_type_ids");
        let mut r = Router {
            session,
            tok,
            examples: Vec::new(),
            needs_token_types,
        };
        let mut ex = Vec::with_capacity(EXAMPLES.len());
        for (intent, text) in EXAMPLES {
            ex.push((*intent, r.embed(text)?));
        }
        r.examples = ex;
        Ok(r)
    }

    fn embed(&mut self, text: &str) -> Result<Vec<f32>, String> {
        let enc = self
            .tok
            .encode(format!("query: {text}"), true)
            .map_err(|e| format!("encode: {e}"))?;
        let ids: Vec<i64> = enc.get_ids().iter().map(|&x| x as i64).collect();
        let mask: Vec<i64> = enc.get_attention_mask().iter().map(|&x| x as i64).collect();
        let n = ids.len();
        assert!(n > 0, "the tokenizer always adds <s> and </s>");
        let shape = vec![1i64, n as i64];
        let ids_t = Tensor::from_array((shape.clone(), ids)).map_err(|e| e.to_string())?;
        let mask_t =
            Tensor::from_array((shape.clone(), mask.clone())).map_err(|e| e.to_string())?;
        let outs = if self.needs_token_types {
            let tt = Tensor::from_array((shape, vec![0i64; n])).map_err(|e| e.to_string())?;
            self.session.run(ort::inputs! {
                "input_ids" => ids_t, "attention_mask" => mask_t, "token_type_ids" => tt,
            })
        } else {
            self.session
                .run(ort::inputs! { "input_ids" => ids_t, "attention_mask" => mask_t })
        }
        .map_err(|e| format!("run: {e}"))?;
        let (dims, data) = outs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("extract: {e}"))?;
        let dims: Vec<i64> = dims.iter().copied().collect();
        if dims.len() != 3 || dims[1] as usize != n {
            return Err(format!("unexpected output shape {dims:?}"));
        }
        let hidden = dims[2] as usize;
        // Mean of the token vectors under the attention mask, then unit length.
        // (Dividing the sum by the token count is skipped: unit length
        // after the normalisation is the same vector.)
        let mut v = vec![0f32; hidden];
        for (t, m) in mask.iter().enumerate() {
            if *m == 0 {
                continue;
            }
            for (k, acc) in v.iter_mut().enumerate() {
                *acc += data[t * hidden + k];
            }
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
        for x in &mut v {
            *x /= norm;
        }
        let len = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            len.is_finite() && (len - 1.0).abs() < 1e-3,
            "embedding must be unit length"
        );
        Ok(v)
    }

    /// (best intent, margin to the runner-up intent)
    fn classify(&mut self, text: &str) -> Result<(Intent, f32), String> {
        let q = self.embed(text)?;
        let mut best: Vec<(Intent, f32)> = Vec::new();
        for (intent, e) in &self.examples {
            let sim: f32 = q.iter().zip(e).map(|(a, b)| a * b).sum();
            match best.iter_mut().find(|(i, _)| i == intent) {
                Some(slot) => slot.1 = slot.1.max(sim),
                None => best.push((*intent, sim)),
            }
        }
        best.sort_by(|a, b| b.1.total_cmp(&a.1));
        Ok((
            best[0].0,
            best[0].1 - best.get(1).map(|x| x.1).unwrap_or(0.0),
        ))
    }
}

/// Load the model in the background (the execute shortcut was just pressed).
pub fn warm() {
    std::thread::spawn(|| {
        let mut g = ROUTER.lock().unwrap_or_else(|e| e.into_inner());
        if g.is_some() {
            return;
        }
        let t = std::time::Instant::now();
        match Router::load() {
            Ok(r) => {
                *g = Some(r);
                crate::log(&format!(
                    "[PcRouter] e5 ready in {} ms",
                    t.elapsed().as_millis()
                ));
            }
            Err(e) => crate::log(&format!("[PcRouter] e5 unavailable: {e}")),
        }
    });
}

/// A plan when the router is sure, else `None` (the caller asks Claude).
/// `detail` says why, for the log.
pub fn route(command: &str, names: &[(String, String)]) -> (Option<Plan>, String) {
    let (plan, margin, detail) = route_scored(command, names);
    if margin < MIN_MARGIN {
        return (None, format!("{detail} below {MIN_MARGIN}"));
    }
    (plan, detail)
}

/// The plan the slots allow (whatever the margin), the margin, and why.
fn route_scored(command: &str, names: &[(String, String)]) -> (Option<Plan>, f32, String) {
    let mut g = ROUTER.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        match Router::load() {
            Ok(r) => *g = Some(r),
            Err(e) => return (None, 0.0, format!("e5 unavailable: {e}")),
        }
    }
    let r = g.as_mut().expect("just loaded");
    let (intent, margin) = match r.classify(command) {
        Ok(x) => x,
        Err(e) => return (None, 0.0, format!("e5 error: {e}")),
    };
    drop(g);
    let detail = format!("intent={intent:?} margin={margin:.3}");
    let w = words(command);
    let apps = apps_in(&w, names);
    let this_window = w
        .windows(2)
        .any(|p| (p[0] == "questa" && p[1] == "finestra") || (p[0] == "this" && p[1] == "window"));
    let plan = match intent {
        Intent::Open if apps.len() == 1 => {
            let mut steps = vec![Step::Open {
                app: apps[0].clone(),
            }];
            let mut say = format!("Apro {}", apps[0]);
            if let Some(side) = side_of(&w) {
                steps.push(Step::Place {
                    app: apps[0].clone(),
                    side,
                });
                say = format!("{say}, {}", side_label(side));
            }
            Some(Plan { steps, say })
        }
        Intent::Tile if apps.len() == 2 => Some(Plan {
            say: format!("{} e {} affiancati", apps[0], apps[1]),
            steps: vec![Step::Tile {
                left: apps[0].clone(),
                right: apps[1].clone(),
            }],
        }),
        Intent::Move => match (side_of(&w), apps.as_slice()) {
            (Some(side), [app]) => Some(Plan {
                say: format!("{app}: {}", side_label(side)),
                steps: vec![Step::Place {
                    app: app.clone(),
                    side,
                }],
            }),
            (Some(side), []) if this_window => Some(Plan {
                say: format!("Finestra: {}", side_label(side)),
                steps: vec![Step::Place {
                    app: String::new(),
                    side,
                }],
            }),
            _ => None,
        },
        Intent::Minimize if apps.len() == 1 => Some(Plan {
            say: format!("{} ridotto a icona", apps[0]),
            steps: vec![Step::Minimize {
                app: apps[0].clone(),
            }],
        }),
        Intent::Web => web_of(&w).map(|(site, query)| Plan {
            say: if query.is_empty() {
                format!("Apro {site}")
            } else {
                format!("Cerco \"{query}\" su {site}")
            },
            steps: vec![Step::Web { site, query }],
        }),
        Intent::Browser => browser_action_of(&w).map(|a| Plan {
            say: format!("Browser: {a}"),
            steps: vec![Step::Browser { action: a.into() }],
        }),
        Intent::Media => media_action_of(&w).map(|a| Plan {
            say: format!("Media: {a}"),
            steps: vec![Step::Media { action: a.into() }],
        }),
        Intent::Folder => folder_of(&w).map(|f| Plan {
            say: format!("Cartella: {f}"),
            steps: vec![Step::OpenFolder { folder: f.into() }],
        }),
        Intent::Settings => settings_of(&w).map(|p| Plan {
            say: format!("Impostazioni: {p}"),
            steps: vec![Step::Settings { page: p.into() }],
        }),
        Intent::FindFiles => {
            let when = when_of(&w);
            let query: Vec<&str> = w
                .iter()
                .map(String::as_str)
                .filter(|t| !STOP.contains(t) && !DATE_WORDS.contains(t) && !FILE_WORDS.contains(t))
                .collect();
            Some(Plan {
                say: "Cerco i file".into(),
                steps: vec![Step::FindFiles {
                    query: query.join(" "),
                    when,
                }],
            })
        }
        _ => None,
    };
    match plan {
        Some(p) => (Some(p), margin, detail),
        None => (
            None,
            margin,
            format!("{detail} apps={apps:?}: slots incomplete"),
        ),
    }
}

// -- Shadow mode: would a live partial have been enough? --------------
//
// While an execute session records, every partial transcript is routed and
// the result kept. Nothing is executed. When the command runs for real, the
// log says, per candidate rule, when it would have fired, on which words,
// and whether its plan matches the one that ran.

/// Stricter than `MIN_MARGIN`: acting before the user is done must be surer.
const LIVE_MARGIN: f32 = 0.05;

struct Obs {
    ms: u128,
    text: String,
    plan: Option<Plan>,
    margin: f32,
}

struct Shadow {
    started: std::time::Instant,
    obs: Vec<Obs>,
    /// A live rule already ran the command: the stop must not run it again.
    fired: bool,
}

/// Live execution is on unless DIMMY_PC_LIVE=0.
fn live_enabled() -> bool {
    std::env::var("DIMMY_PC_LIVE")
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// True once a live partial ran the command in this session.
pub fn live_fired() -> bool {
    SHADOW
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|s| s.fired)
}

static SHADOW: Mutex<Option<Shadow>> = Mutex::new(None);

pub fn shadow_begin() {
    *SHADOW.lock().unwrap_or_else(|e| e.into_inner()) = Some(Shadow {
        started: std::time::Instant::now(),
        obs: Vec::new(),
        fired: false,
    });
}

/// A partial transcript arrived. Only an execute session that is still
/// recording is observed; the routing runs off the caller thread.
pub fn shadow_observe(cumulative: &str) {
    use crate::dictation_session::{Kind, Phase};
    if !matches!(
        crate::dictation_session::phase(),
        Phase::Recording {
            kind: Kind::Execute,
            ..
        }
    ) {
        return;
    }
    let text = cumulative.trim().to_string();
    let ms = {
        let g = SHADOW.lock().unwrap_or_else(|e| e.into_inner());
        match g.as_ref() {
            Some(s) if !text.is_empty() && s.obs.last().map(|o| o.text != text).unwrap_or(true) => {
                s.started.elapsed().as_millis()
            }
            _ => return,
        }
    };
    std::thread::spawn(move || {
        let (plan, margin, detail) = route_scored(&text, &crate::pc_control::known_names());
        let sure = plan.is_some() && margin >= LIVE_MARGIN;
        let mut stable = false;
        let mut fire = false;
        if let Some(s) = SHADOW.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            stable = sure
                && s.obs.last().is_some_and(|prev| {
                    prev.margin >= LIVE_MARGIN
                        && prev
                            .plan
                            .as_ref()
                            .zip(plan.as_ref())
                            .is_some_and(|(a, b)| same_plan(a, b))
                });
            s.obs.push(Obs {
                ms,
                text: text.clone(),
                plan: plan.clone(),
                margin,
            });
            s.obs.sort_by_key(|o| o.ms);
            // Claimed under the lock: one live run per session, and the
            // stop path sees it before it plans anything.
            if stable
                && !s.fired
                && live_enabled()
                && plan.as_ref().is_some_and(crate::pc_control::live_safe)
            {
                s.fired = true;
                fire = true;
            }
        }
        if fire {
            let plan = plan.clone().expect("fire implies a plan");
            let t = std::time::Instant::now();
            let result = crate::pc_control::run(&plan);
            crate::log(&format!(
                "[PcLive] fired at {ms} ms on {text:?}: {:?} -> {:?} in {} ms",
                plan.steps,
                result.as_ref().map(|_| "ok"),
                t.elapsed().as_millis()
            ));
            let fired = serde_json::json!({
                "say": plan.say,
                "text": text,
                "ok": result.is_ok(),
                "error": result.err(),
            });
            crate::ffi::emit_event("pc_live_fired", &fired.to_string());
        }
        // What the local model makes of the words so far, for the live view.
        let payload = serde_json::json!({
            "ms": ms as u64,
            "text": text,
            "detail": detail,
            "margin": margin,
            "say": plan.as_ref().map(|p| p.say.clone()),
            "sure": sure,
            "stable": stable,
        });
        crate::ffi::emit_event("pc_live", &payload.to_string());
    });
}

fn same_plan(a: &Plan, b: &Plan) -> bool {
    let app = |x: &str, y: &str| {
        x.is_empty() || y.is_empty() || name_score(x, y, "") >= 0.8 || name_score(y, x, "") >= 0.8
    };
    a.steps.len() == b.steps.len()
        && a.steps.iter().zip(&b.steps).all(|(x, y)| match (x, y) {
            (Step::Open { app: p }, Step::Open { app: q }) => app(p, q),
            (Step::Focus { app: p }, Step::Focus { app: q }) => app(p, q),
            (Step::Place { app: p, side: s }, Step::Place { app: q, side: t }) => {
                s == t && app(p, q)
            }
            (
                Step::Tile {
                    left: l1,
                    right: r1,
                },
                Step::Tile {
                    left: l2,
                    right: r2,
                },
            ) => app(l1, l2) && app(r1, r2),
            (Step::FindFiles { when: w1, .. }, Step::FindFiles { when: w2, .. }) => w1 == w2,
            (Step::TypeAtEnd { .. }, Step::TypeAtEnd { .. }) => true,
            (Step::Minimize { app: p }, Step::Minimize { app: q }) => app(p, q),
            (Step::Restore { app: p }, Step::Restore { app: q }) => app(p, q),
            (Step::ShowDesktop, Step::ShowDesktop) => true,
            (Step::Web { site: a, query: p }, Step::Web { site: b, query: q }) => {
                norm(a) == norm(b) && norm(p) == norm(q)
            }
            (Step::Browser { action: a }, Step::Browser { action: b }) => a == b,
            (Step::Media { action: a }, Step::Media { action: b }) => a == b,
            (Step::Key { action: a, .. }, Step::Key { action: b, .. }) => a == b,
            (Step::OpenFolder { folder: a }, Step::OpenFolder { folder: b }) => a == b,
            (Step::Settings { page: a }, Step::Settings { page: b }) => a == b,
            (Step::Screenshot, Step::Screenshot) => true,
            _ => false,
        })
}

/// The command ran with `final_plan`: log what each live rule would have done.
pub fn shadow_finish(final_plan: Option<&Plan>) {
    let Some(s) = SHADOW.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let now = s.started.elapsed().as_millis();
    let sure = |o: &Obs| o.plan.is_some() && o.margin >= LIVE_MARGIN;
    let first_sure = s.obs.iter().find(|o| sure(o));
    let first_stable = s.obs.windows(2).find(|w| {
        sure(&w[0])
            && sure(&w[1])
            && same_plan(w[0].plan.as_ref().unwrap(), w[1].plan.as_ref().unwrap())
    });
    let verdict = |o: Option<&Obs>| match (o, final_plan) {
        (None, _) => "never".to_string(),
        (Some(o), Some(f)) => format!(
            "at {} ms ({} ms before execute) on {:?}: {}",
            o.ms,
            now.saturating_sub(o.ms),
            o.text,
            if same_plan(o.plan.as_ref().unwrap(), f) {
                "SAME plan"
            } else {
                "DIFFERENT plan"
            }
        ),
        (Some(o), None) => format!("at {} ms on {:?}: final had no plan", o.ms, o.text),
    };
    crate::log(&format!(
        "[PcShadow] partials={} sure: {} | sure+stable: {} | final={:?}",
        s.obs.len(),
        verdict(first_sure),
        verdict(first_stable.map(|w| &w[1])),
        final_plan.map(|p| &p.steps)
    ));
}

fn side_label(s: Side) -> &'static str {
    match s {
        Side::Left => "a sinistra",
        Side::Right => "a destra",
        Side::Full => "a schermo intero",
        Side::OtherMonitor => "sull'altro monitor",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<(String, String)> {
        [
            ("notepad++", "nuovo 2 - Notepad++"),
            ("olk", "Mail - Outlook"),
            ("ms-teams", "Chat | Microsoft Teams"),
            ("chrome", "Google Chrome"),
            ("Excel", ""),
        ]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
    }

    /// `DIMMY_PC_ROUTER_DIR=<dir with model_quantized.onnx + tokenizer.json>
    /// cargo test --release --lib --features pc-router golden -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn golden_commands_route_like_the_offline_measurement() {
        use Intent::*;
        let gold: &[(&str, Intent)] = &[
            ("apri Teams", Open),
            ("apri teams e mettilo a sinistra", Open),
            ("lancia Spotify", Open),
            ("aprimi il browser", Open),
            ("vai su youtube", Web),
            ("fammi partire excel", Open),
            ("open Outlook", Open),
            ("start Visual Studio Code", Open),
            ("affianca Teams e Outlook", Tile),
            ("metti Chrome e Word uno accanto all'altro", Tile),
            ("dividi lo schermo tra la mail e il calendario", Tile),
            ("affianca le finestre", Tile),
            ("put Slack and Chrome side by side", Tile),
            ("split the screen between Excel and the browser", Tile),
            ("sposta Chrome a destra", Move),
            ("metti questa finestra a sinistra", Move),
            ("massimizza Word", Move),
            ("porta Teams sull'altro monitor", Move),
            ("metti outlook a schermo intero", Move),
            ("move this window to the right", Move),
            ("send Spotify to the other screen", Move),
            ("trova i file di ieri", FindFiles),
            ("cerca il preventivo di Rossi", FindFiles),
            ("trovami i pdf che ho scaricato oggi", FindFiles),
            (
                "dove ho messo la presentazione della settimana scorsa",
                FindFiles,
            ),
            ("cerca i documenti modificati questa settimana", FindFiles),
            ("trova la fattura di ottobre", FindFiles),
            ("find the files I edited yesterday", FindFiles),
            ("where is the budget spreadsheet from last week", FindFiles),
            ("Puoi aprirmi Teams?", Open),
            ("Nascondi notepad", Minimize),
            ("riduci a icona Outlook", Minimize),
            ("Apri una nuova scheda", Browser),
            ("Apri una nuova tab", Browser),
            ("torna alla pagina precedente", Browser),
            ("Naviga su YouTube", Web),
            ("cerca su YouTube la ricetta della carbonara", Web),
            ("metti in pausa la musica", Media),
            ("prossima canzone", Media),
            ("alza il volume", Media),
            ("apri la cartella download", Folder),
            ("apri le impostazioni del bluetooth", Settings),
        ];
        let mut r = Router::load().expect("set DIMMY_PC_ROUTER_DIR");
        let (mut ok, mut sure_wrong, mut passed) = (0, 0, 0);
        let mut ms = Vec::new();
        for (text, want) in gold {
            let t = std::time::Instant::now();
            let (got, margin) = r.classify(text).unwrap();
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            let sure = margin >= MIN_MARGIN;
            ok += usize::from(got == *want);
            sure_wrong += usize::from(got != *want && sure);
            passed += usize::from(!sure);
            println!(
                "{} {:>5.1}ms m={margin:.3} {got:?} | {text}",
                if got == *want { "OK " } else { "BAD" },
                ms.last().unwrap()
            );
        }
        ms.sort_by(f64::total_cmp);
        println!("right {ok}/{}  wrong-but-sure {sure_wrong}  passed to Claude {passed}  median {:.1} ms", gold.len(), ms[ms.len() / 2]);
        assert_eq!(
            sure_wrong, 0,
            "a confident wrong action moves the wrong window"
        );
    }

    #[test]
    fn the_shadow_verdict_compares_actions_not_spellings() {
        let place = |app: &str, side| Plan {
            say: String::new(),
            steps: vec![Step::Place {
                app: app.into(),
                side,
            }],
        };
        assert!(same_plan(
            &place("Notepad++", Side::Right),
            &place("notepad", Side::Right)
        ));
        assert!(!same_plan(
            &place("notepad", Side::Right),
            &place("notepad", Side::Left)
        ));
        assert!(!same_plan(
            &place("chrome", Side::Right),
            &place("excel", Side::Right)
        ));
    }

    #[test]
    fn new_actions_read_their_slot_from_keywords() {
        assert_eq!(
            web_of(&words("naviga su YouTube")),
            Some(("youtube".into(), String::new()))
        );
        assert_eq!(
            web_of(&words("cerca su Google il meteo di domani")),
            Some(("google".into(), "meteo domani".into()))
        );
        assert_eq!(
            browser_action_of(&words("apri una nuova tab")),
            Some("new_tab")
        );
        assert_eq!(browser_action_of(&words("torna indietro")), Some("back"));
        assert_eq!(
            media_action_of(&words("abbassa il volume")),
            Some("volume_down")
        );
        assert_eq!(
            media_action_of(&words("metti in pausa")),
            Some("play_pause")
        );
        assert_eq!(
            folder_of(&words("mostrami l'ultimo file scaricato")),
            Some("latest_download")
        );
        assert_eq!(
            settings_of(&words("apri le impostazioni bluetooth")),
            Some("bluetooth")
        );
        assert_eq!(
            apps_in(&words("Apri Chrom apri Chrome"), &names()),
            vec!["chrom"],
            "two spellings of one app are one app"
        );
    }

    #[test]
    fn slots_come_from_keywords_and_app_names() {
        let w = words("affianca Teams e Notepad++");
        assert_eq!(apps_in(&w, &names()), vec!["teams", "notepad++"]);
        assert_eq!(side_of(&words("metti chrome a destra")), Some(Side::Right));
        assert_eq!(
            side_of(&words("porta Teams sull'altro monitor")),
            Some(Side::OtherMonitor)
        );
        assert_eq!(
            side_of(&words("metti outlook a schermo intero")),
            Some(Side::Full)
        );
        assert_eq!(when_of(&words("trova i file di ieri")), When::Yesterday);
        assert_eq!(
            when_of(&words("la presentazione della settimana scorsa")),
            When::LastWeek
        );
        assert_eq!(
            apps_in(&words("trova i file di ieri"), &names()),
            Vec::<String>::new()
        );
    }
}
