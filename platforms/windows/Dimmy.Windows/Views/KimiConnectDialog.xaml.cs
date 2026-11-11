using System;
using System.Diagnostics;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Views;

/// <summary>
/// Guided 3-page Kimi Code setup (Install -> Sign in -> Test). Sibling of
/// <see cref="CodexConnectDialog"/>, with one difference that comes from the
/// user, not the CLI: nothing is pasted. Each install is a button that opens
/// a visible terminal running the vendor's command, so the user sees what
/// runs and Windows asks for consent where it has to.
/// </summary>
public sealed partial class KimiConnectDialog : ContentDialog
{
    public bool Completed { get; private set; }

    /// <summary>Start at page 1 regardless of detection ("Re-run setup").</summary>
    public bool ForceStartAtStep1 { get; set; }

    private enum Page { Install = 1, SignIn = 2, Test = 3 }

    private const string GlyphSuccess = "\uE73E"; // CheckMark
    private const string GlyphPending = "\uE9F5"; // neutral placeholder
    private const string GlyphInfo = "\uE946";    // Info
    private const string GlyphError = "\uEA39";   // ErrorBadge

    private const string CmdGit = "winget install --id Git.Git -e --source winget";

    private Page _currentPage = Page.Install;
    private bool _gitOk;
    private bool _kimiOk;
    private bool _signInOk;
    private bool _signInStarted;
    private int _signInTicks;
    private bool _testOk;
    private DispatcherQueueTimer? _pollTimer;

    public KimiConnectDialog()
    {
        InitializeComponent();
        Opened += OnOpened;
        Closed += (_, _) => StopPoll();
    }

    private void OnOpened(ContentDialog sender, ContentDialogOpenedEventArgs args)
    {
        ProbeStatus();
        var start = Page.Install;
        if (!ForceStartAtStep1 && _gitOk && _kimiOk)
            start = _signInOk ? Page.Test : Page.SignIn;
        EnterPage(start);
    }

    private void ProbeStatus()
    {
        try
        {
            _gitOk = DimmyNative.KimiGitBashPresent();
            var s = DimmyNative.RecheckKimi();
            _kimiOk = s != DimmyNative.ClaudeCodeStatus.NotInstalled;
            _signInOk = s == DimmyNative.ClaudeCodeStatus.Ready;
        }
        catch (Exception ex)
        {
            App.Log($"KimiWizard: probe exc {ex.Message}", "Kimi");
            _gitOk = _kimiOk = _signInOk = false;
        }
    }

    // ── State machine ─────────────────────────────────────────────────

    private void EnterPage(Page page)
    {
        _currentPage = page;
        Page1Panel.Visibility = page == Page.Install ? Visibility.Visible : Visibility.Collapsed;
        Page2Panel.Visibility = page == Page.SignIn ? Visibility.Visible : Visibility.Collapsed;
        Page3Panel.Visibility = page == Page.Test ? Visibility.Visible : Visibility.Collapsed;

        var accent = (Brush)Application.Current.Resources["AccentFillColorDefaultBrush"];
        bool dark = ActualTheme == ElementTheme.Dark;
        Brush inactive = new SolidColorBrush(dark
            ? Microsoft.UI.ColorHelper.FromArgb(0xFF, 0xA6, 0xA6, 0xA6)
            : Microsoft.UI.ColorHelper.FromArgb(0xFF, 0x70, 0x70, 0x70));
        Dot1.Fill = accent;
        Dot2.Fill = page >= Page.SignIn ? accent : inactive;
        Dot3.Fill = page >= Page.Test ? accent : inactive;

        SecondaryButtonText = page == Page.Install ? "" : "Back";
        PrimaryButtonText = page == Page.Test ? "Done" : "Next";
        StopPoll();
        switch (page)
        {
            case Page.Install:
                UpdateInstallUi();
                if (!(_gitOk && _kimiOk)) StartPoll();
                break;
            case Page.SignIn:
                UpdateSignInUi();
                if (!_signInOk) StartPoll();
                break;
            case Page.Test:
                _ = RunTestAsync();
                break;
        }
    }

    // ── Page 1 — Install ──────────────────────────────────────────────

    private void UpdateInstallUi()
    {
        var success = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
        var neutral = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];

        GitGlyph.Glyph = _gitOk ? GlyphSuccess : GlyphPending;
        GitGlyph.Foreground = _gitOk ? success : neutral;
        GitText.Text = _gitOk
            ? "Git for Windows installed."
            : "Git for Windows is needed by Kimi Code.";
        InstallGitBtn.Visibility = _gitOk ? Visibility.Collapsed : Visibility.Visible;

        KimiGlyph.Glyph = _kimiOk ? GlyphSuccess : GlyphPending;
        KimiGlyph.Foreground = _kimiOk ? success : neutral;
        KimiText.Text = _kimiOk ? "Kimi Code CLI installed." : "Kimi Code CLI not installed yet.";
        InstallKimiBtn.Visibility = _kimiOk ? Visibility.Collapsed : Visibility.Visible;
        // Kimi's installer works without Git, but the CLI does not start
        // without it: install in that order.
        InstallKimiBtn.IsEnabled = _gitOk;

        IsPrimaryButtonEnabled = _gitOk && _kimiOk;
    }

    private void InstallGit_Click(object sender, RoutedEventArgs e)
    {
        if (RunInTerminal(CmdGit))
            GitText.Text = "Installing Git in the terminal. Accept the Windows prompt if it appears...";
        else
            GitText.Text = $"Couldn't open a terminal. Run `{CmdGit}` yourself.";
    }

    private void InstallKimi_Click(object sender, RoutedEventArgs e)
    {
        var cmd = DimmyNative.GetKimiInstallCommand();
        if (cmd.Length > 0 && RunInTerminal(cmd))
            KimiText.Text = "Installing Kimi Code in the terminal...";
        else
            KimiText.Text = "Couldn't open a terminal. See kimi.ai/code for the installer.";
    }

    /// <summary>A visible PowerShell that runs <paramref name="cmd"/> and stays
    /// open, so the user sees the result. Neither command has a quote in it.</summary>
    private static bool RunInTerminal(string cmd)
    {
        Debug.Assert(!cmd.Contains('"'), "command must not need escaping");
        try
        {
            Process.Start(new ProcessStartInfo
            {
                FileName = "powershell.exe",
                Arguments = $"-NoProfile -NoExit -ExecutionPolicy Bypass -Command \"{cmd}\"",
                UseShellExecute = true,
                WorkingDirectory = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
            });
            return true;
        }
        catch (Exception ex)
        {
            App.Log($"KimiWizard: terminal spawn failed {ex.Message}", "Kimi");
            return false;
        }
    }

    // ── Page 2 — Sign in ──────────────────────────────────────────────

    private void UpdateSignInUi()
    {
        var success = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
        var neutral = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];
        if (_signInOk)
        {
            SignInGlyph.Glyph = GlyphSuccess;
            SignInGlyph.Foreground = success;
            SignInText.Text = "Signed in to Kimi.";
            SignInBtn.Visibility = Visibility.Collapsed;
            SignInRing.IsActive = false;
            SignInRing.Visibility = Visibility.Collapsed;
        }
        else
        {
            SignInGlyph.Glyph = GlyphInfo;
            SignInGlyph.Foreground = neutral;
            SignInBtn.Visibility = Visibility.Visible;
        }
        IsPrimaryButtonEnabled = _signInOk;
    }

    private void SignIn_Click(object sender, RoutedEventArgs e)
    {
        if (!DimmyNative.SpawnKimiLogin())
        {
            SignInText.Text = "Couldn't start the sign-in. Try again, or run `kimi login` in a terminal.";
            DimmyNative.TrackEvent("kimi_cli.login_completed", new { outcome = "spawn_failed" });
            return;
        }
        _signInStarted = true;
        _signInTicks = 0;
        SignInText.Text = "Your browser opens kimi.ai with a code. Confirm it there; Dimmy sees the sign-in by itself.";
        SignInRing.IsActive = true;
        SignInRing.Visibility = Visibility.Visible;
        // Stays enabled: if the browser tab was closed or the code expired,
        // one more click starts a new sign-in.
        SignInBtn.Content = "Sign in again";
        StartPoll();
    }

    // Documented exception to the no-FFI-polling rule (CLAUDE.md): what we
    // wait for is an EXTERNAL change (an installer or a browser sign-in in
    // another process). The core cannot emit an event for what it has not
    // observed. 2 s while a page waits; stops on success and on close.
    private void StartPoll()
    {
        StopPoll();
        var dq = DispatcherQueue.GetForCurrentThread();
        _pollTimer = dq.CreateTimer();
        _pollTimer.Interval = TimeSpan.FromSeconds(2);
        _pollTimer.IsRepeating = true;
        _pollTimer.Tick += (_, _) => PollTick();
        _pollTimer.Start();
    }

    private void PollTick()
    {
        var wasSignedIn = _signInOk;
        ProbeStatus();
        if (_currentPage == Page.Install)
        {
            UpdateInstallUi();
            if (_gitOk && _kimiOk) StopPoll();
            return;
        }
        if (_currentPage != Page.SignIn) return;
        if (_signInOk && !wasSignedIn && _signInStarted)
            DimmyNative.TrackEvent("kimi_cli.login_completed", new { outcome = "success" });
        if (_signInOk)
        {
            StopPoll();
            UpdateSignInUi();
            return;
        }
        if (_signInStarted && ++_signInTicks >= 90) // 3 minutes
        {
            StopPoll();
            _signInStarted = false;
            DimmyNative.TrackEvent("kimi_cli.login_completed", new { outcome = "timeout" });
            SignInText.Text = "Sign-in not completed in 3 minutes. Try again.";
            SignInRing.IsActive = false;
            SignInRing.Visibility = Visibility.Collapsed;
        }
    }

    private void StopPoll()
    {
        _pollTimer?.Stop();
        _pollTimer = null;
    }

    // ── Page 3 — Test ─────────────────────────────────────────────────

    private async Task RunTestAsync()
    {
        var success = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
        var neutral = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];
        _testOk = false;
        IsPrimaryButtonEnabled = false;
        RetestBtn.IsEnabled = false;
        TestGlyph.Glyph = GlyphPending;
        TestGlyph.Foreground = neutral;
        TestText.Text = "Sending a test message to Kimi...";
        TestRing.IsActive = true;
        TestRing.Visibility = Visibility.Visible;
        ModelsText.Visibility = Visibility.Collapsed;

        var (result, elapsedMs) = await Task.Run(DimmyNative.PingKimi);
        _testOk = result == DimmyNative.ClaudeCodePingResult.Ok;
        TestRing.IsActive = false;
        TestRing.Visibility = Visibility.Collapsed;
        TestGlyph.Glyph = _testOk ? GlyphSuccess : GlyphError;
        TestGlyph.Foreground = _testOk ? success : neutral;
        TestText.Text = KimiPingText(result, elapsedMs);

        if (_testOk)
        {
            var models = await Task.Run(DimmyNative.GetKimiModels);
            if (models.Count > 0)
            {
                ModelsText.Text = "Models in your plan: "
                                  + string.Join(", ", models.Select(m => m.Name));
                ModelsText.Visibility = Visibility.Visible;
            }
        }
        RetestBtn.IsEnabled = true;
        IsPrimaryButtonEnabled = _testOk;
    }

    /// <summary>One message per ping result. Shared with the Settings card.</summary>
    internal static string KimiPingText(DimmyNative.ClaudeCodePingResult result, int elapsedMs) =>
        result switch
        {
            DimmyNative.ClaudeCodePingResult.Ok =>
                $"✓ Connection OK — {elapsedMs} ms round-trip via the local `kimi` CLI.",
            DimmyNative.ClaudeCodePingResult.NotInstalled =>
                "✗ `kimi` not found. Install Kimi Code first.",
            DimmyNative.ClaudeCodePingResult.NotLoggedIn =>
                "✗ Not signed in. Sign in with your Kimi account.",
            DimmyNative.ClaudeCodePingResult.SpawnFailed =>
                "✗ Could not start the Kimi CLI. Is Git for Windows installed? See dimmy.log.",
            DimmyNative.ClaudeCodePingResult.Timeout =>
                "✗ Timed out after 60 s — network or usage-limit issue.",
            DimmyNative.ClaudeCodePingResult.Reported =>
                "✗ Kimi refused the request: your plan may not include Kimi Code, or it reached its usage limit. See dimmy.log.",
            DimmyNative.ClaudeCodePingResult.EmptyResponse =>
                "✗ Kimi answered with nothing. Try again.",
            DimmyNative.ClaudeCodePingResult.InvalidUtf8 =>
                "✗ Unexpected answer from `kimi`. See dimmy.log.",
            _ => "✗ Unknown error. See dimmy.log.",
        };

    private void Retest_Click(object sender, RoutedEventArgs e) => _ = RunTestAsync();

    // ── ContentDialog buttons ─────────────────────────────────────────

    private void OnPrimaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        switch (_currentPage)
        {
            case Page.Install:
                args.Cancel = true;
                EnterPage(_signInOk ? Page.Test : Page.SignIn);
                break;
            case Page.SignIn:
                args.Cancel = true;
                EnterPage(Page.Test);
                break;
            case Page.Test:
                if (_testOk) Completed = true;
                else args.Cancel = true;
                break;
        }
    }

    private void OnSecondaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        args.Cancel = true;
        EnterPage(_currentPage == Page.Test ? Page.SignIn : Page.Install);
    }
}
