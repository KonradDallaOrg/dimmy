using System.Threading.Tasks;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

/// <summary>
/// Speaker labels for a meeting that just stopped. The live transcript is
/// written per track in 15-30 s chunks and cannot be split by voice; when the
/// user turned speaker labels on, the whole meeting is transcribed again from
/// the saved audio with diarization (core <c>dimmy_meeting_retranscribe</c>),
/// so the recap that follows already knows who said what.
/// </summary>
public static class DiarizationService
{
    public static bool Enabled()
    {
        try
        {
            var buf = new byte[1 << 14];
            int n = DimmyNative.dimmy_get_config_json(buf, buf.Length);
            if (n <= 0) return false;
            using var doc = System.Text.Json.JsonDocument.Parse(
                System.Text.Encoding.UTF8.GetString(buf, 0, n));
            // Local STT only: re-transcribing with a cloud provider would
            // upload the whole meeting a second time. There, labels come from
            // an explicit "Regenerate transcript" instead.
            var root = doc.RootElement;
            return root.TryGetProperty("diarization_enabled", out var el)
                && el.ValueKind == System.Text.Json.JsonValueKind.True
                && root.TryGetProperty("stt_mode", out var mode)
                && mode.GetString() == "local"
                && DimmyNative.dimmy_diarization_model_present() == 1;
        }
        catch (System.Text.Json.JsonException) { return false; }
    }

    /// The speaker-labelled transcript of <paramref name="dir"/>, or
    /// <paramref name="liveTranscript"/> unchanged when labels are off or the
    /// pass fails — a failed diarization must never cost the user the
    /// transcript or the recap they would have had without it.
    public static async Task<string> RelabelIfEnabledAsync(string dir, string liveTranscript)
    {
        if (string.IsNullOrEmpty(dir) || !Enabled()) return liveTranscript;
        var sw = System.Diagnostics.Stopwatch.StartNew();
        var labelled = await Task.Run(() =>
        {
            var buf = new byte[1 << 22];
            int rc = DimmyNative.dimmy_meeting_retranscribe(dir, buf, buf.Length);
            return rc > 0 ? System.Text.Encoding.UTF8.GetString(buf, 0, rc) : null;
        });
        App.Log($"diarized relabel {(labelled != null ? "ok" : "failed")} in {sw.Elapsed.TotalSeconds:F1}s", "Meeting");
        return string.IsNullOrWhiteSpace(labelled) ? liveTranscript : labelled!;
    }
}
