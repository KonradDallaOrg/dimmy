using CommunityToolkit.Mvvm.ComponentModel;

namespace Dimmy.Windows.ViewModels;

/// One row of the App Rules editor. Wraps the Rust-side
/// `core/src/app_rules.rs::AppRule` for two-way XAML binding.
public partial class AppRuleViewModel : ObservableObject
{
    [ObservableProperty] private string _matchPattern = "";
    [ObservableProperty] private string _matchType = "process_name";
    [ObservableProperty] private string _llmStyle = "off";
    [ObservableProperty] private string _llmTranslateTo = "";
    [ObservableProperty] private string _label = "";
    [ObservableProperty] private bool _enabled = true;

    /// Segoe Fluent Icons code point inferred from the pattern.
    /// Used as the fallback when no SVG brand asset matches.
    public string IconGlyph => InferIconGlyph(MatchPattern);

    /// Path to a bundled brand SVG (SimpleIcons-style) under
    /// Assets/AppIcons/ when one exists for this process name. Empty
    /// when no brand match — the XAML row template then renders the
    /// fallback FontIcon (IconGlyph). Future: SHGetFileInfo runtime
    /// extraction for arbitrary user-added rules.
    public string IconAssetUri => InferIconAssetUri(MatchPattern);

    private static string InferIconAssetUri(string pattern)
    {
        var p = (pattern ?? "").ToLowerInvariant();
        // Strip the .exe suffix on Win and look up by stem.
        var stem = p.EndsWith(".exe") ? p.Substring(0, p.Length - 4) : p;
        // Lookup table: process stem → bundled SVG basename. Add new
        // brands here when shipping more icons (drop the SVG file in
        // Assets/AppIcons/ first; .csproj globs **\*.svg).
        var lookup = new System.Collections.Generic.Dictionary<string, string>
        {
            ["slack"] = "slack",
            ["discord"] = "discord",
            ["teams"] = "teams",
            ["ms-teams"] = "teams",
            ["outlook"] = "outlook",
            ["chrome"] = "chrome",
            ["firefox"] = "firefox",
            ["msedge"] = "msedge",
            ["brave"] = "brave",
            ["code"] = "code",
            ["cursor"] = "cursor",
            ["notepad++"] = "notepad++",
            ["whatsapp"] = "whatsapp",
            ["telegram"] = "telegram",
            ["notion"] = "notion",
            ["obsidian"] = "obsidian",
            ["winword"] = "winword",
            ["excel"] = "excel",
        };
        if (lookup.TryGetValue(stem, out var name))
            return $"ms-appx:///Assets/AppIcons/{name}.svg";
        return "";
    }

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
