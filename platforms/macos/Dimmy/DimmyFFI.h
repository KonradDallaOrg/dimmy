//
//  DimmyFFI.h
//  Dimmy
//
//  C bridging header for the Rust FFI layer (ffi.rs).
//  All functions are defined in libdimmy_lib.a.
//

#ifndef DimmyFFI_h
#define DimmyFFI_h

#include <stdint.h>

// ── Lifecycle ───────────────────────────────────────────────────────

/// Initialize the Dimmy core. Must be called once before any other function.
/// Returns 0 on success, -1 on error.
int32_t dimmy_init(void);

/// Shut down: stop audio, save config and clean up.
void dimmy_shutdown(void);

/// Register event callback. The native UI provides a function pointer that
/// receives JSON strings for events (recording_progress, chunk_status, etc.).
void dimmy_set_event_callback(void (*_Nonnull cb)(const char * _Nonnull));

// ── Recording ───────────────────────────────────────────────────────

/// Start recording. Returns 0=OK, -1=no API key, -2=already recording.
int32_t dimmy_start_recording(void);

/// Stop recording and get transcript. Returns transcript length, or negative on error.
/// Transcript is written to out_buf (null-terminated).
int32_t dimmy_stop_recording(char * _Nonnull out_buf, int32_t buf_len);

/// Cancel recording without transcribing.
void dimmy_cancel_recording(void);

// ── Config ──────────────────────────────────────────────────────────

/// Get full config as JSON string. Returns length written, or -1 on error.
int32_t dimmy_get_config_json(char * _Nonnull out_buf, int32_t buf_len);

/// Set config from JSON string. Returns 0=OK, -1=error.
int32_t dimmy_set_config_json(const char * _Nonnull json_ptr);

// ── Audio ───────────────────────────────────────────────────────────

/// Get current microphone amplitude (0.0 - 1.0).
float dimmy_get_amplitude(void);

/// Peak amplitude of the secondary (loopback / system audio) buffer.
/// 0.0 when no Mix-mode meeting is active or the buffer is empty.
/// Used by the meeting-window dual-band waveform to render mic + system
/// as two stacked bands so the user sees both streams at a glance.
float dimmy_get_loopback_amplitude(void);

/// Get device list as JSON array. Returns length written, or -1 on error.
int32_t dimmy_list_devices_json(char * _Nonnull out_buf, int32_t buf_len);

/// Check audio device health. Returns JSON with diagnostic info.
int32_t dimmy_check_audio_health(char * _Nonnull out_buf, int32_t buf_len);

// ── LLM ─────────────────────────────────────────────────────────────

/// Cycle LLM style. direction: +1 = next, -1 = previous.
void dimmy_cycle_llm_style(int32_t direction);

/// Cycle LLM tone. direction: +1 = next, -1 = previous.
void dimmy_cycle_llm_tone(int32_t direction);

/// Process text through LLM enhancement. Returns length written to buffer,
/// or -1 on error, 0 if LLM disabled/style=Off.
int32_t dimmy_process_with_llm(const char * _Nonnull text_ptr,
                               char * _Nonnull out_buf,
                               int32_t buf_len);

/// Command Mode: transform (or replace) the user's selected text using
/// their spoken instruction. Returns bytes written (>0) on success, or
/// -1 invalid input, -2 dispatch failed, -3 no LLM key, -4 local model
/// missing, -5 runtime create failed.
int32_t dimmy_command_transform(const char * _Nonnull selection_ptr,
                                const char * _Nonnull spoken_ptr,
                                char * _Nonnull out_buf,
                                int32_t buf_len);

// ── Stats ───────────────────────────────────────────────────────────

/// Update cumulative stats. Returns 0=OK, -1=invalid input.
int32_t dimmy_update_stats(int32_t words, double speaking_secs);

// ── Utility ─────────────────────────────────────────────────────────

/// Check if an API key is configured. Returns 1=yes, 0=no.
int32_t dimmy_has_api_key(void);

/// Check if recording is active. Returns 1=yes, 0=no.
int32_t dimmy_is_recording(void);

// ── Local STT ──────────────────────────────────────────────────────

/// Get JSON array of available local models with download status.
/// Returns bytes written, or -1 on error.
int32_t dimmy_list_local_models(char * _Nonnull out_buf, int32_t buf_len);

/// Download a model file. BLOCKING — call from a background thread.
/// Emits "model_download_progress" events via the event callback.
/// Returns 0 on success, -1 on error.
int32_t dimmy_download_model(const char * _Nonnull filename);

/// Check if a specific model file exists locally.
/// Returns 1=yes, 0=no.
int32_t dimmy_model_exists(const char * _Nonnull filename);

// ── Parakeet TDT v3 FP32 (alternative local STT backend) ────────────

/// 1 = the Parakeet FP32 bundle (~2.5 GB) is fully on disk, 0 otherwise.
int32_t dimmy_parakeet_bundle_present(void);

/// Download the Parakeet bundle into the dimmy config dir. BLOCKING —
/// call from a background thread. Emits "parakeet_bundle_download_progress"
/// events with {downloaded,total}. Returns 0=OK, -1=error.
int32_t dimmy_parakeet_download_bundle(void);

/// Direct PCM → text via Parakeet (used by tests / smoke; the live STT
/// path goes through dimmy_stop_recording()). Returns bytes written, or -1.
int32_t dimmy_parakeet_transcribe(const float * _Nullable pcm_ptr,
                                  int32_t pcm_len,
                                  char * _Nonnull out_buf,
                                  int32_t buf_len);

/// Pre-load the Parakeet sessions + run a tiny dummy inference so the
/// user's first real recording skips the ~6 s cold path. BLOCKING —
/// call from a background thread. Returns 0 on success, -1 on error
/// (most commonly "bundle not present" — caller should guard).
int32_t dimmy_parakeet_warmup(void);

// ── Local LLM ────────────────────────────────────────────────────────

/// Get JSON array of available local LLM models with download status.
int32_t dimmy_list_llm_models(char * _Nonnull out_buf, int32_t buf_len);

/// Download an LLM model file. BLOCKING — call from background thread.
int32_t dimmy_download_llm_model(const char * _Nonnull filename);

/// Check if a specific LLM model file exists locally. Returns 1=yes, 0=no.
int32_t dimmy_llm_model_exists(const char * _Nonnull filename);

// ── Transcription History ──────────────────────────────────────────

/// Save a transcript to history. Returns transcript ID, or -1 on error.
int32_t dimmy_history_save(const char * _Nonnull text,
                           const char * _Nullable language,
                           double duration);

/// Get recent transcripts as JSON array. Returns bytes written, or -1 on error.
int32_t dimmy_history_recent(int32_t limit,
                             char * _Nonnull out_buf,
                             int32_t buf_len);

/// Search transcripts via full-text search. Returns bytes written, or -1 on error.
int32_t dimmy_history_search(const char * _Nonnull query,
                             int32_t limit,
                             char * _Nonnull out_buf,
                             int32_t buf_len);

/// Delete a transcript by ID. Returns 0=OK, -1=error.
int32_t dimmy_history_delete(int32_t id);

/// Get history stats as JSON. Returns bytes written, or -1 on error.
int32_t dimmy_history_stats(char * _Nonnull out_buf, int32_t buf_len);

// ── Telemetry ──────────────────────────────────────────────────────

/// Submit user feedback through the telemetry pipeline. Best-effort:
/// returns 0 on success (queued) or empty input, -1 on precondition error.
/// Empty messages are silent no-ops.
int32_t dimmy_telemetry_capture_feedback(const char * _Nullable kind,
                                         const char * _Nullable message,
                                         const char * _Nullable email);

/// 1 = analytics on, 0 = off. Reflects the user toggle (PostHog).
int32_t dimmy_telemetry_is_enabled(void);
int32_t dimmy_telemetry_set_enabled(int32_t enabled);

/// 1 = crash reporting on, 0 = off. Reflects the user toggle (Sentry).
int32_t dimmy_telemetry_is_crash_enabled(void);
int32_t dimmy_telemetry_set_crash_enabled(int32_t enabled);

/// Anonymous distinct_id used by PostHog. Returns bytes written, or -1.
int32_t dimmy_telemetry_anonymous_id(char * _Nonnull out_buf, int32_t buf_len);

/// Wipe + regenerate the anonymous_id (used by "Reset analytics ID").
int32_t dimmy_telemetry_reset_anonymous_id(void);

/// Full telemetry status as JSON. Returns bytes written, or -1.
int32_t dimmy_telemetry_status(char * _Nonnull out_buf, int32_t buf_len);

// ── Autostart ──────────────────────────────────────────────────────

int32_t dimmy_autostart_is_enabled(void);
int32_t dimmy_autostart_set_enabled(int32_t enabled);

// ── Diagnostics / GPU ──────────────────────────────────────────────

/// Probe default input device + record a few ms of audio to flag issues.
/// Returns bytes written to a JSON status report, or -1 on error.
int32_t dimmy_check_audio_health(char * _Nonnull out_buf, int32_t buf_len);

/// Build version (CARGO_PKG_VERSION). Returns bytes written, or -1.
int32_t dimmy_get_version(char * _Nonnull out_buf, int32_t buf_len);

/// Build flavor — "" (prod) or "staging". Embedded at compile time
/// via DIMMY_BUILD_FLAVOR. UIs surface a "STAGING" watermark on
/// non-prod flavors. Returns bytes written, or -1 on null buffer.
int32_t dimmy_build_flavor(char * _Nonnull out_buf, int32_t buf_len);

/// Config-dir name as embedded at build time via DIMMY_CONFIG_NAMESPACE
/// (default "dimmy"). DECOUPLED from build flavor since 2026-05-16 — a
/// flavor=staging build that ships under the prod packId keeps the prod
/// config dir. Native UIs MUST use this to locate config.json / meetings
/// / history; deriving the dir from the flavor is the 2026-05-16
/// onboarding-restart bug. Returns bytes written, or -1.
int32_t dimmy_config_dir_name(char * _Nonnull out_buf, int32_t buf_len);

/// Effective meetings directory — honours the user's `meeting_storage_path`
/// override (with a writability fallback to the default
/// `<configdir>/meetings`). Resolve fresh on each call; the user can change
/// it at runtime. Returns bytes written, or -1.
int32_t dimmy_meetings_dir(char * _Nonnull out_buf, int32_t buf_len);

/// Embedded model catalog as a JSON string (single source of truth for the
/// cloud models offered per provider). Returns the TOTAL byte length of the
/// catalog JSON, always — pass out_buf=NULL to query the length, and if the
/// return value is >= buf_len the content was truncated (allocate return+1
/// and call again).
int32_t dimmy_model_catalog_json(char * _Nullable out_buf, int32_t buf_len);

/// GPU known-bad marker status as JSON. Returns bytes written, or -1.
int32_t dimmy_gpu_get_status(char * _Nonnull out_buf, int32_t buf_len);

/// Clear the known-bad GPU marker so we re-probe Metal next launch.
int32_t dimmy_gpu_clear_known_bad(void);

// ── Licensing ──────────────────────────────────────────────────────

// dimmy_license_set_server_url removed from the public ABI. The Rust
// implementation is now gated behind cfg(debug_assertions) and the
// Mac Settings UI that called it has been deleted. Release dylibs
// embed the URL via DIMMY_LICENSE_SERVER_URL at compile time.

/// Current license status as JSON. Schema:
///   { kind, tier?, days_remaining?, days_offline?, error?,
///     cloud_enabled, updates_enabled, scopes: [...] }
int32_t dimmy_license_status_json(char * _Nonnull out_buf, int32_t buf_len);

/// POST /api/trial/start { email }. Writes JSON {ok, magic_link?, error?}.
int32_t dimmy_license_request_trial(const char * _Nonnull email,
                                    char * _Nonnull out_buf,
                                    int32_t buf_len);

/// GET /api/activate?code=...&device_label=... — on success persists the
/// returned token to ~/.config/dimmy/license.json.
int32_t dimmy_license_redeem(const char * _Nonnull code,
                             const char * _Nullable device_label,
                             char * _Nonnull out_buf,
                             int32_t buf_len);

/// POST /api/refresh — bumps last_seen + rotates token.
int32_t dimmy_license_refresh(char * _Nonnull out_buf, int32_t buf_len);

/// Delete the on-disk license file. Useful for "Sign out". Returns 0=ok.
int32_t dimmy_license_clear(void);

/// Capability check: 1=scope present, 0=denied, -1=null input.
int32_t dimmy_license_has_scope(const char * _Nonnull scope_name);

/// POST /api/devices/list — JSON {ok, license_id, tier, max_devices, devices: [...]}.
int32_t dimmy_license_devices_list(char * _Nonnull out_buf, int32_t buf_len);

/// POST /api/devices/deactivate { device_id? }. Pass NULL to self-sign-out.
int32_t dimmy_license_device_deactivate(const char * _Nullable device_id,
                                        char * _Nonnull out_buf,
                                        int32_t buf_len);

/// POST /api/checkout/create — returns Stripe Checkout URL (or 409
/// with current_tier when the email already has an active license).
/// `tier` must be "monthly" | "annual" | "lifetime".
/// `email` is optional (NULL = anonymous / token-authenticated path).
int32_t dimmy_license_checkout_url(const char * _Nonnull tier,
                                   const char * _Nullable email,
                                   char * _Nonnull out_buf,
                                   int32_t buf_len);

/// POST /api/billing-portal — returns Stripe Customer Portal URL.
/// Only valid for paid licenses.
int32_t dimmy_license_billing_portal_url(char * _Nonnull out_buf, int32_t buf_len);

/// POST /api/plan-change — switch monthly⇄annual via Stripe subscription
/// update API (proration handled server-side). Reject path: lifetime tier
/// or first-purchase. After success, callers should call dimmy_license_refresh
/// to pick up the new tier in the local token.
int32_t dimmy_license_plan_change(const char * _Nonnull new_tier,
                                  char * _Nonnull out_buf,
                                  int32_t buf_len);

// ── App context (foreground-app rules) ───────────────────────────

/// Push the foreground-app snapshot captured at hotkey-down. JSON shape:
///   {"process_name":"","bundle_id":"com.tinyspeck.slackmacgap","wm_class":""}
/// Returns 0 on success, non-zero on parse error.
int32_t dimmy_set_app_context(const char * _Nullable json_ptr);

/// Clear the foreground-app snapshot. Call after transcription completes
/// so a stale snapshot can't bleed into the next recording.
void dimmy_clear_app_context(void);

// ── Audio file load ──────────────────────────────────────────────

/// Synchronously transcribe a WAV file via the active local STT
/// backend. Returns transcript length on success, negative on error.
/// Emits `file_transcribe_progress` events between chunks. Cloud mode
/// is unsupported via this entry point (returns -4).
int32_t dimmy_transcribe_file(const char * _Nonnull path_ptr,
                              char * _Nonnull out_buf,
                              int32_t buf_len);

// ── Meeting mode ─────────────────────────────────────────────────

/// Start a meeting recording. Returns the session id length on success;
/// -1 already-active or unable to lock; -3 audio/session start failure.
int32_t dimmy_meeting_start(char * _Nonnull out_buf, int32_t buf_len);

/// Push externally-captured system-audio samples (macOS ScreenCaptureKit path).
/// Forwards to audio_buffer_secondary and aec_ref_ring in the Rust worker.
/// Returns 0 on success, -1 on error.
int32_t dimmy_push_loopback_audio(const float * _Nonnull samples, int32_t count, int32_t sample_rate);

/// Sample rate (Hz) the cpal mic stream is currently running at, or 0
/// when no recording is in flight. Used by SystemAudioCaptureService
/// to match SCStream's rate to the mic's native rate (avoids macOS
/// audio HAL renegotiation during meetings).
int32_t dimmy_get_active_mic_sample_rate(void);

/// Device-native sample rate of the cpal mic (Hz), or 0 when no recording
/// is in flight. Use this in preference to `dimmy_get_active_mic_sample_rate`
/// when sizing an external loopback path (e.g. SCStream) so the wire rate
/// matches the mic's actual rate — not the canonical post-resample 48 kHz.
int32_t dimmy_get_active_mic_device_rate(void);

/// Probed sample rate cpal would open the configured mic at (no stream
/// opened). Use this when `dimmy_get_active_mic_sample_rate` returns 0
/// because cpal hasn't started yet — typical for macOS SCStream config
/// that runs concurrently with the first dimmy_meeting_start.
int32_t dimmy_probe_primary_sample_rate(void);

/// Tell the Rust core which rate the next dimmy_push_loopback_audio
/// stream will run at. Called by SystemAudioCaptureService BEFORE
/// dimmy_meeting_start so audio_system.wav is opened with the correct
/// header. Pass 0 to clear. Returns 0 on success, -1 on out-of-range.
int32_t dimmy_set_loopback_sample_rate(int32_t rate);

/// Stop the active meeting. Returns JSON with id/dir/transcript/
/// duration_secs/chunk_count/error. Negative on error / no active session.
int32_t dimmy_meeting_stop(char * _Nonnull out_buf, int32_t buf_len);

/// Persist the LLM-produced recap + actions (+ optional translation) into
/// the meeting directory. Pass nulls/empty to skip a field. 0 ok, -1 error.
int32_t dimmy_meeting_save_post_process(const char * _Nonnull dir_ptr,
                                        const char * _Nullable recap_ptr,
                                        const char * _Nullable actions_ptr,
                                        const char * _Nullable translated_ptr);

/// JSON array of orphaned meeting directories (`.recording` marker still
/// present from a crashed session). UI can offer "recover meeting?".
int32_t dimmy_meeting_list_orphans(char * _Nonnull out_buf, int32_t buf_len);

/// 1 = a meeting is currently active, 0 otherwise. Used to gate the
/// dictation hotkey while a meeting is recording (cpal collision).
int32_t dimmy_meeting_is_active(void);

/// Pause the active meeting. While paused the worker keeps cpal capturing
/// (we don't bounce the streams — that races with device acquisition on
/// resume) but skips WAV writes + STT chunks. On resume, the worker
/// advances its cursors past the gap so the paused window is excluded
/// from `audio.wav` and from the chunked transcript timeline; a
/// `[paused N ms]` line lands in `transcripts.txt` at the seam.
///
/// Return-code contract:
///   1  → state actually flipped (was running, now paused)
///   0  → no-op (already paused, or no meeting active)
///  -1  → internal lock failure (rare)
int32_t dimmy_meeting_pause(void);

/// Resume a paused meeting. Idempotent: same return-code contract as
/// `dimmy_meeting_pause` (1 flipped, 0 no-op, -1 lock failure).
int32_t dimmy_meeting_resume(void);

/// 1 if the active meeting is currently paused, 0 otherwise (including
/// no meeting active). Used to render the Pause/Resume button correctly
/// when re-attaching to an in-flight meeting (e.g. window reopened
/// while a meeting started from the pill is still running).
int32_t dimmy_meeting_is_paused(void);

// ── Recap LLM provider key (multi-vendor keystore) ───────────────

/// Save an API key for a specific provider into the chosen keystore
/// scope, WITHOUT changing `llm_api_url`. Pass `scope_ptr="llm"` for
/// the LLM-scope (cross-vendor recap that reuses LLM scope) or
/// `scope_ptr="recap"` for the dedicated Recap-scope (toggle "Use my
/// saved <Vendor> key" OFF). Empty `key_ptr` clears the stored key
/// for that (scope, provider) pair.
/// Returns: 0 saved/cleared, -1 invalid arg, -2 keystore write failed.
int32_t dimmy_save_llm_provider_key(const char * _Nonnull scope_ptr,
                                    const char * _Nonnull provider_ptr,
                                    const char * _Nonnull key_ptr);

// ── Notion integration ────────────────────────────────────────────

/// Save the user's Notion integration token to the AES-256 keystore.
/// Empty string clears it. Returns 0 on success, -1 on failure.
int32_t dimmy_notion_set_token(const char * _Nonnull token_ptr);

/// Returns 1 if a Notion token is currently stored, 0 otherwise.
int32_t dimmy_notion_has_token(void);

/// Verify the stored token by pinging /v1/users/me. Writes JSON
/// `{"ok":true,"bot_name":"...","workspace_name":"..."}` or
/// `{"ok":false,"error":"..."}` to out_buf. Returns the JSON length,
/// or -1 on invalid args.
int32_t dimmy_notion_test_connection(char * _Nonnull out_buf, int32_t buf_len);

/// Search Notion for accessible pages + databases. Writes a JSON
/// array of `{id,object,title,parent_label,url}` to out_buf.
/// Returns the JSON length, -1 on invalid args.
int32_t dimmy_notion_search(const char * _Nonnull query_ptr,
                            char * _Nonnull out_buf, int32_t buf_len);

/// Send a meeting's recap.md to the configured Notion target.
/// Writes `{"ok":true,"page_id":"...","page_url":"https://..."}` or
/// `{"ok":false,"error":"..."}`. Returns the JSON length, -1 on
/// invalid args.
int32_t dimmy_notion_send_recap(const char * _Nonnull meeting_dir_ptr,
                                char * _Nonnull out_buf, int32_t buf_len);

// ── Raw LLM call (bypasses dictation rewrite) ────────────────────

/// Send `prompt` to the configured LLM endpoint without the dictation
/// llm_style wrapper. `model_override` (nullable / empty for "use the
/// configured api_model") lets the caller pick a stronger model for
/// recap quality. Returns response byte length, or:
///   -1 invalid args, -2 missing config, -3 HTTP/parse error.
int32_t dimmy_llm_call_raw(const char * _Nonnull prompt_ptr,
                           const char * _Nullable model_override_ptr,
                           int32_t max_tokens,
                           char * _Nonnull out_buf,
                           int32_t buf_len);

// ── History v2 update hooks ──────────────────────────────────────

/// Backfill the `enhanced_text` column for the row created by the most
/// recent `dimmy_history_save`. Empty/null clears the column. 0 ok, -1 err.
int32_t dimmy_history_update_enhanced(int32_t id, const char * _Nullable text_ptr);

/// Set the `word_timestamps` JSON column (caller serialises the array).
/// Empty/null clears. 0 ok, -1 err.
int32_t dimmy_history_update_word_timestamps(int32_t id, const char * _Nullable json_ptr);

/// Set audio_path + size_bytes when audio retention writes a WAV. Pass
/// null path to unlink. 0 ok, -1 err.
int32_t dimmy_history_update_audio(int32_t id,
                                   const char * _Nullable path_ptr,
                                   int64_t size_bytes);

// ── User dictionary (vocabulary biasing) ─────────────────────────

/// Append `word_ptr` to the user dictionary. Case-insensitive dedupe +
/// persists to config.json. Returns:
///   0  added (new word)
///   1  already present (no-op)
///  -1  invalid args / persistence failure
int32_t dimmy_user_dict_add(const char * _Nonnull word_ptr);

/// Remove all entries matching `word_ptr` (case-insensitive). Returns
/// the count of entries dropped (0 = no match), or -1 on failure.
int32_t dimmy_user_dict_remove(const char * _Nonnull word_ptr);

/// Write the current dictionary to `out_buf` as a JSON array of
/// strings. Returns byte length, -1 invalid args, -2 buf too small.
int32_t dimmy_user_dict_list_json(char * _Nonnull out_buf, int32_t buf_len);

// ── Claude Code subscription login ────────────────────────────
// Use the Anthropic Pro/Team/Max subscription via the local
// `claude` CLI. See core/src/claude_code.rs.

/// Probe local Claude Code state.
///   0 = ready (binary + credentials)
///   1 = installed but not logged in
///   2 = not installed
int32_t dimmy_claude_code_status(void);

/// Resolved path of the `claude` binary, or 0 if not installed.
int32_t dimmy_claude_code_binary_path(char * _Nonnull out_buf, int32_t buf_len);

/// Spawn `claude /login` in a new Terminal window. Returns 0 on
/// success, -1 not installed, -2 spawn failure. Caller polls
/// dimmy_claude_code_status() to detect login completion.
int32_t dimmy_claude_code_spawn_login(void);

/// Send a content-free "ping" prompt through the local CLI to test
/// the end-to-end pipeline. Returns elapsed_ms (always > 0) on
/// success, or a negative error code:
///   -1 not installed, -2 not logged in, -3 spawn failed,
///   -4 timeout (15 s), -5 non-zero exit, -6 invalid utf-8 output.
int32_t dimmy_claude_code_ping(void);

/// Diagnostic snapshot of CLI binary detection — JSON with every
/// candidate path tried, which existed, whether the login-shell
/// fallback found one, and credentials-source label. Used by
/// Settings → About → Diagnostics so a user reporting "claude is
/// installed but Dimmy says NotInstalled" can copy/paste the JSON
/// into a bug report. Returns bytes written, or -1 (null buf) /
/// -2 (buf too small).
int32_t dimmy_claude_code_diagnostics(char * _Nonnull out_buf, int32_t buf_len);

/// JSON snapshot of Node.js detection:
///   {found, path, version, major, meets_minimum}
/// meets_minimum: major >= 18 (claude-code's minimum runtime).
/// Used by the setup wizard's Step 1.
int32_t dimmy_claude_code_node_status(char * _Nonnull out_buf, int32_t buf_len);

/// Invalidate cached binary lookups (claude + node) and return the
/// fresh claude status code (0/1/2). Used by the wizard's "I
/// installed it, recheck" buttons.
int32_t dimmy_claude_code_recheck(void);

/// Claude Desktop MCP bridge — JSON snapshot:
///   {installed, install_path?, extension_path?, config_patched,
///    extension_installed, extension_enabled, extension_version?,
///    entry_command?, heartbeat_age_secs?,
///    last_call?{tool,ago_secs,ok,elapsed_ms}}
/// `config_patched` is kept as a legacy alias of `extension_installed`
/// so the host bindings don't need a coordinated rename. Missing
/// fields are omitted, never null — "present" means "known".
int32_t dimmy_claude_desktop_status(char * _Nonnull out_buf, int32_t buf_len);

/// Install Dimmy as a Claude Desktop extension (DXT v0.3 unpacked).
/// Copies the dimmy-mcp binary into the user's Claude Extensions
/// dir, drops a manifest.json + icon.png next to it, flips the
/// per-extension enable flag. Idempotent. Returns 0 on success,
/// negative on categorical errors (-1..-5).
int32_t dimmy_claude_desktop_install(const char * _Nonnull binary_path,
                                     const char * _Nonnull version);

/// Remove the Dimmy Claude Desktop extension. rc 1 = removed, 0 =
/// nothing to remove (idempotent), negative on error.
int32_t dimmy_claude_desktop_uninstall(void);

/// Track a host-side telemetry event by name with optional JSON
/// properties. Used by the Mac UI to emit categorical events whose
/// trigger is host-side (login outcome polling, file-load source,
/// app-rules add/remove, etc.). Returns 0 emitted, -1 invalid input,
/// -2 unknown event name.
int32_t dimmy_telemetry_track_typed(const char * _Nonnull name_ptr,
                                    const char * _Nullable props_json_ptr);

/// Push one mic-activity observation from the host's audio-session
/// poll. `mic_active` = 1 iff some process is currently capturing
/// the default microphone (CoreAudio
/// kAudioDevicePropertyDeviceIsRunningSomewhere on Mac), 0 otherwise.
/// `app_id` = lowercase canonical id ("teams"/"zoom"/"slack"/
/// "discord"/"webex") if the host could infer it, NULL otherwise.
/// Returns: 0 = no transition / suppressed, 1 = call_detected emitted,
/// 2 = call_ended emitted, 3 = meeting.stop_suggested emitted,
/// -1 = invalid UTF-8 in app_id.
int32_t dimmy_call_signal(int32_t mic_active, const char * _Nullable app_id);

/// Push one system-audio-activity observation (loopback render-side).
/// AND-combined with mic for the stop-suggestion gate. Calling this
/// once switches the gate to AND-with-sys mode for the lifetime of
/// the state. Returns 0 / 3 / -1 (same semantics).
int32_t dimmy_call_signal_sys(int32_t sys_active, const char * _Nullable app_id);

/// Authoritative "call ended" signal — the host has positive evidence
/// the process that originated the meeting stopped capturing the mic
/// (its audio-input claim disappeared). Bypasses the amplitude-silence
/// heuristic and fires `meeting.stop_suggested` immediately, gated only
/// by the same one-shot + keep-recording cooldown guards. No args
/// (state is global). Returns 3 (stop_suggested emitted) or 0 (no-op).
int32_t dimmy_call_signal_session_ended(void);

/// Arm the stop-suggestion path for a meeting started OUTSIDE the
/// "Record now" nudge — i.e. manually from the meeting window / pill —
/// while a call is detected by the host. Without this the call
/// detector's `recording_active_from_us` stays false and
/// `dimmy_call_signal_session_ended` returns NoChange, so closing the
/// call never suggests stop for a manually-started meeting. Idempotent;
/// returns 1.
int32_t dimmy_call_meeting_started_external(void);

/// Compute a peak-summary JSON `{peaks:[f32;bucket_count], duration_secs}`
/// from any audio file the Rust loader can decode (WAV via hound, m4a
/// / mp3 / aac / flac / ogg via Symphonia). Same role as the in-Swift
/// `WavPeaks.readPeaks`, but multi-format — used by the Mac meeting
/// waveform when the on-disk track is Ogg/Vorbis instead of WAV.
/// Returns bytes written or -1 (bad args) / -2 (decode) / -3 (buf too
/// small — retry with a larger buffer).
int32_t dimmy_compute_audio_peaks(const char * _Nonnull path,
                                  int32_t bucket_count,
                                  char * _Nonnull out_buf,
                                  int32_t buf_len);

/// Decode any supported audio file (WAV via hound, m4a/mp3/aac/flac/ogg
/// via Symphonia) and write it as a real mono int16 WAV at the
/// source's native sample rate. Used by AudioPlaybackBar to play
/// Ogg/Vorbis meetings via AVAudioPlayer (which doesn't decode Ogg
/// natively on macOS) — the bar decodes to a tmp WAV first. Returns
/// the dst file size on success, or -1 (bad args) / -2 (decode) /
/// -3 (WAV write).
int32_t dimmy_decode_audio_to_wav(const char * _Nonnull src,
                                  const char * _Nonnull dst);

/// Record the user's response to a call/stop nudge.
/// `response` ∈ {"record_now","not_now","never","timeout",
/// "stop_and_recap","keep_recording","stop_timeout"}.
/// 0 ok, -1 null/invalid encoding, -2 unknown response string.
int32_t dimmy_call_signal_response(const char * _Nullable app_id,
                                   const char * _Nonnull response);

/// JSON snapshot of the call-detector state (enabled, excluded apps,
/// active cooldowns, current_app). Bytes written, -1 null buf,
/// -2 buf too small.
int32_t dimmy_call_detector_state(char * _Nonnull out_buf, int32_t buf_len);

/// Returns 1 if the two combos conflict (one is a subset of the other
/// so pressing one would also fire the other), else 0. A null / empty /
/// unparseable combo never conflicts. Used by the Mac command-hotkey
/// recorder to reject overlap with the dictation or dictionary
/// shortcut before saving. The combo grammar matches the Rust
/// `parse_combo` parser: tokens separated by `+`, modifiers
/// `ctrl|shift|cmd|win|opt|alt`, non-modifier keys by name
/// (`a`-`z`, `0`-`9`, `f1`-`f12`, `space`, `tab`, `enter`, `esc`,
/// `backspace`, `num0`-`num9`). Examples: `"cmd+shift+d"`, `"cmd+opt"`.
int32_t dimmy_hotkey_combos_conflict(const char * _Nullable a,
                                     const char * _Nullable b);

#endif /* DimmyFFI_h */
