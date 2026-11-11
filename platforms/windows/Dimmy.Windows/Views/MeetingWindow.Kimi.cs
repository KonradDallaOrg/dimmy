using System;
using System.Diagnostics;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Dimmy.Windows.Views;

/// <summary>
/// "Recap with Kimi": the Kimi sibling of "Recap with Claude Desktop".
/// Neither Kimi app can pre-fill a prompt from outside (checked in both apps'
/// code, 2026-11-09), so the recap prompt goes on the clipboard.
/// <list type="bullet">
/// <item>Kimi Work (the Kimi app) with the Dimmy plugin installed: opens Work
/// (<c>kimi-work://home</c>, its only external link that does something);
/// the prompt uses Dimmy's tools.</item>
/// <item>Otherwise Kimi Code Desktop: opens a new chat in the meeting's folder
/// (<c>kimi-code://open?root=…&amp;new=1</c>).</item>
/// </list>
/// </summary>
public sealed partial class MeetingWindow
{
    private void RefreshOpenInKimiButton()
    {
        if (OpenInKimiBtn == null) return;
        var s = Interop.DimmyNative.GetKimiMcpStatus();
        var work = s.WorkInstalled && !string.IsNullOrEmpty(s.WorkAppPath);
        OpenInKimiBtn.Visibility = work || !string.IsNullOrEmpty(s.DesktopPath)
            ? Visibility.Visible
            : Visibility.Collapsed;
        ToolTipService.SetToolTip(OpenInKimiBtn, work ? "Recap with Kimi Work" : "Open in Kimi Code Desktop");
    }

    private static string KimiToolsPrompt(string meetingId) =>
        $"Recap Dimmy meeting `{meetingId}`.\n\n" +
        "1. Call `dimmy_get_recap_template` to fetch Dimmy's house format.\n" +
        $"2. Call `dimmy_get_meeting` with id `{meetingId}` to read the transcript.\n" +
        "3. Produce a recap that follows the template's rules exactly (first line is a Markdown H1 title in the transcript's language).\n" +
        $"4. Call `dimmy_save_recap` with id `{meetingId}` and your recap markdown to persist it back into Dimmy.\n" +
        "5. Confirm to me once saved.";

    private void OpenInKimi_Click(object sender, RoutedEventArgs e)
    {
        var dir = _viewingMeetingDir ?? _activeMeetingDir;
        if (string.IsNullOrEmpty(dir)) return;
        var meetingId = System.IO.Path.GetFileName(dir.TrimEnd('\\', '/'));
        var s = Interop.DimmyNative.GetKimiMcpStatus();
        var work = s.WorkInstalled && !string.IsNullOrEmpty(s.WorkAppPath);

        string prompt, link, opened;
        if (work)
        {
            prompt = KimiToolsPrompt(meetingId);
            link = KimiDesktopConnectDialog.WorkLink;
            opened = "Opening Kimi Work. The recap prompt is on your clipboard: start a new chat and press Ctrl+V.";
        }
        else
        {
            prompt = s.Registered
                ? KimiToolsPrompt(meetingId)
                : "This folder is a Dimmy meeting. Read `transcripts.txt` and write a recap in the transcript's language: " +
                  "a Markdown H1 title, then summary, decisions and action items. Save it as `recap.md` in this folder.";
            link = $"kimi-code://open?root={Uri.EscapeDataString(dir)}&new=1";
            opened = "Opening Kimi Code in this meeting's folder. The recap prompt is on your clipboard: press Ctrl+V.";
        }

        try
        {
            var pkg = new global::Windows.ApplicationModel.DataTransfer.DataPackage();
            pkg.SetText(prompt);
            global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(pkg);
        }
        catch { }

        try
        {
            App.Log($"OpenInKimi: meeting={meetingId} work={work}", "Kimi");
            Process.Start(new ProcessStartInfo(link) { UseShellExecute = true });
            ShowToast(opened);
        }
        catch (Exception ex)
        {
            App.Log($"OpenInKimi exc: {ex.Message}", "Kimi");
            ShowToast("Couldn't open Kimi. The prompt is on your clipboard.");
        }
    }
}
