using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Views;

/// <summary>Three focused steps to get Command mode working: bind a
/// shortcut, confirm the model answers, then use it for real.
///
/// <para>Deliberately NOT built on OnboardingWindow. That wizard's step
/// count is pinned by an assertion on the Mac side
/// (<c>SelfTests.testOnboardingStepCount</c>) which <c>fatalError</c>s the
/// app at launch, and generalising it would make that constant stop meaning
/// what it says. A separate window costs a file and removes the hazard.</para>
///
/// <para>The last step is the point of the whole thing. It does not mock
/// anything: the real Rust hook fires, the real UIA selection reader picks up
/// the sample text, the real microphone records, the real STT transcribes and
/// the real LLM transforms. If any link is broken the user finds out here, in
/// ten seconds, instead of the first time they need it.</para></summary>
public sealed partial class CommandWizardWindow : Window
{
    private int _step;
    private const int TotalSteps = 3;

    /// What the wizard offers as the thing to transform. Deliberately shaped
    /// like something dictated and never tidied up: run-on, no punctuation to
    /// speak of, three subjects in one breath. "Hello world" proves the wiring
    /// and teaches nothing about what the feature is for.
    ///
    /// In English, like every other word in this window. It used to be Italian
    /// while the interface around it was not, which left the reader deciding
    /// which language the thing they were about to SAY should be in.
    private const string SampleText =
        "so for the NFC project we still need to decide who runs the demo, "
        + "then there's the cost question nobody has closed yet, and Jasmine "
        + "wanted to know whether the QR code stays or we drop it entirely";

    /// Phrased as spoken instructions, not as button labels: "summarise it in
    /// three points" is something a person says, "Summarise (3 points)" is
    /// something they look for and fail to find.
    private static readonly string[] Suggestions =
    {
        "\"turn this into a bullet list\"",
        "\"summarise it in three points\"",
        "\"rewrite it so I can send it to a client\"",
        "\"translate it into Italian\"",
    };

    /// The command shortcut in force before the wizard touched the hook, so
    /// abandoning the wizard cannot leave the user with a binding they never
    /// confirmed.
    private readonly string _originalCombo;
    private bool _committed;
    private bool _armed;
    private bool _busy;

    public CommandWizardWindow()
    {
        InitializeComponent();
        // Taller than the other two wizards: step 3 stacks a hint, the sample
        // box, the speak-don't-type note, a status bar and the result.
        WindowHelper.ResizeLogical(this, 660, 620);

        // Follow the theme the user picked. Without this a wizard renders in
        // the SYSTEM theme while the rest of the app honours the setting, so a
        // user on light with a dark Windows gets a dark window out of nowhere.
        if (Content is FrameworkElement themeRoot)
            themeRoot.RequestedTheme = ThemeHelper.ResolvedElementTheme();

        _originalCombo = App.Instance?.UiPrefs?.CommandHotkey ?? "";
        Recorder.Shortcut = string.IsNullOrWhiteSpace(_originalCombo)
            ? "Ctrl+Alt+C"
            : _originalCombo;
        Recorder.ShortcutChanged += (_, __) => ValidateShortcut();

        SampleBox.Text = SampleText;
        SuggestionLabel.Text =
            $"For example: {Suggestions[new Random().Next(Suggestions.Length)]}";

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
        BackBtn.Visibility = _step > 0 ? Visibility.Visible : Visibility.Collapsed;
        SkipBtn.Visibility = _step == 1 ? Visibility.Visible : Visibility.Collapsed;
        StepLabel.Text = $"Step {_step + 1} of {TotalSteps}";

        switch (_step)
        {
            case 0:
                TitleLabel.Text = "Pick a shortcut";
                SubtitleLabel.Text =
                    "Select some text anywhere, hold this shortcut, and say what to do with it.";
                NextBtn.Content = "Continue";
                break;
            case 1:
                TitleLabel.Text = "Check the model";
                SubtitleLabel.Text =
                    "Command mode uses your dictation LLM, not the recap one. "
                    + "One tiny call proves the key and the model both work.";
                NextBtn.Content = "Continue";
                ShowModelSummary();
                break;
            case 2:
                TitleLabel.Text = "Try it";
                SubtitleLabel.Text = "";
                NextBtn.Content = "Done";
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
        if (_step == 2) DisarmTrial();
        if (_step == 0) return;
        _step--;
        RenderStep();
    }

    private void Skip_Click(object sender, RoutedEventArgs e)
    {
        _step = 2;
        RenderStep();
    }

    // ── Step 0: the shortcut ────────────────────────────────────────

    /// <summary>True when the combo is bindable AND does not collide with the
    /// dictation hotkey. Without the collision check the user ends up with two
    /// bindings on the same keys and no idea why one of them stopped firing —
    /// the core already ships `dimmy_hotkey_combos_conflict` for exactly
    /// this.</summary>
    private bool ValidateShortcut()
    {
        var combo = Recorder.Shortcut ?? "";
        if (string.IsNullOrWhiteSpace(combo))
        {
            Warn("Press a combination that includes a real key, like Ctrl+Alt+C.");
            return false;
        }

        var dictation = App.Instance?.AppViewModel?.Shortcut ?? "";
        if (!string.IsNullOrWhiteSpace(dictation) && Conflicts(combo, dictation))
        {
            Warn($"That is already your dictation shortcut ({dictation}). Pick another.");
            return false;
        }

        var meeting = App.Instance?.UiPrefs?.MeetingHotkey ?? "";
        if (!string.IsNullOrWhiteSpace(meeting) && Conflicts(combo, meeting))
        {
            Warn($"That is already your meeting shortcut ({meeting}). Pick another.");
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
            if (prefs != null)
            {
                prefs.CommandHotkey = combo;
                prefs.Save();
            }
            App.Instance?.ReregisterCommandHotkey(combo);
            _committed = true;
            App.Log($"CommandWizard: bound {combo}", "Wizard");
        }
        catch (Exception ex) { App.Log($"CommandWizard commit exc: {ex.Message}", "Wizard"); }
    }

    // ── Step 1: the model ───────────────────────────────────────────

    private void ShowModelSummary()
    {
        // Read the config the core owns rather than a view model that may
        // not have been opened yet: Settings is the only thing that fills
        // those properties, and this wizard can run before it ever opens.
        var (mode, model) = ReadLlmConfig();
        ModelSummary.Text = string.IsNullOrWhiteSpace(model)
            ? "No LLM model is configured yet. Set one in Settings, then come back."
            : $"Using {(mode == "local" ? "the local model" : "the cloud model")} {model}.";
        TestModelBtn.IsEnabled = !string.IsNullOrWhiteSpace(model);
    }

    /// <summary>(mode, model) for the DICTATION LLM — the one command mode
    /// dispatches through. Not `recap_model_override`: that is a separate
    /// setting and using it here would test the wrong thing.</summary>
    private static (string mode, string model) ReadLlmConfig()
    {
        try
        {
            var json = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_config_json, 65536);
            if (string.IsNullOrEmpty(json)) return ("cloud", "");
            using var doc = System.Text.Json.JsonDocument.Parse(json);
            var root = doc.RootElement;
            var mode = root.TryGetProperty("llm_mode", out var m)
                ? (m.GetString() ?? "cloud") : "cloud";
            var key = mode == "local" ? "local_llm_model" : "llm_api_model";
            var model = root.TryGetProperty(key, out var v) ? (v.GetString() ?? "") : "";
            return (mode, model);
        }
        catch { return ("cloud", ""); }
    }

    /// <summary>One real round trip through the configured LLM. Uses
    /// `dimmy_llm_call_raw` — the same entry point the recap uses — rather
    /// than a mock, because the failures worth catching here are a missing
    /// key, a model that is not on disk, and a provider that refuses the
    /// model id.</summary>
    private async void TestModel_Click(object sender, RoutedEventArgs e)
    {
        if (_busy) return;
        _busy = true;
        TestModelBtn.IsEnabled = false;
        TestRing.IsActive = true;
        TestResult.IsOpen = false;
        try
        {
            var answer = await Task.Run(() =>
                DimmyNative.ReadBuffer((b, n) => DimmyNative.dimmy_llm_call_raw(
                    "Reply with exactly: ok", "", 16, b, n), 4096));
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

    /// <summary>Open Settings ON the page that owns this setting, and pull
    /// it to the front. Plain `OpenSettingsWindow()` looked broken when the
    /// wizard had been launched FROM Settings: the window was already open,
    /// so Activate() only raised it behind this one and nothing seemed to
    /// happen. `OpenSettingsWindowAt` navigates and foregrounds.</summary>
    private void OpenLlmSettings_Click(object sender, RoutedEventArgs e)
    {
        try { App.Instance?.OpenSettingsWindowAt("output"); }
        catch (Exception ex) { App.Log($"CommandWizard settings exc: {ex.Message}", "Wizard"); }
    }

    // ── Step 2: the real thing ──────────────────────────────────────

    /// <summary>Select the sample text and wait for the real hotkey.
    ///
    /// <para>The selection is real, not injected: the command path reads the
    /// focused element's selection through UIA, and this window IS the focused
    /// app, so selecting the box's contents makes the normal flow pick it
    /// up. Every link in the chain is the shipping one.</para></summary>
    private void ArmForTrial()
    {
        SampleBox.Focus(FocusState.Programmatic);
        SampleBox.SelectAll();

        // Name the actual shortcut rather than "your shortcut": this is the
        // step where the user has to perform it, and it was chosen two screens
        // ago.
        TryHint.Text =
            $"The text below is selected. Hold {Recorder.Shortcut}, say what you "
            + "want done with it, then let go.";

        if (_armed) return;
        var hk = App.Instance?.HotkeyServiceInstance;
        if (hk == null)
        {
            TryStatus.Severity = InfoBarSeverity.Warning;
            TryStatus.Message = "The hotkey service is not running, so this step cannot listen.";
            return;
        }
        hk.CommandHotkeyPressed += OnPressed;
        var vm = App.Instance?.AppViewModel;
        if (vm != null) vm.TranscriptReady += OnResult;
        _armed = true;
        TryStatus.Severity = InfoBarSeverity.Informational;
        TryStatus.Message = $"Hold {Recorder.Shortcut} and say what to do with the text.";
    }

    private void DisarmTrial()
    {
        if (!_armed) return;
        var hk = App.Instance?.HotkeyServiceInstance;
        if (hk != null) hk.CommandHotkeyPressed -= OnPressed;
        var vm = App.Instance?.AppViewModel;
        if (vm != null) vm.TranscriptReady -= OnResult;
        _armed = false;
    }

    private void OnPressed()
    {
        DispatcherQueue?.TryEnqueue(() =>
        {
            TryStatus.Severity = InfoBarSeverity.Informational;
            TryStatus.Message = "Listening... let go when you are done.";
        });
    }

    /// The transformed text arrives the same way it does in normal use — the
    /// app pastes it and records it — so watch the view model rather than
    /// re-running the transform ourselves and measuring something else.
    private void OnResult(string text)
    {
        if (string.IsNullOrWhiteSpace(text)) return;
        DispatcherQueue?.TryEnqueue(() =>
        {
            ResultText.Text = text;
            ResultScroll.Visibility = Visibility.Visible;
            TryStatus.Severity = InfoBarSeverity.Success;
            TryStatus.Message = "That is command mode. You can use it anywhere you can select text.";
        });
    }

    // ── Teardown ────────────────────────────────────────────────────

    /// <summary>Leave nothing half-applied. A wizard closed on step 0 must not
    /// have rebound the user's keyboard, and a trial left armed would keep
    /// appending later command results into a window that is gone.</summary>
    private void Teardown()
    {
        DisarmTrial();
        if (!_committed)
        {
            try { App.Instance?.ReregisterCommandHotkey(_originalCombo); }
            catch (Exception ex) { App.Log($"CommandWizard restore exc: {ex.Message}", "Wizard"); }
        }
    }
}
