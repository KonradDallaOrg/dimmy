using System;
using System.Runtime.InteropServices;

namespace Dimmy.Windows.Interop;

/// <summary>
/// P/Invoke declarations for all 15 FFI functions exported by dimmy.dll (Rust cdylib).
/// See core/src/ffi.rs for the Rust side.
/// </summary>
public static class DimmyNative
{
    private const string DLL = "dimmy_lib";

    // ── Callback delegate ────────────────────────────────────────────
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    public delegate void EventCallback(IntPtr jsonPtr);

    // ── Lifecycle ────────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_init();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_shutdown();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_set_event_callback(EventCallback cb);

    // ── Call auto-detect ─────────────────────────────────────────────
    /// Push one mic-in-use observation. `appId` may be null when the
    /// host could not infer a VoIP process from the running set. Return
    /// codes: 1 = call_detected emitted, 2 = call_ended emitted,
    /// 0 = no transition (suppressed / debouncing / unchanged), -1 = malformed ptr.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_call_signal(
        int micActive,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? appId);

    /// Record the user's response to a call-detected nudge.
    /// `response` ∈ {"record_now","not_now","never","timeout",
    ///               "stop_and_recap","keep_recording","stop_timeout"}.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_call_signal_response(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? appId,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string response);

    /// Compute waveform peaks for any audio file the file-load path
    /// decodes (WAV via hound, m4a/mp3/aac/flac/ogg via Symphonia).
    /// Output JSON: `{"peaks":[f32; bucket_count], "duration_secs": f64}`.
    /// Returns bytes written, or -1 / -2 / -3 per the Rust contract.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_compute_audio_peaks(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path,
        int bucketCount,
        byte[] outBuf,
        int bufLen);

    /// Decode any audio file the loader supports and rewrite it as a
    /// real mono int16 WAV at the source's native sample rate.
    /// Returns destination file size on success, or -1/-2/-3 per the
    /// Rust contract (see core/src/ffi.rs::dimmy_decode_audio_to_wav).
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_decode_audio_to_wav(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string srcPath,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string dstPath);

    /// Push one system-audio activity observation. `sysActive` = 1 iff
    /// the loopback / render-side audio is currently emitting above
    /// the floor. Calling this even once flips the stop-suggestion
    /// gate to AND-with-sys mode for this process — both mic AND sys
    /// must be silent past their thresholds (5 s each) before the
    /// stop popup is offered. Return codes: 3 = meeting.stop_suggested
    /// emitted, 0 = no transition, -1 = malformed app id.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_call_signal_sys(
        int sysActive,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? appId);

    /// Authoritative "the call ended" signal — host has positive
    /// evidence the originating WASAPI capture session disappeared.
    /// Bypasses the amplitude-silence thresholds and fires
    /// `meeting.stop_suggested` immediately, gated only by the
    /// one-shot + KeepRecording cooldown guards. Used by the
    /// session-id-driven stop path; mutually exclusive with the
    /// amplitude heuristic — CallDetectionService picks one based
    /// on whether the meeting was started via call-detect.
    /// Returns 3 / 0.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_call_signal_session_ended();

    /// Arm the stop-suggestion path for a meeting started OUTSIDE the
    /// "Record now" nudge (manual start while a call is detected), so
    /// closing the call still suggests stop. Returns 1.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_call_meeting_started_external();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_call_detector_state(byte[] outBuf, int bufLen);

    // ── Recording ────────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_start_recording();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_stop_recording(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_cancel_recording();

    // ── Config ───────────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_get_config_json(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_set_config_json(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string json);

    // ── GPU diagnostics ──────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_gpu_get_status(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_gpu_clear_known_bad();

    // ── Audio ────────────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern float dimmy_get_amplitude();

    /// Peak amplitude of the SECONDARY (loopback / system) audio buffer.
    /// Returns 0.0 when no Mix recording is active. Used by the meeting
    /// window dual-band waveform to draw mic + system as separate bands.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern float dimmy_get_loopback_amplitude();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_list_devices_json(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_check_audio_health(byte[] outBuf, int bufLen);

    // ── LLM ──────────────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_cycle_llm_style(int direction);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_cycle_llm_tone(int direction);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_process_with_llm(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string text,
        byte[] outBuf, int bufLen);

    // ── Command Mode ─────────────────────────────────────────────────
    // Transform (or replace) the user's selected text using their spoken
    // instruction. rc: >0 bytes written, -1 invalid, -2 dispatch failed,
    // -3 no key, -4 local model missing, -5 runtime create failed.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_command_transform(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string selection,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string spoken,
        byte[] outBuf, int bufLen);

    // ── Stats ────────────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_update_stats(int words, double speakingSecs);

    // ── Local STT model management ──────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_list_local_models(byte[] buf, int len);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_download_model(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string filename);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_model_exists(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string filename);

    // ── Local LLM model management ────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_list_llm_models(byte[] buf, int len);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_download_llm_model(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string filename);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_llm_model_exists(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string filename);

    // ── Parakeet TDT v3 FP32 (alternative local STT backend) ─────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_parakeet_bundle_present();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_parakeet_download_bundle();

    // ── App context ──────────────────────────────────────────────
    /// Push the foreground-app snapshot so the Rust core can resolve
    /// app_rules at LLM-enhance time. JSON shape:
    ///   { "process_name": "slack.exe", "bundle_id": "", "wm_class": "" }
    /// Pass any subset of keys; missing ones default to empty string.
    /// Returns 0 on success, non-zero on parse error.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl,
        CharSet = CharSet.Ansi, BestFitMapping = false, ThrowOnUnmappableChar = true)]
    public static extern int dimmy_set_app_context(
        [MarshalAs(UnmanagedType.LPStr)] string json);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_clear_app_context();

    // ── History ──────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_history_save(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string text,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string language,
        double duration);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_history_recent(int limit, byte[] buf, int len);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_history_search(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string query,
        int limit, byte[] buf, int len);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_history_delete(int id);

    // ── File-load transcription (offline) ────────────────────────
    /// Synchronously transcribe a WAV file using the active local
    /// backend. See core/src/ffi.rs::dimmy_transcribe_file for return
    /// codes. On success the transcript is written to `outBuf` and
    /// the function returns its byte length.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_transcribe_file(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string path,
        byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_history_update_enhanced(
        int id,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? text);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_history_update_audio(
        int id,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? path,
        long sizeBytes);

    /// Set the word_timestamps JSON column for a history row.
    /// Caller serialises `[{"word":"...","start_ms":N,"end_ms":N}]`.
    /// Empty / null clears the field. Returns 0 on success, -1 on error.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_history_update_word_timestamps(
        int id,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? json);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_history_stats(byte[] buf, int len);

    // ── Meeting mode (long-form recording) ───────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_meeting_start(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_meeting_stop(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_meeting_save_post_process(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string dir,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? recap,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? actions,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? translated);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_meeting_list_orphans(byte[] outBuf, int bufLen);

    /// 1 = meeting currently recording; 0 = no active meeting. Used
    /// to gate the dictation hotkey so a parallel recording can't
    /// corrupt the shared cpal audio buffer.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_meeting_is_active();

    /// Pause / resume the in-flight meeting. cpal streams keep running
    /// in the background, but the meeting worker stops writing the WAV
    /// files / emitting STT chunks. On resume the worker advances past
    /// the paused window so the gap is excluded from the audio + the
    /// transcript timeline; a `[paused N ms]` marker is written into
    /// transcripts.txt at the seam.
    /// Returns 1 if state flipped, 0 if no-op (already in target state
    /// or no meeting active), -1 internal failure.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_meeting_pause();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_meeting_resume();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_meeting_is_paused();

    // ── Custom vocabulary dictionary ───────────────────────────────
    /// Append `word` to the user dictionary (case-insensitive dedupe +
    /// persist to config.json). Returns 0 if added, 1 if already
    /// present, -1 on invalid args / persistence failure.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_user_dict_add(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string word);

    /// Remove `word` (case-insensitive). Returns the count of entries
    /// dropped (0 = no match), or -1 on failure.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_user_dict_remove(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string word);

    /// Read the current dictionary as a JSON array of strings into
    /// outBuf. Returns the byte length, -1 on invalid args, or -2
    /// when buf_len is too small for the JSON (caller retries with
    /// a larger buffer).
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_user_dict_list_json(
        byte[] outBuf, int bufLen);

    // ── Recap LLM provider key (multi-vendor + multi-scope keystore) ─
    /// Save an API key for a specific provider into the chosen
    /// keystore scope, WITHOUT changing `llm_api_url`. Used by the
    /// Recap section: pass scope="llm" for the LLM-scope key (cross-
    /// vendor recap that reuses the LLM scope) or scope="recap" for
    /// the dedicated Recap-scope key (toggle "Use my saved <Vendor>
    /// key" OFF). Empty `key` clears the stored key for that
    /// (scope, vendor) pair.
    /// Returns:
    ///   0  saved/cleared
    ///   -1 invalid arg (NULL, bad UTF-8, unknown scope, unknown / unmapped vendor)
    ///   -2 keystore write failed
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_save_llm_provider_key(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string scope,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string provider,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string key);

    // ── Notion integration ──────────────────────────────────────────
    /// Save the user's Notion integration token to the AES-256 keystore.
    /// Empty string clears it. Returns 0 on success, -1 on failure.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_notion_set_token(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string token);

    /// Returns 1 if a Notion token is currently stored, 0 otherwise.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_notion_has_token();

    /// Verify the stored token by pinging /v1/users/me. Writes JSON
    /// envelope `{"ok":true,"bot_name":"...","workspace_name":"..."}`
    /// or `{"ok":false,"error":"..."}` to outBuf. Returns the JSON
    /// length, or -1 on invalid args.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_notion_test_connection(
        byte[] outBuf, int bufLen);

    /// Search Notion for accessible pages + databases. `query` is an
    /// optional substring filter — pass empty string to list everything.
    /// Writes a JSON array of `{id,object,title,parent_label,url}` to
    /// outBuf. Returns the JSON length, -1 on invalid args, or writes
    /// `{"error":"..."}` envelope on API failure.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_notion_search(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string query,
        byte[] outBuf, int bufLen);

    /// Send a meeting's recap.md to the configured Notion target.
    /// Reads the meeting dir's recap.md + transcripts.txt for the title
    /// hint, sends via the markdown-content API. Writes a JSON envelope
    /// `{"ok":true,"page_id":"...","page_url":"https://..."}` or
    /// `{"ok":false,"error":"..."}`. Returns the JSON length, -1 on
    /// invalid args.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_notion_send_recap(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string meetingDir,
        byte[] outBuf, int bufLen);

    /// Raw LLM call — bypasses the dictation rewrite wrapper. Pass
    /// empty string for `modelOverride` to use the user-configured
    /// llm_api_model. Used by meeting recap + audio-load summarizer.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_llm_call_raw(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string prompt,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string modelOverride,
        int maxTokens,
        byte[] outBuf, int bufLen);

    // ── Hotkey (low-level keyboard hook via Rust) ─────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_hotkey_install();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_hotkey_set(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string combo);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_hotkey_take_event();

    // Optional dedicated command-mode hotkey — runs on the SAME Rust hook,
    // so it inherits toggle/PTT + every combo (incl. modifier-only like
    // Win+Alt). Empty combo disables it.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_hotkey_set_command(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string combo);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_hotkey_take_command_event();

    // 1 if the two combos conflict (one is a subset of the other) — used to
    // reject a command hotkey that collides with the dictation/dict hotkey.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_hotkey_combos_conflict(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string a,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string b);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_hotkey_start_recording();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_hotkey_poll_recording();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_hotkey_take_recorded(byte[] buf, int len);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_hotkey_stop_recording();

    // ── Utility ──────────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_has_api_key();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_is_recording();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_get_version(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_build_flavor(byte[] outBuf, int bufLen);

    /// Returns the per-install config dir name embedded at build time
    /// via DIMMY_CONFIG_NAMESPACE (default "dimmy"). MUST be used to
    /// derive %APPDATA% paths in C# — deriving from
    /// `dimmy_build_flavor` produces the wrong dir when a
    /// flavor=staging build ships under the prod packId.
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_config_dir_name(byte[] outBuf, int bufLen);

    /// Returns the *effective* meeting storage directory — the user's
    /// `meeting_storage_path` override if set + writable, else the
    /// default `&lt;config_dir&gt;/meetings`. This is the single source the
    /// UI must use to read/write meeting artefacts; never re-derive the
    /// path locally (it ignores the override and the writability fallback).
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_meetings_dir(byte[] outBuf, int bufLen);

    // ── Managed helpers ──────────────────────────────────────────────

    /// <summary>Read a buffer-returning FFI call into a C# string.</summary>
    public static string? ReadBuffer(Func<byte[], int, int> ffiCall, int bufSize = 8192)
    {
        var buf = new byte[bufSize];
        int len = ffiCall(buf, buf.Length);
        if (len < 0) return null;
        return System.Text.Encoding.UTF8.GetString(buf, 0, len);
    }

    /// <summary>Marshal the event callback JSON pointer to a C# string.</summary>
    public static string? MarshalEventJson(IntPtr jsonPtr)
    {
        if (jsonPtr == IntPtr.Zero) return null;
        return Marshal.PtrToStringUTF8(jsonPtr);
    }

    // ── Local STT helpers ────────────────────────────────────────
    public static string? ListLocalModels() => ReadBuffer(dimmy_list_local_models);

    // ── Local LLM helpers ───────────────────────────────────────
    public static string? ListLocalLlmModels() => ReadBuffer(dimmy_list_llm_models);

    // ── History helpers ──────────────────────────────────────────
    public static string? HistoryRecent(int limit) =>
        ReadBuffer((buf, len) => dimmy_history_recent(limit, buf, len));

    public static string? HistorySearch(string query, int limit) =>
        ReadBuffer((buf, len) => dimmy_history_search(query, limit, buf, len));

    public static string? HistoryStats() => ReadBuffer(dimmy_history_stats);

    public static string? GpuGetStatus() => ReadBuffer(dimmy_gpu_get_status);

    // ── Telemetry ────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_telemetry_set_enabled(int enabled);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_telemetry_is_enabled();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_telemetry_anonymous_id(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_telemetry_reset_anonymous_id();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_telemetry_status(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_telemetry_set_crash_enabled(int enabled);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_telemetry_is_crash_enabled();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_telemetry_capture_feedback(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? kind,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string message,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? email);

    /// Track a typed event by name with a JSON property object.
    /// `name` is the stable event name ("meeting.recap_completed",
    /// "pill.style_scrolled", etc.). `propsJson` is a JSON object with
    /// the categorical fields that variant declares — `null` or empty
    /// for property-less events. Returns 0 on success, -1 invalid
    /// input, -2 unknown event name (host fell out of date).
    ///
    /// Privacy: the host MUST pass categorical / bucketed values.
    /// Never pass user content. The Rust core enforces the
    /// categorical allowlist per field — anything off-list lands as
    /// "unknown".
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_telemetry_track_typed(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string name,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? propsJson);

    /// Helper that builds the JSON + calls the FFI. Swallows
    /// exceptions because telemetry must NEVER abort a user flow.
    public static void TrackEvent(string name, object? props = null)
    {
        try
        {
            var json = props is null
                ? null
                : System.Text.Json.JsonSerializer.Serialize(props);
            _ = dimmy_telemetry_track_typed(name, json);
        }
        catch
        {
            // Best-effort — telemetry failures must not bubble up.
        }
    }

    // ── Claude Code subscription login ────────────────────────────
    //
    // Use the user's Anthropic Pro/Team/Max subscription via the
    // official `claude` CLI instead of API keys. See
    // `core/src/claude_code.rs` for the full design.

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_claude_code_status();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_claude_code_binary_path(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_claude_code_spawn_login();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_claude_code_ping();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_claude_code_node_status(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_claude_code_recheck();

    public enum ClaudeCodeStatus { Ready = 0, NotLoggedIn = 1, NotInstalled = 2 }

    /// <summary>
    /// Node.js detection snapshot for the setup wizard's Step 1.
    /// Mirror of the Rust `dimmy_claude_code_node_status` JSON envelope.
    /// </summary>
    public sealed record NodeStatus(
        bool Found,
        string? Path,
        string? Version,
        int? Major,
        bool MeetsMinimum);

    /// <summary>
    /// Outcome of the Test Connection button. Maps directly to the
    /// Rust FFI return-code table (see dimmy_claude_code_ping doc).
    /// </summary>
    public enum ClaudeCodePingResult
    {
        Ok,
        NotInstalled,
        NotLoggedIn,
        SpawnFailed,
        Timeout,
        NonZeroExit,
        InvalidUtf8,
        UnknownError,
    }

    public static ClaudeCodeStatus GetClaudeCodeStatus()
    {
        try { return (ClaudeCodeStatus)dimmy_claude_code_status(); }
        catch { return ClaudeCodeStatus.NotInstalled; }
    }

    public static string? GetClaudeCodeBinaryPath() =>
        ReadBuffer(dimmy_claude_code_binary_path, 4096);

    public static bool SpawnClaudeCodeLogin()
    {
        try { return dimmy_claude_code_spawn_login() == 0; }
        catch { return false; }
    }

    /// <summary>
    /// Run a small ping round-trip through the local Claude CLI.
    /// Returns (result, elapsed_ms). elapsed_ms is meaningful only when
    /// result == Ok; for error results it's 0.
    /// </summary>
    public static (ClaudeCodePingResult result, int elapsedMs) PingClaudeCode()
    {
        int rc;
        try { rc = dimmy_claude_code_ping(); }
        catch { return (ClaudeCodePingResult.UnknownError, 0); }
        return rc switch
        {
            > 0 => (ClaudeCodePingResult.Ok, rc),
            -1 => (ClaudeCodePingResult.NotInstalled, 0),
            -2 => (ClaudeCodePingResult.NotLoggedIn, 0),
            -3 => (ClaudeCodePingResult.SpawnFailed, 0),
            -4 => (ClaudeCodePingResult.Timeout, 0),
            -5 => (ClaudeCodePingResult.NonZeroExit, 0),
            -6 => (ClaudeCodePingResult.InvalidUtf8, 0),
            _ => (ClaudeCodePingResult.UnknownError, 0),
        };
    }

    /// <summary>
    /// Probe for a local Node.js install. Returns a structured snapshot
    /// the setup wizard binds to (Found/Path/Version/Major/MeetsMinimum).
    /// MeetsMinimum is true iff the detected major version is &gt;= 18 — the
    /// minimum runtime version claude-code requires.
    /// </summary>
    public static NodeStatus GetNodeStatus()
    {
        var json = ReadBuffer(dimmy_claude_code_node_status, 2048);
        if (string.IsNullOrEmpty(json))
            return new NodeStatus(false, null, null, null, false);
        try
        {
            using var doc = System.Text.Json.JsonDocument.Parse(json);
            var root = doc.RootElement;
            bool found = root.TryGetProperty("found", out var f) && f.GetBoolean();
            if (!found) return new NodeStatus(false, null, null, null, false);
            string? path = root.TryGetProperty("path", out var p) ? p.GetString() : null;
            string? version = root.TryGetProperty("version", out var v) && v.ValueKind == System.Text.Json.JsonValueKind.String
                ? v.GetString() : null;
            int? major = root.TryGetProperty("major", out var m) && m.ValueKind == System.Text.Json.JsonValueKind.Number
                ? m.GetInt32() : null;
            bool meets = root.TryGetProperty("meets_minimum", out var mm) && mm.GetBoolean();
            return new NodeStatus(found, path, version, major, meets);
        }
        catch
        {
            return new NodeStatus(false, null, null, null, false);
        }
    }

    /// <summary>
    /// Invalidate cached binary lookups (claude + node) and return the
    /// fresh Claude Code status. Called by the wizard's "I installed it,
    /// recheck" buttons — without this, Dimmy keeps stale "not found"
    /// answers until restart.
    /// </summary>
    public static ClaudeCodeStatus RecheckClaudeCode()
    {
        try { return (ClaudeCodeStatus)dimmy_claude_code_recheck(); }
        catch { return ClaudeCodeStatus.NotInstalled; }
    }

    // ── Claude Desktop MCP bridge ─────────────────────────────────
    //
    // Counterpart to the standalone `dimmy-mcp` binary shipped in the
    // installer alongside `Dimmy.exe`. The wizard installs Dimmy as a
    // Claude Desktop *extension* — a folder under
    // `<roaming>\Claude\Claude Extensions\dimmy\` containing
    // manifest.json + icon.png + dimmy-mcp.exe. Claude Desktop
    // auto-discovers it on startup; this is the same shape Anthropic
    // ships built-in connectors (PowerPoint by Anthropic etc.) in.

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_claude_desktop_status(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl, CharSet = CharSet.Ansi)]
    public static extern int dimmy_claude_desktop_install(string binaryPath, string versionPtr);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_claude_desktop_uninstall();

    /// <summary>
    /// One-shot snapshot of MCP bridge state. Every field is optional —
    /// "present" means "fact known". The wizard binds to Installed +
    /// ExtensionInstalled to decide which step to land on; the Settings
    /// status card uses HeartbeatAgeSecs to render "alive" vs "idle".
    ///
    /// ConfigPatched is kept as a legacy alias of ExtensionInstalled —
    /// it predates the DXT-extension wizard and means "Dimmy is
    /// registered with Claude in any form".
    /// </summary>
    public sealed record ClaudeDesktopStatus(
        bool Installed,
        string? InstallPath,
        string? ExtensionPath,
        bool ExtensionInstalled,
        bool ExtensionEnabled,
        string? ExtensionVersion,
        string? EntryCommand,
        long? HeartbeatAgeSecs,
        string? LastCallTool,
        long? LastCallAgoSecs,
        bool? LastCallOk,
        long? LastCallElapsedMs)
    {
        // Legacy alias for code paths that still bind to "is Dimmy
        // registered?" — same value, different name. New code should
        // read ExtensionInstalled.
        public bool ConfigPatched => ExtensionInstalled;
    }

    public static ClaudeDesktopStatus GetClaudeDesktopStatus()
    {
        var empty = new ClaudeDesktopStatus(false, null, null, false, false, null, null, null, null, null, null, null);
        var json = ReadBuffer(dimmy_claude_desktop_status, 2048);
        if (string.IsNullOrEmpty(json)) return empty;
        try
        {
            using var doc = System.Text.Json.JsonDocument.Parse(json);
            var r = doc.RootElement;
            bool installed = r.TryGetProperty("installed", out var i) && i.GetBoolean();
            string? installPath = r.TryGetProperty("install_path", out var ip) ? ip.GetString() : null;
            string? extPath = r.TryGetProperty("extension_path", out var xp) ? xp.GetString() : null;
            bool extInstalled = r.TryGetProperty("extension_installed", out var ei) && ei.GetBoolean();
            bool extEnabled = r.TryGetProperty("extension_enabled", out var ee) && ee.GetBoolean();
            string? extVersion = r.TryGetProperty("extension_version", out var ev) ? ev.GetString() : null;
            string? entryCmd = r.TryGetProperty("entry_command", out var ec) ? ec.GetString() : null;
            long? hbAge = r.TryGetProperty("heartbeat_age_secs", out var h) ? h.GetInt64() : null;
            string? lcTool = null; long? lcAgo = null; bool? lcOk = null; long? lcMs = null;
            if (r.TryGetProperty("last_call", out var lc) && lc.ValueKind == System.Text.Json.JsonValueKind.Object)
            {
                lcTool = lc.TryGetProperty("tool", out var t) ? t.GetString() : null;
                lcAgo = lc.TryGetProperty("ago_secs", out var ago) ? ago.GetInt64() : null;
                lcOk = lc.TryGetProperty("ok", out var ok) ? ok.GetBoolean() : null;
                lcMs = lc.TryGetProperty("elapsed_ms", out var em) ? em.GetInt64() : null;
            }
            return new ClaudeDesktopStatus(installed, installPath, extPath, extInstalled, extEnabled, extVersion, entryCmd, hbAge, lcTool, lcAgo, lcOk, lcMs);
        }
        catch
        {
            return empty;
        }
    }

    /// <summary>
    /// Install Dimmy as a Claude Desktop extension. Returns (success,
    /// rc) — rc is the raw Rust return code so the caller can log
    /// which categorical error fired (binary missing, copy locked,
    /// manifest write failed, …). 0 = ok, negative = error.
    /// </summary>
    public static (bool ok, int rc) InstallClaudeDesktopExtension(string binaryPath, string version)
    {
        try
        {
            int rc = dimmy_claude_desktop_install(binaryPath, version);
            return (rc == 0, rc);
        }
        catch { return (false, int.MinValue); }
    }

    public static (bool removed, int rc) UninstallClaudeDesktopExtension()
    {
        try
        {
            int rc = dimmy_claude_desktop_uninstall();
            return (rc == 1, rc);
        }
        catch { return (false, int.MinValue); }
    }

    public static bool TelemetryEnabled
    {
        get => dimmy_telemetry_is_enabled() == 1;
        set => dimmy_telemetry_set_enabled(value ? 1 : 0);
    }

    public static bool CrashReportsEnabled
    {
        get => dimmy_telemetry_is_crash_enabled() == 1;
        set => dimmy_telemetry_set_crash_enabled(value ? 1 : 0);
    }

    public static string? TelemetryAnonymousId() => ReadBuffer(dimmy_telemetry_anonymous_id);

    public static void TelemetryResetAnonymousId() => dimmy_telemetry_reset_anonymous_id();

    public static string? TelemetryStatus() => ReadBuffer(dimmy_telemetry_status);

    public static int CaptureFeedback(string kind, string message, string? email = null)
        => dimmy_telemetry_capture_feedback(kind, message, email);

    // ── Autostart (Launch at login) ─────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_autostart_set_enabled(int enabled);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_autostart_is_enabled();

    /// <summary>
    /// Cross-platform "launch at login" toggle. Wraps the OS-specific
    /// mechanisms (HKCU\…\Run on Windows, LaunchAgent plist on macOS,
    /// XDG autostart on Linux). Setting this to true writes the
    /// platform's user-scope autostart entry; setting it to false
    /// removes it. Both are reversible and require no admin rights.
    ///
    /// NB: the property setter throws on OS-level failure — callers
    /// should wrap in try/catch and revert the UI toggle on
    /// exception, otherwise the user sees the switch flip but
    /// autostart didn't actually engage.
    /// </summary>
    public static bool AutostartEnabled
    {
        get => dimmy_autostart_is_enabled() == 1;
        set
        {
            int rc = dimmy_autostart_set_enabled(value ? 1 : 0);
            if (rc != 0)
                throw new InvalidOperationException(
                    $"Failed to set autostart to {value} (return code {rc})");
        }
    }

    // ── Licensing ────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_license_status_json(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_license_plan_change(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string newTier,
        byte[] outBuf, int bufLen);

    // dimmy_license_set_server_url removed: the FFI is now debug-only on
    // the Rust side (gated behind cfg(debug_assertions)) and the
    // Settings UI override that called it has been deleted. Release
    // builds embed the URL via DIMMY_LICENSE_SERVER_URL at compile
    // time and refuse to be re-pointed. Local debug runs that need a
    // custom endpoint should use a debug DLL and call the FFI directly
    // from a test harness — never from shipped UI code.

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_license_request_trial(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string email,
        byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_license_redeem(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string code,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string deviceLabel,
        byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_license_refresh(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_license_clear();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_license_has_scope(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string scopeName);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_license_devices_list(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_license_device_deactivate(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? deviceId,
        byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_license_checkout_url(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string tier,
        [MarshalAs(UnmanagedType.LPUTF8Str)] string? email,
        byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_license_billing_portal_url(byte[] outBuf, int bufLen);
}
