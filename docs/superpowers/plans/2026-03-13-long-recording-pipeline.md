# Long Recording Pipeline — Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make Dimmy handle recordings of any duration (up to 30min hard cap) without hitting provider file size or duration limits, while preserving accurate single-pass transcription quality.

**Architecture:** On stop, instead of sending the entire buffer as one WAV, estimate the output size and compare against the provider's limit. If it fits, send as-is (zero change). If it doesn't, split into chunks sized to ~80% of the provider limit (NOT fixed 10s — fat chunks = more context = better transcription), find a silence boundary near the split point, transcribe each chunk sequentially, concatenate. Chunk streaming during recording (5-12s) is untouched — that's a different feature (live preview).

**Tech Stack:** Rust (Tauri backend), existing audio pipeline types (RawAudio → ProcessedAudio → WavPayload)

---

## How it works today

```
L'utente configura UN provider STT e UNA API key.
Non c'è multi-provider, non c'è fallback.

start_recording:
  cpal capture 48kHz → audio_buffer (Vec<f32>, cresce fino a 30min)

  if chunk_streaming ON:
    ogni 200ms: controlla buffer → split a silenzi (5-12s) → preprocess → STT → testo parziale nella pill
    Questo è "live preview" — l'utente vede cosa sta dicendo in tempo reale

stop_recording:
  prende TUTTO audio_buffer → preprocess → downsample 16kHz → UN WAV → STT API → testo finale
  Il testo finale SOVRASCRIVE i parziali dello streaming
  Poi: opzionalmente LLM post-processing → paste nella app attiva
```

## Il problema

| Durata | WAV 16kHz 16-bit | Groq/OpenAI (25MB) | Gemini inline (20MB) | Deepgram (2GB) |
|--------|------------------|--------------------|----------------------|----------------|
| 1 min  | 1.9 MB           | OK                 | OK                   | OK             |
| 5 min  | 9.4 MB           | OK                 | OK                   | OK             |
| 10 min | 18.8 MB          | OK                 | OK                   | OK             |
| 13 min | 24.4 MB          | AL LIMITE          | SFORA                | OK             |
| 25 min | 47 MB            | SFORA              | SFORA                | OK             |
| 30 min | 56 MB            | SFORA              | SFORA                | OK             |

Oggi per registrazioni >10-13min: **errore HTTP 413 dal provider. Trascrizione persa.**

## Design decisions

1. **Chunk size = f(provider limit)**, non fisso a 10s. Un chunk Groq/OpenAI = ~10min di audio (~20MB, 80% del limite). Un chunk Gemini = ~8min (~16MB). Deepgram: zero chunking, manda tutto.
2. **Chunking solo al momento dello stop** — il chunk streaming live (5-12s) è una feature diversa e resta invariato.
3. **Zero cambiamenti per registrazioni corte** — se il WAV sta nel limite, passa dritto come oggi.
4. **Sequenziale, non parallelo** — i chunk vanno uno alla volta. Rate limit, ordine garantito, gestione errori semplice.
5. **UI non si blocca** — tutto è async. Per registrazioni chunked l'utente vede "Processing 2/3..." invece di una barra ferma.
6. **Concatenazione = join(" ")** — i chunk si spezzano ai silenzi, quindi unire con spazio è corretto.

## Flusso proposto

```
stop_recording:
  INTERO audio_buffer → preprocess → ProcessedAudio

  estimated_wav = estimate_wav_size(processed)    # senza encodare, solo calcolo
  limit = Provider::from_url(api_url).max_file_bytes()

  if estimated_wav <= limit:
    # Registrazione corta — percorso identico a oggi, ZERO cambiamenti
    downsample → WAV → STT API → risultato finale

  else:
    # Registrazione lunga — chunk per size
    max_chunk_samples = (limit * 0.8) / 2 * (sample_rate / 16000)
    # ↑ 80% del limite in byte → convertito in campioni al sample rate nativo
    # Per Groq (25MB): ~10 min per chunk
    # Per Gemini (20MB): ~8 min per chunk
    # Per Deepgram (2GB): non ci arriva mai, manda tutto intero

    chunks = split_at_silence(processed, max_chunk_samples)
    # Cerca un silenzio (RMS < 0.01, finestra 300ms) nell'ultimo 25% del chunk
    # Se non trova silenzio, taglia al max (meglio un taglio netto che un errore 413)

    results = []
    for i, chunk in chunks:
      emit("final-chunk-progress", {current: i+1, total: len(chunks)})
      downsample → WAV → STT API → testo
      results.push(testo)

    risultato_finale = results.join(" ")
```

### Esempio concreto: registrazione 25 minuti con Groq

```
25 min audio → preprocess → ~47MB WAV stimato
Limite Groq: 25MB → serve chunking
80% di 25MB = 20MB → ~10.4 min per chunk

split_at_silence trova:
  Chunk 1: 0:00 - 10:23  (silenzio trovato a 10:23)
  Chunk 2: 10:23 - 20:45  (silenzio trovato a 20:45)
  Chunk 3: 20:45 - 25:00  (residuo)

UI mostra: "Processing 1/3..." → "Processing 2/3..." → "Processing 3/3..." → testo finale
Tempo totale: ~15-20 sec (3 API call da ~5-7s ciascuna)
```

### Stesso audio, stesso utente, ma con Deepgram

```
25 min audio → preprocess → ~47MB WAV stimato
Limite Deepgram: 2GB → 47MB < 2GB → NESSUN chunking
Manda tutto intero come oggi.
Timeout scalato: 30s + 47MB = ~77s timeout
```

## Timeout

Oggi: 30s fisso per tutte le request. Per un chunk da 20MB il provider potrebbe impiegare 10-15s per processare.

Fix: `timeout = 30s base + 1s per MB`. Un chunk da 20MB = 50s timeout. Sicuro e non eccessivo.

## Cosa NON fa questo piano

- **Multi-provider / fallback**: non c'è. L'utente ha UN provider, tutto va lì.
- **Gemini File API**: non ora. Restiamo con inline (20MB). Se sfora, chunka. File API = feature futura per "modalità riunione".
- **Deepgram upload da 2GB**: il nostro hard cap è 30min = 56MB. Deepgram lo digerisce senza problemi. Non mandiamo mai 2GB.
- **Modalità riunione/call**: idea futura — registrazioni >30min, salvataggio su GDrive, trascrizione asincrona. Richiede: File API Gemini o Deepgram async, integrazione OAuth GDrive, UI dedicata. **Non in questo piano.**

---

## File Structure

| File | Action | Responsibility |
|------|--------|---------------|
| `src-tauri/src/provider.rs` | Modify | Add `max_file_bytes()` per provider |
| `src-tauri/src/audio.rs` | Modify | Add `estimate_wav_size()` e `split_at_silence()` su ProcessedAudio |
| `src-tauri/src/transcribe.rs` | Modify | Add `transcribe_chunked()` — stima, splitta se serve, trascrive, concatena |
| `src-tauri/src/lib.rs` | Modify | Update `stop_recording` per usare `transcribe_chunked` + progress events |
| `src/main.js` | Modify | Listener per `final-chunk-progress` |

---

## Chunk 1: Provider Limits

### Task 1: Add provider file size limits

**Files:**
- Modify: `src-tauri/src/provider.rs:18-67`

- [ ] **Step 1: Write failing tests for provider limits**

Add to existing `mod tests` in `provider.rs`:

```rust
#[test]
fn provider_file_limits() {
    assert_eq!(Provider::Groq.max_file_bytes(), 25 * 1024 * 1024);
    assert_eq!(Provider::OpenAI.max_file_bytes(), 25 * 1024 * 1024);
    assert_eq!(Provider::OpenRouter.max_file_bytes(), 25 * 1024 * 1024);
    assert_eq!(Provider::Deepgram.max_file_bytes(), 2 * 1024 * 1024 * 1024);
    assert_eq!(Provider::Gemini.max_file_bytes(), 20 * 1024 * 1024);
    assert_eq!(Provider::Anthropic.max_file_bytes(), 25 * 1024 * 1024);
    assert_eq!(Provider::Custom.max_file_bytes(), 25 * 1024 * 1024);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd /home/konrad/code/pai-voice/src-tauri && cargo test provider::tests::provider_file_limits -- --nocapture`
Expected: FAIL — `max_file_bytes` does not exist

- [ ] **Step 3: Implement max_file_bytes**

Add to `impl Provider` in `provider.rs`:

```rust
/// Maximum file size in bytes this provider accepts for STT upload.
pub fn max_file_bytes(&self) -> usize {
    match self {
        Self::Deepgram => 2 * 1024 * 1024 * 1024, // 2 GB
        Self::Gemini => 20 * 1024 * 1024,          // 20 MB (inline_data limit)
        _ => 25 * 1024 * 1024,                     // 25 MB (Groq, OpenAI, OpenRouter, etc.)
    }
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cd /home/konrad/code/pai-voice/src-tauri && cargo test provider::tests -- --nocapture`
Expected: all PASS

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/provider.rs
git commit -m "feat(provider): add max_file_bytes per provider for STT upload limits"
```

---

## Chunk 2: Audio Size Estimation & Splitting

### Task 2: Add estimate_wav_size and split_at_silence to ProcessedAudio

**Files:**
- Modify: `src-tauri/src/audio.rs:228-261`

- [ ] **Step 1: Write failing tests for WAV size estimation**

Add to `mod tests` in `audio.rs`:

```rust
#[test]
fn estimate_wav_size_at_16khz() {
    // 1 sec at 48kHz → 16000 samples at 16kHz → 32000 bytes + 44 header
    let audio = ProcessedAudio {
        samples: vec![0.0; 48000],
        sample_rate: 48000,
    };
    let estimated = audio.estimate_wav_size();
    assert!(
        (estimated as i64 - 32044).abs() < 100,
        "Expected ~32044, got {}",
        estimated
    );
}

#[test]
fn estimate_wav_size_30min() {
    let audio = ProcessedAudio {
        samples: vec![0.0; 48000 * 30 * 60],
        sample_rate: 48000,
    };
    let estimated = audio.estimate_wav_size();
    // ~57.6 MB
    assert!(
        estimated > 50_000_000 && estimated < 60_000_000,
        "30min should be ~57MB, got {}",
        estimated
    );
}
```

- [ ] **Step 2: Run tests to verify fail**

Run: `cd /home/konrad/code/pai-voice/src-tauri && cargo test audio::tests::estimate_wav_size -- --nocapture`
Expected: FAIL

- [ ] **Step 3: Implement estimate_wav_size**

Add to `impl ProcessedAudio` in `audio.rs`:

```rust
/// Estimate WAV file size after downsampling to 16kHz, without encoding.
/// Formula: (samples / downsample_ratio) * 2 bytes + 44 byte header.
pub fn estimate_wav_size(&self) -> usize {
    let ratio = self.sample_rate as f64 / 16000.0;
    let output_samples = (self.samples.len() as f64 / ratio).ceil() as usize;
    output_samples * 2 + 44
}
```

- [ ] **Step 4: Run tests to verify pass**

Run: `cd /home/konrad/code/pai-voice/src-tauri && cargo test audio::tests::estimate_wav_size -- --nocapture`
Expected: PASS

- [ ] **Step 5: Write failing tests for split_at_silence**

```rust
#[test]
fn split_short_audio_no_split() {
    let audio = ProcessedAudio {
        samples: vec![0.5; 48000 * 5], // 5 sec
        sample_rate: 48000,
    };
    let chunks = audio.split_at_silence(48000 * 600); // 600s max = way bigger
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].samples.len(), 48000 * 5);
}

#[test]
fn split_by_size_with_silence() {
    // 25s audio: 10s speech + 1s silence + 14s speech
    // max_chunk = 12s worth of samples → should split at the silence
    let sr: usize = 48000;
    let mut samples = vec![0.5f32; sr * 10];
    samples.extend(vec![0.0f32; sr]);
    samples.extend(vec![0.5f32; sr * 14]);
    let audio = ProcessedAudio {
        samples,
        sample_rate: sr as u32,
    };
    let chunks = audio.split_at_silence(sr * 12);
    assert_eq!(chunks.len(), 2, "Should split into 2 chunks at silence");
    assert!(chunks[0].samples.len() <= sr * 12);
    assert!(chunks[1].samples.len() <= sr * 15); // remainder
}

#[test]
fn split_force_at_max_no_silence() {
    // Continuous speech, no silence → must force-split at max
    let sr: usize = 48000;
    let audio = ProcessedAudio {
        samples: vec![0.5f32; sr * 25],
        sample_rate: sr as u32,
    };
    let chunks = audio.split_at_silence(sr * 10);
    assert!(chunks.len() >= 3, "25s / 10s max = at least 3 chunks");
    for (i, chunk) in chunks.iter().enumerate() {
        assert!(
            chunk.samples.len() <= sr * 10,
            "Chunk {} exceeds max: {}s",
            i,
            chunk.samples.len() / sr
        );
    }
}

#[test]
fn split_preserves_all_samples() {
    // Total samples in = total samples out
    let sr: usize = 48000;
    let total = sr * 20;
    let audio = ProcessedAudio {
        samples: vec![0.3f32; total],
        sample_rate: sr as u32,
    };
    let chunks = audio.split_at_silence(sr * 8);
    let reconstructed: usize = chunks.iter().map(|c| c.samples.len()).sum();
    assert_eq!(reconstructed, total, "Splitting must not lose samples");
}
```

- [ ] **Step 6: Run tests to verify fail**

Run: `cd /home/konrad/code/pai-voice/src-tauri && cargo test audio::tests::split -- --nocapture`
Expected: FAIL

- [ ] **Step 7: Implement split_at_silence**

Add to `impl ProcessedAudio` in `audio.rs`:

```rust
/// Split audio into chunks, each at most `max_chunk_samples` long.
/// Tries to split at silence boundaries (RMS < 0.01 for 300ms).
/// Searches backwards from the max boundary within the last 25% of the chunk.
/// If no silence found, force-splits at max (better a hard cut than a 413 error).
pub fn split_at_silence(self, max_chunk_samples: usize) -> Vec<ProcessedAudio> {
    let sr = self.sample_rate;
    let total = self.samples.len();

    if total <= max_chunk_samples {
        return vec![ProcessedAudio {
            samples: self.samples,
            sample_rate: sr,
        }];
    }

    let silence_threshold: f32 = 0.01;
    let silence_window = (sr as f64 * 0.3) as usize; // 300ms
    let mut chunks = Vec::new();
    let mut offset = 0;

    while offset < total {
        let remaining = total - offset;
        if remaining <= max_chunk_samples {
            chunks.push(ProcessedAudio {
                samples: self.samples[offset..total].to_vec(),
                sample_rate: sr,
            });
            break;
        }

        // Search for silence in the last 25% of the chunk window
        let chunk_end = offset + max_chunk_samples;
        let search_start = offset + (max_chunk_samples - max_chunk_samples / 4);
        let mut split_at = None;

        if chunk_end > silence_window {
            for pos in (search_start..chunk_end).rev() {
                let win_start = pos.saturating_sub(silence_window);
                if win_start < offset {
                    break;
                }
                let window = &self.samples[win_start..pos];
                let rms = (window.iter().map(|s| s * s).sum::<f32>()
                    / window.len() as f32)
                    .sqrt();
                if rms < silence_threshold {
                    split_at = Some(pos);
                    break;
                }
            }
        }

        let end = split_at.unwrap_or(chunk_end);
        chunks.push(ProcessedAudio {
            samples: self.samples[offset..end].to_vec(),
            sample_rate: sr,
        });
        offset = end;
    }

    chunks
}
```

- [ ] **Step 8: Run tests to verify pass**

Run: `cd /home/konrad/code/pai-voice/src-tauri && cargo test audio::tests::split -- --nocapture`
Expected: PASS

- [ ] **Step 9: Commit**

```bash
git add src-tauri/src/audio.rs
git commit -m "feat(audio): add estimate_wav_size and split_at_silence for provider-aware chunking"
```

---

## Chunk 3: Chunked Transcription + Timeout Scaling

### Task 3: Add transcribe_chunked and scale timeouts

**Files:**
- Modify: `src-tauri/src/transcribe.rs`

- [ ] **Step 1: Scale timeout in transcribe_audio based on WAV size**

In `transcribe_audio`, replace the three hardcoded `timeout(Duration::from_secs(30))` with a size-based calculation. Add at the top of the function:

```rust
    // Scale timeout: 30s base + 1s per MB of audio
    let timeout_secs = 30 + (wav_data.len() / (1024 * 1024));
    let timeout = std::time::Duration::from_secs(timeout_secs as u64);
```

Then pass `timeout` to each inner function. The simplest way: calculate it inside each helper too (they all receive `wav_data`), so no signature change needed.

In `transcribe_audio` (OpenAI-compatible path):
```rust
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()?;
```

In `transcribe_audio_deepgram`:
```rust
    let timeout_secs = 30 + (wav_data.len() / (1024 * 1024));
    let timeout = std::time::Duration::from_secs(timeout_secs as u64);
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()?;
```

In `transcribe_audio_gemini`:
```rust
    let timeout_secs = 30 + (wav_data.len() / (1024 * 1024));
    let timeout = std::time::Duration::from_secs(timeout_secs as u64);
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()?;
```

- [ ] **Step 2: Add transcribe_chunked**

Add to `transcribe.rs`:

```rust
use crate::audio::ProcessedAudio;
use crate::provider::Provider;

/// Transcribe ProcessedAudio, automatically chunking if it would exceed the provider's
/// file size limit. Chunk size = 80% of provider limit (safety margin for headers/rounding).
///
/// For short audio that fits in one request, this is a transparent pass-through.
/// For long audio, splits at silence boundaries, transcribes each chunk sequentially,
/// and concatenates results.
///
/// `on_progress` is called before each chunk with (current_1indexed, total_chunks).
pub async fn transcribe_chunked(
    api_url: &str,
    model: &str,
    api_key: &str,
    audio: ProcessedAudio,
    language: &str,
    prompt: &str,
    max_wav_bytes: usize,
    on_progress: Option<&dyn Fn(usize, usize)>,
) -> Result<String, crate::error::TranscribeError> {
    let estimated = audio.estimate_wav_size();

    if estimated <= max_wav_bytes {
        // Fits in one request — pass through unchanged
        let payload = audio.to_wav_payload().map_err(|e| {
            crate::error::TranscribeError::Api {
                status: 0,
                body: e.to_string(),
            }
        })?;
        return transcribe_audio(api_url, model, api_key, &payload.data, language, prompt).await;
    }

    // Calculate max chunk size in samples from the byte limit
    // WAV 16kHz 16-bit mono = 32000 bytes/sec of audio
    // But samples are at native sample_rate, so: max_bytes → max_seconds → max_samples
    let bytes_per_sec: usize = 32000; // at 16kHz after downsample
    let max_secs = max_wav_bytes / bytes_per_sec;
    let safe_secs = (max_secs as f64 * 0.8) as usize; // 80% safety margin
    let max_chunk_samples = safe_secs * audio.sample_rate as usize;

    let chunks = audio.split_at_silence(max_chunk_samples);
    let total = chunks.len();
    let mut results = Vec::with_capacity(total);

    for (i, chunk) in chunks.into_iter().enumerate() {
        if let Some(cb) = &on_progress {
            cb(i + 1, total);
        }

        let payload = chunk.to_wav_payload().map_err(|e| {
            crate::error::TranscribeError::Api {
                status: 0,
                body: format!("chunk {}/{}: {}", i + 1, total, e),
            }
        })?;
        let text =
            transcribe_audio(api_url, model, api_key, &payload.data, language, prompt).await?;
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            results.push(trimmed.to_string());
        }
    }

    if results.is_empty() {
        return Err(crate::error::TranscribeError::Empty);
    }

    Ok(results.join(" "))
}
```

- [ ] **Step 3: Run cargo check**

Run: `cd /home/konrad/code/pai-voice/src-tauri && cargo check 2>&1 | head -30`
Expected: compiles

- [ ] **Step 4: Commit**

```bash
git add src-tauri/src/transcribe.rs
git commit -m "feat(transcribe): add transcribe_chunked with provider-aware sizing and timeout scaling"
```

---

## Chunk 4: Wire Up stop_recording + Frontend Progress

### Task 4: Update stop_recording and add frontend progress listener

**Files:**
- Modify: `src-tauri/src/lib.rs:931-990`
- Modify: `src/main.js`

- [ ] **Step 1: Replace the final transcription block in stop_recording**

In `lib.rs`, replace everything from line 951 (`// Downsample to 16kHz + encode WAV`) through line 970 (the `transcribe_audio` call) with:

```rust
    // Determine provider file size limit
    let provider = provider::Provider::from_url(&api_url);
    let max_bytes = provider.max_file_bytes();

    if debug_log_on {
        let estimated = processed.estimate_wav_size();
        debug_transcription(&format!(
            "FINAL | estimated_wav={} bytes | limit={} bytes ({:?}) | chunked={}",
            estimated,
            max_bytes,
            provider,
            estimated > max_bytes
        ));
    }

    let final_send_time = std::time::Instant::now();

    let handle_ref = &app_handle;
    let transcript = transcribe::transcribe_chunked(
        &api_url,
        &api_model,
        &api_key,
        processed,
        &language,
        &prompt,
        max_bytes,
        Some(&|current, total| {
            let _ = handle_ref.emit(
                "final-chunk-progress",
                serde_json::json!({ "current": current, "total": total }),
            );
        }),
    )
    .await
    .map_err(|e| e.to_string())?;
```

Keep the debug logging after it unchanged.

- [ ] **Step 2: Add frontend listener for final-chunk-progress**

Add to `main.js` after the `transcription-final` listener (~line 351):

```javascript
listen('final-chunk-progress', (event) => {
  const { current, total } = event.payload;
  if (total > 1) {
    transcriptText.textContent = `Processing ${current}/${total}...`;
    transcriptText.scrollLeft = transcriptText.scrollWidth;
  }
});
```

- [ ] **Step 3: Run cargo check**

Run: `cd /home/konrad/code/pai-voice/src-tauri && cargo check 2>&1 | head -30`
Expected: compiles

- [ ] **Step 4: Run all existing tests**

Run: `cd /home/konrad/code/pai-voice/src-tauri && cargo test 2>&1 | tail -20`
Expected: all pass

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/lib.rs src/main.js
git commit -m "feat: wire up chunked final transcription with provider limits and UI progress"
```

---

## Chunk 5: Run Benchmark Tests

### Task 5: Commit the benchmark script and run the quick tier

**Files:**
- Existing (uncommitted): `tests/test_benchmark.sh`, `.gitignore`

- [ ] **Step 1: Commit the benchmark script**

```bash
git add tests/test_benchmark.sh .gitignore
git commit -m "test: add STT provider benchmark script with multi-tier audio samples"
```

- [ ] **Step 2: Download test audio samples**

```bash
cd /home/konrad/code/pai-voice
bash tests/test_benchmark.sh download
```

- [ ] **Step 3: Run quick tier (5s, 15s, 30s samples)**

```bash
bash tests/test_benchmark.sh quick
```

Review the results table. Check that all configured providers return reasonable transcriptions.

- [ ] **Step 4: Run medium tier if quick passes (1min, 3min, 5min)**

```bash
bash tests/test_benchmark.sh medium
```

This will test chunks closer to (but still under) the provider limits. No chunking should be needed yet.

- [ ] **Step 5: Save results**

Results go to `tests/results/` (gitignored). Review but don't commit — these are local reference data.

---

## Summary

| Cosa cambia | Perché |
|---|---|
| `Provider::max_file_bytes()` | Ogni provider ha un limite diverso: Deepgram 2GB, Gemini 20MB, Groq/OpenAI 25MB |
| `ProcessedAudio::estimate_wav_size()` | Decidere se serve chunking senza encodare |
| `ProcessedAudio::split_at_silence(max_samples)` | Spezza ai silenzi, chunk grossi quanto il provider permette |
| `transcribe_chunked()` | Stima → se ok passa dritto → se sfora, splitta per size, trascrive, concatena |
| `stop_recording` update | Usa `transcribe_chunked` al posto di `transcribe_audio` |
| Timeout scalato | 30s base + 1s/MB, evita timeout su chunk grossi |
| Frontend progress | "Processing 2/3..." durante chunked final |
| Benchmark tests | Verifica coi provider reali che tutto funziona |

### Cosa NON cambia
- **Chunk streaming durante registrazione** (5-12s) — invariato
- **Registrazioni corte (<10-13min)** — percorso identico a oggi
- **Audio preprocessing** — VAD, AGC, highpass invariati
- **LLM post-processing** — riceve il testo concatenato, nessun cambiamento

### Futuro (non in questo piano)
- **Modalità riunione/call** — registrazioni >30min, Gemini File API (2GB, 9.5h), Deepgram async callback, salvataggio su GDrive, UI dedicata con timeline. Feature separata che richiede OAuth GDrive e UX specifica.
