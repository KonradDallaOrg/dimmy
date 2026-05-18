using System;
using System.Collections.Generic;
using Microsoft.UI;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Windows.Graphics;
using WinRT.Interop;
using Dimmy.Windows.Helpers;

namespace Dimmy.Windows.Views;

/// Subtitle-style live caption — borderless, transparent, centered at
/// the bottom of the primary display. No background fill, just white
/// text on a soft drop-shadow so it remains readable over any wallpaper
/// or focused app. Shows a FIFO buffer of the last N chunks (default 2):
/// when a new chunk arrives the oldest one rolls out. The cumulative
/// text used for the final paste lives upstream — this window only owns
/// the on-screen scroll.
public sealed partial class CaptionWindow : Window
{
    private const int VisibleChunks = 2;
    private const int LogicalWidthFraction = 70; // percent of screen width
    private const int BottomMarginPx = 80;       // gap above taskbar
    private const int FixedHeightPx = 110;       // room for ~2 wrapped lines
    private const int MinWidthPx = 480;
    private const int MaxWidthPx = 1200;

    private readonly Queue<string> _chunks = new();

    public CaptionWindow()
    {
        this.InitializeComponent();
        Title = "Dimmy Captions";

        // Same transparency recipe the PillWindow uses (project memory:
        // feedback_window_transparency, "8-step technique"). Without
        // ExtendsContentIntoTitleBar + TransparentBackdrop the WinUI 3
        // window paints the system Mica/acrylic + a thin chrome stripe
        // even when our XAML root is Background="Transparent". Apply
        // the same recipe so the caption renders as bare text on the
        // desktop, like real movie subtitles.
        ExtendsContentIntoTitleBar = true;
        var backdrop = new Helpers.TransparentBackdrop();
        backdrop.Hwnd = WindowHelper.GetHwnd(this);
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

        Hide();
    }

    /// Append a new chunk delta to the FIFO. Trims to the last
    /// `VisibleChunks` entries. Updates the visible text immediately.
    public void PushChunk(string delta)
    {
        if (string.IsNullOrWhiteSpace(delta)) return;
        _chunks.Enqueue(delta.Trim());
        while (_chunks.Count > VisibleChunks)
        {
            _chunks.Dequeue();
        }
        var text = string.Join("  ·  ", _chunks);
        MainText.Text = text;
        ShadowText.Text = text;
        ShadowText2.Text = text;
    }

    public void Reset()
    {
        _chunks.Clear();
        MainText.Text = "";
        ShadowText.Text = "";
        ShadowText2.Text = "";
    }

    /// Park the window centered horizontally, near the bottom of the
    /// primary display. Width scales to a percentage of the screen so
    /// captions feel like proper subtitles, not a balloon.
    public void PositionAtScreenBottom()
    {
        var appWindow = WindowHelper.GetAppWindow(this);
        if (appWindow == null) return;

        var displayArea = DisplayArea.GetFromWindowId(appWindow.Id, DisplayAreaFallback.Primary);
        if (displayArea == null) return;
        var work = displayArea.WorkArea;

        int width = Math.Clamp(
            (int)(work.Width * (LogicalWidthFraction / 100.0)),
            MinWidthPx,
            MaxWidthPx);
        int height = FixedHeightPx;

        appWindow.Resize(new SizeInt32(width, height));
        int x = work.X + (work.Width - width) / 2;
        int y = work.Y + work.Height - height - BottomMarginPx;
        appWindow.Move(new PointInt32(x, y));
    }

    public void Show()
    {
        // SW_SHOWNOACTIVATE — every chunk re-shows the caption (after
        // the user shifted focus to/from it) WITHOUT promoting it to
        // foreground. AppWindow.Show() defaults to activate=true and
        // would otherwise re-steal focus on every chunk, defeating
        // the foreground-restore safety net in StopAndProcessAsync.
        WindowHelper.ShowWithoutActivating(this);
    }

    public void Hide()
    {
        var appWindow = WindowHelper.GetAppWindow(this);
        appWindow?.Hide();
    }
}
