using System;
using System.Collections.Generic;
using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.System;
using Windows.UI;

namespace Dimmy.Windows.Views.Controls;

public sealed partial class ShortcutRecorder : UserControl
{
    private bool _isRecording;
    private readonly HashSet<VirtualKey> _pressedKeys = [];

    public static readonly DependencyProperty ShortcutProperty =
        DependencyProperty.Register(nameof(Shortcut), typeof(string),
            typeof(ShortcutRecorder), new PropertyMetadata("Win+Alt", OnShortcutChanged));

    public string Shortcut
    {
        get => (string)GetValue(ShortcutProperty);
        set => SetValue(ShortcutProperty, value);
    }

    /// <summary>When true, only accept combos that bind via Win32
    /// RegisterHotKey — a modifier PLUS a real key. Modifier-only combos
    /// (Ctrl+Shift) and unmappable keys are rejected with a hint. Set this
    /// for the dictionary + command hotkeys (RegisterHotKey-based); the main
    /// dictation hotkey leaves it false because it runs on the Rust hook,
    /// which also accepts two-modifier combos like Win+Alt.</summary>
    public static readonly DependencyProperty RequireKeyProperty =
        DependencyProperty.Register(nameof(RequireKey), typeof(bool),
            typeof(ShortcutRecorder), new PropertyMetadata(false));

    public bool RequireKey
    {
        get => (bool)GetValue(RequireKeyProperty);
        set => SetValue(RequireKeyProperty, value);
    }

    public event EventHandler<string>? ShortcutChanged;

    public ShortcutRecorder()
    {
        this.InitializeComponent();
        UpdateDisplay();
    }

    private static void OnShortcutChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
    {
        ((ShortcutRecorder)d).UpdateDisplay();
    }

    private void UpdateDisplay()
    {
        if (_isRecording)
        {
            DisplayText.Text = "Press your shortcut...";
            RecorderBorder.Background = new SolidColorBrush(Color.FromArgb(255, 234, 179, 8)); // Orange
            DisplayText.FontSize = 16;
        }
        else
        {
            // Empty is a valid state for optional hotkeys (e.g. the command
            // hotkey is opt-in) — show a placeholder rather than a blank box.
            var empty = string.IsNullOrWhiteSpace(Shortcut);
            DisplayText.Text = empty ? "Not set" : Shortcut;
            RecorderBorder.Background = (Brush)Application.Current.Resources["CardBackgroundFillColorDefaultBrush"];
            DisplayText.FontSize = empty ? 16 : 24;
        }
    }

    private void OnPointerPressed(object sender, PointerRoutedEventArgs e)
    {
        _isRecording = true;
        _pressedKeys.Clear();
        UpdateDisplay();
        this.Focus(FocusState.Programmatic);
    }

    private void OnKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (!_isRecording) return;

        _pressedKeys.Add(e.Key);
        e.Handled = true;
    }

    private void OnKeyUp(object sender, KeyRoutedEventArgs e)
    {
        if (!_isRecording) return;

        // Build shortcut string from pressed keys
        var parts = new List<string>();
        if (_pressedKeys.Contains(VirtualKey.LeftWindows) || _pressedKeys.Contains(VirtualKey.RightWindows))
            parts.Add("Win");
        if (_pressedKeys.Contains(VirtualKey.Control) || _pressedKeys.Contains(VirtualKey.LeftControl) || _pressedKeys.Contains(VirtualKey.RightControl))
            parts.Add("Ctrl");
        if (_pressedKeys.Contains(VirtualKey.Menu) || _pressedKeys.Contains(VirtualKey.LeftMenu) || _pressedKeys.Contains(VirtualKey.RightMenu))
            parts.Add("Alt");
        if (_pressedKeys.Contains(VirtualKey.Shift) || _pressedKeys.Contains(VirtualKey.LeftShift) || _pressedKeys.Contains(VirtualKey.RightShift))
            parts.Add("Shift");

        // Add non-modifier keys
        foreach (var key in _pressedKeys)
        {
            if (key is not (VirtualKey.LeftWindows or VirtualKey.RightWindows
                or VirtualKey.Control or VirtualKey.LeftControl or VirtualKey.RightControl
                or VirtualKey.Menu or VirtualKey.LeftMenu or VirtualKey.RightMenu
                or VirtualKey.Shift or VirtualKey.LeftShift or VirtualKey.RightShift))
            {
                parts.Add(key.ToString());
            }
        }

        var candidate = string.Join("+", parts);

        bool valid;
        if (RequireKey)
        {
            // RegisterHotKey-based hotkeys (dict + command): the combo must
            // parse to a modifier + a real, mappable key. This rejects the
            // two-modifier combos the OS hook would accept (Ctrl+Shift) and
            // any key whose name doesn't map to a VK (shows up as a number).
            valid = parts.Count > 0
                    && Services.DictHotkeyParser.TryParse(candidate, out _, out _);
        }
        else
        {
            int modCount = parts.Count(p => p is "Win" or "Ctrl" or "Alt" or "Shift");
            int nonModCount = parts.Count - modCount;
            bool hasFKey = _pressedKeys.Any(k => k is >= VirtualKey.F1 and <= VirtualKey.F24);
            valid = parts.Count > 0
                    && ((modCount >= 1 && nonModCount >= 1) || modCount >= 2 || hasFKey);
        }

        _isRecording = false;
        _pressedKeys.Clear();

        if (valid)
        {
            Shortcut = candidate;
            ShortcutChanged?.Invoke(this, Shortcut);
            UpdateDisplay();
        }
        else
        {
            // Keep the previous Shortcut; show a one-line hint instead of the
            // junk combo so the user knows to add a real key.
            DisplayText.Text = "Use a modifier + a key";
            DisplayText.FontSize = 16;
            RecorderBorder.Background =
                (Brush)Application.Current.Resources["CardBackgroundFillColorDefaultBrush"];
        }
        e.Handled = true;
    }
}
