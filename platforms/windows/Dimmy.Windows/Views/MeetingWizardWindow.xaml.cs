using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Views;

/// <summary>Four focused steps to get Meeting mode working: bind a shortcut,
/// confirm the recap model answers, decide where recaps go, then record one.
///
/// <para>Sibling of <see cref="CommandWizardWindow"/> and, like it, a separate
/// window rather than a variant of the onboarding wizard — whose step count is
/// pinned by a Mac-side assertion that <c>fatalError</c>s the app at
/// launch.</para>
///
/// <para>Step 2 is the reason this wizard has four steps and Command has
/// three. A recap that nobody can find is a recap that did not happen, and
/// both destinations (Notion, a synced folder) are one-time setup the user
/// otherwise has to go hunting for in Settings.</para></summary>
public sealed partial class MeetingWizardWindow : Window
{
    private int _step;
    private const int TotalSteps = 4;

    private readonly string _originalCombo;
    private bool _committed;
    private bool _armed;
    private bool _busy;

    public MeetingWizardWindow()
    {
        InitializeComponent();
        WindowHelper.ResizeLogical(this, 680, 600);

        // Follow the theme the user picked. Without this a wizard renders in
        // the SYSTEM theme while the rest of the app honours the setting, so a
        // user on light with a dark Windows gets a dark window out of nowhere.
        if (Content is FrameworkElement themeRoot)
            themeRoot.RequestedTheme = ThemeHelper.ResolvedElementTheme();

        _originalCombo = App.Instance?.UiPrefs?.MeetingHotkey ?? "";
        Recorder.Shortcut = string.IsNullOrWhiteSpace(_originalCombo)
            ? "Ctrl+Alt+M"
            : _originalCombo;
        Recorder.ShortcutChanged += (_, __) => ValidateShortcut();

        Closed += (_, __) => Teardown();
        RenderStep();
        ValidateShortcut();
    }

    // ── Steps ───────────────────────────────────────────────────────

    private void RenderStep()
    {
        Step0.Visibility = _step == 0 ? Visibility.Visible : Visibility.Collapsed;
        Step1.Visibility = _step == 1 ? Visibility.Visible : Visibility.Collapsed;
        Step2.Visibility = _step == 2 ? Visibility.Visible : Visibility.Collapsed;
        Step3.Visibility = _step == 3 ? Visibility.Visible : Visibility.Collapsed;
        BackBtn.Visibility = _step > 0 ? Visibility.Visible : Visibility.Collapsed;
        SkipBtn.Visibility = _step is 1 or 2 ? Visibility.Visible : Visibility.Collapsed;
        StepLabel.Text = $"Step {_step + 1} of {TotalSteps}";
        NextBtn.Content = _step == TotalSteps - 1 ? "Done" : "Continue";

        switch (_step)
        {
            case 0:
                TitleLabel.Text = "Pick a shortcut";
                SubtitleLabel.Text = "One key combination starts and stops a meeting from anywhere.";
                break;
            case 1:
                TitleLabel.Text = "Check the recap model";
                SubtitleLabel.Text =
                    "The recap is the whole point of a meeting recording. "
                    + "One tiny call proves the model answers before you rely on it.";
                ShowModelSummary();
                break;
            case 2:
                TitleLabel.Text = "Where the recap goes";
                SubtitleLabel.Text = "Both are optional. The recap is always saved with the meeting.";
                RefreshDestinations();
                break;
            case 3:
                TitleLabel.Text = "Record a real one";
                SubtitleLabel.Text = $"Press {Recorder.Shortcut}, say a few sentences, press it again.";
                ArmForTrial();
                break;
        }
    }

    private void Next_Click(object sender, RoutedEventArgs e)
    {
        if (_step == 0)
        {
            if (!ValidateShortcut()) return;
            CommitShortcut();
        }
        if (_step >= TotalSteps - 1) { Close(); return; }
        _step++;
        RenderStep();
    }

    private void Back_Click(object sender, RoutedEventArgs e)
    {
        if (_step == 3) DisarmTrial();
        if (_step == 0) return;
        _step--;
        RenderStep();
    }

    private void Skip_Click(object sender, RoutedEventArgs e)
    {
        _step = TotalSteps - 1;
        RenderStep();
    }

    // ── Step 0: shortcut ────────────────────────────────────────────

    private bool ValidateShortcut()
    {
        var combo = Recorder.Shortcut ?? "";
        if (string.IsNullOrWhiteSpace(combo))
        {
            Warn("Press a combination that includes a real key, like Ctrl+Alt+M.");
            return false;
        }
        var dictation = App.Instance?.AppViewModel?.Shortcut ?? "";
        if (!string.IsNullOrWhiteSpace(dictation) && Conflicts(combo, dictation))
        {
            Warn($"That is already your dictation shortcut ({dictation}). Pick another.");
            return false;
        }
        var command = App.Instance?.UiPrefs?.CommandHotkey ?? "";
        if (!string.IsNullOrWhiteSpace(command) && Conflicts(combo, command))
        {
            Warn($"That is already your command shortcut ({command}). Pick another.");
            return false;
        }
        ShortcutWarn.IsOpen = false;
        NextBtn.IsEnabled = true;
        return true;
    }

    private static bool Conflicts(string a, string b)
    {
        try { return DimmyNative.dimmy_hotkey_combos_conflict(a, b) == 1; }
        catch { return string.Equals(a, b, StringComparison.OrdinalIgnoreCase); }
    }

    private void Warn(string message)
    {
        ShortcutWarn.Message = message;
        ShortcutWarn.IsOpen = true;
        NextBtn.IsEnabled = false;
    }

    private void CommitShortcut()
    {
        var combo = Recorder.Shortcut ?? "";
        try
        {
            var prefs = App.Instance?.UiPrefs;
            if (prefs != null) { prefs.MeetingHotkey = combo; prefs.Save(); }
            App.Instance?.ReregisterMeetingHotkey(combo);
            _committed = true;
            App.Log($"MeetingWizard: bound {combo}", "Wizard");
        }
        catch (Exception ex) { App.Log($"MeetingWizard commit exc: {ex.Message}", "Wizard"); }
    }

    // ── Step 1: recap model ─────────────────────────────────────────

    private void ShowModelSummary()
    {
        var (label, override_) = ReadRecapModel();
        ModelSummary.Text = label;
        TestModelBtn.IsEnabled = true;
        _recapOverride = override_;
    }

    private string _recapOverride = "";

    /// <summary>What the recap will actually use. `recap_model_override` wins
    /// when set; empty means it inherits the main LLM config, which is a
    /// distinction worth showing rather than hiding behind one label.</summary>
    private static (string label, string modelOverride) ReadRecapModel()
    {
        try
        {
            var json = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_config_json, 65536);
            if (string.IsNullOrEmpty(json)) return ("No configuration found.", "");
            using var doc = System.Text.Json.JsonDocument.Parse(json);
            var root = doc.RootElement;
            var over = root.TryGetProperty("recap_model_override", out var o)
                ? (o.GetString() ?? "") : "";
            if (!string.IsNullOrWhiteSpace(over))
                return ($"Recaps use {over}.", over);
            var mode = root.TryGetProperty("llm_mode", out var m) ? (m.GetString() ?? "cloud") : "cloud";
            var key = mode == "local" ? "local_llm_model" : "llm_api_model";
            var model = root.TryGetProperty(key, out var v) ? (v.GetString() ?? "") : "";
            return (string.IsNullOrWhiteSpace(model)
                ? "No LLM model is configured yet. Set one in Settings, then come back."
                : $"Recaps inherit your main model, {model}.", "");
        }
        catch { return ("Could not read the configuration.", ""); }
    }

    private async void TestModel_Click(object sender, RoutedEventArgs e)
    {
        if (_busy) return;
        _busy = true;
        TestModelBtn.IsEnabled = false;
        TestRing.IsActive = true;
        TestResult.IsOpen = false;
        try
        {
            var over = _recapOverride;
            var answer = await Task.Run(() =>
                DimmyNative.ReadBuffer((b, n) => DimmyNative.dimmy_llm_call_raw(
                    "Reply with exactly: ok", over, 16, b, n), 4096));
            var ok = !string.IsNullOrWhiteSpace(answer);
            TestResult.Severity = ok ? InfoBarSeverity.Success : InfoBarSeverity.Error;
            TestResult.Message = ok
                ? $"The model answered: {answer!.Trim()}"
                : "No answer. Check the model and the API key in Settings.";
            TestResult.IsOpen = true;
        }
        catch (Exception ex)
        {
            TestResult.Severity = InfoBarSeverity.Error;
            TestResult.Message = $"The call failed: {ex.Message}";
            TestResult.IsOpen = true;
        }
        finally
        {
            TestRing.IsActive = false;
            TestModelBtn.IsEnabled = true;
            _busy = false;
        }
    }

    /// <summary>See the note in CommandWizardWindow: a bare
    /// `OpenSettingsWindow()` looks broken when Settings is already open
    /// behind this window.</summary>
    private void OpenSettings_Click(object sender, RoutedEventArgs e)
    {
        try { App.Instance?.OpenSettingsWindowAt("output"); }
        catch (Exception ex) { App.Log($"MeetingWizard settings exc: {ex.Message}", "Wizard"); }
    }

    // ── Step 2: destinations ────────────────────────────────────────

    private void RefreshDestinations()
    {
        var connected = false;
        try { connected = DimmyNative.dimmy_notion_has_token() == 1; } catch { }
        NotionState.Text = connected
            ? "Connected. Recaps can be pushed to a page or database."
            : "Not connected. Uses your own Notion integration token.";
        NotionBtn.Content = connected ? "Reconfigure" : "Set up Notion";
        NotionAuto.IsEnabled = connected;

        var prefs = App.Instance?.UiPrefs;
        var folder = prefs?.RecapExportFolder ?? "";
        FolderState.Text = string.IsNullOrWhiteSpace(folder) ? "No folder set" : folder;
        FolderBtn.Content = string.IsNullOrWhiteSpace(folder) ? "Choose a folder" : "Change";
    }

    /// <summary>Hands off to the existing three-step Notion dialog rather than
    /// reimplementing token entry and destination search here. One flow, one
    /// place to fix.</summary>
    private async void Notion_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var dlg = new NotionConnectDialog { XamlRoot = Content.XamlRoot };
            await dlg.ShowAsync();
        }
        catch (Exception ex) { App.Log($"MeetingWizard notion exc: {ex.Message}", "Wizard"); }
        RefreshDestinations();
    }

    private void NotionAuto_Changed(object sender, RoutedEventArgs e)
    {
        // Config is written by the core only, never by the host directly.
        try
        {
            var on = NotionAuto.IsChecked == true ? "true" : "false";
            DimmyNative.dimmy_set_config_json($"{{\"notion_auto_send\":{on}}}");
        }
        catch (Exception ex) { App.Log($"MeetingWizard notion-auto exc: {ex.Message}", "Wizard"); }
    }

    private void Folder_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var hwnd = WinRT.Interop.WindowNative.GetWindowHandle(this);
            var picked = Win32FileDialog.PickFolder(hwnd, "Where should recaps be written?");
            if (string.IsNullOrWhiteSpace(picked)) return;
            var prefs = App.Instance?.UiPrefs;
            if (prefs != null) { prefs.RecapExportFolder = picked!; prefs.Save(); }
            RefreshDestinations();
        }
        catch (Exception ex) { App.Log($"MeetingWizard folder exc: {ex.Message}", "Wizard"); }
    }

    // ── Step 3: record a real one ───────────────────────────────────

    /// <summary>Watch the REAL meeting, not a simulation: the same
    /// `meeting_chunk` events the meeting window renders. If the hotkey, the
    /// microphone, the loopback or the STT is broken, nothing appears here and
    /// the user knows now.</summary>
    private void ArmForTrial()
    {
        if (_armed) return;
        var vm = App.Instance?.AppViewModel;
        if (vm == null) return;
        vm.MeetingChunkReceived += OnChunk;
        _armed = true;
        TryStatus.Severity = InfoBarSeverity.Informational;
        TryStatus.Message = $"Waiting for you to press {Recorder.Shortcut}...";
    }

    private void DisarmTrial()
    {
        if (!_armed) return;
        var vm = App.Instance?.AppViewModel;
        if (vm != null) vm.MeetingChunkReceived -= OnChunk;
        _armed = false;
    }

    private void OnChunk(string dir, string speaker, string line, long elapsedMs, int chunkCount)
    {
        DispatcherQueue?.TryEnqueue(() =>
        {
            LiveText.Text += line;
            LiveScroll.Visibility = Visibility.Visible;
            TryStatus.Severity = InfoBarSeverity.Success;
            TryStatus.Message =
                $"It works — {chunkCount} chunk(s) transcribed. Press the shortcut again to stop "
                + "and generate the recap.";
        });
    }

    // ── Teardown ────────────────────────────────────────────────────

    private void Teardown()
    {
        DisarmTrial();
        if (!_committed)
        {
            try { App.Instance?.ReregisterMeetingHotkey(_originalCombo); }
            catch (Exception ex) { App.Log($"MeetingWizard restore exc: {ex.Message}", "Wizard"); }
        }
    }
}
