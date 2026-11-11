//! Local intent router for PC commands — EXPERIMENT (branch `exp/pc-actions`).
//!
//! multilingual-e5-small (int8 ONNX, ~118 MB) embeds the spoken command; a
//! softmax classifier trained at load on ~400 examples
//! (`assets/pc-router-examples.json`, an explicit "none" class included)
//! picks the action with a probability. Measured 2026-11-07 on 326 commands
//! written independently (69% Italian, 68 out of scope): 81% right overall,
//! and at p >= 0.60 no wrong action and no out-of-scope line executed
//! (22% of the commands decided that surely); at p >= 0.50, 1 mistake in 111.
//! Embedding nearest-example routing reached 56%, Laya (a decision model)
//! 34%, e5 then Laya 44%. Below `min_prob()`, on "none", or when a slot can't
//! be filled, the router answers `None` and the command goes to Claude.
//!
//! Slots without a model: side and date from keywords, app names by fuzzy
//! match of the command's words against open windows and Start-menu apps.

use crate::pc_control::{name_score, norm, Plan, Side, Step, When};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::sync::Mutex;
use tokenizers::Tokenizer;

fn min_prob() -> f32 {
    selected().min_prob
}

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
    // The intents below are decided by their words alone (`basic_plan`):
    // the classifier has no examples of them and never answers one.
    Focus,
    Close,
    Restore,
    Desktop,
    Switch,
    Screenshot,
    Keys,
    Type,
    /// Not a PC command: dictation, a question, an unsupported request.
    None,
}

impl Intent {
    fn from_key(k: &str) -> Option<Self> {
        Some(match k {
            "open" => Intent::Open,
            "tile" => Intent::Tile,
            "move" => Intent::Move,
            "find_files" => Intent::FindFiles,
            "minimize" => Intent::Minimize,
            "web" => Intent::Web,
            "browser" => Intent::Browser,
            "media" => Intent::Media,
            "folder" => Intent::Folder,
            "settings" => Intent::Settings,
            "none" => Intent::None,
            _ => return None,
        })
    }
}

/// The training examples, compiled in. Keys are intents; `_comment` is skipped.
fn examples() -> Vec<(Intent, String)> {
    let doc: serde_json::Value =
        serde_json::from_str(include_str!("../assets/pc-router-examples.json"))
            .expect("pc-router-examples.json is valid JSON (a unit test checks it)");
    let mut out = Vec::new();
    for (k, v) in doc.as_object().expect("an object of intent -> examples") {
        let Some(intent) = Intent::from_key(k) else {
            continue;
        };
        for t in v
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str())
        {
            out.push((intent, t.to_string()));
        }
    }
    out
}

/// Multinomial logistic regression (softmax) on unit-length embeddings, full
/// batch, Adam. Same model as the offline bench (sklearn, C=8): L2 weight
/// 1/(C·n). Returns (weights [classes × dim], bias [classes]).
fn train_softmax(x: &[Vec<f32>], y: &[usize], classes: usize) -> (Vec<f32>, Vec<f32>) {
    assert!(!x.is_empty() && x.len() == y.len(), "one label per example");
    assert!(y.iter().all(|&c| c < classes), "labels in range");
    let (n, dim) = (x.len(), x[0].len());
    let l2 = 1.0 / (8.0 * n as f32);
    let (lr, b1, b2, eps) = (0.05f32, 0.9f32, 0.999f32, 1e-8f32);
    let mut w = vec![0f32; classes * dim];
    let mut b = vec![0f32; classes];
    let (mut mw, mut vw) = (vec![0f32; w.len()], vec![0f32; w.len()]);
    let (mut mb, mut vb) = (vec![0f32; classes], vec![0f32; classes]);
    let mut gw = vec![0f32; w.len()];
    let mut gb = vec![0f32; classes];
    let mut p = vec![0f32; classes];
    for step in 1..=400 {
        gw.iter_mut().for_each(|g| *g = 0.0);
        gb.iter_mut().for_each(|g| *g = 0.0);
        for (xi, &yi) in x.iter().zip(y) {
            softmax_into(&w, &b, xi, &mut p);
            for c in 0..classes {
                let d = (p[c] - f32::from(c == yi)) / n as f32;
                gb[c] += d;
                let row = &mut gw[c * dim..(c + 1) * dim];
                for (g, xv) in row.iter_mut().zip(xi) {
                    *g += d * xv;
                }
            }
        }
        for (g, wv) in gw.iter_mut().zip(&w) {
            *g += l2 * wv;
        }
        let (c1, c2) = (1.0 - b1.powi(step), 1.0 - b2.powi(step));
        for i in 0..w.len() {
            mw[i] = b1 * mw[i] + (1.0 - b1) * gw[i];
            vw[i] = b2 * vw[i] + (1.0 - b2) * gw[i] * gw[i];
            w[i] -= lr * (mw[i] / c1) / ((vw[i] / c2).sqrt() + eps);
        }
        for i in 0..classes {
            mb[i] = b1 * mb[i] + (1.0 - b1) * gb[i];
            vb[i] = b2 * vb[i] + (1.0 - b2) * gb[i] * gb[i];
            b[i] -= lr * (mb[i] / c1) / ((vb[i] / c2).sqrt() + eps);
        }
    }
    assert!(
        w.iter().chain(&b).all(|v| v.is_finite()),
        "training stayed finite"
    );
    (w, b)
}

fn softmax_into(w: &[f32], b: &[f32], x: &[f32], out: &mut [f32]) {
    let dim = x.len();
    for (c, o) in out.iter_mut().enumerate() {
        *o = b[c]
            + w[c * dim..(c + 1) * dim]
                .iter()
                .zip(x)
                .map(|(a, v)| a * v)
                .sum::<f32>();
    }
    let max = out.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let mut sum = 0.0;
    for o in out.iter_mut() {
        *o = (*o - max).exp();
        sum += *o;
    }
    for o in out.iter_mut() {
        *o /= sum;
    }
}

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

/// `web_of`, in the session: a search that names no site goes to the site
/// the session already opened ("vai su YouTube", then "cerca gatti").
fn web_in_context(w: &[String]) -> Option<(String, String)> {
    let (site, query) = web_of(w)?;
    let named = w.iter().any(|t| SITES.contains(&t.as_str()));
    let last = crate::pc_control::last_site();
    Some(if named || last.is_empty() {
        (site, query)
    } else {
        (last, query)
    })
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
    if has("muto")
        || has("muta")
        || has("mute")
        || has("silenzia")
        || (has("togli") && has("audio"))
    {
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
    } else if has("videos")
        || (has("video") && (has("cartella") || has("folder") || has("i") || has("miei")))
    {
        // "Apri il video" names something on the screen, not the folder
        // (2026-11-10: it opened Explorer over a page of videos).
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
    } else if has("schermo") || has("monitor") || has("display") || has("luminosità") {
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
        || has("maximize")
        || w.iter()
            .any(|t| t.starts_with("massimizza") || t.starts_with("ingrandisci"))
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

/// A word that carries an action, a place or a time, never an app's name.
fn not_a_name(t: &str) -> bool {
    STOP.contains(&t)
        || CLOSE_WORDS.contains(&t)
        || RESTORE_WORDS.contains(&t)
        || FOCUS_WORDS.contains(&t)
        || PRESS_VERBS.contains(&t)
        // "Apri il video" is something on the screen, not "Editor video".
        || t == "video"
}

/// App names said in the command, in order, matched against `names`
/// (open windows' processes and titles, Start-menu apps).
fn apps_in(w: &[String], names: &[(String, String)]) -> Vec<String> {
    let mut found: Vec<(String, String)> = Vec::new();
    let mut i = 0;
    while i < w.len() {
        if not_a_name(&w[i]) {
            i += 1;
            continue;
        }
        // Longest n-gram (up to 3 content words) that names an app.
        let mut taken = 0;
        for n in (1..=3).rev() {
            if i + n > w.len() || w[i..i + n].iter().any(|t| not_a_name(t)) {
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pooling {
    /// Mean of the token vectors (e5).
    Mean,
    /// The first token's vector (BGE-M3 dense).
    Cls,
}

/// An encoder the router can run: where it comes from (pinned revision, each
/// file to its SHA-256), where it lives, and how it is used.
pub struct ModelSpec {
    pub id: &'static str,
    /// Sub-folder of `<config>/pc-router` ("" for the first model, which
    /// shipped there before there was a choice).
    subdir: &'static str,
    base: &'static str,
    files: [(&'static str, &'static str, &'static str); 2],
    pub size_mb: u32,
    prefix: &'static str,
    pooling: Pooling,
    /// After the stop the local plan runs at this probability, else Claude.
    min_prob: f32,
    /// Live, while the user talks: the lowest probability with no wrong
    /// action on the independent set, plus 0.05.
    live_prob: f32,
}

/// Measured on the 326-command set (Python, same classifier): e5-small 81%
/// right / 29% decided live / 14 ms; e5-base 83% / 40% / 25 ms; BGE-M3 85% /
/// 53% / 76 ms (fp32). All three MIT.
pub const MODELS: [ModelSpec; 3] = [
    ModelSpec {
        id: "e5-small",
        subdir: "",
        base: "https://huggingface.co/Xenova/multilingual-e5-small/resolve/761b726dd34fb83930e26aab4e9ac3899aa1fa78",
        files: [
            ("onnx/model_quantized.onnx", "model_quantized.onnx", "f80102d3f2a1229f387d3c81909990d8945513e347b0eab049f7de3c6f98c193"),
            ("tokenizer.json", "tokenizer.json", "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39"),
        ],
        size_mb: 129,
        prefix: "query: ",
        pooling: Pooling::Mean,
        min_prob: 0.50,
        live_prob: 0.60,
    },
    ModelSpec {
        id: "e5-base",
        subdir: "e5-base",
        base: "https://huggingface.co/Xenova/multilingual-e5-base/resolve/1ec9243030a27d1a115d5c340572074c125b58b2",
        files: [
            ("onnx/model_quantized.onnx", "model_quantized.onnx", "df7a9a29309e3ad491e1783adf8baee710262cc06079c7cbab63c630277fac94"),
            ("tokenizer.json", "tokenizer.json", "62c24cdc13d4c9952d63718d6c9fa4c287974249e16b7ade6d5a85e7bbb75626"),
        ],
        size_mb: 282,
        prefix: "query: ",
        pooling: Pooling::Mean,
        min_prob: 0.50,
        live_prob: 0.60,
    },
    ModelSpec {
        id: "bge-m3",
        subdir: "bge-m3",
        base: "https://huggingface.co/Xenova/bge-m3/resolve/4de13258303883538bd53b696b452bf8099f0858",
        files: [
            ("onnx/model_quantized.onnx", "model_quantized.onnx", "0826f8c1ab9edf1801db86c61919d4d108e8bfc0b809ec823ad366882ff0b77d"),
            ("tokenizer.json", "tokenizer.json", "6710678b12670bc442b99edc952c4d996ae309a7020c1fa0096dd245c2faf790"),
        ],
        size_mb: 560,
        prefix: "",
        pooling: Pooling::Cls,
        // Surer of itself than e5: no wrong action from 0.70 on (Rust, int8).
        min_prob: 0.65,
        live_prob: 0.75,
    },
];

static SELECTED: Mutex<&'static str> = Mutex::new("e5-small");

/// The encoder in use: the one selected, or DIMMY_PC_ROUTER_MODEL (tests).
pub fn selected() -> &'static ModelSpec {
    let want = std::env::var("DIMMY_PC_ROUTER_MODEL")
        .ok()
        .unwrap_or_else(|| SELECTED.lock().map(|s| s.to_string()).unwrap_or_default());
    MODELS.iter().find(|m| m.id == want).unwrap_or(&MODELS[0])
}

/// Switch encoder. A change unloads the router; the host then downloads the
/// model if needed and warms it. Returns false for an unknown id.
pub fn select(id: &str) -> bool {
    let Some(spec) = MODELS.iter().find(|m| m.id == id) else {
        return false;
    };
    let changed = {
        let mut cur = SELECTED.lock().unwrap_or_else(|e| e.into_inner());
        let changed = *cur != spec.id;
        *cur = spec.id;
        changed
    };
    if changed {
        *ROUTER.lock().unwrap_or_else(|e| e.into_inner()) = None;
        crate::log(&format!("[PcRouter] encoder -> {}", spec.id));
    }
    true
}

pub fn model_present() -> bool {
    spec_present(selected())
}

/// An encoder by id ("e5-small", "e5-base", "bge-m3").
pub fn find(id: &str) -> Option<&'static ModelSpec> {
    MODELS.iter().find(|m| m.id == id)
}

pub fn spec_present(spec: &ModelSpec) -> bool {
    model_dir(spec).is_some_and(|dir| files_present(&dir, spec))
}

/// The files of one encoder on disk, for the download center's delete.
pub fn spec_files(spec: &ModelSpec) -> Vec<std::path::PathBuf> {
    let Some(dir) = model_dir(spec) else {
        return Vec::new();
    };
    spec.files
        .iter()
        .map(|(_, name, _)| dir.join(name))
        .collect()
}

/// Drop the loaded router when it runs on this encoder, so Windows lets its
/// files go.
pub fn unload(spec: &ModelSpec) {
    let mut r = ROUTER.lock().unwrap_or_else(|e| e.into_inner());
    if r.as_ref().is_some_and(|r| r.spec.id == spec.id) {
        *r = None;
        crate::log(&format!("[PcRouter] unloaded {}", spec.id));
    }
}

fn files_present(dir: &std::path::Path, spec: &ModelSpec) -> bool {
    spec.files.iter().all(|(_, name, _)| {
        std::fs::metadata(dir.join(name))
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false)
    })
}

/// Download one encoder; the download center calls it. Resumable; every
/// file is checked against its pinned SHA-256 and deleted on mismatch, so a
/// retry starts clean.
pub async fn download(spec: &ModelSpec, progress: impl Fn(u64, u64)) -> Result<(), String> {
    let dir = model_dir(spec).ok_or("config dir unknown")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {:?}: {e}", dir))?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let total: u64 = spec.size_mb as u64 * 1024 * 1024;
    let mut base: u64 = 0;
    for (remote, name, sha) in spec.files {
        let dest = dir.join(name);
        if crate::download::verify_file(&dest, &[], Some(sha)).is_ok() {
            base += std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            progress(base, total);
            continue;
        }
        let _ = std::fs::remove_file(&dest);
        let url = format!("{}/{remote}", spec.base);
        crate::log(&format!("[PcRouter] downloading {} {name}", spec.id));
        crate::download::download_resumable(&client, &url, &dest, &[], |done, _| {
            progress(base + done, total)
        })
        .await
        .map_err(|e| format!("{name}: {e}"))?;
        if let Err(e) = crate::download::verify_file(&dest, &[], Some(sha)) {
            let _ = std::fs::remove_file(&dest);
            return Err(format!("{name}: {e}"));
        }
        base += std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
        progress(base, total);
    }
    // The model this call downloaded, not the one selected now: the user can
    // pick another encoder while this download runs.
    assert!(
        files_present(&dir, spec),
        "both files must exist after a download"
    );
    Ok(())
}

/// The example embeddings depend only on the encoder and the examples: kept
/// on disk, a restart (or BGE-M3, ~15 s to embed them) skips the work.
fn cache_key(spec: &ModelSpec) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in spec
        .id
        .bytes()
        .chain(include_str!("../assets/pc-router-examples.json").bytes())
    {
        h ^= byte as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn read_cache(path: &std::path::Path, n: usize) -> Option<Vec<Vec<f32>>> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 8 {
        return None;
    }
    let rows = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
    let dim = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
    if rows != n || dim == 0 || bytes.len() != 8 + rows * dim * 4 {
        return None;
    }
    let mut out = Vec::with_capacity(rows);
    for r in 0..rows {
        let start = 8 + r * dim * 4;
        let v: Vec<f32> = bytes[start..start + dim * 4]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        if v.iter().any(|x| !x.is_finite()) {
            return None;
        }
        out.push(v);
    }
    Some(out)
}

fn write_cache(path: &std::path::Path, x: &[Vec<f32>]) {
    let dim = x.first().map(|v| v.len()).unwrap_or(0);
    let mut bytes = Vec::with_capacity(8 + x.len() * dim * 4);
    bytes.extend_from_slice(&(x.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(dim as u32).to_le_bytes());
    for v in x {
        for f in v {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, &bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

struct Router {
    spec: &'static ModelSpec,
    session: Session,
    tok: Tokenizer,
    needs_token_types: bool,
    classes: Vec<Intent>,
    w: Vec<f32>,
    b: Vec<f32>,
}

static ROUTER: Mutex<Option<Router>> = Mutex::new(None);

fn model_dir(spec: &ModelSpec) -> Option<std::path::PathBuf> {
    // The override lets the ignored accuracy test point at the real files
    // (tests run against an isolated config dir).
    let root = match std::env::var("DIMMY_PC_ROUTER_DIR") {
        Ok(d) => std::path::PathBuf::from(d),
        Err(_) => crate::config_dir_path()?.join("pc-router"),
    };
    Some(if spec.subdir.is_empty() {
        root
    } else {
        root.join(spec.subdir)
    })
}

impl Router {
    fn load() -> Result<Self, String> {
        let spec = selected();
        let dir = model_dir(spec).ok_or("no config dir")?;
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
            spec,
            session,
            tok,
            needs_token_types,
            classes: Vec::new(),
            w: Vec::new(),
            b: Vec::new(),
        };
        let ex = examples();
        let t_embed = std::time::Instant::now();
        let mut classes: Vec<Intent> = Vec::new();
        let mut y = Vec::with_capacity(ex.len());
        for (intent, _) in &ex {
            let c = match classes.iter().position(|k| k == intent) {
                Some(c) => c,
                None => {
                    classes.push(*intent);
                    classes.len() - 1
                }
            };
            y.push(c);
        }
        let cache = dir.join(format!("examples-{:016x}.f32", cache_key(spec)));
        let cached = read_cache(&cache, ex.len());
        let from_cache = cached.is_some();
        let x = match cached {
            Some(x) => x,
            None => {
                let mut x = Vec::with_capacity(ex.len());
                for (_, text) in &ex {
                    x.push(r.embed(text)?);
                }
                write_cache(&cache, &x);
                x
            }
        };
        let embed_ms = t_embed.elapsed().as_millis();
        crate::log(&format!(
            "[PcRouter] encoder {} ({}), examples {}",
            spec.id,
            spec.size_mb,
            if from_cache {
                "from cache"
            } else {
                "embedded and cached"
            }
        ));
        let t_train = std::time::Instant::now();
        let (w, b) = train_softmax(&x, &y, classes.len());
        crate::log(&format!(
            "[PcRouter] {} examples embedded in {embed_ms} ms, classifier trained in {} ms",
            x.len(),
            t_train.elapsed().as_millis()
        ));
        r.classes = classes;
        r.w = w;
        r.b = b;
        Ok(r)
    }

    fn embed(&mut self, text: &str) -> Result<Vec<f32>, String> {
        let enc = self
            .tok
            .encode(format!("{}{text}", self.spec.prefix), true)
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
        // Mean of the token vectors under the attention mask (e5), or the
        // first token's vector (BGE-M3), then unit length. (Dividing the sum
        // by the token count is skipped: unit length makes it the same vector.)
        let mut v = vec![0f32; hidden];
        match self.spec.pooling {
            Pooling::Cls => v.copy_from_slice(&data[0..hidden]),
            Pooling::Mean => {
                for (t, m) in mask.iter().enumerate() {
                    if *m == 0 {
                        continue;
                    }
                    for (k, acc) in v.iter_mut().enumerate() {
                        *acc += data[t * hidden + k];
                    }
                }
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

    /// (most likely intent, its probability)
    fn classify(&mut self, text: &str) -> Result<(Intent, f32), String> {
        let q = self.embed(text)?;
        let mut p = vec![0f32; self.classes.len()];
        softmax_into(&self.w, &self.b, &q, &mut p);
        let (k, prob) = p
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(k, v)| (k, *v))
            .expect("at least one class");
        assert!((0.0..=1.0001).contains(&prob), "a probability");
        Ok((self.classes[k], prob))
    }
}

static LOADING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Load the model in the background, once. The lock is taken only to install
/// the loaded router, so a command arriving meanwhile is never held up.
pub fn warm() {
    use std::sync::atomic::Ordering::SeqCst;
    if ROUTER.lock().unwrap_or_else(|e| e.into_inner()).is_some() || LOADING.swap(true, SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let t = std::time::Instant::now();
        match Router::load() {
            Ok(r) => {
                crate::log(&format!(
                    "[PcRouter] {} ready in {} ms",
                    r.spec.id,
                    t.elapsed().as_millis()
                ));
                *ROUTER.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
            }
            Err(e) => crate::log(&format!("[PcRouter] {} unavailable: {e}", selected().id)),
        }
        LOADING.store(false, SeqCst);
    });
}

/// A plan when the router is sure, else `None` (the caller asks Claude).
/// `detail` says why, for the log.
pub fn route(command: &str, names: &[(String, String)]) -> (Option<Plan>, String) {
    let (plan, margin, detail) = route_scored(command, names);
    if margin < min_prob() {
        return (None, format!("{detail} below {}", min_prob()));
    }
    (plan, detail)
}

// -- Basic commands: a clear keyword decides, not the classifier ----------
//
// "Ingrandisci Notepad" scored p=0.41 on BGE-M3 with "ingrandisci Chrome" in
// its examples: short commands confuse the classifier. A basic command is
// taken only when EVERY word is accounted for (the action's own words, filler,
// the app's name), so "cerca come mettere Chrome a sinistra" still goes to the
// classifier and to Claude.

const FILLER: &[&str] = &[
    "alle",
    "agli",
    "questa",
    "questo",
    "quella",
    "quello",
    "ehm",
    "allora",
    "tipo",
    "puoi",
    "che",
    "ho",
    "my",
    "a",
    "bit",
    "one",
    "folder",
    "il",
    "lo",
    "la",
    "i",
    "gli",
    "le",
    "l",
    "un",
    "una",
    "uno",
    "e",
    "ed",
    "a",
    "ad",
    "al",
    "alla",
    "allo",
    "all",
    "ai",
    "su",
    "sul",
    "sulla",
    "sull",
    "in",
    "di",
    "del",
    "della",
    "dello",
    "dei",
    "delle",
    "da",
    "per",
    "favore",
    "grazie",
    "mi",
    "me",
    "ti",
    "ci",
    "po",
    "pure",
    "subito",
    "adesso",
    "ora",
    "dai",
    "please",
    "the",
    "to",
    "on",
    "of",
    "and",
    "my",
    "app",
    "finestra",
    "programma",
];

const SIDE_WORDS: &[&str] = &[
    "sinistra",
    "destra",
    "schermo",
    "intero",
    "tutto",
    "altro",
    "secondo",
    "monitor",
    "portatile",
    "laptop",
    "left",
    "right",
    "full",
    "screen",
    "other",
    "second",
    "questa",
    "this",
    "window",
];

const MOVE_WORDS: &[&str] = &[
    "ingrandisci",
    "ingrandiscimi",
    "ingrandiscila",
    "ingrandiscilo",
    "massimizza",
    "massimizzala",
    "massimizzalo",
    "metti",
    "mettilo",
    "mettila",
    "mettimi",
    "sposta",
    "spostalo",
    "spostala",
    "spostami",
    "porta",
    "portalo",
    "portala",
    "portami",
    "fammi",
    "vedere",
    "maximize",
    "move",
    "put",
];

const OPEN_WORDS: &[&str] = &[
    "apri", "aprimi", "avvia", "lancia", "fai", "partire", "mettilo", "mettila", "open", "start",
    "launch",
];

const MINIMIZE_WORDS: &[&str] = &[
    "riduci",
    "riducilo",
    "riducila",
    "icona",
    "minimizza",
    "minimizzala",
    "minimizzalo",
    "nascondi",
    "nascondila",
    "nascondilo",
    "minimize",
    "hide",
];

const TILE_WORDS: &[&str] = &[
    "affianca",
    "affiancami",
    "affiancate",
    "affiancati",
    "metti",
    "mettimi",
    "vicini",
    "vicino",
    "accanto",
    "fianco",
    "insieme",
    "tile",
    "side",
    "by",
];

const MEDIA_WORDS: &[&str] = &[
    "passa",
    "torna",
    "song",
    "music",
    "turn",
    "muta",
    "riproduzione",
    "pausa",
    "play",
    "metti",
    "riprendi",
    "ferma",
    "musica",
    "canzone",
    "brano",
    "traccia",
    "prossima",
    "prossimo",
    "successiva",
    "successivo",
    "next",
    "skip",
    "salta",
    "precedente",
    "previous",
    "alza",
    "abbassa",
    "aumenta",
    "diminuisci",
    "volume",
    "su",
    "giù",
    "up",
    "down",
    "muto",
    "mute",
    "silenzia",
    "togli",
    "audio",
    "pause",
];

const BROWSER_WORDS: &[&str] = &[
    "apri",
    "aprimi",
    "passa",
    "chiusa",
    "chiuso",
    "this",
    "page",
    "nuova",
    "scheda",
    "tab",
    "chiudi",
    "riapri",
    "successiva",
    "prossima",
    "precedente",
    "indietro",
    "avanti",
    "torna",
    "vai",
    "ricarica",
    "aggiorna",
    "pagina",
    "new",
    "close",
    "reopen",
    "next",
    "previous",
    "back",
    "forward",
    "reload",
    "refresh",
    "go",
];

const SETTINGS_WORDS: &[&str] = &[
    "mostra",
    "mostrami",
    "vai",
    "notification",
    "pagina",
    "fammi",
    "vedere",
    "windows",
    "apri",
    "aprimi",
    "impostazioni",
    "settings",
    "bluetooth",
    "wifi",
    "wi",
    "fi",
    "rete",
    "audio",
    "suono",
    "sound",
    "schermo",
    "monitor",
    "display",
    "luminosità",
    "notifiche",
    "notifications",
    "open",
];

const FOLDER_WORDS: &[&str] = &[
    "aprirmi",
    "esplora",
    "folder",
    "my",
    "me",
    "apri",
    "aprimi",
    "mostra",
    "mostrami",
    "fammi",
    "vedere",
    "cartella",
    "download",
    "downloads",
    "scaricato",
    "scaricati",
    "ultimo",
    "last",
    "latest",
    "file",
    "documenti",
    "documents",
    "desktop",
    "scrivania",
    "immagini",
    "foto",
    "pictures",
    "photos",
    "musica",
    "music",
    "video",
    "videos",
    "open",
    "show",
];

/// Words that point at the window in front.
const DEICTIC: &[&str] = &[
    "questa", "questo", "finestra", "window", "this", "corrente", "attiva", "current",
];

/// Verbs that carry their object: "mettilo" is "metti" and the app the
/// session's last command named.
const PRONOUN_VERBS: &[&str] = &[
    "mettilo",
    "mettila",
    "spostalo",
    "spostala",
    "portalo",
    "portala",
    "ingrandiscilo",
    "ingrandiscila",
    "massimizzalo",
    "massimizzala",
    "minimizzalo",
    "minimizzala",
    "nascondilo",
    "nascondila",
    "riducilo",
    "riducila",
    "chiudilo",
    "chiudila",
    "ripristinalo",
    "ripristinala",
];

const CLOSE_WORDS: &[&str] = &["chiudi", "chiudila", "chiudilo", "chiudimi", "close"];

const RESTORE_WORDS: &[&str] = &[
    "ripristina",
    "ripristinala",
    "ripristinalo",
    "ripristinami",
    "restore",
];

const FOCUS_WORDS: &[&str] = &[
    "passa", "vai", "torna", "porta", "portami", "primo", "piano", "attiva", "mostra", "mostrami",
    "switch", "go", "bring", "front", "focus", "back",
];

const DESKTOP_WORDS: &[&str] = &[
    "mostra",
    "mostrami",
    "vai",
    "show",
    "go",
    "desktop",
    "scrivania",
    "riduci",
    "icona",
    "nascondi",
    "minimizza",
    "minimize",
    "hide",
    "tutto",
    "tutte",
    "finestre",
    "windows",
    "everything",
    "all",
];

const SWITCH_WORDS: &[&str] = &[
    "finestra",
    "precedente",
    "cambia",
    "altra",
    "switch",
    "window",
    "previous",
    "alt",
    "tab",
    "torna",
    "prima",
    "passa",
];

const SCREENSHOT_WORDS: &[&str] = &[
    "fai",
    "fammi",
    "screenshot",
    "schermata",
    "cattura",
    "istantanea",
    "take",
    "screen",
    "shot",
    "schermo",
    "dello",
];

/// "vai su YouTube": a site and nothing to search for.
const WEBNAV_WORDS: &[&str] = &["vai", "naviga", "apri", "open", "go", "sito"];

const PRESS_VERBS: &[&str] = &["premi", "press", "schiaccia", "pigia"];

const TYPE_VERBS: &[&str] = &["scrivi", "scrivimi", "digita", "type", "write"];

/// After "scrivi": the text goes somewhere named, which a model reads better.
const TYPE_ELSEWHERE: &[&str] = &[
    "in", "su", "a", "al", "alla", "allo", "dentro", "nel", "nella", "sul", "sulla", "at", "on",
    "into", "to", "inside",
];

/// A key action and the ways to ask for it, as content words in any order
/// (filler dropped, "su" kept: here it is a direction).
const KEY_PHRASES: &[(&str, &[&str])] = &[
    ("select_all", &["seleziona tutto", "select all"]),
    ("copy", &["copia", "copy"]),
    ("paste", &["incolla", "paste"]),
    ("cut", &["taglia", "cut"]),
    ("undo", &["annulla", "undo"]),
    ("redo", &["ripeti", "rifai", "redo"]),
    ("save", &["salva", "save"]),
    ("new_line", &["vai capo", "capo", "nuova riga", "new line"]),
    ("enter", &["invio", "enter"]),
    (
        "bottom",
        &["vai fondo", "fondo", "scorri fondo", "vai fine", "bottom"],
    ),
    (
        "top",
        &["vai cima", "cima", "scorri cima", "vai inizio", "top"],
    ),
    (
        "page_down",
        &[
            "scorri giù",
            "scorri giu",
            "scendi",
            "pagina giù",
            "pagina giu",
            "più giù",
            "piu giu",
            "scroll down",
            "page down",
        ],
    ),
    (
        "page_up",
        &[
            "scorri su",
            "sali",
            "pagina su",
            "più su",
            "piu su",
            "scroll up",
            "page up",
        ],
    ),
];

/// The keys named after "premi": "control s", "ctrl+shift+t", "invio",
/// "freccia giù", "pagina su". None when a word is not a key.
fn pressed_keys(tokens: &[String]) -> Option<String> {
    let mut keys: Vec<&str> = Vec::new();
    let mut page = false;
    for t in tokens
        .iter()
        .flat_map(|t| t.split('+'))
        .filter(|t| !t.is_empty())
    {
        let key = match t {
            "il" | "i" | "la" | "tasto" | "tasti" | "e" | "più" | "piu" | "plus" | "key"
            | "the" | "freccia" | "arrow" | "and" | "spaziatrice" => continue,
            "pagina" | "page" | "pag" => {
                page = true;
                continue;
            }
            "control" | "ctrl" | "controllo" => "ctrl",
            "alt" => "alt",
            "shift" | "maiusc" | "maiuscolo" => "shift",
            "windows" | "win" | "start" => "win",
            "invio" | "enter" | "return" => "enter",
            "esc" | "escape" => "escape",
            "tab" | "tabulazione" => "tab",
            "spazio" | "space" | "barra" => "space",
            "backspace" => "backspace",
            "canc" | "cancella" | "delete" | "del" | "elimina" => "delete",
            "su" | "up" if page => "page_up",
            "giù" | "giu" | "down" if page => "page_down",
            "su" | "up" => "up",
            "giù" | "giu" | "down" => "down",
            "sinistra" | "left" => "left",
            "destra" | "right" => "right",
            "home" | "inizio" => "home",
            "end" | "fine" => "end",
            // A letter, a digit, a function key.
            other if other.len() <= 3 && !other.contains('_') => other,
            _ => return None,
        };
        keys.push(key);
    }
    let action = keys.join("+");
    crate::pc_control::key_chord(&action).map(|_| action)
}

/// The key action a command asks for: "premi <keys>", or one of KEY_PHRASES
/// said with nothing else.
fn key_action_of(w: &[String]) -> Option<String> {
    let first = w.iter().position(|t| !FILLER.contains(&t.as_str()))?;
    if PRESS_VERBS.contains(&w[first].as_str()) {
        return pressed_keys(&w[first + 1..]);
    }
    let mut said: Vec<&str> = w
        .iter()
        .map(String::as_str)
        .filter(|t| *t == "su" || !FILLER.contains(t))
        .collect();
    said.sort_unstable();
    KEY_PHRASES.iter().find_map(|(action, ways)| {
        ways.iter()
            .any(|way| {
                let mut way: Vec<&str> = way.split(' ').collect();
                way.sort_unstable();
                way == said
            })
            .then(|| (*action).to_string())
    })
}

fn key_say(action: &str) -> String {
    match action {
        "select_all" => "Seleziono tutto".into(),
        "copy" => "Copio".into(),
        "paste" => "Incollo".into(),
        "cut" => "Taglio".into(),
        "undo" => "Annullo".into(),
        "redo" => "Ripeto".into(),
        "save" => "Salvo".into(),
        "new_line" => "Vado a capo".into(),
        "bottom" => "Vado in fondo".into(),
        "top" => "Vado in cima".into(),
        "page_down" => "Scorro giù".into(),
        "page_up" => "Scorro su".into(),
        other => format!("Premo {other}"),
    }
}

/// The text after "scrivi", as it was said (its case and punctuation kept).
/// None when the words name a place for it, or an open app: Claude reads
/// "scrivi in fondo a Notepad ciao" better than a rule does.
fn type_text_of(command: &str, names: &[(String, String)]) -> Option<String> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let key = |t: &str| words(t).into_iter().next().unwrap_or_default();
    let verb = tokens.iter().position(|t| {
        let k = key(t);
        !k.is_empty() && !FILLER.contains(&k.as_str())
    })?;
    if !TYPE_VERBS.contains(&key(tokens[verb]).as_str()) {
        return None;
    }
    let rest = tokens[verb + 1..].join(" ");
    let rest_words = words(&rest);
    let first = rest_words.first()?;
    let open: Vec<(String, String)> = names
        .iter()
        .filter(|(_, title)| !title.is_empty())
        .cloned()
        .collect();
    if TYPE_ELSEWHERE.contains(&first.as_str()) || !apps_in(&rest_words, &open).is_empty() {
        return None;
    }
    Some(rest)
}

fn desktop_of(w: &[String]) -> bool {
    let has = |x: &str| w.iter().any(|t| t == x);
    let desktop = has("desktop") || has("scrivania");
    let go = has("mostra") || has("mostrami") || has("vai") || has("show") || has("go");
    let hide =
        has("riduci") || has("nascondi") || has("minimizza") || has("minimize") || has("hide");
    let all = has("tutto") || has("tutte") || has("everything") || has("all");
    (desktop && go) || (hide && all)
}

fn switch_of(w: &[String]) -> bool {
    let has = |x: &str| w.iter().any(|t| t == x);
    let window = has("finestra") || has("window");
    (window
        && (has("precedente")
            || has("prima")
            || has("altra")
            || has("cambia")
            || has("previous")
            || has("switch")))
        || (has("alt") && has("tab"))
}

fn screenshot_of(w: &[String]) -> bool {
    let has = |x: &str| w.iter().any(|t| t == x);
    has("screenshot")
        || has("istantanea")
        || (has("screen") && has("shot"))
        || (has("cattura") && (has("schermo") || has("schermata")))
}

/// A word some command rule knows: an action, a place, a time, a filler.
fn known_word(t: &str) -> bool {
    [
        STOP,
        FILLER,
        SIDE_WORDS,
        MOVE_WORDS,
        OPEN_WORDS,
        MINIMIZE_WORDS,
        TILE_WORDS,
        MEDIA_WORDS,
        BROWSER_WORDS,
        SETTINGS_WORDS,
        FOLDER_WORDS,
        DATE_WORDS,
        FILE_WORDS,
        WEB_STOP,
        SITES,
        DEICTIC,
        PRONOUN_VERBS,
        CLOSE_WORDS,
        RESTORE_WORDS,
        FOCUS_WORDS,
        DESKTOP_WORDS,
        SWITCH_WORDS,
        SCREENSHOT_WORDS,
        PRESS_VERBS,
        TYPE_VERBS,
    ]
    .iter()
    .any(|list| list.contains(&t))
        || KEY_PHRASES
            .iter()
            .any(|(_, ways)| ways.iter().any(|way| way.split(' ').any(|x| x == t)))
}

/// The last word of a live partial may be half said: "questa sche" opened
/// the on-screen keyboard (2026-11-08, "sche" inside "Tastiera su schermo").
/// It is whole when a command rule knows it, or when it is a full word of a
/// known app or window name.
fn last_word_whole(text: &str, names: &[(String, String)]) -> bool {
    let w = words(text);
    let Some(last) = w.last() else {
        return false;
    };
    if known_word(last) {
        return true;
    }
    let l = norm(last);
    !l.is_empty()
        && (crate::pc_control::is_alias(&l)
            || names.iter().any(|(name, title)| {
                norm(name) == l
                    || words(name)
                        .iter()
                        .chain(words(title).iter())
                        .any(|x| norm(x) == l)
            }))
}

/// A command still being said: every word is a command word or an app's
/// name, and they do not make a plan yet ("apri", "metti Chrome a"). The
/// end of a segment must not drop it; the next words complete it.
fn is_dangling(text: &str, names: &[(String, String)]) -> bool {
    let w = words(text);
    let app_words: Vec<String> = apps_in(&w, names).iter().flat_map(|g| words(g)).collect();
    let Some(last) = w.last() else {
        return false;
    };
    // It stops on a word that asks for another ("apri il", "metti Chrome
    // a"), or it is one lone command word ("apri"). A whole sentence of
    // known words is not waiting for anything: "fammi vedere il primo
    // video" sat there until the stop (2026-11-10).
    (CONTINUES.contains(&last.as_str()) || w.len() == 1)
        && w.len() <= 5
        && w.iter().all(|t| known_word(t) || app_words.contains(t))
}

/// Words a sentence does not end on.
const CONTINUES: &[&str] = &[
    "il", "lo", "la", "i", "gli", "le", "l", "un", "una", "uno", "a", "ad", "al", "alla", "allo",
    "ai", "alle", "su", "sul", "sulla", "in", "di", "del", "della", "dei", "delle", "da", "per",
    "con", "e", "ed", "o", "the", "an", "to", "on", "of", "and",
];

/// The plan of a basic command, or None: the word rules alone, no model.
pub fn basic(command: &str, names: &[(String, String)]) -> Option<Plan> {
    basic_plan(command, names).map(|(_, plan)| plan)
}

/// A basic command whose every word is accounted for, and its plan.
fn basic_plan(command: &str, names: &[(String, String)]) -> Option<(Intent, Plan)> {
    let w = words(command);
    if let Some(text) = type_text_of(command, names) {
        let plan = Plan {
            say: format!("Scrivo \"{}\"", crate::truncate_utf8(&text, 60)),
            steps: vec![Step::Type { text }],
        };
        return Some((Intent::Type, plan));
    }
    if let Some(action) = key_action_of(&w) {
        let plan = Plan {
            say: key_say(&action),
            steps: vec![Step::Key {
                app: String::new(),
                action,
            }],
        };
        return Some((Intent::Keys, plan));
    }
    let app_words: Vec<String> = apps_in(&w, names).iter().flat_map(|g| words(g)).collect();
    // Order matters only where two lists share a word: "scheda precedente"
    // is not media (no "scheda" there), "finestra precedente" is not media
    // either ("finestra" is filler there), "mostra il desktop" is not the
    // Desktop folder, "apri download" is a folder, not an app.
    let tries: [(Intent, &[&str]); 15] = [
        (Intent::Move, MOVE_WORDS),
        (Intent::Minimize, MINIMIZE_WORDS),
        (Intent::Tile, TILE_WORDS),
        (Intent::Switch, SWITCH_WORDS),
        (Intent::Media, MEDIA_WORDS),
        (Intent::Browser, BROWSER_WORDS),
        (Intent::Desktop, DESKTOP_WORDS),
        (Intent::Settings, SETTINGS_WORDS),
        (Intent::Folder, FOLDER_WORDS),
        (Intent::Close, CLOSE_WORDS),
        (Intent::Restore, RESTORE_WORDS),
        (Intent::Screenshot, SCREENSHOT_WORDS),
        (Intent::Open, OPEN_WORDS),
        (Intent::Focus, FOCUS_WORDS),
        (Intent::Web, WEBNAV_WORDS),
    ];
    for (intent, own) in tries {
        let sides = matches!(intent, Intent::Move | Intent::Open);
        let apps = matches!(
            intent,
            Intent::Move
                | Intent::Minimize
                | Intent::Tile
                | Intent::Open
                | Intent::Focus
                | Intent::Close
                | Intent::Restore
        );
        let deictic = matches!(
            intent,
            Intent::Move | Intent::Minimize | Intent::Close | Intent::Restore
        );
        let sites = intent == Intent::Web;
        let action =
            |t: &String| own.contains(&t.as_str()) || (sides && SIDE_WORDS.contains(&t.as_str()));
        let covered = w.iter().all(|t| {
            action(t)
                || FILLER.contains(&t.as_str())
                || (apps && app_words.contains(t))
                || (deictic && DEICTIC.contains(&t.as_str()))
                || (sites && SITES.contains(&t.as_str()))
        });
        if covered && w.iter().any(action) {
            if let Some(p) = plan_for(intent, command, names) {
                return Some((intent, p));
            }
        }
    }
    None
}

/// The detail of a basic command, so the live path can tell it apart.
const BASIC: &str = "basic";

/// The plan the slots allow (whatever the margin), the margin, and why.
fn route_scored(command: &str, names: &[(String, String)]) -> (Option<Plan>, f32, String) {
    if let Some((intent, plan)) = basic_plan(command, names) {
        return (Some(plan), 1.0, format!("{BASIC} intent={intent:?}"));
    }
    let mut g = ROUTER.lock().unwrap_or_else(|e| e.into_inner());
    // Never load on a command's path (~3 s): this one goes to Claude.
    let Some(r) = g.as_mut() else {
        drop(g);
        warm();
        return (None, 0.0, "e5 not loaded yet".into());
    };
    let (intent, margin) = match r.classify(command) {
        Ok(x) => x,
        Err(e) => return (None, 0.0, format!("e5 error: {e}")),
    };
    drop(g);
    let detail = format!("intent={intent:?} p={margin:.3}");
    let apps = apps_in(&words(command), names);
    let plan = plan_for(intent, command, names);
    match plan {
        Some(p) => (Some(p), margin, detail),
        None => (
            None,
            margin,
            format!("{detail} apps={apps:?}: slots incomplete"),
        ),
    }
}

/// The plan an intent's slots allow. Moving or minimising acts on a window
/// that is open: a Start-menu name there would launch an app nobody asked
/// for ("ingrandisci a tutto scher…" once opened the on-screen keyboard).
fn plan_for(intent: Intent, command: &str, names: &[(String, String)]) -> Option<Plan> {
    let w = words(command);
    let apps = apps_in(&w, names);
    // Open: some window's process or title holds the name ("outlook" is
    // the window "olk: Mail - Outlook", a title match the app list ranks lower).
    let open_apps: Vec<String> = apps
        .iter()
        .filter(|g| {
            names
                .iter()
                .any(|(n, t)| !t.is_empty() && crate::pc_control::name_score(g, n, t) >= 0.7)
        })
        .cloned()
        .collect();
    // "mettilo a destra" means the app the last command named, while its
    // window is open; "questa finestra" (and a pronoun with nothing named
    // before it) means the window in front, which the hands call "".
    let target: Option<String> = if w.iter().any(|t| PRONOUN_VERBS.contains(&t.as_str())) {
        let last = crate::pc_control::last_app();
        let open = !last.is_empty()
            && names
                .iter()
                .any(|(n, t)| !t.is_empty() && name_score(&last, n, t) >= 0.7);
        Some(if open { last } else { String::new() })
    } else if w.iter().any(|t| t == "finestra" || t == "window") {
        Some(String::new())
    } else {
        None
    };
    let window_label = |app: &str| {
        if app.is_empty() {
            "la finestra".to_string()
        } else {
            app.to_string()
        }
    };
    match intent {
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
                say = format!("{say} e lo metto {}", side_label(side));
            }
            Some(Plan { steps, say })
        }
        Intent::Tile if apps.len() == 2 => Some(Plan {
            say: format!("Affianco {} e {}", apps[0], apps[1]),
            steps: vec![Step::Tile {
                left: apps[0].clone(),
                right: apps[1].clone(),
            }],
        }),
        Intent::Move => match (side_of(&w), open_apps.as_slice()) {
            (Some(side), [app]) => Some(Plan {
                say: format!("Sposto {app} {}", side_label(side)),
                steps: vec![Step::Place {
                    app: app.clone(),
                    side,
                }],
            }),
            (Some(side), []) => target.map(|app| Plan {
                say: format!("Sposto {} {}", window_label(&app), side_label(side)),
                steps: vec![Step::Place { app, side }],
            }),
            _ => None,
        },
        Intent::Minimize | Intent::Close | Intent::Restore => {
            let app = match open_apps.as_slice() {
                [app] => app.clone(),
                [] => target?,
                _ => return None,
            };
            let label = window_label(&app);
            Some(match intent {
                Intent::Minimize => Plan {
                    say: format!("Riduco a icona {label}"),
                    steps: vec![Step::Minimize { app }],
                },
                Intent::Close => Plan {
                    say: format!("Chiudo {label}"),
                    steps: vec![Step::Close { app }],
                },
                _ => Plan {
                    say: format!("Ripristino {label}"),
                    steps: vec![Step::Restore { app }],
                },
            })
        }
        Intent::Focus if apps.len() == 1 => Some(Plan {
            say: format!("Passo a {}", apps[0]),
            steps: vec![Step::Open {
                app: apps[0].clone(),
            }],
        }),
        Intent::Desktop if desktop_of(&w) => Some(Plan {
            say: "Mostro il desktop".into(),
            steps: vec![Step::ShowDesktop],
        }),
        Intent::Switch if switch_of(&w) => Some(Plan {
            say: "Torno alla finestra di prima".into(),
            steps: vec![Step::SwitchWindow],
        }),
        Intent::Screenshot if screenshot_of(&w) => Some(Plan {
            say: "Apro la cattura dello schermo".into(),
            steps: vec![Step::Screenshot],
        }),
        Intent::Web => web_in_context(&w).map(|(site, query)| Plan {
            say: if query.is_empty() {
                format!("Apro {site}")
            } else {
                format!("Cerco \"{query}\" su {site}")
            },
            steps: vec![Step::Web { site, query }],
        }),
        Intent::Browser => browser_action_of(&w).map(|a| Plan {
            say: match a {
                "new_tab" => "Apro una nuova scheda",
                "close_tab" => "Chiudo la scheda",
                "reopen_tab" => "Riapro l'ultima scheda chiusa",
                "back" => "Torno alla pagina precedente",
                "forward" => "Vado alla pagina successiva",
                "reload" => "Ricarico la pagina",
                "next_tab" => "Passo alla scheda successiva",
                _ => "Passo alla scheda precedente",
            }
            .into(),
            steps: vec![Step::Browser { action: a.into() }],
        }),
        Intent::Media => media_action_of(&w).map(|a| Plan {
            say: match a {
                "play_pause" => "Metto in pausa o riprendo",
                "next" => "Passo al brano successivo",
                "previous" => "Torno al brano precedente",
                "volume_up" => "Alzo il volume",
                "volume_down" => "Abbasso il volume",
                _ => "Tolgo l'audio",
            }
            .into(),
            steps: vec![Step::Media { action: a.into() }],
        }),
        Intent::Folder => folder_of(&w).map(|f| Plan {
            say: match f {
                "latest_download" => "Ti mostro l'ultimo file scaricato".to_string(),
                "downloads" => "Apro la cartella Download".into(),
                "documents" => "Apro la cartella Documenti".into(),
                "desktop" => "Apro il Desktop".into(),
                "pictures" => "Apro la cartella Immagini".into(),
                "music" => "Apro la cartella Musica".into(),
                _ => "Apro la cartella Video".into(),
            },
            steps: vec![Step::OpenFolder { folder: f.into() }],
        }),
        Intent::Settings => settings_of(&w).map(|p| Plan {
            say: match p {
                "bluetooth" => "Apro le impostazioni Bluetooth",
                "wifi" => "Apro le impostazioni Wi-Fi",
                "sound" => "Apro le impostazioni audio",
                "display" => "Apro le impostazioni dello schermo",
                _ => "Apro le impostazioni delle notifiche",
            }
            .into(),
            steps: vec![Step::Settings { page: p.into() }],
        }),
        Intent::FindFiles => {
            let when = when_of(&w);
            let query: Vec<&str> = w
                .iter()
                .map(String::as_str)
                .filter(|t| !STOP.contains(t) && !DATE_WORDS.contains(t) && !FILE_WORDS.contains(t))
                .collect();
            let period = match when {
                When::Today => " di oggi",
                When::Yesterday => " di ieri",
                When::ThisWeek => " di questa settimana",
                When::LastWeek => " della settimana scorsa",
                When::Any => "",
            };
            Some(Plan {
                say: if query.is_empty() {
                    format!("Cerco i file{period}")
                } else {
                    format!("Cerco \"{}\" tra i file{period}", query.join(" "))
                },
                steps: vec![Step::FindFiles {
                    query: query.join(" "),
                    when,
                }],
            })
        }
        _ => None,
    }
}

// -- Shadow mode: would a live partial have been enough? --------------
//
// While an execute session records, every partial transcript is routed and
// the result kept. Nothing is executed. When the command runs for real, the
// log says, per candidate rule, when it would have fired, on which words,
// and whether its plan matches the one that ran.

/// Stricter than the after-stop threshold: acting before the user is done.
fn live_prob() -> f32 {
    selected().live_prob
}

/// A sure plan runs once no new words have come for this long: Nemotron
/// sends a partial only when the words change, so a pause is the only
/// "done" signal there is (a second identical partial never arrives).
const LIVE_QUIET_MS: u64 = 600;

/// A basic command has every word accounted for and a whole last word, so
/// it needs a shorter pause. Not shorter than this: the recognizer hands
/// over words in 320 ms chunks, and a gap between two chunks is not a pause.
const BASIC_QUIET_MS: u64 = 450;

/// Any other command is complete after this long without new words: it then
/// goes the whole way (app commands, router, the planner), or is dropped.
const SEGMENT_QUIET_MS: u64 = 1200;

struct Obs {
    ms: u128,
    /// The words after the last command, the ones this partial adds.
    text: String,
    /// Words of the whole session transcript at this partial.
    words: usize,
    plan: Option<Plan>,
    margin: f32,
    /// The last word is a whole word (`last_word_whole`): only then may the
    /// plan run before the segment ends.
    whole: bool,
    /// A command still being said (`is_dangling`): the segment end leaves it.
    dangling: bool,
}

struct Shadow {
    started: std::time::Instant,
    /// Partials since the last command.
    obs: Vec<Obs>,
    /// Words of the session transcript already used by a command, or dropped.
    used: usize,
    /// Commands handled while the user talked (run, failed or dropped).
    handled: u32,
    /// The window in front was read for the command being said now.
    read_front: bool,
    /// The session gets whole utterances (`utterance`), not partials.
    utterances: bool,
    /// Every utterance so far, as the engine wrote it.
    heard: String,
}

/// Live execution is on unless DIMMY_PC_LIVE=0.
fn live_enabled() -> bool {
    std::env::var("DIMMY_PC_LIVE")
        .map(|v| v != "0")
        .unwrap_or(true)
}

/// The words the stop still has to run, once the session handled a command
/// (empty: nothing left). None: no command ran, the stop runs the transcript.
pub fn session_rest() -> Option<String> {
    let g = SHADOW.lock().unwrap_or_else(|e| e.into_inner());
    let s = g.as_ref()?;
    (s.handled > 0).then(|| {
        if s.utterances {
            segment(&s.heard, s.used)
                .map(|(text, _)| text)
                .unwrap_or_default()
        } else {
            s.obs.last().map(|o| o.text.clone()).unwrap_or_default()
        }
    })
}

/// A whole utterance arrived: the session runs on an engine that does not
/// stream, and its audio is cut where the speaker pauses. `cumulative` is
/// every utterance so far. The words after the last command are a command,
/// unless they are one still being said ("apri", then "il browser").
pub fn utterance(cumulative: &str, is_final: bool) {
    let (text, words) = {
        let mut g = SHADOW.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = g.as_mut() else { return };
        s.utterances = true;
        s.heard = cumulative.to_string();
        // The stop runs what is left (`session_rest`), not this.
        if is_final || !recording_execute() {
            return;
        }
        let Some(new) = segment(cumulative, s.used) else {
            return;
        };
        new
    };
    std::thread::spawn(move || {
        let names = crate::pc_control::known_names();
        let waits = !crate::pc_help::is_help_request(&text)
            && !crate::pc_help::is_here_request(&text)
            && basic_plan(&text, &names).is_none()
            && is_dangling(&text, &names);
        let payload = serde_json::json!({
            "text": text,
            "detail": if waits { "utterance, waiting for the rest" } else { "utterance" },
            "margin": 0.0,
            "threshold": live_prob(),
            "sure": false,
            "stable": false,
            "live_safe": false,
        });
        crate::ffi::emit_event("pc_live", &payload.to_string());
        if waits {
            crate::log(&format!("[PcLive] {} chars wait for the rest", text.len()));
            return;
        }
        {
            let mut g = SHADOW.lock().unwrap_or_else(|e| e.into_inner());
            let Some(s) = g.as_mut() else { return };
            if words <= s.used {
                return;
            }
            s.used = words;
            s.handled += 1;
        }
        run_claimed(text, None, "utterance");
    });
}

static SHADOW: Mutex<Option<Shadow>> = Mutex::new(None);

/// Commands of one session run one at a time, in the order they were said:
/// "apri Notepad++" must be done before "salva tutto" looks for its menu.
static RUN: Mutex<()> = Mutex::new(());

pub fn shadow_begin() {
    *SHADOW.lock().unwrap_or_else(|e| e.into_inner()) = Some(Shadow {
        started: std::time::Instant::now(),
        obs: Vec::new(),
        used: 0,
        handled: 0,
        // `prepare` reads it when the session starts.
        read_front: true,
        utterances: false,
        heard: String::new(),
    });
}

fn recording_execute() -> bool {
    use crate::dictation_session::{Kind, Phase};
    matches!(
        crate::dictation_session::phase(),
        Phase::Recording {
            kind: Kind::Execute,
            ..
        }
    )
}

/// The words a partial adds after the `used` ones, and its word count.
/// None when it adds nothing (Nemotron may also revise words it sent).
fn segment(cumulative: &str, used: usize) -> Option<(String, usize)> {
    let w: Vec<&str> = cumulative.split_whitespace().collect();
    (w.len() > used).then(|| (w[used..].join(" "), w.len()))
}

/// Take the partial observed at `ms` as a finished command, if it is still
/// the newest one: its words are used from now on, the next command starts
/// after them. `live_only` takes it only when its plan is sure and harmless.
fn claim(s: &mut Shadow, ms: u128, live_only: bool) -> Option<(String, Option<Plan>)> {
    let o = s.obs.last().filter(|o| o.ms == ms)?;
    let sure = o.whole
        && o.margin >= live_prob()
        && o.plan.as_ref().is_some_and(crate::pc_control::live_safe);
    if !sure && (live_only || o.dangling) {
        return None;
    }
    let taken = (o.text.clone(), o.plan.clone().filter(|_| sure));
    assert!(
        o.words > s.used,
        "a partial is observed only when it adds words"
    );
    s.used = o.words;
    s.handled += 1;
    s.obs.clear();
    s.read_front = false;
    Some(taken)
}

/// Run what a claim took, after the commands before it, then read the
/// window in front again: the next command may be for the app just opened.
fn run_claimed(text: String, plan: Option<Plan>, why: &str) {
    let _order = RUN.lock().unwrap_or_else(|e| e.into_inner());
    let t = std::time::Instant::now();
    let (out, ran) = match plan {
        Some(p) => {
            let r = crate::pc_control::run(&p);
            let out = crate::pc_control::Outcome {
                ok: r.is_ok(),
                said: p.say.clone(),
                paste: r.as_ref().ok().cloned().flatten(),
                engine: "live".into(),
                error: r.err().map(|e| crate::truncate_utf8(&e, 200).to_string()),
                ..Default::default()
            };
            if out.ok {
                crate::pc_control::remember(&text, &out.said);
            }
            (out, Some(p))
        }
        // Claude only for two words or more: one word that no rule knows
        // is more often a cut word or a noise than a command.
        None => crate::ffi::pc_execute_text(&text, text.split_whitespace().count() >= 2),
    };
    crate::log(&format!(
        "[PcLive] {why} on {} chars: {} ok={} error={:?} in {} ms",
        text.len(),
        ran.as_ref().map_or_else(
            || "no plan".to_string(),
            |p| crate::pc_control::kinds(&p.steps)
        ),
        out.ok,
        out.error,
        t.elapsed().as_millis()
    ));
    let mut fired = serde_json::to_value(&out).unwrap_or_default();
    fired["text"] = text.into();
    fired["say"] = out.said.clone().into();
    crate::ffi::emit_event("pc_live_fired", &fired.to_string());
    // The next command must not wait for this: the app-commands layer checks
    // by itself that its snapshot is of the window in front.
    drop(_order);
    std::thread::sleep(std::time::Duration::from_millis(300));
    crate::pc_appcmds::capture_foreground();
}

/// A partial transcript arrived. Only an execute session that is still
/// recording is observed; the routing runs off the caller thread.
pub fn shadow_observe(cumulative: &str) {
    if !recording_execute() {
        return;
    }
    let (ms, text, words, first) = {
        let mut g = SHADOW.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = g.as_mut() else { return };
        let Some((text, words)) = segment(cumulative, s.used) else {
            return;
        };
        if s.obs.last().is_some_and(|o| o.text == text) {
            return;
        }
        let first = !std::mem::replace(&mut s.read_front, true);
        (s.started.elapsed().as_millis(), text, words, first)
    };
    // The first words of a command: read the window in front again while
    // the rest is said. The page a click loaded since the last command has
    // other things to click than the one read then.
    if first {
        crate::pc_appcmds::capture_foreground();
    }
    std::thread::spawn(move || {
        let names = crate::pc_control::known_names();
        // A question is answered at the pause, never run as a command.
        let (plan, margin, detail) = if crate::pc_help::is_help_request(&text) {
            (None, 0.0, "help".to_string())
        } else {
            route_scored(&text, &names)
        };
        let whole = last_word_whole(&text, &names);
        let dangling = plan.is_none() && is_dangling(&text, &names);
        let sure = whole && plan.is_some() && margin >= live_prob();
        let quiet_ms = if detail.starts_with(BASIC) {
            BASIC_QUIET_MS
        } else {
            LIVE_QUIET_MS
        };
        let mut stable = false;
        if let Some(s) = SHADOW.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            // A basic command is sure from its first words ("apri Chrome"
            // before "e mettilo a destra"): it waits for the pause.
            stable = sure
                && !detail.starts_with(BASIC)
                && s.obs.last().is_some_and(|prev| {
                    prev.whole
                        && prev.margin >= live_prob()
                        && prev
                            .plan
                            .as_ref()
                            .zip(plan.as_ref())
                            .is_some_and(|(a, b)| same_plan(a, b))
                });
            s.obs.push(Obs {
                ms,
                text: text.clone(),
                words,
                plan: plan.clone(),
                margin,
                whole,
                dangling,
            });
            s.obs.sort_by_key(|o| o.ms);
        }
        // What the local model makes of the words so far, for the live card.
        let payload = serde_json::json!({
            "ms": ms as u64,
            "text": text,
            "detail": detail,
            "margin": margin,
            "threshold": live_prob(),
            "quiet_ms": quiet_ms,
            "say": plan.as_ref().map(|p| p.say.clone()),
            "sure": sure,
            "stable": stable,
            "live_safe": plan.as_ref().is_some_and(crate::pc_control::live_safe),
        });
        crate::ffi::emit_event("pc_live", &payload.to_string());
        if !live_enabled() {
            return;
        }
        let mut waited = 0;
        if !stable {
            waited = quiet_ms;
            std::thread::sleep(std::time::Duration::from_millis(quiet_ms));
        }
        if let Some((text, plan)) = take(ms, true) {
            return run_claimed(text, plan, if stable { "stable" } else { "pause" });
        }
        std::thread::sleep(std::time::Duration::from_millis(SEGMENT_QUIET_MS - waited));
        if let Some((text, plan)) = take(ms, false) {
            run_claimed(text, plan, "segment end");
        }
    });
}

fn take(ms: u128, live_only: bool) -> Option<(String, Option<Plan>)> {
    if !recording_execute() {
        return None;
    }
    let mut g = SHADOW.lock().unwrap_or_else(|e| e.into_inner());
    claim(g.as_mut()?, ms, live_only)
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
            (Step::AppCommand { label: a }, Step::AppCommand { label: b }) => a == b,
            (Step::Type { text: a }, Step::Type { text: b }) => a == b,
            (Step::Close { app: p }, Step::Close { app: q }) => app(p, q),
            (Step::SwitchWindow, Step::SwitchWindow) => true,
            (Step::Click { item: a, .. }, Step::Click { item: b, .. }) => a == b,
            _ => false,
        })
}

/// The command ran with `final_plan`: log what each live rule would have done.
pub fn shadow_finish(final_plan: Option<&Plan>) {
    let Some(s) = SHADOW.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let now = s.started.elapsed().as_millis();
    let sure = |o: &Obs| o.plan.is_some() && o.margin >= live_prob();
    let first_sure = s.obs.iter().find(|o| sure(o));
    let first_stable = s.obs.windows(2).find(|w| {
        sure(&w[0])
            && sure(&w[1])
            && same_plan(w[0].plan.as_ref().unwrap(), w[1].plan.as_ref().unwrap())
    });
    let verdict = |o: Option<&Obs>| match (o, final_plan) {
        (None, _) => "never".to_string(),
        (Some(o), Some(f)) => format!(
            "at {} ms ({} ms before execute) on {} chars: {}",
            o.ms,
            now.saturating_sub(o.ms),
            o.text.len(),
            if same_plan(o.plan.as_ref().unwrap(), f) {
                "SAME plan"
            } else {
                "DIFFERENT plan"
            }
        ),
        (Some(o), None) => format!(
            "at {} ms on {} chars: final had no plan",
            o.ms,
            o.text.len()
        ),
    };
    crate::log(&format!(
        "[PcShadow] partials={} sure: {} | sure+stable: {} | final={}",
        s.obs.len(),
        verdict(first_sure),
        verdict(first_stable.map(|w| &w[1])),
        final_plan.map_or_else(
            || "no plan".to_string(),
            |p| crate::pc_control::kinds(&p.steps)
        )
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
            ("Outlook", ""),
        ]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect()
    }

    // Burned 2026-11-07: e5-small finished downloading after the user had
    // switched to BGE-M3, and the postcondition checked BGE-M3's files.
    #[test]
    fn a_finished_download_is_checked_against_its_own_model() {
        let dir = std::env::temp_dir().join(format!("pc-router-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = &MODELS[0];
        for (_, name, _) in small.files {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        let m3 = MODELS.iter().find(|m| m.id == "bge-m3").unwrap();
        assert!(files_present(&dir, small));
        assert!(!files_present(&dir.join(m3.subdir), m3));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_examples_asset_trains_every_intent() {
        let ex = examples();
        for key in [
            "open",
            "tile",
            "move",
            "minimize",
            "find_files",
            "web",
            "browser",
            "media",
            "folder",
            "settings",
            "none",
        ] {
            let i = Intent::from_key(key).unwrap();
            assert!(
                ex.iter().filter(|(k, _)| *k == i).count() >= 20,
                "{key} needs examples"
            );
        }
    }

    #[test]
    fn the_embedding_cache_round_trips_and_is_per_encoder() {
        let dir = std::env::temp_dir().join(format!("dimmy-pc-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.f32");
        let x = vec![vec![0.5f32, -0.25, 1.0], vec![0.0, 0.125, -1.0]];
        write_cache(&path, &x);
        assert_eq!(read_cache(&path, 2), Some(x));
        assert_eq!(
            read_cache(&path, 3),
            None,
            "a different example count is a miss"
        );
        std::fs::write(&path, [1u8, 2, 3]).unwrap();
        assert_eq!(
            read_cache(&path, 2),
            None,
            "a broken file is a miss, not a panic"
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_ne!(cache_key(&MODELS[0]), cache_key(&MODELS[2]));
        assert!(MODELS
            .iter()
            .all(|m| m.files.iter().all(|(_, _, sha)| sha.len() == 64)));
    }

    #[test]
    fn softmax_training_separates_and_gives_probabilities() {
        let x = vec![
            vec![1.0, 0.0, 0.0],
            vec![0.9, 0.1, 0.0],
            vec![0.0, 1.0, 0.0],
            vec![0.1, 0.9, 0.0],
            vec![0.0, 0.0, 1.0],
            vec![0.0, 0.1, 0.9],
        ];
        let y = vec![0, 0, 1, 1, 2, 2];
        let (w, b) = train_softmax(&x, &y, 3);
        let mut p = vec![0f32; 3];
        for (xi, &yi) in x.iter().zip(&y) {
            softmax_into(&w, &b, xi, &mut p);
            assert!((p.iter().sum::<f32>() - 1.0).abs() < 1e-4);
            let best = p
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap()
                .0;
            assert_eq!(best, yi);
        }
    }

    /// The real model against the independent test set (tests/fixtures/pc_testset.jsonl):
    /// `DIMMY_PC_ROUTER_DIR=<model dir>
    /// cargo test --release --lib --features pc-router independent -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn the_independent_set_routes_like_the_offline_bench() {
        let path = std::env::var("DIMMY_PC_TESTSET").unwrap_or_else(|_| {
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/pc_testset.jsonl"
            )
            .into()
        });
        let trained: std::collections::HashSet<String> = examples()
            .into_iter()
            .map(|(_, t)| t.to_lowercase())
            .collect();
        let t_load = std::time::Instant::now();
        let mut r = Router::load().expect("set DIMMY_PC_ROUTER_DIR");
        println!(
            "encoder {}: loaded in {} ms",
            selected().id,
            t_load.elapsed().as_millis()
        );
        let (mut n, mut ok, mut ms) = (0usize, 0usize, Vec::new());
        let at = |thr: f32, rows: &[(Intent, Intent, f32)]| {
            let sure: Vec<_> = rows
                .iter()
                .filter(|(g, _, p)| *g != Intent::None && *p >= thr)
                .collect();
            let wrong = sure.iter().filter(|(g, w, _)| g != w).count();
            let fa = sure.iter().filter(|(_, w, _)| *w == Intent::None).count();
            let inscope = rows.iter().filter(|(_, w, _)| *w != Intent::None).count();
            let right = sure.iter().filter(|(g, w, _)| g == w).count();
            println!(
                "p >= {thr:.2}: sure {} wrong {wrong} (out-of-scope run {fa}) coverage {:.1}%",
                sure.len(),
                100.0 * right as f64 / inscope.max(1) as f64
            );
            (wrong, fa)
        };
        let mut rows = Vec::new();
        for line in std::fs::read_to_string(&path).unwrap().lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let text = v["text"].as_str().unwrap_or("");
            if text.is_empty() || trained.contains(&text.to_lowercase()) {
                continue;
            }
            let want = Intent::from_key(v["intent"].as_str().unwrap_or("")).unwrap();
            let t = std::time::Instant::now();
            let (got, p) = r.classify(text).unwrap();
            ms.push(t.elapsed().as_secs_f64() * 1000.0);
            n += 1;
            ok += usize::from(got == want);
            rows.push((got, want, p));
        }
        ms.sort_by(f64::total_cmp);
        println!(
            "right {ok}/{n} ({:.1}%), median {:.1} ms",
            100.0 * ok as f64 / n as f64,
            ms[ms.len() / 2]
        );
        for t in [0.50f32, 0.55, 0.60, 0.65, 0.70, 0.75, 0.80, 0.85, 0.90] {
            at(t, &rows);
        }
        at(min_prob(), &rows);
        let (wrong, fa) = at(live_prob(), &rows);
        assert!(ok as f64 / n as f64 >= 0.75, "the classifier regressed");
        assert!(
            wrong <= 1 && fa == 0,
            "the live threshold must not act wrongly"
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

    /// The basic path on the labelled examples and this PC's app names:
    /// `cargo test --lib --features pc-router basic_on_the_examples -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn basic_on_the_examples() {
        let names = crate::pc_control::known_names();
        let v: serde_json::Value =
            serde_json::from_str(include_str!("../assets/pc-router-examples.json")).unwrap();
        let (mut taken, mut wrong, mut total) = (0, 0, 0);
        for (k, list) in v.as_object().unwrap() {
            let Some(want) = Intent::from_key(k) else {
                continue;
            };
            for ex in list.as_array().unwrap() {
                let ex = ex.as_str().unwrap();
                total += 1;
                let hit = basic_plan(ex, &names);
                if hit.is_none() && want != Intent::None {
                    eprintln!("MISS {k}: {ex:?}");
                }
                if let Some((got, p)) = hit {
                    taken += 1;
                    if got != want {
                        wrong += 1;
                        eprintln!("WRONG {k}: {ex:?} -> {got:?} {:?}", p.steps);
                    }
                }
            }
        }
        eprintln!("basic took {taken}/{total}, wrong {wrong}");
        assert_eq!(wrong, 0);
    }

    #[test]
    #[serial_test::serial]
    fn a_session_runs_each_command_on_its_own_words() {
        assert_eq!(
            segment("apri Notepad++", 0),
            Some(("apri Notepad++".into(), 2))
        );
        assert_eq!(
            segment("apri Notepad++ salva tutto", 2),
            Some(("salva tutto".into(), 4))
        );
        assert_eq!(segment("apri Notepad++", 2), None);
        let mut s = Shadow {
            started: std::time::Instant::now(),
            obs: Vec::new(),
            used: 0,
            handled: 0,
            read_front: true,
            utterances: false,
            heard: String::new(),
        };
        let sure = Plan {
            say: String::new(),
            steps: vec![Step::Media {
                action: "play_pause".into(),
            }],
        };
        let obs = |ms, text: &str, words, plan: Option<Plan>, margin| Obs {
            ms,
            text: text.into(),
            words,
            plan,
            margin,
            whole: true,
            dangling: false,
        };
        s.obs.push(obs(10, "pausa", 1, Some(sure.clone()), 1.0));
        assert!(
            claim(&mut s, 9, true).is_none(),
            "an older partial is not taken"
        );
        let (text, plan) = claim(&mut s, 10, true).unwrap();
        assert_eq!((text.as_str(), plan.is_some()), ("pausa", true));
        assert_eq!((s.used, s.handled, s.obs.len()), (1, 1, 0));
        // Not sure: the pause leaves it, the end of the command takes it.
        s.obs.push(obs(20, "salva tutto", 3, None, 0.2));
        assert!(claim(&mut s, 20, true).is_none());
        let (text, plan) = claim(&mut s, 20, false).unwrap();
        assert_eq!((text.as_str(), plan.is_none()), ("salva tutto", true));
        assert_eq!(s.used, 3);
        s.obs.push(obs(30, "e chiudi", 5, None, 0.1));
        *SHADOW.lock().unwrap() = Some(s);
        assert_eq!(session_rest().as_deref(), Some("e chiudi"));
        *SHADOW.lock().unwrap() = None;
        shadow_begin();
        assert_eq!(
            session_rest(),
            None,
            "no command ran: the stop runs the transcript"
        );
        *SHADOW.lock().unwrap() = None;
    }

    #[test]
    fn basic_commands_run_without_the_classifier() {
        let n = names();
        let plan = |c: &str| basic_plan(c, &n).map(|(_, p)| p.steps);
        let place = |app: &str, side| {
            Some(vec![Step::Place {
                app: app.into(),
                side,
            }])
        };
        assert_eq!(
            plan("Ingrandisci Notepad++"),
            place("notepad++", Side::Full)
        );
        assert_eq!(
            plan("metti chrome a tutto schermo, per favore"),
            place("chrome", Side::Full)
        );
        assert_eq!(
            plan("sposta Outlook a sinistra"),
            place("outlook", Side::Left)
        );
        assert_eq!(plan("questa finestra a destra"), place("", Side::Right));
        assert_eq!(
            plan("riduci ad icona Chrome"),
            Some(vec![Step::Minimize {
                app: "chrome".into()
            }])
        );
        assert_eq!(
            plan("apri Excel"),
            Some(vec![Step::Open {
                app: "excel".into()
            }])
        );
        assert_eq!(
            plan("affianca Teams e Outlook"),
            Some(vec![Step::Tile {
                left: "teams".into(),
                right: "outlook".into()
            }])
        );
        let one = |s: Step| Some(vec![s]);
        assert_eq!(
            plan("metti in pausa"),
            one(Step::Media {
                action: "play_pause".into()
            })
        );
        assert_eq!(
            plan("alza il volume"),
            one(Step::Media {
                action: "volume_up".into()
            })
        );
        assert_eq!(
            plan("precedente"),
            one(Step::Media {
                action: "previous".into()
            })
        );
        assert_eq!(
            plan("scheda precedente"),
            one(Step::Browser {
                action: "previous_tab".into()
            })
        );
        assert_eq!(
            plan("nuova scheda"),
            one(Step::Browser {
                action: "new_tab".into()
            })
        );
        assert_eq!(
            plan("apri le impostazioni bluetooth"),
            one(Step::Settings {
                page: "bluetooth".into()
            })
        );
        assert_eq!(
            plan("apri la cartella download"),
            one(Step::OpenFolder {
                folder: "downloads".into()
            })
        );
    }

    #[test]
    fn a_basic_command_needs_every_word_accounted_for() {
        let n = names();
        for c in [
            "cerca come mettere chrome a sinistra",
            "ingrandisci a tutto scher",
            "metti Excel a destra",
            "ferma la riunione su Teams",
            "apri il file del preventivo",
            "",
        ] {
            assert!(basic_plan(c, &n).is_none(), "{c}");
        }
    }

    #[test]
    fn moving_acts_only_on_an_open_window() {
        let names = vec![
            ("Notepad++".to_string(), "nuovo 1 - Notepad++".to_string()),
            ("Tastiera su schermo".to_string(), String::new()),
        ];
        let plan = |i, c| plan_for(i, c, &names).map(|p| p.steps);
        assert_eq!(plan(Intent::Move, "ingrandisci a tutto scher"), None);
        assert_eq!(
            plan(Intent::Minimize, "riduci a icona tastiera su schermo"),
            None
        );
        assert_eq!(
            plan(Intent::Move, "ingrandisci notepad++"),
            Some(vec![Step::Place {
                app: "notepad++".into(),
                side: Side::Full
            }])
        );
        assert!(plan(Intent::Open, "apri tastiera su schermo").is_some());
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
    #[test]
    fn windows_are_closed_restored_and_switched_by_their_words() {
        let n = names();
        let plan = |c: &str| basic_plan(c, &n).map(|(_, p)| p.steps);
        let one = |s: Step| Some(vec![s]);
        assert_eq!(
            plan("chiudi Notepad++"),
            one(Step::Close {
                app: "notepad++".into()
            })
        );
        assert_eq!(
            plan("chiudi questa finestra"),
            one(Step::Close { app: String::new() })
        );
        assert_eq!(
            plan("chiudi la scheda"),
            one(Step::Browser {
                action: "close_tab".into()
            })
        );
        // Nothing says what to close: never the window that happens to be in front.
        assert_eq!(plan("chiudi"), None);
        assert_eq!(plan("chiudi tutto"), None);
        assert_eq!(
            plan("ripristina Chrome"),
            one(Step::Restore {
                app: "chrome".into()
            })
        );
        for c in ["passa a Teams", "vai su Teams", "torna su Teams"] {
            assert_eq!(
                plan(c),
                one(Step::Open {
                    app: "teams".into()
                }),
                "{c}"
            );
        }
        for c in [
            "finestra precedente",
            "cambia finestra",
            "torna alla finestra di prima",
            "alt tab",
        ] {
            assert_eq!(plan(c), one(Step::SwitchWindow), "{c}");
        }
        for c in [
            "mostra il desktop",
            "vai al desktop",
            "riduci tutto a icona",
        ] {
            assert_eq!(plan(c), one(Step::ShowDesktop), "{c}");
        }
        assert_eq!(
            plan("apri la cartella desktop"),
            one(Step::OpenFolder {
                folder: "desktop".into()
            })
        );
        assert_eq!(plan("fai uno screenshot"), one(Step::Screenshot));
        assert_eq!(
            plan("vai su YouTube"),
            one(Step::Web {
                site: "youtube".into(),
                query: String::new()
            })
        );
    }

    #[test]
    fn keys_are_pressed_by_name() {
        let n = names();
        let key = |c: &str| {
            basic_plan(c, &n).map(|(_, p)| match p.steps.as_slice() {
                [Step::Key { app, action }] if app.is_empty() => action.clone(),
                other => panic!("{c}: {other:?}"),
            })
        };
        for (said, action) in [
            ("copia", "copy"),
            ("incolla", "paste"),
            ("seleziona tutto", "select_all"),
            ("vai a capo", "new_line"),
            ("vai in fondo", "bottom"),
            ("scorri giù", "page_down"),
            ("scorri su", "page_up"),
            ("premi invio", "enter"),
            ("premi il tasto escape", "escape"),
            ("premi control s", "ctrl+s"),
            ("premi ctrl+shift+t", "ctrl+shift+t"),
            ("premi alt f4", "alt+f4"),
            ("premi freccia giù", "down"),
            ("premi pagina giù", "page_down"),
            ("premi windows", "win"),
        ] {
            assert_eq!(key(said).as_deref(), Some(action), "{said}");
        }
        // A word that is not a key is not guessed: the planner reads it.
        for said in ["premi banana", "premi", "salva tutto", "scorri"] {
            assert!(key(said).is_none(), "{said}");
        }
    }

    #[test]
    fn text_after_scrivi_is_typed_as_it_was_said() {
        let n = names();
        let plan = |c: &str| basic_plan(c, &n).map(|(_, p)| p.steps);
        assert_eq!(
            plan("Scrivi Ciao a tutti, come va?"),
            Some(vec![Step::Type {
                text: "Ciao a tutti, come va?".into()
            }])
        );
        // A place for the text, or an open app: Claude reads those.
        assert_eq!(plan("scrivi in fondo a Notepad++ ciao"), None);
        assert_eq!(plan("scrivi ciao su Notepad++"), None);
        assert_eq!(plan("scrivi"), None);
    }

    #[test]
    #[serial_test::serial]
    fn a_pronoun_means_the_app_just_named() {
        let n = names();
        let plan = |c: &str| basic_plan(c, &n).map(|(_, p)| p.steps);
        crate::pc_control::set_last_app("chrome");
        assert_eq!(
            plan("mettilo a destra"),
            Some(vec![Step::Place {
                app: "chrome".into(),
                side: Side::Right
            }])
        );
        assert_eq!(
            plan("riducilo a icona"),
            Some(vec![Step::Minimize {
                app: "chrome".into()
            }])
        );
        // Its window is gone, or nothing was named: the window in front.
        for last in ["paint", ""] {
            crate::pc_control::set_last_app(last);
            assert_eq!(
                plan("ingrandiscilo"),
                Some(vec![Step::Place {
                    app: String::new(),
                    side: Side::Full
                }]),
                "{last:?}"
            );
        }
        assert_eq!(
            plan("ingrandisci"),
            None,
            "no target named, none pointed at"
        );
    }

    // Burned 2026-11-08: "questa sche" (on its way to "questa scheda") ran
    // after 600 ms and opened the on-screen keyboard.
    #[test]
    fn a_half_said_last_word_does_not_run_live() {
        let mut n = names();
        n.push(("Tastiera su schermo".to_string(), String::new()));
        for whole in [
            "apri notepad++",
            "apri il browser",
            "alza il volume",
            "metti chrome a destra",
            "vai su youtube",
        ] {
            assert!(last_word_whole(whole, &n), "{whole}");
        }
        for cut in [
            "questa sche",
            "apri note",
            "ingrandisci a tutto scher",
            "cerca su youtube gat",
            "",
        ] {
            assert!(!last_word_whole(cut, &n), "{cut}");
        }
    }

    #[test]
    fn a_command_still_being_said_is_not_dropped() {
        let n = names();
        for c in ["apri", "metti chrome a", "apri il", "chiudi la"] {
            assert!(is_dangling(c, &n), "{c}");
        }
        for c in ["se non ne fa", "santura così", ""] {
            assert!(!is_dangling(c, &n), "{c}");
        }
        let mut s = Shadow {
            started: std::time::Instant::now(),
            obs: vec![Obs {
                ms: 10,
                text: "apri".into(),
                words: 1,
                plan: None,
                margin: 0.0,
                whole: true,
                dangling: true,
            }],
            used: 0,
            handled: 0,
            read_front: true,
            utterances: false,
            heard: String::new(),
        };
        assert!(
            claim(&mut s, 10, false).is_none(),
            "the segment end leaves it"
        );
        assert_eq!((s.used, s.handled, s.obs.len()), (0, 0, 1));
        // A sure plan whose last word is half said waits for the segment end.
        s.obs[0] = Obs {
            ms: 10,
            text: "pausa".into(),
            words: 1,
            plan: Some(Plan {
                say: String::new(),
                steps: vec![Step::Media {
                    action: "play_pause".into(),
                }],
            }),
            margin: 1.0,
            whole: false,
            dangling: false,
        };
        assert!(claim(&mut s, 10, true).is_none());
        let (_, plan) = claim(&mut s, 10, false).unwrap();
        assert!(
            plan.is_none(),
            "the segment end plans it again on the full words"
        );
    }
    /// The word rules against the independent set, with no model: what they
    /// take, and where they disagree with the label.
    /// `cargo test --lib --features pc-router rules_on_the_independent_set -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn rules_on_the_independent_set() {
        let n = names();
        let (mut taken, mut total) = (0, 0);
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/pc_testset.jsonl"
        ))
        .unwrap();
        for line in text.lines() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let (said, label) = (
                v["text"].as_str().unwrap_or(""),
                v["intent"].as_str().unwrap_or(""),
            );
            total += 1;
            if let Some((got, p)) = basic_plan(said, &n) {
                taken += 1;
                if Intent::from_key(label) != Some(got) {
                    eprintln!("DIFF {label}: {said:?} -> {got:?} {:?}", p.steps);
                }
            }
        }
        eprintln!("rules took {taken}/{total}");
    }

    // An engine that hands over whole utterances: the stop runs only the
    // words after the last command, from what the engine wrote.
    #[test]
    #[serial_test::serial]
    fn the_stop_runs_what_the_utterances_left() {
        let session = |used, handled, heard: &str| Shadow {
            started: std::time::Instant::now(),
            obs: Vec::new(),
            used,
            handled,
            read_front: true,
            utterances: true,
            heard: heard.into(),
        };
        *SHADOW.lock().unwrap() = Some(session(2, 1, "apri chrome chiudi la"));
        assert_eq!(session_rest().as_deref(), Some("chiudi la"));
        *SHADOW.lock().unwrap() = Some(session(2, 1, "apri chrome"));
        assert_eq!(session_rest().as_deref(), Some(""));
        *SHADOW.lock().unwrap() = Some(session(0, 0, "apri"));
        assert_eq!(session_rest(), None, "nothing ran: the stop runs it all");
        *SHADOW.lock().unwrap() = None;
    }
}
