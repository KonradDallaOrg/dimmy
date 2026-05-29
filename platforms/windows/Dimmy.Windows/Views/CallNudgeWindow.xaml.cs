using System;
using System.Collections.Generic;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
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
    // Teams-style toast geometry — narrower + shorter than the previous
    // 380x148 layout. Padding/spacing were tightened in CallNudgeWindow.xaml
    // in the same pass so the three-row grid fits at 360x112.
    private const int CardWidthDip = 360;
    private const int CardHeightDip = 96;
    private const int BottomMarginPx = 60;
    private const int RightMarginPx = 16;
    private static readonly TimeSpan AutoDismiss = TimeSpan.FromSeconds(30);

    /// Display mode of the popup. Same window is reused for both
    /// "meeting detected — start recording?" and "no activity — stop
    /// and recap?" prompts to avoid juggling two near-identical
    /// windows + their show/hide ordering.
    private enum NudgeMode
    {
        Detected,
        StopSuggested,
    }

    private NudgeMode _mode = NudgeMode.Detected;

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

    // Stop-suggestion mode (call ended while we were recording).
    public event Action<string?>? StopAndRecapRequested;
    public event Action<string?>? KeepRecordingRequested;
    public event Action<string?>? StopTimedOut;

    public CallNudgeWindow()
    {
        this.InitializeComponent();
        Title = "Dimmy Call Detected";

        ExtendsContentIntoTitleBar = true;
        // Same transparency recipe as PillWindow: TransparentBackdrop +
        // EnableTransparency strips the Win11 default chrome (rounded
        // corners, 1 px border, shadow). The inner XAML `Border` is
        // then the ONLY visible boundary, so the popup looks like a
        // floating card without the "double edges" the user reported.
        var backdrop = new Helpers.TransparentBackdrop
        {
            Hwnd = WindowHelper.GetHwnd(this),
        };
        this.SystemBackdrop = backdrop;
        ApplyTheme(ThemeHelper.ResolvedElementTheme());

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

        _dismissTimer = DispatcherQueue.CreateTimer();
        _dismissTimer.Interval = AutoDismiss;
        _dismissTimer.Tick += OnDismissTick;

        Hide();
    }

    /// Render the card for a fresh detection. `appId` may be null —
    /// the card adapts to "Microphone in use — record a meeting?".
    public void ShowFor(string? appId)
    {
        _mode = NudgeMode.Detected;
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
        NotNowButton.Content = "Not now";
        RecordButton.Content = "Record now";

        PositionAtScreenBottomRight();
        WindowHelper.ShowWithoutActivating(this);

        // Reset the dismiss timer on each new detection so a quick
        // hide/re-show doesn't shrink the visible window.
        _dismissTimer.Stop();
        _dismissTimer.Start();
    }

    /// Render the card for a stop-suggestion (call appears to have
    /// ended while we were recording). Conditions enforced Rust-side
    /// in `call_detector::handle_inactive` — this method just paints.
    public void ShowStopSuggestion(string? appId)
    {
        _mode = NudgeMode.StopSuggested;
        _currentAppId = appId;
        _currentAppName = ResolveDisplayName(appId);

        TitleText.Text = string.IsNullOrEmpty(appId)
            ? "Call ended?"
            : $"{_currentAppName} call ended?";
        // Honour the recap choice from meeting start: if recap was turned
        // off, the CTA must read (and do) "Stop" — not promise a recap it
        // won't run.
        bool wantRecap = App.Instance?.AppViewModel?.MeetingGenerateRecap ?? true;
        BodyText.Text = wantRecap
            ? "No activity for a while. Stop & recap?"
            : "No activity for a while. Stop recording?";
        HeaderIcon.Glyph = "";
        DontAskMenuItem.Visibility = Visibility.Collapsed;
        NotNowButton.Content = "Keep recording";
        RecordButton.Content = wantRecap ? "Stop & recap" : "Stop";

        PositionAtScreenBottomRight();
        WindowHelper.ShowWithoutActivating(this);

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
        var mode = _mode;
        Hide();
        if (mode == NudgeMode.StopSuggested)
        {
            StopAndRecapRequested?.Invoke(app);
        }
        else
        {
            RecordRequested?.Invoke(app);
        }
    }

    private void OnNotNowClicked(object sender, RoutedEventArgs e)
    {
        _dismissTimer.Stop();
        var app = _currentAppId;
        var mode = _mode;
        Hide();
        if (mode == NudgeMode.StopSuggested)
        {
            KeepRecordingRequested?.Invoke(app);
        }
        else
        {
            NotNowRequested?.Invoke(app);
        }
    }

    private void OnNeverClicked(object sender, RoutedEventArgs e)
    {
        // "Don't ask again" is meaningless in stop-suggestion mode —
        // the menu item is collapsed there, so this can only fire in
        // Detected mode. Fall back to the cooldown response just in
        // case (idempotent on the Rust side).
        _dismissTimer.Stop();
        var app = _currentAppId;
        var mode = _mode;
        Hide();
        if (mode == NudgeMode.StopSuggested)
        {
            KeepRecordingRequested?.Invoke(app);
        }
        else
        {
            NeverRequested?.Invoke(app);
        }
    }

    private void OnCloseClicked(object sender, RoutedEventArgs e)
    {
        // X without picking a menu item: treat as "keep doing what
        // you were doing" — Not now in detect mode, Keep recording
        // in stop mode. Cooldown applies in both cases.
        _dismissTimer.Stop();
        var app = _currentAppId;
        var mode = _mode;
        Hide();
        if (mode == NudgeMode.StopSuggested)
        {
            KeepRecordingRequested?.Invoke(app);
        }
        else
        {
            NotNowRequested?.Invoke(app);
        }
    }

    private void OnDismissTick(DispatcherQueueTimer sender, object args)
    {
        _dismissTimer.Stop();
        var app = _currentAppId;
        var mode = _mode;
        Hide();
        if (mode == NudgeMode.StopSuggested)
        {
            StopTimedOut?.Invoke(app);
        }
        else
        {
            TimedOut?.Invoke(app);
        }
    }

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hwnd);

    private void ApplyTheme(ElementTheme theme)
    {
        bool dark = theme == ElementTheme.Dark;
        // Match Win11 native notification palette (same tones the pill
        // uses for its MenuFlyoutPresenter): near-black panel + soft
        // white text on dark, near-white panel + near-black text on
        // light. Border is a subtle 1px stroke that picks the readable
        // colour for each theme.
        var bg = new Microsoft.UI.Xaml.Media.SolidColorBrush(dark
            ? global::Windows.UI.Color.FromArgb(0xFF, 0x2B, 0x2B, 0x2B)
            : global::Windows.UI.Color.FromArgb(0xFF, 0xF9, 0xF9, 0xF9));
        var fg = new Microsoft.UI.Xaml.Media.SolidColorBrush(dark
            ? global::Windows.UI.Color.FromArgb(0xFF, 0xF2, 0xF2, 0xF2)
            : global::Windows.UI.Color.FromArgb(0xFF, 0x1A, 0x1A, 0x1A));
        var stroke = new Microsoft.UI.Xaml.Media.SolidColorBrush(dark
            ? global::Windows.UI.Color.FromArgb(0x40, 0xFF, 0xFF, 0xFF)
            : global::Windows.UI.Color.FromArgb(0x20, 0x00, 0x00, 0x00));
        RootBorder.Background = bg;
        RootBorder.BorderBrush = stroke;
        // Propagate theme to ThemeResource consumers (AccentButtonStyle).
        if (Content is FrameworkElement root) root.RequestedTheme = theme;
        // Explicit Foreground on the text/icon nodes so they don't fall
        // back to the system text colour bleeding through.
        TitleText.Foreground = fg;
        BodyText.Foreground = fg;
        HeaderIcon.Foreground = fg;
        CloseButton.Foreground = fg;
        NotNowButton.Foreground = fg;
    }
}
