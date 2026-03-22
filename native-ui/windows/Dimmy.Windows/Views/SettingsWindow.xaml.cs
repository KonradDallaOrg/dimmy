using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Interop;
using Dimmy.Windows.ViewModels;

namespace Dimmy.Windows.Views;

public sealed partial class SettingsWindow : Window
{
    public SettingsViewModel ViewModel { get; } = new();
    private string _currentTag = "general";

    public SettingsWindow()
    {
        this.InitializeComponent();
        // Force light theme on the entire window content (including NavigationView pane)
        if (Content is FrameworkElement root)
        {
            root.RequestedTheme = ElementTheme.Light;
            root.DataContext = ViewModel;
        }
        Title = "Dimmy Settings";

        var appWindow = WindowHelper.GetAppWindow(this);
        appWindow.Resize(new global::Windows.Graphics.SizeInt32(620, 440));

        LoadConfig();
    }

    private void LoadConfig()
    {
        var json = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_config_json, 16384);
        if (json != null) ViewModel.LoadFromJson(json);
    }

    private void Nav_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        if (args.SelectedItem is NavigationViewItem item && item.Tag is string tag)
        {
            _currentTag = tag;
            GeneralPanel.Visibility = Visibility.Collapsed;
            ShortcutPanel.Visibility = Visibility.Collapsed;
            OutputPanel.Visibility = Visibility.Collapsed;
            OverlayPanel.Visibility = Visibility.Collapsed;
            AboutPanel.Visibility = Visibility.Collapsed;
            StatsPanel.Visibility = Visibility.Collapsed;
            DebugPanel.Visibility = Visibility.Collapsed;

            var panel = tag switch
            {
                "general" => GeneralPanel,
                "shortcut" => ShortcutPanel,
                "output" => OutputPanel,
                "overlay" => OverlayPanel,
                "about" => AboutPanel,
                "stats" => StatsPanel,
                "debug" => DebugPanel,
                _ => GeneralPanel,
            };
            panel.Visibility = Visibility.Visible;
        }
    }

    private void Save_Click(object sender, RoutedEventArgs e)
    {
        // Pull password values from PasswordBoxes into ViewModel before serializing
        if (!string.IsNullOrEmpty(ApiKeyBox.Password))
            ViewModel.ApiKey = ApiKeyBox.Password;
        if (!string.IsNullOrEmpty(LlmApiKeyBox.Password))
            ViewModel.LlmApiKey = LlmApiKeyBox.Password;

        var json = ViewModel.ToJson();
        DimmyNative.dimmy_set_config_json(json);
        App.Instance?.ReloadConfig();
        this.Close();
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        this.Close();
    }
}
