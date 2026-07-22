using System;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Graphics;

using Dimmy.Windows.Helpers;

namespace Dimmy.Windows.Views;

/// A small non-activating card in the bottom-right that asks whether to
/// transcribe + recap a voice note the user shared to their Telegram
/// Saved Messages. Same overlay recipe as CallNudgeWindow — it must NOT
/// steal focus, because the audio arrives while the user is doing
/// something else. One window instance is reused across prompts; the
/// TelegramService shows them one at a time from its queue.
///
/// Unlike the call nudge there is deliberately NO auto-dismiss timer: a
/// timed-out prompt would either lose the audio (if it silently dismissed)
/// or contradict "always ask" (if it auto-accepted). The prompt stays up
/// until the user picks Transcribe or Dismiss.
public sealed partial class TelegramNudgeWindow : Window
{
    private const int CardWidthDip = 360;
    private const int CardHeightDip = 96;
    private const int BottomMarginPx = 60;
    private const int RightMarginPx = 16;

    private int _currentMsgId;

    /// User accepted — the arg is the Telegram message id to process.
    public event Action<int>? AcceptRequested;

    /// User dismissed (button, X, or Dismiss) — the arg is the message id.
    public event Action<int>? DismissRequested;

    public TelegramNudgeWindow()
    {
        this.InitializeComponent();
        Title = "Dimmy Telegram Audio";

        ExtendsContentIntoTitleBar = true;
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

        Hide();
    }

    /// Paint + show the card for one pending audio.
    public void ShowFor(int msgId, string filename, long sizeBytes)
    {
        _currentMsgId = msgId;
        var name = string.IsNullOrEmpty(filename) ? "Voice note" : filename;
        TitleText.Text = "New audio from Telegram";
        BodyText.Text = sizeBytes > 0
            ? $"{name}  -  {FormatSize(sizeBytes)}. Transcribe and recap it?"
            : $"{name}. Transcribe and recap it?";

        PositionAtScreenBottomRight();
        WindowHelper.ShowWithoutActivating(this);
    }

    public void Hide()
    {
        var appWindow = WindowHelper.GetAppWindow(this);
        appWindow?.Hide();
    }

    private static string FormatSize(long bytes)
    {
        if (bytes >= 1024L * 1024L)
            return $"{bytes / (1024.0 * 1024.0):0.#} MB";
        if (bytes >= 1024L)
            return $"{bytes / 1024.0:0} KB";
        return $"{bytes} B";
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

    private void OnAcceptClicked(object sender, RoutedEventArgs e)
    {
        var id = _currentMsgId;
        Hide();
        AcceptRequested?.Invoke(id);
    }

    private void OnDismissClicked(object sender, RoutedEventArgs e)
    {
        var id = _currentMsgId;
        Hide();
        DismissRequested?.Invoke(id);
    }

    private void OnCloseClicked(object sender, RoutedEventArgs e)
    {
        var id = _currentMsgId;
        Hide();
        DismissRequested?.Invoke(id);
    }

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern uint GetDpiForWindow(IntPtr hwnd);

    private void ApplyTheme(ElementTheme theme)
    {
        bool dark = theme == ElementTheme.Dark;
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
        if (Content is FrameworkElement root) root.RequestedTheme = theme;
        TitleText.Foreground = fg;
        BodyText.Foreground = fg;
        HeaderIcon.Foreground = fg;
        CloseButton.Foreground = fg;
        DismissButton.Foreground = fg;
    }
}
