using System;
using CommunityToolkit.Mvvm.Input;
using H.NotifyIcon;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Dimmy.Windows.ViewModels;

namespace Dimmy.Windows.Services;

public class TrayService : IDisposable
{
    private TaskbarIcon? _trayIcon;
    private readonly AppViewModel _vm;
    private readonly Action _onSettingsClick;
    private readonly Action _onQuitClick;
    private readonly Action _onTogglePill;

    public TrayService(AppViewModel vm, Action onTogglePill, Action onSettingsClick, Action onQuitClick)
    {
        _vm = vm;
        _onTogglePill = onTogglePill;
        _onSettingsClick = onSettingsClick;
        _onQuitClick = onQuitClick;
    }

    public void Initialize(XamlRoot xamlRoot)
    {
        _trayIcon = new TaskbarIcon();
        _trayIcon.ToolTipText = "Dimmy — Ready";

        // TODO: Set icon from Assets/dimmy.ico
        // _trayIcon.IconSource = new BitmapImage(new Uri("ms-appx:///Assets/dimmy.ico"));

        // Left click: toggle pill
        _trayIcon.LeftClickCommand = new RelayCommand(() => _onTogglePill());

        // Build the right-click context menu
        _trayIcon.ContextFlyout = BuildContextMenu();
    }

    private MenuFlyout BuildContextMenu()
    {
        var flyout = new MenuFlyout();

        // Status line
        var statusText = _vm.IsRecording ? "● Recording..." : "● Ready";
        flyout.Items.Add(new MenuFlyoutItem
        {
            Text = statusText,
            IsEnabled = false,
        });

        flyout.Items.Add(new MenuFlyoutSeparator());

        // Language
        var langDisplay = string.IsNullOrEmpty(_vm.Language) ? "(auto)" : _vm.Language;
        flyout.Items.Add(new MenuFlyoutItem
        {
            Text = $"Language: {langDisplay}",
            IsEnabled = false,
        });

        // Style
        var styleDisplay = string.IsNullOrEmpty(_vm.LlmStyle) || _vm.LlmStyle == "off"
            ? "off"
            : _vm.LlmStyle;
        flyout.Items.Add(new MenuFlyoutItem
        {
            Text = $"Style: {styleDisplay}",
            IsEnabled = false,
        });

        // Mode
        var modeDisplay = string.IsNullOrEmpty(_vm.ShortcutMode) ? "toggle" : _vm.ShortcutMode;
        flyout.Items.Add(new MenuFlyoutItem
        {
            Text = $"Mode: {modeDisplay}",
            IsEnabled = false,
        });

        // Shortcut
        var shortcutDisplay = string.IsNullOrEmpty(_vm.Shortcut) ? "Win+Alt" : _vm.Shortcut;
        flyout.Items.Add(new MenuFlyoutItem
        {
            Text = $"Shortcut: {shortcutDisplay}",
            IsEnabled = false,
        });

        flyout.Items.Add(new MenuFlyoutSeparator());

        // Settings
        var settingsItem = new MenuFlyoutItem { Text = "Settings..." };
        settingsItem.Click += (_, _) => _onSettingsClick();
        flyout.Items.Add(settingsItem);

        // Quit
        var quitItem = new MenuFlyoutItem { Text = "Quit Dimmy" };
        quitItem.Click += (_, _) => _onQuitClick();
        flyout.Items.Add(quitItem);

        return flyout;
    }

    public void UpdateState(string tooltip, string iconPath)
    {
        if (_trayIcon == null) return;
        _trayIcon.ToolTipText = tooltip;
        // Rebuild context menu so status line reflects current recording state
        _trayIcon.ContextFlyout = BuildContextMenu();
        // TODO: update icon
    }

    public void Dispose()
    {
        _trayIcon?.Dispose();
        GC.SuppressFinalize(this);
    }
}
