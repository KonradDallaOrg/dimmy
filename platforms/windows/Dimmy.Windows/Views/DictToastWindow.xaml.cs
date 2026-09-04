using System;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Dimmy.Windows.Helpers;

namespace Dimmy.Windows.Views;

/// <summary>
/// Self-dismissing in-app toast for dictionary feedback. Positioned in
/// the bottom-right of the primary screen (the same anchor area Win11
/// toasts use) so visual muscle memory carries over. Auto-closes after
/// long enough to read it (<see cref="Services.ToastDuration"/>); can be
/// dismissed earlier by click.
///
/// Why this instead of <c>Microsoft.Windows.AppNotifications</c>: the
/// modern AppNotification API requires the app to be MSIX-packaged
/// OR pre-register a ToastActivator CLSID in HKCU\Software\Classes —
/// Dimmy ships unpackaged via Velopack, so the COM-activation glue
/// isn't there and Show() silently swallows the toast. A bespoke
/// WinUI 3 window has zero side requirements and we already use the
/// same pattern for the pill, caption, and meeting windows.
/// </summary>
public sealed partial class DictToastWindow : Window
{
    private const int WindowWidth = 380;
    private const int WindowHeight = 80;
    private const int MarginPx = 16;

    private DispatcherTimer? _closeTimer;

    public DictToastWindow(string title, string body)
    {
        InitializeComponent();
        Title = "Dimmy Dictionary";
        TitleText.Text = title;
        BodyText.Text = body;
        ApplyThemePalette();
        SetupWindow();

        // Click anywhere on the card → dismiss early.
        ToastCard.PointerPressed += (_, _) => Close();

        // Auto-close after long enough to READ it. DispatcherTimer is fine
        // here — it fires once on the UI thread and closes the window; we're
        // not racing any other UI.
        //
        // The duration used to be a flat 3 s, which suited the four-word
        // dictionary toasts it was written for. It does not suit the ones
        // added since: "Transcription is falling behind ... try Parakeet or a
        // smaller model ... your recording is safe and continues" is 30 words
        // and vanished before it could be read (reported 2026-09-04). Those
        // are exactly the toasts that tell the user what to DO.
        _closeTimer = new DispatcherTimer
        {
            Interval = TimeSpan.FromSeconds(Services.ToastDuration.For(title, body)),
        };
        _closeTimer.Tick += (_, _) =>
        {
            _closeTimer.Stop();
            try { Close(); } catch { }
        };
        _closeTimer.Start();
    }

    /// <summary>Pick light / dark colours from <see cref="ThemeHelper"/>
    /// and paint them directly onto the card brushes. Bespoke palettes
    /// instead of <c>ThemeResource</c> lookups because this is a
    /// transient toast window — it has no parent in the visual tree
    /// whose RequestedTheme would propagate, so theme-resource keys
    /// would always resolve against the system theme regardless of
    /// the user's saved preference.</summary>
    private void ApplyThemePalette()
    {
        bool dark = ThemeHelper.ResolvedIsDark();
        // Same colours used by the pill MenuFlyoutPresenter style — see
        // PillWindow.xaml.cs `ThemedPresenterStyle` for the rationale
        // behind these specific values. Keeping them in sync means
        // every transient surface (popup menus, dict toast) looks
        // visually unified.
        if (dark)
        {
            ToastCard.Background = new SolidColorBrush(global::Windows.UI.Color.FromArgb(0xFF, 0x2B, 0x2B, 0x2B));
            ToastCard.BorderBrush = new SolidColorBrush(global::Windows.UI.Color.FromArgb(0x40, 0xFF, 0xFF, 0xFF));
            TitleText.Foreground = new SolidColorBrush(global::Windows.UI.Color.FromArgb(0xFF, 0xF2, 0xF2, 0xF2));
            BodyText.Foreground = new SolidColorBrush(global::Windows.UI.Color.FromArgb(0xFF, 0xB0, 0xB0, 0xB0));
        }
        else
        {
            ToastCard.Background = new SolidColorBrush(global::Windows.UI.Color.FromArgb(0xFF, 0xF9, 0xF9, 0xF9));
            ToastCard.BorderBrush = new SolidColorBrush(global::Windows.UI.Color.FromArgb(0x20, 0x00, 0x00, 0x00));
            TitleText.Foreground = new SolidColorBrush(global::Windows.UI.Color.FromArgb(0xFF, 0x1A, 0x1A, 0x1A));
            BodyText.Foreground = new SolidColorBrush(global::Windows.UI.Color.FromArgb(0xFF, 0x60, 0x60, 0x60));
        }
    }

    private void SetupWindow()
    {
        ExtendsContentIntoTitleBar = true;
        // Borderless + always-on-top via OverlappedPresenter so the
        // toast survives even when the user is in a fullscreen app
        // (game, video player). Resizing disabled — the card is a
        // single fixed-size affordance.
        var appWindow = WindowHelper.GetAppWindow(this);
        if (appWindow?.Presenter is OverlappedPresenter presenter)
        {
            presenter.IsResizable = false;
            presenter.SetBorderAndTitleBar(false, false);
            presenter.IsAlwaysOnTop = true;
            presenter.IsMaximizable = false;
            presenter.IsMinimizable = false;
        }
        appWindow?.Resize(new global::Windows.Graphics.SizeInt32(WindowWidth, WindowHeight));

        // Transparent + click-through-able backdrop so the rounded
        // card stands alone (no rectangular window chrome behind).
        var backdrop = new TransparentBackdrop { Hwnd = WindowHelper.GetHwnd(this) };
        SystemBackdrop = backdrop;
        WindowHelper.EnableTransparency(this);

        // Anchor: bottom-right with a 16px margin from each edge,
        // shifted up another ~48px to clear the taskbar height on
        // typical Win11 setups. Multi-monitor: lands on the monitor
        // containing the cursor at construction time (= where the
        // user is currently working).
        var dq = Microsoft.UI.Windowing.DisplayArea.GetFromWindowId(
            appWindow!.Id,
            Microsoft.UI.Windowing.DisplayAreaFallback.Primary);
        if (dq is not null)
        {
            var wa = dq.WorkArea; // excludes taskbar
            int x = wa.X + wa.Width - WindowWidth - MarginPx;
            int y = wa.Y + wa.Height - WindowHeight - MarginPx;
            appWindow.Move(new global::Windows.Graphics.PointInt32(x, y));
        }
    }
}
