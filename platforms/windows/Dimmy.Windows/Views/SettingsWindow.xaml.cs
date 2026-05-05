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
using Dimmy.Windows.Services;
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

        // Refresh License page when the running Dimmy redeems via dimmy:// URL.
        // The URL-scheme dispatch hits App.xaml.cs which calls LicenseService.RedeemAsync
        // off-thread; firing LicenseChanged lets us update the visible status card
        // without polling. We marshal back to UI thread via DispatcherQueue.
        LicenseService.LicenseChanged += OnLicenseChangedExternal;
        this.Closed += (_, __) => LicenseService.LicenseChanged -= OnLicenseChangedExternal;

        // Subscribe to Parakeet download progress events routed through
        // the App-level FFI callback. AppViewModel.HandleEvent already
        // marshals onto the UI thread before invoking the event.
        if (Application.Current is App app)
        {
            app.AppViewModel.ParakeetDownloadProgress += OnParakeetProgress;
            this.Closed += (_, __) =>
                app.AppViewModel.ParakeetDownloadProgress -= OnParakeetProgress;
        }

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
        // that are NOT in config.json (Rust computes them from keystore).
        // We also cache the per-provider has_*_key flags so a dropdown
        // change can refresh the green-check without first persisting
        // config + waiting for Rust to round-trip.
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
                _sttKeyByProvider.Clear();
                _llmKeyByProvider.Clear();
                CacheProviderKeyFlags(r);
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
            // Refresh the green-check badge for the newly-selected provider.
            // Without this, switching to a provider with a saved key still
            // showed "no key" until the user closed + reopened Settings.
            ViewModel.HasLlmKey = LookupLlmKeyForTag(tag);
        }
    }

    // Per-provider has_key flags cached at Settings open. Maps the
    // ComboBox tag (provider Name lowercased) to true/false. Filled by
    // CacheProviderKeyFlags() on Settings load.
    private readonly System.Collections.Generic.Dictionary<string, bool> _sttKeyByProvider = new(StringComparer.OrdinalIgnoreCase);
    private readonly System.Collections.Generic.Dictionary<string, bool> _llmKeyByProvider = new(StringComparer.OrdinalIgnoreCase);

    private void CacheProviderKeyFlags(System.Text.Json.JsonElement r)
    {
        // STT — `has_<provider>_key` flag set per Provider variant.
        // The dropdown tag is the lowercase provider name; the ProviderPreset
        // table holds the exact Tag→Name mapping. We hash both forms so a
        // tag like "groq-v3" still resolves to has_groq_key.
        foreach (var (key, prov) in new[] {
            ("has_groq_key", "groq"),
            ("has_openai_key", "openai"),
            ("has_gemini_key", "gemini"),
            ("has_deepgram_key", "deepgram"),
            ("has_fireworks_key", "fireworks"),
            ("has_together_key", "together"),
            ("has_custom_key", "custom"),
        })
        {
            if (r.TryGetProperty(key, out var v)) _sttKeyByProvider[prov] = v.GetBoolean();
        }
        foreach (var (key, prov) in new[] {
            ("has_groq_llm_key", "groq"),
            ("has_openai_llm_key", "openai"),
            ("has_anthropic_llm_key", "anthropic"),
            ("has_gemini_llm_key", "gemini"),
            ("has_openrouter_llm_key", "openrouter"),
            ("has_fireworks_llm_key", "fireworks"),
            ("has_together_llm_key", "together"),
            ("has_custom_llm_key", "custom"),
        })
        {
            if (r.TryGetProperty(key, out var v)) _llmKeyByProvider[prov] = v.GetBoolean();
        }
    }

    private bool LookupSttKeyForTag(string tag)
    {
        // Tags like "groq-v3" / "groq-distil" resolve to has_groq_key.
        // Custom is the catch-all.
        var baseProv = tag.Split('-')[0];
        if (_sttKeyByProvider.TryGetValue(baseProv, out var v)) return v;
        return _sttKeyByProvider.TryGetValue("custom", out var c) && c;
    }

    private bool LookupLlmKeyForTag(string tag)
    {
        var baseProv = tag.Split('-')[0];
        if (_llmKeyByProvider.TryGetValue(baseProv, out var v)) return v;
        return _llmKeyByProvider.TryGetValue("custom", out var c) && c;
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
        {
            SyncLocalSttBackend();
        }
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
                SyncLocalSttBackend();
        }
    }

    // ── Local STT backend (whisper / parakeet) ───────────────────────

    private void SyncLocalSttBackend()
    {
        // Select matching ComboBox item from ViewModel value.
        for (int i = 0; i < LocalSttBackendComboBox.Items.Count; i++)
        {
            if (LocalSttBackendComboBox.Items[i] is ComboBoxItem item
                && item.Tag is string tag && tag == ViewModel.LocalSttBackend)
            {
                LocalSttBackendComboBox.SelectedIndex = i;
                break;
            }
        }
        ApplyBackendVisibility(ViewModel.LocalSttBackend);
        if (ViewModel.LocalSttBackend == "parakeet")
            CheckParakeetStatus();
        else
            CheckModelStatus();
    }

    private void ApplyBackendVisibility(string backend)
    {
        bool isParakeet = backend == "parakeet";
        WhisperCardsPanel.Visibility = isParakeet ? Visibility.Collapsed : Visibility.Visible;
        ParakeetCardsPanel.Visibility = isParakeet ? Visibility.Visible : Visibility.Collapsed;
    }

    private void LocalSttBackend_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!_loaded) return;
        if (LocalSttBackendComboBox.SelectedItem is ComboBoxItem item && item.Tag is string tag)
        {
            ViewModel.LocalSttBackend = tag;
            ApplyBackendVisibility(tag);
            if (tag == "parakeet")
                CheckParakeetStatus();
            else
                CheckModelStatus();
        }
    }

    private void CheckParakeetStatus()
    {
        try
        {
            int present = DimmyNative.dimmy_parakeet_bundle_present();
            if (present == 1)
            {
                ParakeetStatusText.Text = "Ready";
                DownloadParakeetBtn.Visibility = Visibility.Collapsed;
            }
            else
            {
                ParakeetStatusText.Text = "Not downloaded";
                DownloadParakeetBtn.Visibility = Visibility.Visible;
                DownloadParakeetBtn.IsEnabled = true;
                DownloadParakeetBtn.Content = "Download (2.5 GB)";
            }
        }
        catch
        {
            ParakeetStatusText.Text = "Unable to check";
        }
    }

    private async void DownloadParakeet_Click(object sender, RoutedEventArgs e)
    {
        DownloadParakeetBtn.IsEnabled = false;
        DownloadParakeetBtn.Content = "Downloading...";
        // Start indeterminate; switches to determinate as soon as the
        // first parakeet_bundle_download_progress event with total > 0
        // arrives from the Rust core.
        ParakeetDownloadProgress.IsIndeterminate = true;
        ParakeetDownloadProgress.Value = 0;
        ParakeetDownloadProgress.Visibility = Visibility.Visible;
        ParakeetStatusText.Text = "Starting download...";

        try
        {
            int rc = await Task.Run(() => DimmyNative.dimmy_parakeet_download_bundle());
            if (rc == 0)
            {
                ParakeetStatusText.Text = "Ready";
                DownloadParakeetBtn.Visibility = Visibility.Collapsed;
            }
            else
            {
                ParakeetStatusText.Text = "Download failed";
                DownloadParakeetBtn.Content = "Retry Download";
                DownloadParakeetBtn.IsEnabled = true;
            }
        }
        catch
        {
            ParakeetStatusText.Text = "Download failed";
            DownloadParakeetBtn.Content = "Retry Download";
            DownloadParakeetBtn.IsEnabled = true;
        }
        finally
        {
            ParakeetDownloadProgress.Visibility = Visibility.Collapsed;
        }
    }

    private void OnParakeetProgress(long downloaded, long total)
    {
        // Total may be 0 when Content-Length was unavailable on one of
        // the bundle files — keep the bar indeterminate in that case.
        if (total <= 0)
        {
            ParakeetDownloadProgress.IsIndeterminate = true;
            ParakeetStatusText.Text =
                $"Downloading... {FormatMb(downloaded)} so far";
            return;
        }
        ParakeetDownloadProgress.IsIndeterminate = false;
        double percent = Math.Min(100, downloaded * 100.0 / total);
        ParakeetDownloadProgress.Value = percent;
        ParakeetStatusText.Text =
            $"Downloading... {FormatMb(downloaded)} / {FormatMb(total)} ({percent:F0}%)";
    }

    private static string FormatMb(long bytes)
    {
        if (bytes >= 1024L * 1024L * 1024L)
            return $"{bytes / 1024.0 / 1024.0 / 1024.0:F2} GB";
        return $"{bytes / 1024.0 / 1024.0:F0} MB";
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
            // Refresh the green-check badge for the newly-selected provider.
            // Without this, switching to a provider with a saved key still
            // showed "no key" until the user closed + reopened Settings.
            ViewModel.HasApiKey = LookupSttKeyForTag(tag);
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
            LicensePanel.Visibility = Visibility.Collapsed;
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
                "license" => LicensePanel,
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
            else if (tag == "license")
            {
                RefreshLicenseStatus();
            }
        }
    }

    // ── License ────────────────────────────────────────────────────────

    private void RefreshLicenseStatus()
    {
        try
        {
            var s = LicenseService.GetStatus();
            (string head, string detail) = s.Kind switch
            {
                "Unrestricted" => ("Source build — no licensing",
                                    "This binary was built without a licensing public key. All features are unlocked."),
                "NotFound"     => ("No license on this device",
                                    "Start a trial below or paste an activation code from your email."),
                "TrialActive"  => ($"Trial — {s.DaysRemaining} day(s) left",
                                    "You're in your free 14-day trial. Cloud + auto-update are enabled."),
                "TrialExpired" => ("Trial expired",
                                    "Your trial has ended. Cloud features are paused. Purchase a license to continue."),
                "Active"       => ($"Active — {s.Tier} ({s.DaysRemaining} day(s) left)",
                                    s.CancelsAt is long ca
                                        ? $"Subscription scheduled to cancel on {DateTimeOffset.FromUnixTimeSeconds(ca).LocalDateTime:MMM d, yyyy}. You keep cloud features until then."
                                        : "Thanks for supporting Dimmy. All cloud features are enabled."),
                "Expired"      => ("License expired",
                                    "Renew to re-enable cloud features."),
                "Suspended"    => ($"Suspended — offline {s.DaysOffline} day(s)",
                                    "Reconnect this device to refresh your license."),
                "Invalid"      => ("License file invalid",
                                    s.Error ?? "Re-activate this device."),
                _              => (s.Kind, s.Error ?? string.Empty),
            };
            LicenseStatusHeadline.Text = head;
            LicenseStatusDetail.Text = detail;

            // Tier pill + trailing days + tinted border — mirrors the
            // statusHero design from macOS MacLicensePage.swift. Drives
            // four signals at a glance: state (color), tier (badge text),
            // remaining time (trailing number), category (border tint).
            ApplyLicenseHero(s);

            // Stripe Customer Portal button — only meaningful for paid
            // licenses. Trials and source-build have no Stripe billing
            // attached. Lifetime DOES — the portal still surfaces
            // invoices and lets the user update their payment method
            // for future purchases on the same Stripe customer.
            LicenseManageSubButton.Visibility =
                s.Kind == "Active" && s.Tier is "monthly" or "annual" or "lifetime"
                    ? Visibility.Visible
                    : Visibility.Collapsed;
            ApplyBuyCardForStatus(s);
            PopulateScopeGrid(s);
            // Devices are server-side data — refresh asynchronously, only when
            // we actually have a license to query against.
            if (s.Kind is "TrialActive" or "Active" or "Suspended")
                _ = RefreshDevicesAsync();
            else
            {
                LicenseDevicesList.Children.Clear();
                LicenseDeviceCountLabel.Text = string.Empty;
            }
        }
        catch (Exception ex)
        {
            LicenseStatusHeadline.Text = "Status check failed";
            LicenseStatusDetail.Text = ex.Message;
        }
    }

    /// <summary>
    /// Compute and render the "status hero" decorations: tier pill text +
    /// color, trailing days counter, tinted border around the whole card.
    /// Mirrors macOS MacLicensePage statusHero so both platforms feel
    /// identical at a glance.
    /// </summary>
    private void ApplyLicenseHero(LicenseService.Status s)
    {
        // ── Tint per state ─────────────────────────────────────────────
        // Color is reserved for *positive* state. TrialActive=orange
        // ("act soon"), Active=green ("you're paid"), Unrestricted=purple
        // ("dev"). Everything else (NotFound, TrialExpired, Expired,
        // Invalid, Suspended) collapses to neutral gray — a license
        // problem isn't an error to alarm about, it's just a state.
        // Red was reading as "something is broken", wrong vibe for a
        // user who simply hasn't activated yet.
        global::Windows.UI.Color tint = s.Kind switch
        {
            "TrialActive"  => global::Windows.UI.Color.FromArgb(0xFF, 0xFF, 0x9F, 0x0A),
            "Active"       => global::Windows.UI.Color.FromArgb(0xFF, 0x34, 0xC7, 0x59),
            "Unrestricted" => global::Windows.UI.Color.FromArgb(0xFF, 0x9C, 0x5B, 0xFF),
            _              => global::Windows.UI.Color.FromArgb(0xFF, 0x90, 0x90, 0x99),
        };

        // ── Badge text ─────────────────────────────────────────────────
        // Active branches by tier so a paying user sees the SKU, not just
        // a generic "PRO". Trial / Suspended / Expired collapse to the
        // state name.
        string badge = s.Kind switch
        {
            "Unrestricted" => "DEV",
            "TrialActive" or "TrialExpired" => "TRIAL",
            "Active" => s.Tier switch
            {
                "monthly"  => "PRO • MONTHLY",
                "annual"   => "PRO • ANNUAL",
                "lifetime" => "PRO • LIFETIME",
                _          => "PRO",
            },
            "Expired"   => "EXPIRED",
            "Suspended" => "SUSPENDED",
            "Invalid"   => "INVALID",
            "NotFound"  => "INACTIVE",
            _           => s.Kind?.ToUpperInvariant() ?? "INACTIVE",
        };
        LicenseTierBadge.Text = badge;
        // Win11 Fluent badge: 14% tinted fill, 35% tinted stroke, matching
        // tint-colored text. Subtler than the prior solid fill + white
        // text, matches the visual weight of native InfoBadge / accent
        // pills in Settings / Edge / Store.
        var badgeFill = tint;   badgeFill.A   = 0x24; // ~14%
        var badgeStroke = tint; badgeStroke.A = 0x59; // ~35%
        LicenseTierBadgeBorder.Background  = new Microsoft.UI.Xaml.Media.SolidColorBrush(badgeFill);
        LicenseTierBadgeBorder.BorderBrush = new Microsoft.UI.Xaml.Media.SolidColorBrush(badgeStroke);
        LicenseTierBadge.Foreground        = new Microsoft.UI.Xaml.Media.SolidColorBrush(tint);
        LicenseTierBadgeBorder.Visibility = Visibility.Visible;

        // ── Hero card ─────────────────────────────────────────────────
        // Fluent prefers theme-aware surfaces over saturated colored
        // borders. We use a faint 5% tint background and the standard
        // ControlStrokeColorDefaultBrush for the border — the colored
        // accent stays in the badge + trailing days counter.
        var heroFill = tint; heroFill.A = 0x0D; // ~5%
        LicenseStatusBorder.Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(heroFill);
        LicenseStatusBorder.BorderBrush =
            (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["ControlStrokeColorDefaultBrush"];

        // ── Trailing big-number ───────────────────────────────────────
        // TrialActive / Active → days remaining. Suspended → days offline.
        // Everything else → no trailing column.
        if (s.Kind is "TrialActive" or "Active" && s.DaysRemaining is long d)
        {
            LicenseTrailingValue.Text = d.ToString();
            LicenseTrailingLabel.Text = d == 1 ? "DAY LEFT" : "DAYS LEFT";
            LicenseTrailingPanel.Visibility = Visibility.Visible;
        }
        else if (s.Kind == "Suspended" && s.DaysOffline is int o)
        {
            LicenseTrailingValue.Text = o.ToString();
            LicenseTrailingLabel.Text = o == 1 ? "DAY OFFLINE" : "DAYS OFFLINE";
            LicenseTrailingPanel.Visibility = Visibility.Visible;
        }
        else
        {
            LicenseTrailingPanel.Visibility = Visibility.Collapsed;
        }
    }

    private void PopulateScopeGrid(LicenseService.Status s)
    {
        LicenseScopeGrid.Children.Clear();
        var active = new System.Collections.Generic.HashSet<string>(s.Scopes,
            StringComparer.OrdinalIgnoreCase);
        foreach (var (key, display, descr) in LicenseService.ScopeNames.All)
        {
            bool granted = active.Contains(key);

            // One row per capability. Three columns: glyph, name + description, status.
            var row = new Grid { ColumnSpacing = 12 };
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(28) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

            var glyph = new FontIcon
            {
                Glyph = granted ? "" : "", // CheckMark vs Cancel
                FontSize = 16,
                // System accent brushes are theme-aware AND high-contrast in
                // both modes. Secondary at FontSize 16 was still rendering
                // as ghost-faint on the LayerOnAcrylic light card. Critical
                // (red) carries "missing/locked" without a wall of text.
                Foreground = granted
                    ? (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["SystemFillColorSuccessBrush"]
                    : (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["SystemFillColorCriticalBrush"],
                VerticalAlignment = VerticalAlignment.Top,
                Margin = new Thickness(0, 2, 0, 0),
            };
            Grid.SetColumn(glyph, 0);

            var labelStack = new StackPanel { Spacing = 1 };
            labelStack.Children.Add(new TextBlock
            {
                Text = display,
                FontSize = 13,
                FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            });
            labelStack.Children.Add(new TextBlock
            {
                Text = descr,
                FontSize = 12,
                // Per-row description was 11/Secondary which renders as
                // ghost on the LayerOnAcrylic light card. Bump to 12 +
                // Primary so it actually reads in light mode.
                Foreground = (Microsoft.UI.Xaml.Media.Brush)
                    Application.Current.Resources["TextFillColorPrimaryBrush"],
                Opacity = 0.85,
                TextWrapping = TextWrapping.Wrap,
            });
            Grid.SetColumn(labelStack, 1);

            var statusText = new TextBlock
            {
                Text = granted ? "Included" : "Not included",
                FontSize = 11,
                FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
                // Match the glyph: green Success for Included, red Critical
                // for missing. Plain Secondary was invisible in light theme.
                Foreground = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources[
                    granted ? "SystemFillColorSuccessBrush" : "SystemFillColorCriticalBrush"],
                VerticalAlignment = VerticalAlignment.Top,
                Margin = new Thickness(0, 4, 0, 0),
            };
            Grid.SetColumn(statusText, 2);

            row.Children.Add(glyph);
            row.Children.Add(labelStack);
            row.Children.Add(statusText);
            LicenseScopeGrid.Children.Add(row);
        }
    }

    private async void License_StartTrial_Click(object sender, RoutedEventArgs e)
    {
        var email = (LicenseTrialEmailBox.Text ?? string.Empty).Trim();
        if (string.IsNullOrEmpty(email) || !email.Contains('@'))
        {
            ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Error, "Enter a valid email address.");
            return;
        }
        LicenseTrialButton.IsEnabled = false;
        try
        {
            ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Informational, "Requesting magic link…");
            var r = await LicenseService.RequestTrialAsync(email);
            if (!r.Ok)
            {
                ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Error, r.Error ?? "Request failed.");
                return;
            }

            // Production: server emails the magic link, UI shows "check your inbox".
            // Dev (local server): server returns the link directly — we open it via the
            // OS to exercise the same dimmy:// path the email click would trigger.
            var link = r.MagicLink;
            if (string.IsNullOrEmpty(link))
            {
                ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Success,
                    "Check your email. Click the magic link from the device you want to activate.");
                return;
            }

            if (link.StartsWith("dimmy://", StringComparison.OrdinalIgnoreCase))
            {
                ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Informational,
                    "Activating via magic link…");
                System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(link)
                {
                    UseShellExecute = true,
                });

                // Poll status — the URL-scheme dispatch hands the code to the
                // running Dimmy via named pipe; redeem completes asynchronously.
                if (await WaitForActivationAsync(TimeSpan.FromSeconds(8)))
                {
                    RefreshLicenseStatus();
                    ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Success, "Activated. Welcome to Dimmy.");
                }
                else
                {
                    var fallback = r.Code ?? ExtractCode(link) ?? string.Empty;
                    ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Warning,
                        "Auto-activation didn't complete. Open the fallback below and paste this code: " + fallback);
                }
            }
            else
            {
                // Production case — server returned an HTTPS link (e.g. license.dimmy.app/m/...).
                // Email is the canonical delivery; just confirm.
                ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Success,
                    "Magic link sent to " + email + ". Click it from this device to activate.");
            }
        }
        catch (Exception ex)
        {
            ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Error, ex.Message);
        }
        finally
        {
            LicenseTrialButton.IsEnabled = true;
        }
    }

    private void OnLicenseChangedExternal()
    {
        // Always refresh, regardless of which tab is currently visible.
        // Pre-fix the check was `_currentTag == "license"` — that
        // skipped the refresh whenever the dimmy:// activate flow fired
        // NotifyChanged BEFORE NavigateToTag had set the tag to
        // "license" (the order in App.xaml.cs HandleForwardedCommand:
        // NotifyChanged → OpenSettingsWindowAt → NavigateToTag). The
        // user landed on the License page with stale "NotFound" data
        // and concluded activation hadn't worked.
        // RefreshLicenseStatus is cheap (one FFI call + a few bindings)
        // so always-running it on any LicenseChanged notification is
        // correct and idempotent.
        DispatcherQueue.TryEnqueue(RefreshLicenseStatus);
    }

    /// Navigate the SettingsWindow's NavigationView to the given tag.
    /// Called by App.xaml.cs after a successful URL-scheme activation so
    /// the user lands on the License page with the post-redeem status.
    /// When navigating to "license" we also force-refresh the status
    /// view so the badge reflects whatever the most recent activate /
    /// refresh wrote to disk — even if the LicenseChanged event raced
    /// past the page before this navigation completed.
    public void NavigateToTag(string tag)
    {
        DispatcherQueue.TryEnqueue(() =>
        {
            foreach (var item in NavView.MenuItems)
            {
                if (item is NavigationViewItem nv && nv.Tag is string t && t == tag)
                {
                    NavView.SelectedItem = nv;
                    if (tag == "license")
                    {
                        try { RefreshLicenseStatus(); }
                        catch { /* page bindings might not be ready yet */ }
                    }
                    return;
                }
            }
        });
    }

    /// Poll dimmy_license_status until kind transitions into TrialActive/Active,
    /// or timeout. Used right after triggering a dimmy:// magic link to surface
    /// the activation result inline instead of forcing the user to click Refresh.
    private static async Task<bool> WaitForActivationAsync(TimeSpan budget)
    {
        var start = DateTime.UtcNow;
        while (DateTime.UtcNow - start < budget)
        {
            await Task.Delay(400);
            var s = LicenseService.GetStatus();
            if (s.Kind == "TrialActive" || s.Kind == "Active") return true;
        }
        return false;
    }

    private async void License_Activate_Click(object sender, RoutedEventArgs e)
    {
        var raw = (LicenseCodeBox.Text ?? string.Empty).Trim();
        if (string.IsNullOrEmpty(raw))
        {
            ShowInfoBar(LicenseActivateInfoBar, InfoBarSeverity.Error, "Paste a code or magic-link URL.");
            return;
        }
        var code = ExtractCode(raw) ?? raw;
        var label = (LicenseDeviceLabelBox.Text ?? string.Empty).Trim();
        if (string.IsNullOrEmpty(label)) label = Environment.MachineName;
        try
        {
            ShowInfoBar(LicenseActivateInfoBar, InfoBarSeverity.Informational, "Activating…");
            var r = await LicenseService.RedeemAsync(code, label);
            if (r.Ok)
            {
                ShowInfoBar(LicenseActivateInfoBar, InfoBarSeverity.Success, "Activated. Welcome to Dimmy.");
                RefreshLicenseStatus();
            }
            else
            {
                ShowInfoBar(LicenseActivateInfoBar, InfoBarSeverity.Error, r.Error ?? "Activation failed.");
            }
        }
        catch (Exception ex)
        {
            ShowInfoBar(LicenseActivateInfoBar, InfoBarSeverity.Error, ex.Message);
        }
    }

    private async void License_Refresh_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var r = await LicenseService.RefreshAsync();
            // Status either way — the user wants to see the result.
            RefreshLicenseStatus();
            if (!r.Ok)
                ShowInfoBar(LicenseActivateInfoBar, InfoBarSeverity.Warning, r.Error ?? "Refresh failed.");
        }
        catch (Exception ex)
        {
            ShowInfoBar(LicenseActivateInfoBar, InfoBarSeverity.Error, ex.Message);
        }
    }

    private async void License_Clear_Click(object sender, RoutedEventArgs e)
    {
        // Confirm dialog with the message users keep missing: signing out
        // removes the license from THIS device only — the Stripe sub
        // stays alive and keeps billing. To stop billing, use Manage
        // subscription. Without this dialog, users sign out then click
        // Buy expecting to "start over" and end up with a duplicate
        // charge that the server-side gate has to refund.
        var dlg = new ContentDialog
        {
            Title = "Sign out from this device?",
            Content =
                "Your subscription on Stripe will stay active and will keep " +
                "billing on its renewal date. To cancel billing, use 'Manage subscription' instead.\n\n" +
                "If you sign out, your activation token on this device is removed. " +
                "You can sign in again from the same email — we'll resend the magic link.",
            PrimaryButtonText = "Sign out",
            SecondaryButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Secondary,
            XamlRoot = this.Content?.XamlRoot,
        };
        if ((await dlg.ShowAsync()) != ContentDialogResult.Primary) return;
        LicenseService.Clear();
        RefreshLicenseStatus();
    }

    /// <summary>
    /// "Manage subscription" — POST /api/billing-portal with the on-disk
    /// token, open the returned Stripe Customer Portal URL in the
    /// system browser. Read the token directly from the license file
    /// rather than going through FFI: this is a one-shot operation
    /// (no need for the Rust HTTP wrapper) and keeps the rebuild
    /// surface to a UI-only change.
    /// </summary>
    private async void License_ManageSubscription_Click(object sender, RoutedEventArgs e)
    {
        // Goes through the licensing FFI so the call hits whichever
        // server URL was embedded at build time via
        // DIMMY_LICENSE_SERVER_URL (staging → license-staging.dimmy.app,
        // prod → license.dimmy.app, debug → localhost mock). The
        // runtime override that used to live behind a Settings text
        // box has been removed for safety.
        LicenseManageSubButton.IsEnabled = false;
        try
        {
            var r = await LicenseService.BillingPortalUrlAsync();
            if (!r.Ok || string.IsNullOrEmpty(r.Url))
            {
                ShowInfoBar(LicenseManageSubInfoBar, InfoBarSeverity.Error,
                    r.Error ?? "Portal response missing URL.");
                return;
            }
            await global::Windows.System.Launcher.LaunchUriAsync(new Uri(r.Url));
        }
        catch (Exception ex)
        {
            ShowInfoBar(LicenseManageSubInfoBar, InfoBarSeverity.Error,
                $"Cannot open portal: {ex.Message}");
        }
        finally
        {
            LicenseManageSubButton.IsEnabled = true;
        }
    }

    private async void License_DevicesReload_Click(object sender, RoutedEventArgs e)
    {
        await RefreshDevicesAsync();
    }

    /// Show / hide + relabel the Buy card and individual tier buttons
    /// based on the current status. Tier-aware: hide buttons at-or-below
    /// the active tier so an Active{Monthly} user only sees Annual +
    /// Lifetime as upgrade options, an Active{Annual} user only sees
    /// Lifetime, and an Active{Lifetime} user sees no Buy at all
    /// (lifetime is the ceiling). Trial → all three (legitimate
    /// trial→paid). NotFound / TrialExpired / Expired / Suspended → all
    /// three (first purchase or repurchase). Unrestricted / Invalid →
    /// the whole card stays hidden.
    ///
    /// Plan-change for active users (downgrade, cancel) goes through
    /// the Stripe Customer Portal, NOT through these Buy buttons —
    /// the portal hint TextBlock surfaces this when relevant so the
    /// user doesn't accidentally start a duplicate subscription.
    private void ApplyBuyCardForStatus(LicenseService.Status s)
    {
        string headline = string.Empty;
        string detail = string.Empty;
        bool show = false;
        bool showMonthly = true, showAnnual = true, showLifetime = true;
        string monthlyLabel = "Monthly";
        string annualLabel = "Annual";
        string lifetimeLabel = "Lifetime";
        bool showPortalHint = false;

        switch (s.Kind)
        {
            case "NotFound":
                headline = "Buy a license";
                detail = "Pick a plan and Stripe will email you a magic link to activate immediately. No trial required.";
                show = true;
                break;
            case "TrialActive":
                headline = "Upgrade to Pro";
                detail = "Skip the trial and unlock cloud features without interruption.";
                show = true;
                break;
            case "TrialExpired":
                headline = "Trial ended — buy to continue";
                detail = "Cloud features are paused. Pick a plan to re-activate. Local + BYOK keep working free either way.";
                show = true;
                break;
            case "Expired":
                headline = "Renew your license";
                detail = "Cloud features are paused. Pick a plan to re-activate.";
                show = true;
                break;
            case "Suspended":
                headline = "Resume your license";
                detail = "Pick a plan to restore cloud features.";
                show = true;
                break;
            case "Active":
                switch (s.Tier?.ToLowerInvariant())
                {
                    case "monthly":
                        headline = "Upgrade your plan";
                        detail = "Switch to Annual for the best value, or Lifetime to skip renewals entirely.";
                        show = true;
                        showMonthly = false;
                        annualLabel = "Switch to Annual";
                        lifetimeLabel = "Upgrade to Lifetime";
                        showPortalHint = true;
                        break;
                    case "annual":
                        headline = "Change your plan";
                        detail = "Drop to Monthly (you'll get a credit on the next invoice) or jump to Lifetime for one final payment.";
                        show = true;
                        showMonthly = true;          // ← reveal Switch to Monthly
                        showAnnual = false;
                        monthlyLabel = "Switch to Monthly";
                        lifetimeLabel = "Upgrade to Lifetime";
                        showPortalHint = true;
                        break;
                    case "lifetime":
                        // Ceiling tier — nothing above it.
                        show = false;
                        break;
                    default:
                        // Unknown tier (future-proof): stay hidden,
                        // operator decides via portal.
                        show = false;
                        break;
                }
                break;
            // Unrestricted, Invalid, anything else → hidden.
        }

        LicenseBuyCard.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
        if (!show) return;

        LicenseBuyHeadline.Text = headline;
        LicenseBuyDetail.Text = detail;
        LicenseBuyMonthlyButton.Visibility = showMonthly ? Visibility.Visible : Visibility.Collapsed;
        LicenseBuyAnnualButton.Visibility = showAnnual ? Visibility.Visible : Visibility.Collapsed;
        LicenseBuyLifetimeButton.Visibility = showLifetime ? Visibility.Visible : Visibility.Collapsed;
        // Collapse the column too so a hidden button doesn't leave a
        // gap in the row layout.
        LicenseBuyButtonsGrid.ColumnDefinitions[0].Width =
            showMonthly ? new GridLength(1, GridUnitType.Star) : new GridLength(0);
        LicenseBuyButtonsGrid.ColumnDefinitions[1].Width =
            showAnnual ? new GridLength(1, GridUnitType.Star) : new GridLength(0);
        LicenseBuyButtonsGrid.ColumnDefinitions[2].Width =
            showLifetime ? new GridLength(1, GridUnitType.Star) : new GridLength(0);
        LicenseBuyMonthlyLabel.Text = monthlyLabel;
        LicenseBuyAnnualLabel.Text = annualLabel;
        LicenseBuyLifetimeLabel.Text = lifetimeLabel;
        LicenseBuyPortalHint.Visibility = showPortalHint ? Visibility.Visible : Visibility.Collapsed;
    }

    private void License_BuyMonthly_Click(object sender, RoutedEventArgs e) => _ = BuyTierAsync("monthly");
    private void License_BuyAnnual_Click(object sender, RoutedEventArgs e)  => _ = BuyTierAsync("annual");
    private void License_BuyLifetime_Click(object sender, RoutedEventArgs e) => _ = BuyTierAsync("lifetime");

    private static string ToTitleCase(string s) =>
        string.IsNullOrEmpty(s) ? s : char.ToUpperInvariant(s[0]) + s.Substring(1);

    /// <summary>
    /// Modal asking the user for the email tied to their Stripe purchase.
    /// Used by the pre-checkout gate to look up an existing license server-side
    /// before spinning up a Checkout session — closes the post-sign-out
    /// double-charge edge case. Returns the trimmed lowercase email on
    /// confirm, or null on cancel.
    /// </summary>
    private async Task<string?> PromptBuyerEmailAsync(string prefilled, string tier)
    {
        var emailBox = new TextBox
        {
            PlaceholderText = "you@example.com",
            Text = prefilled ?? string.Empty,
            MinWidth = 280,
        };
        var helpText = new TextBlock
        {
            Text =
                "Used to look up an existing license + match the Stripe customer. " +
                "If you've bought before with this email, we'll resend the magic link " +
                "instead of charging you again.",
            FontSize = 12,
            TextWrapping = TextWrapping.Wrap,
            Foreground = (Microsoft.UI.Xaml.Media.Brush)
                Application.Current.Resources["TextFillColorSecondaryBrush"],
            Margin = new Thickness(0, 8, 0, 0),
        };
        var stack = new StackPanel { Spacing = 4 };
        stack.Children.Add(emailBox);
        stack.Children.Add(helpText);

        var dlg = new ContentDialog
        {
            Title = $"Continue to {ToTitleCase(tier)} checkout",
            Content = stack,
            PrimaryButtonText = "Continue",
            SecondaryButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = this.Content?.XamlRoot,
        };
        // Disable Continue until the input looks like an email.
        dlg.IsPrimaryButtonEnabled = LooksLikeEmail(emailBox.Text);
        emailBox.TextChanged += (_, _) =>
            dlg.IsPrimaryButtonEnabled = LooksLikeEmail(emailBox.Text);

        var result = await dlg.ShowAsync();
        if (result != ContentDialogResult.Primary) return null;
        var trimmed = (emailBox.Text ?? string.Empty).Trim().ToLowerInvariant();
        return LooksLikeEmail(trimmed) ? trimmed : null;
    }

    private static bool LooksLikeEmail(string s) =>
        !string.IsNullOrWhiteSpace(s)
        && s.Contains('@')
        && s.Contains('.')
        && s.Length < 254;

    private async Task BuyTierAsync(string tier)
    {
        // Distinguish "plan change" (Active monthly⇄annual) from "first
        // purchase / lifetime upgrade". Plan change goes through
        // /api/plan-change (subscription update + proration) so the user
        // is NOT charged a fresh full-price invoice on top of their
        // existing sub. Anything else (first purchase, trial→paid,
        // sub→lifetime, expired/suspended renew) goes through Stripe
        // Checkout as before.
        try
        {
            DisableBuyButtons(true);
            var status = LicenseService.GetStatus();
            bool isPlanChange =
                status.Kind == "Active" &&
                (status.Tier == "monthly" || status.Tier == "annual") &&
                (tier == "monthly" || tier == "annual");

            if (isPlanChange)
            {
                // Confirm dialog so the user knows the click mutates an
                // existing sub (proration on next invoice, no second
                // card prompt) rather than opening a fresh Checkout.
                // Without it, the click 'flagga istantaneamente' the
                // new tier and the silent UX feels off.
                var confirmDialog = new ContentDialog
                {
                    Title = $"Switch plan to {ToTitleCase(tier)}?",
                    Content =
                        $"You're already subscribed (current: {ToTitleCase(status.Tier ?? "")}). " +
                        $"Switching to {tier} mutates your existing subscription:\n\n" +
                        "• No new payment now — Stripe reuses your saved card.\n" +
                        "• Stripe issues a prorated invoice on the next billing date " +
                        "(credit for unused days of the old plan, debit for the new one).\n" +
                        "• No magic-link email — your license stays active, just the tier changes.",
                    PrimaryButtonText = $"Switch to {ToTitleCase(tier)}",
                    SecondaryButtonText = "Cancel",
                    DefaultButton = ContentDialogButton.Primary,
                    XamlRoot = this.Content?.XamlRoot,
                };
                var result = await confirmDialog.ShowAsync();
                if (result != ContentDialogResult.Primary)
                {
                    ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Informational,
                        "Plan change cancelled.");
                    return;
                }
                ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Informational,
                    $"Switching plan to {tier}…");
                var r = await LicenseService.PlanChangeAsync(tier);
                if (!r.Ok)
                {
                    ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Error,
                        r.Error ?? "Plan change failed.");
                    return;
                }
                // Stripe webhook will fire customer.subscription.updated;
                // give it a beat, then refresh + re-render UI. The
                // refresh-and-render loop also renames the badge.
                await Task.Delay(1500);
                var refresh = await LicenseService.RefreshAsync();
                RefreshLicenseStatus();
                if (refresh.Ok)
                {
                    ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Success,
                        $"Plan switched to {tier}. Stripe will issue a prorated invoice automatically.");
                }
                else
                {
                    ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Informational,
                        $"Plan switched to {tier}. Refresh in a moment to see the new badge.");
                }
                return;
            }

            // Pre-checkout email gate: ask the user for their email
            // before minting the Stripe Checkout URL. The server uses
            // the email to look up an existing license and 409 BEFORE
            // Stripe charges the card. Pre-fill with whatever we have
            // saved in UiPreferences (survives Sign out — no auth, just
            // UX convenience).
            var prefs = Services.UiPreferences.Load();
            var promptedEmail = await PromptBuyerEmailAsync(
                prefilled: prefs.BuyerEmail ?? string.Empty,
                tier: tier);
            if (promptedEmail is null)
            {
                ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Informational,
                    "Purchase cancelled.");
                return;
            }
            // Persist for next time.
            prefs.BuyerEmail = promptedEmail;
            prefs.Save();

            ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Informational, $"Opening Stripe checkout for {tier}…");
            var c = await LicenseService.CreateCheckoutAsync(tier, promptedEmail);
            if (!c.Ok || string.IsNullOrEmpty(c.Url))
            {
                // 409 path → license already exists for this email. Offer
                // 'Send magic link instead' fallback (re-issues activation
                // email for the existing license via /api/trial/start).
                if (c.StatusCode == 409 && !string.IsNullOrEmpty(c.CurrentTier))
                {
                    var dlg = new ContentDialog
                    {
                        Title = $"You already have a {c.CurrentTier} license",
                        Content =
                            $"The email {promptedEmail} is already linked to an active {c.CurrentTier} license. " +
                            "We can resend the activation magic link for that license — " +
                            "no new payment, no second sub.",
                        PrimaryButtonText = "Send magic link",
                        SecondaryButtonText = "Cancel",
                        DefaultButton = ContentDialogButton.Primary,
                        XamlRoot = this.Content?.XamlRoot,
                    };
                    if ((await dlg.ShowAsync()) == ContentDialogResult.Primary)
                    {
                        var t = await LicenseService.RequestTrialAsync(promptedEmail);
                        ShowInfoBar(LicenseBuyInfoBar,
                            t.Ok ? InfoBarSeverity.Success : InfoBarSeverity.Error,
                            t.Ok
                                ? $"Magic link sent to {promptedEmail}. Check your inbox to activate."
                                : (t.Error ?? "Could not resend magic link."));
                    }
                    else
                    {
                        ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Informational,
                            "Cancelled.");
                    }
                    return;
                }
                ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Error, c.Error ?? "Could not start checkout.");
                return;
            }
            await global::Windows.System.Launcher.LaunchUriAsync(new Uri(c.Url));
            ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Success,
                "Checkout opened in your browser. After payment, check your email for the magic link to activate.");
        }
        catch (Exception ex)
        {
            ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Error, ex.Message);
        }
        finally
        {
            DisableBuyButtons(false);
        }
    }

    private void DisableBuyButtons(bool disabled)
    {
        LicenseBuyMonthlyButton.IsEnabled = !disabled;
        LicenseBuyAnnualButton.IsEnabled = !disabled;
        LicenseBuyLifetimeButton.IsEnabled = !disabled;
    }

    private async Task RefreshDevicesAsync()
    {
        try
        {
            LicenseDeviceCountLabel.Text = "Loading…";
            var list = await LicenseService.ListDevicesAsync();
            if (!list.Ok)
            {
                LicenseDeviceCountLabel.Text = list.Error ?? "Failed to fetch devices";
                LicenseDevicesList.Children.Clear();
                return;
            }
            int activeCount = list.Devices.Count(d => d.Status == "active");
            LicenseDeviceCountLabel.Text =
                $"{activeCount} active / {list.MaxDevices} max";
            LicenseDevicesList.Children.Clear();
            foreach (var d in list.Devices)
            {
                LicenseDevicesList.Children.Add(BuildDeviceRow(d));
            }
        }
        catch (Exception ex)
        {
            LicenseDeviceCountLabel.Text = ex.Message;
        }
    }

    private FrameworkElement BuildDeviceRow(LicenseService.DeviceInfo d)
    {
        bool active = d.Status == "active";
        var row = new Border
        {
            Padding = new Thickness(12, 8, 12, 8),
            CornerRadius = new CornerRadius(4),
            Background = (Microsoft.UI.Xaml.Media.Brush)
                Application.Current.Resources["SubtleFillColorSecondaryBrush"],
        };
        var grid = new Grid { ColumnSpacing = 12 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var labelStack = new StackPanel { Spacing = 1 };
        var nameRun = new TextBlock
        {
            Text = string.IsNullOrEmpty(d.Label) ? "(unnamed device)" : d.Label,
            FontSize = 13,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
        };
        if (d.IsSelf)
        {
            nameRun.Inlines.Add(new Microsoft.UI.Xaml.Documents.Run
            {
                Text = "  · this device",
                FontWeight = Microsoft.UI.Text.FontWeights.Normal,
                Foreground = (Microsoft.UI.Xaml.Media.Brush)
                    Application.Current.Resources["TextFillColorTertiaryBrush"],
            });
        }
        labelStack.Children.Add(nameRun);
        labelStack.Children.Add(new TextBlock
        {
            Text = active
                ? $"Last seen: {DateTimeOffset.FromUnixTimeSeconds(d.LastSeen).LocalDateTime:g}"
                : $"Status: {d.Status}",
            FontSize = 11,
            Foreground = (Microsoft.UI.Xaml.Media.Brush)
                Application.Current.Resources["TextFillColorSecondaryBrush"],
        });
        Grid.SetColumn(labelStack, 0);

        var btn = new Button
        {
            Content = d.IsSelf ? "Sign out this device" : "Sign out",
            IsEnabled = active,
            VerticalAlignment = VerticalAlignment.Center,
        };
        btn.Click += async (_, _) => await DeactivateDeviceAsync(d);
        Grid.SetColumn(btn, 1);

        grid.Children.Add(labelStack);
        grid.Children.Add(btn);
        row.Child = grid;
        return row;
    }

    private async Task DeactivateDeviceAsync(LicenseService.DeviceInfo d)
    {
        try
        {
            var r = await LicenseService.DeactivateDeviceAsync(d.IsSelf ? null : d.DeviceId);
            if (!r.Ok)
            {
                LicenseDeviceCountLabel.Text = r.Error ?? "Deactivation failed";
                return;
            }
            // Self-sign-out clears the local license file; refresh status will
            // flip the page back to NotFound. Otherwise just reload the list.
            RefreshLicenseStatus();
        }
        catch (Exception ex)
        {
            LicenseDeviceCountLabel.Text = ex.Message;
        }
    }

    private static string? ExtractCode(string? input)
    {
        if (string.IsNullOrEmpty(input)) return null;
        var idx = input.IndexOf("code=", StringComparison.OrdinalIgnoreCase);
        if (idx < 0) return input.Trim();
        var rest = input.Substring(idx + 5);
        var amp = rest.IndexOf('&');
        return amp >= 0 ? rest.Substring(0, amp) : rest;
    }

    private static void ShowInfoBar(InfoBar bar, InfoBarSeverity sev, string msg)
    {
        bar.Severity = sev;
        bar.Message = msg;
        bar.IsOpen = true;
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
        // Append " · STAGING" suffix on staging builds. The sidebar banner
        // already announces the flavor loudly; this just makes sure About
        // page screenshots can never be mistaken for prod ones.
        var flavorSuffix = BuildInfo.IsStaging ? " · STAGING" : string.Empty;
        HeroTitleText.Text = $"Dimmy {_currentVersion}{flavorSuffix}";
        HeroSubText.Text = $"Version {_currentVersion}{flavorSuffix}";
        // Sidebar staging banner — flip on once we know the flavor. Done
        // here (rather than in the constructor) because XAML elements
        // are initialised lazily.
        if (StagingBanner is not null)
            StagingBanner.Visibility = BuildInfo.IsStaging
                ? Microsoft.UI.Xaml.Visibility.Visible
                : Microsoft.UI.Xaml.Visibility.Collapsed;
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
