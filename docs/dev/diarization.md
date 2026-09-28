# Speaker diarization

"Who said what" in meeting transcripts. Off by default; Settings → Voice input →
Speakers → *Identify speakers in meetings* (config `diarization_enabled`).

## Model

NVIDIA **Nemotron-3-Diarization** (released 2026-09-23): a 100 M-parameter streaming
Sortformer, up to 8 speakers, 10 ms resolution, OpenMDW-1.1 licence (commercial use
allowed). We run the community ONNX export `joosthel/Nemotron-3-Diarization-ONNX`,
pinned to revision `4a911fc` and checked by SHA-256:

| File | Size | Role |
|---|---|---|
| `preprocessor_core.onnx` | 136 KB | preemphasized PCM → 128-bin log-mel |
| `model.int8.onnx` | 104 MB | one 30.4 s chunk + speaker context → logits + embeddings |

Downloaded to `<config>/diarization/` by `dimmy_diarization_download`. The speaker cache
that carries identity across chunks (FIFO + arrival-order speaker cache) is NOT in the
graph: it is ported in `core/src/diarize.rs::SpeakerCache` from the export's numpy
reference. The silence embedding it needs is `core/assets/diarization_silence_embeds.f32`.

## Where it runs

Never in the capture path (THE AUDIO RULE). It is a pass over the saved per-track
audio inside `dimmy_meeting_retranscribe`:

1. Each track (`audio_mic`, `audio_system`) is transcribed as before, but words with
   timestamps are kept: Parakeet word timestamps, whisper token timestamps
   (`local_stt::transcribe_local_words`), Deepgram words. Engines without timestamps
   (Qwen, other cloud providers) fall back to the dominant speaker per line.
2. The track is diarized (`diarize::diarize`, offline preset, 4 ORT threads).
3. `diarize::group_words` puts every word on the speaker who talks most during it and
   groups turns; `diarize::label_bands` numbers speakers (system track first) and
   writes `speakers.json`.
4. `transcripts.txt` keeps its format; the label is the speaker's NAME:
   `[00:01:23] [Speaker 2] …`. The mic track stays `[mic]` unless the diarizer hears
   more than one person on it (an in-person meeting).

The Windows host runs it automatically after a meeting stops **only with local STT**
(`Services/DiarizationService.cs`): with cloud STT it would upload the whole meeting a
second time, so there it happens only on *Regenerate transcript*.

## Names

`dimmy_meeting_rename_speaker(dir, id, name)` rewrites `speakers.json` AND the labels in
`transcripts.txt`, so recap, search, Notion/Obsidian export and the MCP server all read
the real name without knowing about diarization. Names are 1–40 chars, no brackets,
unique, and never `mic` / `system` / `paused`. A later re-run keeps names by speaker id.

## Measurements (i7-12700H, CPU, 2026-09-28)

| | |
|---|---|
| Speed, offline preset, 4 threads | 66–72× realtime (11 min meeting in 9.9 s) |
| ORT default threads (one per core) | 5–7× SLOWER — the pool straddles E-cores |
| Italian, 4 voices (MLS) | DER 5.0 %, 0.8 % of words on the wrong speaker |
| Italian, 6 voices | DER 11.8 %, 5–6 % wrong words |
| Italian, 2 voices | one voice split in two (13–26 % wrong words) — unexplained |
| Rust port vs Python reference | same DER on the 4-voice set; chunk-level decisions match within 0–6 % on real audio (ORT 1.22 vs 1.30 numerics) |

Italian is not among the model's training languages (EN/ZH/HI/KN/TE/BN); it works anyway.

The streaming presets (0.32–1.04 s latency) keep up on CPU at 1.04 s with 4 threads
(RTFx ≈ 5), not at 0.32 s. Live labels are not implemented: labels can still change as
the cache learns a voice, and the post-pass is both cheaper and more accurate.

## Not done yet

- macOS / Linux hosts (the core works on any platform with `local-stt-parakeet`).
- Live (during-recording) labels.
- Keeping live word timestamps so the post-stop pass could skip re-transcription.
