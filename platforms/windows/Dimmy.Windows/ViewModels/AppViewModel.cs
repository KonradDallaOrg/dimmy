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

    /// <summary>Wall-clock UTC when the current recording started (set on the
    /// recording_started event). Used to scale the transcription timeout with
    /// the recording length — a multi-minute local dictation legitimately takes
    /// far longer than a flat 30s to transcribe.</summary>
    public DateTime? RecordingStartedUtc { get; set; }
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

    /// Recap/command text as the LLM writes it, accumulated from the core's
    /// `llm_stream` event. A slow open-weight model can take half a minute
    /// before its first word (measured: 35s batch vs 11.7s streamed on
    /// Kimi K3), and showing nothing for that long reads as a hang — the
    /// question "is it stuck?" came from the user, not from a hypothesis.
    /// Empty between runs; the meeting window clears it when a recap starts.
    [ObservableProperty] private string _llmStreamText = "";

    // ── Local-model download state ──────────────────────────────────
    // The download runs on a background thread (Task.Run → FFI) and keeps
    // going even if the Settings window is closed. The Rust core emits
    // progress to this singleton view-model, so the latest state survives
    // navigating away + back: SettingsWindow restores its bar from here on
    // open and subscribes to the events below for live updates.
    [ObservableProperty] private bool _sttModelDownloadActive;
    /// 0-100, or -1 for indeterminate (Content-Length unavailable).
    [ObservableProperty] private double _sttModelDownloadPercent;
    [ObservableProperty] private string _sttModelDownloadLabel = "";
    [ObservableProperty] private bool _llmModelDownloadActive;
    [ObservableProperty] private double _llmModelDownloadPercent;
    [ObservableProperty] private string _llmModelDownloadLabel = "";

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

    /// Mirrors UiPreferences.TrayIconAlwaysVisible. When true, App pins the
    /// system-tray icon to the Win11 always-visible area (IsPromoted). Default
    /// false (opt-in).
    [ObservableProperty] private bool _trayIconAlwaysVisible;

    /// <summary>Command Mode: when true, the next recording transforms
    /// the user's CURRENTLY-SELECTED text using what they speak as the
    /// instruction (e.g. select a paragraph, hold the hotkey, say "make
    /// this more concise"). When false, normal dictation. Runtime-only
    /// (a transient mode, not persisted) — the user toggles it from the
    /// pill menu and it stays until toggled off. The dictation hotkey is
    /// reused; the mode decides whether the spoken text inserts (off) or
    /// transforms the selection (on).</summary>
    [ObservableProperty] private bool _commandMode;

    /// <summary>Transient one-shot command flag, set ONLY while a recording
    /// started by the dedicated command hotkey is in flight. Independent of
    /// <see cref="CommandMode"/> (the sticky menu toggle): the pill shows its
    /// amber command dot when EITHER is set, and the stop routes to the
    /// command transform when either is set, but the one-shot clears itself
    /// after the command completes so we revert to normal output — without
    /// flipping the persistent menu toggle.</summary>
    [ObservableProperty] private bool _commandOneShot;

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

    /// Fires when the core emits a STRUCTURED failure (`error` event
    /// carrying `source`): (source, provider, category, message).
    /// Message-only `error` events (e.g. "No speech detected") stay
    /// pill-only and do NOT fire this. Fired on the UI thread.
    public event Action<string, string, string, string>? CoreFailure;

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

    /// Fires on each Whisper/STT model download progress event. Args:
    /// (filename, downloaded_bytes, total_bytes). `total` is 0 when the
    /// server gave no Content-Length (treat as indeterminate). UI thread.
    public event Action<string, long, long>? SttModelDownloadProgress;

    /// Fires on each local-LLM model download progress event. Same args
    /// shape as <see cref="SttModelDownloadProgress"/>.
    public event Action<string, long, long>? LlmModelDownloadProgress;

    /// Update the persisted download state. Active flips false once the
    /// transfer completes (downloaded >= total, total known).
    private void ApplyDownloadState(string filename, long downloaded, long total, bool isLlm)
    {
        bool done = total > 0 && downloaded >= total;
        bool active = !done;
        double percent = total > 0 ? Math.Min(100.0, downloaded * 100.0 / total) : -1.0;
        string label = total > 0
            ? $"Downloading {filename}… {FormatBytes(downloaded)} / {FormatBytes(total)} ({percent:F0}%)"
            : $"Downloading {filename}… {FormatBytes(downloaded)}";
        if (done) label = "";

        if (isLlm)
        {
            LlmModelDownloadActive = active;
            LlmModelDownloadPercent = percent;
            LlmModelDownloadLabel = label;
        }
        else
        {
            SttModelDownloadActive = active;
            SttModelDownloadPercent = percent;
            SttModelDownloadLabel = label;
        }
    }

    private static string FormatBytes(long bytes)
    {
        if (bytes <= 0) return "0 MB";
        double mb = bytes / (1024.0 * 1024.0);
        return mb >= 1024.0 ? $"{mb / 1024.0:F2} GB" : $"{mb:F1} MB";
    }

    /// Fires when the chunked transcriber emits a new chunk. Args:
    /// (cumulative_text, is_final). App.xaml.cs uses this to show /
    /// hide the CaptionWindow and to keep its text in sync.
    public event Action<string, bool>? SttChunkReceived;

    /// Raised when a realtime streaming dictation session (Deepgram) emits
    /// a STABLE finalised segment that is safe to inject at the cursor.
    /// Args: (segmentText). Only fires for `engine == "deepgram"` chunks
    /// with a non-empty delta — interim previews carry an empty delta and
    /// are shown only in the live caption, never injected. The host types
    /// each segment at the cursor as the user speaks.
    public event Action<string>? StreamingSegmentFinalized;

    /// True once the current dictation session has produced at least one
    /// streaming (Deepgram) chunk. The host uses this to SUPPRESS the
    /// final clipboard paste at stop — the segments were already injected
    /// live, so pasting the whole transcript again would duplicate it.
    /// Reset by the host when a new recording starts.
    public bool StreamingDictationActive { get; set; }

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

    // ── Telegram inbox source ────────────────────────────────────────
    /// Auth/connection state changed. Args: (phase, account, pending).
    /// phase ∈ disabled|no_credentials|logged_out|wait_code|wait_password|
    /// connected|error. The Settings Telegram section drives its login UI
    /// off this.
    public event Action<string, string, int>? TelegramStateChanged;

    /// A shared audio is waiting for a decision. Args: (msgId, filename,
    /// dateEpoch, sizeBytes, isBacklog). Host asks "transcribe + recap?"
    /// then calls dimmy_telegram_process(msgId) on accept, or _dismiss.
    public event Action<int, string, long, long, bool>? TelegramPendingAudio;

    /// A shared audio has been downloaded to a local path. Args: (msgId,
    /// path, filename). Host runs its file-load transcribe (+ recap) on the
    /// path, then calls dimmy_telegram_mark_processed(msgId).
    public event Action<int, string, string>? TelegramAudioReady;

    /// A Telegram worker error (login/download/etc.). Args: (message).
    public event Action<string>? TelegramError;

    /// Fires when a single dictation crosses the 5 min soft threshold
    /// (core `dictation.long_warning`). Host shows a Meeting-mode nudge.
    public event Action? DictationLongWarning;

    /// Fires when a single dictation hits the 10 min hard cap (core
    /// `dictation.max_duration`). Host auto-stops + pastes what was said,
    /// bounding the otherwise-unbounded dictation buffer, then nudges
    /// toward Meeting mode. See App.OnDictationMaxDuration.
    public event Action? DictationMaxDurationReached;

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
                case "model_download_progress":
                    {
                        var fn = payload.GetProperty("filename").GetString() ?? "";
                        var dl = payload.GetProperty("downloaded").GetInt64();
                        var tot = payload.GetProperty("total").GetInt64();
                        ApplyDownloadState(fn, dl, tot, isLlm: false);
                        SttModelDownloadProgress?.Invoke(fn, dl, tot);
                    }
                    break;
                case "llm_model_download_progress":
                    {
                        var fn = payload.GetProperty("filename").GetString() ?? "";
                        var dl = payload.GetProperty("downloaded").GetInt64();
                        var tot = payload.GetProperty("total").GetInt64();
                        ApplyDownloadState(fn, dl, tot, isLlm: true);
                        LlmModelDownloadProgress?.Invoke(fn, dl, tot);
                    }
                    break;
                case "llm_stream":
                    {
                        // phase: start | delta | end. Only the OpenAI-compatible
                        // providers stream; Anthropic and Gemini-native answer in
                        // one shot and simply never emit this.
                        var phase = payload.TryGetProperty("phase", out var ph)
                            ? (ph.GetString() ?? "")
                            : "";
                        var chunk = payload.TryGetProperty("delta", out var ld)
                            ? (ld.GetString() ?? "")
                            : "";
                        if (phase == "start") LlmStreamText = "";
                        else if (phase == "delta") LlmStreamText += chunk;
                    }
                    break;
                case "stt_chunk":
                    {
                        var cumulative = payload.GetProperty("cumulative").GetString() ?? "";
                        var isFinal = payload.GetProperty("is_final").GetBoolean();
                        var engine = payload.TryGetProperty("engine", out var en)
                            ? (en.GetString() ?? "")
                            : "";
                        var delta = payload.TryGetProperty("delta", out var dl)
                            ? (dl.GetString() ?? "")
                            : "";
                        LiveCaptionText = cumulative;
                        SttChunkReceived?.Invoke(cumulative, isFinal);
                        // Realtime typing engines emit a STABLE (append-only)
                        // delta: the cloud streaming engines (Deepgram,
                        // OpenAI Realtime) AND the local chunked engine in
                        // typing mode (engine="local-stream", any local
                        // backend — Parakeet or whisper). All inject each
                        // delta at the cursor and suppress the final paste.
                        // Plain chunked captions (engine="parakeet"/"whisper")
                        // fall through to caption display only.
                        //
                        // This list is a whitelist, so a NEW streaming engine
                        // in the core is inert until its name is added here:
                        // it streams into the caption, never reaches the
                        // document, and the host still pastes everything at
                        // stop. That is exactly how the OpenAI engine shipped
                        // broken on 2026-08-05 — the core was working and the
                        // text had nowhere to go.
                        if (engine == "deepgram" || engine == "openai" || engine == "local-stream")
                        {
                            StreamingDictationActive = true;
                            if (!string.IsNullOrEmpty(delta))
                                StreamingSegmentFinalized?.Invoke(delta);
                        }
                    }
                    break;
                case "file_transcribe_progress":
                    {
                        // File-load emits {processed_secs,total_secs,percent};
                        // meeting re-transcribe emits {percent} only. Read each
                        // defensively so the percent-only payload doesn't throw.
                        double processed = payload.TryGetProperty("processed_secs", out var ps) ? ps.GetDouble() : 0;
                        double total = payload.TryGetProperty("total_secs", out var ts) ? ts.GetDouble() : 0;
                        double percent = payload.TryGetProperty("percent", out var pc) ? pc.GetDouble() : 0;
                        FileTranscribeProgress?.Invoke(processed, total, percent);
                    }
                    break;
                case "recording_started":
                    // New session — clear the streaming flag so a prior
                    // streaming dictation doesn't suppress this session's
                    // paste if it turns out to be a non-streaming one.
                    StreamingDictationActive = false;
                    if (SuppressRecordingStarted)
                    {
                        SuppressRecordingStarted = false;
                        // Late callback after PTT stop — ignore to prevent stuck Recording state
                    }
                    else
                    {
                        RecordingStartedUtc = DateTime.UtcNow;
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
                    // Structured failures also reach a system toast via
                    // CoreFailure — the pill Error state alone proved
                    // invisible (2026-07-04: four Groq HTTP 403 in a
                    // row, the user never saw why).
                    if (payload.TryGetProperty("source", out var srcEl))
                    {
                        var prov = payload.TryGetProperty("provider", out var pEl)
                            ? (pEl.GetString() ?? "") : "";
                        var cat = payload.TryGetProperty("category", out var cEl)
                            ? (cEl.GetString() ?? "") : "";
                        CoreFailure?.Invoke(srcEl.GetString() ?? "", prov, cat, msg);
                    }
                    break;
                case "telegram_state":
                    {
                        var phase = payload.TryGetProperty("phase", out var ph) ? (ph.GetString() ?? "") : "";
                        var account = payload.TryGetProperty("account", out var ac) ? (ac.GetString() ?? "") : "";
                        var pending = payload.TryGetProperty("pending", out var pe) ? pe.GetInt32() : 0;
                        TelegramStateChanged?.Invoke(phase, account, pending);
                    }
                    break;
                case "telegram_pending":
                    {
                        var msgId = payload.TryGetProperty("msg_id", out var mi) ? mi.GetInt32() : 0;
                        var filename = payload.TryGetProperty("filename", out var fn) ? (fn.GetString() ?? "") : "";
                        var date = payload.TryGetProperty("date", out var dt) ? dt.GetInt64() : 0;
                        var size = payload.TryGetProperty("size", out var sz) ? sz.GetInt64() : 0;
                        var backlog = payload.TryGetProperty("backlog", out var bk) && bk.GetBoolean();
                        TelegramPendingAudio?.Invoke(msgId, filename, date, size, backlog);
                    }
                    break;
                case "telegram_audio":
                    {
                        var msgId = payload.TryGetProperty("msg_id", out var mi) ? mi.GetInt32() : 0;
                        var path = payload.TryGetProperty("path", out var pa) ? (pa.GetString() ?? "") : "";
                        var filename = payload.TryGetProperty("filename", out var fn) ? (fn.GetString() ?? "") : "";
                        if (!string.IsNullOrEmpty(path))
                            TelegramAudioReady?.Invoke(msgId, path, filename);
                    }
                    break;
                case "telegram_error":
                    {
                        var tmsg = payload.TryGetProperty("message", out var em) ? (em.GetString() ?? "") : "";
                        if (!string.IsNullOrEmpty(tmsg))
                            TelegramError?.Invoke(tmsg);
                    }
                    break;
                case "dictation.long_warning":
                    // Core fired the 5 min soft threshold on a single dictation.
                    DictationLongWarning?.Invoke();
                    break;
                case "dictation.max_duration":
                    // Core fired the 10 min hard cap: the host stops + pastes
                    // (bounds the unbounded dictation buffer) and nudges toward
                    // Meeting mode. See App.OnDictationMaxDuration.
                    DictationMaxDurationReached?.Invoke();
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
