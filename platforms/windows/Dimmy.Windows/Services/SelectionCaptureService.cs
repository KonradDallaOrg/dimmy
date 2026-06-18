using System;
using System.Threading.Tasks;
using WinClipboard = global::Windows.ApplicationModel.DataTransfer.Clipboard;
using WinDataPackage = global::Windows.ApplicationModel.DataTransfer.DataPackage;
using WinStandardDataFormats = global::Windows.ApplicationModel.DataTransfer.StandardDataFormats;

namespace Dimmy.Windows.Services;

/// <summary>
/// Capture whatever text is currently selected in the foreground app,
/// without permanently clobbering the user's clipboard.
///
/// Strategy (battle-tested by the dictionary hotkey since 2026-05-12):
///   1. Stash the existing clipboard text.
///   2. Write a sentinel and wait for the clipboard sequence number to
///      bump — proves the clipboard is writable and gives us a known
///      baseline.
///   3. Send a synthetic Ctrl+C and wait for a SECOND seq bump — proves
///      the foreground app actually fulfilled the copy (i.e. there WAS a
///      selection). No bump ⇒ nothing selected / password field / frozen.
///   4. Read the copied text, reject the sentinel (race) and empties.
///   5. Restore the user's original clipboard.
///
/// Why synthetic Ctrl+C and not UI Automation: UIA's
/// TextPattern.GetSelection only works in apps that implement the text
/// provider (Office, WPF, modern edit controls) and is empty in the
/// Electron apps where power users live (VS Code, Slack, Notion). The
/// Ctrl+C path works in ~99% of apps because "copy" is universal — it's
/// the same approach every shipping dictation competitor uses. UIA can
/// be layered later as a no-clipboard fast path.
///
/// This is Command Mode's selection grab. It deliberately has NO length
/// cap (unlike the dictionary hotkey, which caps at 100 chars) — Command
/// Mode operates on paragraphs.
/// </summary>
public static class SelectionCaptureService
{
    private const string Sentinel = "__DIMMY_CMD_PROBE_v1__";
    private const int SeqPollIntervalMs = 1;
    private const int SetContentMaxWaitMs = 200;
    private const int CopyMaxWaitMs = 500;

    /// <summary>Return the foreground selection text, or null if nothing
    /// was selected / the clipboard couldn't be probed. Never throws.</summary>
    public static async Task<string?> CaptureAsync()
    {
        string? previousText = null;
        try
        {
            try
            {
                var prev = WinClipboard.GetContent();
                if (prev.Contains(WinStandardDataFormats.Text))
                    previousText = await prev.GetTextAsync();
            }
            catch { }

            // Phase 1: write sentinel, wait for seq bump (clipboard writable?)
            uint preSet = TextInjectionService.ClipboardSequenceNumber();
            var probe = new WinDataPackage();
            probe.SetText(Sentinel);
            WinClipboard.SetContent(probe);
            if (!await WaitForSequenceBumpAsync(preSet, SetContentMaxWaitMs))
                return null;

            // Phase 2: atomic Ctrl+C
            uint preCopy = TextInjectionService.ClipboardSequenceNumber();
            TextInjectionService.SendCtrlC();

            // Phase 3: wait for the app to fulfill the copy (= had a selection)
            if (!await WaitForSequenceBumpAsync(preCopy, CopyMaxWaitMs))
            {
                RestoreClipboard(previousText);
                return null;
            }

            var dp = WinClipboard.GetContent();
            if (!dp.Contains(WinStandardDataFormats.Text))
            {
                RestoreClipboard(previousText);
                return null;
            }
            var text = (await dp.GetTextAsync() ?? "").Trim();
            if (text == Sentinel)
            {
                // seq bumped but content is still our sentinel — a race
                // where another writer beat the app's copy, or Ctrl+C copied
                // nothing. Trim-then-compare so a trailing CRLF the OS adds
                // to the probe doesn't sneak the sentinel through as a
                // "selection". Treat as no selection rather than garbage.
                RestoreClipboard(previousText);
                return null;
            }
            RestoreClipboard(previousText);
            return string.IsNullOrEmpty(text) ? null : text;
        }
        catch
        {
            RestoreClipboard(previousText);
            return null;
        }
    }

    private static async Task<bool> WaitForSequenceBumpAsync(uint baselineSeq, int timeoutMs)
    {
        var sw = System.Diagnostics.Stopwatch.StartNew();
        while (sw.ElapsedMilliseconds < timeoutMs)
        {
            uint now = TextInjectionService.ClipboardSequenceNumber();
            if (now == 0) return false; // caller lacks clipboard read right
            if (now != baselineSeq) return true;
            await Task.Delay(SeqPollIntervalMs);
        }
        return false;
    }

    private static void RestoreClipboard(string? previousText)
    {
        try
        {
            if (previousText is null)
            {
                // The clipboard was empty before we probed; we wrote the
                // sentinel into it. Clear it back to empty so the sentinel
                // never lingers for the user's next manual paste — the bug
                // that surfaced `__DIMMY_CMD_PROBE_v1__` in pasted output.
                WinClipboard.Clear();
                return;
            }
            var dp = new WinDataPackage();
            dp.SetText(previousText);
            WinClipboard.SetContent(dp);
        }
        catch { }
    }
}
