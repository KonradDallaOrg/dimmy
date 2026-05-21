using System;
using System.IO;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Views;

/// <summary>
/// 3-step Claude Desktop MCP connection wizard. Mirror of the
/// NotionConnectDialog state-machine shape so the two integrations
/// feel identical from the user's side.
///
/// Flow:
///   Step 1 — detect install. Show Download link if missing.
///   Step 2 — patch claude_desktop_config.json (atomic, with backup).
///   Step 3 — poll for first heartbeat after Claude Desktop restart.
///
/// Re-runnable: caller can pass InitialStep to jump (e.g. step 3 to
/// retry a stalled heartbeat check after a Velopack update changed
/// the binary path).
/// </summary>
public sealed partial class ClaudeDesktopConnectDialog : ContentDialog
{
    public int InitialStep { get; set; } = 1;
    public bool Completed { get; private set; }

    private int _currentStep = 1;
    private bool _installed;
    private bool _patched;
    private DispatcherTimer? _heartbeatTimer;

    public ClaudeDesktopConnectDialog()
    {
        InitializeComponent();
        Opened += OnOpened;
        Closing += OnClosing;
    }

    private void OnOpened(ContentDialog sender, ContentDialogOpenedEventArgs args)
    {
        _currentStep = Math.Clamp(InitialStep, 1, 3);
        ApplyStep();
        _ = RefreshStatusAsync();
        if (_currentStep == 3)
        {
            StartHeartbeatPolling();
        }
    }

    private void OnClosing(ContentDialog sender, ContentDialogClosingEventArgs args)
    {
        StopHeartbeatPolling();
    }

    private void ApplyStep()
    {
        Step1Panel.Visibility = _currentStep == 1 ? Visibility.Visible : Visibility.Collapsed;
        Step2Panel.Visibility = _currentStep == 2 ? Visibility.Visible : Visibility.Collapsed;
        Step3Panel.Visibility = _currentStep == 3 ? Visibility.Visible : Visibility.Collapsed;

        var accent = (Brush)Application.Current.Resources["AccentFillColorDefaultBrush"];
        var idle = (Brush)Application.Current.Resources["ControlStrokeColorDefaultBrush"];
        Dot1.Fill = _currentStep >= 1 ? accent : idle;
        Dot2.Fill = _currentStep >= 2 ? accent : idle;
        Dot3.Fill = _currentStep >= 3 ? accent : idle;

        SecondaryButtonText = _currentStep == 1 ? "" : "Back";
        IsSecondaryButtonEnabled = _currentStep != 1;
        switch (_currentStep)
        {
            case 1:
                PrimaryButtonText = "Next";
                IsPrimaryButtonEnabled = _installed;
                break;
            case 2:
                PrimaryButtonText = "Next";
                IsPrimaryButtonEnabled = _patched;
                break;
            case 3:
                PrimaryButtonText = "Done";
                // Heartbeat poll re-enables this when first heartbeat lands;
                // user can also manually Done after restart.
                IsPrimaryButtonEnabled = true;
                break;
        }
    }

    // ── Button handlers ───────────────────────────────────────────

    private async void OnPrimaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        if (_currentStep < 3)
        {
            args.Cancel = true;
            _currentStep++;
            ApplyStep();
            if (_currentStep == 2)
            {
                await RefreshStatusAsync();
            }
            else if (_currentStep == 3)
            {
                StartHeartbeatPolling();
            }
            return;
        }
        Completed = true;
    }

    private void OnSecondaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        args.Cancel = true;
        if (_currentStep > 1)
        {
            _currentStep--;
            ApplyStep();
            if (_currentStep < 3) StopHeartbeatPolling();
        }
    }

    private void OnCloseClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        StopHeartbeatPolling();
    }

    // ── Step 1 — detect ───────────────────────────────────────────

    private async Task RefreshStatusAsync()
    {
        var status = await Task.Run(() => DimmyNative.GetClaudeDesktopStatus());
        _installed = status.Installed;
        _patched = status.ConfigPatched;
        DispatcherQueue.TryEnqueue(() => ApplyStatus(status));
    }

    private void ApplyStatus(DimmyNative.ClaudeDesktopStatus status)
    {
        // Step 1 visuals
        if (status.Installed)
        {
            DetectGlyph.Glyph = ""; // checkmark
            DetectGlyph.Foreground = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
            DetectStatus.Text = "Claude Desktop is installed.";
            DetectPath.Text = status.InstallPath ?? "";
            DetectPath.Visibility = string.IsNullOrEmpty(status.InstallPath)
                ? Visibility.Collapsed : Visibility.Visible;
            InstallBtn.Visibility = Visibility.Collapsed;
            RecheckBtn.Visibility = Visibility.Collapsed;
        }
        else
        {
            DetectGlyph.Glyph = ""; // warning
            DetectGlyph.Foreground = (Brush)Application.Current.Resources["SystemFillColorCautionBrush"];
            DetectStatus.Text = "We couldn't find Claude Desktop on this Mac.";
            DetectPath.Visibility = Visibility.Collapsed;
            InstallBtn.Visibility = Visibility.Visible;
            RecheckBtn.Visibility = Visibility.Visible;
        }

        // Step 2 — populate paths even before we land here, so the
        // user can scan the change preview while moving from step 1.
        ConfigPathText.Text = string.IsNullOrEmpty(status.ConfigPath)
            ? "(will be created on first launch)" : status.ConfigPath;
        BinaryPathText.Text = ResolveMcpBinaryPath() ?? "(not found in installation folder)";
        if (status.ConfigPatched)
        {
            PatchOkGlyph.Visibility = Visibility.Visible;
            PatchStatus.Text = "Already registered.";
        }

        ApplyStep(); // refresh button-enabled state from new flags
    }

    private async void InstallClaude_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            await global::Windows.System.Launcher.LaunchUriAsync(
                new Uri("https://claude.ai/download"));
        }
        catch (Exception ex)
        {
            App.Log($"ClaudeDesktopWizard install-link exc: {ex}", "ClaudeDesktop");
        }
    }

    private async void Recheck_Click(object sender, RoutedEventArgs e)
    {
        await RefreshStatusAsync();
    }

    // ── Step 2 — patch ────────────────────────────────────────────

    private async void Patch_Click(object sender, RoutedEventArgs e)
    {
        var binary = ResolveMcpBinaryPath();
        if (string.IsNullOrEmpty(binary))
        {
            PatchStatus.Text = "Couldn't locate dimmy-mcp.exe next to Dimmy.exe.";
            return;
        }
        PatchBtn.IsEnabled = false;
        PatchRing.IsActive = true;
        PatchRing.Visibility = Visibility.Visible;
        PatchOkGlyph.Visibility = Visibility.Collapsed;
        PatchStatus.Text = "Writing config…";
        bool ok = await Task.Run(() => DimmyNative.PatchClaudeDesktopConfig(binary));
        PatchRing.IsActive = false;
        PatchRing.Visibility = Visibility.Collapsed;
        PatchBtn.IsEnabled = true;
        if (ok)
        {
            _patched = true;
            PatchOkGlyph.Visibility = Visibility.Visible;
            PatchStatus.Text = "Registered. Next, restart Claude Desktop.";
            IsPrimaryButtonEnabled = true;
        }
        else
        {
            PatchStatus.Text = "Failed — Dimmy log has the details.";
        }
    }

    // ── Step 3 — heartbeat poll ───────────────────────────────────

    private void StartHeartbeatPolling()
    {
        if (_heartbeatTimer != null) return;
        _heartbeatTimer = new DispatcherTimer
        {
            // 1 Hz is fine — the heartbeat file is rewritten every 30 s
            // by dimmy-mcp; this timer is only "did something appear yet?"
            // OS-event semantics aren't available, hence the poll.
            Interval = TimeSpan.FromSeconds(1)
        };
        _heartbeatTimer.Tick += HeartbeatTick;
        _heartbeatTimer.Start();
    }

    private void StopHeartbeatPolling()
    {
        if (_heartbeatTimer == null) return;
        _heartbeatTimer.Stop();
        _heartbeatTimer.Tick -= HeartbeatTick;
        _heartbeatTimer = null;
    }

    private async void HeartbeatTick(object? sender, object e)
    {
        var status = await Task.Run(() => DimmyNative.GetClaudeDesktopStatus());
        DispatcherQueue.TryEnqueue(() =>
        {
            if (status.HeartbeatAgeSecs.HasValue && status.HeartbeatAgeSecs.Value < 90)
            {
                HeartbeatGlyph.Glyph = "";
                HeartbeatGlyph.Foreground = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
                HeartbeatStatus.Text = $"Connected. Last heartbeat {status.HeartbeatAgeSecs.Value}s ago.";
                HeartbeatRing.IsActive = false;
                HeartbeatRing.Visibility = Visibility.Collapsed;
                Completed = true;
                StopHeartbeatPolling();
            }
        });
    }

    private async void OpenClaude_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var status = await Task.Run(() => DimmyNative.GetClaudeDesktopStatus());
            if (!string.IsNullOrEmpty(status.InstallPath))
            {
                System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(
                    status.InstallPath) { UseShellExecute = true });
            }
        }
        catch (Exception ex)
        {
            App.Log($"ClaudeDesktopWizard open exc: {ex}", "ClaudeDesktop");
        }
    }

    // ── Helpers ───────────────────────────────────────────────────

    /// <summary>
    /// Resolve the path to dimmy-mcp.exe shipped alongside Dimmy.exe.
    /// We deliberately do NOT use AppDomain.BaseDirectory — under
    /// single-file publish that path is the exe dir but with Velopack
    /// updates it can point at a stage dir mid-update. Process path is
    /// authoritative.
    /// </summary>
    private static string? ResolveMcpBinaryPath()
    {
        try
        {
            var exe = Environment.ProcessPath;
            if (string.IsNullOrEmpty(exe)) return null;
            var dir = Path.GetDirectoryName(exe);
            if (string.IsNullOrEmpty(dir)) return null;
            var candidate = Path.Combine(dir, "dimmy-mcp.exe");
            return File.Exists(candidate) ? candidate : null;
        }
        catch
        {
            return null;
        }
    }
}
