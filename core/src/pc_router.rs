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

use crate::pc_control::{name_score, Plan, Side, Step, When};
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
];

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
    let mut found: Vec<(usize, String)> = Vec::new();
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
            let score = names
                .iter()
                .map(|(name, title)| name_score(&gram, name, title))
                .fold(0.0f32, f32::max);
            if score >= 0.8 {
                found.push((i, gram));
                taken = n;
                break;
            }
        }
        i += taken.max(1);
    }
    let mut out: Vec<String> = Vec::new();
    for (_, g) in found {
        if !out.contains(&g) {
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
}

static SHADOW: Mutex<Option<Shadow>> = Mutex::new(None);

pub fn shadow_begin() {
    *SHADOW.lock().unwrap_or_else(|e| e.into_inner()) = Some(Shadow {
        started: std::time::Instant::now(),
        obs: Vec::new(),
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
        let (plan, margin, _) = route_scored(&text, &crate::pc_control::known_names());
        if let Some(s) = SHADOW.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            s.obs.push(Obs {
                ms,
                text,
                plan,
                margin,
            });
            s.obs.sort_by_key(|o| o.ms);
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
            ("vai su youtube", Open),
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
