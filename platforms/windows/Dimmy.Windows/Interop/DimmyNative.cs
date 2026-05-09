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
