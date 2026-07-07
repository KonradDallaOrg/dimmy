# Known-good baseline (v0.6.66) — as-built behavior + freeze contract

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

**How it works today.** `dimmy_meeting_start` (ffi.rs:3613) refuses a second session (MEETING mutex, :3622) and ALWAYS forces `AudioSource::Mix` (:3634) — the legacy `audio_source` config is ignored for meetings. Mic + system both run at `MEETING_CANONICAL_RATE=48kHz` (audio.rs:24) via cpal-callback resampling; the worker (meeting.rs:581 `worker_loop`) mixes mic+system into `audio(.ogg/.wav)` plus separate `audio_mic`/`audio_system` sinks, transcribes mic and system chunks separately every `chunk_secs` (default 15, clamp 5–60) into `transcripts.txt`, and emits `meeting_chunk` events. A dedicated AEC worker (aec.rs:72) drains 480-sample/10ms frames (HPF→AEC3→NS→AGC2) using loopback as render reference and mic as capture, appending cleaned mic to the primary buffer. **System audio per platform:** Windows = default-output WASAPI loopback (`resolve_loopback_device` audio.rs:1176; `process_loopback.rs` per-process capture is scaffolding — `spawn_process_capture` returns Err at :182); macOS = Core Audio process tap on 14.4+ else ScreenCaptureKit, pushing via `dimmy_push_loopback_audio` (ffi.rs:2806); Linux = same external push path, WAV-only. `dimmy_meeting_stop` (ffi.rs:3774) emits meeting-inactive immediately, then joins the worker via `join_bounded(120s)` (meeting.rs:447) so a wedged HAL yields `TimedOut` instead of a frozen app. Call auto-detect is a pure IO-free state machine (call_detector.rs) fed by host polling `dimmy_call_signal` (~1s).

**Owns.** core/src/meeting.rs (MeetingSession, worker_loop, TrackSink, join_bounded, meta helpers); core/src/ffi.rs:3600–3997 (meeting FFI), :2806–2874 (loopback push), :8952–9260 (call-detector FFI); core/src/audio.rs (Mix capture, loopback resolve/resampler, canonical rate, capture gate); core/src/aec.rs (AEC3 worker); core/src/process_loopback.rs (Windows per-process — enumeration works, capture is Err stub); core/src/call_detector.rs (state machine); platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift (tap + off-thread HAL teardown); platforms/macos/Dimmy/Services/SystemAudioCaptureService.swift (backend selection); platforms/windows/Dimmy.Windows/Services/CallDetectionService.cs; platforms/macos/Dimmy/Services/CallDetectionManager.swift.

**Invariants (do NOT change).**
- Meetings are ALWAYS `AudioSource::Mix`; legacy `audio_source` is ignored (ffi.rs:3634). Both streams run at 48kHz (audio.rs:24).
- `align_secondary` enforces `secondary.len()==primary.len()` each 100ms tick (meeting.rs:778) — the only thing keeping mic and late-waking loopback wall-clock aligned.
- AEC worker never blocks on an empty ref ring — it zero-pads the render frame (aec.rs:154) so mic always flows.
- `join_bounded(120s)` (meeting.rs:447) guarantees `dimmy_meeting_stop` returns even if the worker/HAL wedges.
- macOS: HAL destroy MUST run off-main (SystemAudioProcessTap.swift:595 `teardownQueue`, assert :614); aggregate uses `TapAutoStartKey=false` + explicit `AudioDeviceStart` (:188/:241) or Tahoe yields 0 samples; loopback rate MUST be the ASBD-read rate published before samples flow (SystemAudioCaptureService.swift:255/:160).
- `MEETING_CAPTURE_GATED` gates capture appends while paused and is always cleared on start/stop (audio.rs:53, meeting.rs:402). `.recording` marker written at start, removed only on clean stop (meeting.rs:305/:483) — drives crash recovery.
- Windows has NO per-process loopback: `process_loopback::spawn_process_capture` is an Err stub — only default-output WASAPI loopback works.

**Config.** `meeting_chunk_secs` (5–60, default 15), `stt_mode`, `api_url/api_model/api_key`, `local_stt_backend` (whisper|parakeet), `local_model`, `language`, `prompt`+`user_dict`, `selected_device`; call-detector: `enabled`, `min_active_secs`(1), `cooldown_secs`(1800); cargo feature `local-dfn` (DFN3 vs nnnoiseless); Ogg/Vorbis gate = Windows+macOS only (Linux WAV); `MEETING_CANONICAL_RATE=48000` compile-time const.

---

## 2. Automatic meeting recap (transcript → structured LLM recap → recap.md/actions.json, independent auth/model dispatch, Notion/folder export)

**How it works today.** After `dimmy_meeting_stop` returns `{dir, transcript}`, recap fires if the per-meeting "Generate recap" toggle is on (`MeetingGenerateRecap`/`generateRecap`, default true). `BuildStructuredRecapPrompt` (Win MeetingRecapHelpers.cs:198; Mac MeetingPostProcessService.swift:359) builds a fixed prompt demanding line-1 `# Title` H1, an invisible `<!-- dimmy-type: KEY -->` tag, and EXACTLY 11 `===NAME===` sections. Host calls `dimmy_llm_call_raw(prompt, modelOverride, maxTokens, buf, buf_len)` (ffi.rs:5107) → `crate::llm::process_raw_prompt` (llm.rs:1002). Recap auth (`recap_auth_method`) is orthogonal to dictation auth: empty ⇒ `api_key`, NEVER inherits `subscription` (ffi.rs:5156–5165). Recap vendor is derived from the picked model id, key resolved cross-scope (ffi.rs:5262–5375). On rc>0 the host runs `ParseStructuredRecap` → `BuildMarkdownFromSections` → `dimmy_meeting_save_post_process(dir, recapMd, actionsPlain, null)` (ffi.rs:3853 → meeting.rs:1326) which writes `recap.md` + `actions.json` and syncs the title into meta.json. **Platform split (real divergence):** Windows in-window Stop uses `MeetingWindow.GeneratePostProcessAsync` (MeetingWindow.xaml.cs:508/1351) — inline, NO notes.md, NO Notion; Windows pill/hotkey Stop uses the shared `MeetingPostProcessService.RunRecapAsync` (reads notes.md, exports, Notion-auto-sends). macOS routes BOTH in-window and pill stops through the single shared `MeetingPostProcessService.runRecap`.

**Owns.** core/src/ffi.rs:5107 (`dimmy_llm_call_raw` — auth+model routing, key resolution), :5509 (`categorize_llm_error_to_rc` — the −1..−11 contract), :3853 (`dimmy_meeting_save_post_process`); core/src/llm.rs:1002 (`process_raw_prompt` — CLI/HTTP branches, truncation guard at :1142); core/src/meeting.rs:1326 (`save_post_process`); platforms/windows/Dimmy.Windows/Helpers/MeetingRecapHelpers.cs (prompt/11-section contract/parser/rc→message); Services/MeetingPostProcessService.cs (shared Win pipeline + Notion); Views/MeetingWindow.xaml.cs:1351/1663 (in-window path + `PickRecapModel`); Services/RecapExportService.cs (folder export); platforms/macos/Dimmy/Services/MeetingPostProcessService.swift; Utilities/RecapModel.swift; Controllers/PillWindowController.swift:385.

**Invariants (do NOT change).**
- The 11 `===NAME===` markers are a wire contract shared verbatim by prompt builder, parser, and renderer across Win C# and Mac Swift (MeetingRecapHelpers.cs:39 == MeetingPostProcessService.swift:187). Rename in one → silent round-trip break.
- `recap_auth_method` is INDEPENDENT of dictation `llm_auth_method`; empty ⇒ `api_key`, NEVER inherits `subscription` (ffi.rs:5156–5165). Subscription routes ONLY to `claude`/`codex` CLI; a non-Claude/non-OpenAI model under subscription MUST fall back to `api_key` (ffi.rs:5382–5398).
- Recap vendor is DERIVED from model id, not a config field (ffi.rs:5262–5267). Auto model-pick reads the LIVE `dimmy_get_config_json` snapshot, NOT config.json file (RecapModel.swift:33 / MeetingWindow.xaml.cs:1675).
- The −1..−11 rc table is shared 3 ways and must stay in sync: ffi.rs:5509, Win `RecapRcToUserMessage` (MeetingRecapHelpers.cs:637), Mac `Failure.description`/DimmyCore+V2.swift:325. Only the numeric rc / categorical `&static str` crosses FFI on error — the HTTP body (echoes transcript) is stripped (ffi.rs:5462–5477).
- First output line must be `# Title` H1 → `parse_recap_title` (meeting.rs:1364) writes meta.json. Truncation guard returns `Truncated` (rc −10) after ONE 4×-headroom retry (llm.rs:1142–1159) — never save a partial recap. 600s CLI/HTTP timeout (llm.rs:1133).
- Notion auto-send + folder export are best-effort, never throw/block (MeetingPostProcessService.cs:123–147; RecapExportService.cs try/catch). Obsidian = folder export; there is no separate Obsidian integration.

**Config.** `recap_auth_method` (empty⇒api_key; subscription→claude/codex CLI), `recap_model_override` (empty=Auto; `cloud:<id>`; `local:<file.gguf>`), `recap_use_same_key` (default true), `llm_mode`, `llm_api_url/model/key`, `local_llm_model`, `notion_auto_send`, `UiPreferences.RecapExportFolder` / UserDefaults `recapExportFolder`, `MeetingGenerateRecap`/`generateRecap`, `meetingType` taxonomy key.

---

## 3. Global shortcuts (dictation / command-mode / meeting)

**How it works today.** Three independent bindings run on ONE OS keyboard hook per platform. In Rust (hotkey.rs) they are statics DICT (:194), CMD (:196), MTNG (:201); each `Binding` (:33) holds packed L/R VK codes + down-flags + `combo_active` + a one-slot event mailbox. `Binding::process` (:103) is pure logic — PRESSED fires only on the false→true swap of `combo_active`, RELEASED only on true→false (idempotent under auto-repeat). **Windows** is the real hook: `install_hook` (:1043) installs `WH_KEYBOARD_LL`; `keyboard_proc` (:974) feeds every physical key to all three bindings; on a Pressed transition it sets `MODIFIER_SUPPRESS` and calls `emit_synthetic_combo_release` (:841) — injecting synthetic KEYUPs (and a `VK_NONAME` chord-buster :735 when WIN is a modifier) so the shell never opens Start Menu; suppression clears only when NO binding is `combo_active` AND all three report all_released (:1025–1033). C# `HotkeyService.cs` polls the mailboxes on a 10ms thread; toggle-vs-hold is decided HOST-SIDE (`PttMode`, config `shortcut_mode=="hold"`). **macOS** does NOT use the Rust hook (`dimmy_hotkey_install` never called; Rust macOS CGEventTap is dead code and omits MTNG). Swift `HotkeyManager.swift` owns a `.cgSessionEventTap`; `CommandComboState` (:985) ports the Rust Binding for command+meeting. **Linux is a no-op stub** (hotkey.rs:1429, platforms/linux/src/hotkey.rs:33 unimplemented) — shortcuts non-functional. `combos_conflict` (:534) = subset relation; hosts validate before binding.

**Owns.** core/src/hotkey.rs:33 (Binding + process()), :194 (DICT/CMD/MTNG), :456/:484/:493 (set_*), :534 (combos_conflict), :667 (Windows mod: keyboard_proc :974, install_hook :1043, emit_synthetic_combo_release :841, VK_NONAME :735), :1062 (macOS mod — UNUSED on shipping), :1429 (Linux stub); core/src/ffi.rs:6271–6355 & 7605–7694 (hotkey FFI); platforms/windows/Dimmy.Windows/Services/HotkeyService.cs; App.xaml.cs:364/621/650–659; Services/UiPreferences.cs:77/86/94; Views/SettingsWindow.xaml.cs:271–293 (conflict validation); platforms/macos/Dimmy/Managers/HotkeyManager.swift (real macOS owner); State/AppState.swift:818/901/1265/1282; platforms/linux/src/hotkey.rs (stub).

**Invariants (do NOT change).**
- Three SEPARATE statics with independent state — prevents cross-firing (hotkey.rs:33/194–201). Event fed to ALL three bindings, each with its own event slot (:1009–1020).
- PRESSED/RELEASED fire only on the `combo_active` edge via swap (:119/:126/:179/:182) — makes auto-repeat idempotent.
- `MODIFIER_SUPPRESS` clears ONLY when no binding is combo_active AND all three all_released (:1025–1033); injected events (`LLKHF_INJECTED`) MUST bypass the state machine (:985); suppression is scoped to combo keys so Win+E/Win+L still reach the OS.
- `VK_NONAME` chord-buster is required specifically when WIN is a modifier (:735/:846–850) — Alt/Ctrl/Shift don't count.
- Toggle-vs-hold is a HOST decision (core only emits edges): Windows `PttMode`/`shortcut_mode`, macOS `preferredMode`. Meeting hotkey is TOGGLE-ONLY on every surface — host acts on pressed, ignores released (HotkeyService.cs:138–143, HotkeyManager.swift:400–409).
- Dictation binding NEVER disables (falls back to default); command & meeting DO disable on empty combo. macOS correctness depends on `dimmy_hotkey_install` NOT being called there.
- Command/meeting combos live in host-local prefs (ui_prefs.json / UserDefaults), NOT AppConfig — `lib.rs` has no command/meeting field.

**Config.** config.json `shortcut` (dictation; core-owned; default win+alt / cmd+option), `shortcut_mode` ('hold'|'toggle'); Windows ui_prefs.json `CommandHotkey` (default '' disabled), `MeetingHotkey` (host default 'ctrl+alt+m' — core MTNG starts empty), `DictHotkey` (add-to-dictionary, 'ctrl+shift+d'); macOS UserDefaults `shortcutEncoded`/`commandHotkeyEncoded`/`meetingHotkeyEncoded`/`preferredMode`.

---

## 4. API Keys + Providers (STT / LLM / recap storage + resolution)

**How it works today.** All keys live in ONE encrypted file `keys.enc` in the config dir (keystore.rs; path `config_dir_path().join("keys.enc")` :502) — a JSON map of entry-name → base64 blob. **Encryption is AES-256-CTR + HMAC-SHA256 (encrypt-then-MAC), NOT GCM** (keystore.rs:194–306; module doc line 1 says so explicitly). Per-entry: `base64(nonce[12] || ciphertext || mac[32])`; MAC verified constant-time BEFORE decrypt (:299). Master key = `SHA-256(username:hostname:dimmy-local-key-v1)` (:19–37), split into enc/mac subkeys (:248). SHA-256/HMAC/AES are hand-rolled in-file, NIST-validated. OS keyring is READ-ONLY migration fallback only; `use_keyring` forced false. Entries namespaced by `KeyringScope` (provider.rs:265–311): `Stt→api-key-<v>`, `Llm→llm-key-<v>`, `Recap→recap-key-<v>`, `NotionToken→integration-notion-token`. `Provider` enum detects vendor from URL/model-id/tag. STT key resolves cross-scope within the SAME vendor (STT→LLM→Recap, ffi.rs:122–162); LLM/recap resolve Llm(vendor)→Stt(vendor). The unified Providers card saves ONE key into EVERY capable scope (SettingsWindow.Providers.cs:342–368; Mac MacProvidersPage.swift:365–429), validated against vendor capability by `dimmy_save_llm_provider_key` (ffi.rs:4751–4818). Subscription URLs `claude-code://` and `codex://` skip key resolution entirely.

**Owns.** core/src/keystore.rs (encryption, machine key, save/load/has_key/migrate); core/src/provider.rs (Provider + KeyringScope + capability gates); core/src/ffi.rs (STT/LLM/recap resolution, `load_key_any_scope`, config setter, `dimmy_save_llm_provider_key`); core/src/lib.rs (AppConfig identity fields, `save_config_file` single-writer); core/src/claude_code.rs + codex.rs (synthetic URLs); platforms/windows/Dimmy.Windows/Views/SettingsWindow.Providers.cs; ViewModels/SettingsViewModel.cs (ToJson if-empty-omit); Services/ProviderCatalog.cs (KeySaveScopes); platforms/macos/Dimmy/Views/Settings/MacProvidersPage.swift.

**Invariants (do NOT change).**
- AES-256-CTR + HMAC-SHA256, MAC-verified-before-decrypt (keystore.rs:194–306/:299) — do NOT "upgrade" to GCM assumptions.
- Master key = `SHA-256(username:hostname:dimmy-local-key-v1)` (:19–37), machine-bound. Changing the derivation string, subkey tags, blob layout, or username/hostname source makes every existing `keys.enc` undecryptable (silent total key loss — only a WARNING logged, :529).
- `KeyringScope::entry_name()` strings and `Provider::as_str()` vendor tags are the ON-DISK keys — renaming orphans stored keys. Empty-string save DELETES the entry; `has_key` treats stored-empty as absent (:606–611/:646–649).
- Cross-scope fallback stays within the SAME vendor only (ffi.rs:3268–3289). Config setter applies identity fields ONLY when the JSON key is present (ffi.rs:2026/2137); host ToJson OMITS empty identity/credential fields (SettingsViewModel.cs:786–819) — together they prevent a transient empty VM from wiping saved URLs/keys (the 2026-05-16 incident).
- Single-writer: only Rust writes config.json (`save_config_file`, lib.rs:972); UIs go through `dimmy_set_config_json`.
- Subscription detection depends on exact schemes `claude-code://` / `codex://` (claude_code.rs:986, codex.rs:656) and must be checked BEFORE key resolution. `dimmy_save_llm_provider_key` enforces scope-vs-vendor capability (ffi.rs:4784–4795).

**Config.** `api_url/api_model`, `llm_api_url/llm_api_model` (also holds synthetic URLs), `local_model`/`local_llm_model`, `selected_device`, `llm_auth_method`, `recap_auth_method`, `llm_use_same_key`/`recap_use_same_key`, `recap_model_override`, `llm_enabled`/`llm_mode`, `use_keyring` (forced false), `meeting_storage_path`, `notion_target_id/kind/title` (token stored as `KeyringScope::NotionToken`).

---

## 5. Dictation + Paste (hotkey → capture → STT → LLM → paste), Streaming dictation, Command Mode

**How it works today.** `dimmy_start_recording` (ffi.rs:710) refuses if meeting active (rc −7), already recording (rc −2), or cloud-mode with no key (rc −1); forces `audio_sample_rate=48kHz` (:761); sends `AudioCommand::Start` with `AudioSource::Mic` (:784) — **dictation is MIC-ONLY** (Mix/AEC dropped ~60% of samples). Engine selection (:829): streaming+cloud+Deepgram-key → `DeepgramStreamer`; streaming+local → `ChunkedTranscriber` typing mode ("local-stream"); chunk_streaming+local → caption mode; else "batch". Streaming engines emit a `(delta, cumulative, is_final)` contract via `stt_chunk` events; `is_final` exactly ONCE at stop-drain. Host injects stable deltas at the cursor via SendInput unicode (Win AppViewModel.cs:382 → App.xaml.cs:1539; Mac captions only). `dimmy_stop_recording` (ffi.rs:960) is guarded by `STOP_IN_PROGRESS` (concurrent → rc −9), DRAINS streaming/chunked workers BEFORE clearing `audio_buffer` (:1064/:1079), then route-aware `preprocess_route` (preprocess.rs:541): Raw / Full-guarded (local) / HighpassOnly (cloud). A non-empty live transcript is canonical (final paste suppressed); an empty live result falls back to batch STT on the intact buffer (:1319). LLM enhancement is a SEPARATE second FFI call `dimmy_process_with_llm` → `process_text` (llm.rs:562), which degrades gracefully to raw transcript on any failure. **Paste:** Win `TextInjectionService.PasteText` (SendInput Ctrl+V with BOTH wVk+wScan, phantom-modifier release, focus restore); Mac CGEvent Cmd+V; Linux wtype/ydotool/xdotool. **Command Mode** (App.xaml.cs:2014): reads selection (UIA snapshot at PRESS, fallback Ctrl+C), stop-transcribes the instruction RAW, calls `dimmy_command_transform` (ffi.rs:3402), pastes the result.

**Owns.** core/src/ffi.rs:710–1690 (start/stop, STT dispatch, engine selection, history/telemetry); core/src/preprocess.rs:521–641 (`preprocess_route` + `process_buffer_guarded`); core/src/deepgram_stream.rs (WS engine); core/src/chunked_stt.rs (3s window + 500ms overlap + last-3-words dedup); core/src/llm.rs:562 (`process_text`); core/src/filler.rs (`remove_fillers`); core/src/hotkey.rs; platforms/windows/Dimmy.Windows/Services/TextInjectionService.cs; App.xaml.cs:1884+ (stop/paste/command orchestration); Services/TranscriptionService.cs (2-step stop→LLM); platforms/macos/Dimmy/Managers/TextInjector.swift + HotkeyManager.swift; platforms/linux/src/text_injector.rs.

**Invariants (do NOT change).**
- Dictation captures MIC-ONLY (`AudioSource::Mic`, ffi.rs:784); routing through Mix/AEC drops ~60% of audio. `audio_sample_rate` forced to 48kHz at start (:761) — the mic callback resamples to 48k; device-native rate → 3× slowdown + hallucinations.
- Streaming/chunked workers MUST drain BEFORE `audio_buffer` is cleared (ffi.rs:1064/:1079) or the trailing sentence is lost. `stt_chunk` delta is APPEND-ONLY/stable for deepgram + local-stream (interim = empty delta) — the host types it directly.
- Non-empty live transcript is canonical AND final paste is suppressed (`StreamingDictationActive`, App.xaml.cs:1970); empty live result MUST fall back to batch STT on the intact buffer (ffi.rs:1319).
- `STOP_IN_PROGRESS` atomic (ffi.rs:977): only one stop runs; concurrent callers get rc −9 → host strict no-op.
- Windows synthetic keystrokes MUST set BOTH wVk AND wScan (MapVirtualKey, TextInjectionService.cs:297) or Electron/Chrome/IME silently drop paste. PasteText/SendCtrlC MUST release phantom-held modifiers before the chord (:161/:337) and restore `_targetContext` to foreground before SendInput (App.xaml.cs:1925).
- Deepgram streaming rides tokio-tungstenite native-tls/schannel, NOT rustls (deepgram_stream.rs:24) — rustls reintroduces the 0xc0000409 load crash.
- `process_text` / `dimmy_command_transform` degrade gracefully to the RAW transcript on failure (llm.rs / ffi.rs:3372). `preprocess_route` is the single source of truth: cloud=HighpassOnly, local=Full-guarded, disabled=Raw (preprocess.rs:541). Command mode uses the spoken instruction RAW (no dictation LLM).

**Config.** `stt_mode`, `streaming_dictation`, `chunk_streaming_enabled`, `local_stt_backend`+`local_model`, Deepgram STT key (gates cloud streaming), `preprocessing_enabled`, `filler_removal_enabled`, `llm_enabled`+`llm_style`+`llm_tone`+`llm_translate_to`+custom prompt+`auth_method`, `language`, `prompt`+`user_dict`, `input_gain`, `save_audio_in_history`, `keep_in_clipboard`, `live_captions`/`LiveCaptionsEnabled`, `auto_recap_threshold_secs`, dedicated command hotkey (CMD binding) + `CommandMode` sticky toggle.

---

## Freeze rule

This is the as-built, known-good baseline for these five subsystems at v0.6.66. Each feature is owned by the cited files at the cited lines; the "Invariants" lists are the specific behaviors that other code, tests, and users now depend on — several were paid for in production incidents (the 48kHz forced rate, MIC-ONLY dictation, drain-before-clear, wVk+wScan paste, native-tls Deepgram, if-empty-omit config wipe protection, AES-CTR+HMAC key derivation, the −1..−11 rc table, the 11-section recap contract, `align_secondary`, `join_bounded`, off-main HAL teardown, `TapAutoStart=false`). Treat any change that touches an invariant as a breaking change: it requires a reproduced test, cross-platform parity (Win/Mac/Linux), and explicit sign-off — NOT an incidental refactor. When in doubt whether "it was always like this", this document is the answer of record; if the code no longer matches a citation here, that divergence is the thing to investigate.

---

## See also

- [`audio-pipeline.md`](audio-pipeline.md) — the DSP details behind §1 and §5 (VAD/AGC, route-aware preprocess, the AUDIO-00x invariants).
- [`modules.md`](modules.md) — per-module reference for the files cited above.
- [`known-bugs.md`](known-bugs.md) — the platform traps (macOS 26 CoreAudio HAL wedge, macOS FFI MACOS-00x, audio AUDIO-00x) that these invariants defend against.
- [`system-audio-capture-tests.md`](system-audio-capture-tests.md) — the automated coverage for §1.
- [`testing.md`](testing.md) — test tiers; the unit tests that pin the §1 stop-safety and §3 shortcut invariants.
- Back to the playbook: [`../../CLAUDE.md`](../../CLAUDE.md) (navigation + frozen-release recipe).
