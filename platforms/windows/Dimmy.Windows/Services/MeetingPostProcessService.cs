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
/// Helper methods (BuildStructuredRecapPrompt / ParseStructuredRecap /
/// BuildMarkdownFromSections / PickRecapModel) live as `internal
/// static` on MeetingWindow so the prompt + parser stay in lockstep
/// with the UI rendering.
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
    public static async Task<RecapResult> RunRecapAsync(string dir, string transcript)
    {
        if (string.IsNullOrEmpty(dir))
            return new RecapResult { Success = false, Error = "missing dir" };
        if (string.IsNullOrWhiteSpace(transcript))
            return new RecapResult { Success = false, Dir = dir, Error = "empty transcript" };

        try
        {
            var prompt = MeetingWindow.BuildStructuredRecapPromptInternal(transcript);
            var modelOverride = MeetingWindow.PickRecapModelInternal();
            App.Log($"recap (shared) model='{modelOverride}' prompt {prompt.Length} chars dir='{dir}'",
                "MeetingRecap");

            var buf = new byte[1 << 18]; // 256 KB response buffer
            int rc = await Task.Run(() =>
                DimmyNative.dimmy_llm_call_raw(prompt, modelOverride, 16000, buf, buf.Length));

            if (rc <= 0)
            {
                var msg = rc switch
                {
                    -2 => "Configure an LLM API key + URL first.",
                    -3 => "LLM HTTP call failed (see dimmy.log).",
                    _ => $"LLM call returned {rc}",
                };
                App.Log($"recap (shared) failed rc={rc}: {msg}", "MeetingRecap");
                return new RecapResult { Success = false, Dir = dir, Error = msg };
            }

            var raw = System.Text.Encoding.UTF8.GetString(buf, 0, rc);
            var sections = MeetingWindow.ParseStructuredRecapInternal(raw);
            var recapMarkdown = MeetingWindow.BuildMarkdownFromSectionsInternal(sections);
            var actionsPlain = sections.GetValueOrDefault("ACTIONS", "");

            int saveRc = DimmyNative.dimmy_meeting_save_post_process(
                dir, recapMarkdown, actionsPlain, null);
            App.Log($"recap (shared) saved rc={saveRc}", "MeetingRecap");

            return new RecapResult { Success = true, Dir = dir, Sections = sections };
        }
        catch (Exception ex)
        {
            App.Log($"recap (shared) exc: {ex}", "MeetingRecap");
            return new RecapResult { Success = false, Dir = dir, Error = ex.Message };
        }
    }
}
