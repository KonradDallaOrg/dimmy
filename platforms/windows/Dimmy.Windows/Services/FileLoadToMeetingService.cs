using System;
using System.IO;
using System.Text.Json;
using System.Threading.Tasks;

namespace Dimmy.Windows.Services;

/// <summary>
/// Bridge between the Settings → "Transcribe a file" card and the
/// Meeting recap pipeline. Given a successful file-load transcript +
/// the source WAV path, this:
///   1. Mints a synthetic meeting directory under the same root the
///      live recorder uses (<c>%APPDATA%\dimmy\meetings\&lt;guid&gt;</c>),
///   2. Writes `transcripts.txt` in the same `[ELAPSED_MS ms] [LABEL] text`
///      grammar the recap LLM prompt expects (so the existing
///      <see cref="MeetingPostProcessService"/> works without a code
///      branch),
///   3. Writes `meta.json` with title/started_at/duration so the
///      Meeting → History list renders the new entry nicely,
///   4. Optionally copies the source WAV to `audio.wav` so playback
///      from the Done view works (small enough to be worth it for
///      most files; the user can pick a 95-min recording too and a
///      copy is fine — disk is cheap, we already keep history audio
///      under retention rules in the Rust core),
///   5. Hands off to <see cref="MeetingPostProcessService.RunRecapAsync"/>
///      which does the LLM call + writes `recap.md` / `actions.json`.
///
/// Single-writer rule is preserved: only this service writes to the
/// meeting dir on the file-load path; Rust still owns config.json and
/// the live-meeting writers.
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
            var meetingsRoot = Path.Combine(BuildInfo.ConfigDirPath, "meetings");
            Directory.CreateDirectory(meetingsRoot);

            var dirName = $"{DateTime.UtcNow:yyyyMMddTHHmmss}-{Guid.NewGuid():N}";
            var dir = Path.Combine(meetingsRoot, dirName);
            Directory.CreateDirectory(dir);

            // transcripts.txt — single line; the recap LLM prompt
            // tolerates a single `[0 ms] [<label>] <text>` line.
            var label = "file";
            var transcriptsPath = Path.Combine(dir, "transcripts.txt");
            await File.WriteAllTextAsync(
                transcriptsPath,
                $"[0 ms] [{label}] {transcript.Trim()}{Environment.NewLine}");

            // audio.wav — sidecar for the Done view (waveform + media
            // player). WAV sources can be copied verbatim; everything
            // else (m4a / mp3 / aac / flac / ogg) goes through the
            // Rust `dimmy_decode_audio_to_wav` FFI so the resulting
            // file is a real RIFF/WAVE container and the WAV-only
            // consumers downstream (WavPeaks.ReadPeaks,
            // MediaPlayerElement on some codec-poor systems) keep
            // working without per-format branches.
            double durationSecs = 0;
            try
            {
                if (File.Exists(sourceWavPath))
                {
                    var audioDest = Path.Combine(dir, "audio.wav");
                    var ext = Path.GetExtension(sourceWavPath);
                    bool isWav = !string.IsNullOrEmpty(ext)
                        && ext.Equals(".wav", StringComparison.OrdinalIgnoreCase);
                    if (isWav)
                    {
                        File.Copy(sourceWavPath, audioDest, overwrite: true);
                    }
                    else
                    {
                        int rc = await Task.Run(() =>
                            Dimmy.Windows.Interop.DimmyNative.dimmy_decode_audio_to_wav(
                                sourceWavPath, audioDest));
                        if (rc <= 0)
                        {
                            App.Log($"FileLoadToMeeting: decode-to-wav rc={rc} for {sourceWavPath}", "FileLoad");
                        }
                    }
                    durationSecs = Helpers.WavPeaks.ReadDurationSecs(audioDest);
                }
            }
            catch (Exception ex)
            {
                App.Log($"FileLoadToMeeting: audio copy/decode failed: {ex.Message}", "FileLoad");
            }

            // meta.json — the MeetingWindow history loader reads
            // `started_at` (string) + `duration_secs` (double) + `title`
            // (string). Title comes from the source filename so the user
            // can tell loaded files apart from live recordings at a
            // glance.
            var fname = string.IsNullOrEmpty(sourceWavPath)
                ? "Imported audio"
                : Path.GetFileNameWithoutExtension(sourceWavPath);
            var meta = new
            {
                title = $"File: {fname}",
                started_at = DateTime.Now.ToString("yyyy-MM-dd HH:mm"),
                duration_secs = durationSecs,
                origin = "file-load",
            };
            await File.WriteAllTextAsync(
                Path.Combine(dir, "meta.json"),
                JsonSerializer.Serialize(meta, new JsonSerializerOptions { WriteIndented = true }));

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
