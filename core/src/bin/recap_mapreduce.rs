//! recap_mapreduce — hierarchical local recap: summarise the transcript in
//! pieces, then summarise the summaries.
//!
//! One pass over a whole meeting produces garbage on a 2-4B quantised model:
//! measured 2026-09-04 on a real 9099-word meeting, Gemma 4 E2B emitted
//! `===TLDR==`, collapsed its bullet lists onto one line, and stopped after
//! two of four sections. The SAME model on an 1800-word slice produced clean,
//! correct output. The failure is length, not capability — which is what the
//! literature reports (smaller chunks beat full context; "lost in the middle";
//! length-induced degradation dominating the error for long inputs).
//!
//! Usage:
//!   recap_mapreduce <transcript.txt> <out.md> [chunk_words]
//! Env: DIMMY_RECAP_LOCAL=<gguf>  (required)

use std::path::PathBuf;
use std::time::Instant;

fn map_prompt(part: usize, total: usize, chunk: &str) -> String {
    // Classify AT THE MAP STEP. Asking the reduce to sort a flat bullet list
    // into topics / decisions / actions produced three identical sections on a
    // real meeting (2026-09-04): the model cannot tell them apart once the
    // context that distinguished them is gone. Here it still has the words
    // that were actually said, and each line only has to be labelled.
    format!(
        "This is part {part} of {total} of a meeting transcript (raw          speech-to-text: errors, overlapping speakers, filler — ignore that).

         Write one line per substantive point, each starting with EXACTLY one          of these labels:
         TOPIC: something that was discussed
         DECISION: something that was decided
         ACTION: something someone agreed to do

         No preamble, no conclusion, no commentary about the transcript.          Answer in the SAME LANGUAGE as the transcript.

{chunk}"
    )
}

/// Group the labelled map lines. Sorting is mechanical here — the model
/// already made the judgement per line, with the transcript in front of it.
fn group(notes: &str) -> (Vec<String>, Vec<String>, Vec<String>) {
    let (mut t, mut d, mut a) = (Vec::new(), Vec::new(), Vec::new());
    for line in notes.lines() {
        let l = line.trim().trim_start_matches(['-', '*', ' ']).trim();
        if let Some(r) = l.strip_prefix("TOPIC:") {
            t.push(r.trim().to_string());
        } else if let Some(r) = l.strip_prefix("DECISION:") {
            d.push(r.trim().to_string());
        } else if let Some(r) = l.strip_prefix("ACTION:") {
            a.push(r.trim().to_string());
        }
    }
    (t, d, a)
}

fn tldr_prompt(notes: &str) -> String {
    format!(
        "Below are the points from one meeting.

         Write two or three sentences: what the meeting was about and what came          out of it. Answer in the SAME LANGUAGE as the points. Output only          those sentences.

Points:
{notes}"
    )
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let input = PathBuf::from(
        args.get(1)
            .ok_or("usage: recap_mapreduce <in> <out> [chunk_words]")?,
    );
    let output = PathBuf::from(args.get(2).ok_or("missing output path")?);
    let chunk_words: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1200);

    let model = dimmy_lib::local_llm::model_path(
        &std::env::var("DIMMY_RECAP_LOCAL").map_err(|_| "set DIMMY_RECAP_LOCAL")?,
    );
    let transcript = std::fs::read_to_string(&input)?;
    let words: Vec<&str> = transcript.split_whitespace().collect();
    let chunks: Vec<String> = words.chunks(chunk_words).map(|c| c.join(" ")).collect();
    eprintln!(
        "[mr] {} words -> {} chunks of {} words",
        words.len(),
        chunks.len(),
        chunk_words
    );

    let t_all = Instant::now();
    let mut notes = String::new();
    for (i, c) in chunks.iter().enumerate() {
        let t = Instant::now();
        let out = dimmy_lib::local_llm::process_raw_prompt_local(
            &model,
            &map_prompt(i + 1, chunks.len(), c),
            400,
        )?;
        eprintln!(
            "[mr] map {}/{}: {:.0}s, {} chars",
            i + 1,
            chunks.len(),
            t.elapsed().as_secs_f64(),
            out.len()
        );
        notes.push_str(out.trim());
        notes.push('\n');
    }

    let (topics, decisions, actions) = group(&notes);
    eprintln!(
        "[mr] notes {} chars -> {} topics, {} decisions, {} actions",
        notes.len(),
        topics.len(),
        decisions.len(),
        actions.len()
    );

    let t = Instant::now();
    let tldr = dimmy_lib::local_llm::process_raw_prompt_local(&model, &tldr_prompt(&notes), 400)?;
    eprintln!("[mr] tldr: {:.0}s", t.elapsed().as_secs_f64());
    eprintln!("[mr] TOTAL {:.0}s", t_all.elapsed().as_secs_f64());

    let bullets = |v: &Vec<String>| {
        if v.is_empty() {
            "- None".to_string()
        } else {
            v.iter().map(|x| format!("- {x}")).collect::<Vec<_>>().join(
                "
",
            )
        }
    };
    let final_out = format!(
        "===TLDR===
{}

===TOPICS===
{}

===DECISIONS===
{}

===ACTIONS===
{}
",
        tldr.trim(),
        bullets(&topics),
        bullets(&decisions),
        bullets(&actions)
    );

    std::fs::write(&output, &final_out)?;
    std::fs::write(output.with_extension("notes.txt"), &notes)?;
    Ok(())
}
