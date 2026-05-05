using System;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Windows.Graphics;
using WinRT.Interop;
using Dimmy.Windows.Helpers;

namespace Dimmy.Windows.Views;

/// Floating caption window — sits a few pixels below the pill while
/// the realtime chunked transcriber is producing partials. Borderless,
/// click-through-friendly, transparent corners. Auto-resizes to the
/// text height up to a fixed max width that mirrors Win11 live captions.
///
/// Why a separate window (not a sub-element of the pill): the pill
/// design is locked — shape, proportions, colors. Stretching it to
/// fit a growing transcript would break the captured-in-memory invariants.
/// A separate WS_POPUP is the same pattern the OS itself uses for live
/// captions and toasts.
public sealed partial class CaptionWindow : Window
{
    private const int MaxLogicalWidth = 720;
    private const int MaxLogicalHeight = 200;

    public CaptionWindow()
    {
        this.InitializeComponent();
        Title = "Dimmy Captions";

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
            try
            {
                appWindow.IsShownInSwitchers = false;
            }
            catch { /* not all SDK versions expose it */ }
        }

        // Transparent system backdrop so corners don't show a white
        // square around our rounded Border.
        try
        {
            this.SystemBackdrop = null;
            if (Content is FrameworkElement root)
            {
                root.RequestedTheme = ElementTheme.Dark;
            }
        }
        catch { }

        WindowHelper.ResizeLogical(this, MaxLogicalWidth, 60);
        Hide();
    }

    public void SetText(string text)
    {
        if (string.IsNullOrEmpty(text))
        {
            CaptionText.Text = "";
            return;
        }
        CaptionText.Text = text;
        // Re-measure so the caption resizes to fit content.
        CaptionBorder.Measure(new global::Windows.Foundation.Size(MaxLogicalWidth, MaxLogicalHeight));
        var desired = CaptionBorder.DesiredSize;
        // Add a small margin so the rounded corner of the border
        // isn't clipped by the window edge.
        int w = Math.Min(MaxLogicalWidth, (int)Math.Ceiling(desired.Width) + 8);
        int h = Math.Min(MaxLogicalHeight, (int)Math.Ceiling(desired.Height) + 8);
        WindowHelper.ResizeLogical(this, Math.Max(240, w), Math.Max(40, h));
    }

    /// Position the window directly below the given anchor rect (the
    /// pill window's bounds in screen coordinates). Centered horizontally,
    /// 12 px gap below.
    public void PositionBelow(int pillScreenX, int pillScreenY, int pillScreenWidth, int pillScreenHeight)
    {
        var appWindow = WindowHelper.GetAppWindow(this);
        if (appWindow == null) return;

        var captionWidth = appWindow.Size.Width;
        int x = pillScreenX + (pillScreenWidth - captionWidth) / 2;
        int y = pillScreenY + pillScreenHeight + 12;
        appWindow.Move(new PointInt32(x, y));
    }

    public void Show()
    {
        var appWindow = WindowHelper.GetAppWindow(this);
        appWindow?.Show();
    }

    public void Hide()
    {
        var appWindow = WindowHelper.GetAppWindow(this);
        appWindow?.Hide();
    }
}
