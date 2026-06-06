//! Realtime streaming dictation over Deepgram's WebSocket STT.
//!
//! This is the true-streaming twin of [`crate::chunked_stt::ChunkedTranscriber`].
//! Where the chunked path slices the shared PCM buffer into overlapping
//! windows and re-transcribes each one through a local model, this module
//! keeps a single persistent WebSocket open to Deepgram, pushes the audio
//! the cpal callback captures as it arrives (PCM16 little-endian, 16 kHz
//! mono), and receives interim + final results word-by-word.
//!
//! It deliberately reuses the *exact same* callback contract as the chunked
//! transcriber — `(delta, cumulative, is_final)` — so the whole host UI
//! pipeline (the `stt_chunk` event -> live caption strip -> segment inject)
//! works without any change to the contract:
//!
//! - `delta`: text that has just become STABLE (a Deepgram `is_final`
//!   segment). Safe for the host to inject at the cursor. Empty for
//!   interim (still-changing) updates.
//! - `cumulative`: the best full text to render in the live caption right
//!   now (committed finals + the current interim tail).
//! - `is_final`: stream-level finished, emitted exactly ONCE at the end
//!   after `stop()` drains the trailing finals. Same semantics as the
//!   chunked drain, so existing consumers are unaffected.
//!
//! TLS note: we ride `tokio-tungstenite` with the `native-tls` feature on
//! purpose. The cdylib already standardised on native-tls (schannel on
//! Windows) because rustls 0.23+ panics in the Velopack/WinAppSDK load path
//! without `CryptoProvider::install_default()` (see the sentry dep comment
//! in Cargo.toml and project_sentry_dll_load_crash_2026_04_26). Adding a
//! rustls WS client would reintroduce that 0xc0000409 crash class.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

/// How often the sender loop drains newly-captured audio out to the socket.
/// 100 ms keeps the wire chunk in Deepgram's happy range (20-250 ms) while
/// not waking the runtime needlessly. Same cadence as the chunked poll.
const SEND_INTERVAL: Duration = Duration::from_millis(100);

/// How long, after `stop()`, we wait for Deepgram to flush its trailing
/// final results before giving up and emitting whatever we have. The tail
/// of a sentence after `CloseStream` is typically delivered in well under
/// a second; 3 s is a generous ceiling so a slow network doesn't truncate.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// Callback shape, identical to [`crate::chunked_stt::ChunkCallback`] so the
/// FFI layer fans both engines out through the same `stt_chunk` event.
pub type StreamCallback = dyn Fn(&str, &str, bool) + Send + Sync + 'static;

pub struct DeepgramStreamer {
    cancel: Arc<AtomicBool>,
    final_text: Arc<Mutex<String>>,
    handle: Option<JoinHandle<()>>,
}

impl DeepgramStreamer {
    /// Spawn the streaming worker. `audio_buffer` is the same shared PCM
    /// buffer the cpal callback writes into; `device_sample_rate` is the
    /// rate it is filled at (cpal canonical rate, NOT 16 kHz). `api_key`
    /// is the Deepgram secret; `language` is an ISO code (empty = Deepgram
    /// multilingual auto). `base_url` is the wss endpoint base (callers can
    /// override the model via query params).
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        audio_buffer: Arc<Mutex<Vec<f32>>>,
        device_sample_rate: u32,
        api_key: String,
        language: String,
        base_url: String,
        keyterms: String,
        on_chunk: Arc<StreamCallback>,
    ) -> Self {
        assert!(
            device_sample_rate > 0,
            "device_sample_rate must be positive"
        );
        assert!(!api_key.is_empty(), "Deepgram api_key must not be empty");

        let cancel = Arc::new(AtomicBool::new(false));
        let final_text = Arc::new(Mutex::new(String::new()));

        let cancel_w = cancel.clone();
        let final_w = final_text.clone();
        let handle = thread::Builder::new()
            .name("deepgram-stream".into())
            .spawn(move || {
                // Each worker owns a single-threaded tokio runtime. The
                // process-wide `tokio::runtime` is created per call elsewhere
                // (one-shot batch transcribe), so we follow the same pattern
                // rather than reaching for a shared global runtime.
                let rt = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(rt) => rt,
                    Err(e) => {
                        crate::log(&format!("[dg-stream] runtime build failed: {e}"));
                        on_chunk("", "", true);
                        return;
                    }
                };
                let committed = Arc::new(Mutex::new(String::new()));
                let result = rt.block_on(run_stream(
                    audio_buffer,
                    device_sample_rate,
                    api_key,
                    language,
                    base_url,
                    keyterms,
                    cancel_w,
                    committed.clone(),
                    on_chunk.clone(),
                ));
                let final_txt = committed.lock().map(|s| s.clone()).unwrap_or_default();
                if let Err(e) = result {
                    crate::log(&format!("[dg-stream] stream error: {e}"));
                }
                // The single terminal `is_final` emit, mirroring the chunked
                // drain. Lives here (not in run_stream) so it fires on both
                // the clean and the error/early-return paths exactly once.
                on_chunk("", &final_txt, true);
                if let Ok(mut s) = final_w.lock() {
                    *s = final_txt;
                }
            })
            .expect("spawn deepgram-stream thread");

        Self {
            cancel,
            final_text,
            handle: Some(handle),
        }
    }

    /// Signal the worker to flush the trailing audio, close the socket and
    /// exit. Joins and returns the final cumulative transcript. Bounded by
    /// [`DRAIN_TIMEOUT`].
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
    language: String,
    base_url: String,
    keyterms: String,
    cancel: Arc<AtomicBool>,
    committed: Arc<Mutex<String>>,
    on_chunk: Arc<StreamCallback>,
) -> Result<(), String> {
    let url = compose_deepgram_ws_url(&base_url, &language, 16_000, &keyterms);
    let mut request = url
        .into_client_request()
        .map_err(|e| format!("bad ws url: {e}"))?;
    let auth = HeaderValue::from_str(&format!("Token {api_key}"))
        .map_err(|e| format!("bad auth header: {e}"))?;
    request.headers_mut().insert("Authorization", auth);

    let (ws, _resp) = connect_async(request)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;
    crate::log("[dg-stream] websocket connected");
    let (mut write, mut read) = ws.split();

    // Reader task: parse Deepgram results, accumulate committed finals,
    // emit interim previews + stable deltas through the shared callback.
    let committed_r = committed.clone();
    let on_chunk_r = on_chunk.clone();
    let reader = tokio::spawn(async move {
        while let Some(msg) = read.next().await {
            match msg {
                Ok(Message::Text(txt)) => {
                    if let Some((transcript, is_final)) = parse_dg_message(&txt) {
                        let transcript = transcript.trim();
                        if transcript.is_empty() {
                            continue;
                        }
                        let mut acc = match committed_r.lock() {
                            Ok(a) => a,
                            Err(p) => p.into_inner(),
                        };
                        if is_final {
                            if !acc.is_empty() && !acc.ends_with(' ') {
                                acc.push(' ');
                            }
                            acc.push_str(transcript);
                            let cum = acc.clone();
                            drop(acc);
                            // delta carries the stable segment -> injectable.
                            on_chunk_r(transcript, &cum, false);
                        } else {
                            let preview = if acc.is_empty() {
                                transcript.to_string()
                            } else {
                                format!("{} {}", acc.trim_end(), transcript)
                            };
                            drop(acc);
                            on_chunk_r("", &preview, false);
                        }
                    }
                }
                Ok(Message::Close(_)) => break,
                Err(e) => {
                    crate::log(&format!("[dg-stream] read error: {e}"));
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
            let pcm_16k = downsample_if_needed(&slice, device_sample_rate);
            let bytes = pcm_f32_to_i16le(&pcm_16k);
            if let Err(e) = write.send(Message::Binary(bytes)).await {
                crate::log(&format!("[dg-stream] send error: {e}"));
                break;
            }
        }
    }

    // Flush the residual tail captured after the last tick, then tell
    // Deepgram to finalise and close.
    let tail = drain_new_samples(&audio_buffer, &mut last_sent);
    if !tail.is_empty() {
        let pcm_16k = downsample_if_needed(&tail, device_sample_rate);
        let _ = write
            .send(Message::Binary(pcm_f32_to_i16le(&pcm_16k)))
            .await;
    }
    let _ = write
        .send(Message::Text("{\"type\":\"CloseStream\"}".to_string()))
        .await;

    // Give the reader a bounded window to deliver the trailing finals.
    // The single terminal `is_final` emit happens in the worker thread
    // after this returns (so it fires on both the clean and error paths),
    // so we don't emit it here.
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

fn downsample_if_needed(samples: &[f32], source_rate: u32) -> Vec<f32> {
    if source_rate == 16_000 {
        samples.to_vec()
    } else {
        crate::preprocess::downsample_to_16k(samples, source_rate)
    }
}

/// Convert mono f32 samples in `[-1.0, 1.0]` to little-endian PCM16 bytes.
/// NaN/Inf are coerced to silence and everything is clamped, so a corrupt
/// frame can never overflow the i16 cast or ship garbage to Deepgram.
pub fn pcm_f32_to_i16le(samples: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        let clamped = if s.is_finite() {
            s.clamp(-1.0, 1.0)
        } else {
            0.0
        };
        let v = (clamped * 32767.0) as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    debug_assert_eq!(out.len(), samples.len() * 2);
    out
}

/// Build the Deepgram realtime WS URL. `base_url` may be a bare host
/// (`wss://api.deepgram.com/v1/listen`) or already carry a `?model=...`
/// query — we append params with the right separator either way. `language`
/// empty selects Deepgram's multilingual mode. `keyterms` is a comma-list of
/// vocabulary biasing terms (Nova-3 `keyterm`), same convention as the batch
/// path's [`crate::transcribe::compose_deepgram_url`]. Pure for unit testing.
pub fn compose_deepgram_ws_url(
    base_url: &str,
    language: &str,
    sample_rate: u32,
    keyterms: &str,
) -> String {
    let base = if base_url.trim().is_empty() {
        "wss://api.deepgram.com/v1/listen"
    } else {
        base_url.trim()
    };
    let mut url = base.to_string();
    if !url.contains("model=") {
        let sep = if url.contains('?') { "&" } else { "?" };
        url = format!("{url}{sep}model=nova-3");
    }
    url = format!(
        "{url}&encoding=linear16&sample_rate={sample_rate}&channels=1&interim_results=true&punctuate=true&smart_format=true"
    );
    if language.trim().is_empty() {
        url = format!("{url}&language=multi");
    } else {
        url = format!("{url}&language={}", language.trim());
    }
    for term in keyterms
        .split(',')
        .map(|t| t.trim())
        .filter(|t| !t.is_empty())
    {
        let escaped = term
            .replace('%', "%25")
            .replace('&', "%26")
            .replace(' ', "%20");
        url = format!("{url}&keyterm={escaped}");
    }
    url
}

/// Extract `(transcript, is_final)` from one Deepgram realtime message.
/// Returns `None` for non-result frames (Metadata, UtteranceEnd, anything
/// without a transcript). Pure for unit testing.
pub fn parse_dg_message(json: &str) -> Option<(String, bool)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let transcript = v
        .get("channel")?
        .get("alternatives")?
        .get(0)?
        .get("transcript")?
        .as_str()?
        .to_string();
    let is_final = v.get("is_final").and_then(|b| b.as_bool()).unwrap_or(false);
    Some((transcript, is_final))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm16_clamps_and_handles_non_finite() {
        let samples = [0.0f32, 1.0, -1.0, 2.0, -2.0, f32::NAN, f32::INFINITY];
        let bytes = pcm_f32_to_i16le(&samples);
        assert_eq!(bytes.len(), samples.len() * 2);
        // 0.0 -> 0
        assert_eq!(i16::from_le_bytes([bytes[0], bytes[1]]), 0);
        // 1.0 -> 32767
        assert_eq!(i16::from_le_bytes([bytes[2], bytes[3]]), 32767);
        // -1.0 -> -32767
        assert_eq!(i16::from_le_bytes([bytes[4], bytes[5]]), -32767);
        // 2.0 clamps to 1.0 -> 32767
        assert_eq!(i16::from_le_bytes([bytes[6], bytes[7]]), 32767);
        // -2.0 clamps to -1.0 -> -32767
        assert_eq!(i16::from_le_bytes([bytes[8], bytes[9]]), -32767);
        // NaN -> 0, Inf -> 0
        assert_eq!(i16::from_le_bytes([bytes[10], bytes[11]]), 0);
        assert_eq!(i16::from_le_bytes([bytes[12], bytes[13]]), 0);
    }

    #[test]
    fn pcm16_empty_is_empty() {
        assert!(pcm_f32_to_i16le(&[]).is_empty());
    }

    #[test]
    fn ws_url_defaults_model_and_params() {
        let url = compose_deepgram_ws_url("wss://api.deepgram.com/v1/listen", "", 16_000, "");
        assert!(url.contains("model=nova-3"));
        assert!(url.contains("encoding=linear16"));
        assert!(url.contains("sample_rate=16000"));
        assert!(url.contains("interim_results=true"));
        assert!(url.contains("language=multi"));
        assert!(!url.contains("keyterm="));
    }

    #[test]
    fn ws_url_honours_explicit_model_and_language() {
        let url = compose_deepgram_ws_url(
            "wss://api.deepgram.com/v1/listen?model=nova-2",
            "it",
            16_000,
            "",
        );
        assert!(url.contains("model=nova-2"));
        assert!(!url.contains("model=nova-3"));
        assert!(url.contains("language=it"));
        assert!(!url.contains("language=multi"));
    }

    #[test]
    fn ws_url_appends_keyterms_escaped() {
        let url = compose_deepgram_ws_url(
            "wss://api.deepgram.com/v1/listen",
            "en",
            16_000,
            "Dimmy, Rust core",
        );
        assert!(url.contains("keyterm=Dimmy"));
        assert!(url.contains("keyterm=Rust%20core"));
    }

    #[test]
    fn parse_results_final() {
        let json = r#"{"type":"Results","channel":{"alternatives":[{"transcript":"hello world","confidence":0.98}]},"is_final":true,"speech_final":false}"#;
        let (t, f) = parse_dg_message(json).expect("should parse");
        assert_eq!(t, "hello world");
        assert!(f);
    }

    #[test]
    fn parse_results_interim() {
        let json = r#"{"type":"Results","channel":{"alternatives":[{"transcript":"hel"}]},"is_final":false}"#;
        let (t, f) = parse_dg_message(json).expect("should parse");
        assert_eq!(t, "hel");
        assert!(!f);
    }

    #[test]
    fn parse_metadata_returns_none() {
        let json = r#"{"type":"Metadata","request_id":"abc","duration":1.0}"#;
        assert!(parse_dg_message(json).is_none());
    }

    #[test]
    fn parse_utterance_end_returns_none() {
        let json = r#"{"type":"UtteranceEnd","last_word_end":3.2}"#;
        assert!(parse_dg_message(json).is_none());
    }

    #[test]
    fn parse_garbage_returns_none() {
        assert!(parse_dg_message("not json").is_none());
        assert!(parse_dg_message("{}").is_none());
    }

    #[test]
    fn drain_new_samples_advances_cursor() {
        let buf = Arc::new(Mutex::new(vec![1.0f32, 2.0, 3.0]));
        let mut cursor = 0usize;
        let first = drain_new_samples(&buf, &mut cursor);
        assert_eq!(first, vec![1.0, 2.0, 3.0]);
        assert_eq!(cursor, 3);
        // No new samples -> empty, cursor unchanged.
        let second = drain_new_samples(&buf, &mut cursor);
        assert!(second.is_empty());
        assert_eq!(cursor, 3);
        // Append more -> only the new tail is drained.
        buf.lock().unwrap().extend_from_slice(&[4.0, 5.0]);
        let third = drain_new_samples(&buf, &mut cursor);
        assert_eq!(third, vec![4.0, 5.0]);
        assert_eq!(cursor, 5);
    }
}
