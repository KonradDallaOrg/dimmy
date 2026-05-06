using System;
using System.Collections.Generic;
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
    [ObservableProperty] private int _chunkCurrent;
    [ObservableProperty] private int _chunkTotal;
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
    [ObservableProperty] private bool _showInTaskbar;

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

    /// Fires when the Rust core emits a file_transcribe_progress
    /// event during dimmy_transcribe_file. Args: (processed_secs,
    /// total_secs, percent 0-100). Used by Settings → Home → file
    /// load card to drive a determinate progress bar.
    public event Action<double, double, double>? FileTranscribeProgress;

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
                    break;
                case "error":
                    var msg = payload.GetProperty("message").GetString() ?? "Unknown error";
                    SetError(msg);
                    break;
            }
        }
        catch (JsonException) { }
        catch (KeyNotFoundException) { }
    }
}
