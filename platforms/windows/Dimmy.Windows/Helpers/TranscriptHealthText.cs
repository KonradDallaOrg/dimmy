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
        var t = ReadStrings(dir, "transcript", "status", "reason", "engine");
        return (t[0], t[1], t[2]);
    }

    /// `recap.status` / `recap.reason` from meta.json: saved, pending,
    /// failed, or (null, null) for a meeting with no record of one.
    public static (string? Status, string? Reason) ReadRecap(string dir)
    {
        var r = ReadStrings(dir, "recap", "status", "reason");
        return (r[0], r[1]);
    }

    /// The Notion page the recap went to, or null when it was never sent.
    public static string? NotionPageUrl(string dir) => ReadStrings(dir, "notion", "page_url")[0];

    /// What the Done view says in place of a recap that failed.
    public static string RecapFailed(string? reason) =>
        string.IsNullOrWhiteSpace(reason)
            ? "The recap failed. Use Regenerate recap to try again."
            : $"The recap failed: {reason}. Use Regenerate recap to try again.";

    private static string?[] ReadStrings(string dir, string objectName, params string[] names)
    {
        var none = new string?[names.Length];
        try
        {
            var path = Path.Combine(dir, "meta.json");
            if (!File.Exists(path)) return none;
            using var doc = JsonDocument.Parse(File.ReadAllText(path));
            if (!doc.RootElement.TryGetProperty(objectName, out var o) || o.ValueKind != JsonValueKind.Object)
                return none;
            var values = new string?[names.Length];
            for (int i = 0; i < names.Length; i++)
                values[i] = o.TryGetProperty(names[i], out var v) && v.ValueKind == JsonValueKind.String
                    ? v.GetString() : null;
            return values;
        }
        catch (IOException) { return none; }
        catch (JsonException) { return none; }
        catch (System.UnauthorizedAccessException) { return none; }
    }

    public static string EmptyTranscriptFor(string dir)
    {
        var (status, reason, engine) = ReadMeta(dir);
        return EmptyTranscript(status, reason, engine);
    }

    private static string Capitalize(string s) =>
        s.Length == 0 ? s : char.ToUpperInvariant(s[0]) + s[1..];
}
