using System;
using System.Diagnostics;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.ApplicationModel.DataTransfer;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Views;

/// <summary>
/// 2-step ChatGPT (Codex) subscription setup wizard. Linear, modal,
/// re-runnable. Same shape as <see cref="ClaudeConnectDialog"/>: the
/// Codex CLI ships as a native standalone binary now (no Node.js),
/// installed via a one-line PowerShell command.
///
/// Steps:
///   1. Detect / install Codex (irm install.ps1 | iex).
///   2. Sign in (browser flow via `codex login`) + auto-test a tiny
///      ping prompt.
///
/// Smart-skip: on Open we probe the binary + login state once and start
/// at the FIRST incomplete step. A machine where the CLI is already
/// installed lands on Step 2 with test-ready state.
///
/// Completion contract: <see cref="Completed"/> is true iff Step 2's
/// test ping returned a positive result. The SettingsWindow caller then
/// lights up the Output subscription toggles.
///
/// Reuses the existing dimmy_codex_* FFI (status / recheck / spawn-login
/// / ping) — no new native surface.
///
/// Glyphs use \uXXXX escapes (ASCII in source) so a source-encoding
/// hiccup can't strip the literal and leave the FontIcon blank — the
/// bug that shipped on the first Claude wizard build.
/// </summary>
public sealed partial class CodexConnectDialog : ContentDialog
{
    private const string GlyphSuccess = "\uE73E"; // CheckMark
    private const string GlyphError = "\uEA39"; // Info / status dot
    private const string GlyphPending = "\uE895"; // Sync / placeholder

    /// <summary>True iff Step 2's test ping returned a positive result.</summary>
    public bool Completed { get; private set; }

    /// <summary>
    /// When true, bypass the smart-skip and start at Step 1 regardless
    /// of current detection state. Caller uses this for "Re-run setup".
    /// </summary>
    public bool ForceStartAtStep1 { get; set; }

    private enum Step { Install = 1, SignIn = 2 }

    private Step _currentStep = Step.Install;
    private bool _codexOk;   // binary present (any status != NotInstalled)
    private bool _signInOk;  // logged in (status == Ready)
    private bool _testOk;

    private DispatcherQueueTimer? _credentialsPollTimer;
    private CancellationTokenSource? _autoTestCts;

    public CodexConnectDialog()
    {
        InitializeComponent();
        Opened += OnOpened;
        Closed += OnClosed;
    }

    // ── Lifecycle ─────────────────────────────────────────────────────

    private void OnOpened(ContentDialog sender, ContentDialogOpenedEventArgs args)
    {
        ProbeAllStates();
        var startAt = Step.Install;
        if (!ForceStartAtStep1 && _codexOk) startAt = Step.SignIn;
        EnterStep(startAt);
    }

    private void OnClosed(ContentDialog sender, ContentDialogClosedEventArgs args)
    {
        StopCredentialsPoll();
        _autoTestCts?.Cancel();
        _autoTestCts?.Dispose();
        _autoTestCts = null;
    }

    private void ProbeAllStates()
    {
        try
        {
            var s = DimmyNative.GetCodexStatus();
            _codexOk = s != DimmyNative.ClaudeCodeStatus.NotInstalled;
            _signInOk = s == DimmyNative.ClaudeCodeStatus.Ready;
            UpdateCodexUi(s);
            UpdateSignInUi(s);
        }
        catch (Exception ex)
        {
            App.Log($"CodexWizard: probe exc {ex.Message}", "Wizard");
            _codexOk = false;
            _signInOk = false;
        }
    }

    // ── State machine ─────────────────────────────────────────────────

    private void EnterStep(Step step)
    {
        _currentStep = step;
        Step1Panel.Visibility = step == Step.Install ? Visibility.Visible : Visibility.Collapsed;
        Step2Panel.Visibility = step == Step.SignIn ? Visibility.Visible : Visibility.Collapsed;

        var accent = (Brush)Application.Current.Resources["AccentFillColorDefaultBrush"];
        var inactive = (Brush)Application.Current.Resources["ControlStrokeColorDefaultBrush"];
        Dot1.Fill = step >= Step.Install ? accent : inactive;
        Dot2.Fill = step >= Step.SignIn ? accent : inactive;

        SecondaryButtonText = step == Step.Install ? "" : "Back";
        switch (step)
        {
            case Step.Install:
                PrimaryButtonText = "Next";
                IsPrimaryButtonEnabled = _codexOk;
                break;
            case Step.SignIn:
                PrimaryButtonText = _testOk ? "Close & enable" : "Test connection";
                IsPrimaryButtonEnabled = _signInOk;
                if (_signInOk && !_testOk) _ = AutoRunTestAsync();
                break;
        }
    }

    // ── Step 1 — Install ──────────────────────────────────────────────

    private void UpdateCodexUi(DimmyNative.ClaudeCodeStatus status)
    {
        var brushSuccess = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
        var brushCritical = (Brush)Application.Current.Resources["SystemFillColorCriticalBrush"];

        if (status != DimmyNative.ClaudeCodeStatus.NotInstalled)
        {
            var path = DimmyNative.GetCodexBinaryPath() ?? "";
            CodexStatusGlyph.Glyph = GlyphSuccess;
            CodexStatusGlyph.Foreground = brushSuccess;
            CodexStatusText.Text = string.IsNullOrEmpty(path)
                ? "Codex CLI detected"
                : $"Codex CLI detected at {path}";
            CodexActionPanel.Visibility = Visibility.Collapsed;
            CodexHelpText.Visibility = Visibility.Collapsed;
        }
        else
        {
            CodexStatusGlyph.Glyph = GlyphError;
            CodexStatusGlyph.Foreground = brushCritical;
            CodexStatusText.Text = "Codex CLI not installed. Run the command below in a terminal.";
            CodexActionPanel.Visibility = Visibility.Visible;
            CodexHelpText.Visibility = Visibility.Visible;
        }
        IsPrimaryButtonEnabled = _currentStep == Step.Install ? _codexOk : IsPrimaryButtonEnabled;
    }

    private void CopyInstallCommand_Click(object sender, RoutedEventArgs e)
    {
        var pkg = new DataPackage();
        pkg.SetText("irm https://chatgpt.com/codex/install.ps1 | iex");
        Clipboard.SetContent(pkg);
        CopyCmdBtn.Content = "Copied!";
        var dq = DispatcherQueue.GetForCurrentThread();
        var t = dq.CreateTimer();
        t.Interval = TimeSpan.FromMilliseconds(1500);
        t.IsRepeating = false;
        t.Tick += (s, _) =>
        {
            CopyCmdBtn.Content = "Copy command";
            t.Stop();
        };
        t.Start();
    }

    private void OpenTerminal_Click(object sender, RoutedEventArgs e)
    {
        if (TrySpawn("wt.exe", "")) return;
        if (TrySpawn("powershell.exe", "-NoExit")) return;
        TrySpawn("cmd.exe", "/K");
    }

    private static bool TrySpawn(string fileName, string args)
    {
        try
        {
            var psi = new ProcessStartInfo
            {
                FileName = fileName,
                Arguments = args,
                UseShellExecute = true,
                WorkingDirectory = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
            };
            Process.Start(psi);
            return true;
        }
        catch
        {
            return false;
        }
    }

    private void RecheckCodex_Click(object sender, RoutedEventArgs e)
    {
        var rc = DimmyNative.RecheckCodex();
        _codexOk = rc != DimmyNative.ClaudeCodeStatus.NotInstalled;
        _signInOk = rc == DimmyNative.ClaudeCodeStatus.Ready;
        UpdateCodexUi(rc);
        IsPrimaryButtonEnabled = _codexOk;
    }

    // ── Step 2 — Sign in + Test ───────────────────────────────────────

    private void UpdateSignInUi(DimmyNative.ClaudeCodeStatus status)
    {
        var brushSuccess = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
        var brushCritical = (Brush)Application.Current.Resources["SystemFillColorCriticalBrush"];

        if (status == DimmyNative.ClaudeCodeStatus.Ready)
        {
            SignInGlyph.Glyph = GlyphSuccess;
            SignInGlyph.Foreground = brushSuccess;
            SignInText.Text = "Signed in with your ChatGPT plan.";
            SignInBtn.Visibility = Visibility.Collapsed;
            RecheckSignInBtn.Visibility = Visibility.Collapsed;
        }
        else if (status == DimmyNative.ClaudeCodeStatus.NotLoggedIn)
        {
            SignInGlyph.Glyph = GlyphError;
            SignInGlyph.Foreground = brushCritical;
            SignInText.Text = "Not signed in. Click Sign in to open the ChatGPT login flow.";
            SignInBtn.Visibility = Visibility.Visible;
            RecheckSignInBtn.Visibility = Visibility.Visible;
        }
        else
        {
            SignInGlyph.Glyph = GlyphError;
            SignInGlyph.Foreground = brushCritical;
            SignInText.Text = "Codex CLI missing — go back to Step 1.";
            SignInBtn.Visibility = Visibility.Collapsed;
            RecheckSignInBtn.Visibility = Visibility.Visible;
        }
        if (_currentStep == Step.SignIn)
            IsPrimaryButtonEnabled = _signInOk;
    }

    private void SignIn_Click(object sender, RoutedEventArgs e)
    {
        var ok = DimmyNative.SpawnCodexLogin();
        if (!ok)
        {
            SignInText.Text = "Couldn't spawn the login flow. Run `codex login` from a terminal manually.";
            return;
        }
        SignInText.Text = "Complete the ChatGPT sign-in. Dimmy will detect when you're signed in…";
        SignInRing.IsActive = true;
        SignInRing.Visibility = Visibility.Visible;
        SignInBtn.IsEnabled = false;
        StartCredentialsPoll();
    }

    private void RecheckSignIn_Click(object sender, RoutedEventArgs e)
    {
        var rc = DimmyNative.RecheckCodex();
        _codexOk = rc != DimmyNative.ClaudeCodeStatus.NotInstalled;
        _signInOk = rc == DimmyNative.ClaudeCodeStatus.Ready;
        UpdateSignInUi(rc);
        if (_signInOk && !_testOk) _ = AutoRunTestAsync();
    }

    private void StartCredentialsPoll()
    {
        StopCredentialsPoll();
        var dq = DispatcherQueue.GetForCurrentThread();
        _credentialsPollTimer = dq.CreateTimer();
        _credentialsPollTimer.Interval = TimeSpan.FromSeconds(2);
        _credentialsPollTimer.IsRepeating = true;
        _credentialsPollTimer.Tick += (s, _) =>
        {
            var rc = DimmyNative.RecheckCodex();
            if (rc == DimmyNative.ClaudeCodeStatus.Ready)
            {
                StopCredentialsPoll();
                _signInOk = true;
                SignInRing.IsActive = false;
                SignInRing.Visibility = Visibility.Collapsed;
                SignInBtn.IsEnabled = true;
                UpdateSignInUi(rc);
                _ = AutoRunTestAsync();
            }
        };
        _credentialsPollTimer.Start();
    }

    private void StopCredentialsPoll()
    {
        _credentialsPollTimer?.Stop();
        _credentialsPollTimer = null;
    }

    private async Task AutoRunTestAsync()
    {
        if (_testOk) return;
        _autoTestCts?.Cancel();
        _autoTestCts = new CancellationTokenSource();
        var ct = _autoTestCts.Token;
        TestStatusRow.Visibility = Visibility.Visible;
        TestGlyph.Glyph = GlyphPending;
        var brushNeutral = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];
        TestGlyph.Foreground = brushNeutral;
        TestText.Text = "Test connection: pinging ChatGPT…";
        TestRing.IsActive = true;
        TestRing.Visibility = Visibility.Visible;
        RetryTestBtn.Visibility = Visibility.Collapsed;
        IsPrimaryButtonEnabled = false;

        var (result, elapsedMs) = await Task.Run(() => DimmyNative.PingCodex(), ct);
        if (ct.IsCancellationRequested) return;
        TestRing.IsActive = false;
        TestRing.Visibility = Visibility.Collapsed;

        var brushSuccess = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
        var brushCritical = (Brush)Application.Current.Resources["SystemFillColorCriticalBrush"];
        if (result == DimmyNative.ClaudeCodePingResult.Ok)
        {
            _testOk = true;
            TestGlyph.Glyph = GlyphSuccess;
            TestGlyph.Foreground = brushSuccess;
            TestText.Text = $"Test connection OK ({elapsedMs} ms). ChatGPT subscription ready.";
            RetryTestBtn.Visibility = Visibility.Collapsed;
            PrimaryButtonText = "Close & enable";
            IsPrimaryButtonEnabled = true;
        }
        else
        {
            _testOk = false;
            TestGlyph.Glyph = GlyphError;
            TestGlyph.Foreground = brushCritical;
            TestText.Text = $"Test failed: {DescribePingResult(result)}";
            RetryTestBtn.Visibility = Visibility.Visible;
            PrimaryButtonText = "Test connection";
            IsPrimaryButtonEnabled = true;
        }
    }

    private static string DescribePingResult(DimmyNative.ClaudeCodePingResult r) => r switch
    {
        DimmyNative.ClaudeCodePingResult.NotInstalled => "Codex CLI not installed",
        DimmyNative.ClaudeCodePingResult.NotLoggedIn => "Not signed in",
        DimmyNative.ClaudeCodePingResult.SpawnFailed => "Couldn't spawn the CLI subprocess",
        DimmyNative.ClaudeCodePingResult.Timeout => "Timed out (30 s)",
        DimmyNative.ClaudeCodePingResult.NonZeroExit => "CLI returned an error code",
        DimmyNative.ClaudeCodePingResult.InvalidUtf8 => "CLI returned invalid output",
        _ => "Unknown error",
    };

    private void RetryTest_Click(object sender, RoutedEventArgs e)
    {
        _testOk = false;
        _ = AutoRunTestAsync();
    }

    // ── ContentDialog buttons ─────────────────────────────────────────

    private void OnPrimaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        switch (_currentStep)
        {
            case Step.Install:
                if (!_codexOk) { args.Cancel = true; return; }
                args.Cancel = true;
                EnterStep(Step.SignIn);
                break;
            case Step.SignIn:
                if (_testOk)
                {
                    Completed = true;
                    // args.Cancel stays false → dialog closes.
                }
                else if (_signInOk)
                {
                    args.Cancel = true;
                    _ = AutoRunTestAsync();
                }
                else
                {
                    args.Cancel = true;
                }
                break;
        }
    }

    private void OnSecondaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        args.Cancel = true;
        EnterStep(Step.Install);
    }

    private void OnCloseClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        // Default: dialog closes with Completed=false.
    }
}
