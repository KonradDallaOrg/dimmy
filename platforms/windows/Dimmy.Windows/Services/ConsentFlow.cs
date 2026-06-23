using System;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Windowing;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

/// <summary>
/// Recording-consent flow shown before a meeting starts (which captures other
/// people). Mandatory and on by default. ALL the wording (title, body, the
/// helper line, the announcement, and both button labels) comes from the shared
/// Rust core (<see cref="DimmyNative.ConsentText"/>) so every platform — and
/// every language — says the same thing. Flow: a confirmation dialog, then on
/// accept speak the announcement aloud + copy a chat message + log each step.
///
/// The dialog is a dedicated, self-hosted window sized to its content (no host
/// XamlRoot needed). It used to be a ContentDialog hosted on the pill's tiny
/// XamlRoot — which rendered clipped/illegible — and then a fixed-size host
/// window that was taller than the content (black void). Burned 2026-06-24.
///
/// NOTE: WinRT types are written as `global::Windows.*` on purpose — the app
/// namespace is `Dimmy.Windows`, so a bare `Windows.*` resolves to
/// `Dimmy.Windows.*` and fails to compile (CS0234).
/// </summary>
public static class ConsentFlow
{
    private static global::Windows.Media.Playback.MediaPlayer? _player;

    // Content column width in DIPs. The window's client area is sized to this
    // width and to whatever height the wrapped content needs.
    private const double ContentWidth = 460;

    /// <summary>Returns true if the user confirmed consent and the meeting may
    /// start; false if they cancelled. The <paramref name="xamlRoot"/> argument
    /// is accepted for call-site compatibility but no longer used — the dialog
    /// hosts its own correctly-sized window.</summary>
    public static async Task<bool> ConfirmAndAnnounceAsync(XamlRoot? xamlRoot, string lang)
    {
        _ = xamlRoot;
        string T(string kind, string fallback) => DimmyNative.ConsentText(kind, lang) ?? fallback;

        var title = T("title", "Recording notice");
        var modal = T("modal",
            "You are about to record audio that may include other people. Confirm you have informed all participants and obtained their consent.");
        var intro = T("intro",
            "Dimmy will read this notice aloud and copy it so you can paste it in the meeting chat:");
        var announcement = T("announcement",
            "Quick note: this meeting is being recorded and transcribed for note-taking.");
        var confirmLabel = T("confirm", "I have consent, start");
        var cancelLabel = T("cancel", "Cancel");

        var theme = Dimmy.Windows.Helpers.ThemeHelper.ResolvedElementTheme();
        var tcs = new TaskCompletionSource<bool>();

        var win = new Window { Title = title };

        var content = new StackPanel { Spacing = 12, Padding = new Thickness(24, 22, 24, 16) };
        content.Children.Add(new TextBlock
        {
            Text = title,
            FontSize = 20,
            FontWeight = global::Microsoft.UI.Text.FontWeights.SemiBold,
            TextWrapping = TextWrapping.Wrap,
        });
        content.Children.Add(new TextBlock { Text = modal, TextWrapping = TextWrapping.Wrap });
        content.Children.Add(new TextBlock
        {
            Text = intro,
            Opacity = 0.7,
            FontSize = 12.5,
            TextWrapping = TextWrapping.Wrap,
        });
        content.Children.Add(new Border
        {
            Background = Application.Current.Resources["CardBackgroundFillColorSecondaryBrush"] as Brush,
            CornerRadius = new CornerRadius(8),
            Padding = new Thickness(12, 10, 12, 10),
            Child = new TextBlock
            {
                Text = announcement,
                TextWrapping = TextWrapping.Wrap,
                FontStyle = global::Windows.UI.Text.FontStyle.Italic,
            },
        });

        var cancelBtn = new Button { Content = cancelLabel, MinWidth = 96 };
        var startBtn = new Button { Content = confirmLabel, MinWidth = 96 };
        if (Application.Current.Resources["AccentButtonStyle"] is Style accent)
            startBtn.Style = accent;
        cancelBtn.Click += (_, __) => { tcs.TrySetResult(false); try { win.Close(); } catch { } };
        startBtn.Click += (_, __) => { tcs.TrySetResult(true); try { win.Close(); } catch { } };
        content.Children.Add(new StackPanel
        {
            Orientation = Orientation.Horizontal,
            HorizontalAlignment = HorizontalAlignment.Right,
            Spacing = 10,
            Children = { cancelBtn, startBtn },
        });

        var root = new Grid
        {
            RequestedTheme = theme,
            Background = Application.Current.Resources["ApplicationPageBackgroundThemeBrush"] as Brush,
        };
        root.Children.Add(content);
        win.Content = root;

        try
        {
            if (win.AppWindow.Presenter is OverlappedPresenter p)
            {
                p.IsResizable = false;
                p.IsMaximizable = false;
                p.IsMinimizable = false;
                p.IsAlwaysOnTop = true;
            }
        }
        catch { }

        // Size the client area to exactly fit the wrapped content, then centre.
        root.Loaded += (_, __) =>
        {
            try
            {
                double scale = root.XamlRoot?.RasterizationScale ?? 1.0;
                root.Measure(new global::Windows.Foundation.Size(ContentWidth, double.PositiveInfinity));
                double dipH = root.DesiredSize.Height;
                if (dipH < 80) dipH = 360; // fallback if layout not ready
                int w = (int)Math.Ceiling(ContentWidth * scale);
                int h = (int)Math.Ceiling(dipH * scale);
                var aw = win.AppWindow;
                aw.ResizeClient(new global::Windows.Graphics.SizeInt32(w, h));
                var da = DisplayArea.GetFromWindowId(aw.Id, DisplayAreaFallback.Primary);
                int x = da.WorkArea.X + (da.WorkArea.Width - aw.Size.Width) / 2;
                int y = da.WorkArea.Y + (da.WorkArea.Height - aw.Size.Height) / 2;
                aw.Move(new global::Windows.Graphics.PointInt32(x, y));
            }
            catch { }
        };

        // Closing via the X (or any other route) without a button counts as cancel.
        win.Closed += (_, __) => tcs.TrySetResult(false);
        win.Activate();

        var confirmed = await tcs.Task;
        if (!confirmed)
        {
            DimmyNative.ConsentLogEvent("declined", lang);
            return false;
        }
        DimmyNative.ConsentLogEvent("confirmed", lang);

        // Chat message for the participants (the reliable channel for remotes).
        try
        {
            var dp = new global::Windows.ApplicationModel.DataTransfer.DataPackage();
            dp.SetText(announcement);
            global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(dp);
            DimmyNative.ConsentLogEvent("chat_copied", lang);
        }
        catch { /* clipboard failure must not block the meeting */ }

        // Speak it (fire-and-forget; reaches remotes only if the user is unmuted).
        _ = SpeakAsync(announcement, lang);

        return true;
    }

    private static async Task SpeakAsync(string text, string lang)
    {
        try
        {
            using var synth = new global::Windows.Media.SpeechSynthesis.SpeechSynthesizer();
            var voice = global::Windows.Media.SpeechSynthesis.SpeechSynthesizer.AllVoices
                .FirstOrDefault(v => v.Language.StartsWith(lang, StringComparison.OrdinalIgnoreCase));
            if (voice != null) synth.Voice = voice;
            var stream = await synth.SynthesizeTextToStreamAsync(text);
            _player ??= new global::Windows.Media.Playback.MediaPlayer();
            _player.Source = global::Windows.Media.Core.MediaSource.CreateFromStream(stream, stream.ContentType);
            _player.Play();
            DimmyNative.ConsentLogEvent("announced", lang);
        }
        catch { /* TTS failure must never block the meeting */ }
    }
}
