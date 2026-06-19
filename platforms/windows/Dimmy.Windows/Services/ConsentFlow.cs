using System;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

/// <summary>
/// Recording-consent flow shown before a meeting starts (which captures other
/// people). Mandatory and on by default. The notice text + audit log come from
/// the shared Rust core (DimmyNative.ConsentText / ConsentLogEvent) so every
/// platform says the same thing. Flow: a confirmation modal, then on accept
/// speak the announcement aloud + copy a chat message + log each step.
///
/// Reach: the spoken notice is heard by the user and anyone in the same room;
/// REMOTE call participants only hear it if the user is unmuted, so the copied
/// chat message is the reliable channel for them. A failure to speak or copy
/// must never block the meeting.
///
/// NOTE: WinRT types are written as `global::Windows.*` on purpose — the app
/// namespace is `Dimmy.Windows`, so a bare `Windows.*` resolves to
/// `Dimmy.Windows.*` and fails to compile (CS0234).
/// </summary>
public static class ConsentFlow
{
    private static global::Windows.Media.Playback.MediaPlayer? _player;

    /// <summary>Returns true if the user confirmed consent and the meeting may
    /// start; false if they cancelled.</summary>
    public static async Task<bool> ConfirmAndAnnounceAsync(XamlRoot? xamlRoot, string lang)
    {
        var modal = DimmyNative.ConsentText("modal", lang)
            ?? "You are about to record audio that may include other people. Confirm you have informed all participants and obtained their consent.";
        var announcement = DimmyNative.ConsentText("announcement", lang)
            ?? "Quick note: this meeting is being recorded and transcribed for note-taking.";

        var body = new StackPanel { Spacing = 10 };
        body.Children.Add(new TextBlock { Text = modal, TextWrapping = TextWrapping.Wrap });
        body.Children.Add(new TextBlock
        {
            Text = "Dimmy will read this notice aloud and copy it so you can paste it in the meeting chat:",
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

        var dialog = new ContentDialog
        {
            RequestedTheme = Dimmy.Windows.Helpers.ThemeHelper.ResolvedElementTheme(),
            XamlRoot = xamlRoot,
            Title = "Recording notice",
            Content = body,
            PrimaryButtonText = "I have consent, start",
            CloseButtonText = "Cancel",
            // Safe default: the highlighted button is Cancel, not Start.
            DefaultButton = ContentDialogButton.Close,
        };

        ContentDialogResult result;
        try { result = await dialog.ShowAsync(); }
        catch { result = ContentDialogResult.None; }

        if (result != ContentDialogResult.Primary)
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
