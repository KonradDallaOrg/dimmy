using System;
using System.Diagnostics;
using Microsoft.UI.Xaml;

namespace Dimmy.Windows.Views;

/// <summary>
/// "Open in Kimi Code": the Kimi sibling of "Recap with Claude Desktop".
/// Kimi Code Desktop's only deep link is <c>kimi-code://open?root=&lt;dir&gt;&amp;new=1</c>
/// (a new chat in a folder; it cannot pre-fill a prompt), so the recap
/// prompt goes on the clipboard and the chat opens in the meeting's folder,
/// where Kimi can also read transcripts.txt itself.
/// </summary>
public sealed partial class MeetingWindow
{
    private void RefreshOpenInKimiButton()
    {
        if (OpenInKimiBtn == null) return;
        var s = Interop.DimmyNative.GetKimiMcpStatus();
        OpenInKimiBtn.Visibility = string.IsNullOrEmpty(s.DesktopPath)
            ? Visibility.Collapsed
            : Visibility.Visible;
    }

    private void OpenInKimi_Click(object sender, RoutedEventArgs e)
    {
        var dir = _viewingMeetingDir ?? _activeMeetingDir;
        if (string.IsNullOrEmpty(dir)) return;
        var meetingId = System.IO.Path.GetFileName(dir.TrimEnd('\\', '/'));
        var mcp = Interop.DimmyNative.GetKimiMcpStatus().Registered;

        var prompt = mcp
            ? $"Recap Dimmy meeting `{meetingId}`.\n\n" +
              "1. Call `dimmy_get_recap_template` to fetch Dimmy's house format.\n" +
              $"2. Call `dimmy_get_meeting` with id `{meetingId}` to read the transcript.\n" +
              "3. Produce a recap that follows the template's rules exactly (first line is a Markdown H1 title in the transcript's language).\n" +
              $"4. Call `dimmy_save_recap` with id `{meetingId}` and your recap markdown to persist it back into Dimmy.\n" +
              "5. Confirm to me once saved."
            : "This folder is a Dimmy meeting. Read `transcripts.txt` and write a recap in the transcript's language: " +
              "a Markdown H1 title, then summary, decisions and action items. Save it as `recap.md` in this folder.";

        try
        {
            var pkg = new global::Windows.ApplicationModel.DataTransfer.DataPackage();
            pkg.SetText(prompt);
            global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(pkg);
        }
        catch { }

        var link = $"kimi-code://open?root={Uri.EscapeDataString(dir)}&new=1";
        try
        {
            App.Log($"OpenInKimi: meeting={meetingId} mcp={mcp}", "Kimi");
            Process.Start(new ProcessStartInfo(link) { UseShellExecute = true });
            ShowToast("Opening Kimi Code in this meeting's folder. The recap prompt is on your clipboard: paste it.");
        }
        catch (Exception ex)
        {
            App.Log($"OpenInKimi exc: {ex.Message}", "Kimi");
            ShowToast("Couldn't open Kimi Code Desktop. The prompt is on your clipboard.");
        }
    }
}
