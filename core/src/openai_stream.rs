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
/// `completed` events before giving up with whatever has arrived.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// Sample rate we declare and send. The Realtime API takes the rate as an
/// explicit field, and 16 kHz is what the rest of the pipeline already
/// produces (`downsample_to_16k`), so there is no reason to ship 50 % more
/// bytes at 24 kHz for a model that resamples internally anyway.
const STREAM_SAMPLE_RATE: u32 = 16_000;

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
    /// filled at (NOT [`STREAM_SAMPLE_RATE`] — we downsample on the way out).
    /// `language` is an ISO code (empty = let the model decide), `keywords`
    /// are literal vocabulary terms and `prompt` is free-form context.
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
            device_sample_rate > 0,
            "device_sample_rate must be positive"
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
        headers.insert("OpenAI-Beta", HeaderValue::from_static("realtime=v1"));
    }

    let (ws, _resp) = connect_async(request)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    crate::log("[oai-stream] websocket connected");
    let (mut write, mut read) = ws.split();

    // Configure the session before any audio: the server needs to know the
    // wire format, which model to run and what context to bias with.
    let session = compose_session_update(&model, &language, &prompt, &keywords, STREAM_SAMPLE_RATE);
    write
        .send(Message::Text(session))
        .await
        .map_err(|e| format!("session.update failed: {e}"))?;

    let committed_r = committed.clone();
    let on_chunk_r = on_chunk.clone();
    let reader = tokio::spawn(async move {
        // Text of the turn currently being spoken, replaced as deltas arrive.
        let mut partial = String::new();
        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(txt)) => match parse_oai_message(&txt) {
                    Some(StreamEvent::Delta(d)) => {
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
                        on_chunk_r("", &preview, false);
                    }
                    Some(StreamEvent::Completed(text)) => {
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
                        // delta carries the stable turn -> injectable at the cursor.
                        on_chunk_r(&text, &cum, false);
                    }
                    Some(StreamEvent::Error(e)) => {
                        crate::log(&format!("[oai-stream] server error: {e}"));
                    }
                    None => {}
                },
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    crate::log(&format!("[oai-stream] read error: {e}"));
                    break;
                }
                _ => {}
            }
        }
    });

    // Sender loop: drain newly-captured audio to the socket until cancelled.
    let mut last_sent: usize = 0;
    loop {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(SEND_INTERVAL).await;
        let slice = drain_new_samples(&audio_buffer, &mut last_sent);
        if !slice.is_empty() {
            let frame = compose_audio_append(&slice, device_sample_rate);
            if let Err(e) = write.send(Message::Text(frame)).await {
                crate::log(&format!("[oai-stream] send error: {e}"));
                break;
            }
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
    let _ = write
        .send(Message::Text(
            "{\"type\":\"input_audio_buffer.commit\"}".to_string(),
        ))
        .await;

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

/// Build one `input_audio_buffer.append` frame: downsample to the declared
/// rate, convert to PCM16 LE, base64 it. Reuses the Deepgram streamer's
/// PCM conversion so both engines clamp and NaN-guard identically.
fn compose_audio_append(samples: &[f32], source_rate: u32) -> String {
    let pcm = if source_rate == STREAM_SAMPLE_RATE {
        samples.to_vec()
    } else {
        crate::preprocess::downsample_to_16k(samples, source_rate)
    };
    let bytes = crate::deepgram_stream::pcm_f32_to_i16le(&pcm);
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    serde_json::json!({
        "type": "input_audio_buffer.append",
        "audio": b64,
    })
    .to_string()
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
    serde_json::json!({
        "type": "session.update",
        "session": {
            "type": "transcription",
            "audio": {
                "input": {
                    "format": { "type": "audio/pcm", "rate": sample_rate },
                    "transcription": transcription,
                    // Server-side VAD segments the turns for us; one
                    // `completed` per utterance is exactly the granularity
                    // the host injects at the cursor.
                    "turn_detection": { "type": "server_vad" },
                }
            }
        }
    })
    .to_string()
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
        assert_eq!(input["turn_detection"]["type"], "server_vad");
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
        // 16 kHz in, 16 kHz declared -> no resample, 2 bytes per sample.
        let samples = vec![0.0f32, 0.5, -0.5, 1.0];
        let v = parse(&compose_audio_append(&samples, 16_000));
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
    fn audio_append_downsamples_when_the_device_rate_differs() {
        let samples = vec![0.1f32; 48_000]; // 1 s at 48 kHz
        let v = parse(&compose_audio_append(&samples, 48_000));
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(v["audio"].as_str().unwrap())
            .unwrap();
        // 1 s at 16 kHz = 16 000 samples = 32 000 bytes, give or take the
        // resampler's edge handling.
        let produced = decoded.len() / 2;
        assert!(
            (15_000..=17_000).contains(&produced),
            "expected ~16 000 samples at 16 kHz, got {produced}"
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
            "{}",
            "not json",
        ] {
            assert_eq!(parse_oai_message(frame), None, "frame: {frame}");
        }
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
