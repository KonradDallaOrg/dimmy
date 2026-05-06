using System;
using System.Text;
using System.Threading.Tasks;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

/// <summary>
/// Handles the stop-recording → transcribe → LLM enhance pipeline.
/// Used by both the hotkey handler (App.xaml.cs) and the pill stop button (PillWindow).
/// </summary>
public static class TranscriptionService
{
    private const int BufSize = 65536;
    private static readonly TimeSpan TranscribeTimeout = TimeSpan.FromSeconds(30);
    private static readonly TimeSpan LlmTimeout = TimeSpan.FromSeconds(30);

    /// <summary>
    /// Stop recording, transcribe, optionally run LLM enhancement, and return the final text.
    /// Returns null if transcription is empty or timed out.
    /// Throws on unexpected errors.
    /// </summary>
    public static async Task<TranscriptionResult> StopAndProcessAsync()
    {
        // Step 1: Stop recording + transcribe (blocking FFI, run on thread pool)
        var transcribeTask = Task.Run(() =>
        {
            var buf = new byte[BufSize];
            int len = DimmyNative.dimmy_stop_recording(buf, buf.Length);
            return len > 0 ? Encoding.UTF8.GetString(buf, 0, len) : null;
        });

        var completed = await Task.WhenAny(transcribeTask, Task.Delay(TranscribeTimeout));
        if (completed != transcribeTask)
            return TranscriptionResult.Timeout("Transcription timed out (30s)");

        var transcript = await transcribeTask;
        if (string.IsNullOrEmpty(transcript))
            return TranscriptionResult.Empty();

        // Step 2: LLM enhancement (if enabled — the Rust side checks llm_enabled + style)
        var llmTask = Task.Run(() =>
        {
            var buf = new byte[BufSize];
            int len = DimmyNative.dimmy_process_with_llm(transcript, buf, buf.Length);
            return len > 0 ? Encoding.UTF8.GetString(buf, 0, len) : null;
        });

        var llmCompleted = await Task.WhenAny(llmTask, Task.Delay(LlmTimeout));
        if (llmCompleted != llmTask)
        {
            // LLM timed out → use raw transcript (graceful degradation)
            System.Diagnostics.Debug.WriteLine("[Dimmy] LLM timeout, using raw transcript");
            return TranscriptionResult.Success(transcript);
        }

        var enhanced = await llmTask;
        // If LLM returned empty or failed, fall back to raw transcript
        var finalText = string.IsNullOrEmpty(enhanced) ? transcript : enhanced;

        // Persist the enhanced text on the most recent history row.
        // dimmy_stop_recording already saved the raw transcript; we
        // don't have its id back, so we ask Rust for the most recent
        // row and update it. Race-free because we're synchronous from
        // hotkey-release through here, and every recording produces
        // exactly one save_v2 call.
        if (!string.IsNullOrEmpty(enhanced) && enhanced != transcript)
        {
            try
            {
                var listBuf = new byte[8192];
                int listLen = DimmyNative.dimmy_history_recent(1, listBuf, listBuf.Length);
                if (listLen > 0)
                {
                    var listJson = Encoding.UTF8.GetString(listBuf, 0, listLen);
                    using var doc = System.Text.Json.JsonDocument.Parse(listJson);
                    if (doc.RootElement.GetArrayLength() > 0)
                    {
                        var topId = doc.RootElement[0].GetProperty("id").GetInt64();
                        DimmyNative.dimmy_history_update_enhanced((int)topId, enhanced);
                    }
                }
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"[Dimmy] update_enhanced failed: {ex.Message}");
            }
        }

        return TranscriptionResult.Success(finalText);
    }
}

/// <summary>
/// Result of the transcription + LLM pipeline.
/// </summary>
public sealed class TranscriptionResult
{
    public string? Text { get; private init; }
    public string? Error { get; private init; }
    public bool IsSuccess => Text != null && Error == null;
    public bool IsEmpty => Text == null && Error == null;
    public bool IsTimeout => Error != null;

    public static TranscriptionResult Success(string text) => new() { Text = text };
    public static TranscriptionResult Empty() => new();
    public static TranscriptionResult Timeout(string message) => new() { Error = message };
}
