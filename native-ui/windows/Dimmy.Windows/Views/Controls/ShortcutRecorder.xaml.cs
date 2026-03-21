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
            DisplayText.Text = Shortcut;
            RecorderBorder.Background = (Brush)Application.Current.Resources["CardBackgroundFillColorDefaultBrush"];
            DisplayText.FontSize = 24;
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

        // Validate: need at least 2 modifiers OR 1 special key
        bool valid = parts.Count(p => p is "Win" or "Ctrl" or "Alt" or "Shift") >= 2
                  || _pressedKeys.Any(k => k is >= VirtualKey.F1 and <= VirtualKey.F24);

        if (valid && parts.Count > 0)
        {
            Shortcut = string.Join("+", parts);
            ShortcutChanged?.Invoke(this, Shortcut);
        }

        _isRecording = false;
        _pressedKeys.Clear();
        UpdateDisplay();
        e.Handled = true;
    }
}
