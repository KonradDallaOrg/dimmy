using System;
using System.Diagnostics;
using System.IO;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Interop;
using Dimmy.Windows.Services;
using Dimmy.Windows.ViewModels;

namespace Dimmy.Windows.Views;

public sealed partial class OnboardingWindow : Window
{
    public OnboardingViewModel ViewModel { get; } = new();

    /// Sentinel tag for the Parakeet entry in OnboardingLocalModelComboBox.
    /// Distinguished from whisper filenames by prefix.
    private const string ParakeetTag = "parakeet:fp32";

    private ModelPrefetchService _prefetch = new();
    private readonly DispatcherQueue _dq = DispatcherQueue.GetForCurrentThread();
    private CancellationTokenSource? _keyValidateCts;
    private CancellationTokenSource? _parakeetDownloadCts;
    private bool _onboardingLoaded;

    public OnboardingWindow()
    {
        this.InitializeComponent();
        ((FrameworkElement)Content).DataContext = ViewModel;
        Title = "Dimmy";

        var appWindow = WindowHelper.GetAppWindow(this);
        WindowHelper.ResizeLogical(this, 680, 600);
        if (appWindow?.Presenter is Microsoft.UI.Windowing.OverlappedPresenter presenter)
        {
            presenter.IsResizable = true;
            presenter.IsMaximizable = false;
        }

        _prefetch.StateChanged += Prefetch_StateChanged;
        Closed += OnboardingWindow_Closed;

        // Subscribe to Parakeet download progress (FFI-routed), so the
        // same DownloadPercent / status binding the whisper prefetch
        // updates is also driven by the Rust core when the user picks
        // Parakeet from the ComboBox.
        if (Application.Current is App app)
        {
            app.AppViewModel.ParakeetDownloadProgress += OnParakeetDownloadProgress;
        }

        DetectPriorState();
        PopulateOnboardingModelCombo();
        _prefetch.StartBasePrefetch();
        _onboardingLoaded = true;
    }

    private void PopulateOnboardingModelCombo()
    {
        try
        {
            var json = DimmyNative.ListLocalModels();
            if (string.IsNullOrEmpty(json)) return;

            using var doc = JsonDocument.Parse(json);
            OnboardingLocalModelComboBox.Items.Clear();
            int defaultIdx = 0;
            int idx = 0;
            foreach (var el in doc.RootElement.EnumerateArray())
            {
                var name = el.GetProperty("name").GetString() ?? "";
                var filename = el.GetProperty("filename").GetString() ?? "";
                var sizeMb = el.GetProperty("size_mb").GetInt32();
                var downloaded = el.GetProperty("downloaded").GetBoolean();
                var status = downloaded ? "Ready" : $"{sizeMb}MB";
                OnboardingLocalModelComboBox.Items.Add(new ComboBoxItem
                {
                    Content = $"{name} ({status})",
                    Tag = filename,
                });
                if (filename == ModelPaths.BaseModelFilename)
                    defaultIdx = idx;
                idx++;
            }

            bool parakeetReady = false;
            try { parakeetReady = DimmyNative.dimmy_parakeet_bundle_present() == 1; }
            catch { }
            OnboardingLocalModelComboBox.Items.Add(new ComboBoxItem
            {
                Content = $"Parakeet TDT v3 FP32 ({(parakeetReady ? "Ready" : "2.5GB")})",
                Tag = ParakeetTag,
            });

            OnboardingLocalModelComboBox.SelectedIndex = defaultIdx;
            ViewModel.SelectedLocalModelTag = ModelPaths.BaseModelFilename;
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[Onboarding] PopulateOnboardingModelCombo: {ex.Message}");
        }
    }

    private void OnboardingLocalModel_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!_onboardingLoaded) return;
        if (OnboardingLocalModelComboBox.SelectedItem is not ComboBoxItem item) return;
        if (item.Tag is not string tag || tag == ViewModel.SelectedLocalModelTag) return;

        ViewModel.SelectedLocalModelTag = tag;

        // Tear down whatever is in flight: a whisper download via the
        // prefetch service, or a Parakeet FFI download.
        try { _prefetch.StateChanged -= Prefetch_StateChanged; } catch { }
        try { _prefetch.Dispose(); } catch { }
        _parakeetDownloadCts?.Cancel();
        _parakeetDownloadCts = null;

        // Reset the visible progress so the user immediately sees the
        // new download starting fresh.
        ViewModel.DownloadPercent = 0;
        ViewModel.DownloadBytesText = "";
        ViewModel.IsLocalReady = false;
        ViewModel.IsLocalFailed = false;

        if (tag == ParakeetTag)
        {
            ViewModel.DownloadStatusText = "Starting Parakeet download...";
            StartParakeetDownload();
        }
        else
        {
            ViewModel.DownloadStatusText = "Starting download...";
            _prefetch = new ModelPrefetchService();
            _prefetch.StateChanged += Prefetch_StateChanged;
            long expected = WhisperExpectedSize(tag);
            _prefetch.StartFor(tag, expected);
        }
    }

    /// Look up the expected size of a whisper model file from the Rust
    /// core's manifest (dimmy_list_local_models JSON). Falls back to a
    /// generic estimate when not found, which only affects the initial
    /// progress bar before Content-Length comes back.
    private static long WhisperExpectedSize(string filename)
    {
        try
        {
            var json = DimmyNative.ListLocalModels();
            if (string.IsNullOrEmpty(json)) return 200L * 1024 * 1024;
            using var doc = JsonDocument.Parse(json);
            foreach (var el in doc.RootElement.EnumerateArray())
            {
                var fn = el.GetProperty("filename").GetString();
                if (fn == filename)
                    return (long)el.GetProperty("size_mb").GetInt32() * 1024L * 1024L;
            }
        }
        catch { }
        return 200L * 1024 * 1024;
    }

    private void StartParakeetDownload()
    {
        var cts = new CancellationTokenSource();
        _parakeetDownloadCts = cts;
        Task.Run(() =>
        {
            int rc = DimmyNative.dimmy_parakeet_download_bundle();
            _dq.TryEnqueue(() =>
            {
                if (cts.IsCancellationRequested) return;
                if (rc == 0)
                {
                    ViewModel.IsLocalReady = true;
                    ViewModel.IsLocalFailed = false;
                    ViewModel.DownloadPercent = 100;
                    ViewModel.DownloadStatusText = "Ready";
                }
                else
                {
                    ViewModel.IsLocalFailed = true;
                    ViewModel.IsLocalReady = false;
                    ViewModel.DownloadStatusText = "Download failed";
                    ViewModel.LocalErrorText = "Parakeet bundle download failed";
                }
            });
        });
    }

    private void OnParakeetDownloadProgress(long downloaded, long total)
    {
        // Only update the onboarding UI while the user has Parakeet
        // selected — otherwise a stale event from a cancelled download
        // would clobber the whisper prefetch progress.
        if (ViewModel.SelectedLocalModelTag != ParakeetTag) return;
        if (total <= 0)
        {
            ViewModel.DownloadStatusText = "Downloading";
            ViewModel.DownloadBytesText = $"{downloaded / 1024.0 / 1024.0:0} MB";
            return;
        }
        ViewModel.DownloadPercent = Math.Min(100, downloaded * 100.0 / total);
        ViewModel.DownloadStatusText = "Downloading";
        ViewModel.DownloadBytesText = $"{downloaded / 1024.0 / 1024.0:0} / {total / 1024.0 / 1024.0:0} MB";
        ViewModel.IsLocalReady = false;
    }

    private void DetectPriorState()
    {
        try
        {
            var baseFile = ModelPaths.GetModelFilePath(ModelPaths.BaseModelFilename);
            if (File.Exists(baseFile) && new FileInfo(baseFile).Length > ModelPaths.BaseModelSizeBytes / 2)
            {
                ViewModel.IsLocalReady = true;
                ViewModel.DownloadPercent = 100;
                ViewModel.DownloadStatusText = "Ready";
                ViewModel.DownloadBytesText = "Already downloaded";
            }
        }
        catch (Exception ex) { Debug.WriteLine($"[Onboarding] DetectPriorState local: {ex.Message}"); }

        try
        {
            if (DimmyNative.dimmy_has_api_key() == 1)
            {
                ViewModel.IsCloudReady = true;
                ViewModel.CloudErrorText = "Key already configured";
            }
        }
        catch (Exception ex) { Debug.WriteLine($"[Onboarding] DetectPriorState cloud: {ex.Message}"); }
    }

    private void Prefetch_StateChanged(object? sender, ModelDownloadState state)
    {
        _dq.TryEnqueue(() =>
        {
            long total = state.BaseTotalBytes > 0 ? state.BaseTotalBytes : ModelPaths.BaseModelSizeBytes;
            double percent = total == 0 ? 0 : Math.Min(100, state.BaseBytesDownloaded * 100.0 / total);
            ViewModel.DownloadPercent = percent;
            ViewModel.DownloadBytesText = FormatBytes(state.BaseBytesDownloaded, total);

            switch (state.BaseStatus)
            {
                case ModelDownloadStatus.NotStarted:
                    ViewModel.DownloadStatusText = "Preparing";
                    break;
                case ModelDownloadStatus.Downloading:
                    ViewModel.DownloadStatusText = "Downloading";
                    ViewModel.IsLocalReady = false;
                    ViewModel.IsLocalFailed = false;
                    ViewModel.IsLocalOffline = false;
                    break;
                case ModelDownloadStatus.Ready:
                    ViewModel.DownloadStatusText = "Ready";
                    ViewModel.IsLocalReady = true;
                    ViewModel.IsLocalFailed = false;
                    ViewModel.IsLocalOffline = false;
                    break;
                case ModelDownloadStatus.Failed:
                    ViewModel.DownloadStatusText = "Download failed";
                    ViewModel.LocalErrorText = state.BaseError ?? "Unknown error";
                    ViewModel.IsLocalFailed = true;
                    ViewModel.IsLocalReady = false;
                    break;
                case ModelDownloadStatus.Offline:
                    ViewModel.DownloadStatusText = "Offline — connect to download";
                    ViewModel.LocalErrorText = state.BaseError ?? "No internet";
                    ViewModel.IsLocalOffline = true;
                    ViewModel.IsLocalReady = false;
                    break;
                case ModelDownloadStatus.Cancelled:
                    break;
            }
        });
    }

    private static string FormatBytes(long done, long total)
    {
        double doneMb = done / 1024.0 / 1024.0;
        double totalMb = total / 1024.0 / 1024.0;
        return $"{doneMb:0} / {totalMb:0} MB";
    }

    private void LocalCard_Tapped(object sender, TappedRoutedEventArgs e)
    {
        ViewModel.Choice = ModelChoice.Local;
        if (ViewModel.IsLocalFailed) _prefetch.Retry();
    }

    private void CloudCard_Tapped(object sender, TappedRoutedEventArgs e)
    {
        ViewModel.Choice = ModelChoice.Cloud;
        GroqKeyBox?.Focus(FocusState.Programmatic);
    }

    private void GroqKeyBox_PasswordChanged(object sender, RoutedEventArgs e)
    {
        ViewModel.GroqApiKey = GroqKeyBox.Password;
        ViewModel.IsCloudReady = false;
        ViewModel.CloudErrorText = "";
        ScheduleKeyValidation();
    }

    private void ScheduleKeyValidation()
    {
        _keyValidateCts?.Cancel();
        _keyValidateCts = new CancellationTokenSource();
        var ct = _keyValidateCts.Token;
        var key = ViewModel.GroqApiKey;

        _ = Task.Run(async () =>
        {
            try { await Task.Delay(500, ct).ConfigureAwait(false); }
            catch (OperationCanceledException) { return; }

            if (string.IsNullOrWhiteSpace(key)) return;

            _dq.TryEnqueue(() => ViewModel.IsValidatingKey = true);

            GroqKeyValidationResult result;
            try { result = await GroqKeyValidator.ValidateAsync(key, ct).ConfigureAwait(false); }
            catch (OperationCanceledException) { return; }

            _dq.TryEnqueue(() =>
            {
                if (ct.IsCancellationRequested) return;
                ViewModel.IsValidatingKey = false;
                ViewModel.IsCloudReady = result.IsValid;
                ViewModel.CloudErrorText = result.IsValid ? "Key verified" : (result.Error ?? "");
            });
        }, ct);
    }

    private void NextStep_Click(object sender, RoutedEventArgs e)
    {
        // Persist model choice when leaving the choice step
        if (ViewModel.CurrentStep == 1)
        {
            PersistModelChoice();
        }

        ViewModel.NextStep();

        // Reaching "Try It" — activate pill so user can test recording
        if (ViewModel.CurrentStep == 3)
        {
            App.Instance?.ShowPillAndHotkey();
        }
    }

    private void PersistModelChoice()
    {
        try
        {
            string json;
            switch (ViewModel.Choice)
            {
                case ModelChoice.Local:
                    bool isParakeet = ViewModel.SelectedLocalModelTag == ParakeetTag;
                    json = JsonSerializer.Serialize(new
                    {
                        stt_mode = "local",
                        local_model = isParakeet
                            ? ModelPaths.BaseModelFilename
                            : (string.IsNullOrEmpty(ViewModel.SelectedLocalModelTag)
                                ? ModelPaths.BaseModelFilename
                                : ViewModel.SelectedLocalModelTag),
                        local_stt_backend = isParakeet ? "parakeet" : "whisper",
                    });
                    break;
                case ModelChoice.Cloud:
                    json = JsonSerializer.Serialize(new
                    {
                        stt_mode = "cloud",
                        api_key = ViewModel.GroqApiKey.Trim(),
                    });
                    break;
                default:
                    return;
            }
            DimmyNative.dimmy_set_config_json(json);
            App.Instance?.ReloadConfig();
            App.MarkOnboardingComplete();
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[Onboarding] PersistModelChoice failed: {ex.Message}");
        }
    }

    private void PrevStep_Click(object sender, RoutedEventArgs e) => ViewModel.PreviousStep();

    private void FinishOnboarding_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var configJson = JsonSerializer.Serialize(new
            {
                shortcut = ViewModel.Shortcut,
                shortcut_mode = ViewModel.ShortcutMode,
            });
            DimmyNative.dimmy_set_config_json(configJson);
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[Onboarding] FinishOnboarding persist failed: {ex.Message}");
        }

        this.Close();
    }

    private void OnboardingWindow_Closed(object sender, WindowEventArgs args)
    {
        _keyValidateCts?.Cancel();
        _parakeetDownloadCts?.Cancel();
        try { _prefetch.StateChanged -= Prefetch_StateChanged; } catch { }
        try { _prefetch.Dispose(); } catch { }
        if (Application.Current is App app)
        {
            app.AppViewModel.ParakeetDownloadProgress -= OnParakeetDownloadProgress;
        }
    }
}
