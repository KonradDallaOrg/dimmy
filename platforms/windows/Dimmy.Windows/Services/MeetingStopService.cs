using System;
using System.Threading;
using System.Threading.Tasks;
using Dimmy.Windows.Interop;
using Dimmy.Windows.ViewModels;

namespace Dimmy.Windows.Services;

/// <summary>The one way a meeting stops: the meeting window's Stop, the
/// pill, the meeting shortcut and the call-detect "Stop &amp; recap" all land
/// here. Stops the core, says when the audio is incomplete, relabels
/// speakers, runs the recap the user chose at start, and always tells an
/// open meeting window that the stop is over. The window and the pill each
/// had their own copy of this until 2026-11-09, and they had drifted (only
/// one toasted a failed recap, only the other showed the speaker pass).
/// Mac mirror: PillWindowController.stopMeetingFromPill.</summary>
public static class MeetingStopService
{
    private static int _inFlight;

    /// <summary>False when another stop was already running: the second
    /// press is dropped instead of reaching the core as a stop of nothing
    /// (rc -1, shown as an error).</summary>
    public static async Task<bool> StopAsync(AppViewModel vm)
    {
        if (Interlocked.Exchange(ref _inFlight, 1) == 1)
        {
            App.Log("meeting stop already running — this press is dropped", "Meeting");
            return false;
        }
        string? stoppedDir = null;
        // Background work, like a loaded file: the pill says
        // "Transcribing..." through TranscribesRunning until the transcript
        // is final, then "Recap..." through RecapsRunning. The dictation
        // state is never written, so a dictation can start while the meeting
        // saves. Mac parity: PillWindowController.stopMeetingFromPill.
        bool transcribing = true;
        vm.TranscribesRunning++;
        void TranscriptFinal()
        {
            if (!transcribing) return;
            transcribing = false;
            vm.TranscribesRunning--;
        }
        try
        {
            App.Log("meeting stop — calling dimmy_meeting_stop", "Meeting");

            var (rc, dir, transcript, stopError, labeled) = await Task.Run(() =>
            {
                var buf = new byte[1 << 22];
                int code = DimmyNative.dimmy_meeting_stop(buf, buf.Length);
                if (code <= 0) return (code, "", "", (string?)null, false);
                var json = System.Text.Encoding.UTF8.GetString(buf, 0, code);
                try
                {
                    using var doc = System.Text.Json.JsonDocument.Parse(json);
                    var root = doc.RootElement;
                    var d = root.GetProperty("dir").GetString() ?? "";
                    var t = root.GetProperty("transcript").GetString() ?? "";
                    // WAV finalize errors (disk full, IO) ride the same payload.
                    var err = root.TryGetProperty("error", out var e) ? e.GetString() : null;
                    bool lab = !(root.TryGetProperty("speaker_pass", out var sl)
                        && sl.ValueKind == System.Text.Json.JsonValueKind.True);
                    return (code, d, t, string.IsNullOrWhiteSpace(err) ? null : err, lab);
                }
                catch { return (code, "", "", (string?)null, false); }
            });
            App.Log($"meeting stop rc={rc} dir='{dir}' transcript={transcript.Length} chars", "Meeting");

            if (rc <= 0)
            {
                vm.SetError($"Meeting stop rc={rc}");
                return true;
            }
            stoppedDir = dir;
            if (stopError != null)
            {
                App.Log($"meeting stop reported error: {stopError}", "Meeting");
                DictNotificationService.ShowMeetingAudioIncomplete(stopError);
                if (!string.IsNullOrEmpty(dir)) Helpers.MeetingDoneNotes.Shared.AudioIncomplete(dir);
            }

            // Read BEFORE the speaker pass: the next meeting can start while
            // it runs and writes its own choice into the same flag.
            bool wantRecap = vm.MeetingGenerateRecap;
            // Speaker labels (when enabled) replace the live per-track
            // transcript before the recap reads it.
            if (!string.IsNullOrEmpty(dir))
                transcript = await DiarizationService.RelabelIfEnabledAsync(dir, transcript, labeled);
            TranscriptFinal();
            if (wantRecap && !string.IsNullOrEmpty(dir) && !string.IsNullOrWhiteSpace(transcript))
            {
                var result = await MeetingPostProcessService.RunRecapAsync(dir, transcript);
                if (!result.Success)
                {
                    App.Log($"meeting stop: recap failed: {result.Error}", "Meeting");
                    vm.SetError(result.Error ?? "Recap failed");
                    // The meeting window must leave "Wrapping up" on a failed
                    // recap too: to the transcript + audio that did succeed,
                    // where the recap can be regenerated. Burned 2026-06-02.
                    App.Instance?.NotifyMeetingRecapSaved(dir);
                }
                else
                {
                    App.Log($"meeting stop: recap saved to {result.Dir}", "Meeting");
                    App.Instance?.NotifyMeetingRecapSaved(result.Dir);
                }
            }
            else
            {
                App.Log(wantRecap
                    ? "meeting stop: no transcript / dir — skipping recap"
                    : "meeting stop: recap disabled at start — skipping recap", "Meeting");
                if (!string.IsNullOrEmpty(dir)) App.Instance?.NotifyMeetingRecapSaved(dir);
            }
            return true;
        }
        catch (Exception ex)
        {
            App.Log($"meeting stop exc: {ex.Message}", "Meeting");
            vm.SetError(ex.Message);
            return true;
        }
        finally
        {
            // Every way out lets an open meeting window leave "Wrapping up":
            // a failed stop (rc <= 0, an exception, no folder) used to return
            // without a word, and the window refused to close for good.
            TranscriptFinal();
            App.Instance?.NotifyMeetingStopEnded(stoppedDir);
            Volatile.Write(ref _inFlight, 0);
        }
    }
}
