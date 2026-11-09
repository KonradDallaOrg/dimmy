using System;
using System.Diagnostics;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Views;

/// <summary>
/// 3-step Kimi Code Desktop MCP wizard (Install -> Connect -> Verify), the
/// Kimi sibling of <see cref="ClaudeDesktopConnectDialog"/>. The install is
/// one button (winget in a visible terminal), the connect writes one key in
/// ~/.kimi-code/mcp.json, the verify waits for dimmy-mcp's Kimi heartbeat.
/// </summary>
public sealed partial class KimiDesktopConnectDialog : ContentDialog
{
    public int InitialStep { get; set; } = 1;

    private const string GlyphSuccess = "\uE73E";
    private const string GlyphPending = "\uE9F5";
    private const string CmdInstall =
        "winget install --id MoonshotAI.KimiCodeDesktop -e --source winget --accept-package-agreements --accept-source-agreements";
    /// A heartbeat younger than this means Kimi started dimmy-mcp just now.
    private const long FreshHeartbeatSecs = 90;

    private int _step = 1;
    private DimmyNative.KimiMcpStatus _status = new(false, false, null, null, null);
    private DispatcherQueueTimer? _poll;

    public KimiDesktopConnectDialog()
    {
        InitializeComponent();
        Opened += (_, _) => { Probe(); Enter(InitialStep); };
        Closed += (_, _) => StopPoll();
    }

    private void Probe() => _status = DimmyNative.GetKimiMcpStatus();

    private bool DesktopOk => !string.IsNullOrEmpty(_status.DesktopPath);
    private bool HeartbeatFresh => _status.HeartbeatAgeSecs is long a && a <= FreshHeartbeatSecs;

    private void Enter(int step)
    {
        _step = step;
        Step1Panel.Visibility = step == 1 ? Visibility.Visible : Visibility.Collapsed;
        Step2Panel.Visibility = step == 2 ? Visibility.Visible : Visibility.Collapsed;
        Step3Panel.Visibility = step == 3 ? Visibility.Visible : Visibility.Collapsed;
        var accent = (Brush)Application.Current.Resources["AccentFillColorDefaultBrush"];
        Brush inactive = new SolidColorBrush(ActualTheme == ElementTheme.Dark
            ? Microsoft.UI.ColorHelper.FromArgb(0xFF, 0xA6, 0xA6, 0xA6)
            : Microsoft.UI.ColorHelper.FromArgb(0xFF, 0x70, 0x70, 0x70));
        Dot1.Fill = accent;
        Dot2.Fill = step >= 2 ? accent : inactive;
        Dot3.Fill = step >= 3 ? accent : inactive;
        SecondaryButtonText = step == 1 ? "" : "Back";
        PrimaryButtonText = step == 3 ? "Done" : "Next";
        StopPoll();
        switch (step)
        {
            case 1:
                UpdateStep1();
                if (!DesktopOk) StartPoll();
                break;
            case 2:
                ConfigPathText.Text = _status.ConfigPath ?? "~/.kimi-code/mcp.json";
                BinaryPathText.Text = ClaudeDesktopConnectDialog.ResolveMcpBinaryPath()
                                      ?? "dimmy-mcp.exe not found next to Dimmy.exe";
                UpdateStep2();
                break;
            case 3:
                UpdateStep3();
                if (!HeartbeatFresh) StartPoll();
                break;
        }
    }

    // ── Step 1 ────────────────────────────────────────────────────────

    private void UpdateStep1()
    {
        var success = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
        var neutral = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];
        DetectGlyph.Glyph = DesktopOk ? GlyphSuccess : GlyphPending;
        DetectGlyph.Foreground = DesktopOk ? success : neutral;
        if (DesktopOk)
        {
            DetectStatus.Text = "Kimi Code Desktop installed.";
            DetectRing.IsActive = false;
            DetectRing.Visibility = Visibility.Collapsed;
        }
        else if (!DetectRing.IsActive)
        {
            DetectStatus.Text = "Kimi Code Desktop is not installed.";
        }
        InstallBtn.Visibility = DesktopOk ? Visibility.Collapsed : Visibility.Visible;
        IsPrimaryButtonEnabled = DesktopOk;
    }

    private void Install_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            Process.Start(new ProcessStartInfo
            {
                FileName = "powershell.exe",
                Arguments = $"-NoProfile -NoExit -Command \"{CmdInstall}\"",
                UseShellExecute = true,
                WorkingDirectory = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
            });
            DetectStatus.Text = "Installing in the terminal. It appears here when done.";
            DetectRing.IsActive = true;
            DetectRing.Visibility = Visibility.Visible;
        }
        catch (Exception ex)
        {
            App.Log($"KimiDesktop: install spawn failed {ex.Message}", "Kimi");
            DetectStatus.Text = "Couldn't open a terminal. Download it from kimi.com/code.";
        }
    }

    // ── Step 2 ────────────────────────────────────────────────────────

    private void UpdateStep2()
    {
        ConnectOkGlyph.Visibility = _status.Registered ? Visibility.Visible : Visibility.Collapsed;
        ConnectBtn.Content = _status.Registered ? "Connect again" : "Connect";
        if (_status.Registered && string.IsNullOrEmpty(ConnectStatus.Text))
            ConnectStatus.Text = "Dimmy is registered.";
        IsPrimaryButtonEnabled = _status.Registered;
    }

    private void Connect_Click(object sender, RoutedEventArgs e)
    {
        var binary = ClaudeDesktopConnectDialog.ResolveMcpBinaryPath();
        var rc = binary == null ? -2 : DimmyNative.InstallKimiMcp(binary);
        App.Log($"KimiDesktop: connect rc={rc}", "Kimi");
        ConnectStatus.Text = rc switch
        {
            0 => "Connected.",
            -2 => "dimmy-mcp.exe was not found next to Dimmy.exe.",
            -3 => "~/.kimi-code/mcp.json is not valid JSON. Dimmy left it untouched; fix it and try again.",
            _ => "Could not write ~/.kimi-code/mcp.json. See dimmy.log.",
        };
        Probe();
        UpdateStep2();
    }

    // ── Step 3 ────────────────────────────────────────────────────────

    private void UpdateStep3()
    {
        var success = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
        var neutral = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];
        var ok = HeartbeatFresh;
        HeartbeatGlyph.Glyph = ok ? GlyphSuccess : GlyphPending;
        HeartbeatGlyph.Foreground = ok ? success : neutral;
        HeartbeatStatus.Text = ok
            ? "Kimi Code started Dimmy. You can ask it about your meetings."
            : "Waiting for Kimi Code to start Dimmy...";
        HeartbeatRing.IsActive = !ok;
        HeartbeatRing.Visibility = ok ? Visibility.Collapsed : Visibility.Visible;
        OpenKimiBtn.IsEnabled = DesktopOk;
        // Done stays available: verifying is a courtesy, the entry is written.
        IsPrimaryButtonEnabled = true;
    }

    private void OpenKimi_Click(object sender, RoutedEventArgs e)
    {
        if (!DesktopOk) return;
        try
        {
            Process.Start(new ProcessStartInfo { FileName = _status.DesktopPath!, UseShellExecute = true });
        }
        catch (Exception ex)
        {
            App.Log($"KimiDesktop: open failed {ex.Message}", "Kimi");
        }
    }

    // Documented exception to the no-FFI-polling rule (CLAUDE.md): what we
    // wait for happens in another process (an installer, Kimi Code starting
    // dimmy-mcp). 2 s while a step waits; stops on success and on close.
    private void StartPoll()
    {
        StopPoll();
        _poll = DispatcherQueue.GetForCurrentThread().CreateTimer();
        _poll.Interval = TimeSpan.FromSeconds(2);
        _poll.IsRepeating = true;
        _poll.Tick += (_, _) =>
        {
            Probe();
            if (_step == 1) { UpdateStep1(); if (DesktopOk) StopPoll(); }
            else if (_step == 3) { UpdateStep3(); if (HeartbeatFresh) StopPoll(); }
        };
        _poll.Start();
    }

    private void StopPoll()
    {
        _poll?.Stop();
        _poll = null;
    }

    private void OnPrimaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        if (_step < 3)
        {
            args.Cancel = true;
            Enter(_step + 1);
        }
    }

    private void OnSecondaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        args.Cancel = true;
        Enter(Math.Max(1, _step - 1));
    }
}
