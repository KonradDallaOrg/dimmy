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
/// Hosting: when an app window is already open (Meeting/Settings) we show a
/// normal <see cref="ContentDialog"/> in it. When triggered globally (the
/// meeting hotkey) there is no window, so we build a small dedicated dialog
/// WINDOW sized to its content, themed (system / in-app), with a Mica backdrop.
/// History of what NOT to do (all burned 2026-06-24): the tiny pill XamlRoot
/// clipped a ContentDialog; a fixed-size host window was taller than the card
/// (black void); a backdrop-less window rendered as a giant dark rectangle; a
/// full-work-area acrylic host was a punch in the eye.
///
/// NOTE: WinRT types are written as `global::Windows.*` on purpose — the app
/// namespace is `Dimmy.Windows`, so a bare `Windows.*` resolves to
/// `Dimmy.Windows.*` and fails to compile (CS0234).
/// </summary>
public static class ConsentFlow
{
    private static global::Windows.Media.Playback.MediaPlayer? _player;

    // Dialog content column width, in DIPs.
    private const double ContentWidth = 440;

    /// Announce without asking, for the auto-record path: the user
    /// answered the question once in Settings, and a dialog at call time
    /// would defeat "start recording immediately". The notice is played on
    /// this machine's output and copied for pasting into the meeting chat,
    /// the same as the manual path. Neither tells the other participants by
    /// itself (headphones, call echo cancellation), which is why the hint
    /// names the chat. Mac mirror: MeetingConsentFlow.announceOnly.
    public static void AnnounceOnly(string lang)
    {
        var (text, variant) = DimmyNative.ConsentAnnouncement(lang);
        var announcement = text
            ?? "Quick note: this meeting is being recorded and transcribed for note-taking.";
        CopyNotice(announcement, lang);
        // AnnounceAsync writes the audit entry once the notice has actually
        // been played; logging it here as well recorded every one twice.
        _ = AnnounceAsync(announcement, lang, variant);
    }

    /// The pasteable notice is the only part that can reach people who do
    /// not hear this machine's speakers, so it is copied on every start and
    /// the user is told to paste it. Best-effort: a clipboard failure must
    /// not block the meeting.
    private static void CopyNotice(string announcement, string lang)
    {
        try
        {
            var dp = new global::Windows.ApplicationModel.DataTransfer.DataPackage();
            dp.SetText(announcement);
            global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(dp);
            DimmyNative.ConsentLogEvent("notice_copied", lang);
            DictNotificationService.ShowNoticeCopied();
        }
        catch { /* clipboard failure must not block the meeting */ }
    }

    private static string AutoRecordMarker =>
        System.IO.Path.Combine(BuildInfo.ConfigDirPath, ".auto_record_confirmed");

    /// One-time confirmation the first time "Start recording automatically"
    /// is turned on. A marker file, not a UiPreferences field: the App keeps
    /// a long-lived UiPreferences copy and re-saves it, which would erase a
    /// flag written by the Settings window.
    public static async Task<bool> ConfirmAutoRecordAsync(XamlRoot? xamlRoot, string lang)
    {
        try { if (System.IO.File.Exists(AutoRecordMarker)) return true; } catch { }

        var dialog = new ContentDialog
        {
            RequestedTheme = Dimmy.Windows.Helpers.ThemeHelper.ResolvedElementTheme(),
            XamlRoot = xamlRoot,
            Title = "Record calls automatically?",
            Content = new TextBlock
            {
                TextWrapping = TextWrapping.Wrap,
                Text =
                    "Other participants are not notified automatically. Dimmy plays a notice on " +
                    "your speakers and copies it so you can paste it in the meeting chat, but " +
                    "people on the call may not hear it.\n\n" +
                    "In some places (for example California and Germany) everyone must consent " +
                    "to being recorded. Getting that consent is your responsibility.",
            },
            PrimaryButtonText = "Turn on",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        bool ok;
        try { ok = await dialog.ShowAsync() == ContentDialogResult.Primary; }
        catch { return false; }
        if (!ok) return false;

        try { System.IO.File.WriteAllText(AutoRecordMarker, "1"); }
        catch (Exception ex) { App.Log($"auto-record marker not written: {ex.Message}", "Consent"); }
        DimmyNative.ConsentLogEvent("auto_start_confirmed", lang);
        return true;
    }

    public static async Task<bool> ConfirmAndAnnounceAsync(XamlRoot? xamlRoot, string lang)
    {
        string T(string kind, string fallback) => DimmyNative.ConsentText(kind, lang) ?? fallback;

        var title = T("title", "Recording notice");
        var modal = T("modal",
            "You are about to record audio that may include other people. Confirm you have informed all participants and obtained their consent.");
        var intro = T("intro",
            "Dimmy will read this notice aloud and copy it so you can paste it in the meeting chat:");
        // Picked once: the dialog shows it, the clipboard carries it and the
        // recording speaks it, and all three must be the same wording.
        var (announcementText, variant) = DimmyNative.ConsentAnnouncement(lang);
        var announcement = announcementText
            ?? "Quick note: this meeting is being recorded and transcribed for note-taking.";
        var confirmLabel = T("confirm", "I have consent, start");
        var cancelLabel = T("cancel", "Cancel");
        var theme = Dimmy.Windows.Helpers.ThemeHelper.ResolvedElementTheme();

        var confirmed = xamlRoot != null
            ? await ShowInWindowDialogAsync(xamlRoot, theme, title, modal, intro, announcement, confirmLabel, cancelLabel)
            : await ShowStandaloneAsync(theme, title, modal, intro, announcement, confirmLabel, cancelLabel);

        if (!confirmed)
        {
            DimmyNative.ConsentLogEvent("declined", lang);
            return false;
        }
        DimmyNative.ConsentLogEvent("confirmed", lang);

        CopyNotice(announcement, lang);
        _ = AnnounceAsync(announcement, lang, variant);
        return true;
    }

    // ContentDialog hosted in an already-open app window.
    private static async Task<bool> ShowInWindowDialogAsync(
        XamlRoot xamlRoot, ElementTheme theme, string title, string modal,
        string intro, string announcement, string confirmLabel, string cancelLabel)
    {
        var dialog = new ContentDialog
        {
            RequestedTheme = theme,
            XamlRoot = xamlRoot,
            Title = title,
            Content = BuildBody(modal, intro, announcement),
            PrimaryButtonText = confirmLabel,
            CloseButtonText = cancelLabel,
            DefaultButton = ContentDialogButton.Close,
        };
        try { return await dialog.ShowAsync() == ContentDialogResult.Primary; }
        catch { return false; }
    }

    // Small dedicated dialog window for the global (hotkey) trigger.
    private static async Task<bool> ShowStandaloneAsync(
        ElementTheme theme, string title, string modal,
        string intro, string announcement, string confirmLabel, string cancelLabel)
    {
        var tcs = new TaskCompletionSource<bool>();
        var win = new Window { Title = title, SystemBackdrop = new MicaBackdrop() };

        var stack = new StackPanel { Spacing = 12, Padding = new Thickness(24, 22, 24, 18) };
        stack.Children.Add(new TextBlock
        {
            Text = title,
            FontSize = 20,
            FontWeight = global::Microsoft.UI.Text.FontWeights.SemiBold,
            TextWrapping = TextWrapping.Wrap,
        });
        stack.Children.Add(new TextBlock { Text = modal, TextWrapping = TextWrapping.Wrap });
        stack.Children.Add(new TextBlock
        {
            Text = intro,
            TextWrapping = TextWrapping.Wrap,
            Opacity = 0.75,
            FontSize = 12.5,
        });
        stack.Children.Add(new TextBlock
        {
            Text = announcement,
            TextWrapping = TextWrapping.Wrap,
            FontStyle = global::Windows.UI.Text.FontStyle.Italic,
        });

        var cancelBtn = new Button { Content = cancelLabel, MinWidth = 110 };
        var startBtn = new Button { Content = confirmLabel, MinWidth = 110 };
        if (Application.Current.Resources["AccentButtonStyle"] is Style accent)
            startBtn.Style = accent;
        cancelBtn.Click += (_, __) => { tcs.TrySetResult(false); try { win.Close(); } catch { } };
        startBtn.Click += (_, __) => { tcs.TrySetResult(true); try { win.Close(); } catch { } };
        stack.Children.Add(new StackPanel
        {
            Orientation = Orientation.Horizontal,
            HorizontalAlignment = HorizontalAlignment.Right,
            Spacing = 10,
            Margin = new Thickness(0, 6, 0, 0),
            Children = { cancelBtn, startBtn },
        });

        var root = new Grid { RequestedTheme = theme }; // transparent → Mica shows through
        root.Children.Add(stack);
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
            // Pre-size to a sensible estimate so we never flash the giant default.
            win.AppWindow.Resize(new global::Windows.Graphics.SizeInt32(480, 380));
        }
        catch { }

        root.Loaded += (_, __) =>
        {
            try
            {
                double scale = root.XamlRoot?.RasterizationScale ?? 1.0;
                root.Measure(new global::Windows.Foundation.Size(ContentWidth, double.PositiveInfinity));
                double dipH = root.DesiredSize.Height;
                if (dipH < 80 || double.IsNaN(dipH)) dipH = 340;
                var aw = win.AppWindow;
                aw.ResizeClient(new global::Windows.Graphics.SizeInt32(
                    (int)Math.Ceiling(ContentWidth * scale),
                    (int)Math.Ceiling(dipH * scale)));
                var da = DisplayArea.GetFromWindowId(aw.Id, DisplayAreaFallback.Primary);
                aw.Move(new global::Windows.Graphics.PointInt32(
                    da.WorkArea.X + (da.WorkArea.Width - aw.Size.Width) / 2,
                    da.WorkArea.Y + (da.WorkArea.Height - aw.Size.Height) / 2));
            }
            catch { }
            cancelBtn.Focus(FocusState.Programmatic);
        };

        win.Closed += (_, __) => tcs.TrySetResult(false);
        win.Activate();
        return await tcs.Task;
    }

    private static StackPanel BuildBody(string modal, string intro, string announcement)
    {
        var body = new StackPanel { Spacing = 10 };
        body.Children.Add(new TextBlock { Text = modal, TextWrapping = TextWrapping.Wrap });
        body.Children.Add(new TextBlock
        {
            Text = intro,
            TextWrapping = TextWrapping.Wrap,
            Opacity = 0.8,
            FontSize = 12,
        });
        body.Children.Add(new TextBlock
        {
            Text = announcement,
            TextWrapping = TextWrapping.Wrap,
            FontStyle = global::Windows.UI.Text.FontStyle.Italic,
        });
        return body;
    }

    /// Speak the notice: the recorded take if the core has one, the system
    /// voice otherwise. The fallback is not decoration — a machine without
    /// working audio decoding still has to tell the room it is recording.
    private static async Task AnnounceAsync(string text, string lang, int variant)
    {
        if (DimmyNative.ConsentAudioMuted())
        {
            App.Log("spoken notice muted by the mute-consent-audio file", "Consent");
            DimmyNative.ConsentLogEvent("announce_muted", lang);
            return;
        }
        if (TryPlayRecordedTake(lang, variant))
        {
            // Truthful name: this started playback on THIS machine. It says
            // nothing about whether anyone on the call heard it.
            DimmyNative.ConsentLogEvent("notice_played_locally", lang);
            return;
        }
        await SpeakAsync(text, lang);
    }

    private static bool TryPlayRecordedTake(string lang, int variant)
    {
        try
        {
            var mp3 = DimmyNative.ConsentAudio(lang, variant);
            if (mp3 == null || mp3.Length == 0)
            {
                App.Log($"no recorded take for {lang}/{variant}, using the system voice", "Consent");
                return false;
            }
            var stream = new global::Windows.Storage.Streams.InMemoryRandomAccessStream();
            var w = new global::Windows.Storage.Streams.DataWriter(stream.GetOutputStreamAt(0));
            w.WriteBytes(mp3);
            w.StoreAsync().AsTask().GetAwaiter().GetResult();
            // DetachStream before disposing the writer. Disposing it while it
            // still owns the output stream closes the stream underneath, and
            // MediaPlayer then plays nothing at all — no exception, no sound,
            // and the audit log still says the room was told.
            w.DetachStream();
            w.Dispose();
            stream.Seek(0);

            _player ??= new global::Windows.Media.Playback.MediaPlayer();
            _player.Source = global::Windows.Media.Core.MediaSource.CreateFromStream(stream, "audio/mpeg");
            _player.Play();
            App.Log($"recorded take {lang}/{variant}, {mp3.Length} bytes", "Consent");
            return true;
        }
        catch (Exception ex)
        {
            App.Log($"recorded take failed ({ex.GetType().Name}: {ex.Message}), using the system voice", "Consent");
            return false;
        }
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
            DimmyNative.ConsentLogEvent("notice_played_locally", lang);
        }
        catch { /* TTS failure must never block the meeting */ }
    }
}
