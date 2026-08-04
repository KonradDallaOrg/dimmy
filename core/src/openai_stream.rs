//! Realtime streaming dictation over OpenAI's Realtime WebSocket.
//!
//! Sibling of [`crate::deepgram_stream`]. Both keep one socket open for the
//! life of a dictation, push mic audio as the cpal callback produces it, and
//! report interim + stable text through the SAME `(delta, cumulative,
//! is_final)` contract, so the host pipeline (`stt_chunk` event → live caption
//! → cursor injection) is engine-agnostic and needed no change to gain this
//! second engine.
//!
//! Where the two protocols differ:
//!
//! | | Deepgram | OpenAI |
//! |---|---|---|
//! | config | query params on the URL | a `session.update` JSON frame after connect |
//! | audio | raw binary frames | base64 inside `input_audio_buffer.append` |
//! | interim | `is_final:false` results | `…input_audio_transcription.delta` |
//! | stable | `is_final:true` results | `…input_audio_transcription.completed` |
//! | biasing | `keyterm` params | `keywords` array + free-form `prompt` |
//!
//! Turn segmentation is left to OpenAI's server VAD: it decides where an
//! utterance ends and emits one `completed` per turn, which is exactly the
//! granularity the host wants to inject at the cursor.
//!
//! TLS note: same as the Deepgram streamer — `tokio-tungstenite` on
//! `native-tls` (schannel), never rustls. A rustls WS client would reintroduce
//! the 0xc0000409 crash in the Velopack/WinAppSDK load path (see the sentry
//! dep comment in Cargo.toml).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

/// How often the sender loop drains newly-captured audio to the socket.
/// Matches the Deepgram streamer so both engines feel identical to the user.
const SEND_INTERVAL: Duration = Duration::from_millis(100);

/// How long, after `stop()`, to wait for OpenAI to flush the trailing
/// turn before giving up with whatever has arrived. Generous because the
/// server VAD has to close the final turn first (see
/// [`TRAILING_SILENCE_MS`]) and only then transcribes it.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(6);

/// Minimum audio in a turn before a pause is allowed to close it. The API
/// rejects a commit carrying under 100 ms; this leaves margin and stops a
/// hesitant speaker from producing a turn per syllable.
const MIN_COMMIT_MS: usize = 800;

/// Hard ceiling on turn length. Reached only when no pause is detected — a
/// noisy room, or someone who does not stop for breath. Bounds how long text
/// can lag behind speech.
const MAX_COMMIT_MS: usize = 8_000;

/// RMS below which a just-captured window counts as a pause.
///
/// `preprocess::ENERGY_FLOOR` (0.015) is the wrong constant here: it sits at
/// -36 dBFS, which is ABOVE the measured median speech level of this
/// project's own captures (0.019), so it would read most speech as silence.
/// -48 dBFS sits under quiet speech and above room tone.
const PAUSE_RMS: f32 = 0.004;

/// Commit when the speaker pauses, not on a timer.
///
/// A transcription session runs with `turn_detection` null (the live model
/// rejects anything else), so the server performs no voice activity detection
/// and never closes a turn by itself: the client owns the boundaries, and
/// without a commit the server simply holds the audio.
///
/// Committing on a fixed 2 s clock cut words in half — "tempo reale" came
/// back as "Temporeale", "mi incolli" as "e mail con lì" (observed
/// 2026-08-05). Each turn is also transcribed in isolation, so a boundary
/// inside a phrase costs the model the context that would have disambiguated
/// it. Cutting where the speaker already stopped avoids both.
fn is_pause(samples: &[f32]) -> bool {
    if samples.is_empty() {
        return false;
    }
    let sum: f64 = samples
        .iter()
        .map(|&s| {
            let s = if s.is_finite() { s as f64 } else { 0.0 };
            s * s
        })
        .sum();
    ((sum / samples.len() as f64).sqrt() as f32) < PAUSE_RMS
}

/// Latency setting for the live model: `minimal` | `low` | `medium` | `high`
/// | `xhigh`. Lower emits partial text sooner, higher gives the model room to
/// revise before committing. Dictation shows words as they land, so it wants
/// the fast end without going to the noisiest extreme.
const TRANSCRIPTION_DELAY: &str = "low";

/// The ONLY input rate the Realtime API accepts. Not a minimum, not a
/// suggestion — the server rejects `session.update` on both sides of it, and
/// a rejected session accepts no audio at all, which then surfaces later and
/// misleadingly as "buffer only has 0.00ms of audio":
///
/// > 16000 → integer below minimum value. Expected a value >= 24000
/// > 48000 → integer above maximum value. Expected a value <= 24000
///
/// Both observed against the live endpoint 2026-08-04. This is why the rate
/// is a hard constant rather than "whatever capture produces": every other
/// STT route here wants 16 kHz, so 24 kHz is the odd one out and must not be
/// quietly "improved" to match its neighbours.
const REQUIRED_INPUT_RATE: u32 = 24_000;

/// Default realtime endpoint. `intent=transcription` selects a
/// transcription-only session (no model responses, no audio out).
const DEFAULT_WS_URL: &str = "wss://api.openai.com/v1/realtime?intent=transcription";

/// Callback shape, identical to [`crate::chunked_stt::ChunkCallback`] and
/// [`crate::deepgram_stream::StreamCallback`] so the FFI layer fans every
/// engine out through the same `stt_chunk` event.
pub type StreamCallback = dyn Fn(&str, &str, bool) + Send + Sync + 'static;

pub struct OpenAiStreamer {
    cancel: Arc<AtomicBool>,
    final_text: Arc<Mutex<String>>,
    handle: Option<JoinHandle<()>>,
}

impl OpenAiStreamer {
    /// Spawn the streaming worker. `audio_buffer` is the shared PCM buffer the
    /// cpal callback writes into and `device_sample_rate` the rate it is
    /// filled at; audio is resampled from there to [`REQUIRED_INPUT_RATE`] on
    /// the way out. `language` is an ISO code (empty = let the model decide),
    /// `keywords` are literal vocabulary terms and `prompt` is free-form
    /// context.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        audio_buffer: Arc<Mutex<Vec<f32>>>,
        device_sample_rate: u32,
        api_key: String,
        model: String,
        language: String,
        prompt: String,
        keywords: Vec<String>,
        on_chunk: Arc<StreamCallback>,
    ) -> Self {
        assert!(
            device_sample_rate >= REQUIRED_INPUT_RATE,
            "capture rate {} is below the {} Hz this API requires — it can only \
             be downsampled to, never up",
            device_sample_rate,
            REQUIRED_INPUT_RATE
        );
        assert!(!api_key.is_empty(), "OpenAI api_key must not be empty");

        let cancel = Arc::new(AtomicBool::new(false));
        let final_text = Arc::new(Mutex::new(String::new()));

        let cancel_w = cancel.clone();
        let final_w = final_text.clone();
        let handle = thread::Builder::new()
            .name("openai-stream".into())
            .spawn(move || {
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        crate::log(&format!("[oai-stream] runtime build failed: {e}"));
                        on_chunk("", "", true);
                        return;
                    }
                };
                let committed = Arc::new(Mutex::new(String::new()));
                let result = rt.block_on(run_stream(
                    audio_buffer,
                    device_sample_rate,
                    api_key,
                    model,
                    language,
                    prompt,
                    keywords,
                    cancel_w,
                    committed.clone(),
                    on_chunk.clone(),
                ));
                let final_txt = committed.lock().map(|s| s.clone()).unwrap_or_default();
                if let Err(e) = result {
                    crate::log(&format!("[oai-stream] stream error: {e}"));
                }
                // The single terminal `is_final` emit. Lives here, not in
                // run_stream, so it fires exactly once on both the clean and
                // the error/early-return paths.
                on_chunk("", &final_txt, true);
                if let Ok(mut s) = final_w.lock() {
                    *s = final_txt;
                }
            })
            .expect("spawn openai-stream thread");

        Self {
            cancel,
            final_text,
            handle: Some(handle),
        }
    }

    /// Signal the worker to flush the trailing audio, close the socket and
    /// exit. Joins and returns the final cumulative transcript.
    pub fn stop(mut self) -> String {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
        self.final_text
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_stream(
    audio_buffer: Arc<Mutex<Vec<f32>>>,
    device_sample_rate: u32,
    api_key: String,
    model: String,
    language: String,
    prompt: String,
    keywords: Vec<String>,
    cancel: Arc<AtomicBool>,
    committed: Arc<Mutex<String>>,
    on_chunk: Arc<StreamCallback>,
) -> Result<(), String> {
    let mut request = DEFAULT_WS_URL
        .into_client_request()
        .map_err(|e| format!("bad ws url: {e}"))?;
    {
        let headers = request.headers_mut();
        headers.insert(
            "Authorization",
            HeaderValue::from_str(&format!("Bearer {api_key}"))
                .map_err(|e| format!("bad auth header: {e}"))?,
        );
        // NO `OpenAI-Beta: realtime=v1` header. Sending it selects the retired
        // Beta surface and the server closes the socket immediately with
        // "The Realtime Beta API is no longer supported. Please use
        // /v1/realtime for the GA API." Most tutorials and answers still show
        // that header — it was mandatory until the API went GA. Observed
        // against the live endpoint 2026-08-04.
    }

    let (ws, _resp) = connect_async(request)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    crate::log("[oai-stream] websocket connected");
    let (mut write, mut read) = ws.split();

    // Configure the session before any audio: the server needs to know the
    // wire format, which model to run and what context to bias with.
    let session =
        compose_session_update(&model, &language, &prompt, &keywords, REQUIRED_INPUT_RATE);
    write
        .send(Message::Text(session))
        .await
        .map_err(|e| format!("session.update failed: {e}"))?;

    let committed_r = committed.clone();
    let on_chunk_r = on_chunk.clone();
    let reader = tokio::spawn(async move {
        // Text of the turn currently being spoken, replaced as deltas arrive.
        let mut partial = String::new();
        // Frame types we don't act on, logged ONCE each. Without this a
        // protocol change is invisible: the socket connects, no error is
        // raised, and every transcription frame is silently dropped because
        // it arrived under a name we don't match. Only the `type` field is
        // logged — never payload text, which is user speech.
        let mut unseen: std::collections::HashSet<String> = std::collections::HashSet::new();
        // Counters for the handled events. Without these the log only shows
        // what we DIDN'T understand, so a session where transcription works
        // but is dropped downstream looks identical to one where the server
        // sent nothing at all.
        let (mut deltas, mut completions) = (0usize, 0usize);
        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(txt)) => match parse_oai_message(&txt) {
                    Some(StreamEvent::Delta(d)) => {
                        deltas += 1;
                        if deltas == 1 {
                            crate::log("[oai-stream] transcript deltas flowing");
                        }
                        partial.push_str(&d);
                        let acc = match committed_r.lock() {
                            Ok(a) => a.clone(),
                            Err(p) => p.into_inner().clone(),
                        };
                        let preview = if acc.is_empty() {
                            partial.clone()
                        } else {
                            format!("{} {}", acc.trim_end(), partial)
                        };
                        // The delta goes out as an INJECTABLE delta, not just
                        // a caption update. The host injects at the cursor
                        // only when this field is non-empty, so sending ""
                        // here (as the Deepgram path does for its interim
                        // results) meant text streamed into the caption strip
                        // and never reached the document — the user watched a
                        // working transcription produce nothing where they
                        // were typing.
                        //
                        // Safe to inject because these deltas are additive:
                        // each carries newly available text, not a rewrite of
                        // what came before. The `completed` that closes the
                        // turn therefore must NOT inject again.
                        on_chunk_r(&d, &preview, false);
                    }
                    Some(StreamEvent::Completed(text)) => {
                        completions += 1;
                        partial.clear();
                        let text = text.trim().to_string();
                        if text.is_empty() {
                            continue;
                        }
                        let mut acc = match committed_r.lock() {
                            Ok(a) => a,
                            Err(p) => p.into_inner(),
                        };
                        if !acc.is_empty() && !acc.ends_with(' ') {
                            acc.push(' ');
                        }
                        acc.push_str(&text);
                        let cum = acc.clone();
                        drop(acc);
                        // Empty delta ON PURPOSE: every word of this turn has
                        // already been injected as it arrived. Re-sending the
                        // polished text here would paste the whole turn a
                        // second time. The authoritative version still lands
                        // in `committed`, which is what history stores.
                        on_chunk_r("", &cum, false);
                    }
                    Some(StreamEvent::Error(e)) => {
                        crate::log(&format!("[oai-stream] server error: {e}"));
                    }
                    None => {
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) {
                            if let Some(t) =
                                v.get("type").and_then(|t| t.as_str()).map(str::to_string)
                            {
                                if unseen.insert(t.clone()) {
                                    // Shape, not content, for the frames that
                                    // decide whether transcription is on and
                                    // where the text lives.
                                    if t == "session.updated" || t.starts_with("conversation.item")
                                    {
                                        crate::log(&format!(
                                            "[oai-stream] unhandled frame: {t} shape={}",
                                            describe_shape(&v)
                                        ));
                                    } else {
                                        crate::log(&format!("[oai-stream] unhandled frame: {t}"));
                                    }
                                }
                            }
                        }
                    }
                },
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    crate::log(&format!("[oai-stream] read error: {e}"));
                    break;
                }
                _ => {}
            }
        }
        // A turn whose deltas arrived but whose `completed` never did is still
        // real transcript. Dropping it made a session that had visibly
        // produced text return the empty string, which trips the host's
        // work-loss fallback and re-transcribes the whole recording in batch —
        // the user sees the live caption fill in and then the result arrive
        // seconds late from somewhere else.
        if !partial.trim().is_empty() {
            let mut acc = match committed_r.lock() {
                Ok(a) => a,
                Err(p) => p.into_inner(),
            };
            if !acc.is_empty() && !acc.ends_with(' ') {
                acc.push(' ');
            }
            acc.push_str(partial.trim());
        }
        crate::log(&format!(
            "[oai-stream] reader done: {deltas} delta(s), {completions} completion(s)"
        ));
    });

    // Sender loop: drain newly-captured audio to the socket until cancelled,
    // committing a turn every COMMIT_INTERVAL_MS.
    //
    // The commit is what produces text. With `turn_detection` null (which the
    // live model requires) the server runs no VAD of its own and will sit on
    // audio indefinitely: an 18 s dictation produced not one frame after
    // `session.updated`. The client owns turn boundaries here.
    let mut last_sent: usize = 0;
    let mut uncommitted_ms: usize = 0;
    let mut quiet_now = false;
    loop {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(SEND_INTERVAL).await;
        let slice = drain_new_samples(&audio_buffer, &mut last_sent);
        if !slice.is_empty() {
            uncommitted_ms += (slice.len() * 1000) / device_sample_rate as usize;
            quiet_now = is_pause(&slice);
            let frame = compose_audio_append(&slice, device_sample_rate);
            if let Err(e) = write.send(Message::Text(frame)).await {
                crate::log(&format!("[oai-stream] send error: {e}"));
                break;
            }
        }
        // Close the turn where the speaker already stopped; fall back to the
        // ceiling when no pause ever comes.
        let due = (quiet_now && uncommitted_ms >= MIN_COMMIT_MS) || uncommitted_ms >= MAX_COMMIT_MS;
        if due {
            if let Err(e) = write.send(Message::Text(commit_frame())).await {
                crate::log(&format!("[oai-stream] commit error: {e}"));
                break;
            }
            uncommitted_ms = 0;
            quiet_now = false;
        }
    }

    // Flush the residual tail, then commit it so the server finalises the
    // in-flight turn instead of discarding it with the socket.
    let tail = drain_new_samples(&audio_buffer, &mut last_sent);
    if !tail.is_empty() {
        let _ = write
            .send(Message::Text(compose_audio_append(
                &tail,
                device_sample_rate,
            )))
            .await;
    }
    // Close the final turn. Without this the tail of the dictation is audio
    // the server has received but never been told to transcribe.
    let _ = write.send(Message::Text(commit_frame())).await;

    // Bounded window for the trailing `completed` events. The single terminal
    // emit happens in the worker after this returns.
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, reader).await;
    Ok(())
}

/// Snapshot `buffer[*last_sent..]` and advance `*last_sent`. The lock is held
/// only for the copy so the cpal callback can keep writing.
fn drain_new_samples(buffer: &Arc<Mutex<Vec<f32>>>, last_sent: &mut usize) -> Vec<f32> {
    match buffer.lock() {
        Ok(b) => {
            let len = b.len();
            if len <= *last_sent {
                return Vec::new();
            }
            let slice = b[*last_sent..len].to_vec();
            *last_sent = len;
            slice
        }
        Err(_) => Vec::new(),
    }
}

/// Build one `input_audio_buffer.append` frame: resample to
/// [`REQUIRED_INPUT_RATE`], convert to PCM16 LE, base64 it. Reuses the
/// Deepgram streamer's PCM conversion so both engines clamp and NaN-guard
/// identically.
fn compose_audio_append(samples: &[f32], source_rate: u32) -> String {
    let pcm = crate::preprocess::downsample_to(samples, source_rate, REQUIRED_INPUT_RATE);
    let bytes = crate::deepgram_stream::pcm_f32_to_i16le(&pcm);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    serde_json::json!({
        "type": "input_audio_buffer.append",
        "audio": b64,
    })
    .to_string()
}

/// The frame that closes a turn and asks for its transcript.
fn commit_frame() -> String {
    "{\"type\":\"input_audio_buffer.commit\"}".to_string()
}

/// Build the `session.update` frame that configures a transcription session.
///
/// `keywords` carries literal terms (names, jargon) and `prompt` free-form
/// context — the two biasing channels this model family exposes separately,
/// unlike whisper where both have to be crammed into one prompt string.
/// Empty fields are omitted rather than sent blank, so an unconfigured
/// dictation gets the model's own defaults. Pure for unit testing.
pub fn compose_session_update(
    model: &str,
    language: &str,
    prompt: &str,
    keywords: &[String],
    sample_rate: u32,
) -> String {
    let mut transcription = serde_json::json!({
        "model": if model.trim().is_empty() { "gpt-live-transcribe" } else { model.trim() },
    });
    if !prompt.trim().is_empty() {
        transcription["prompt"] = serde_json::json!(prompt.trim());
    }
    let terms: Vec<&str> = keywords
        .iter()
        .map(|k| k.trim())
        .filter(|k| !k.is_empty())
        .collect();
    if !terms.is_empty() {
        transcription["keywords"] = serde_json::json!(terms);
    }
    if !language.trim().is_empty() {
        transcription["languages"] = serde_json::json!([language.trim()]);
    }
    // Latency/quality knob for the live model: lower delay emits partial text
    // sooner, higher delay lets the model revise before committing. `low` is
    // the dictation trade-off — the user is watching words appear.
    transcription["delay"] = serde_json::json!(TRANSCRIPTION_DELAY);
    serde_json::json!({
        "type": "session.update",
        "session": {
            "type": "transcription",
            "audio": {
                "input": {
                    "format": { "type": "audio/pcm", "rate": sample_rate },
                    "transcription": transcription,
                    // MUST be null. The live transcription model does its own
                    // segmentation and rejects the whole session.update with
                    // "Turn detection is not supported for this transcription
                    // model." — and a rejected update is silent: no
                    // `session.updated` arrives, the session keeps its
                    // defaults, and every turn comes back with a null
                    // transcript. Observed live 2026-08-04.
                    "turn_detection": serde_json::Value::Null,
                }
            }
        }
    })
    .to_string()
}

/// Render the SHAPE of a JSON value: keys and types, never values. Strings
/// collapse to `str(len)` and numbers to `num`, so a frame can be inspected
/// in the log without any of it being user speech.
///
/// Exists because two protocol mismatches in a row were invisible from the
/// outside — the socket connected, raised no error, and dropped every
/// transcript because it arrived under a key we did not read. Names alone
/// were not enough to find it; the shape is.
pub fn describe_shape(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(m) => {
            let inner: Vec<String> = m
                .iter()
                .map(|(k, val)| format!("{k}:{}", describe_shape(val)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        serde_json::Value::Array(a) => match a.first() {
            Some(first) => format!("[{}x{}]", a.len(), describe_shape(first)),
            None => "[]".to_string(),
        },
        serde_json::Value::String(s) => format!("str({})", s.chars().count()),
        serde_json::Value::Number(_) => "num".to_string(),
        serde_json::Value::Bool(_) => "bool".to_string(),
        serde_json::Value::Null => "null".to_string(),
    }
}

/// The subset of server events this engine acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    /// Incremental text for the turn in progress.
    Delta(String),
    /// A finished turn: stable text, safe to inject.
    Completed(String),
    /// Server-reported error (surfaced to the log, not to the transcript).
    Error(String),
}

/// Extract the actionable event from one Realtime server frame, or `None` for
/// the many session/lifecycle frames this engine ignores. Pure for testing.
pub fn parse_oai_message(json: &str) -> Option<StreamEvent> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    match v.get("type")?.as_str()? {
        "conversation.item.input_audio_transcription.delta" => {
            Some(StreamEvent::Delta(v.get("delta")?.as_str()?.to_string()))
        }
        "conversation.item.input_audio_transcription.completed" => Some(StreamEvent::Completed(
            v.get("transcript")?.as_str()?.to_string(),
        )),
        // The GA surface delivers the finished turn inside the conversation
        // item rather than as a standalone transcription event: the socket
        // emits added -> done, and `done` carries the text on
        // `item.content[].transcript`. Observed live 2026-08-04 — a session
        // that only listened for the transcription events above connected,
        // received audio, produced turns, and transcribed nothing.
        "conversation.item.done" => {
            let text = v
                .get("item")?
                .get("content")?
                .as_array()?
                .iter()
                .filter_map(|c| c.get("transcript").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join(" ");
            if text.trim().is_empty() {
                None
            } else {
                Some(StreamEvent::Completed(text))
            }
        }
        "error" => {
            // The payload nests the human-readable reason; fall back to the
            // whole object so an unexpected shape still reaches the log.
            let msg = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error")
                .to_string();
            Some(StreamEvent::Error(msg))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> serde_json::Value {
        serde_json::from_str(s).expect("frame must be valid JSON")
    }

    #[test]
    fn session_update_has_the_transcription_shape() {
        let s = compose_session_update("gpt-live-transcribe", "it", "", &[], 16_000);
        let v = parse(&s);
        assert_eq!(v["type"], "session.update");
        assert_eq!(v["session"]["type"], "transcription");
        let input = &v["session"]["audio"]["input"];
        assert_eq!(input["format"]["type"], "audio/pcm");
        assert_eq!(input["format"]["rate"], 16_000);
        assert_eq!(input["transcription"]["model"], "gpt-live-transcribe");
    }

    #[test]
    fn session_update_never_sends_turn_detection() {
        // Regression: `{"type":"server_vad"}` here is rejected with "Turn
        // detection is not supported for this transcription model", and the
        // rejection is SILENT — no session.updated, defaults retained, every
        // transcript null. The live model segments on its own.
        let v = parse(&compose_session_update("m", "it", "p", &[], 24_000));
        assert!(
            v["session"]["audio"]["input"]["turn_detection"].is_null(),
            "turn_detection must be null, got {}",
            v["session"]["audio"]["input"]["turn_detection"]
        );
    }

    #[test]
    fn session_update_sets_the_latency_knob() {
        let v = parse(&compose_session_update("m", "", "", &[], 24_000));
        assert_eq!(
            v["session"]["audio"]["input"]["transcription"]["delay"],
            "low"
        );
    }

    #[test]
    fn session_update_sends_language_as_a_list() {
        // The singular `language` spelling is the whisper contract and is
        // rejected here — this model family takes `languages[]`.
        let v = parse(&compose_session_update("m", "it", "", &[], 16_000));
        let langs = &v["session"]["audio"]["input"]["transcription"]["languages"];
        assert!(langs.is_array(), "languages must be a list, got {langs}");
        assert_eq!(langs[0], "it");
        assert!(
            v["session"]["audio"]["input"]["transcription"]
                .get("language")
                .is_none(),
            "must not send the singular whisper-style field"
        );
    }

    #[test]
    fn session_update_omits_empty_context_fields() {
        let v = parse(&compose_session_update("m", "", "", &[], 16_000));
        let t = &v["session"]["audio"]["input"]["transcription"];
        for f in ["prompt", "keywords", "languages"] {
            assert!(t.get(f).is_none(), "{f} must be omitted when empty");
        }
    }

    #[test]
    fn session_update_carries_prompt_and_keywords_separately() {
        let kw = vec![
            "Velopack".to_string(),
            "  ".to_string(),
            "Notion".to_string(),
        ];
        let v = parse(&compose_session_update(
            "m",
            "",
            " riunione tecnica ",
            &kw,
            16_000,
        ));
        let t = &v["session"]["audio"]["input"]["transcription"];
        assert_eq!(t["prompt"], "riunione tecnica", "prompt must be trimmed");
        assert_eq!(t["keywords"][0], "Velopack");
        assert_eq!(t["keywords"][1], "Notion", "blank terms must be dropped");
        assert_eq!(t["keywords"].as_array().map(|a| a.len()), Some(2));
    }

    #[test]
    fn session_update_defaults_the_model_when_unset() {
        let v = parse(&compose_session_update("  ", "", "", &[], 16_000));
        assert_eq!(
            v["session"]["audio"]["input"]["transcription"]["model"],
            "gpt-live-transcribe"
        );
    }

    #[test]
    fn audio_append_is_base64_pcm16() {
        // Source already at the required rate -> straight through, 2 bytes/sample.
        let samples = vec![0.0f32, 0.5, -0.5, 1.0];
        let v = parse(&compose_audio_append(&samples, REQUIRED_INPUT_RATE));
        assert_eq!(v["type"], "input_audio_buffer.append");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(v["audio"].as_str().expect("audio must be a string"))
            .expect("audio must be valid base64");
        assert_eq!(decoded.len(), samples.len() * 2);
        assert_eq!(i16::from_le_bytes([decoded[0], decoded[1]]), 0);
        assert_eq!(i16::from_le_bytes([decoded[2], decoded[3]]), 16383);
        assert_eq!(i16::from_le_bytes([decoded[6], decoded[7]]), 32767);
    }

    #[test]
    fn audio_append_resamples_capture_to_the_required_rate() {
        // 48 kHz capture -> 24 kHz on the wire. Sending 48 kHz is rejected
        // ("integer above maximum value. Expected a value <= 24000"), so the
        // 2:1 decimation here is load-bearing, not an optimisation.
        let samples = vec![0.1f32; 48_000]; // 1 s at 48 kHz
        let v = parse(&compose_audio_append(&samples, 48_000));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(v["audio"].as_str().unwrap())
            .unwrap();
        let produced = decoded.len() / 2;
        assert!(
            (produced as i64 - 24_000).abs() <= 1,
            "1 s of 48 kHz capture must become ~24 000 samples, got {produced}"
        );
    }

    #[test]
    fn session_update_declares_exactly_the_required_rate() {
        // Regression, both directions, both seen live 2026-08-04:
        //   16 000 -> "integer below minimum value. Expected >= 24000"
        //   48 000 -> "integer above maximum value. Expected <= 24000"
        // 24 kHz is a point, not a range, and a rejected session.update means
        // the socket silently accepts no audio at all.
        let v = parse(&compose_session_update(
            "m",
            "",
            "",
            &[],
            REQUIRED_INPUT_RATE,
        ));
        assert_eq!(
            v["session"]["audio"]["input"]["format"]["rate"]
                .as_u64()
                .expect("rate must be an integer"),
            24_000,
            "the Realtime API accepts 24 kHz and nothing else"
        );
    }

    #[test]
    fn parse_delta_and_completed() {
        assert_eq!(
            parse_oai_message(
                r#"{"type":"conversation.item.input_audio_transcription.delta","delta":"cia"}"#
            ),
            Some(StreamEvent::Delta("cia".into()))
        );
        assert_eq!(
            parse_oai_message(
                r#"{"type":"conversation.item.input_audio_transcription.completed","transcript":"ciao mondo"}"#
            ),
            Some(StreamEvent::Completed("ciao mondo".into()))
        );
    }

    #[test]
    fn parse_error_extracts_the_message() {
        let e = parse_oai_message(
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"bad model"}}"#,
        );
        assert_eq!(e, Some(StreamEvent::Error("bad model".into())));
    }

    #[test]
    fn parse_ignores_lifecycle_and_garbage_frames() {
        for frame in [
            r#"{"type":"session.created","session":{}}"#,
            r#"{"type":"session.updated","session":{}}"#,
            r#"{"type":"input_audio_buffer.speech_started"}"#,
            r#"{"type":"input_audio_buffer.committed"}"#,
            r#"{"type":"conversation.item.added","item":{"content":[]}}"#,
            "{}",
            "not json",
        ] {
            assert_eq!(parse_oai_message(frame), None, "frame: {frame}");
        }
    }

    #[test]
    fn parse_takes_the_transcript_out_of_conversation_item_done() {
        // The GA surface carries the finished turn here rather than in a
        // standalone transcription event. Missing this means a session that
        // connects, receives audio and yields nothing.
        let frame = r#"{"type":"conversation.item.done","item":{"id":"x","role":"user",
            "content":[{"type":"input_audio","transcript":"ciao mondo"}]}}"#;
        assert_eq!(
            parse_oai_message(frame),
            Some(StreamEvent::Completed("ciao mondo".into()))
        );
    }

    #[test]
    fn parse_ignores_a_done_item_with_no_text() {
        // An empty turn must not emit — it would push a blank segment at the
        // user's cursor.
        for frame in [
            r#"{"type":"conversation.item.done","item":{"content":[{"type":"input_audio"}]}}"#,
            r#"{"type":"conversation.item.done","item":{"content":[{"transcript":"  "}]}}"#,
            r#"{"type":"conversation.item.done","item":{}}"#,
        ] {
            assert_eq!(parse_oai_message(frame), None, "frame: {frame}");
        }
    }

    #[test]
    fn is_pause_separates_speech_from_room_tone() {
        // Levels taken from this project's own audio_debug captures: median
        // speech RMS 0.019, and the pauses between words sit far below it.
        let speech: Vec<f32> = (0..2400).map(|i| (i as f32 * 0.05).sin() * 0.06).collect();
        assert!(!is_pause(&speech), "speech must not read as a pause");

        let room_tone: Vec<f32> = (0..2400)
            .map(|i| (i as f32 * 0.37).sin() * 0.0015)
            .collect();
        assert!(is_pause(&room_tone), "room tone must read as a pause");

        assert!(is_pause(&vec![0.0f32; 2400]), "silence is a pause");
        assert!(
            !is_pause(&[]),
            "no audio is not a pause — nothing to commit"
        );
    }

    #[test]
    fn is_pause_survives_non_finite_samples() {
        // A NaN must not poison the RMS into deciding the speaker stopped
        // (or never stops) — it is coerced to silence like everywhere else.
        let mut s = vec![0.06f32; 2400];
        s[0] = f32::NAN;
        s[1] = f32::INFINITY;
        assert!(!is_pause(&s));
    }

    #[test]
    fn drain_new_samples_advances_cursor() {
        let buf = Arc::new(Mutex::new(vec![1.0f32, 2.0, 3.0]));
        let mut cursor = 0usize;
        assert_eq!(drain_new_samples(&buf, &mut cursor), vec![1.0, 2.0, 3.0]);
        assert_eq!(cursor, 3);
        assert!(drain_new_samples(&buf, &mut cursor).is_empty());
        buf.lock().unwrap().extend_from_slice(&[4.0, 5.0]);
        assert_eq!(drain_new_samples(&buf, &mut cursor), vec![4.0, 5.0]);
        assert_eq!(cursor, 5);
    }
}
