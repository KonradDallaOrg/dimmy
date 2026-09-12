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
/// Guided 3-page Gemini CLI setup wizard (Install -> Run -> Finish).
/// Linear, modal, re-runnable — the same machine as the Codex wizard, and
/// deliberately so: a user who has been through one should recognise the
/// other.
///
/// <para>Three things are specific to this CLI:</para>
/// <list type="bullet">
/// <item>It ships on npm, so page 1 defaults to npm and warns about Node.
/// There is no winget package, which is why the Codex wizard's default tab
/// has no counterpart here.</item>
/// <item>There is no <c>gemini login</c>. Sign-in launches the CLI bare and
/// the user picks "Login with Google" from the auth menu it shows.</item>
/// <item>The npx tab installs nothing, so its Finish page can never go
/// green. It is offered for trying the CLI out, and the wizard says so
/// rather than leaving the user staring at a spinner.</item>
/// </list>
/// </summary>
public sealed partial class GeminiConnectDialog : ContentDialog
{
    public bool Completed { get; private set; }

    /// <summary>Start at page 1 regardless of detection (the "Re-run setup"
    /// entry point from the connected card).</summary>
    public bool ForceStartAtStep1 { get; set; }

    private enum Page { Install = 1, Run = 2, Finish = 3 }

    private const string GlyphSuccess = ""; // CheckMark
    private const string GlyphPending = ""; // neutral placeholder
    private const string GlyphInfo = "";    // Info
    private const string GlyphCopy = "";    // Copy

    private const string CmdNpm = "npm install -g @google/gemini-cli";
    private const string CmdNpx = "npx @google/gemini-cli";
    private const string CmdBrew = "brew install gemini-cli";

    private const string NoteNpm = "Needs Node.js 20 or newer.";
    private const string NoteNpx =
        "Runs it without installing. Good for a look, but Dimmy needs an installed `gemini` to call, so pick npm to finish setup.";
    private const string NoteBrew =
        "Homebrew is macOS and Linux only. On Windows use the npm tab.";

    private Page _currentPage = Page.Install;
    private bool _geminiOk;
    private bool _signInOk;
    private DispatcherQueueTimer? _pollTimer;
    private DispatcherQueueTimer? _copyFeedbackTimer;

    public GeminiConnectDialog()
    {
        InitializeComponent();
        Opened += OnOpened;
        Closed += OnClosed;
    }

    private void OnOpened(ContentDialog sender, ContentDialogOpenedEventArgs args)
    {
        ProbeStatus();
        var start = Page.Install;
        if (!ForceStartAtStep1 && _geminiOk) start = Page.Finish;
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
            var s = DimmyNative.GetGeminiCliStatus();
            _geminiOk = s != DimmyNative.ClaudeCodeStatus.NotInstalled;
            _signInOk = s == DimmyNative.ClaudeCodeStatus.Ready;
        }
        catch (Exception ex)
        {
            App.Log($"GeminiWizard: probe exc {ex.Message}", "Gemini");
            _geminiOk = false;
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
        // near-white on a light dialog).
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
                PrimaryButtonText = "Next";
                IsPrimaryButtonEnabled = true;
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
        if (TabNpx.IsChecked == true) return CmdNpx;
        if (TabBrew.IsChecked == true) return CmdBrew;
        return CmdNpm;
    }

    private void CmdTab_Checked(object sender, RoutedEventArgs e)
    {
        // Fires during InitializeComponent before the fields exist.
        if (CommandText == null || TabNoteText == null) return;
        CommandText.Text = SelectedCommand();
        TabNoteText.Text =
            TabNpx.IsChecked == true ? NoteNpx :
            TabBrew.IsChecked == true ? NoteBrew :
            NoteNpm;
    }

    /// <summary>Best-effort clipboard write. SetContent throws
    /// CLIPBRD_E_CANT_OPEN when another process holds the clipboard open
    /// (remote-desktop sessions and clipboard managers do this routinely);
    /// copying is a convenience here, never worth crashing a click
    /// handler.</summary>
    private static bool TrySetClipboard(string text)
    {
        try
        {
            var pkg = new DataPackage();
            pkg.SetText(text);
            Clipboard.SetContent(pkg);
            return true;
        }
        catch (Exception ex)
        {
            App.Log($"GeminiWizard: clipboard set failed {ex.Message}", "Gemini");
            return false;
        }
    }

    private void CopyCmd_Click(object sender, RoutedEventArgs e)
    {
        if (!TrySetClipboard(SelectedCommand())) return;
        CopyCmdGlyph.Glyph = GlyphSuccess;
        _copyFeedbackTimer?.Stop();
        var dq = DispatcherQueue.GetForCurrentThread();
        _copyFeedbackTimer = dq.CreateTimer();
        _copyFeedbackTimer.Interval = TimeSpan.FromSeconds(1.5);
        _copyFeedbackTimer.IsRepeating = false;
        _copyFeedbackTimer.Tick += (s, _) => CopyCmdGlyph.Glyph = GlyphCopy;
        _copyFeedbackTimer.Start();
    }

    // ── Page 2 — Run ──────────────────────────────────────────────────

    private void OpenTerminal_Click(object sender, RoutedEventArgs e)
    {
        // Auto-run: open a terminal that immediately runs the selected
        // install command, so the user doesn't have to paste or type.
        var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
        var cmd = SelectedCommand();
        // The page-2 info flyout promises the command on the clipboard as a
        // fallback if the terminal fails — keep that promise.
        TrySetClipboard(cmd);
        if (!TrySpawn("cmd.exe", $"/K {cmd}", home))
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

    // Documented exception to the no-FFI-polling rule (CLAUDE.md): the
    // condition being awaited is an EXTERNAL filesystem change (the user
    // finishing `npm install` or a browser login in another process). The
    // core cannot emit an event for something it hasn't observed; recheck +
    // probe every 2 s while this page is open IS the event source. The
    // timer stops on sign-in success and on dialog close.
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
            DimmyNative.RecheckGeminiCli();
            ProbeStatus();
            UpdateFinishUi();
            if (_signInOk && !prevSignedIn)
                StopPoll(); // fully done — stop hammering the filesystem
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

        if (!_geminiOk)
        {
            InstallGlyph.Glyph = GlyphPending;
            InstallGlyph.Foreground = neutral;
            // npx installs nothing, so this page would spin forever on that
            // tab. Say why instead of implying the install is still running.
            InstallText.Text = TabNpx.IsChecked == true
                ? "npx doesn't install anything. Go Back and choose the npm tab to finish setup."
                : "Run the command in your terminal. It appears here when done.";
            InstallRing.IsActive = TabNpx.IsChecked != true;
            InstallRing.Visibility = TabNpx.IsChecked == true
                ? Visibility.Collapsed
                : Visibility.Visible;
            SignInRow.Visibility = Visibility.Collapsed;
            SignInBtn.Visibility = Visibility.Collapsed;
            SignInHint.Visibility = Visibility.Collapsed;
            IsPrimaryButtonEnabled = false;
            return;
        }

        // Binary present.
        InstallGlyph.Glyph = GlyphSuccess;
        InstallGlyph.Foreground = success;
        InstallText.Text = "Gemini CLI installed.";
        InstallRing.IsActive = false;
        InstallRing.Visibility = Visibility.Collapsed;
        SignInRow.Visibility = Visibility.Visible;

        if (_signInOk)
        {
            SignInGlyph.Glyph = GlyphSuccess;
            SignInGlyph.Foreground = success;
            SignInText.Text = "Signed in with Google.";
            SignInBtn.Visibility = Visibility.Collapsed;
            SignInHint.Visibility = Visibility.Collapsed;
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
            SignInHint.Visibility = Visibility.Visible;
            IsPrimaryButtonEnabled = false;
        }
    }

    private void SignIn_Click(object sender, RoutedEventArgs e)
    {
        var ok = DimmyNative.SpawnGeminiCliLogin();
        if (!ok)
        {
            SignInText.Text = "Couldn't start the sign-in. Try again, or run `gemini` in a terminal.";
            return;
        }
        SignInText.Text = "Choose 'Login with Google' in the terminal. Dimmy detects it automatically...";
        SignInRing.IsActive = true;
        SignInRing.Visibility = Visibility.Visible;
        SignInBtn.IsEnabled = false;
        StartPoll();
    }

    private void Recheck_Click(object sender, RoutedEventArgs e)
    {
        DimmyNative.RecheckGeminiCli();
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
