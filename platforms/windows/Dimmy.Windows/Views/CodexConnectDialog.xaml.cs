using System;
using System.Diagnostics;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.ApplicationModel.DataTransfer;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Views;

/// <summary>
/// Guided 3-page Codex setup wizard (Install -> Run -> Finish). Linear,
/// modal, re-runnable. The Codex CLI is a native standalone binary (no
/// Node.js); page 1 offers winget / PowerShell / npm install commands,
/// defaulting to winget because it runs in plain cmd with no PowerShell 7
/// or Node dependency. Copy and Open-terminal auto-advance; the Finish
/// page polls for the binary, drives the sign-in, and enables Done on a
/// green status.
/// </summary>
public sealed partial class CodexConnectDialog : ContentDialog
{
    public bool Completed { get; private set; }

    /// <summary>Start at page 1 regardless of detection (the "Re-run setup"
    /// entry point from the connected card).</summary>
    public bool ForceStartAtStep1 { get; set; }

    private enum Page { Install = 1, Run = 2, Finish = 3 }

    private const string GlyphSuccess = ""; // CheckMark
    private const string GlyphPending = ""; // neutral placeholder
    private const string GlyphInfo = "";    // Info

    private const string CmdWinget = "winget install OpenAI.Codex";
    private const string CmdPwsh = "irm https://chatgpt.com/codex/install.ps1 | iex";
    private const string CmdNpm = "npm install -g @openai/codex";

    private Page _currentPage = Page.Install;
    private bool _codexOk;
    private bool _signInOk;
    private DispatcherQueueTimer? _pollTimer;

    public CodexConnectDialog()
    {
        InitializeComponent();
        Opened += OnOpened;
        Closed += OnClosed;
    }

    private void OnOpened(ContentDialog sender, ContentDialogOpenedEventArgs args)
    {
        ProbeStatus();
        var start = Page.Install;
        if (!ForceStartAtStep1 && _codexOk) start = Page.Finish;
        EnterPage(start);
    }

    private void OnClosed(ContentDialog sender, ContentDialogClosedEventArgs args)
    {
        StopPoll();
    }

    private void ProbeStatus()
    {
        try
        {
            var s = DimmyNative.GetCodexStatus();
            _codexOk = s != DimmyNative.ClaudeCodeStatus.NotInstalled;
            _signInOk = s == DimmyNative.ClaudeCodeStatus.Ready;
        }
        catch (Exception ex)
        {
            App.Log($"CodexWizard: probe exc {ex.Message}", "Codex");
            _codexOk = false;
            _signInOk = false;
        }
    }

    // ── State machine ─────────────────────────────────────────────────

    private void EnterPage(Page page)
    {
        _currentPage = page;
        Page1Panel.Visibility = page == Page.Install ? Visibility.Visible : Visibility.Collapsed;
        Page2Panel.Visibility = page == Page.Run ? Visibility.Visible : Visibility.Collapsed;
        Page3Panel.Visibility = page == Page.Finish ? Visibility.Visible : Visibility.Collapsed;

        var accent = (Brush)Application.Current.Resources["AccentFillColorDefaultBrush"];
        // Resolve the inactive grey from the dialog's ACTUAL theme, not the
        // app resources (which return the app-theme brush and can render
        // near-white on a light dialog). Explicit greys, clearly visible on
        // each background.
        bool dark = ActualTheme == ElementTheme.Dark;
        Brush inactive = new SolidColorBrush(dark
            ? Microsoft.UI.ColorHelper.FromArgb(0xFF, 0xA6, 0xA6, 0xA6)
            : Microsoft.UI.ColorHelper.FromArgb(0xFF, 0x70, 0x70, 0x70));
        Dot1.Fill = page >= Page.Install ? accent : inactive;
        Dot2.Fill = page >= Page.Run ? accent : inactive;
        Dot3.Fill = page >= Page.Finish ? accent : inactive;

        SecondaryButtonText = page == Page.Install ? "" : "Back";
        switch (page)
        {
            case Page.Install:
                // No footer "Next" — the blue in-content "Copy and continue"
                // button is the single action, and it auto-advances. Keeps
                // exactly one accent (blue) button per page.
                PrimaryButtonText = "";
                break;
            case Page.Run:
                PrimaryButtonText = "";
                break;
            case Page.Finish:
                PrimaryButtonText = "Done";
                IsPrimaryButtonEnabled = _signInOk;
                StartPoll();
                UpdateFinishUi();
                break;
        }
    }

    // ── Page 1 — Install ──────────────────────────────────────────────

    private string SelectedCommand()
    {
        if (TabPwsh.IsChecked == true) return CmdPwsh;
        if (TabNpm.IsChecked == true) return CmdNpm;
        return CmdWinget;
    }

    private void CmdTab_Checked(object sender, RoutedEventArgs e)
    {
        // Fires during InitializeComponent before CommandText exists.
        if (CommandText == null) return;
        CommandText.Text = SelectedCommand();
    }

    private void CopyCommand_Click(object sender, RoutedEventArgs e)
    {
        var pkg = new DataPackage();
        pkg.SetText(SelectedCommand());
        Clipboard.SetContent(pkg);
        EnterPage(Page.Run);
    }

    // ── Page 2 — Run ──────────────────────────────────────────────────

    private void OpenTerminal_Click(object sender, RoutedEventArgs e)
    {
        // Auto-run: open a terminal that immediately runs the selected
        // install command, so the user doesn't have to paste or type.
        // cmd for winget/npm (works everywhere), PowerShell for the irm tab.
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        var cmd = SelectedCommand();
        if (TabPwsh.IsChecked == true)
            TrySpawn("powershell.exe", $"-NoExit -Command \"{cmd}\"", home);
        else if (!TrySpawn("cmd.exe", $"/K {cmd}", home))
            TrySpawn("powershell.exe", $"-NoExit -Command \"{cmd}\"", home);
        EnterPage(Page.Finish);
    }

    private static bool TrySpawn(string fileName, string args, string workDir)
    {
        try
        {
            Process.Start(new ProcessStartInfo
            {
                FileName = fileName,
                Arguments = args,
                UseShellExecute = true,
                WorkingDirectory = workDir,
            });
            return true;
        }
        catch
        {
            return false;
        }
    }

    // ── Page 3 — Finish ───────────────────────────────────────────────

    private void StartPoll()
    {
        StopPoll();
        var dq = DispatcherQueue.GetForCurrentThread();
        _pollTimer = dq.CreateTimer();
        _pollTimer.Interval = TimeSpan.FromSeconds(2);
        _pollTimer.IsRepeating = true;
        _pollTimer.Tick += (s, _) =>
        {
            var prevSignedIn = _signInOk;
            DimmyNative.RecheckCodex();
            ProbeStatus();
            UpdateFinishUi();
            if (_signInOk && !prevSignedIn)
                StopPoll(); // fully done — stop hammering the CLI
        };
        _pollTimer.Start();
    }

    private void StopPoll()
    {
        _pollTimer?.Stop();
        _pollTimer = null;
    }

    private void UpdateFinishUi()
    {
        var success = (Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
        var neutral = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];

        if (!_codexOk)
        {
            InstallGlyph.Glyph = GlyphPending;
            InstallGlyph.Foreground = neutral;
            InstallText.Text = "Run the command in your terminal. It appears here when done.";
            InstallRing.IsActive = true;
            InstallRing.Visibility = Visibility.Visible;
            SignInRow.Visibility = Visibility.Collapsed;
            SignInBtn.Visibility = Visibility.Collapsed;
            IsPrimaryButtonEnabled = false;
            return;
        }

        // Binary present.
        InstallGlyph.Glyph = GlyphSuccess;
        InstallGlyph.Foreground = success;
        InstallText.Text = "Codex CLI installed.";
        InstallRing.IsActive = false;
        InstallRing.Visibility = Visibility.Collapsed;
        SignInRow.Visibility = Visibility.Visible;

        if (_signInOk)
        {
            SignInGlyph.Glyph = GlyphSuccess;
            SignInGlyph.Foreground = success;
            SignInText.Text = "Signed in to ChatGPT.";
            SignInBtn.Visibility = Visibility.Collapsed;
            SignInRing.IsActive = false;
            SignInRing.Visibility = Visibility.Collapsed;
            IsPrimaryButtonEnabled = true;
        }
        else
        {
            SignInGlyph.Glyph = GlyphInfo;
            SignInGlyph.Foreground = neutral;
            SignInText.Text = "Not signed in yet.";
            SignInBtn.Visibility = Visibility.Visible;
            IsPrimaryButtonEnabled = false;
        }
    }

    private void SignIn_Click(object sender, RoutedEventArgs e)
    {
        var ok = DimmyNative.SpawnCodexLogin();
        if (!ok)
        {
            SignInText.Text = "Couldn't start the sign-in. Try again, or run `codex login` in a terminal.";
            return;
        }
        SignInText.Text = "Complete the browser sign-in. Dimmy detects it automatically...";
        SignInRing.IsActive = true;
        SignInRing.Visibility = Visibility.Visible;
        SignInBtn.IsEnabled = false;
        StartPoll();
    }

    private void Recheck_Click(object sender, RoutedEventArgs e)
    {
        DimmyNative.RecheckCodex();
        ProbeStatus();
        UpdateFinishUi();
    }

    // ── ContentDialog buttons ─────────────────────────────────────────

    private void OnPrimaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        switch (_currentPage)
        {
            case Page.Install:
                args.Cancel = true;
                EnterPage(Page.Run);
                break;
            case Page.Run:
                args.Cancel = true;
                EnterPage(Page.Finish);
                break;
            case Page.Finish:
                if (_signInOk)
                {
                    Completed = true; // dialog closes
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
        StopPoll();
        var prev = _currentPage switch
        {
            Page.Run => Page.Install,
            Page.Finish => Page.Run,
            _ => Page.Install,
        };
        EnterPage(prev);
    }

    private void OnCloseClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        // Closes with Completed=false. Caller decides what to do.
    }
}
