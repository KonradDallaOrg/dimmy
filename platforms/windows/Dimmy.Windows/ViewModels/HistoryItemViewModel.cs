using System;
using CommunityToolkit.Mvvm.ComponentModel;

namespace Dimmy.Windows.ViewModels;

/// One row in the History list. Mirrors the JSON shape returned by
/// `dimmy_history_recent` / `_search` (v2 schema).
public partial class HistoryItemViewModel : ObservableObject
{
    [ObservableProperty] private long _id;
    [ObservableProperty] private string _text = "";
    [ObservableProperty] private string? _enhancedText;
    [ObservableProperty] private string _language = "en";
    [ObservableProperty] private double _timestamp;
    [ObservableProperty] private double _duration;
    [ObservableProperty] private int _wordCount;
    [ObservableProperty] private string? _audioPath;
    [ObservableProperty] private string? _appProcessName;
    [ObservableProperty] private string? _appBundleId;
    [ObservableProperty] private string? _llmStyle;
    [ObservableProperty] private string? _llmTranslateTo;
    [ObservableProperty] private long _sizeBytes;

    /// Display-friendly timestamp (local time, "yyyy-MM-dd HH:mm").
    public string TimestampDisplay
    {
        get
        {
            try
            {
                var utc = DateTimeOffset.FromUnixTimeSeconds((long)Timestamp).UtcDateTime;
                return utc.ToLocalTime().ToString("yyyy-MM-dd HH:mm");
            }
            catch { return ""; }
        }
    }

    /// Compact preview — first 80 chars of the enhanced text if present,
    /// else raw text. Single line, no surrounding whitespace.
    public string Preview
    {
        get
        {
            var s = (string.IsNullOrWhiteSpace(EnhancedText) ? Text : EnhancedText) ?? "";
            s = s.Replace('\n', ' ').Replace('\r', ' ').Trim();
            return s.Length > 80 ? s.Substring(0, 80) + "…" : s;
        }
    }

    /// "12s" / "1m 35s" — human-readable recording duration.
    public string DurationDisplay
    {
        get
        {
            var secs = (int)Math.Round(Duration);
            if (secs < 60) return $"{secs}s";
            var m = secs / 60; var s = secs % 60;
            return s == 0 ? $"{m}m" : $"{m}m {s}s";
        }
    }

    /// Either app_process_name (Win) or app_bundle_id (Mac) or "—".
    public string AppDisplay
    {
        get
        {
            if (!string.IsNullOrWhiteSpace(AppProcessName)) return AppProcessName!;
            if (!string.IsNullOrWhiteSpace(AppBundleId)) return AppBundleId!;
            return "—";
        }
    }

    public bool HasAudio => !string.IsNullOrEmpty(AudioPath);
}
