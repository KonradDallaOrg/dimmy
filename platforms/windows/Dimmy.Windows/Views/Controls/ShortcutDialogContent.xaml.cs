using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using Dimmy.Windows.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Dimmy.Windows.Views.Controls;

/// <summary>Body of the "set shortcut" dialog: the keys as the user presses
/// them, the reason a combination can't be used, and the warnings. Owned by
/// <see cref="ShortcutEditor"/>, which opens the recorder and feeds this.</summary>
public sealed partial class ShortcutDialogContent : UserControl
{
    private readonly ObservableCollection<string> _keys = [];
    private bool _showingError;

    public event EventHandler? ResetRequested;
    public event EventHandler? ClearRequested;

    public ShortcutDialogContent(string hint, string defaultLabel, bool canClear)
    {
        InitializeComponent();
        KeysControl.ItemsSource = _keys;
        HintText.Text = hint;
        ResetLink.Visibility = string.IsNullOrEmpty(defaultLabel) ? Visibility.Collapsed : Visibility.Visible;
        ResetText.Text = $"Reset to {defaultLabel}";
        ClearLink.Visibility = canClear ? Visibility.Visible : Visibility.Collapsed;
        UpdateEmpty();
    }

    /// <summary>The shortcut configured now, before any key is pressed.</summary>
    public void ShowKeys(IReadOnlyList<string> keys)
    {
        Sync(keys);
        SetError("");
    }

    public void Show(RecorderVerdict v)
    {
        Sync(v.Keys);
        SetError(v.Error);
        WarningsPanel.Children.Clear();
        foreach (var w in v.Warnings)
        {
            WarningsPanel.Children.Add(new InfoBar
            {
                Severity = InfoBarSeverity.Warning,
                IsClosable = false,
                IsOpen = true,
                Message = w,
            });
        }
    }

    /// <summary>Edit the collection in place rather than replacing it, so only
    /// the keys that just went down play the entrance animation.</summary>
    private void Sync(IReadOnlyList<string> keys)
    {
        for (int i = _keys.Count - 1; i >= 0; i--)
        {
            if (!Contains(keys, _keys[i])) _keys.RemoveAt(i);
        }
        for (int i = 0; i < keys.Count; i++)
        {
            if (i < _keys.Count && _keys[i] == keys[i]) continue;
            int at = _keys.IndexOf(keys[i]);
            if (at >= 0) _keys.Move(at, i);
            else _keys.Insert(i, keys[i]);
        }
        while (_keys.Count > keys.Count) _keys.RemoveAt(_keys.Count - 1);
        UpdateEmpty();
    }

    private static bool Contains(IReadOnlyList<string> list, string s)
    {
        foreach (var x in list) if (x == s) return true;
        return false;
    }

    private void SetError(string error)
    {
        bool isError = !string.IsNullOrEmpty(error);
        ErrorBar.Message = error;
        ErrorBar.IsOpen = isError;
        if (isError == _showingError) return;
        _showingError = isError;
        KeysControl.ItemTemplate = (DataTemplate)Resources[isError ? "ErrorKeyTemplate" : "KeyTemplate"];
    }

    private void UpdateEmpty() =>
        EmptyText.Visibility = _keys.Count == 0 ? Visibility.Visible : Visibility.Collapsed;

    private void Reset_Click(object sender, RoutedEventArgs e) => ResetRequested?.Invoke(this, EventArgs.Empty);

    private void Clear_Click(object sender, RoutedEventArgs e) => ClearRequested?.Invoke(this, EventArgs.Empty);
}
