//! Does speaker diarization actually work on Dimmy's own meeting audio?
//!
//! Sends a past meeting's SYSTEM channel to Deepgram with diarize=true and
//! prints the speaker-labelled turns. The mic channel is deliberately not
//! sent: that one is already known to be the user, for free, by construction.
//!
//! CARGO_TARGET_DIR=E:\probe cargo run --release --bin diarize_probe -- <ogg>

use dimmy_lib::provider::{KeyringScope, Provider};

#[tokio::main]
async fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: diarize_probe <file.ogg>");
    let audio = std::fs::read(&path).expect("read audio");
    println!("file: {path} ({} bytes)", audio.len());

    let store = dimmy_lib::keystore::KeyStore::new();
    let key = store
        .load_key(KeyringScope::Stt(Provider::Deepgram), false)
        .or_else(|| store.load_key(KeyringScope::Llm(Provider::Deepgram), false))
        .unwrap_or_default();
    if key.is_empty() {
        println!("no Deepgram key stored — cannot probe");
        return;
    }

    let url = "https://api.deepgram.com/v1/listen\
               ?model=nova-3&diarize=true&punctuate=true&utterances=true&detect_language=true";
    let t0 = std::time::Instant::now();
    let resp = reqwest::Client::new()
        .post(url)
        .header("Authorization", format!("Token {key}"))
        .header("Content-Type", "audio/ogg")
        .body(audio)
        .send()
        .await
        .expect("send");

    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        println!(
            "HTTP {status}: {}",
            body.chars().take(300).collect::<String>()
        );
        return;
    }
    let v: serde_json::Value = serde_json::from_str(&body).expect("json");
    println!("transcribed in {:.1}s", t0.elapsed().as_secs_f32());

    let utts = v["results"]["utterances"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if utts.is_empty() {
        println!("no utterances returned");
        return;
    }

    // How many distinct speakers, and how much each one talks — the numbers
    // that say whether the split is plausible or the model just carved noise.
    let mut per_speaker: std::collections::BTreeMap<i64, (f64, usize)> = Default::default();
    for u in &utts {
        let sp = u["speaker"].as_i64().unwrap_or(-1);
        let dur = u["end"].as_f64().unwrap_or(0.0) - u["start"].as_f64().unwrap_or(0.0);
        let e = per_speaker.entry(sp).or_insert((0.0, 0));
        e.0 += dur;
        e.1 += 1;
    }
    println!(
        "\nspeakers: {}  |  utterances: {}",
        per_speaker.len(),
        utts.len()
    );
    for (sp, (secs, n)) in &per_speaker {
        println!("  speaker {sp}: {secs:6.1}s over {n} turns");
    }

    // Print every turn of the MINORITY speakers: an over-split shows up as a
    // "speaker" made entirely of short fragments that read like the main one.
    let main_speaker = per_speaker
        .iter()
        .max_by(|a, b| a.1 .0.total_cmp(&b.1 .0))
        .map(|(k, _)| *k);
    for (sp, _) in per_speaker
        .iter()
        .filter(|(k, _)| Some(**k) != main_speaker)
    {
        println!("\n--- every turn of minority speaker S{sp} ---");
        for u in utts.iter().filter(|u| u["speaker"].as_i64() == Some(*sp)) {
            println!(
                "[{:>6.1}s {:.1}s] {}",
                u["start"].as_f64().unwrap_or(0.0),
                u["end"].as_f64().unwrap_or(0.0) - u["start"].as_f64().unwrap_or(0.0),
                u["transcript"].as_str().unwrap_or("")
            );
        }
    }

    println!("\n--- first 25 turns ---");
    for u in utts.iter().take(25) {
        println!(
            "[{:>6.1}s] S{}: {}",
            u["start"].as_f64().unwrap_or(0.0),
            u["speaker"].as_i64().unwrap_or(-1),
            u["transcript"].as_str().unwrap_or("")
        );
    }

    // Short turns are where diarization is least reliable — an embedding from
    // under a second of speech is close to a coin flip.
    let short = utts
        .iter()
        .filter(|u| u["end"].as_f64().unwrap_or(0.0) - u["start"].as_f64().unwrap_or(0.0) < 1.0)
        .count();
    println!(
        "\nturns under 1s: {short}/{} ({:.0}%) — the fragile ones",
        utts.len(),
        100.0 * short as f32 / utts.len() as f32
    );
}
