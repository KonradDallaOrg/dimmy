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
/// 3-step Claude Code subscription setup wizard. Linear, modal,
/// re-runnable. Same shape as <see cref="NotionConnectDialog"/>:
/// internal state machine flips StepNPanel visibility + relabels
/// the ContentDialog buttons per step.
///
/// Steps:
///   1. Detect / install Node.js (≥ 18 required).
///   2. Detect / install Claude CLI (npm install -g).
///   3. Sign in (browser OAuth via `claude /login`) + auto-test
///      a tiny ping prompt.
///
/// Smart-skip: on Open we probe every precondition once and start
/// at the FIRST incomplete step. A machine where everything's
/// already set lands on Step 3 with test-ready state + the
/// "Close &amp; enable" button armed.
///
/// Completion contract: <see cref="Completed"/> is true iff the
/// user reached Step 3 with a green test. The SettingsWindow
/// caller then sets <c>llm_auth_method=subscription</c> via FFI.
/// </summary>
public sealed partial class ClaudeConnectDialog : ContentDialog
{
    /// <summary>True iff Step 3's test ping returned a positive result.</summary>
    public bool Completed { get; private set; }

    /// <summary>
    /// When true, bypass the smart-skip and start at Step 1 regardless
    /// of current detection state. Caller uses this for "Re-run setup"
    /// (e.g. user wants to walk through the wizard again to inspect /
    /// re-validate their install) — without it the smart-skip would
    /// jump straight to Step 3 on an already-configured machine.
    /// </summary>
    public bool ForceStartAtStep1 { get; set; }

    private enum Step { Node = 1, ClaudeCli = 2, SignIn = 3 }

    // Segoe Fluent Icons codepoints. Using compile-time consts so
    // a source-encoding hiccup can't strip the literals (which would
    // leave the FontIcon glyph blank, showing as a rendering box —
    // the bug that shipped to the user on the first build).
    private const string GlyphSuccess = "";   // Checkmark
    private const string GlyphCaution = "";   // Warning triangle
    private const string GlyphCritical = "";  // Important / X-on-circle
    private const string GlyphPending = "";   // Globe / placeholder
    private const string GlyphInfo = "";      // Info

    private Step _currentStep = Step.Node;
    private bool _nodeOk;
    private bool _claudeOk;
    private bool _signInOk;
    private bool _testOk;

    private DispatcherQueueTimer? _credentialsPollTimer;
    private CancellationTokenSource? _autoTestCts;

    public ClaudeConnectDialog()
    {
        InitializeComponent();
        Opened += OnOpened;
        Closed += OnClosed;
    }

    // ── Lifecycle ─────────────────────────────────────────────────────

    private void OnOpened(ContentDialog sender, ContentDialogOpenedEventArgs args)
    {
        // Smart-skip: probe everything once, jump to first incomplete
        // step. The recheck buttons inside each step do their own per-
        // step probe so the state stays fresh as the user progresses.
        ProbeAllStates();
        var startAt = Step.Node;
        if (!ForceStartAtStep1)
        {
            if (_nodeOk) startAt = Step.ClaudeCli;
            if (_nodeOk && _claudeOk) startAt = Step.SignIn;
        }
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
            var node = DimmyNative.GetNodeStatus();
            _nodeOk = node.Found && node.MeetsMinimum;
            UpdateNodeUi(node);
        }
        catch (Exception ex)
        {
            App.Log($"ClaudeWizard: node probe exc {ex.Message}", "Wizard");
            _nodeOk = false;
        }
        try
        {
            var s = DimmyNative.GetClaudeCodeStatus();
            _claudeOk = s != DimmyNative.ClaudeCodeStatus.NotInstalled;
            _signInOk = s == DimmyNative.ClaudeCodeStatus.Ready;
            UpdateClaudeUi(s);
            UpdateSignInUi(s);
        }
        catch (Exception ex)
        {
            App.Log($"ClaudeWizard: claude probe exc {ex.Message}", "Wizard");
            _claudeOk = false;
            _signInOk = false;
        }
    }

    // ── State machine ─────────────────────────────────────────────────

    private void EnterStep(Step step)
    {
        _currentStep = step;
        Step1Panel.Visibility = step == Step.Node ? Visibility.Visible : Visibility.Collapsed;
        Step2Panel.Visibility = step == Step.ClaudeCli ? Visibility.Visible : Visibility.Collapsed;
        Step3Panel.Visibility = step == Step.SignIn ? Visibility.Visible : Visibility.Collapsed;

        var accent = (Brush)Application.Current.Resources["AccentFillColorDefaultBrush"];
        var inactive = (Brush)Application.Current.Resources["ControlStrokeColorDefaultBrush"];
        Dot1.Fill = step >= Step.Node ? accent : inactive;
        Dot2.Fill = step >= Step.ClaudeCli ? accent : inactive;
        Dot3.Fill = step >= Step.SignIn ? accent : inactive;

        // Button visibility / labels per step.
        SecondaryButtonText = step == Step.Node ? "" : "Back";
        switch (step)
        {
            case Step.Node:
                PrimaryButtonText = "Next";
                IsPrimaryButtonEnabled = _nodeOk;
                break;
            case Step.ClaudeCli:
                PrimaryButtonText = "Next";
                IsPrimaryButtonEnabled = _claudeOk;
                break;
            case Step.SignIn:
                PrimaryButtonText = _testOk ? "Close & enable" : "Test connection";
                IsPrimaryButtonEnabled = _signInOk;
                // Auto-fire test when we landed here already signed in
                // (e.g. user comes back to wizard after manual install).
                if (_signInOk && !_testOk) _ = AutoRunTestAsync();
                break;
        }
    }

    // ── Step 1 — Node.js ──────────────────────────────────────────────

    private void UpdateNodeUi(DimmyNative.NodeStatus node)
    {
        var brushSuccess = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
        var brushCaution = (Brush)Application.Current.Resources["SystemFillColorCautionBrush"];
        var brushCritical = (Brush)Application.Current.Resources["SystemFillColorCriticalBrush"];

        if (node.Found && node.MeetsMinimum)
        {
            NodeStatusGlyph.Glyph = "";
            NodeStatusGlyph.Foreground = brushSuccess;
            NodeStatusText.Text = node.Version != null
                ? $"Node.js v{node.Version} detected"
                : "Node.js detected";
            NodeActionPanel.Visibility = Visibility.Collapsed;
            NodeHelpText.Visibility = Visibility.Collapsed;
        }
        else if (node.Found && !node.MeetsMinimum)
        {
            NodeStatusGlyph.Glyph = "";
            NodeStatusGlyph.Foreground = brushCaution;
            NodeStatusText.Text = node.Version != null
                ? $"Node.js v{node.Version} found, but Claude needs v18 or newer. Please upgrade."
                : "Node.js found but version too old (need v18+).";
            NodeActionPanel.Visibility = Visibility.Visible;
            NodeHelpText.Visibility = Visibility.Visible;
        }
        else
        {
            NodeStatusGlyph.Glyph = "";
            NodeStatusGlyph.Foreground = brushCritical;
            NodeStatusText.Text = "Node.js not found. Click below to install.";
            NodeActionPanel.Visibility = Visibility.Visible;
            NodeHelpText.Visibility = Visibility.Visible;
        }
        IsPrimaryButtonEnabled = _currentStep == Step.Node ? _nodeOk : IsPrimaryButtonEnabled;
    }

    private void OpenNodeJs_Click(object sender, RoutedEventArgs e)
    {
        // Latest LTS landing page — auto-detects OS + offers the right
        // installer. Better UX than hard-coding the .msi URL (which
        // bakes in a version and goes stale).
        OpenUrl("https://nodejs.org/en/download/");
    }

    private void RecheckNode_Click(object sender, RoutedEventArgs e)
    {
        DimmyNative.RecheckClaudeCode(); // clears node + claude caches
        var node = DimmyNative.GetNodeStatus();
        _nodeOk = node.Found && node.MeetsMinimum;
        UpdateNodeUi(node);
        IsPrimaryButtonEnabled = _nodeOk;
    }

    // ── Step 2 — Claude CLI ───────────────────────────────────────────

    private void UpdateClaudeUi(DimmyNative.ClaudeCodeStatus status)
    {
        var brushSuccess = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
        var brushCritical = (Brush)Application.Current.Resources["SystemFillColorCriticalBrush"];

        if (status != DimmyNative.ClaudeCodeStatus.NotInstalled)
        {
            var path = DimmyNative.GetClaudeCodeBinaryPath() ?? "";
            ClaudeStatusGlyph.Glyph = "";
            ClaudeStatusGlyph.Foreground = brushSuccess;
            ClaudeStatusText.Text = string.IsNullOrEmpty(path)
                ? "Claude CLI detected"
                : $"Claude CLI detected at {path}";
            ClaudeActionPanel.Visibility = Visibility.Collapsed;
            ClaudeHelpText.Visibility = Visibility.Collapsed;
        }
        else
        {
            ClaudeStatusGlyph.Glyph = "";
            ClaudeStatusGlyph.Foreground = brushCritical;
            ClaudeStatusText.Text = "Claude CLI not installed. Run the command below in a terminal.";
            ClaudeActionPanel.Visibility = Visibility.Visible;
            ClaudeHelpText.Visibility = Visibility.Visible;
        }
        IsPrimaryButtonEnabled = _currentStep == Step.ClaudeCli ? _claudeOk : IsPrimaryButtonEnabled;
    }

    private void CopyNpmCommand_Click(object sender, RoutedEventArgs e)
    {
        var pkg = new DataPackage();
        pkg.SetText("npm install -g @anthropic-ai/claude-code");
        Clipboard.SetContent(pkg);
        CopyCmdBtn.Content = "Copied!";
        // Restore label after 1.5 s so the user doesn't see "Copied!"
        // forever — they may want to copy again.
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
        // Try Windows Terminal first (Win11 default, modern UX). Fall
        // back to PowerShell, then cmd if even that fails — every
        // Windows install has cmd.exe so the chain can't end empty-handed.
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

    private void RecheckClaude_Click(object sender, RoutedEventArgs e)
    {
        var rc = DimmyNative.RecheckClaudeCode();
        _claudeOk = rc != DimmyNative.ClaudeCodeStatus.NotInstalled;
        _signInOk = rc == DimmyNative.ClaudeCodeStatus.Ready;
        UpdateClaudeUi(rc);
        IsPrimaryButtonEnabled = _claudeOk;
    }

    // ── Step 3 — Sign in + Test ───────────────────────────────────────

    private void UpdateSignInUi(DimmyNative.ClaudeCodeStatus status)
    {
        var brushSuccess = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
        var brushCritical = (Brush)Application.Current.Resources["SystemFillColorCriticalBrush"];

        if (status == DimmyNative.ClaudeCodeStatus.Ready)
        {
            SignInGlyph.Glyph = "";
            SignInGlyph.Foreground = brushSuccess;
            SignInText.Text = "Signed in to Claude.";
            SignInBtn.Visibility = Visibility.Collapsed;
            RecheckSignInBtn.Visibility = Visibility.Collapsed;
        }
        else if (status == DimmyNative.ClaudeCodeStatus.NotLoggedIn)
        {
            SignInGlyph.Glyph = "";
            SignInGlyph.Foreground = brushCritical;
            SignInText.Text = "Not signed in. Click Sign in to open the browser flow.";
            SignInBtn.Visibility = Visibility.Visible;
            RecheckSignInBtn.Visibility = Visibility.Visible;
        }
        else
        {
            // NotInstalled — shouldn't land here normally; the smart-skip
            // sends the user back to Step 2 first.
            SignInGlyph.Glyph = "";
            SignInGlyph.Foreground = brushCritical;
            SignInText.Text = "Claude CLI missing — go back to Step 2.";
            SignInBtn.Visibility = Visibility.Collapsed;
            RecheckSignInBtn.Visibility = Visibility.Visible;
        }
        if (_currentStep == Step.SignIn)
            IsPrimaryButtonEnabled = _signInOk;
    }

    private void SignIn_Click(object sender, RoutedEventArgs e)
    {
        // Spawn `claude /login` — opens a new terminal window with the
        // OAuth URL. The user completes it in the browser, then we
        // detect the credentials file appearing via the poll below.
        var ok = DimmyNative.SpawnClaudeCodeLogin();
        if (!ok)
        {
            SignInText.Text = "Couldn't spawn the login flow. Try again or run `claude /login` from a terminal manually.";
            return;
        }
        SignInText.Text = "Complete the browser flow. Dimmy will detect when you're signed in…";
        SignInRing.IsActive = true;
        SignInRing.Visibility = Visibility.Visible;
        SignInBtn.IsEnabled = false;
        StartCredentialsPoll();
    }

    private void RecheckSignIn_Click(object sender, RoutedEventArgs e)
    {
        var rc = DimmyNative.RecheckClaudeCode();
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
            var rc = DimmyNative.RecheckClaudeCode();
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
        TestGlyph.Glyph = "";
        var brushNeutral = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];
        TestGlyph.Foreground = brushNeutral;
        TestText.Text = "Test connection: pinging Claude…";
        TestRing.IsActive = true;
        TestRing.Visibility = Visibility.Visible;
        RetryTestBtn.Visibility = Visibility.Collapsed;
        IsPrimaryButtonEnabled = false;

        var (result, elapsedMs) = await Task.Run(() => DimmyNative.PingClaudeCode(), ct);
        if (ct.IsCancellationRequested) return;
        TestRing.IsActive = false;
        TestRing.Visibility = Visibility.Collapsed;

        var brushSuccess = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
        var brushCritical = (Brush)Application.Current.Resources["SystemFillColorCriticalBrush"];
        if (result == DimmyNative.ClaudeCodePingResult.Ok)
        {
            _testOk = true;
            TestGlyph.Glyph = "";
            TestGlyph.Foreground = brushSuccess;
            TestText.Text = $"Test connection OK ({elapsedMs} ms). Claude subscription ready.";
            RetryTestBtn.Visibility = Visibility.Collapsed;
            PrimaryButtonText = "Close & enable";
            IsPrimaryButtonEnabled = true;
        }
        else
        {
            _testOk = false;
            TestGlyph.Glyph = "";
            TestGlyph.Foreground = brushCritical;
            TestText.Text = $"Test failed: {DescribePingResult(result)}";
            RetryTestBtn.Visibility = Visibility.Visible;
            PrimaryButtonText = "Test connection";
            IsPrimaryButtonEnabled = true;
        }
    }

    private static string DescribePingResult(DimmyNative.ClaudeCodePingResult r) => r switch
    {
        DimmyNative.ClaudeCodePingResult.NotInstalled => "Claude CLI not installed",
        DimmyNative.ClaudeCodePingResult.NotLoggedIn => "Not signed in",
        DimmyNative.ClaudeCodePingResult.SpawnFailed => "Couldn't spawn the CLI subprocess",
        DimmyNative.ClaudeCodePingResult.Timeout => "Timed out (15 s)",
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
            case Step.Node:
                if (!_nodeOk) { args.Cancel = true; return; }
                args.Cancel = true;
                EnterStep(Step.ClaudeCli);
                break;
            case Step.ClaudeCli:
                if (!_claudeOk) { args.Cancel = true; return; }
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
                    // Primary button is "Test connection" — fire ping
                    // without closing.
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
        var prev = _currentStep switch
        {
            Step.ClaudeCli => Step.Node,
            Step.SignIn => Step.ClaudeCli,
            _ => Step.Node,
        };
        EnterStep(prev);
    }

    private void OnCloseClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        // Default: dialog closes with Completed=false. Caller decides
        // whether to flip llm_auth_method=subscription based on the flag.
    }

    // ── Helpers ───────────────────────────────────────────────────────

    private static void OpenUrl(string url)
    {
        try
        {
            Process.Start(new ProcessStartInfo
            {
                FileName = url,
                UseShellExecute = true,
            });
        }
        catch (Exception ex)
        {
            App.Log($"ClaudeWizard: OpenUrl failed for {url}: {ex.Message}", "Wizard");
        }
    }
}
