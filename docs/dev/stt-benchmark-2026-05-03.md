# STT benchmark live — 2026-05-03

> Numeri reali misurati su 2 audio italiani (~3.5s). Confronta Groq, Fireworks,
> Together (Whisper/Parakeet/Voxtral), Deepgram, OpenAI, Parakeet locale via
> sherpa-onnx INT8 + fp32 onnx-asr. Drives the Fase 1+ decisions for the
> `feat/stt-providers-expansion` branch. Script di riproduzione in
> [`tests/stt_benchmark/`](../../tests/stt_benchmark/).

Sample audio usati (entrambi 48kHz mono F32 PCM, già preprocessati da Dimmy):
- `2026-03-23_22-51-13/processed.wav` — 3.54s, "Sì, sarebbe più corretto calcolarlo così."
- `2026-03-23_22-51-28/processed.wav` — 3.24s, "Calcolo dove viene fatto in Rust o nella UI."

## Update sera 2026-05-03: Parakeet FP32 + Canary + Whisper locale CPU

| Modello locale | Latenza warm | Output sample 1 | Output sample 2 |
|---|---|---|---|
| **Parakeet TDT v3 FP32 (onnx-asr)** | **337-547 ms** | "Sì, sarebbe più corretto..." ✓ | "calcolo dove viene fatto in Rust..." ✓ |
| Canary 1B v2 (onnx-asr) | 1.0-1.3 s | "Yeah, it would be more correct..." (auto-translate to EN) | "Calculation where..." (translate to EN) |
| Whisper-large-v3-turbo ONNX (onnx-community) | **15-17 s** ❌ | corretto | corretto |
| Parakeet INT8 (sherpa-onnx) | 580-683 ms | "Sarebbe..." (perde "Sì") | "**Alcolo**" (perde "C") |

**Decisione**: Parakeet fp32 è il chiaro winner per local-stt. Sostituisce Whisper distil nel path `local-stt`. Modello 2.5 GB. Latenza sub-second su CPU.

**Canary skip**: ASR+translate dual-mode, default traduce in EN. Senza flag `task=transcribe` esposto a livello onnx-asr non utilizzabile.

**Whisper-large-v3-turbo locale CPU = 15-17s**: inutilizzabile per dictation. Whisper richiede GPU. Su CPU Parakeet è 30-50x più veloce.

**Note libreria**: `onnx-asr` v0.11 espone Parakeet fp32 (auto-download da `istupakov/parakeet-tdt-0.6b-v3-onnx`), Canary 1B v2, Whisper-large-v3-turbo, Vosk, T-One. Niente Moonshine. Niente Voxtral nel registry.

**fp16 Parakeet**: NON esiste come bundle ufficiale. Solo INT8 o FP32 disponibili. Per fp16 serve conversione manuale dai pesi NeMo originali.

## Update sera 2: chunking real-time Parakeet su CPU

Test su 176s di audio italiano reale, dividendo in chunk fissi e processando uno per uno.

| Chunk | Avg latency | Max latency | Real-time safe? | Quality |
|---|---|---|---|---|
| 3s | 477 ms | 770 ms | ✅ 74% margine | leggera regressione (errore minore) |
| **5s ⭐** | 676 ms | 901 ms | ✅ 82% margine | identica al full-transcribe |
| 10s | 1789 ms | 2901 ms | ✅ 71% margine | identica al full-transcribe |

**Insight chiave**: con chunking 5s real-time, la latenza percepita post-stop diventa ~900ms anche per audio di 3 minuti, perché il processing è avvenuto in parallelo alla registrazione. Pareggia Groq cloud anche sui long recording (1.4s).

**Esperienza UX premium**: testo che appare durante la registrazione, ogni 5s una porzione si materializza con <1s di delay. Non è quello che Dimmy fa oggi.

**Implementazione (Fase 4)**: aggiungere un thread/task lato Rust che ogni 5s di accumulato nel ring buffer manda il chunk al backend ONNX. Word boundaries: overlap 300-500ms tra chunk con dedup, oppure VAD-driven chunking (Dimmy ha già VAD nel pipeline). Concatenazione lato client con normalizzazione capitalizzazione.

**Whisper.cpp NON dismesso**: resta come opzione di backend per chi ha disco limitato o vuole modelli piccoli (tiny/base 75-244 MB). Parakeet diventa il default high-quality.

## Classifica unificata

| Backend | Modello | Latenza warm | Output sample 1 | Output sample 2 | $/min |
|---|---|---|---|---|---|
| **Local CPU (sherpa-onnx INT8)** | Parakeet TDT v3 | **580-683ms** (greedy) | "Sarebbe più corretto calcolarlo così." | "**Alcolo** dove viene fatto in Rust o nella UI?" ⚠️ | 0 |
| Groq | whisper-large-v3-turbo | 749ms | "Sarebbe…" | " calcolo dove viene fatto in Rust…" | ~$0.0007 |
| Fireworks | whisper-v3-turbo | 788ms | "Sarebbe…" | (n/a) | $0.0009 |
| Together | nvidia/parakeet-tdt-0.6b-v3 | 831ms | "**Sì**, sarebbe…" ⭐ | "calcolo dove viene fatto in Rust…" | $0.0015 |
| Together | mistralai/Voxtral-Mini-3B-2507 | 1.447s | "Sarebbe…" | (n/a) | $0.0015 |
| Together | openai/whisper-large-v3 | 1.585s | "sarebbe…" (no maiusc) | "calcolo… rust…UI." (rust minusc) | $0.0015 |
| Deepgram diretto | Nova-3 | 2.158s | "Sarebbe…" | (n/a) | $0.0043 |
| OpenAI | whisper-1 | 2.127s | "sarebbe…" (no maiusc) | (n/a) | $0.006 |
| Fireworks | whisper-v3 large | 2.895s | "sarebbe…" (no maiusc) | (n/a) | $0.0015 |

## Insights

1. **Parakeet locale CPU su WSL = 580ms warm**, sub-second. Più veloce di Groq cloud. Su Mac M-series o desktop reale sarà ancora più rapido.
2. Il **Parakeet via Together (cloud)** è l'unico che capta "Sì," iniziale del sample 1 — superiore a tutti gli Whisper-based per la precisione sui first-token.
3. Il **Parakeet locale INT8** sbaglia "Alcolo" invece di "calcolo": è una perdita di precisione del INT8, NON del decoding (`modified_beam_search` non lo fixa). Opzioni: usare fp16/fp32 (modello cresce 2-4x) oppure accettare la imperfezione.
4. **Fireworks ≈ Groq** (margin 39ms): non è un upgrade, è un'alternativa equivalente. Vale come fallback se Groq cade.
5. **Voxtral** funziona ma è il più lento dei modelli "premium" su Together (1.4s). Niente vantaggi visibili rispetto a Whisper su questi sample.
6. **Deepgram via Together (Nova-3, Flux) NON è serverless**: serve dedicated endpoint a ore-GPU. Skip.
7. **OpenAI/Fireworks Whisper-large** non producono punteggiatura senza prompt esplicito. Output peggiore di Whisper-turbo.

## Scelte per la Fase 1+ del branch `feat/stt-providers-expansion`

Priorità ROI:

1. **Fireworks/Together come preset Custom Provider** (mezza giornata).
   - Together OpenAI-compat su `https://api.together.xyz/v1/audio/transcriptions`, model id da settings UI.
   - Fireworks su `https://audio-turbo.api.fireworks.ai/v1/audio/transcriptions`, model `whisper-v3-turbo`.
   - Permette **Parakeet via REST** dentro Dimmy senza nuovo backend Rust.
2. **Parakeet locale via sherpa-onnx** (1.5-2 settimane).
   - Crate Rust: `sherpa-rs` o usare ONNX Runtime nativo (`ort`).
   - Feature flag `local-stt-onnx`, branching in `local_stt.rs` o nuovo `local_stt_onnx.rs`.
   - Modello INT8 (640 MB) come default. Opzione fp16 (1.3 GB) come "high quality" toggle.
   - Cross-platform: ONNX Runtime EP sono CPU/CUDA/CoreML/DirectML.
3. **Google Chirp 3** (1-2 giorni).
   - Solo dopo aver visto numeri da chiave Google. Non testato in Fase 0 perché serve carta su GCP.
4. **AssemblyAI** (1-2 giorni). Solo se Fase 0 dà numeri convincenti, non testato.

## Comandi-tipo verificati live

```bash
# Fireworks turbo (OpenAI-compat)
curl -X POST https://audio-turbo.api.fireworks.ai/v1/audio/transcriptions \
  -H "Authorization: Bearer $FIREWORKS_API_KEY" \
  -F "file=@audio.wav" -F "model=whisper-v3-turbo" -F "language=it"

# Together / Parakeet (OpenAI-compat)
curl -X POST https://api.together.xyz/v1/audio/transcriptions \
  -H "Authorization: Bearer $TOGETHER_API_KEY" \
  -F "file=@audio.wav" -F "model=nvidia/parakeet-tdt-0.6b-v3" -F "language=it"

# Parakeet locale via sherpa-onnx (Python)
python3 -c "
import sherpa_onnx, wave, numpy as np
r = sherpa_onnx.OfflineRecognizer.from_transducer(
    encoder='.../encoder.int8.onnx', decoder='.../decoder.int8.onnx',
    joiner='.../joiner.int8.onnx', tokens='.../tokens.txt',
    num_threads=4, decoding_method='greedy_search',
    model_type='nemo_transducer')
# accept_waveform + decode_stream
"
```

## Stato chiavi e file

- `/home/konrad/code/pai-voice/.env` contiene `FIREWORKS_API_KEY` e `TOGETHER_API_KEY`.
- Bundle Parakeet locale: `/home/konrad/code/pai-voice/.scratch/parakeet/sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8/` (672 MB estratto).
- Script test locale: `.scratch/parakeet/test_local.py`.
- Together Deepgram (Nova-3, Flux) richiede dedicated endpoint, skip per Fase 0.

## Modelli Together STT serverless verificati funzionanti

- `openai/whisper-large-v3`
- `nvidia/parakeet-tdt-0.6b-v3`
- `mistralai/Voxtral-Mini-3B-2507`

Endpoint: `https://api.together.xyz/v1/audio/transcriptions`. Pattern multipart OpenAI-compat. Pricing: tutti $0.0015/min.
