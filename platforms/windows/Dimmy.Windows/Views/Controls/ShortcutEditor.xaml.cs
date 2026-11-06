using System;
using System.Collections.Generic;
using System.Text.Json;
using System.Threading.Tasks;
using Dimmy.Windows.Interop;
using Dimmy.Windows.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace Dimmy.Windows.Views.Controls;

/// <summary>One shortcut, shown as keys, changed through a dialog that records
/// the new combination as the user presses it. The rules (which combinations
/// are allowed, conflicts with the other shortcuts, keys Windows already uses)
/// live in the core recorder, so every place that edits a shortcut behaves the
/// same: Settings, the setup wizards and onboarding.</summary>
public sealed partial class ShortcutEditor : UserControl
{
    /// <summary>Only one recorder can run, and the hook feeds whichever is open.</summary>
    private static bool _dialogOpen;

    public static readonly DependencyProperty ShortcutProperty =
        DependencyProperty.Register(nameof(Shortcut), typeof(string), typeof(ShortcutEditor),
            new PropertyMetadata("", (d, _) => ((ShortcutEditor)d).UpdateDisplay()));

    /// <summary>The combination in the core grammar (<c>ctrl+shift+d</c>).
    /// Empty = not set.</summary>
    public string Shortcut
    {
        get => (string)GetValue(ShortcutProperty);
        set => SetValue(ShortcutProperty, value);
    }

    /// <summary><c>dictation</c> | <c>command</c> | <c>meeting</c> | <c>dictionary</c>.</summary>
    public string Target { get; set; } = "dictation";

    /// <summary>Shown as "Reset to …" in the dialog when set.</summary>
    public string DefaultShortcut { get; set; } = "";

    /// <summary>The shortcut is optional: the dialog offers to remove it.</summary>
    public bool AllowClear { get; set; }

    /// <summary>Big keys, for a page where the shortcut is the whole point
    /// (onboarding).</summary>
    public bool IsLarge { get; set; }

    /// <summary>The four shortcuts as the caller knows them, keyed by target,
    /// for the conflict check. Settings passes the values it is editing;
    /// everyone else gets what is saved.</summary>
    public Func<IReadOnlyDictionary<string, string>>? CurrentShortcuts { get; set; }

    public event EventHandler<string>? ShortcutChanged;

    public ShortcutEditor()
    {
        InitializeComponent();
        Loaded += (_, _) =>
        {
            if (IsLarge) PreviewKeys.ItemTemplate = (DataTemplate)Resources["LargeKeyTemplate"];
            UpdateDisplay();
        };
    }

    private void UpdateDisplay()
    {
        var keys = KeyLabels(Shortcut);
        PreviewKeys.ItemsSource = keys;
        bool set = keys.Count > 0;
        PreviewKeys.Visibility = set ? Visibility.Visible : Visibility.Collapsed;
        EditIcon.Visibility = set ? Visibility.Visible : Visibility.Collapsed;
        Placeholder.Visibility = set ? Visibility.Collapsed : Visibility.Visible;
        AutomationProperties.SetName(EditButton, set
            ? $"{TitleFor(Target)}: {string.Join(" ", keys)}. Change"
            : $"{TitleFor(Target)}: not set. Set shortcut");
    }

    private async void Edit_Click(object sender, RoutedEventArgs e) => await EditAsync();

    private async Task EditAsync()
    {
        if (_dialogOpen || XamlRoot == null) return;
        _dialogOpen = true;

        var content = new ShortcutDialogContent(
            HintFor(Target),
            string.Join("+", KeyLabels(DefaultShortcut)),
            AllowClear && !string.IsNullOrWhiteSpace(Shortcut));
        content.ShowKeys(KeyLabels(Shortcut));

        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            RequestedTheme = ActualTheme,
            Title = TitleFor(Target),
            Content = content,
            PrimaryButtonText = "Save",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            IsPrimaryButtonEnabled = false,
        };

        string? result = null;
        RecorderVerdict? last = null;
        void OnVerdict(RecorderVerdict v)
        {
            if (v.Target != Target) return;
            last = v;
            content.Show(v);
            dialog.IsPrimaryButtonEnabled = v.CanSave;
        }
        content.ResetRequested += (_, _) => { result = DefaultShortcut; dialog.Hide(); };
        content.ClearRequested += (_, _) => { result = ""; dialog.Hide(); };
        dialog.PrimaryButtonClick += (_, _) => { if (last is { CanSave: true }) result = last.Combo; };

        HotkeyRecorderEvents.Received += OnVerdict;
        try
        {
            // The hook may not exist yet (first run, onboarding). Installing
            // is idempotent in the core.
            DimmyNative.dimmy_hotkey_install();
            int rc = DimmyNative.dimmy_hotkey_recorder_start(Target, CurrentJson());
            if (rc != 0) App.Log($"recorder_start({Target}) rc={rc}", "Hotkey");
            await dialog.ShowAsync();
        }
        catch (Exception ex)
        {
            App.Log($"shortcut dialog exc: {ex.Message}", "Hotkey");
        }
        finally
        {
            DimmyNative.dimmy_hotkey_recorder_stop();
            HotkeyRecorderEvents.Received -= OnVerdict;
            _dialogOpen = false;
        }

        if (result == null || string.Equals(result, Shortcut, StringComparison.OrdinalIgnoreCase)) return;
        Shortcut = result;
        ShortcutChanged?.Invoke(this, result);
    }

    private string CurrentJson()
    {
        var current = CurrentShortcuts?.Invoke() ?? SavedShortcuts();
        return JsonSerializer.Serialize(current);
    }

    private static IReadOnlyDictionary<string, string> SavedShortcuts()
    {
        // From disk, not App.UiPrefs: Settings saves through its own instance,
        // so the App's copy can be behind.
        var prefs = UiPreferences.Load();
        return new Dictionary<string, string>
        {
            ["dictation"] = App.Instance?.AppViewModel?.Shortcut ?? "",
            ["command"] = prefs.CommandHotkey ?? "",
            ["meeting"] = prefs.MeetingHotkey ?? "",
            ["dictionary"] = string.IsNullOrEmpty(prefs.DictHotkey) ? "ctrl+shift+d" : prefs.DictHotkey,
        };
    }

    private static string TitleFor(string target) => target switch
    {
        "command" => "Command shortcut",
        "meeting" => "Meeting shortcut",
        "dictionary" => "Add-to-dictionary shortcut",
        _ => "Dictation shortcut",
    };

    private static string HintFor(string target) => target switch
    {
        "dictionary" => "Press the keys you want to use: a modifier and a key, like Ctrl+Shift+D.",
        _ => "Press the keys you want to use: a modifier and a key, or two modifiers together like Win+Alt.",
    };

    public static List<string> KeyLabels(string? combo) => ShortcutLabels.FromCombo(combo);
}
