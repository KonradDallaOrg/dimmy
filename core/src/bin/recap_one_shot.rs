//! One-shot meeting recap generator. Reads a transcript text file,
//! builds the same Notion-quality prompt the C# Meeting UI uses,
//! routes through `dimmy_lib::llm::process_raw_prompt`, and writes the
//! Anthropic Opus 4.7 (adaptive thinking) response to a markdown file.
//!
//! Used 2026-05-08 to recap the user's 95-min meeting transcript that
//! couldn't run through the UI because the meeting wasn't recorded via
//! Dimmy itself (file dropped from Notion export).
//!
//! Usage:
//!   cargo run --release --bin recap_one_shot -- \
//!     "C:\\Users\\konradd\\Downloads\\meeting.txt" \
//!     "C:\\Users\\konradd\\Downloads\\meeting_recap.md"
//!
//! Defaults: input  = C:\Users\konradd\Downloads\meeting.txt
//!           output = same dir, suffix `_recap.md`
//!
//! Reuses the API key already stored in Dimmy's keystore (keys.enc).

use std::path::PathBuf;

fn structured_recap_prompt(transcript: &str) -> String {
    // Verbatim port of MeetingWindow.xaml.cs::BuildStructuredRecapPrompt
    // (commit 0f5163f). Keep these two in lockstep — the parser on the C#
    // side splits on the exact ===KEY=== markers below.
    let mut s = String::with_capacity(transcript.len() + 8 * 1024);
    s.push_str("You are a senior meeting analyst writing a polished, Notion-style ");
    s.push_str("summary of an audio recording. Output ONLY markdown with the EXACT ");
    s.push_str("marker headings shown — a downstream parser splits on them.\n\n");

    s.push_str("## Transcript format\n");
    s.push_str("Each line: `[ELAPSED_MS ms] [SPEAKER_LABEL] text`.\n");
    s.push_str("Speaker labels: `[mic]` = the user recording (treat as \"you\" / first person ");
    s.push_str("when the language allows), `[system]` = remote participant(s) coming through ");
    s.push_str("speakers/loopback (treat as \"the remote party\" / \"interlocutor\" / specific ");
    s.push_str("name only if explicitly mentioned in the transcript). When only `[mic]` is ");
    s.push_str("present, the recording is monologue / dictation; when only `[system]` is ");
    s.push_str("present, the user was a silent listener.\n\n");

    s.push_str("## Output language\n");
    s.push_str("Auto-detect from the transcript. For mixed languages, pick the dominant one. ");
    s.push_str(
        "Do NOT translate. If the transcript is in Italian, write the recap in Italian.\n\n",
    );

    s.push_str("## Sections (emit ALL of them, in this order)\n\n");

    s.push_str("## ===CONTEXT===\n");
    s.push_str("2-4 sentences inferring the SETTING of the meeting from cues in the ");
    s.push_str("transcript: how many distinct voices, the apparent purpose (status sync? ");
    s.push_str("kickoff? interview? decision-making? brainstorm?), the apparent domain ");
    s.push_str("(engineering / sales / product / personal / academic). Don't invent names — ");
    s.push_str("say \"the user\" / \"the remote participant\" unless explicitly named.\n\n");

    s.push_str("## ===TLDR===\n");
    s.push_str("1-2 sentences. The single most important thing a busy reader needs to know. ");
    s.push_str(
        "Ruthless filtering: if you removed everything else, what would they still need?\n\n",
    );

    s.push_str("## ===HIGHLIGHTS===\n");
    s.push_str("3-5 most pivotal moments, one per line:\n");
    s.push_str("- `[MM:SS]` **Topic shorthand** — why this matters in one sentence.\n");
    s.push_str("Convert ELAPSED_MS to MM:SS. Pick highlights that change the meeting's ");
    s.push_str("trajectory: a decision, a conflict, a key reveal, a commitment, a moment of ");
    s.push_str("alignment after debate. Skip routine status updates. If fewer than 3 pivotal ");
    s.push_str("moments exist, list only what genuinely qualifies.\n\n");

    s.push_str("## ===NARRATIVE===\n");
    s.push_str("2-4 paragraphs of FLOWING PROSE (NOT bullets). Tell the story of the meeting: ");
    s.push_str("what was discussed, in which order, with which tone, who pushed for what, ");
    s.push_str("where alignment came easy and where it was contested. Reference `[mic]` / ");
    s.push_str("`[system]` inline when attribution matters. Quote a memorable phrase verbatim ");
    s.push_str("when it captures a moment (`\"...\"`). This is the section a busy stakeholder ");
    s.push_str("would read INSTEAD of listening to the recording.\n\n");

    s.push_str("## ===KEY_DECISIONS===\n");
    s.push_str("Bullet list of decisions actually MADE in the meeting (not proposed, not ");
    s.push_str("considered). Each item:\n");
    s.push_str("- **[topic]** : [decision verbatim or paraphrased] — decided by ");
    s.push_str("[owner if known else \"consensus\"]; impact: [low/medium/high] with one-sentence ");
    s.push_str("rationale.\n");
    s.push_str("Use a single line `—` if no decisions were reached.\n\n");

    s.push_str("## ===TOPICS===\n");
    s.push_str("Group the discussion into 3-7 distinct topics, sorted by importance (NOT ");
    s.push_str("chronological). For each:\n");
    s.push_str("- ### Topic title (1-3 words)\n");
    s.push_str("- 2-4 bullets capturing what was discussed\n");
    s.push_str("- _Sentiment_: aligned / debated / blocked / open\n");
    s.push_str("- _Quote_: `\"verbatim sentence from transcript\"` — only if a single ");
    s.push_str("sentence captures the moment; omit otherwise (do NOT approximate).\n\n");

    s.push_str("## ===ACTIONS===\n");
    s.push_str("Numbered list of action items EXPLICITLY committed to in the transcript. ");
    s.push_str("Never invent. Each action:\n");
    s.push_str("N. **[owner]** : [task] — due: [explicit date / event / \"unspecified\"]; ");
    s.push_str("priority: [P0/P1/P2 inferred from urgency cues like \"asap\", \"this week\", ");
    s.push_str("\"eventually\"].\n");
    s.push_str("Use `—` if no actions.\n\n");

    s.push_str("## ===OPEN_QUESTIONS===\n");
    s.push_str("Genuine questions raised but NOT resolved. Phrase as the question itself, ");
    s.push_str("not as \"they asked about X\". One bullet per question. Use `—` if none.\n\n");

    s.push_str("## ===RISKS===\n");
    s.push_str("Risks, blockers, dependencies surfaced. Each:\n");
    s.push_str(
        "- **[topic]** — [risk in 1 sentence]; likelihood [low/med/high], impact [low/med/high].\n",
    );
    s.push_str("Use `—` if none.\n\n");

    s.push_str("## ===NEXT_STEPS===\n");
    s.push_str("Numbered list of the 3-5 immediate next things that should happen for the ");
    s.push_str("meeting's trajectory (NOT the same as Actions — those are owned tasks; ");
    s.push_str("Next Steps is the work's direction). Use `—` if none.\n\n");

    s.push_str("## ===FOLLOWUPS===\n");
    s.push_str("Things to ASK / CLARIFY offline because the meeting didn't fully resolve them:\n");
    s.push_str("- **[topic]** — ask [person if mentioned else \"the relevant party\"] about ");
    s.push_str("[specific thing].\n");
    s.push_str("Use `—` if none.\n\n");

    s.push_str("## Hard rules\n");
    s.push_str("- Output the sections in the exact order above. ALL section markers must ");
    s.push_str("appear, even if the section content is just `—`.\n");
    s.push_str("- Output language follows the transcript dominant language.\n");
    s.push_str("- NEVER invent: participants, dates, amounts, project names, technical terms, ");
    s.push_str("deadlines, organizational affiliations, or anything not directly evidenced in ");
    s.push_str("the transcript. If unsure, omit rather than fabricate.\n");
    s.push_str("- Quotes (`\"...\"`) must be VERBATIM from the transcript. If you can't find ");
    s.push_str("an exact match, do NOT include the quote — paraphrase outside of quote marks ");
    s.push_str("or omit entirely.\n");
    s.push_str("- Convert ELAPSED_MS timestamps to MM:SS for display only. Use the original ");
    s.push_str("[N ms] only for internal reasoning if needed.\n");
    s.push_str("- No filler phrases (\"the meeting discussed\", \"various topics were covered\", ");
    s.push_str("\"in conclusion\", \"overall\").\n");
    s.push_str("- No em-dashes (`—`) in prose outside the markers and bullet separators. ");
    s.push_str("Use periods, commas, or colons instead.\n");
    s.push_str("- Be SHARP and CONCISE. Senior leaders read these summaries — every sentence ");
    s.push_str("must earn its place.\n\n");

    s.push_str("## Transcript\n");
    s.push_str(transcript);
    s
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let input = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| r"C:\Users\konradd\Downloads\meeting.txt".to_string());
    let output = args.get(2).cloned().unwrap_or_else(|| {
        let p = PathBuf::from(&input);
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("meeting");
        let dir = p.parent().unwrap_or_else(|| std::path::Path::new("."));
        dir.join(format!("{}_recap.md", stem))
            .to_string_lossy()
            .into_owned()
    });

    eprintln!("[recap] reading transcript from {}", input);
    let transcript = std::fs::read_to_string(&input)?;
    eprintln!("[recap] transcript: {} chars", transcript.len());

    let prompt = structured_recap_prompt(&transcript);
    eprintln!("[recap] prompt: {} chars", prompt.len());

    // Pull the Anthropic LLM key from Dimmy's local encrypted keystore.
    let ks = dimmy_lib::keystore::KeyStore::new();
    let api_key = ks
        .load_key(
            dimmy_lib::provider::KeyringScope::Llm(dimmy_lib::provider::Provider::Anthropic),
            false,
        )
        .ok_or("No Anthropic LLM key in keystore — set one via Settings first")?;

    let api_url = "https://api.anthropic.com/v1/messages";
    let model = "claude-opus-4-7";
    let max_tokens = 32_000u64;

    eprintln!(
        "[recap] calling {} with model={} (key {} chars)",
        api_url,
        model,
        api_key.len()
    );
    let t = std::time::Instant::now();
    let response =
        dimmy_lib::llm::process_raw_prompt(api_url, model, &api_key, &prompt, max_tokens).await?;
    let elapsed = t.elapsed().as_secs_f32();
    eprintln!(
        "[recap] received {} chars in {:.1}s",
        response.len(),
        elapsed
    );

    std::fs::write(&output, &response)?;
    eprintln!("[recap] wrote {}", output);

    Ok(())
}
