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
- [x] core `diarize.rs`: download, mel, speaker cache, segments + tests
- [x] config field `diarization_enabled` + FFI (status/download/enable)
- [x] retranscribe integration + speakers.json + rename FFI
- [x] recap/names: transcript labels resolved to names
- [x] Win Settings toggle + download progress
- [x] Win MeetingWindow: speaker list/rename, labels, per-speaker waveform tab
- [ ] Win build (DLL frozen feature set + C# x64) + local run for the user

## Next step / notes
- Core committed 0fb15555; Win UI + docs 99d0829d. 1022 Rust + 487 C# tests green.
- Build env: `C:/dp/t.cmd <cmd>` (vcvars64 + Ninja + CARGO_TARGET_DIR=C:/dp). The DLL is built
  there with the frozen feature set, then copied (dimmy_lib/ggml*/llama*/mtmd*.dll) into
  core/target/release so the csproj picks it up.
- The user's dev Dimmy runs from bin/x64/Debug; it must be closed before the final C# build.
- Auto relabel after stop runs ONLY with stt_mode=local (cloud would re-upload the meeting).
- Remaining: build + launch for the user's local test. Then wait for feedback.
