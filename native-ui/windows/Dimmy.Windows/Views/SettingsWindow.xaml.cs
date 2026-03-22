using System.Linq;
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

        // Force light theme on the entire window content
        if (Content is FrameworkElement root)
        {
            root.RequestedTheme = ElementTheme.Light;
            root.DataContext = ViewModel;
        }

        // Force light theme on NavigationView pane (it uses a separate visual tree)
        NavView.RequestedTheme = ElementTheme.Light;
        NavView.PaneDisplayMode = NavigationViewPaneDisplayMode.Left;

        Title = "Dimmy Settings";

        var appWindow = WindowHelper.GetAppWindow(this);
        appWindow.Resize(new global::Windows.Graphics.SizeInt32(620, 480));

        LoadConfig();
        SyncProviderComboBox();
    }

    private void LoadConfig()
    {
        var json = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_config_json, 16384);
        if (json != null) ViewModel.LoadFromJson(json);
    }

    /// <summary>
    /// Sync the Provider ComboBox selection to match the current ApiUrl from config.
    /// </summary>
    private void SyncProviderComboBox()
    {
        var preset = SettingsViewModel.ProviderPresets.FirstOrDefault(p =>
            !string.IsNullOrEmpty(p.Url) && p.Url == ViewModel.ApiUrl);

        if (preset != null)
        {
            var tag = preset.Name.ToLowerInvariant();
            for (int i = 0; i < ProviderComboBox.Items.Count; i++)
            {
                if (ProviderComboBox.Items[i] is ComboBoxItem item && item.Tag is string t && t == tag)
                {
                    ProviderComboBox.SelectedIndex = i;
                    return;
                }
            }
        }

        // No match — select "Custom endpoint" (last item)
        ProviderComboBox.SelectedIndex = ProviderComboBox.Items.Count - 1;
        CustomUrlBox.Visibility = Visibility.Visible;
        CustomModelBox.Visibility = Visibility.Visible;
    }

    private void Provider_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (sender is ComboBox cb && cb.SelectedItem is ComboBoxItem item && item.Tag is string tag)
        {
            var preset = SettingsViewModel.ProviderPresets.FirstOrDefault(p =>
                p.Name.ToLowerInvariant() == tag);

            if (preset != null && !string.IsNullOrEmpty(preset.Url))
            {
                ViewModel.ApiUrl = preset.Url;
                ViewModel.ApiModel = preset.DefaultModel;
                // Hide custom fields for known presets
                CustomUrlBox.Visibility = Visibility.Collapsed;
                CustomModelBox.Visibility = Visibility.Collapsed;
            }
            else
            {
                // "Custom" selected — show URL/model TextBoxes
                CustomUrlBox.Visibility = Visibility.Visible;
                CustomModelBox.Visibility = Visibility.Visible;
            }
        }
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
