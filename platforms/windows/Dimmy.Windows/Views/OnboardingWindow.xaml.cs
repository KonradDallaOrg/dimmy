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
    private readonly DateTime _onboardingStartedAt = DateTime.UtcNow;
    private bool _onboardingCompleted; // distinguishes Finish vs window-close abandonment

    /// Map Win wizard step index → canonical step name shared with Mac
    /// + Rust telemetry allowlist. Keep aligned with the strings in
    /// `core/src/ffi.rs::dimmy_telemetry_track_typed`'s onboarding
    /// allowlist (welcome / provider / shortcut / try_it on Win;
    /// permissions + model_download exist only on Mac).
    private static string OnboardingStepName(int step) => step switch
    {
        0 => "welcome",
        1 => "provider",
        2 => "shortcut",
        3 => "try_it",
        _ => "welcome",
    };

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

        // Bootstrap the Parakeet download if it was set as the default
        // selection in PopulateOnboardingModelCombo. The selection-changed
        // handler returned early during init because `_onboardingLoaded`
        // was still false, so without this nudge the bundle never starts
        // downloading — meanwhile DetectPriorState may have set
        // IsLocalReady=true from a stale whisper base bin, enabling
        // Continue and letting the user finish onboarding with a
        // missing Parakeet bundle. Burned 2026-05-20 (v0.6.47).
        BootstrapDefaultLocalModelDownload();

        // Funnel anchor. Fires once per wizard launch — if the user
        // dismissed the window before, AppDelegate / startup logic
        // re-shows the window, that re-fires `.started` which is the
        // documented behaviour (each abandonment + re-entry is a new
        // funnel attempt in PostHog).
        DimmyNative.TrackEvent("onboarding.started");
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
            int parakeetIdx = OnboardingLocalModelComboBox.Items.Count - 1;

            // Parakeet TDT v3 is the recommended Local default — better
            // quality than Whisper base, runs well on CPU. When the
            // bundle is already present pick it outright; when it isn't,
            // also prefer it (label clearly shows "2.5GB" so the user
            // sees the download cost). Falls back to Whisper Base only
            // if the Parakeet item failed to mount (defensive).
            if (parakeetIdx >= 0)
            {
                OnboardingLocalModelComboBox.SelectedIndex = parakeetIdx;
                ViewModel.SelectedLocalModelTag = ParakeetTag;
            }
            else
            {
                OnboardingLocalModelComboBox.SelectedIndex = defaultIdx;
                ViewModel.SelectedLocalModelTag = ModelPaths.BaseModelFilename;
            }
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

    /// Kick off whatever the selected local-model default needs to
    /// reach Ready state, and reset `IsLocalReady` to reflect the
    /// ACTUAL state of THAT model rather than whatever DetectPriorState
    /// found (which always looks at whisper base regardless of which
    /// model is selected). Called once after `_onboardingLoaded` flips
    /// true so the SelectionChanged guard doesn't suppress us anymore.
    private void BootstrapDefaultLocalModelDownload()
    {
        try
        {
            if (ViewModel.SelectedLocalModelTag != ParakeetTag) return;

            bool parakeetReady = false;
            try { parakeetReady = DimmyNative.dimmy_parakeet_bundle_present() == 1; }
            catch { }

            if (parakeetReady)
            {
                ViewModel.IsLocalReady = true;
                ViewModel.DownloadPercent = 100;
                ViewModel.DownloadStatusText = "Ready";
                ViewModel.DownloadBytesText = "Already downloaded";
                return;
            }

            // Bundle missing — clear any stale "ready" state inherited
            // from DetectPriorState (which probes whisper base, not
            // Parakeet) and start the FFI download. Without this clear
            // the Continue button stays enabled and the user can finish
            // onboarding with the Parakeet bundle still half-downloaded.
            ViewModel.IsLocalReady = false;
            ViewModel.IsLocalFailed = false;
            ViewModel.DownloadPercent = 0;
            ViewModel.DownloadBytesText = "";
            ViewModel.DownloadStatusText = "Starting Parakeet download...";
            StartParakeetDownload();
        }
        catch (Exception ex)
        {
            Debug.WriteLine($"[Onboarding] BootstrapDefaultLocalModelDownload: {ex.Message}");
        }
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
        // Always clear "ready" while the download is in flight,
        // regardless of whether `total` is known yet. Before the HEAD
        // requests resolve, total=0 and the previous code path was
        // skipping the IsLocalReady reset — so a stale TRUE from
        // DetectPriorState (whisper base bin present) could survive
        // and enable Continue while Parakeet was still downloading.
        ViewModel.IsLocalReady = false;
        if (total <= 0)
        {
            ViewModel.DownloadStatusText = "Downloading";
            ViewModel.DownloadBytesText = $"{downloaded / 1024.0 / 1024.0:0} MB";
            return;
        }
        ViewModel.DownloadPercent = Math.Min(100, downloaded * 100.0 / total);
        ViewModel.DownloadStatusText = "Downloading";
        ViewModel.DownloadBytesText = $"{downloaded / 1024.0 / 1024.0:0} / {total / 1024.0 / 1024.0:0} MB";
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
                    ViewModel.DownloadStatusText = "Offline. Connect to download.";
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

    // Subscriber to AppViewModel.TranscriptReady installed when the
    // wizard enters Step 3 ("Try It"), removed when the wizard
    // advances away from it OR when the window closes. Stored so the
    // unsubscribe always pairs with the subscribe.
    private Action<string>? _trialTranscriptHandler;

    private void SubscribeTrialTranscript()
    {
        if (_trialTranscriptHandler != null) return;
        var appVm = App.Instance?.AppViewModel;
        if (appVm == null) return;
        _trialTranscriptHandler = text =>
        {
            _dq.TryEnqueue(() =>
            {
                ViewModel.TrialText = text;
                ViewModel.IsTrialSuccess = !string.IsNullOrWhiteSpace(text);
                ViewModel.IsRecordingTrial = false;
            });
        };
        appVm.TranscriptReady += _trialTranscriptHandler;
    }

    private void UnsubscribeTrialTranscript()
    {
        if (_trialTranscriptHandler == null) return;
        var appVm = App.Instance?.AppViewModel;
        if (appVm != null) appVm.TranscriptReady -= _trialTranscriptHandler;
        _trialTranscriptHandler = null;
    }

    private void NextStep_Click(object sender, RoutedEventArgs e)
    {
        // Capture the step the user is LEAVING — this is the one that's
        // "completed" (they finished it and are advancing). The post-
        // increment ViewModel.CurrentStep change happens below.
        var leavingStep = OnboardingStepName(ViewModel.CurrentStep);

        // Persist model choice when leaving the choice step
        if (ViewModel.CurrentStep == 1)
        {
            PersistModelChoice();
        }

        // Leaving Step 3 → unsubscribe so a transcript captured AFTER
        // the wizard closes doesn't try to push into a stale ViewModel.
        if (ViewModel.CurrentStep == 3) UnsubscribeTrialTranscript();

        ViewModel.NextStep();

        DimmyNative.TrackEvent("onboarding.step_completed", new { step = leavingStep });

        // Reaching "Try It" — activate pill so user can test recording,
        // and wire up the transcript preview so what they dictate shows
        // up inline in the TrialText box (otherwise the result is pasted
        // into whatever app has focus + the user sees nothing here).
        if (ViewModel.CurrentStep == 3)
        {
            App.Instance?.ShowPillAndHotkey();
            SubscribeTrialTranscript();
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
                    // Pin the Groq STT endpoint explicitly: the validated key is
                    // a Groq key, but on a re-run config.json may already point
                    // api_url/api_model at another provider (Rust merge keeps
                    // absent keys). Pair must match core/src/lib.rs
                    // DEFAULT_API_URL / DEFAULT_MODEL.
                    json = JsonSerializer.Serialize(new
                    {
                        stt_mode = "cloud",
                        api_key = ViewModel.GroqApiKey.Trim(),
                        api_url = "https://api.groq.com/openai/v1/audio/transcriptions",
                        api_model = "whisper-large-v3-turbo",
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

        // Funnel terminal — the user clicked the explicit "Start
        // using Dimmy" button. Distinguish from the abandonment
        // path (window dismissed before reaching this handler) by
        // setting `_onboardingCompleted` so OnboardingWindow_Closed
        // doesn't double-emit. `path` records the user's STT choice
        // for the cloud-vs-local funnel split.
        _onboardingCompleted = true;
        var path = ViewModel.Choice switch
        {
            ModelChoice.Cloud => "cloud",
            ModelChoice.Local => "local",
            _ => "unknown",
        };
        var duration = (ulong)Math.Max(0, (DateTime.UtcNow - _onboardingStartedAt).TotalSeconds);
        DimmyNative.TrackEvent("onboarding.completed", new
        {
            path,
            duration_secs = duration,
        });

        this.Close();
    }

    private void OnboardingWindow_Closed(object sender, WindowEventArgs args)
    {
        _keyValidateCts?.Cancel();
        _parakeetDownloadCts?.Cancel();
        try { _prefetch.StateChanged -= Prefetch_StateChanged; } catch { }
        try { _prefetch.Dispose(); } catch { }
        // Detach the "Try It" transcript subscriber even if the user
        // closed the window mid-step — otherwise the next dictation
        // after onboarding tries to push into a disposed ViewModel.
        UnsubscribeTrialTranscript();
        if (Application.Current is App app)
        {
            app.AppViewModel.ParakeetDownloadProgress -= OnParakeetDownloadProgress;
        }
        // Abandonment: user dismissed the window without clicking
        // "Start using Dimmy". Skip if FinishOnboarding_Click already
        // fired (the explicit completion path); otherwise emit the
        // funnel terminal so PostHog can compute drop-off-by-step.
        if (!_onboardingCompleted)
        {
            try
            {
                DimmyNative.TrackEvent("onboarding.abandoned", new
                {
                    last_step = OnboardingStepName(ViewModel.CurrentStep),
                });
            }
            catch { /* telemetry must never abort window close */ }
        }
    }
}
