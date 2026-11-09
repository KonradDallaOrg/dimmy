using System.IO;
using System.Text.Json;

namespace Dimmy.Windows.Helpers;

/// What to tell the user when a meeting's transcriber produces no text.
/// Driven by the core's `meeting_transcription_unavailable` event (reason
/// codes from `core/src/transcriber_health.rs`) and by the `transcript`
/// object the core writes into `meta.json` at stop. Before this a failed
/// meeting and a silent one looked identical, and the window blamed the VAD.
/// Mac mirror: `TranscriptHealthText` in DimmyCore.swift.
public static class TranscriptHealthText
{
    public static string EngineName(string engine) => engine switch
    {
        "nemotron" => "Nemotron",
        "parakeet" => "Parakeet",
        "whisper" => "Whisper",
        "whistle" => "Whistle",
        "cloud" => "the cloud provider",
        _ => "the transcription engine",
    };

    /// One sentence naming the cause.
    public static string Cause(string engine, string reason) => reason switch
    {
        "not_enough_memory" => $"There is not enough free memory to load {EngineName(engine)}.",
        "model_missing" => $"The {EngineName(engine)} model is not downloaded.",
        "no_api_key" => "There is no API key for the transcription provider.",
        "auth" => "The transcription provider refused the API key.",
        "network" => "The transcription provider cannot be reached.",
        "rate_limit" => "The transcription provider is limiting requests.",
        _ => $"{Capitalize(EngineName(engine))} returned errors.",
    };

    /// What the user can do about it.
    public static string Hint(string reason) => reason switch
    {
        "not_enough_memory" => "Close some programs, then use Regenerate transcript after the meeting.",
        "model_missing" => "Download it in Settings, Transcription, then use Regenerate transcript.",
        "no_api_key" or "auth" => "Check the key in Settings, Providers, then use Regenerate transcript.",
        "network" => "Check your connection. Regenerate transcript can redo it after the meeting.",
        _ => "Regenerate transcript can redo it after the meeting.",
    };

    public const string UnavailableTitle = "Transcription stopped";

    public static string UnavailableBody(string engine, string reason) =>
        $"{Cause(engine, reason)} Your recording is safe and continues. {Hint(reason)}";

    public const string RecoveredTitle = "Transcription resumed";

    public const string RecoveredBody =
        "The transcript continues from here. Regenerate transcript after the meeting fills the gap.";

    /// The placeholder shown instead of an empty transcript, from the
    /// `transcript` object of meta.json (`status`, `reason`). Meetings
    /// recorded before the object existed report nothing, so nothing is
    /// claimed about them.
    public static string EmptyTranscript(string? status, string? reason, string? engine) => status switch
    {
        "failed" => $"(no transcript: {Cause(engine ?? "", reason ?? "")} The audio is saved: use Regenerate transcript.)",
        "silent" => "(no transcript: no speech was found in the recording)",
        "empty" => "(no transcript: the recording is too short)",
        _ => "(no transcript: use Regenerate transcript to transcribe the audio)",
    };

    /// Reads `transcript.status` / `transcript.reason` and the engine from a
    /// meeting folder's meta.json. Missing or unreadable: (null, null, null).
    public static (string? Status, string? Reason, string? Engine) ReadMeta(string dir)
    {
        try
        {
            var path = Path.Combine(dir, "meta.json");
            if (!File.Exists(path)) return (null, null, null);
            using var doc = JsonDocument.Parse(File.ReadAllText(path));
            if (!doc.RootElement.TryGetProperty("transcript", out var t) || t.ValueKind != JsonValueKind.Object)
                return (null, null, null);
            string? Str(string name) => t.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String
                ? v.GetString() : null;
            return (Str("status"), Str("reason"), Str("engine"));
        }
        catch (IOException) { return (null, null, null); }
        catch (JsonException) { return (null, null, null); }
        catch (System.UnauthorizedAccessException) { return (null, null, null); }
    }

    public static string EmptyTranscriptFor(string dir)
    {
        var (status, reason, engine) = ReadMeta(dir);
        return EmptyTranscript(status, reason, engine);
    }

    private static string Capitalize(string s) =>
        s.Length == 0 ? s : char.ToUpperInvariant(s[0]) + s[1..];
}
