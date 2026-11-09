using System;
using System.IO;
using System.Threading.Tasks;

namespace Dimmy.Windows.Services;

/// <summary>
/// Bridge between the Settings → "Transcribe a file" card (and Telegram)
/// and the Meeting recap pipeline. Given a successful file-load transcript +
/// the source audio path, the core makes the meeting (`dimmy_meeting_import`:
/// folder, transcripts.txt, audio copy, meta.json, the same shape as a live
/// meeting), then <see cref="MeetingPostProcessService.RunRecapAsync"/> runs
/// the recap. Only the core writes the meeting folder.
/// </summary>
public static class FileLoadToMeetingService
{
    public sealed class Result
    {
        public bool Success { get; init; }
        public string Dir { get; init; } = "";
        public string? Error { get; init; }
    }

    /// <summary>
    /// Create a meeting dir for the file-load transcript and trigger
    /// the recap pipeline. Returns the dir + success/failure so the
    /// caller can deep-link the user into the Done view.
    /// </summary>
    public static async Task<Result> RunAsync(string sourceWavPath, string transcript)
    {
        if (string.IsNullOrWhiteSpace(transcript))
            return new Result { Success = false, Error = "empty transcript" };

        try
        {
            // Probed on the source: the core copies it, keeping its own
            // container (an hour of m4a is a tenth of the WAV it used to be
            // decoded into).
            double durationSecs = 0;
            try
            {
                if (File.Exists(sourceWavPath))
                    durationSecs = Helpers.WavPeaks.ReadDurationAny(sourceWavPath);
            }
            catch (Exception ex)
            {
                App.Log($"FileLoadToMeeting: duration probe failed: {ex.Message}", "FileLoad");
            }
            if (!double.IsFinite(durationSecs) || durationSecs < 0) durationSecs = 0;

            // The core makes the meeting (folder, transcripts.txt, audio copy,
            // meta.json) in the shape a live one has: this service used to
            // write all of it, with its own folder names and a local-time
            // `started_at` string where every other meeting has Unix seconds.
            var buf = new byte[4096];
            int n = Dimmy.Windows.Interop.DimmyNative.dimmy_meeting_import(
                string.IsNullOrEmpty(sourceWavPath) ? null : sourceWavPath,
                transcript, durationSecs, buf, buf.Length);
            if (n <= 0)
                return new Result { Success = false, Error = $"could not create the meeting (rc {n})" };
            var dir = System.Text.Encoding.UTF8.GetString(buf, 0, n);

            App.Log($"FileLoadToMeeting: dir created at {dir}, transcript={transcript.Length}c, duration={durationSecs:F1}s", "FileLoad");
            Dimmy.Windows.Interop.DimmyNative.TrackEvent("meeting.imported_from_file");

            var recapResult = await MeetingPostProcessService.RunRecapAsync(dir, transcript);
            if (!recapResult.Success)
            {
                return new Result
                {
                    Success = false,
                    Dir = dir,
                    Error = recapResult.Error ?? "recap failed",
                };
            }

            App.Log($"FileLoadToMeeting: recap succeeded for dir={dir}", "FileLoad");
            return new Result { Success = true, Dir = dir };
        }
        catch (Exception ex)
        {
            App.Log($"FileLoadToMeeting: exc {ex.GetType().Name}: {ex.Message}", "FileLoad");
            return new Result { Success = false, Error = ex.Message };
        }
    }
}
