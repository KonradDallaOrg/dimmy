using System;
using System.Diagnostics;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Views;

/// <summary>
/// 3-step Kimi (Kimi Work) wizard: Install -> Add the Dimmy plugin -> Try it.
/// The Kimi sibling of <see cref="ClaudeDesktopConnectDialog"/>. "Add to Kimi"
/// registers Dimmy as a Personal plugin (and in ~/.kimi-code/mcp.json for
/// Kimi Code); Kimi only activates it after the user presses Install in its
/// Plugins page, which this page watches for.
/// </summary>
public sealed partial class KimiDesktopConnectDialog : ContentDialog
{
    public int InitialStep { get; set; } = 1;

    private const string GlyphSuccess = "\uE73E";
    private const string GlyphPending = "\uE9F5";
    private const string CmdInstall =
        "winget install --id MoonshotAI.Kimi -e --source winget --accept-package-agreements --accept-source-agreements";
    /// Kimi Work's only external deep link that does something: open Work.
    internal const string WorkLink = "kimi-work://home";
    /// A heartbeat younger than this means Kimi Work started dimmy-mcp just now.
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

    private bool AppOk => !string.IsNullOrEmpty(_status.WorkAppPath);
    /// Set up = opened once and signed in: Kimi Work's data folder exists.
    private bool ReadyOk => AppOk && _status.WorkFound;
    private bool HeartbeatFresh =>
        _status.WorkHeartbeatAgeSecs is long a && a <= FreshHeartbeatSecs;

    private static Brush Success => (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
    private static Brush Neutral => (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];

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
        Refresh();
        StopPoll();
        StartPoll();
    }

    private void Refresh()
    {
        switch (_step)
        {
            case 1: UpdateStep1(); break;
            case 2: UpdateStep2(); break;
            default: UpdateStep3(); break;
        }
    }

    // ── Step 1 ────────────────────────────────────────────────────────

    private void UpdateStep1()
    {
        DetectGlyph.Glyph = ReadyOk ? GlyphSuccess : GlyphPending;
        DetectGlyph.Foreground = ReadyOk ? Success : Neutral;
        if (ReadyOk)
        {
            DetectStatus.Text = "Kimi is installed and set up.";
            DetectRing.IsActive = false;
            DetectRing.Visibility = Visibility.Collapsed;
        }
        else if (AppOk)
        {
            DetectStatus.Text = "Kimi is installed. Open it once and sign in.";
            DetectRing.IsActive = false;
            DetectRing.Visibility = Visibility.Collapsed;
        }
        else if (!DetectRing.IsActive)
        {
            DetectStatus.Text = "Kimi is not installed.";
        }
        InstallBtn.Visibility = AppOk ? Visibility.Collapsed : Visibility.Visible;
        DownloadLink.Visibility = AppOk ? Visibility.Collapsed : Visibility.Visible;
        OpenKimi1Btn.Visibility = AppOk && !ReadyOk ? Visibility.Visible : Visibility.Collapsed;
        IsPrimaryButtonEnabled = ReadyOk;
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
            DetectStatus.Text = "Installing in the terminal. Then open Kimi once and sign in.";
            DetectRing.IsActive = true;
            DetectRing.Visibility = Visibility.Visible;
        }
        catch (Exception ex)
        {
            App.Log($"KimiWork: install spawn failed {ex.Message}", "Kimi");
            DetectStatus.Text = "Couldn't open a terminal. Download Kimi from kimi.ai.";
        }
    }

    // ── Step 2 ────────────────────────────────────────────────────────

    private void UpdateStep2()
    {
        RegisterGlyph.Glyph = _status.WorkRegistered ? GlyphSuccess : GlyphPending;
        RegisterGlyph.Foreground = _status.WorkRegistered ? Success : Neutral;
        RegisterStatus.Text = _status.WorkRegistered
            ? "Dimmy is in Kimi's Personal plugins."
            : "Not added yet.";
        ConnectBtn.Content = _status.WorkRegistered ? "Add again" : "Add to Kimi";

        InstalledGlyph.Glyph = _status.WorkInstalled ? GlyphSuccess : GlyphPending;
        InstalledGlyph.Foreground = _status.WorkInstalled ? Success : Neutral;
        InstalledStatus.Text = _status.WorkInstalled
            ? "Installed in Kimi."
            : "In Kimi: Plugins → Personal → Dimmy → Install.";
        var waiting = _status.WorkRegistered && !_status.WorkInstalled;
        InstalledRing.IsActive = waiting;
        InstalledRing.Visibility = waiting ? Visibility.Visible : Visibility.Collapsed;
        IsPrimaryButtonEnabled = _status.WorkInstalled;
    }

    private void Connect_Click(object sender, RoutedEventArgs e)
    {
        var binary = ClaudeDesktopConnectDialog.ResolveMcpBinaryPath();
        var rc = binary == null ? -2 : DimmyNative.InstallKimiMcp(binary);
        App.Log($"KimiWork: add plugin rc={rc}", "Kimi");
        ConnectStatus.Text = rc switch
        {
            0 => "",
            -2 => "dimmy-mcp.exe was not found next to Dimmy.exe.",
            -3 => "~/.kimi-code/mcp.json is not valid JSON. Dimmy left it untouched; fix it and try again.",
            _ => "Could not write the plugin. See dimmy.log.",
        };
        Probe();
        UpdateStep2();
        if (rc == 0) OpenKimi_Click(sender, e);
    }

    // ── Step 3 ────────────────────────────────────────────────────────

    private void UpdateStep3()
    {
        var ok = HeartbeatFresh;
        HeartbeatGlyph.Glyph = ok ? GlyphSuccess : GlyphPending;
        HeartbeatGlyph.Foreground = ok ? Success : Neutral;
        HeartbeatStatus.Text = ok
            ? "Kimi Work started Dimmy. Ask it about your meetings."
            : "Waiting for Kimi to start Dimmy...";
        HeartbeatRing.IsActive = !ok;
        HeartbeatRing.Visibility = ok ? Visibility.Collapsed : Visibility.Visible;
        // Done stays available: the plugin is installed, verifying is a courtesy.
        IsPrimaryButtonEnabled = true;
    }

    private void OpenKimi_Click(object sender, RoutedEventArgs e)
    {
        try { Process.Start(new ProcessStartInfo(WorkLink) { UseShellExecute = true }); }
        catch (Exception ex) { App.Log($"KimiWork: open failed {ex.Message}", "Kimi"); }
    }

    // Documented exception to the no-FFI-polling rule (CLAUDE.md): what we
    // wait for happens in another process (an installer, the user pressing
    // Install in Kimi, Kimi starting dimmy-mcp). 2 s while the dialog is
    // open; stops on close.
    private void StartPoll()
    {
        _poll = DispatcherQueue.GetForCurrentThread().CreateTimer();
        _poll.Interval = TimeSpan.FromSeconds(2);
        _poll.IsRepeating = true;
        _poll.Tick += (_, _) => { Probe(); Refresh(); };
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
