using System;
using System.Collections.Generic;
using Microsoft.UI.Dispatching;

using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

/// <summary>
/// Host side of the Telegram audio inbox. The Rust worker
/// (core/src/telegram.rs) owns the account login + download; this service
/// only:
///   1. starts the worker on launch if the user had it enabled (dimmy_init
///      loads the flag but does not spawn the worker),
///   2. on each incoming voice note (telegram_pending) either auto-processes
///      it or shows a "transcribe + recap?" prompt (host-owned ask-vs-auto),
///   3. on download (telegram_audio) runs the SAME file-load transcribe +
///      recap pipeline the Settings "Transcribe a file" card uses, then
///      marks the message processed.
///
/// Event-driven (no polling): it subscribes to the AppViewModel Telegram
/// events, which the App-level FFI callback already marshals onto the UI
/// thread. One nudge window is reused; prompts are shown one at a time from
/// an internal queue so a burst of shares can't orphan earlier ones.
/// </summary>
public sealed class TelegramService : IDisposable
{
    private readonly DispatcherQueue _dispatcher;
    private readonly ViewModels.AppViewModel _vm;

    private Views.TelegramNudgeWindow? _nudge;
    private readonly Queue<PendingAudio> _queue = new();
    private bool _promptShowing;
    private bool _autoProcess;
    private bool _disposed;
    /// True while this service is the one driving the pill. Telegram work ran
    /// for minutes with the pill idle, which read as a frozen app.
    private bool _drivingPill;

    private const string StartedReply =
        "Dimmy got it. Transcribing now, I will reply here when the recap is ready.";

    private readonly record struct PendingAudio(int MsgId, string Filename, long Size);

    public TelegramService(DispatcherQueue dispatcher, ViewModels.AppViewModel vm)
    {
        _dispatcher = dispatcher ?? throw new ArgumentNullException(nameof(dispatcher));
        _vm = vm ?? throw new ArgumentNullException(nameof(vm));
        _vm.TelegramPendingAudio += OnPendingAudio;
        _vm.TelegramAudioReady += OnAudioReady;
        _vm.TelegramError += OnError;
    }

    /// <summary>Start the worker if the persisted config has Telegram
    /// enabled, and seed the ask-vs-auto flag. Safe no-op when disabled or
    /// when the build lacks the `telegram` cargo feature (set_enabled is a
    /// stub then).</summary>
    public void Start()
    {
        try
        {
            var cfg = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_config_json, 16384);
            bool enabled = false;
            if (!string.IsNullOrEmpty(cfg))
            {
                using var doc = System.Text.Json.JsonDocument.Parse(cfg);
                var root = doc.RootElement;
                enabled = root.TryGetProperty("telegram_enabled", out var te)
                          && te.ValueKind == System.Text.Json.JsonValueKind.True;
                _autoProcess = root.TryGetProperty("telegram_auto_process", out var ap)
                               && ap.ValueKind == System.Text.Json.JsonValueKind.True;
            }
            if (enabled)
                DimmyNative.dimmy_telegram_set_enabled(1);
            App.Log($"TelegramService start (enabled={enabled}, auto={_autoProcess})", "Telegram");
        }
        catch (Exception ex)
        {
            App.Log($"TelegramService.Start EXC: {ex.Message}", "Telegram");
        }
    }

    /// <summary>Keep the host-owned "auto-process" decision in sync when the
    /// user saves Settings.</summary>
    public void SetAutoProcess(bool on) => _autoProcess = on;

    private void OnPendingAudio(int msgId, string filename, long date, long size, bool backlog)
    {
        _dispatcher.TryEnqueue(() =>
        {
            try
            {
                if (_autoProcess)
                {
                    // Auto-process shows no prompt, so this was the only
                    // moment with nothing on screen: download plus
                    // transcription plus recap can run for many minutes.
                    DictNotificationService.ShowTelegramReceived(filename);
                    DimmyNative.dimmy_telegram_process(msgId);
                    DimmyNative.dimmy_telegram_reply(msgId, StartedReply);
                    return;
                }
                _queue.Enqueue(new PendingAudio(msgId, filename, size));
                ShowNextIfIdle();
            }
            catch (Exception ex)
            {
                App.Log($"Telegram OnPendingAudio EXC: {ex.Message}", "Telegram");
            }
        });
    }

    private void ShowNextIfIdle()
    {
        if (_promptShowing || _queue.Count == 0) return;
        var item = _queue.Dequeue();
        _promptShowing = true;
        EnsureNudge().ShowFor(item.MsgId, item.Filename, item.Size);
    }

    private Views.TelegramNudgeWindow EnsureNudge()
    {
        if (_nudge != null) return _nudge;
        _nudge = new Views.TelegramNudgeWindow();
        _nudge.AcceptRequested += id =>
        {
            try
            {
                DimmyNative.dimmy_telegram_process(id);
                DimmyNative.dimmy_telegram_reply(id, StartedReply);
            }
            catch (Exception ex) { App.Log($"Telegram accept EXC: {ex.Message}", "Telegram"); }
            _promptShowing = false;
            ShowNextIfIdle();
        };
        _nudge.DismissRequested += id =>
        {
            try { DimmyNative.dimmy_telegram_dismiss(id); }
            catch (Exception ex) { App.Log($"Telegram dismiss EXC: {ex.Message}", "Telegram"); }
            _promptShowing = false;
            ShowNextIfIdle();
        };
        return _nudge;
    }

    private void OnAudioReady(int msgId, string path, string filename)
    {
        // The worker downloaded the file; transcribe + recap off the UI
        // thread, reusing the exact file-load pipeline the Settings card uses.
        // dimmy_transcribe_file saves the transcript to History itself.
        BeginPillActivity(ViewModels.AppState.Transcribing);
        _ = System.Threading.Tasks.Task.Run(async () =>
        {
            try
            {
                var buf = new byte[1 << 22]; // 4 MB transcript buffer, matches SettingsWindow
                int rc = DimmyNative.dimmy_transcribe_file(path, buf, buf.Length);
                if (rc <= 0)
                {
                    App.Log($"Telegram transcribe rc={rc} file={filename}", "Telegram");
                    EndPillActivity();
                    DictNotificationService.ShowTelegramFailed(filename);
                    DimmyNative.dimmy_telegram_reply(msgId,
                        "Dimmy could not transcribe this one. It stays in the inbox, so sending it again is not needed.");
                    return; // leave unprocessed so the user can retry
                }

                var transcript = System.Text.Encoding.UTF8.GetString(buf, 0, rc);
                BeginPillActivity(ViewModels.AppState.Processing);
                var result = await FileLoadToMeetingService.RunAsync(path, transcript);

                // Transcript is saved to History regardless of recap outcome,
                // so mark the message done either way (retrying wouldn't help
                // if the recap failed on missing LLM config).
                DimmyNative.dimmy_telegram_mark_processed(msgId);
                EndPillActivity();

                if (result.Success)
                {
                    DictNotificationService.ShowTelegramRecapReady(filename);
                    DimmyNative.dimmy_telegram_reply(msgId,
                        RecapReply(result.Dir) ?? "Transcribed and recapped. The recap is in Dimmy.");
                    App.Log($"Telegram audio processed: {filename} ({rc} chars)", "Telegram");
                }
                else
                {
                    App.Log($"Telegram recap failed: {result.Error}", "Telegram");
                    DictNotificationService.ShowTelegramTranscribedNoRecap(filename);
                    DimmyNative.dimmy_telegram_reply(msgId,
                        "Transcribed and saved to Dimmy's history. No recap was produced: check the recap model in Settings.");
                }
            }
            catch (Exception ex)
            {
                App.Log($"Telegram OnAudioReady EXC: {ex.Message}", "Telegram");
                EndPillActivity();
                DictNotificationService.ShowTelegramFailed(filename);
            }
        });
    }

    private void OnError(string message)
    {
        App.Log($"Telegram worker error: {message}", "Telegram");
    }

    /// <summary>Light up the pill, but never take it from a dictation or a
    /// meeting: those own the same state and their stop paths would then be
    /// fighting this one.</summary>
    private void BeginPillActivity(ViewModels.AppState state)
    {
        _dispatcher.TryEnqueue(() =>
        {
            if (!_drivingPill && _vm.CurrentState != ViewModels.AppState.Idle) return;
            _drivingPill = true;
            _vm.SetState(state);
        });
    }

    /// <summary>Back to idle, unless something else took the pill over.</summary>
    private void EndPillActivity()
    {
        _dispatcher.TryEnqueue(() =>
        {
            if (!_drivingPill) return;
            _drivingPill = false;
            if (_vm.CurrentState is ViewModels.AppState.Transcribing or ViewModels.AppState.Processing)
                _vm.SetState(ViewModels.AppState.Idle);
        });
    }

    /// <summary>The finished-work reply, from the recap.md the pipeline just
    /// wrote. Null when the file is missing or has nothing worth quoting.</summary>
    private static string? RecapReply(string dir)
    {
        if (string.IsNullOrEmpty(dir)) return null;
        try
        {
            var file = System.IO.Path.Combine(dir, "recap.md");
            if (!System.IO.File.Exists(file)) return null;
            return Helpers.TelegramReplyText.FromRecapMarkdown(System.IO.File.ReadAllText(file));
        }
        catch { return null; }
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        try
        {
            _vm.TelegramPendingAudio -= OnPendingAudio;
            _vm.TelegramAudioReady -= OnAudioReady;
            _vm.TelegramError -= OnError;
        }
        catch { }
        try { _nudge?.Close(); } catch { }
    }
}
