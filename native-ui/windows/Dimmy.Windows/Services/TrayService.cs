using System;
using CommunityToolkit.Mvvm.Input;
using H.NotifyIcon;
using Microsoft.UI.Xaml;

namespace Dimmy.Windows.Services;

public class TrayService : IDisposable
{
    private TaskbarIcon? _trayIcon;
    private readonly Action _onSettingsClick;
    private readonly Action _onQuitClick;
    private readonly Action _onTogglePill;

    public TrayService(Action onTogglePill, Action onSettingsClick, Action onQuitClick)
    {
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

        // Right-click context menu
        // H.NotifyIcon supports MenuFlyout — attach in XAML or build here
        // TODO: build context menu with Status, Language, Style, Mode, Shortcut, Settings, Quit items
    }

    public void UpdateState(string tooltip, string iconPath)
    {
        if (_trayIcon == null) return;
        _trayIcon.ToolTipText = tooltip;
        // TODO: update icon
    }

    public void Dispose()
    {
        _trayIcon?.Dispose();
        GC.SuppressFinalize(this);
    }
}
