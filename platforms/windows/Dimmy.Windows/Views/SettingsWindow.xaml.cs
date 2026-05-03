using System;
using System.Linq;
using System.Net.Http;
using System.Text.Json;
using System.Threading.Tasks;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Interop;
using Dimmy.Windows.ViewModels;

namespace Dimmy.Windows.Views;

public sealed partial class SettingsWindow : Window
{
    public SettingsViewModel ViewModel { get; } = new();
    private string _currentTag = "home";
    private bool _loaded; // suppress SelectionChanged during init

    public SettingsWindow()
    {
        this.InitializeComponent();

        if (Content is FrameworkElement root)
        {
            root.RequestedTheme = ElementTheme.Light;
            root.DataContext = ViewModel;
        }

        Title = "Dimmy Settings";

        var appWindow = WindowHelper.GetAppWindow(this);
        WindowHelper.ResizeLogical(this, 920, 640);

        // Set window icon
        try
        {
            var iconPath = System.IO.Path.Combine(
                System.AppContext.BaseDirectory, "Assets", "dimmy.ico");
            if (System.IO.File.Exists(iconPath))
                appWindow?.SetIcon(iconPath);
        }
        catch { }

        LoadConfig();
        ViewModel.LoadGpuStatus();
        SyncProviderComboBox();
        SyncLlmProviderComboBox();
        SyncLanguageComboBox();
        SyncThemeRadioButtons();
        PopulateLocalModels();
        SyncSttMode();
        PopulateLocalLlmModels();
        SyncLlmMode();
        PopulateStats();
        PopulateVersion();

        // Default to Home tab. Without this the NavigationView starts with no
        // selection, so the user sees the Home panel content (Visibility=
        // Visible in XAML) but no sidebar highlight, which is jarring.
        if (NavView.MenuItems.Count > 0 && NavView.MenuItems[0] is NavigationViewItem first)
        {
            NavView.SelectedItem = first;
        }

        // Pulse "Saved" InfoBar on any ViewModel field change (Win11 auto-save
        // pattern). The Save button still flushes to disk; this is purely
        // a visual hint that the form is dirty.
        ViewModel.PropertyChanged += (_, _) => PulseSavedInfoBar();

        _loaded = true;
    }

    private void LoadConfig()
    {
        // Read from config.json file first — it has all fields including UI-only ones
        string? fileJson = null;
        try
        {
            var configDir = System.Environment.GetFolderPath(System.Environment.SpecialFolder.ApplicationData);
            var path = System.IO.Path.Combine(configDir, "dimmy", "config.json");
            if (System.IO.File.Exists(path))
                fileJson = System.IO.File.ReadAllText(path);
        }
        catch { }

        if (!string.IsNullOrEmpty(fileJson))
            ViewModel.LoadFromJson(fileJson);

        // Win-only UI prefs live outside config.json (CLAUDE.md
        // single-writer rule). Pull them in so the toggles in the Pill
        // section reflect the on-disk state.
        var uiPrefs = Services.UiPreferences.Load();
        ViewModel.PillShowOnStartup = uiPrefs.PillShowOnStartup;
        ViewModel.PillShowOnHotkey = uiPrefs.PillShowOnHotkey;

        // Also read from FFI for runtime-only fields (has_key, has_llm_key, devices)
        // that are NOT in config.json (Rust computes them from keystore)
        try
        {
            var ffiJson = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_config_json, 16384);
            if (!string.IsNullOrEmpty(ffiJson))
            {
                using var doc = System.Text.Json.JsonDocument.Parse(ffiJson);
                var r = doc.RootElement;
                if (r.TryGetProperty("has_key", out var hk))
                    ViewModel.HasApiKey = hk.GetBoolean();
                if (r.TryGetProperty("has_llm_key", out var hlk))
                    ViewModel.HasLlmKey = hlk.GetBoolean();
                if (r.TryGetProperty("devices", out var devArr) &&
                    devArr.ValueKind == System.Text.Json.JsonValueKind.Array)
                {
                    var list = new System.Collections.Generic.List<string>();
                    foreach (var d in devArr.EnumerateArray())
                        if (d.GetString() is string s) list.Add(s);
                    ViewModel.Devices = list;
                }
            }
        }
        catch { }

        System.Diagnostics.Debug.WriteLine(
            $"[Settings] Loaded: has_key={ViewModel.HasApiKey}, has_llm_key={ViewModel.HasLlmKey}, " +
            $"llm_enabled={ViewModel.LlmStyle != "off"}, llm_style={ViewModel.LlmStyle}, " +
            $"llm_url={ViewModel.LlmApiUrl}, use_same_key={ViewModel.LlmUseSameKey}");
    }

    /// <summary>
    /// Sync the Provider ComboBox selection to match the current ApiUrl from config.
    /// </summary>
    private void SyncProviderComboBox()
    {
        // Match by URL + model (multiple presets can share the same URL, e.g. Groq turbo vs v3)
        var preset = SettingsViewModel.ProviderPresets.FirstOrDefault(p =>
            !string.IsNullOrEmpty(p.Url) && p.Url == ViewModel.ApiUrl && p.DefaultModel == ViewModel.ApiModel)
            ?? SettingsViewModel.ProviderPresets.FirstOrDefault(p =>
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

    private void SyncLlmProviderComboBox()
    {
        // Match by URL + model (multiple presets can share the same URL, e.g. Anthropic Haiku vs Sonnet)
        var preset = SettingsViewModel.LlmProviderPresets.FirstOrDefault(p =>
            !string.IsNullOrEmpty(p.Url) && p.Url == ViewModel.LlmApiUrl && p.DefaultModel == ViewModel.LlmApiModel)
            ?? SettingsViewModel.LlmProviderPresets.FirstOrDefault(p =>
            !string.IsNullOrEmpty(p.Url) && p.Url == ViewModel.LlmApiUrl);

        if (preset != null)
        {
            var tag = preset.Name.ToLowerInvariant();
            for (int i = 0; i < LlmProviderComboBox.Items.Count; i++)
            {
                if (LlmProviderComboBox.Items[i] is ComboBoxItem item && item.Tag is string t && t == tag)
                {
                    LlmProviderComboBox.SelectedIndex = i;
                    return;
                }
            }
        }

        // No match — select "Custom endpoint" (last item)
        LlmProviderComboBox.SelectedIndex = LlmProviderComboBox.Items.Count - 1;
        LlmCustomUrlBox.Visibility = Visibility.Visible;
        LlmCustomModelBox.Visibility = Visibility.Visible;
    }

    private void LlmProvider_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (sender is ComboBox cb && cb.SelectedItem is ComboBoxItem item && item.Tag is string tag)
        {
            var preset = SettingsViewModel.LlmProviderPresets.FirstOrDefault(p =>
                p.Name.ToLowerInvariant() == tag);

            if (preset != null && !string.IsNullOrEmpty(preset.Url))
            {
                ViewModel.LlmApiUrl = preset.Url;
                ViewModel.LlmApiModel = preset.DefaultModel;
                // Refresh the green ✓ for the newly-picked provider — without
                // this the badge shows the previous provider's state until the
                // user clicks Save (and Rust echoes back a fresh config).
                ViewModel.HasLlmKey = ViewModel.HasLlmKeyForUrl(preset.Url);
                LlmCustomUrlBox.Visibility = Visibility.Collapsed;
                LlmCustomModelBox.Visibility = Visibility.Collapsed;
            }
            else
            {
                LlmCustomUrlBox.Visibility = Visibility.Visible;
                LlmCustomModelBox.Visibility = Visibility.Visible;
                ViewModel.HasLlmKey = ViewModel.HasLlmKeyForUrl(ViewModel.LlmApiUrl);
            }
        }
    }

    private void SyncLanguageComboBox()
    {
        for (int i = 0; i < SettingsViewModel.Languages.Count; i++)
        {
            if (SettingsViewModel.Languages[i].Key == ViewModel.Language)
            {
                LanguageComboBox.SelectedIndex = i;
                return;
            }
        }
    }

    private void SyncThemeRadioButtons()
    {
        foreach (var child in ThemeRadioPanel.Children)
        {
            if (child is RadioButton rb && rb.Tag is string tag && tag == ViewModel.Theme)
            {
                rb.IsChecked = true;
                // Also apply to settings window appearance
                if (Content is FrameworkElement root)
                    root.RequestedTheme = tag switch
                    {
                        "Light" => ElementTheme.Light,
                        "Dark" => ElementTheme.Dark,
                        _ => ElementTheme.Default,
                    };
                return;
            }
        }
    }

    /// <summary>Model info parsed from FFI JSON.</summary>
    private record LocalModelInfo(string Name, string Filename, int SizeMb, string Description, bool Downloaded);

    private System.Collections.Generic.List<LocalModelInfo> _localModels = new();

    private void PopulateLocalModels()
    {
        try
        {
            var json = DimmyNative.ListLocalModels();
            if (string.IsNullOrEmpty(json)) return;

            using var doc = JsonDocument.Parse(json);
            _localModels.Clear();
            LocalModelComboBox.Items.Clear();

            int selectedIdx = 0;
            int idx = 0;
            foreach (var el in doc.RootElement.EnumerateArray())
            {
                var name = el.GetProperty("name").GetString() ?? "";
                var filename = el.GetProperty("filename").GetString() ?? "";
                var sizeMb = el.GetProperty("size_mb").GetInt32();
                var desc = el.GetProperty("description").GetString() ?? "";
                var downloaded = el.GetProperty("downloaded").GetBoolean();

                _localModels.Add(new LocalModelInfo(name, filename, sizeMb, desc, downloaded));

                var status = downloaded ? "Ready" : $"{sizeMb}MB";
                var item = new ComboBoxItem
                {
                    Content = $"{name} — {desc} ({status})",
                    Tag = filename
                };
                LocalModelComboBox.Items.Add(item);

                if (filename == ViewModel.LocalModel)
                    selectedIdx = idx;
                idx++;
            }

            if (LocalModelComboBox.Items.Count > 0)
                LocalModelComboBox.SelectedIndex = selectedIdx;
        }
        catch { }
    }

    private void LocalModel_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!_loaded) return;
        if (LocalModelComboBox.SelectedItem is ComboBoxItem item && item.Tag is string filename)
        {
            ViewModel.LocalModel = filename;
            CheckModelStatus();
        }
    }

    private void SyncSttMode()
    {
        bool isLocal = ViewModel.SttMode == "local";
        SttModeLocal.IsChecked = isLocal;
        SttModeCloud.IsChecked = !isLocal;
        LocalSttPanel.Visibility = isLocal ? Visibility.Visible : Visibility.Collapsed;
        CloudSttPanel.Visibility = isLocal ? Visibility.Collapsed : Visibility.Visible;

        if (isLocal)
            CheckModelStatus();
    }

    private void SttMode_Checked(object sender, RoutedEventArgs e)
    {
        if (!_loaded) return;
        if (sender is RadioButton rb && rb.Tag is string tag)
        {
            ViewModel.SttMode = tag;
            bool isLocal = tag == "local";
            LocalSttPanel.Visibility = isLocal ? Visibility.Visible : Visibility.Collapsed;
            CloudSttPanel.Visibility = isLocal ? Visibility.Collapsed : Visibility.Visible;

            if (isLocal)
                CheckModelStatus();
        }
    }

    private void CheckModelStatus()
    {
        try
        {
            int exists = DimmyNative.dimmy_model_exists(ViewModel.LocalModel);
            if (exists == 1)
            {
                LocalModelStatus.Text = "Ready";
                DownloadModelBtn.Visibility = Visibility.Collapsed;
            }
            else
            {
                var model = _localModels.Find(m => m.Filename == ViewModel.LocalModel);
                var sizeInfo = model != null ? $" ({model.SizeMb}MB)" : "";
                LocalModelStatus.Text = $"Not downloaded{sizeInfo}";
                DownloadModelBtn.Content = $"Download{sizeInfo}";
                DownloadModelBtn.Visibility = Visibility.Visible;
            }
        }
        catch
        {
            LocalModelStatus.Text = "Unable to check";
            DownloadModelBtn.Visibility = Visibility.Visible;
        }
    }

    private async void DownloadModel_Click(object sender, RoutedEventArgs e)
    {
        DownloadModelBtn.IsEnabled = false;
        DownloadModelBtn.Content = "Downloading...";
        DownloadProgress.Visibility = Visibility.Visible;
        LocalModelStatus.Text = "Downloading...";

        try
        {
            int result = await Task.Run(() => DimmyNative.dimmy_download_model(ViewModel.LocalModel));
            if (result == 0)
            {
                LocalModelStatus.Text = "Ready";
                DownloadModelBtn.Visibility = Visibility.Collapsed;
                // Refresh the ComboBox to show updated download status
                PopulateLocalModels();
            }
            else
            {
                LocalModelStatus.Text = "Download failed";
                DownloadModelBtn.Content = "Retry Download";
                DownloadModelBtn.IsEnabled = true;
            }
        }
        catch
        {
            LocalModelStatus.Text = "Download failed";
            DownloadModelBtn.Content = "Retry Download";
            DownloadModelBtn.IsEnabled = true;
        }
        finally
        {
            DownloadProgress.Visibility = Visibility.Collapsed;
        }
    }

    // ── Local LLM mode ────────────────────────────────────────────

    private System.Collections.Generic.List<LocalModelInfo> _localLlmModels = new();

    private void SyncLlmMode()
    {
        bool isLocal = ViewModel.LlmMode == "local";
        LlmModeLocal.IsChecked = isLocal;
        LlmModeCloud.IsChecked = !isLocal;
        LocalLlmPanel.Visibility = isLocal ? Visibility.Visible : Visibility.Collapsed;
        CloudLlmPanel.Visibility = isLocal ? Visibility.Collapsed : Visibility.Visible;

        if (isLocal)
            CheckLlmModelStatus();
    }

    private void LlmMode_Checked(object sender, RoutedEventArgs e)
    {
        if (!_loaded) return;
        if (sender is RadioButton rb && rb.Tag is string tag)
        {
            ViewModel.LlmMode = tag;
            SyncLlmMode();
        }
    }

    private void PopulateLocalLlmModels()
    {
        try
        {
            var json = DimmyNative.ListLocalLlmModels();
            if (string.IsNullOrEmpty(json)) return;

            using var doc = JsonDocument.Parse(json);
            _localLlmModels.Clear();
            LocalLlmModelComboBox.Items.Clear();

            int selectedIdx = 0;
            int idx = 0;
            foreach (var el in doc.RootElement.EnumerateArray())
            {
                var name = el.GetProperty("name").GetString() ?? "";
                var filename = el.GetProperty("filename").GetString() ?? "";
                var sizeMb = el.GetProperty("size_mb").GetInt32();
                var desc = el.GetProperty("description").GetString() ?? "";
                var downloaded = el.GetProperty("downloaded").GetBoolean();

                _localLlmModels.Add(new LocalModelInfo(name, filename, sizeMb, desc, downloaded));

                var status = downloaded ? "Ready" : $"{sizeMb}MB";
                var item = new ComboBoxItem
                {
                    Content = $"{name} — {desc} ({status})",
                    Tag = filename
                };
                LocalLlmModelComboBox.Items.Add(item);

                if (filename == ViewModel.LocalLlmModel)
                    selectedIdx = idx;
                idx++;
            }

            if (LocalLlmModelComboBox.Items.Count > 0)
                LocalLlmModelComboBox.SelectedIndex = selectedIdx;
        }
        catch { }
    }

    private void LocalLlmModel_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!_loaded) return;
        if (LocalLlmModelComboBox.SelectedItem is ComboBoxItem item && item.Tag is string filename)
        {
            ViewModel.LocalLlmModel = filename;
            CheckLlmModelStatus();
        }
    }

    private void CheckLlmModelStatus()
    {
        try
        {
            int exists = DimmyNative.dimmy_llm_model_exists(ViewModel.LocalLlmModel);
            if (exists == 1)
            {
                LocalLlmModelStatus.Text = "Ready";
                DownloadLlmModelBtn.Visibility = Visibility.Collapsed;
            }
            else
            {
                var model = _localLlmModels.Find(m => m.Filename == ViewModel.LocalLlmModel);
                var sizeInfo = model != null ? $" ({model.SizeMb}MB)" : "";
                LocalLlmModelStatus.Text = $"Not downloaded{sizeInfo}";
                DownloadLlmModelBtn.Content = $"Download{sizeInfo}";
                DownloadLlmModelBtn.Visibility = Visibility.Visible;
            }
        }
        catch
        {
            LocalLlmModelStatus.Text = "Unable to check";
            DownloadLlmModelBtn.Visibility = Visibility.Visible;
        }
    }

    private async void DownloadLlmModel_Click(object sender, RoutedEventArgs e)
    {
        DownloadLlmModelBtn.IsEnabled = false;
        DownloadLlmModelBtn.Content = "Downloading...";
        DownloadLlmProgress.Visibility = Visibility.Visible;
        LocalLlmModelStatus.Text = "Downloading...";

        try
        {
            int result = await Task.Run(() => DimmyNative.dimmy_download_llm_model(ViewModel.LocalLlmModel));
            if (result == 0)
            {
                LocalLlmModelStatus.Text = "Ready";
                DownloadLlmModelBtn.Visibility = Visibility.Collapsed;
                PopulateLocalLlmModels();
            }
            else
            {
                LocalLlmModelStatus.Text = "Download failed";
                DownloadLlmModelBtn.Content = "Retry Download";
                DownloadLlmModelBtn.IsEnabled = true;
            }
        }
        catch
        {
            LocalLlmModelStatus.Text = "Download failed";
            DownloadLlmModelBtn.Content = "Retry Download";
            DownloadLlmModelBtn.IsEnabled = true;
        }
        finally
        {
            DownloadLlmProgress.Visibility = Visibility.Collapsed;
        }
    }

    private void Language_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (LanguageComboBox.SelectedItem is System.Collections.Generic.KeyValuePair<string, string> kvp)
            ViewModel.Language = kvp.Key;
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
            // V2 IA: home / voice / output / pill / rules / shortcut / privacy / about / advanced.
            // Legacy tags (general / overlay / debug / stats) accepted for back-compat with any
            // saved nav-state path elsewhere — they map to the v2 panels behind the scenes.
            HomePanel.Visibility = Visibility.Collapsed;
            GeneralPanel.Visibility = Visibility.Collapsed;
            ShortcutPanel.Visibility = Visibility.Collapsed;
            OutputPanel.Visibility = Visibility.Collapsed;
            OverlayPanel.Visibility = Visibility.Collapsed;
            RulesPanel.Visibility = Visibility.Collapsed;
            AboutPanel.Visibility = Visibility.Collapsed;
            PrivacyPanel.Visibility = Visibility.Collapsed;
            StatsPanel.Visibility = Visibility.Collapsed;
            DebugPanel.Visibility = Visibility.Collapsed;

            var panel = tag switch
            {
                "home" => HomePanel,
                "voice" or "general" => GeneralPanel,
                "output" => OutputPanel,
                "pill" or "overlay" => OverlayPanel,
                "rules" => RulesPanel,
                "shortcut" => ShortcutPanel,
                "privacy" => PrivacyPanel,
                "about" => AboutPanel,
                "advanced" or "debug" => DebugPanel,
                "stats" => StatsPanel,
                _ => HomePanel,
            };
            panel.Visibility = Visibility.Visible;

            if (tag == "privacy")
            {
                RefreshAnonymousIdText();
            }
        }
    }

    /// <summary>
    /// Filter NavigationView items by user-typed query in the AutoSuggestBox.
    /// Hidden items are simply collapsed; the user clears the query to see
    /// everything again. Case-insensitive substring match on Content text.
    /// </summary>
    private void NavSearchBox_TextChanged(AutoSuggestBox sender, AutoSuggestBoxTextChangedEventArgs args)
    {
        var query = (sender.Text ?? string.Empty).Trim().ToLowerInvariant();
        foreach (var item in NavView.MenuItems)
        {
            if (item is NavigationViewItem navItem)
            {
                var label = (navItem.Content as string ?? string.Empty).ToLowerInvariant();
                navItem.Visibility = string.IsNullOrEmpty(query) || label.Contains(query)
                    ? Visibility.Visible
                    : Visibility.Collapsed;
            }
        }
    }

    /// <summary>
    /// Brief "Saved" InfoBar pulse triggered by any setting change. Win11-
    /// native auto-save UX. The actual persistence still happens on Save
    /// click for now; this is a visual hint that the form has been edited.
    /// </summary>
    private DispatcherQueueTimer? _savedPulseTimer;
    private void PulseSavedInfoBar()
    {
        if (!_loaded) return;
        SavedInfoBar.IsOpen = true;
        _savedPulseTimer ??= DispatcherQueue.CreateTimer();
        _savedPulseTimer.Stop();
        _savedPulseTimer.Interval = TimeSpan.FromMilliseconds(1500);
        _savedPulseTimer.IsRepeating = false;
        _savedPulseTimer.Tick -= OnSavedPulseTick;
        _savedPulseTimer.Tick += OnSavedPulseTick;
        _savedPulseTimer.Start();
    }
    private void OnSavedPulseTick(DispatcherQueueTimer sender, object args)
    {
        SavedInfoBar.IsOpen = false;
        sender.Stop();
    }

    private void Save_Click(object sender, RoutedEventArgs e)
    {
        // Pull password values from PasswordBoxes into ViewModel before serializing
        if (!string.IsNullOrEmpty(CloudApiKeyBox.Password))
            ViewModel.ApiKey = CloudApiKeyBox.Password;
        if (!string.IsNullOrEmpty(LlmApiKeyBox.Password))
            ViewModel.LlmApiKey = LlmApiKeyBox.Password;

        var json = ViewModel.ToJson();

        // Tell Rust to update in-memory state and save config.json
        // Rust now knows all fields (including UI appearance), so one writer only.
        try { DimmyNative.dimmy_set_config_json(json); } catch { }

        App.Instance?.ReloadConfig();
        App.Instance?.ApplySettings(ViewModel);
        this.Close();
    }

    /// <summary>Apply the pill-visibility prefs immediately on toggle
    /// so the user sees the effect (next hotkey press / next launch)
    /// without having to click Save. Persistence to ui_prefs.json is
    /// triggered indirectly through App.Instance.ApplySettings →
    /// AppViewModel.PropertyChanged → OnUiPrefsRelevantPropertyChanged.</summary>
    private void PillVisibilityToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (!_loaded) return;
        App.Instance?.ApplySettings(ViewModel);
    }

    private void Theme_Checked(object sender, RoutedEventArgs e)
    {
        if (sender is RadioButton rb && rb.Tag is string tag && Content is FrameworkElement root)
        {
            root.RequestedTheme = tag switch
            {
                "Light" => ElementTheme.Light,
                "Dark" => ElementTheme.Dark,
                _ => ElementTheme.Default,
            };
            // Save theme choice and apply to pill (Light → glass, Dark/Default → dark)
            ViewModel.Theme = tag;
            if (_loaded) App.Instance?.ApplySettings(ViewModel);
        }
    }

    private void BorderStyle_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_loaded && sender is ComboBox cb && cb.SelectedItem is string)
        {
            App.Instance?.ApplySettings(ViewModel);
            RenderPreview();
        }
    }

    private void WaveformStyle_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_loaded && sender is ComboBox cb && cb.SelectedItem is string)
        {
            App.Instance?.ApplySettings(ViewModel);
            RenderPreview();
        }
    }

    private void OverlayPosition_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_loaded && sender is ComboBox cb && cb.SelectedItem is string pos)
            App.Instance?.ApplySettings(ViewModel);
    }

    private void OverlayPositionCell_Checked(object sender, RoutedEventArgs e)
    {
        // Position grid cells: each RadioButton's Tag is the canonical
        // position string. The Tag→ViewModel write happens via the TwoWay
        // binding on IsChecked + StringEqualityConverter; this handler
        // just notifies the live overlay so the pill jumps immediately.
        if (_loaded) App.Instance?.ApplySettings(ViewModel);
    }

    private static Microsoft.UI.Xaml.Media.LinearGradientBrush BuildRainbowBrush()
    {
        var brush = new Microsoft.UI.Xaml.Media.LinearGradientBrush
        {
            StartPoint = new global::Windows.Foundation.Point(0, 0),
            EndPoint = new global::Windows.Foundation.Point(1, 1),
        };
        brush.GradientStops.Add(new Microsoft.UI.Xaml.Media.GradientStop
        { Offset = 0.0, Color = Microsoft.UI.ColorHelper.FromArgb(0xFF, 0x6E, 0xE7, 0xB7) });
        brush.GradientStops.Add(new Microsoft.UI.Xaml.Media.GradientStop
        { Offset = 0.33, Color = Microsoft.UI.ColorHelper.FromArgb(0xFF, 0x60, 0xA5, 0xFA) });
        brush.GradientStops.Add(new Microsoft.UI.Xaml.Media.GradientStop
        { Offset = 0.66, Color = Microsoft.UI.ColorHelper.FromArgb(0xFF, 0xF4, 0x72, 0xB6) });
        brush.GradientStops.Add(new Microsoft.UI.Xaml.Media.GradientStop
        { Offset = 1.0, Color = Microsoft.UI.ColorHelper.FromArgb(0xFF, 0xFB, 0xBF, 0x24) });
        return brush;
    }

    private static Microsoft.UI.Xaml.Media.SolidColorBrush PreviewSolid(byte r, byte g, byte b) =>
        new(Microsoft.UI.ColorHelper.FromArgb(0xFF, r, g, b));

    private string _currentPreviewState = "idle";

    /// <summary>Map a BorderStyle config string ("Rainbow", "Blue", "Green",
    /// "Purple", "Orange", "None") to the brush used on the preview pill.</summary>
    private static Microsoft.UI.Xaml.Media.Brush BorderStyleBrush(string style) => style switch
    {
        "Rainbow" => BuildRainbowBrush(),
        "Blue" => PreviewSolid(0x60, 0xA5, 0xFA),
        "Green" => PreviewSolid(0x4A, 0xDE, 0x80),
        "Purple" => PreviewSolid(0xA8, 0x78, 0xFA),
        "Orange" => PreviewSolid(0xFB, 0xBF, 0x24),
        "None" => PreviewSolid(0x33, 0x41, 0x55),
        _ => BuildRainbowBrush(),
    };

    /// <summary>Build the inner waveform shape based on WaveformStyle.</summary>
    private static UIElement BuildWaveformContent(string style)
    {
        var heights = new double[] { 10, 18, 24, 14, 22, 12, 20 };

        switch (style)
        {
            case "Line":
                // Single sinuous polyline.
                var line = new Microsoft.UI.Xaml.Shapes.Polyline
                {
                    Stroke = PreviewSolid(0x9C, 0xA3, 0xAF),
                    StrokeThickness = 1.5,
                    StrokeLineJoin = Microsoft.UI.Xaml.Media.PenLineJoin.Round,
                    HorizontalAlignment = HorizontalAlignment.Center,
                    VerticalAlignment = VerticalAlignment.Center,
                };
                for (int i = 0; i < 20; i++)
                {
                    double y = (i % 2 == 0 ? -1 : 1) * (4 + (i % 5));
                    line.Points.Add(new global::Windows.Foundation.Point(i * 5, y));
                }
                return line;
            case "Dots":
                // Row of circles, varying sizes.
                var dotPanel = new StackPanel
                {
                    Orientation = Orientation.Horizontal,
                    Spacing = 4,
                    HorizontalAlignment = HorizontalAlignment.Center,
                    VerticalAlignment = VerticalAlignment.Center,
                };
                var sizes = new double[] { 4, 6, 8, 5, 7, 4, 6 };
                foreach (var s in sizes)
                {
                    dotPanel.Children.Add(new Microsoft.UI.Xaml.Shapes.Ellipse
                    {
                        Width = s,
                        Height = s,
                        Fill = PreviewSolid(0x9C, 0xA3, 0xAF),
                        VerticalAlignment = VerticalAlignment.Center,
                    });
                }
                return dotPanel;
            default:
                // Bars / Bars Center / Bars Round
                var align = style == "Bars Center" ? VerticalAlignment.Center : VerticalAlignment.Bottom;
                var radius = style == "Bars Round" ? 1.5 : 0.5;
                var barPanel = new StackPanel
                {
                    Orientation = Orientation.Horizontal,
                    Spacing = 3,
                    HorizontalAlignment = HorizontalAlignment.Center,
                    VerticalAlignment = align,
                };
                foreach (var h in heights)
                {
                    barPanel.Children.Add(new Microsoft.UI.Xaml.Shapes.Rectangle
                    {
                        Width = 3,
                        Height = h,
                        Fill = PreviewSolid(0x9C, 0xA3, 0xAF),
                        RadiusX = radius,
                        RadiusY = radius,
                        VerticalAlignment = align,
                    });
                }
                return barPanel;
        }
    }

    /// <summary>Re-render the preview using the latest state + ViewModel
    /// values. Called on state change AND on BorderStyle/WaveformStyle change.</summary>
    private void RenderPreview()
    {
        if (PreviewContentHost == null || PreviewGlyph == null
            || PreviewCaption == null || PreviewPill == null) return;

        var state = _currentPreviewState;
        var borderStyle = ViewModel?.BorderStyle ?? "Rainbow";
        var waveformStyle = ViewModel?.WaveformStyle ?? "Bars";

        // Border: idle uses BorderStyle if not None; recording uses BorderStyle;
        // other states use semantic colors.
        PreviewPill.BorderBrush = state switch
        {
            "recording" => BorderStyleBrush(borderStyle),
            "transcribing" => PreviewSolid(0x60, 0xA5, 0xFA),
            "done" => PreviewSolid(0x4A, 0xDE, 0x80),
            "error" => PreviewSolid(0xF4, 0x72, 0x6E),
            _ => borderStyle == "None"
                    ? PreviewSolid(0x33, 0x41, 0x55)
                    : BorderStyleBrush(borderStyle),
        };

        PreviewContentHost.Children.Clear();
        bool showBars = state == "recording" || state == "transcribing";
        if (showBars)
        {
            PreviewContentHost.Children.Add(BuildWaveformContent(waveformStyle));
            PreviewGlyph.Visibility = Visibility.Collapsed;
        }
        else
        {
            PreviewGlyph.Visibility = Visibility.Visible;
        }

        PreviewCaption.Text = $"Preview · {state}";
    }

    private void PreviewState_Checked(object sender, RoutedEventArgs e)
    {
        if (sender is not RadioButton rb || rb.Tag is not string state) return;
        _currentPreviewState = state;
        if (PreviewGlyph == null || PreviewCaption == null || PreviewPill == null) return;
        SetPreviewGlyphForState(state);
        RenderPreview();
    }

    /// <summary>Glyph + colour for the non-bars preview states. Kept in a
    /// helper so the Unicode codepoints stay isolated from the rest of the
    /// rendering logic.</summary>
    private void SetPreviewGlyphForState(string state)
    {
        // E720 = microphone, E73E = checkmark, E783 = error/warning.
        switch (state)
        {
            case "idle":
                PreviewGlyph.Glyph = "";
                PreviewGlyph.Foreground = PreviewSolid(0x94, 0xA3, 0xB8);
                break;
            case "done":
                PreviewGlyph.Glyph = "";
                PreviewGlyph.Foreground = PreviewSolid(0x4A, 0xDE, 0x80);
                break;
            case "error":
                PreviewGlyph.Glyph = "";
                PreviewGlyph.Foreground = PreviewSolid(0xF4, 0x72, 0x6E);
                break;
        }
    }

    private void ResetPosition_Click(object sender, RoutedEventArgs e)
    {
        ViewModel.OverlayPosition = "Bottom Right";
        App.Instance?.ApplySettings(ViewModel);
    }

    /// <summary>
    /// Clear the sticky GPU known-bad marker. Effect takes hold on the next
    /// process launch since the GPU backend status is cached for the life of
    /// the current process. We refresh the displayed status so the user sees
    /// the marker disappear immediately.
    /// </summary>
    private void RetryGpu_Click(object sender, RoutedEventArgs e)
    {
        DimmyNative.dimmy_gpu_clear_known_bad();
        ViewModel.LoadGpuStatus();
    }

    private void PopulateStats()
    {
        var secs = ViewModel.StatsTotalSpeakingSecs;
        var mins = (int)(secs / 60);
        var hours = mins / 60;
        var remainMins = mins % 60;

        SpeakingTimeText.Text = hours > 0
            ? $"{hours}h {remainMins}m"
            : $"{mins}m {(int)(secs % 60)}s";

        var saved = ViewModel.TimeSavedEstimate;
        var savedMins = (int)(saved / 60);
        var savedHours = savedMins / 60;
        var savedRemainMins = savedMins % 60;

        TimeSavedText.Text = savedHours > 0
            ? $"~{savedHours}h {savedRemainMins}m"
            : $"~{savedMins}m";
    }

    private string _currentVersion = "0.0.0";

    private void PopulateVersion()
    {
        _currentVersion = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_version, 64) ?? "0.0.0";
        VersionText.Text = $"v{_currentVersion}";
        HeroTitleText.Text = $"Dimmy {_currentVersion}";
        HeroSubText.Text = $"Version {_currentVersion}";
        _ = CheckForUpdateAsync();
    }

    private async void CheckUpdates_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            await global::Windows.System.Launcher.LaunchUriAsync(new Uri("https://dimmy.app/download"));
        }
        catch { }
    }

    private async void ReleaseNotes_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            await global::Windows.System.Launcher.LaunchUriAsync(
                new Uri("https://github.com/KonradDallaOrg/dimmy/releases"));
        }
        catch { }
    }

    private async Task CheckForUpdateAsync()
    {
        try
        {
            using var http = new HttpClient();
            http.DefaultRequestHeaders.UserAgent.ParseAdd("Dimmy-Updater");
            http.Timeout = TimeSpan.FromSeconds(10);

            var resp = await http.GetStringAsync(
                "https://api.github.com/repos/KonradDallaOrg/dimmy/releases/latest");
            using var doc = JsonDocument.Parse(resp);
            var root = doc.RootElement;

            var tagName = root.GetProperty("tag_name").GetString() ?? "";
            var latestVersion = tagName.TrimStart('v');
            var htmlUrl = root.GetProperty("html_url").GetString() ?? "";

            if (IsNewerVersion(latestVersion, _currentVersion) && !string.IsNullOrEmpty(htmlUrl))
            {
                DispatcherQueue.TryEnqueue(() =>
                {
                    UpdateLink.Content = $"Update available: v{latestVersion}";
                    UpdateLink.NavigateUri = new Uri(htmlUrl);
                    UpdateLink.Visibility = Visibility.Visible;
                });
            }
        }
        catch { /* no network = no update check, that's fine */ }
    }

    /// <summary>Compare two semver strings. Returns true if candidate > current.</summary>
    private static bool IsNewerVersion(string candidate, string current)
    {
        if (Version.TryParse(candidate, out var c) && Version.TryParse(current, out var cur))
            return c > cur;
        return false;
    }

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        this.Close();
    }

    // ── Privacy panel handlers ──────────────────────────────────

    private void RefreshAnonymousIdText()
    {
        try
        {
            var id = DimmyNative.TelemetryAnonymousId() ?? "(unavailable)";
            // Display only the first 8 chars + ellipsis to avoid overwhelming
            // the UI with the full UUID. The full ID never needs to be shown.
            var preview = id.Length >= 8 ? $"{id[..8]}…" : id;
            AnonymousIdText.Text = preview;
        }
        catch
        {
            AnonymousIdText.Text = "(unavailable)";
        }
    }

    private void ResetAnonymousId_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            DimmyNative.TelemetryResetAnonymousId();
            AnonymousIdText.Text = "(reset — restart Dimmy to apply)";
        }
        catch { /* best-effort, this is a privacy action */ }
    }

    private void SendFeedback_Click(object sender, RoutedEventArgs e)
    {
        var message = FeedbackText.Text?.Trim() ?? string.Empty;
        if (string.IsNullOrWhiteSpace(message))
        {
            FeedbackStatus.Text = "Type something first.";
            return;
        }
        var kind = (FeedbackKindCombo.SelectedItem as ComboBoxItem)?.Tag as string ?? "general";
        var email = FeedbackEmail.Text?.Trim();
        if (string.IsNullOrWhiteSpace(email)) email = null;

        try
        {
            DimmyNative.CaptureFeedback(kind, message, email);
            FeedbackStatus.Text = "Thanks! Feedback sent.";
            FeedbackText.Text = string.Empty;
            FeedbackEmail.Text = string.Empty;
        }
        catch
        {
            FeedbackStatus.Text = "Couldn't send right now. Try again later.";
        }
    }
}
