using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using Dimmy.Windows.Interop;
using Dimmy.Windows.Views;

namespace Dimmy.Windows.Services;

/// Shared meeting post-process pipeline: takes a stopped meeting's
/// transcript + on-disk dir, builds the Notion-quality structured-recap
/// prompt, calls the configured LLM, persists `recap.md` /
/// `actions.json` next to the audio. Both MeetingWindow.OnStop and
/// PillWindow.Stop_Click (when a meeting is the active recording)
/// route through this service so the recap fires regardless of which
/// surface the user pressed Stop from.
///
/// Pure helper methods — BuildStructuredRecapPrompt /
/// ParseStructuredRecap / BuildMarkdownFromSections — live as
/// `public static` on <see cref="Dimmy.Windows.Helpers.MeetingRecapHelpers"/>
/// so they're testable without a XAML host. PickRecapModel still
/// lives on MeetingWindow as `internal static
/// PickRecapModelInternal` because it depends on AppViewModel state.
public static class MeetingPostProcessService
{
    public sealed class RecapResult
    {
        public bool Success { get; init; }
        public string Dir { get; init; } = "";
        public Dictionary<string, string>? Sections { get; init; }
        public string? Error { get; init; }
    }

    /// Run the recap LLM call for a stopped meeting and persist the
    /// markdown + actions to disk via dimmy_meeting_save_post_process.
    /// Heavy work (LLM call, FFI marshalling) runs on a background
    /// thread; the returned task completes when persisting is done.
    /// <paramref name="meetingType"/> is a taxonomy key (see
    /// <see cref="Helpers.MeetingRecapHelpers.MeetingTypes"/>). Empty /
    /// "auto" → the model classifies the meeting; a specific key tailors
    /// the recap emphasis. The pill stop path passes nothing (auto).
    public static async Task<RecapResult> RunRecapAsync(string dir, string transcript, string meetingType = "")
    {
        if (string.IsNullOrEmpty(dir))
            return new RecapResult { Success = false, Error = "missing dir" };
        if (string.IsNullOrWhiteSpace(transcript))
            return new RecapResult { Success = false, Dir = dir, Error = "empty transcript" };

        var stopwatch = System.Diagnostics.Stopwatch.StartNew();
        try
        {
            // Listener's notes (notes.md) are the user's own emphasis — fold
            // them into the prompt as a high-priority section so both the
            // auto-recap at stop AND a manual re-recap honour them.
            string notes = "";
            try
            {
                var notesPath = System.IO.Path.Combine(dir, "notes.md");
                if (System.IO.File.Exists(notesPath))
                    notes = (await System.IO.File.ReadAllTextAsync(notesPath))?.Trim() ?? "";
            }
            catch { /* notes are best-effort context, never block the recap */ }

            var prompt = Helpers.MeetingRecapHelpers.BuildStructuredRecapPrompt(transcript, notes, meetingType);
            var modelOverride = MeetingWindow.PickRecapModelInternal();
            App.Log($"recap (shared) model='{modelOverride}' prompt {prompt.Length} chars dir='{dir}'",
                "MeetingRecap");

            var buf = new byte[1 << 18]; // 256 KB response buffer
            int rc = await Task.Run(() =>
                DimmyNative.dimmy_llm_call_raw(prompt, modelOverride, 16000, buf, buf.Length));

            if (rc <= 0)
            {
                var msg = RecapRcToUserMessage(rc, modelOverride);
                App.Log($"recap (shared) failed rc={rc}: {msg}", "MeetingRecap");
                DimmyNative.TrackEvent("meeting.recap_completed", new
                {
                    provider = TelemetryBuckets.Provider(ReadLlmApiUrl()),
                    recap_model_bucket = TelemetryBuckets.RecapModel(modelOverride),
                    processing_ms_bucket = TelemetryBuckets.ProcessingMs(stopwatch.ElapsedMilliseconds),
                    success = false,
                });
                return new RecapResult { Success = false, Dir = dir, Error = msg };
            }

            var raw = System.Text.Encoding.UTF8.GetString(buf, 0, rc);
            var sections = Helpers.MeetingRecapHelpers.ParseStructuredRecap(raw);
            var recapMarkdown = Helpers.MeetingRecapHelpers.BuildMarkdownFromSections(sections);
            var actionsPlain = sections.GetValueOrDefault("ACTIONS", "");
            // Resolved meeting type (categorical enum, privacy-safe): the
            // model-detected key in auto mode, else the forced pick.
            var resolvedType = sections.TryGetValue("__TYPE__", out var dt) && !string.IsNullOrWhiteSpace(dt)
                ? dt
                : (string.IsNullOrWhiteSpace(meetingType) ? "auto" : meetingType);

            // modelOverride is what actually ran; empty means "inherit /
            // pick best", which only the core resolves — pass null there
            // rather than guess, so the marker omits the field instead of
            // naming the wrong model. The core strips the cloud:/local:
            // prefix, so hosts pass the picker value verbatim.
            int saveRc = DimmyNative.dimmy_meeting_save_post_process(
                dir, recapMarkdown, actionsPlain, null,
                string.IsNullOrWhiteSpace(modelOverride) ? null : modelOverride);
            App.Log($"recap (shared) saved rc={saveRc}", "MeetingRecap");

            // Best-effort copy into the user's export folder (Obsidian /
            // Drive / Dropbox sync). No-op when unconfigured; never throws.
            RecapExportService.TryExport(dir, sections.GetValueOrDefault("__TITLE__", ""));

            // Recap-completed telemetry (success path). Bucketed
            // provider + model + processing time so dashboards can
            // see which combos are popular without leaking the user's
            // exact model id.
            DimmyNative.TrackEvent("meeting.recap_completed", new
            {
                provider = TelemetryBuckets.Provider(ReadLlmApiUrl()),
                recap_model_bucket = TelemetryBuckets.RecapModel(modelOverride),
                processing_ms_bucket = TelemetryBuckets.ProcessingMs(stopwatch.ElapsedMilliseconds),
                meeting_type = resolvedType,
                success = true,
            });

            // Auto-send to Notion if the user has it configured + opted in.
            // Best-effort: failure is logged but doesn't block the recap
            // pipeline. The user can always retry manually from the Done
            // view's "Notion" button. Reads notion_auto_send fresh from
            // the Rust core's config snapshot so we don't have to thread
            // a mutable VM reference through this service.
            if (Services.NotionService.HasToken() && ReadNotionAutoSend())
            {
                try
                {
                    var notionResult = await Services.NotionService.SendRecapAsync(dir);
                    if (notionResult.Ok)
                    {
                        App.Log($"recap auto-sent to Notion: {notionResult.PageUrl}", "Notion");
                    }
                    else
                    {
                        App.Log($"recap auto-send failed: {notionResult.Error}", "Notion");
                    }
                    DimmyNative.TrackEvent("notion.recap_sent", new
                    {
                        auto = true,
                        ok = notionResult.Ok,
                    });
                }
                catch (Exception ex)
                {
                    App.Log($"recap auto-send exc: {ex.Message}", "Notion");
                    DimmyNative.TrackEvent("notion.recap_sent", new { auto = true, ok = false });
                }
            }

            return new RecapResult { Success = true, Dir = dir, Sections = sections };
        }
        catch (Exception ex)
        {
            App.Log($"recap (shared) exc: {ex}", "MeetingRecap");
            return new RecapResult { Success = false, Dir = dir, Error = ex.Message };
        }
    }

    /// Read the notion_auto_send flag from the Rust core's current
    /// config snapshot. ~5 ms one-shot — only fires once per meeting,
    /// not hot-path. Returns false on any error (config not yet
    /// loaded, JSON parse failure, etc.) so a botched config doesn't
    /// silently auto-send.
    private static bool ReadNotionAutoSend()
    {
        try
        {
            var buf = new byte[1 << 14];
            int n = DimmyNative.dimmy_get_config_json(buf, buf.Length);
            if (n <= 0) return false;
            var json = System.Text.Encoding.UTF8.GetString(buf, 0, n);
            using var doc = System.Text.Json.JsonDocument.Parse(json);
            return doc.RootElement.TryGetProperty("notion_auto_send", out var el)
                && el.GetBoolean();
        }
        catch (Exception ex)
        {
            App.Log($"ReadNotionAutoSend exc: {ex.Message}", "Notion");
            return false;
        }
    }

    /// Translate a `dimmy_llm_call_raw` rc into a short user-facing
    /// message. Thin shim that delegates to
    /// <see cref="Helpers.MeetingRecapHelpers.RecapRcToUserMessage"/>
    /// so the pure-logic helper can live in a file the xUnit Tests
    /// project links directly (without dragging in App / FFI / XAML
    /// deps). Public so MeetingDoneView + MeetingPostProcessService
    /// + PillWindow all call the same source of truth.
    public static string RecapRcToUserMessage(int rc, string modelOverride)
        => Helpers.MeetingRecapHelpers.RecapRcToUserMessage(rc, modelOverride);

    /// Read `llm_api_url` from the Rust core config snapshot. Used
    /// only to derive a categorical provider tag for telemetry
    /// (see TelemetryBuckets.Provider). Returns empty on any
    /// error so the bucket falls to "unset".
    private static string ReadLlmApiUrl()
    {
        try
        {
            var buf = new byte[1 << 14];
            int n = DimmyNative.dimmy_get_config_json(buf, buf.Length);
            if (n <= 0) return "";
            var json = System.Text.Encoding.UTF8.GetString(buf, 0, n);
            using var doc = System.Text.Json.JsonDocument.Parse(json);
            if (doc.RootElement.TryGetProperty("llm_api_url", out var el))
                return el.GetString() ?? "";
            return "";
        }
        catch { return ""; }
    }
}
