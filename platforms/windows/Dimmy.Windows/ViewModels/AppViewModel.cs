using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Text.Json;
using CommunityToolkit.Mvvm.ComponentModel;

namespace Dimmy.Windows.ViewModels;

public enum AppState
{
    Idle,
    Recording,
    Transcribing,
    Processing,
    Completing,
    Error
}

public partial class AppViewModel : ObservableObject
{
    /// <summary>
    /// Optional log sink injected by App at startup. Lets this view-
    /// model emit diagnostic lines without taking a hard dependency on
    /// <c>App.Log</c> — which keeps it cross-compilable into the test
    /// project (Dimmy.Windows.Tests doesn't link App.xaml.cs). Default
    /// no-op; production wires this to <c>App.Log</c> in OnLaunched.
    /// </summary>
    public static System.Action<string, string>? Log;
    private static readonly Dictionary<string, string> StyleColors = new()
    {
        ["off"] = "#41B0B1", ["correct"] = "#2dd4bf", ["summarize"] = "#fbbf24",
        ["elaborate"] = "#4ade80", ["comprehensible"] = "#38bdf8", ["professional"] = "#f472b6",
        ["prompt"] = "#a78bfa", ["genz"] = "#e879f9", ["boomer"] = "#f97316",
        ["emoji"] = "#facc15", ["acronyms"] = "#22d3ee", ["imbruttito"] = "#ef4444",
        ["custom"] = "#fb923c"
    };

    [ObservableProperty] private AppState _currentState = AppState.Idle;
    [ObservableProperty] private bool _isRecording;

    /// <summary>When true, ignore late "recording_started" callbacks from Rust.
    /// Set when PTT release initiates stop before Rust's callback arrives.</summary>
    public bool SuppressRecordingStarted { get; set; }
    [ObservableProperty] private string _statusText = "";
    [ObservableProperty] private string _errorMessage = "";
    [ObservableProperty] private float _amplitude;

    /// Cumulative transcript text emitted by the chunked transcriber
    /// (Rust core, behind chunk_streaming_enabled + Parakeet backend).
    /// Updated on every stt_chunk event during recording. Cleared when
    /// the recording finishes and the final paste is done.
    [ObservableProperty] private string _liveCaptionText = "";

    /// Mirrors the user-facing toggle. When false, App.xaml.cs does
    /// not show the CaptionWindow even if the chunked engine is on.
    [ObservableProperty] private bool _liveCaptionsEnabled = true;

    /// Mirrors the auto-detect toggle from Settings. When false,
    /// App.xaml.cs swallows the `call_detected` event (the Rust state
    /// machine still emits if a race happens; this guard catches the
    /// in-flight tick after the user disables).
    [ObservableProperty] private bool _callDetectEnabled = true;

    /// Excluded apps for the call-detect nudge. Read from config on
    /// load + after every "never" response so the Settings UI list
    /// stays in sync without polling. Stored as lowercase canonical
    /// ids matching the Rust state machine's keys.
    public ObservableCollection<string> CallDetectExcludedApps { get; } = new();

    /// Push a fresh exclusion list from disk into the observable
    /// collection. Called from App.xaml.cs after a "never" response
    /// (the Rust core just persisted to config.json — re-read so the
    /// Settings UI list updates immediately).
    public void RefreshCallDetectExclusions()
    {
        try
        {
            var buf = new byte[8192];
            int n = Interop.DimmyNative.dimmy_get_config_json(buf, buf.Length);
            if (n <= 0) return;
            var json = System.Text.Encoding.UTF8.GetString(buf, 0, n);
            using var doc = System.Text.Json.JsonDocument.Parse(json);
            if (!doc.RootElement.TryGetProperty("call_detect_excluded_apps", out var arr)
                || arr.ValueKind != System.Text.Json.JsonValueKind.Array)
                return;
            CallDetectExcludedApps.Clear();
            foreach (var item in arr.EnumerateArray())
            {
                var s = item.GetString();
                if (!string.IsNullOrEmpty(s)) CallDetectExcludedApps.Add(s);
            }
        }
        catch { /* defensive — settings list will catch up on next reload */ }
    }

    [ObservableProperty] private int _chunkCurrent;
    [ObservableProperty] private int _chunkTotal;

    /// <summary>True while a meeting recording is active in the Rust
    /// core. Updated ONLY in response to the `meeting_state` envelope
    /// from `dimmy_set_event_callback` — never via polling. Replaces
    /// the previous 500 ms <c>_meetingStatePollTimer</c> on the pill
    /// (CLAUDE.md "No FFI-state polling rule").</summary>
    [ObservableProperty] private bool _meetingActive;

    /// <summary>Captured at meeting START from the "Generate recap"
    /// checkbox (defaulted true for call-detect-started meetings). Read by
    /// EVERY stop path — meeting window, pill, call-detect popup — so a
    /// meeting started with recap unchecked never gets a recap, no matter
    /// how it's stopped. Plain property, not bound to any control.</summary>
    public bool MeetingGenerateRecap { get; set; } = true;

    /// <summary>True while the active meeting is paused. Same source
    /// as <see cref="MeetingActive"/> — set from the `meeting_state`
    /// envelope, never polled.</summary>
    [ObservableProperty] private bool _meetingPaused;
    [ObservableProperty] private string _llmStyle = "off";
    [ObservableProperty] private string _deviceName = "";
    [ObservableProperty] private string _language = "";
    /// <summary>
    /// Output translation language. Bound to Rust core's `llm_translate_to`
    /// config field. The pill's language scroll selector writes here, NOT
    /// to `Language` (which is the STT *input* hint set via Settings →
    /// Native language). Empty = no translation, transcript stays in
    /// whatever language the STT auto-detected (or `Language` hinted).
    /// </summary>
    [ObservableProperty] private string _llmTranslateTo = "";
    [ObservableProperty] private string _shortcut = "Win+Alt";
    [ObservableProperty] private string _shortcutMode = "toggle";
    [ObservableProperty] private string _timerText = "00:00";
    [ObservableProperty] private string _borderStyle = "Rainbow";
    [ObservableProperty] private string _waveformStyle = "Bars";
    [ObservableProperty] private string _overlayPosition = "Bottom Right";
    [ObservableProperty] private string _theme = "Default";
    [ObservableProperty] private bool _keepInClipboard;

    /// <summary>If true, the Dimmy entry is registered on the Windows
    /// taskbar (`TaskbarAnchorWindow`). False hides the taskbar button
    /// while keeping the system-tray icon + hotkey alive. Persisted in
    /// UiPreferences. Default true.</summary>
    [ObservableProperty] private bool _showTaskbarIcon = true;

    /// <summary>If true, pressing the global hotkey while the pill is
    /// hidden re-shows it. If false, the pill stays hidden — recording
    /// status is shown only via the taskbar overlay icon. Default true
    /// (legacy behavior). Persisted in UiPreferences.</summary>
    [ObservableProperty] private bool _pillShowOnHotkey = true;

    /// <summary>If true, the pill appears as soon as the app finishes
    /// startup. If false, the app boots in "taskbar only" mode.
    /// Default true. Persisted in UiPreferences.</summary>
    [ObservableProperty] private bool _pillShowOnStartup = true;

    public string LlmStyleColor =>
        StyleColors.TryGetValue(LlmStyle, out var color) ? color : "#41B0B1";

    /// <summary>True when the app is doing something (recording, transcribing, processing) and should not start a new recording.</summary>
    public bool IsBusy => CurrentState is AppState.Recording or AppState.Transcribing or AppState.Processing;

    public void SetState(AppState state)
    {
        CurrentState = state;
        IsRecording = state == AppState.Recording;
        StatusText = state switch
        {
            AppState.Transcribing => "Transcribing...",
            AppState.Processing => "Processing...",
            AppState.Error => ErrorMessage,
            _ => ""
        };
    }

    public void SetError(string message)
    {
        ErrorMessage = message.Length > 200 ? message[..200] : message;
        SetState(AppState.Error);
    }

    public void UpdateChunkProgress(int current, int total)
    {
        ChunkCurrent = current;
        ChunkTotal = total;
    }

    /// Fires when the Rust core emits a Parakeet bundle download
    /// progress event. Args: (downloaded_bytes, total_bytes). `total`
    /// is 0 if Content-Length was unavailable; consumers should treat
    /// that as "indeterminate". Fired on the UI thread.
    public event Action<long, long>? ParakeetDownloadProgress;

    /// Fires when the chunked transcriber emits a new chunk. Args:
    /// (cumulative_text, is_final). App.xaml.cs uses this to show /
    /// hide the CaptionWindow and to keep its text in sync.
    public event Action<string, bool>? SttChunkReceived;

    /// Raised on every `meeting_chunk` event from the Rust core —
    /// fired exactly once per chunk processed by the meeting worker
    /// (~ every 15 s, matching the chunked-STT cadence). Lets
    /// MeetingWindow append the new line to its transcript view
    /// without polling transcripts.txt. Args: (dir, speaker, line,
    /// elapsedMs, chunkCount).
    public event Action<string, string, string, long, int>? MeetingChunkReceived;

    /// Fires when the Rust core emits a file_transcribe_progress
    /// event during dimmy_transcribe_file. Args: (processed_secs,
    /// total_secs, percent 0-100). Used by Settings → Home → file
    /// load card to drive a determinate progress bar.
    public event Action<double, double, double>? FileTranscribeProgress;

    /// Fires when the Rust core emits a `transcript_ready` event with
    /// the final dictation text. Args: (text). Used by the onboarding
    /// "Try It" step to preview the user's first dictation inline
    /// instead of letting it disappear into the focused-app paste.
    /// Pure observer — does NOT affect the paste flow in StopAndProcess.
    public event Action<string>? TranscriptReady;

    /// Fires when the Rust call-detector decides the user is in a
    /// meeting (mic-active past the debounce, not suppressed). Args:
    /// (appIdOrNull, sinceSeconds). Host shows the CallNudgeWindow.
    public event Action<string?, long>? CallDetected;

    /// Fires when the previously-detected mic session ends (mic_active
    /// went back to false). Host hides the CallNudgeWindow if it's
    /// still up. Args: (appIdOrNull).
    public event Action<string?>? CallEnded;

    /// Fires when the call appears to have ended while WE were
    /// actively recording (mic silent for `mic_inactive_for_stop_secs`
    /// after the user accepted a `call_detected` nudge). Host shows
    /// the CallNudgeWindow in stop-suggestion mode so the user can
    /// either stop + recap or keep recording. Args:
    /// (appIdOrNull, inactiveForSecs).
    public event Action<string?, long>? CallStopSuggested;

    public void HandleEvent(string? json)
    {
        if (string.IsNullOrEmpty(json)) return;

        try
        {
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;
            var eventName = root.GetProperty("event").GetString();
            var payload = root.GetProperty("payload");

            switch (eventName)
            {
                case "parakeet_bundle_download_progress":
                    ParakeetDownloadProgress?.Invoke(
                        payload.GetProperty("downloaded").GetInt64(),
                        payload.GetProperty("total").GetInt64());
                    break;
                case "stt_chunk":
                    {
                        var cumulative = payload.GetProperty("cumulative").GetString() ?? "";
                        var isFinal = payload.GetProperty("is_final").GetBoolean();
                        LiveCaptionText = cumulative;
                        SttChunkReceived?.Invoke(cumulative, isFinal);
                    }
                    break;
                case "file_transcribe_progress":
                    {
                        var processed = payload.GetProperty("processed_secs").GetDouble();
                        var total = payload.GetProperty("total_secs").GetDouble();
                        var percent = payload.GetProperty("percent").GetDouble();
                        FileTranscribeProgress?.Invoke(processed, total, percent);
                    }
                    break;
                case "recording_started":
                    if (SuppressRecordingStarted)
                    {
                        SuppressRecordingStarted = false;
                        // Late callback after PTT stop — ignore to prevent stuck Recording state
                    }
                    else
                    {
                        SetState(AppState.Recording);
                    }
                    break;
                case "recording_cancelled":
                    SetState(AppState.Idle);
                    break;
                case "status":
                    var stateStr = payload.GetProperty("state").GetString();
                    if (stateStr == "transcribing") SetState(AppState.Transcribing);
                    else if (stateStr == "processing") SetState(AppState.Processing);
                    break;
                case "chunk_progress":
                    UpdateChunkProgress(
                        payload.GetProperty("current").GetInt32(),
                        payload.GetProperty("total").GetInt32());
                    break;
                case "transcript_ready":
                    // Completing state is set by App.xaml.cs StopAndProcess AFTER paste.
                    // Do NOT set it here — it would race with StopAndProcess and cause
                    // double Completing (Completing→Idle→Completing→Idle).
                    //
                    // Surface the text via TranscriptReady so subscribers
                    // (currently: OnboardingWindow Step 3 "Try It" preview)
                    // get the final transcript without having to scrape the
                    // paste buffer or poll history.db. Pure additive: the
                    // paste path in StopAndProcess is unaffected.
                    {
                        var text = payload.TryGetProperty("text", out var t)
                            ? (t.GetString() ?? "")
                            : "";
                        if (!string.IsNullOrEmpty(text))
                            TranscriptReady?.Invoke(text);
                    }
                    break;
                case "error":
                    var msg = payload.GetProperty("message").GetString() ?? "Unknown error";
                    SetError(msg);
                    break;
                case "meeting_state":
                    // Replaces the 500 ms _meetingStatePollTimer on
                    // PillWindow. Rust emits this exactly once per state
                    // transition (start / pause / resume / stop), so we
                    // get instant updates with zero idle CPU.
                    {
                        var ma = payload.GetProperty("active").GetBoolean();
                        var mp = payload.GetProperty("paused").GetBoolean();
                        Log?.Invoke($"meeting_state event: active={ma} paused={mp}", "Meeting");
                        MeetingActive = ma;
                        MeetingPaused = mp;
                    }
                    break;
                case "meeting_chunk":
                    // Replaces the 2 s DispatcherTimer that
                    // MeetingWindow used to run on transcripts.txt.
                    // Rust emits once per chunk (~15 s cadence), so
                    // the live transcript updates instantly and the
                    // window has zero idle CPU between chunks.
                    {
                        var dir = payload.GetProperty("dir").GetString() ?? "";
                        var speaker = payload.GetProperty("speaker").GetString() ?? "";
                        var line = payload.GetProperty("line").GetString() ?? "";
                        var elapsedMs = payload.GetProperty("elapsed_ms").GetInt64();
                        var chunkCount = payload.GetProperty("chunk_count").GetInt32();
                        MeetingChunkReceived?.Invoke(dir, speaker, line, elapsedMs, chunkCount);
                    }
                    break;
                case "audio.stream_error":
                    {
                        var role = payload.TryGetProperty("role", out var rEl) ? rEl.GetString() : "?";
                        var fmt = payload.TryGetProperty("format", out var fEl) ? fEl.GetString() : "?";
                        var kind = payload.TryGetProperty("kind", out var kEl) ? kEl.GetString() : "?";
                        Log?.Invoke($"AUDIO STREAM ERROR role={role} fmt={fmt} kind={kind} (mid-recording device-change?)", "Audio");
                    }
                    break;
                case "audio.device_change_recovery":
                    {
                        var trigger = payload.TryGetProperty("trigger", out var tEl) ? tEl.GetString() : "?";
                        Log?.Invoke($"AUDIO DEVICE-CHANGE RECOVERY (trigger={trigger}) — reopening streams on new default", "Audio");
                    }
                    break;
                case "call_detected":
                    {
                        string? app = null;
                        if (payload.TryGetProperty("app", out var appEl)
                            && appEl.ValueKind == JsonValueKind.String)
                            app = appEl.GetString();
                        long since = payload.TryGetProperty("since_seconds", out var ssEl)
                            ? ssEl.GetInt64() : 0;
                        CallDetected?.Invoke(app, since);
                    }
                    break;
                case "call_ended":
                    {
                        string? app = null;
                        if (payload.TryGetProperty("app", out var appEl)
                            && appEl.ValueKind == JsonValueKind.String)
                            app = appEl.GetString();
                        CallEnded?.Invoke(app);
                    }
                    break;
                case "meeting.stop_suggested":
                    {
                        string? app = null;
                        if (payload.TryGetProperty("app", out var appEl)
                            && appEl.ValueKind == JsonValueKind.String)
                            app = appEl.GetString();
                        long inactive = payload.TryGetProperty("inactive_for_secs", out var ifEl)
                            ? ifEl.GetInt64() : 0;
                        CallStopSuggested?.Invoke(app, inactive);
                    }
                    break;
            }
        }
        catch (JsonException) { }
        catch (KeyNotFoundException) { }
    }
}
