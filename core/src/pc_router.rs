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
//! 34%, e5 then Laya 44%. Below `MIN_PROB`, on "none", or when a slot can't
//! be filled, the router answers `None` and the command goes to Claude.
//!
//! Slots without a model: side and date from keywords, app names by fuzzy
//! match of the command's words against open windows and Start-menu apps.

use crate::pc_control::{name_score, norm, Plan, Side, Step, When};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use std::sync::Mutex;
use tokenizers::Tokenizer;

/// After the stop the local plan runs at this probability, else Claude decides.
const MIN_PROB: f32 = 0.50;

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

/// multilingual-e5-small int8, pinned to a revision; each file to its SHA-256.
const HF_BASE: &str =
    "https://huggingface.co/Xenova/multilingual-e5-small/resolve/761b726dd34fb83930e26aab4e9ac3899aa1fa78";
const FILES: [(&str, &str, &str); 2] = [
    (
        "onnx/model_quantized.onnx",
        "model_quantized.onnx",
        "f80102d3f2a1229f387d3c81909990d8945513e347b0eab049f7de3c6f98c193",
    ),
    (
        "tokenizer.json",
        "tokenizer.json",
        "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39",
    ),
];
pub const MODEL_SIZE_MB: u32 = 129;

pub fn model_present() -> bool {
    let Some(dir) = model_dir() else {
        return false;
    };
    FILES.iter().all(|(_, name, _)| {
        std::fs::metadata(dir.join(name))
            .map(|m| m.is_file() && m.len() > 0)
            .unwrap_or(false)
    })
}

/// Download the router model. Resumable; every file is checked against its
/// pinned SHA-256 and deleted on mismatch, so a retry starts clean.
pub async fn download(progress: impl Fn(u64, u64)) -> Result<(), String> {
    let dir = model_dir().ok_or("config dir unknown")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {:?}: {e}", dir))?;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(1800))
        .build()
        .map_err(|e| format!("http client: {e}"))?;
    let total: u64 = MODEL_SIZE_MB as u64 * 1024 * 1024;
    let mut base: u64 = 0;
    for (remote, name, sha) in FILES {
        let dest = dir.join(name);
        if crate::download::verify_file(&dest, &[], Some(sha)).is_ok() {
            base += std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            progress(base, total);
            continue;
        }
        let _ = std::fs::remove_file(&dest);
        let url = format!("{HF_BASE}/{remote}");
        crate::log(&format!("[PcRouter] downloading {name}"));
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
    assert!(model_present(), "both files must exist after a download");
    Ok(())
}

struct Router {
    session: Session,
    tok: Tokenizer,
    needs_token_types: bool,
    classes: Vec<Intent>,
    w: Vec<f32>,
    b: Vec<f32>,
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
            needs_token_types,
            classes: Vec::new(),
            w: Vec::new(),
            b: Vec::new(),
        };
        let ex = examples();
        let t_embed = std::time::Instant::now();
        let mut classes: Vec<Intent> = Vec::new();
        let (mut x, mut y) = (Vec::with_capacity(ex.len()), Vec::with_capacity(ex.len()));
        for (intent, text) in &ex {
            let c = match classes.iter().position(|k| k == intent) {
                Some(c) => c,
                None => {
                    classes.push(*intent);
                    classes.len() - 1
                }
            };
            x.push(r.embed(text)?);
            y.push(c);
        }
        let embed_ms = t_embed.elapsed().as_millis();
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
                *ROUTER.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                crate::log(&format!(
                    "[PcRouter] e5 ready in {} ms",
                    t.elapsed().as_millis()
                ));
            }
            Err(e) => crate::log(&format!("[PcRouter] e5 unavailable: {e}")),
        }
        LOADING.store(false, SeqCst);
    });
}

/// A plan when the router is sure, else `None` (the caller asks Claude).
/// `detail` says why, for the log.
pub fn route(command: &str, names: &[(String, String)]) -> (Option<Plan>, String) {
    let (plan, margin, detail) = route_scored(command, names);
    if margin < MIN_PROB {
        return (None, format!("{detail} below {MIN_PROB}"));
    }
    (plan, detail)
}

/// The plan the slots allow (whatever the margin), the margin, and why.
fn route_scored(command: &str, names: &[(String, String)]) -> (Option<Plan>, f32, String) {
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
        Intent::Move => match (side_of(&w), apps.as_slice()) {
            (Some(side), [app]) => Some(Plan {
                say: format!("Sposto {app} {}", side_label(side)),
                steps: vec![Step::Place {
                    app: app.clone(),
                    side,
                }],
            }),
            (Some(side), []) if this_window => Some(Plan {
                say: format!("Sposto la finestra {}", side_label(side)),
                steps: vec![Step::Place {
                    app: String::new(),
                    side,
                }],
            }),
            _ => None,
        },
        Intent::Minimize if apps.len() == 1 => Some(Plan {
            say: format!("Riduco a icona {}", apps[0]),
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

/// Stricter than `MIN_PROB`: acting before the user is done. On the
/// independent 326-command set nothing wrong, and no out-of-scope line, got
/// past 0.60.
const LIVE_PROB: f32 = 0.60;

/// A sure plan runs once no new words have come for this long: Nemotron
/// sends a partial only when the words change, so a pause is the only
/// "done" signal there is (a second identical partial never arrives).
const LIVE_QUIET_MS: u64 = 600;

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

/// Run the plan of the partial observed at `ms` if it is still the newest
/// one, sure, harmless, and nothing ran yet. The claim is taken under the
/// lock, so the stop path (which checks `live_fired`) never runs it twice.
fn try_fire(ms: u128, why: &str) {
    if !live_enabled() || !recording_execute() {
        return;
    }
    let claimed = {
        let mut g = SHADOW.lock().unwrap_or_else(|e| e.into_inner());
        let Some(s) = g.as_mut() else { return };
        let ready = !s.fired
            && s.obs.last().is_some_and(|o| {
                o.ms == ms
                    && o.margin >= LIVE_PROB
                    && o.plan.as_ref().is_some_and(crate::pc_control::live_safe)
            });
        if !ready {
            return;
        }
        s.fired = true;
        let o = s.obs.last().expect("ready implies an observation");
        (
            o.plan.clone().expect("ready implies a plan"),
            o.text.clone(),
        )
    };
    let (plan, text) = claimed;
    let t = std::time::Instant::now();
    let result = crate::pc_control::run(&plan);
    crate::log(&format!(
        "[PcLive] fired ({why}) at {ms} ms on {text:?}: {:?} -> {:?} in {} ms",
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

/// A partial transcript arrived. Only an execute session that is still
/// recording is observed; the routing runs off the caller thread.
pub fn shadow_observe(cumulative: &str) {
    if !recording_execute() {
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
        let sure = plan.is_some() && margin >= LIVE_PROB;
        let mut stable = false;
        if let Some(s) = SHADOW.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            stable = sure
                && s.obs.last().is_some_and(|prev| {
                    prev.margin >= LIVE_PROB
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
        }
        // What the local model makes of the words so far, for the live card.
        let payload = serde_json::json!({
            "ms": ms as u64,
            "text": text,
            "detail": detail,
            "margin": margin,
            "threshold": LIVE_PROB,
            "quiet_ms": LIVE_QUIET_MS,
            "say": plan.as_ref().map(|p| p.say.clone()),
            "sure": sure,
            "stable": stable,
            "live_safe": plan.as_ref().is_some_and(crate::pc_control::live_safe),
        });
        crate::ffi::emit_event("pc_live", &payload.to_string());
        if stable {
            try_fire(ms, "stable");
        } else if sure {
            std::thread::sleep(std::time::Duration::from_millis(LIVE_QUIET_MS));
            try_fire(ms, "pause");
        }
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
            (Step::AppCommand { label: a }, Step::AppCommand { label: b }) => a == b,
            _ => false,
        })
}

/// The command ran with `final_plan`: log what each live rule would have done.
pub fn shadow_finish(final_plan: Option<&Plan>) {
    let Some(s) = SHADOW.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return;
    };
    let now = s.started.elapsed().as_millis();
    let sure = |o: &Obs| o.plan.is_some() && o.margin >= LIVE_PROB;
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
        let mut r = Router::load().expect("set DIMMY_PC_ROUTER_DIR");
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
        at(MIN_PROB, &rows);
        let (wrong, fa) = at(LIVE_PROB, &rows);
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
