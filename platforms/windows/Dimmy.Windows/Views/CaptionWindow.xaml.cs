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

        // Transparent background so only the text + soft shadow show.
        // The system backdrop must be cleared and the WS_EX_NO_REDIRECTION_BITMAP
        // path is already handled by WindowHelper for the pill — same trick
        // works here.
        try { this.SystemBackdrop = null; } catch { }
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
        var appWindow = WindowHelper.GetAppWindow(this);
        appWindow?.Show();
    }

    public void Hide()
    {
        var appWindow = WindowHelper.GetAppWindow(this);
        appWindow?.Hide();
    }
}
