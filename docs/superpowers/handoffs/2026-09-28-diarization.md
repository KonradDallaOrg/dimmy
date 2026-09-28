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
- [x] Win build (DLL frozen feature set + C# x64) + local run for the user (launched 2026-09-29 00:24)

## Next step / notes
- Windows DONE and tested by the user 2026-09-29: UI approved; diarization quality so-so on a
  test recorded with a loud TV in the room (extra voices + narrowband audio) — retest on a real
  call in a quiet room before judging the model.
- Also on the branch: transcript auto-follows playback (scrolls on turn change), fe571be1.
- NEXT (2026-09-30, on the Mac): Swift parity. Core needs no change (Mac ships ort +
  libonnxruntime.dylib). To do in Swift: voice-settings toggle + model download
  (`diarization_download_progress` event), relabel after stop when stt_mode=local
  (mirror `Services/DiarizationService.cs`), transcript renderer accepting any label,
  speaker chips + rename (`dimmy_meeting_rename_speaker`), per-speaker waveform lanes, recap
  prompt text (mirror `MeetingRecapHelpers.cs`), auto-follow. Run `scripts/dev/preflight-mac.sh`.
- Open issues: with whisper, turns split ~1 word late (token timestamps; parakeet is better) —
  consider boundary smoothing. Audio-quality lead (not diarization): both tracks of the
  2026-09-29 test had >99% energy below 4 kHz; AEC logged "ref ring empty → idle" 5 s in while
  loopback had signal — investigate separately.
- Branch pushed 2026-09-29 (origin/staging was still c900be85 = base, nothing to merge).
  Dev probe committed: core/src/bin/diarize_asr_probe.rs (modes in its header).
- 2026-09-29 Mac parity DONE (commits 213c910, e3f7b3c, dfcdaf0): Voice → Speakers toggle +
  download, relabel after stop (window + pill), any-label transcript, chips + rename popover
  (chips wrap, no horizontal scroll), Tracks/Speakers lanes, auto-follow, recap prompt.
  Tested locally by the user on the AMI ES2002a 4-min clip. Parakeet on Mac (FluidAudio) has
  no word timestamps → per-chunk dominant speaker; whisper is precise.
- NEXT: continue on Windows (user). Consider porting the chip wrap to Win if the chips overflow.

## Release 0.7.11 (asked 2026-09-29 night: "stacca una staging e una rc"; user signs Windows in the morning)
Version: 0.7.10 has rc.1 + rc.2 on c900be85 (no diarization) → new content = 0.7.11.
Tags: `v0.7.11-staging.1` (staging-tester.yml, Stripe Test) and `v0.7.11-rc.1` (release.yml,
PROD endpoints, prerelease). Check each with `scripts/check-release-tag.sh <tag>` first.
The sign-windows job needs runner PC-KDALLA + SimplySign: it will wait/fail tonight — that is
expected; the user signs in the morning (re-run sign-windows, or scripts/dev/firma-release.ps1).
- [x] R1 bump core/Cargo.toml + both lock files to 0.7.11 on feat/diarization (commit `chore(release): bump to 0.7.11`)
- [ ] R2 release notes + CHANGELOG block (skill `release-notes`)
- [ ] R3 pre-push checks (fmt, clippy CI flags, lib tests)
- [ ] R4 merge feat/diarization --no-ff into staging, push staging
- [ ] R5 wait for staging-auto-update.yml green (incl. test-install) — do NOT tag if red
- [ ] R6 tag + push v0.7.11-staging.1
- [ ] R7 tag + push v0.7.11-rc.1
- [ ] R8 check both workflow runs reach the sign step / artifacts; report to user
