using System;
using System.Diagnostics;
using System.IO;
using System.Text.Json;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Input;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Interop;
using Dimmy.Windows.Services;
using Dimmy.Windows.ViewModels;

namespace Dimmy.Windows.Views;

public sealed partial class OnboardingWindow : Window
{
    public OnboardingViewModel ViewModel { get; } = new();

    private readonly ModelPrefetchService _prefetch = new();
    private readonly DispatcherQueue _dq = DispatcherQueue.GetForCurrentThread();
    private CancellationTokenSource? _keyValidateCts;

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

        DetectPriorState();
        _prefetch.StartBasePrefetch();
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
            string json = ViewModel.Choice switch
            {
                ModelChoice.Local => JsonSerializer.Serialize(new
                {
                    stt_mode = "local",
                    local_model = ModelPaths.BaseModelFilename,
                }),
                ModelChoice.Cloud => JsonSerializer.Serialize(new
                {
                    stt_mode = "cloud",
                    api_key = ViewModel.GroqApiKey.Trim(),
                }),
                _ => "",
            };
            if (!string.IsNullOrEmpty(json))
            {
                DimmyNative.dimmy_set_config_json(json);
                App.Instance?.ReloadConfig();
                App.MarkOnboardingComplete();
            }
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
        _prefetch.StateChanged -= Prefetch_StateChanged;
        _prefetch.Dispose();
    }
}
