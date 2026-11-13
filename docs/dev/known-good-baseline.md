# Known-good baseline (v0.6.66) — as-built behavior + freeze contract

> **Checked against the code on 2026-11-11** (`origin/staging` at `ebc0b52f`,
> v0.7.15). The `file:line` citations below were moved to that commit; a
> citation without a line names the symbol only.
>
> **Invariants that still hold** (where they are now):
> - §1 Meetings always `AudioSource::Mix` at 48 kHz — `ffi.rs:4759`, `audio.rs:24`.
> - §1 `align_secondary` on every 100 ms tick, and `mic_lag_padding` — `meeting.rs:2247`/`:2334`, `:641`.
> - §1 The AEC worker never blocks on an empty reference ring — `aec.rs:151–170`.
> - §1 `join_bounded(120 s)` at stop — `meeting.rs:515`.
> - §1 macOS off-main HAL teardown, `TapAutoStart=false` + `AudioDeviceStart`, loopback rate published before samples — `SystemAudioProcessTap.swift:1372`/`:1432`/`:492`/`:602`/`:465`, `SystemAudioCaptureService.swift:166`.
> - §1 `MEETING_CAPTURE_GATED` — `audio.rs:53`, `meeting.rs:360`/`:470`/`:484`/`:503`.
> - §1 Windows loopback follows the call's output; the mic-move rule — `audio.rs:1902` (`loopback_target`), `:1662` (`mic_decision`).
> - §2 The 11 `===NAME===` recap markers — `MeetingRecapHelpers.cs:41`, `MeetingPostProcessService.swift:235`.
> - §2 `recap_auth_method` independent of dictation auth, empty ⇒ `api_key` — `ffi.rs:7285–7294`.
> - §2 Recap vendor derived from the model id — `ffi.rs:7398–7409`.
> - §2 Truncation guard (one 4× retry, then rc −10) and 600 s timeout — `llm.rs:1385–1396`, `:1365`.
> - §3 Three bindings, edge-only events, `MODIFIER_SUPPRESS`, injected-event bypass, `VK_NONAME` — `hotkey.rs:212–219`, `:127`, `:977–993`, `:930`, `:855–863`.
> - §4 AES-256-CTR + HMAC-SHA256, key derivation, on-disk entry names — `keystore.rs:200–306`, `:19–37`, `provider.rs:324`.
> - §4 Single-writer config and if-empty-omit — `lib.rs:1308`, `SettingsViewModel.cs:899–929`.
> - §5 Dictation is Mic-only at 48 kHz — `ffi.rs:1160`, `:1137`.
> - §5 Drain before clear, `STOP_IN_PROGRESS` — `ffi.rs:1710–1741`, `:1581`.
> - §5 wVk + wScan paste, phantom-modifier release — `TextInjectionService.cs:310–334`, `:154–179`.
> - §5 Deepgram streaming on native-tls — `deepgram_stream.rs:24`.
> - §5 `preprocess_route` is the single source of truth — `preprocess.rs:685`.
>
> **Changed since v0.6.66** (corrected in place below):
> - §1 Capture and transcription are two threads: `worker_loop` only writes audio, `stt_thread_loop` transcribes — `meeting.rs:1987`, `:1443`.
> - §1 Default `meeting_chunk_secs` is 30, not 15 — `lib.rs:785`.
> - §1 No neural denoiser in the AEC worker — `aec.rs:106–120`.
> - §1 The `.recording` marker is removed at the end of every `MeetingSession::stop` — `meeting.rs:554`; crash recovery is `recover_interrupted` — `meeting.rs:3592`.
> - §1/§5 One owner of the capture (`capture.rs`): a meeting start is refused while a dictation records (rc −4) — `ffi.rs:4783–4788`.
> - §2 Windows has one stop path, `MeetingStopService.StopAsync`; `GeneratePostProcessAsync` is only "Regenerate recap" — `MeetingStopService.cs:105`, `MeetingWindow.xaml.cs:2398`.
> - §2 The recap rc table is −1..−13 — `ffi.rs:7675`, `:7734`.
> - §2 Subscription also routes to the Kimi and Gemini CLIs — `ffi.rs:7524–7547`, `llm.rs:1194–1299`.
> - §4 Undecryptable keystore entries are kept, not lost — `keystore.rs:520–561`.
> - §5 `preprocessing_enabled` is off by default, so the default route is `Raw` — `lib.rs:1225`.
> - §5 Streaming engines now include OpenAI realtime and Nemotron — `ffi.rs:67–72`.
> - §5 The chunked dictation window is 5 s or 15 s, not 3 s — `chunked_stt.rs:371`.

> **What this is.** A code-verified, as-built description of Dimmy's load-bearing
> features at the **v0.6.66** stable release: for each one, *how it works today*,
> the *owning files (file:line)*, the *load-bearing invariants*, and *what would
> silently break it*. It exists so anyone (human or agent) can answer **"was it
> always like this? which file owns it?"** when a feature regresses — diff the
> current code against this document.
>
> **Status: FROZEN.** These paths are not changed without (1) a reproduced bug and
> (2) a test that would have caught it. This is the technical companion to the
> release-side discipline in [`CONTRIBUTING`](../../CONTRIBUTING.md) and the
> "Executing actions with care" + frozen-recipe rules in
> [`../../CLAUDE.md`](../../CLAUDE.md). Update this file **only** when the actual
> behavior deliberately changes — keep it truthful.
>
> **Related docs** (this page is the end-to-end view; those are the deep dives):
> - Audio DSP + route-aware preprocess + AEC: [`audio-pipeline.md`](audio-pipeline.md)
> - Per-module reference: [`modules.md`](modules.md)
> - Known bugs / platform traps (macOS FFI, Windows transparency, audio): [`known-bugs.md`](known-bugs.md)
> - System-audio capture test coverage: [`system-audio-capture-tests.md`](system-audio-capture-tests.md)
> - Testing tiers + how to run: [`testing.md`](testing.md)
> - Streaming dictation internals: [`streaming-dictation-deepgram.md`](streaming-dictation-deepgram.md)

---

## 1. Meeting recording + system-audio capture (Teams/Zoom + mic, always-Mix, AEC, chunked STT, crash-safe stop)

**How it works today.** `dimmy_meeting_start` (ffi.rs:4738) refuses a second session (MEETING mutex, :4743) and a start while a dictation records (rc −4, `capture::claim_meeting`, :4783), and ALWAYS forces `AudioSource::Mix` (:4759) — the legacy `audio_source` config is ignored for meetings. Mic + system both run at `MEETING_CANONICAL_RATE=48kHz` (audio.rs:24) via cpal-callback resampling. The capture worker (meeting.rs:1987 `worker_loop`) mixes mic+system into `audio(.ogg/.wav)` plus separate `audio_mic`/`audio_system` sinks and does nothing else: every `chunk_secs` (default 30 since 2026-09-09, clamp 5–60; 0.5 s for Nemotron) it hands one window per track to a bounded queue (`STT_QUEUE_DEPTH = 4`, `try_send`). The transcription thread (meeting.rs:1443 `stt_thread_loop`) transcribes mic and system separately into `transcripts.txt` and emits `meeting_chunk` events. A dedicated AEC worker (aec.rs:72) drains 480-sample/10ms frames (HPF→AEC3→NS→AGC2, no neural denoiser since 2026-08-10) using loopback as render reference and mic as capture, appending cleaned mic to the primary buffer. **System audio per platform:** Windows = WASAPI endpoint loopback on the output the call app plays to (`capture_sessions.rs` + `loopback_target`, since 2026-11-02), else the default output (`resolve_loopback_device`); macOS = Core Audio process tap on 14.4+ else ScreenCaptureKit, pushing via `dimmy_push_loopback_audio` (ffi.rs:3852); Linux = same external push path, WAV-only. `dimmy_meeting_stop` (ffi.rs:4944) emits meeting-inactive immediately (:4964), then joins the worker via `join_bounded(120s)` (meeting.rs:515; the helper is at :68) so a wedged HAL yields `TimedOut` instead of a frozen app. Inside the worker the three sinks are finalized first, then the transcription thread is joined with its own 90 s bound (meeting.rs:2707–2793). Call auto-detect is a pure IO-free state machine (call_detector.rs) fed by the hosts through `dimmy_call_signal` (ffi.rs:12660); on Windows a 250 ms sampling timer that runs while an app holds the microphone (`CallDetectionService.cs`).

**Owns.** core/src/meeting.rs (MeetingSession, worker_loop, stt_thread_loop, TrackSink, join_bounded, meta helpers); core/src/capture.rs (the one owner of the capture: `claim_meeting` / `claim_dictation`); core/src/ffi.rs:4738–5420 (meeting FFI), :3852 (loopback push), :12660–13036 (call-detector FFI); core/src/audio.rs (Mix capture, loopback resolve/resampler, canonical rate, capture gate); core/src/aec.rs (AEC3 worker); core/src/capture_sessions.rs (the call's mics and outputs); core/src/device_events.rs (OS device notifications); core/src/call_detector.rs (state machine); platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift (tap + off-thread HAL teardown); platforms/macos/Dimmy/Services/SystemAudioCaptureService.swift (backend selection); platforms/windows/Dimmy.Windows/Services/CallDetectionService.cs; platforms/macos/Dimmy/Services/CallDetectionManager.swift.

**Invariants (do NOT change).**
- Meetings are ALWAYS `AudioSource::Mix`; legacy `audio_source` is ignored (ffi.rs:4759). Both streams run at 48kHz (audio.rs:24).
- `align_secondary` pads the system buffer with zeros up to the mic buffer's length each 100ms tick (meeting.rs:2247, called at :2334) — the only thing keeping mic and late-waking loopback wall-clock aligned.
- Nothing may stand between captured audio and the disk (since 2026-09-03): `worker_loop` only reads the buffers, writes the three sinks and queues windows with `try_send`; transcription, host events and telemetry run on `stt_thread_loop`; a full queue drops the window, not the audio. Guard: `meeting.rs` `mod audio_never_blocked`. See [`meeting-audio-durability.md`](meeting-audio-durability.md).
- Its mirror, `mic_lag_padding` (since 2026-11-05): when the mic trails the system track by more than 2 s (muted, dead, reopening), the mic buffer is filled with silence up to the system track, and at stop with no tolerance. The writer clocks on the mic, so without it a mic delivering a quarter of real time wrote the CALL at a quarter of real time (950 s of a 3917 s meeting). A healthy mic is never touched. Guard: `meeting.rs` `mod a_slow_mic_never_throttles_the_recording`.
- AEC worker never blocks on an empty ref ring — it zero-pads the render frame (aec.rs:151–170) so mic always flows.
- `join_bounded(120s)` (meeting.rs:515) guarantees `dimmy_meeting_stop` returns even if the worker/HAL wedges.
- macOS: HAL destroy MUST run off-main (SystemAudioProcessTap.swift:1372 `teardownQueue`, assert :1432); aggregate uses `TapAutoStartKey=false` + explicit `AudioDeviceStart` (:492/:602) or Tahoe yields 0 samples; loopback rate MUST be the real stream rate, published before samples flow (`dimmy_set_loopback_sample_rate`, SystemAudioProcessTap.swift:465/:545, SystemAudioCaptureService.swift:166) and passed again with every push (SystemAudioCaptureService.swift:114; the SCStream path reads it from the ASBD at :258–283).
- `MEETING_CAPTURE_GATED` gates capture appends while paused and is always cleared on start/stop (audio.rs:53, meeting.rs:360/:503). `.recording` marker written at start (meeting.rs:368) and removed at the end of `MeetingSession::stop` (:554) — also after a panicked or timed-out join; only a process that dies leaves it. A leftover marker drives crash recovery (`recover_interrupted`, meeting.rs:3592).
- Windows system audio is endpoint loopback that follows the call's output device (a call app moving its speaker from its own menu silenced the old copy, 2026-11-02). Process loopback was tried and removed: a live Teams call came through it as digital zeros.
- The meeting mic leaves the default device only when it has delivered digital zeros for 4 s, nobody else captures from it, and exactly one other device is being captured (`audio.rs::mic_decision`). A working mic is never moved; a pinned one is only reopened.

**Config.** `meeting_chunk_secs` (5–60, default 30 — `DEFAULT_MEETING_CHUNK_SECS`, lib.rs:785; a stored 15 is migrated to 30), `stt_mode`, `api_url/api_model/api_key`, `local_stt_backend` (whisper|parakeet|qwen|whistle|nemotron), `local_model`, `language`, `prompt`+`user_dict`, `selected_device`, `preprocessing_enabled` (gates the per-window chunk gate on local STT); call-detector: `enabled`, `min_active_secs`(1), `cooldown_secs`(1800); cargo feature `local-dfn` no longer changes meeting capture (the denoiser left the AEC worker on 2026-08-10; the feature only builds DFN3 for the `denoise_offline` tool); Ogg/Vorbis gate = Windows+macOS only (Linux WAV); `MEETING_CANONICAL_RATE=48000` compile-time const.

---

## 2. Automatic meeting recap (transcript → structured LLM recap → recap.md/actions.json, independent auth/model dispatch, Notion/folder export)

**How it works today.** After `dimmy_meeting_stop` returns `{dir, transcript}`, recap fires if the per-meeting "Generate recap" toggle is on (`MeetingGenerateRecap`/`generateRecap`, default true). `BuildStructuredRecapPrompt` (Win MeetingRecapHelpers.cs:209; Mac MeetingPostProcessService.swift:489) builds a fixed prompt demanding line-1 `# Title` H1, an invisible `<!-- dimmy-type: KEY -->` tag, and EXACTLY 11 `===NAME===` sections. Host calls `dimmy_llm_call_raw(prompt, modelOverride, maxTokens, buf, buf_len)` (ffi.rs:7235) → `crate::llm::process_raw_prompt` (llm.rs:1176). Recap auth (`recap_auth_method`) is orthogonal to dictation auth: empty ⇒ `api_key`, NEVER inherits `subscription` (ffi.rs:7285–7294). Recap vendor is derived from the picked model id, key resolved cross-scope (ffi.rs:7398–7510). On rc>0 the host runs `ParseStructuredRecap` → `BuildMarkdownFromSections` → `dimmy_meeting_save_post_process(dir, recapMd, actionsPlain, null, model)` (ffi.rs:5048 → meeting.rs:2936) which writes `recap.md` (stamped with the AI-generated marker) + `actions.json` and syncs the title into meta.json. **Platform split (changed since v0.6.66):** on Windows every stop (meeting window, pill, shortcut, call-detect) goes through `MeetingStopService.StopAsync` (since 2026-11-09), which runs the shared `MeetingPostProcessService.RunRecapAsync` (MeetingStopService.cs:105; reads notes.md, exports, Notion-auto-sends). `MeetingWindow.GeneratePostProcessAsync` (MeetingWindow.xaml.cs:2398) remains only for "Regenerate recap" (:3960): it reads notes.md (since 2026-09-14) and exports to the folder, with NO Notion auto-send. macOS routes BOTH in-window and pill stops through the single shared `MeetingPostProcessService.runRecap` (MeetingViewModel.swift:814, PillWindowController.swift:573).

**Owns.** core/src/ffi.rs:7235 (`dimmy_llm_call_raw` — auth+model routing, key resolution), :7675 (`categorize_llm_error_to_rc`) + :7734 (`local_failure_rc`) — together the −1..−13 contract, :5048 (`dimmy_meeting_save_post_process`); core/src/llm.rs:1176 (`process_raw_prompt` — CLI/HTTP branches, truncation guard at :1385); core/src/meeting.rs:2936 (`save_post_process`); platforms/windows/Dimmy.Windows/Helpers/MeetingRecapHelpers.cs (prompt/11-section contract/parser/rc→message); Services/MeetingStopService.cs (the one Windows stop path); Services/MeetingPostProcessService.cs (shared Win pipeline + Notion); Views/MeetingWindow.xaml.cs:2398/2856 ("Regenerate recap" path + `PickRecapModel`); Services/RecapExportService.cs (folder export); platforms/macos/Dimmy/Services/MeetingPostProcessService.swift; Utilities/RecapModel.swift; Controllers/PillWindowController.swift:573.

**Invariants (do NOT change).**
- The 11 `===NAME===` markers are a wire contract shared verbatim by prompt builder, parser, and renderer across Win C# and Mac Swift (MeetingRecapHelpers.cs:41 == MeetingPostProcessService.swift:235). Rename in one → silent round-trip break.
- `recap_auth_method` is INDEPENDENT of dictation `llm_auth_method`; empty ⇒ `api_key`, NEVER inherits `subscription` (ffi.rs:7285–7294). Subscription routes ONLY to a local CLI: `claude`, `codex` (OpenAI URL) or `kimi` (Kimi URL); under subscription a model that is not Claude-family, on a URL that is neither OpenAI nor Kimi, falls back to `api_key` (ffi.rs:7524–7541). `process_raw_prompt` also has a Gemini CLI branch (`gemini-cli://` or subscription + Gemini URL, llm.rs:1194–1199), which that guard does not exempt.
- Recap vendor is DERIVED from model id, not a config field (ffi.rs:7398–7409). Auto model-pick reads the LIVE `dimmy_get_config_json` snapshot, NOT config.json file (RecapModel.swift:34 / MeetingWindow.xaml.cs:2856).
- The rc table is now −1..−13 (−12 = local model did not fit in GPU memory, −13 = other local-model failure, both from `local_failure_rc`). It is shared 3 ways and must stay in sync: ffi.rs:7675/:7734, Win `RecapRcToUserMessage` (MeetingRecapHelpers.cs:755), Mac `Failure.description`/DimmyCore+V2.swift:566–578. Only the numeric rc / categorical `&static str` crosses FFI on error — the HTTP body (echoes transcript) is stripped (ffi.rs:7604–7643).
- First output line must be `# Title` H1 → `parse_recap_title` (meeting.rs:3190) writes meta.json. Truncation guard returns `Truncated` (rc −10) after ONE 4×-headroom retry (llm.rs:1385–1396) — never save a partial recap. 600s CLI/HTTP timeout (llm.rs:1207–1365).
- Notion auto-send + folder export are best-effort, never throw/block (MeetingPostProcessService.cs:129–184; RecapExportService.cs:27–48 try/catch). Obsidian = folder export; there is no separate Obsidian integration.

**Config.** `recap_auth_method` (empty⇒api_key; subscription→claude/codex/kimi CLI), `recap_model_override` (empty=Auto; `cloud:<id>`; `local:<file.gguf>`), `recap_use_same_key` (default true), `llm_mode`, `llm_api_url/model/key`, `local_llm_model`, `notion_auto_send`, `UiPreferences.RecapExportFolder` / UserDefaults `recapExportFolder`, `MeetingGenerateRecap`/`generateRecap`, `meetingType` taxonomy key.

---

## 3. Global shortcuts (dictation / command-mode / meeting)

**How it works today.** Three independent bindings run on ONE OS keyboard hook per platform. In Rust (hotkey.rs) they are statics DICT (:212), CMD (:214), MTNG (:219); each `Binding` (:32) holds packed L/R VK codes + down-flags + `combo_active`. `Binding::process` (:127) is pure logic — PRESSED fires only on the false→true swap of `combo_active`, RELEASED only on true→false (idempotent under auto-repeat). **Windows** is the real hook: `install_hook` (:1004) installs `WH_KEYBOARD_LL`; `keyboard_proc` (:919) feeds every physical key to all three bindings; on a Pressed transition it sets `MODIFIER_SUPPRESS` and calls `emit_synthetic_combo_release` (:855) — injecting synthetic KEYUPs (and a `VK_NONAME` chord-buster :749 when WIN is a modifier) so the shell never opens Start Menu; suppression clears only when NO binding is `combo_active` AND all three report all_released (:986–994). While the shortcut recorder is open and a Dimmy window is in front, the hook hands every key to `hotkey_recorder` instead of the bindings (:945–960). Each edge goes from the hook onto an unbounded queue; a dedicated thread turns it into one `hotkey` event (`{"binding","edge"}`) on the event callback, and C# `HotkeyService.cs` reacts to it in `HandleEvent` (it polled one-slot mailboxes every 10 ms until 2026-11, which could lose a press); the hook never calls host code. Toggle-vs-hold is decided HOST-SIDE (`PttMode`, config `shortcut_mode=="hold"`). **macOS** does NOT use the Rust hook (`dimmy_hotkey_install` never called; Rust macOS CGEventTap is dead code and omits MTNG). Swift `HotkeyManager.swift` owns a `.cgSessionEventTap`; `CommandComboState` (:1109) ports the Rust Binding for command+meeting. **Linux is a no-op stub** (hotkey.rs:1351–1354, platforms/linux/src/hotkey.rs:33 stub) — shortcuts non-functional. `combos_conflict` (:557) = subset relation; hosts validate before binding.

**Owns.** core/src/hotkey.rs:32 (Binding + process() :127), :212 (DICT/CMD/MTNG), :477/:507/:516 (set_*), :557 (combos_conflict), :648 (deliver_edge), :679 (Windows mod: keyboard_proc :919, install_hook :1004, emit_synthetic_combo_release :855, VK_NONAME :749), :1029 (macOS mod — its CGEventTap is not installed on shipping builds; its keycode table serves the shortcut recorder), :1351 (Linux stub); core/src/hotkey_recorder.rs (the "set shortcut" recorder); core/src/ffi.rs:9371–9445 (hotkey FFI) & :11043–11141 (recorder FFI); platforms/windows/Dimmy.Windows/Services/HotkeyService.cs + HotkeyEdgePolicy.cs; App.xaml.cs; Services/UiPreferences.cs:88/97/105; Views/SettingsWindow.xaml.cs:298–319 (conflict validation); platforms/macos/Dimmy/Managers/HotkeyManager.swift (real macOS owner); State/AppState.swift; platforms/linux/src/hotkey.rs (stub).

**Invariants (do NOT change).**
- Three SEPARATE statics with independent state — prevents cross-firing (hotkey.rs:32/212–219). Event fed to ALL three bindings; each edge is queued with its own binding id (:967–980, `deliver_edge` :648).
- PRESSED/RELEASED fire only on the `combo_active` edge via swap (:143/:149/:157/:163/:199/:201) — makes auto-repeat idempotent.
- `MODIFIER_SUPPRESS` clears ONLY when no binding is combo_active AND all three all_released (:986–994); injected events (`LLKHF_INJECTED`) MUST bypass the state machine (:930); suppression is scoped to combo keys so Win+E/Win+L still reach the OS.
- `VK_NONAME` chord-buster is required specifically when WIN is a modifier (:749/:860–863) — Alt/Ctrl/Shift don't count.
- Toggle-vs-hold is a HOST decision (core only emits edges): Windows `shortcut_mode`, read live on every release via `HotkeyEdgePolicy` (a copied `PttMode` went stale after Settings' auto-save, fixed 2026-11-06), macOS `preferredMode`. Meeting hotkey is TOGGLE-ONLY on every surface — host acts on pressed, ignores released (HotkeyService.cs:99–116, HotkeyManager.swift:331 and :1080–1081).
- Dictation binding NEVER disables (falls back to default); command & meeting DO disable on empty combo. macOS correctness depends on `dimmy_hotkey_install` NOT being called there.
- Command/meeting combos live in host-local prefs (ui_prefs.json / UserDefaults), NOT AppConfig — `lib.rs` has no command/meeting field.

**Config.** config.json `shortcut` (dictation; core-owned; default win+alt / cmd+option), `shortcut_mode` ('hold'|'toggle'); Windows ui_prefs.json `CommandHotkey` (default '' disabled), `MeetingHotkey` (host default 'ctrl+alt+m' — core MTNG starts empty), `DictHotkey` (add-to-dictionary, 'ctrl+shift+d'); macOS UserDefaults `shortcutEncoded`/`commandHotkeyEncoded`/`meetingHotkeyEncoded`/`preferredMode`.

---

## 4. API Keys + Providers (STT / LLM / recap storage + resolution)

**How it works today.** All keys live in ONE encrypted file `keys.enc` in the config dir (keystore.rs; path `config_dir_path().join("keys.enc")` :517) — a JSON map of entry-name → base64 blob. **Encryption is AES-256-CTR + HMAC-SHA256 (encrypt-then-MAC), NOT GCM** (keystore.rs:200–306; module doc line 1 says so explicitly). Per-entry: `base64(nonce[12] || ciphertext || mac[32])`; MAC verified constant-time BEFORE decrypt (:299). Master key = `SHA-256(username:hostname:dimmy-local-key-v1)` (:19–37), split into enc/mac subkeys (:248). SHA-256/HMAC/AES are hand-rolled in-file, NIST-validated. OS keyring is READ-ONLY migration fallback only; `use_keyring` forced false. Entries namespaced by `KeyringScope` (provider.rs:303–340): `Stt→api-key-<v>`, `Llm→llm-key-<v>`, `Recap→recap-key-<v>`, `NotionToken→integration-notion-token`, `ConfluenceToken→integration-confluence-token`. `Provider` enum detects vendor from URL/model-id/tag. STT key resolves cross-scope within the SAME vendor (STT→LLM→Recap, `load_key_any_scope`, ffi.rs:325–366); LLM/recap resolve Llm(vendor)→Stt(vendor). The unified Providers card saves ONE key into EVERY capable scope (SettingsWindow.Providers.cs:576–609; Mac MacProvidersPage.swift:513–544), validated against vendor capability by `dimmy_save_llm_provider_key` (ffi.rs:6851–6915). Subscription URLs `claude-code://` and `codex://` skip key resolution entirely.

**Owns.** core/src/keystore.rs (encryption, machine key, save/load/has_key/migrate); core/src/provider.rs (Provider + KeyringScope + capability gates); core/src/ffi.rs (STT/LLM/recap resolution, `load_key_any_scope`, config setter, `dimmy_save_llm_provider_key`); core/src/lib.rs (AppConfig identity fields, `save_config_file` single-writer); core/src/claude_code.rs + codex.rs (synthetic URLs); platforms/windows/Dimmy.Windows/Views/SettingsWindow.Providers.cs; ViewModels/SettingsViewModel.cs (ToJson if-empty-omit); Services/ProviderCatalog.cs (KeySaveScopes); platforms/macos/Dimmy/Views/Settings/MacProvidersPage.swift.

**Invariants (do NOT change).**
- AES-256-CTR + HMAC-SHA256, MAC-verified-before-decrypt (keystore.rs:200–306/:299) — do NOT "upgrade" to GCM assumptions.
- Master key = `SHA-256(username:hostname:dimmy-local-key-v1)` (:19–37), machine-bound. Changing the derivation string, subkey tags, blob layout, or username/hostname source makes every existing `keys.enc` entry undecryptable (only a WARNING is logged, :554). The entries are not deleted: an entry whose MAC fails is kept as raw ciphertext (`LocalStore::sealed`, :520–561) and written back unchanged on the next save, so it is readable again when the old username/hostname return. Until then the keys are unusable.
- `KeyringScope::entry_name()` strings and `Provider::as_str()` vendor tags are the ON-DISK keys — renaming orphans stored keys. Empty-string save DELETES the entry; `has_key` treats stored-empty as absent (:679–687/:722–725).
- Cross-scope fallback stays within the SAME vendor only (`load_key_any_scope`, ffi.rs:325–366). Config setter applies identity fields ONLY when the JSON key is present (`dimmy_set_config_json`, ffi.rs:2897/3013); host ToJson OMITS empty identity/credential fields (SettingsViewModel.cs:899–929) — together they prevent a transient empty VM from wiping saved URLs/keys (the 2026-05-16 incident).
- Single-writer: only Rust writes config.json (`save_config_file`, lib.rs:1308); UIs go through `dimmy_set_config_json`.
- Subscription detection depends on exact schemes `claude-code://` / `codex://` (claude_code.rs:1437–1441, codex.rs:613–617; also `kimi-cli://` kimi_cli.rs:861 and `gemini-cli://` gemini_cli.rs:597) and must be checked BEFORE key resolution. `dimmy_save_llm_provider_key` enforces scope-vs-vendor capability (ffi.rs:6884–6895).

**Config.** `api_url/api_model`, `llm_api_url/llm_api_model` (also holds synthetic URLs), `local_model`/`local_llm_model`, `selected_device`, `llm_auth_method`, `recap_auth_method`, `llm_use_same_key`/`recap_use_same_key`, `recap_model_override`, `llm_enabled`/`llm_mode`, `use_keyring` (forced false), `meeting_storage_path`, `notion_target_id/kind/title` (token stored as `KeyringScope::NotionToken`).

---

## 5. Dictation + Paste (hotkey → capture → STT → LLM → paste), Streaming dictation, Command Mode

**How it works today.** `dimmy_start_recording` (ffi.rs:1065) refuses if a meeting is active or still holds the capture (rc −7; `capture::claim_dictation`, :1112), already recording or the previous stop still draining (rc −2), or cloud-mode with no key (rc −1); forces `audio_sample_rate=48kHz` (:1137); sends `AudioCommand::Start` with `AudioSource::Mic` (:1160) — **dictation is MIC-ONLY** (Mix/AEC dropped ~60% of samples). Engine selection (:1196–1489): streaming+cloud → `OpenAiStreamer` when the STT provider is OpenAI and its key is saved, else `DeepgramStreamer` when a Deepgram key is saved; streaming+local → `ChunkedTranscriber` typing mode ("local-stream"); chunk_streaming+local → caption mode; with the `nemotron` backend both local modes use `nemotron::Streamer` instead of the chunked engine; else "batch". Streaming engines emit a `(delta, cumulative, is_final)` contract via `stt_chunk` events; `is_final` exactly ONCE at stop-drain. Host injects stable deltas at the cursor via SendInput unicode (Win AppViewModel.cs:641 → App.xaml.cs:1666 `TypeUnicodeText`; Mac captions only). `dimmy_stop_recording` (ffi.rs:1564) is guarded by `STOP_IN_PROGRESS` (concurrent → rc −9), DRAINS streaming/chunked workers BEFORE clearing `audio_buffer` (:1710/:1725, clear at :1741), then route-aware `preprocess_route` (preprocess.rs:685): Raw (preprocessing off — the default since 2026-08-31) / Full-guarded (on + local) / HighpassOnly (on + cloud). A non-empty live transcript is canonical (final paste suppressed); an empty live result falls back to batch STT on the intact buffer (:2001–2005). LLM enhancement is a SEPARATE second FFI call `dimmy_process_with_llm` (ffi.rs:4125) → `process_text` (llm.rs:622); the FFI call degrades gracefully to the raw transcript on any failure. **Paste:** Win `TextInjectionService.PasteText` (SendInput Ctrl+V with BOTH wVk+wScan, phantom-modifier release, focus restore); Mac CGEvent Cmd+V; Linux wtype/ydotool/xdotool. **Command Mode** (App.xaml.cs:2105 `StopAndCommandTransform`): reads selection (UIA snapshot at PRESS, fallback Ctrl+C), stop-transcribes the instruction RAW, calls `dimmy_command_transform` (ffi.rs:4503), pastes the result; on a negative rc it shows the error and pastes nothing.

**Owns.** core/src/ffi.rs:1065–2490 (start/stop, STT dispatch, engine selection, history/telemetry); core/src/capture.rs (capture ownership); core/src/preprocess.rs:665–785 (`preprocess_route` + `process_buffer_guarded`); core/src/deepgram_stream.rs + openai_stream.rs (cloud WS engines); core/src/nemotron.rs (local streaming engine); core/src/chunked_stt.rs (window from `chunk_secs_for_backend`: 5 s for parakeet/whistle with text on screen, else 15 s; + 500ms overlap + suffix-prefix dedup `dedup_last_3_words`, 2–6 words); core/src/llm.rs:622 (`process_text`); core/src/filler.rs (`remove_fillers`); core/src/hotkey.rs; platforms/windows/Dimmy.Windows/Services/TextInjectionService.cs; App.xaml.cs:1958+ (stop/paste/command orchestration); Services/TranscriptionService.cs (2-step stop→LLM); platforms/macos/Dimmy/Managers/TextInjector.swift + HotkeyManager.swift; platforms/linux/src/text_injector.rs.

**Invariants (do NOT change).**
- Dictation captures MIC-ONLY (`AudioSource::Mic`, ffi.rs:1160); routing through Mix/AEC dropped ~60% of audio (measured 2026-07-01). `audio_sample_rate` forced to 48kHz at start (:1137) — the mic callback resamples to 48k; device-native rate → 3× slowdown + hallucinations.
- Streaming/chunked workers MUST drain BEFORE `audio_buffer` is cleared (ffi.rs:1710/:1725, clear at :1741) or the trailing sentence is lost. `stt_chunk` delta is APPEND-ONLY/stable for deepgram + local-stream (interim = empty delta) — the host types it directly.
- Non-empty live transcript is canonical AND final paste is suppressed (`StreamingDictationActive`, App.xaml.cs:2049); empty live result MUST fall back to batch STT on the intact buffer (ffi.rs:2001–2005).
- `STOP_IN_PROGRESS` atomic (ffi.rs:1581): only one stop runs; concurrent callers get rc −9 → host strict no-op.
- Windows synthetic keystrokes MUST set BOTH wVk AND wScan (MapVirtualKey, TextInjectionService.cs:310–334) or Electron/Chrome/IME silently drop paste. PasteText/SendCtrlC MUST release phantom-held modifiers before the chord (:154–179/:358–367) and restore `_targetContext` to foreground before SendInput (App.xaml.cs:2005–2034).
- Deepgram streaming rides tokio-tungstenite native-tls/schannel, NOT rustls (deepgram_stream.rs:24) — rustls reintroduces the 0xc0000409 load crash.
- `dimmy_process_with_llm` degrades gracefully to the RAW transcript on failure (ffi.rs:4334/:4387/:4472). `dimmy_command_transform` does NOT: it returns a negative rc (−2..−5, ffi.rs:4503) and the Windows host (`StopAndCommandTransform`, App.xaml.cs:2105) shows the error and pastes nothing, so a failed command never replaces the selection. `preprocess_route` is the single source of truth: cloud=HighpassOnly, local=Full-guarded, disabled=Raw (preprocess.rs:685); disabled is the default since 2026-08-31 (lib.rs:1225). Command mode uses the spoken instruction RAW (no dictation LLM).

**Config.** `stt_mode`, `streaming_dictation`, `chunk_streaming_enabled`, `local_stt_backend`+`local_model`, Deepgram STT key (gates cloud streaming), `preprocessing_enabled`, `filler_removal_enabled`, `llm_enabled`+`llm_style`+`llm_tone`+`llm_translate_to`+custom prompt+`auth_method`, `language`, `prompt`+`user_dict`, `input_gain`, `save_audio_in_history`, `keep_in_clipboard`, `live_captions`/`LiveCaptionsEnabled`, `auto_recap_threshold_secs`, dedicated command hotkey (CMD binding) + `CommandMode` sticky toggle.

---

## Freeze rule

This is the as-built, known-good baseline for these five subsystems at v0.6.66. Each feature is owned by the cited files; the cited lines were re-pointed to commit `ebc0b52f` on 2026-11-11 (see the status block at the top); the "Invariants" lists are the specific behaviors that other code, tests, and users now depend on — several were paid for in production incidents (the 48kHz forced rate, MIC-ONLY dictation, drain-before-clear, wVk+wScan paste, native-tls Deepgram, if-empty-omit config wipe protection, AES-CTR+HMAC key derivation, the −1..−11 rc table, the 11-section recap contract, `align_secondary`, `join_bounded`, off-main HAL teardown, `TapAutoStart=false`). Treat any change that touches an invariant as a breaking change: it requires a reproduced test, cross-platform parity (Win/Mac/Linux), and explicit sign-off — NOT an incidental refactor. When in doubt whether "it was always like this", this document is the answer of record; if the code no longer matches a citation here, that divergence is the thing to investigate.

---

## See also

- [`audio-pipeline.md`](audio-pipeline.md) — the DSP details behind §1 and §5 (VAD/AGC, route-aware preprocess, the AUDIO-00x invariants).
- [`modules.md`](modules.md) — per-module reference for the files cited above.
- [`known-bugs.md`](known-bugs.md) — the platform traps (macOS 26 CoreAudio HAL wedge, macOS FFI MACOS-00x, audio AUDIO-00x) that these invariants defend against.
- [`system-audio-capture-tests.md`](system-audio-capture-tests.md) — the automated coverage for §1.
- [`testing.md`](testing.md) — test tiers; the unit tests that pin the §1 stop-safety and §3 shortcut invariants.
- Back to the playbook: [`../../CLAUDE.md`](../../CLAUDE.md) (navigation + frozen-release recipe).
