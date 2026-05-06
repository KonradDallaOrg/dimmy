using CommunityToolkit.Mvvm.ComponentModel;

namespace Dimmy.Windows.ViewModels;

/// One row of the App Rules editor. Wraps the Rust-side
/// `core/src/app_rules.rs::AppRule` for two-way XAML binding.
public partial class AppRuleViewModel : ObservableObject
{
    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(IconGlyph))]
    [NotifyPropertyChangedFor(nameof(IconAssetUri))]
    private string _matchPattern = "";
    [ObservableProperty] private string _matchType = "process_name";
    [ObservableProperty] private string _llmStyle = "off";
    [ObservableProperty] private string _llmTranslateTo = "";
    [ObservableProperty] private string _label = "";
    [ObservableProperty] private bool _enabled = true;

    /// Segoe Fluent Icons code point inferred from the pattern.
    /// Used as the fallback when no SVG brand asset matches.
    public string IconGlyph => InferIconGlyph(MatchPattern);

    /// file:/// URI of the cached PNG extracted from the real .exe of
    /// this process (see Helpers/IconExtractor). Empty when the icon
    /// hasn't been extracted yet — the XAML row template falls back
    /// to FontIcon (IconGlyph). The cache is populated:
    ///   • on every hotkey press for whichever app was foreground
    ///     (App.xaml.cs::CaptureAndPushAppContext), and
    ///   • by a warm-up pass over Process.GetProcesses() at Settings
    ///     window open (see SettingsWindow.WarmUpAppIcons).
    public string IconAssetUri => Helpers.IconExtractor.TryGetCachedUri(MatchPattern);

    /// Forces re-evaluation of IconAssetUri after the cache has been
    /// populated by an external pass (warm-up scan). Without this the
    /// XAML binding caches the empty string from the first read and
    /// the row keeps showing the FontIcon fallback forever.
    public void RefreshIconAssetUri() => OnPropertyChanged(nameof(IconAssetUri));

    private static string InferIconGlyph(string pattern)
    {
        var p = (pattern ?? "").ToLowerInvariant();
        if (p.Contains("slack") || p.Contains("discord") || p.Contains("whatsapp")
            || p.Contains("telegram") || p.Contains("messenger") || p.Contains("signal"))
            return ""; // Message
        if (p.Contains("teams") || p.Contains("zoom") || p.Contains("meet"))
            return ""; // People
        if (p.Contains("outlook") || p.Contains("thunderbird") || p.Contains("mailbird"))
            return ""; // Mail
        if (p.Contains("chrome") || p.Contains("firefox") || p.Contains("msedge")
            || p.Contains("brave") || p.Contains("opera") || p.Contains("vivaldi"))
            return ""; // Globe
        if (p.Contains("code") || p.Contains("cursor") || p.Contains("sublime")
            || p.Contains("idea") || p.Contains("rider") || p.Contains("pycharm")
            || p.Contains("webstorm") || p.Contains("clion") || p.Contains("rustrover")
            || p.Contains("notepad++") || p.Contains("terminal") || p.Contains("powershell")
            || p.Contains("cmd"))
            return ""; // CommandPrompt
        if (p.Contains("word") || p.Contains("notion") || p.Contains("obsidian")
            || p.Contains("notepad") || p.Contains("evernote") || p.Contains("onenote")
            || p.Contains("logseq"))
            return ""; // Document
        if (p.Contains("excel") || p.Contains("sheets"))
            return ""; // Calculator
        return ""; // AppIconDefault
    }

    public AppRuleViewModel() { }

    public AppRuleViewModel(
        string matchPattern,
        string matchType,
        string llmStyle,
        string llmTranslateTo,
        string label,
        bool enabled)
    {
        _matchPattern = matchPattern;
        _matchType = matchType;
        _llmStyle = llmStyle;
        _llmTranslateTo = llmTranslateTo ?? "";
        _label = label;
        _enabled = enabled;
    }
}
