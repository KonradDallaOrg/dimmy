using CommunityToolkit.Mvvm.ComponentModel;

namespace Dimmy.Windows.ViewModels;

/// One row of the App Rules editor — wraps the Rust-side
/// `core/src/app_rules.rs::AppRule` for two-way XAML binding.
///
/// Match types supported by the Rust matcher:
/// - "process_name" — exe basename on Windows (e.g. "slack.exe")
/// - "bundle_id"    — macOS app bundle id (e.g. "com.tinyspeck.slackmacgap")
/// - "wm_class"     — Linux X11 WM_CLASS
public partial class AppRuleViewModel : ObservableObject
{
    [ObservableProperty] private string _matchPattern = "";
    [ObservableProperty] private string _matchType = "process_name";
    [ObservableProperty] private string _llmStyle = "off";
    [ObservableProperty] private string _llmTranslateTo = "";
    [ObservableProperty] private string _label = "";
    [ObservableProperty] private bool _enabled = true;

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
