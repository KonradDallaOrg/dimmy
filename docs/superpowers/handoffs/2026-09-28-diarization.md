# Speaker diarization (Nemotron-3-Diarization) — work log

Branch: `feat/diarization` (off `origin/staging` @ c900be85). Windows first; user tests
LOCALLY before anything is pushed. **Do not push, do not tag.**

## Decisions (made, don't re-litigate)
- Model: `joosthel/Nemotron-3-Diarization-ONNX` int8 (`model.int8.onnx` 104 MB +
  `preprocessor_core.onnx`). Offline preset (chunk 340, rc 40, fifo 40, update 300,
  cache 264). One model, no precision picker (fp32 measured no better, 4x bigger).
- Runtime: `ort` (already used by Parakeet), gated by `local-stt-parakeet` feature —
  no new cargo feature, no workflow change. **intra-op threads = 4** (default is 5-7x
  slower on hybrid Intel).
- Diarization is a POST pass over the saved per-track audio, never in the capture loop
  (THE AUDIO RULE). Runs inside `dimmy_meeting_retranscribe` when
  `diarization_enabled` + model present; host triggers a retranscribe after stop.
- Band policy: system track -> always per-speaker labels; mic track -> stays `mic`
  ("Me") unless the diarizer finds >1 speaker on it (in-person meeting).
- Words -> speaker: Parakeet word ts / whisper token ts / Deepgram words; other
  engines fall back to dominant speaker per line.
- Speaker names: `speakers.json` in the meeting dir (id -> name, color index).
- Settings: toggle lives in the Voice input section, NOT under STT providers.

## Status
- [x] Research + benchmark (see memory `diarization-nemotron-eval`)
- [ ] core `diarize.rs`: download, mel, speaker cache, segments + tests
- [ ] config field `diarization_enabled` + FFI (status/download/enable)
- [ ] retranscribe integration + speakers.json + rename FFI
- [ ] recap/names: transcript labels resolved to names
- [ ] Win Settings toggle + download progress
- [ ] Win MeetingWindow: speaker list/rename, labels, per-speaker waveform tab
- [ ] Win build (DLL frozen feature set + C# x64) + local run for the user

## Next step / notes
(update this section at every milestone)
