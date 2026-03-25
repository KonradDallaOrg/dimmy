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
    [ObservableProperty] private int _chunkCurrent;
    [ObservableProperty] private int _chunkTotal;
    [ObservableProperty] private string _llmStyle = "off";
    [ObservableProperty] private string _deviceName = "";
    [ObservableProperty] private string _language = "";
    [ObservableProperty] private string _shortcut = "Win+Alt";
    [ObservableProperty] private string _shortcutMode = "toggle";
    [ObservableProperty] private string _timerText = "00:00";
    [ObservableProperty] private string _borderStyle = "Rainbow";
    [ObservableProperty] private string _waveformStyle = "Bars";
    [ObservableProperty] private string _overlayPosition = "Bottom Right";
    [ObservableProperty] private string _theme = "Default";
    [ObservableProperty] private bool _keepInClipboard;
    [ObservableProperty] private bool _showInTaskbar;

    public string LlmStyleColor =>
        StyleColors.TryGetValue(LlmStyle, out var color) ? color : "#41B0B1";

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
                    SetState(AppState.Completing);
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
