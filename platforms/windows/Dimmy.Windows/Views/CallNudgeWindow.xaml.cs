using System;
using System.Collections.Generic;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Graphics;

using Dimmy.Windows.Helpers;

namespace Dimmy.Windows.Views;

/// Auto-detect nudge: a small card in the bottom-right that asks the
/// user if they want to record a meeting Dimmy just noticed via mic
/// activity. Same non-activating-overlay recipe as PillWindow /
/// CaptionWindow — must NOT steal focus from the user's target app
/// because the user may want to keep typing into Teams chat while the
/// card is up.
///
/// Lifecycle: created lazily on first `call_detected` event,
/// auto-dismisses after 30 s (treated as "Timeout" so the short
/// cooldown applies and the user gets another chance later).
public sealed partial class CallNudgeWindow : Window
{
    private const int CardWidthDip = 360;
    private const int CardHeightDip = 132;
    private const int BottomMarginPx = 80;
    private const int RightMarginPx = 20;
    private static readonly TimeSpan AutoDismiss = TimeSpan.FromSeconds(30);

    private readonly DispatcherQueueTimer _dismissTimer;

    /// Inferred app id ("teams" / "zoom" / null). Sent back in the
    /// response FFI call so the cooldown / exclusion key matches.
    private string? _currentAppId;
    private string _currentAppName = "Meeting";

    /// Display-name map for the 5 whitelist apps. Falls back to the
    /// raw id if missing (defensive — the Rust side may emit new ids
    /// faster than the C# table is updated).
    private static readonly Dictionary<string, string> AppDisplayNames =
        new(StringComparer.OrdinalIgnoreCase)
        {
            ["teams"] = "Microsoft Teams",
            ["zoom"] = "Zoom",
            ["slack"] = "Slack",
            ["discord"] = "Discord",
            ["webex"] = "Cisco Webex",
        };

    public event Action<string?>? RecordRequested;
    public event Action<string?>? NotNowRequested;
    public event Action<string?>? NeverRequested;
    public event Action<string?>? TimedOut;

    public CallNudgeWindow()
    {
        this.InitializeComponent();
        Title = "Dimmy Call Detected";

        ExtendsContentIntoTitleBar = true;
        var backdrop = new TransparentBackdrop
        {
            Hwnd = WindowHelper.GetHwnd(this),
        };
        this.SystemBackdrop = backdrop;

        var appWindow = WindowHelper.GetAppWindow(this);
        if (appWindow?.Presenter is OverlappedPresenter presenter)
        {
            presenter.SetBorderAndTitleBar(false, false);
            presenter.IsResizable = false;
            presenter.IsMaximizable = false;
            presenter.IsMinimizable = false;
            presenter.IsAlwaysOnTop = true;
        }
        if (appWindow != null)
        {
            try { appWindow.IsShownInSwitchers = false; } catch { }
        }
        WindowHelper.EnableTransparency(this);

        if (Content is FrameworkElement root)
        {
            root.RequestedTheme = ElementTheme.Dark;
        }

        _dismissTimer = DispatcherQueue.CreateTimer();
        _dismissTimer.Interval = AutoDismiss;
        _dismissTimer.Tick += OnDismissTick;

        Hide();
    }

    /// Render the card for a fresh detection. `appId` may be null —
    /// the card adapts to "Microphone in use — record a meeting?".
    public void ShowFor(string? appId)
    {
        _currentAppId = appId;
        _currentAppName = ResolveDisplayName(appId);

        if (appId == null)
        {
            TitleText.Text = "Microphone in use";
            BodyText.Text = "Looks like a call. Record + recap with Dimmy?";
            DontAskMenuItem.Visibility = Visibility.Collapsed;
            HeaderIcon.Glyph = ""; // generic mic
        }
        else
        {
            TitleText.Text = $"Meeting detected in {_currentAppName}";
            BodyText.Text = "Dimmy can record + recap this call.";
            DontAskMenuItem.Text = $"Don't ask for {_currentAppName} again";
            DontAskMenuItem.Visibility = Visibility.Visible;
            HeaderIcon.Glyph = ""; // phone / call
        }

        PositionAtScreenBottomRight();
        WindowHelper.ShowWithoutActivating(this);

        // Reset the dismiss timer on each new detection so a quick
        // hide/re-show doesn't shrink the visible window.
        _dismissTimer.Stop();
        _dismissTimer.Start();
    }

    public void Hide()
    {
        _dismissTimer.Stop();
        var appWindow = WindowHelper.GetAppWindow(this);
        appWindow?.Hide();
    }

    private static string ResolveDisplayName(string? appId)
    {
        if (string.IsNullOrEmpty(appId)) return "Meeting";
        return AppDisplayNames.TryGetValue(appId, out var name) ? name : appId;
    }

    private void PositionAtScreenBottomRight()
    {
        var appWindow = WindowHelper.GetAppWindow(this);
        if (appWindow == null) return;
        var displayArea = DisplayArea.GetFromWindowId(appWindow.Id, DisplayAreaFallback.Primary);
        if (displayArea == null) return;
        var work = displayArea.WorkArea;
        var hwnd = WindowHelper.GetHwnd(this);
        var dpi = GetDpiForWindow(hwnd);
        if (dpi == 0) dpi = 96;
        var scale = dpi / 96.0;
        int width = (int)Math.Round(CardWidthDip * scale);
        int height = (int)Math.Round(CardHeightDip * scale);
        appWindow.Resize(new SizeInt32(width, height));
        int x = work.X + work.Width - width - RightMarginPx;
        int y = work.Y + work.Height - height - BottomMarginPx;
        appWindow.Move(new PointInt32(x, y));
    }

    private void OnRecordClicked(object sender, RoutedEventArgs e)
    {
        _dismissTimer.Stop();
        var app = _currentAppId;
        Hide();
        RecordRequested?.Invoke(app);
    }

    private void OnNotNowClicked(object sender, RoutedEventArgs e)
    {
        _dismissTimer.Stop();
        var app = _currentAppId;
        Hide();
        NotNowRequested?.Invoke(app);
    }

    private void OnNeverClicked(object sender, RoutedEventArgs e)
    {
        _dismissTimer.Stop();
        var app = _currentAppId;
        Hide();
        NeverRequested?.Invoke(app);
    }

    private void OnCloseClicked(object sender, RoutedEventArgs e)
    {
        // X without picking a menu item behaves like Not now:
        // the user explicitly dismissed it, but didn't ask to never
        // see it again. Use the full cooldown so we don't re-pester.
        _dismissTimer.Stop();
        var app = _currentAppId;
        Hide();
        NotNowRequested?.Invoke(app);
    }

    private void OnDismissTick(DispatcherQueueTimer sender, object args)
    {
        _dismissTimer.Stop();
        var app = _currentAppId;
        Hide();
        TimedOut?.Invoke(app);
    }

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hwnd);
}
