using System;
using System.Linq;
using System.Net.Http;
using System.Text.Json;
using System.Threading.Tasks;
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
                LlmCustomUrlBox.Visibility = Visibility.Collapsed;
                LlmCustomModelBox.Visibility = Visibility.Collapsed;
            }
            else
            {
                LlmCustomUrlBox.Visibility = Visibility.Visible;
                LlmCustomModelBox.Visibility = Visibility.Visible;
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
            GeneralPanel.Visibility = Visibility.Collapsed;
            ShortcutPanel.Visibility = Visibility.Collapsed;
            OutputPanel.Visibility = Visibility.Collapsed;
            OverlayPanel.Visibility = Visibility.Collapsed;
            AboutPanel.Visibility = Visibility.Collapsed;
            PrivacyPanel.Visibility = Visibility.Collapsed;
            StatsPanel.Visibility = Visibility.Collapsed;
            DebugPanel.Visibility = Visibility.Collapsed;

            var panel = tag switch
            {
                "general" => GeneralPanel,
                "shortcut" => ShortcutPanel,
                "output" => OutputPanel,
                "overlay" => OverlayPanel,
                "about" => AboutPanel,
                "privacy" => PrivacyPanel,
                "stats" => StatsPanel,
                "debug" => DebugPanel,
                _ => GeneralPanel,
            };
            panel.Visibility = Visibility.Visible;

            if (tag == "privacy")
            {
                RefreshAnonymousIdText();
            }
        }
    }

    private void Save_Click(object sender, RoutedEventArgs e)
    {
        // Pull password values from PasswordBoxes into ViewModel before serializing
        if (!string.IsNullOrEmpty(ApiKeyBox.Password))
            ViewModel.ApiKey = ApiKeyBox.Password;
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
        if (_loaded && sender is ComboBox cb && cb.SelectedItem is string style)
            App.Instance?.ApplySettings(ViewModel);
    }

    private void WaveformStyle_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_loaded && sender is ComboBox cb && cb.SelectedItem is string style)
            App.Instance?.ApplySettings(ViewModel);
    }

    private void OverlayPosition_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_loaded && sender is ComboBox cb && cb.SelectedItem is string pos)
            App.Instance?.ApplySettings(ViewModel);
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
        _ = CheckForUpdateAsync();
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
