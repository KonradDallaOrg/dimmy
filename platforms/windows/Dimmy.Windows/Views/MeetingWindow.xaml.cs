using System;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Text.Json;
using System.Threading.Tasks;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Views;

/// Dedicated meeting-mode UI. Distinct from the dictation pill so the
/// user can leave it open, glance at the live transcript, and grab
/// the recap + actions when the meeting wraps. State is kept on the
/// Rust side (MEETING static) — this window is just the front-end.
public sealed partial class MeetingWindow : Window
{
    private DispatcherQueueTimer? _pollTimer;
    private DateTime _startedAt;
    private string? _activeMeetingDir;

    public MeetingWindow()
    {
        App.Log("ctor enter", "Meeting");
        this.InitializeComponent();
        Title = "Dimmy — Meeting";
        try
        {
            var appWindow = WindowHelper.GetAppWindow(this);
            WindowHelper.ResizeLogical(this, 720, 640);
            if (appWindow?.Presenter is OverlappedPresenter presenter)
            {
                presenter.IsResizable = true;
                presenter.IsMaximizable = true;
                presenter.Restore(); // ensure not minimized
            }
            // Centre on the primary display so the window doesn't open
            // off-screen (a known WinUI 3 quirk on multi-monitor setups
            // that have a non-primary monitor with negative coords).
            if (appWindow != null)
            {
                var da = Microsoft.UI.Windowing.DisplayArea.GetFromWindowId(
                    appWindow.Id, Microsoft.UI.Windowing.DisplayAreaFallback.Primary);
                if (da != null)
                {
                    int w = appWindow.Size.Width;
                    int h = appWindow.Size.Height;
                    int x = da.WorkArea.X + (da.WorkArea.Width - w) / 2;
                    int y = da.WorkArea.Y + (da.WorkArea.Height - h) / 2;
                    appWindow.Move(new global::Windows.Graphics.PointInt32(x, y));
                }
            }
            App.Log($"ctor done — appWindow={(appWindow != null)}", "Meeting");
        }
        catch (Exception ex)
        {
            App.Log($"ctor EXC: {ex}", "Meeting");
        }
        Closed += (_, __) => StopPolling();
    }

    // ── Lifecycle ─────────────────────────────────────────────────

    private async void Start_Click(object sender, RoutedEventArgs e)
    {
        StartBtn.IsEnabled = false;
        StatusText.Text = "Starting…";
        try
        {
            var buf = new byte[256];
            int rc = DimmyNative.dimmy_meeting_start(buf, buf.Length);
            if (rc <= 0)
            {
                StatusText.Text = $"Start failed (code {rc})";
                StartBtn.IsEnabled = true;
                return;
            }
            var id = System.Text.Encoding.UTF8.GetString(buf, 0, rc);
            _startedAt = DateTime.UtcNow;
            StatusDot.Fill = new SolidColorBrush(Microsoft.UI.Colors.Crimson);
            StatusText.Text = $"Recording (id {id[..8]}…)";
            StopBtn.IsEnabled = true;
            TranscriptText.Text = "";
            RecapBorder.Visibility = Visibility.Collapsed;
            ActionsBorder.Visibility = Visibility.Collapsed;
            StartPolling();
            App.Log($"meeting started: {id}", "Meeting");
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Error: {ex.Message}";
            StartBtn.IsEnabled = true;
            App.Log($"meeting start exc: {ex}", "Meeting");
        }
        await Task.CompletedTask;
    }

    private async void Stop_Click(object sender, RoutedEventArgs e)
    {
        StopBtn.IsEnabled = false;
        StopPolling();
        StatusText.Text = "Stopping & finalizing…";
        try
        {
            var buf = new byte[1 << 22]; // 4 MB — supports very long transcripts
            int rc = await Task.Run(() => DimmyNative.dimmy_meeting_stop(buf, buf.Length));
            if (rc <= 0)
            {
                StatusText.Text = $"Stop failed (code {rc})";
                StartBtn.IsEnabled = true;
                return;
            }
            var json = System.Text.Encoding.UTF8.GetString(buf, 0, rc);
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;
            var dir = root.GetProperty("dir").GetString() ?? "";
            var transcript = root.GetProperty("transcript").GetString() ?? "";
            var dur = root.GetProperty("duration_secs").GetDouble();
            var chunks = root.GetProperty("chunk_count").GetInt32();
            var err = root.TryGetProperty("error", out var errEl) && errEl.ValueKind == JsonValueKind.String
                ? errEl.GetString() : null;
            _activeMeetingDir = dir;
            DirText.Text = dir;
            OpenFolderBtn.IsEnabled = !string.IsNullOrEmpty(dir);
            TranscriptText.Text = string.IsNullOrEmpty(transcript)
                ? "(no transcript — VAD may have removed all audio)"
                : transcript;
            StatusDot.Fill = new SolidColorBrush(Microsoft.UI.Colors.SeaGreen);
            StatusText.Text = $"Done · {dur:F0}s · {chunks} chunks" + (err != null ? $" · err: {err}" : "");

            // Post-process LLM (recap + actions) if the user opted in.
            if (GenerateRecapCheck.IsChecked == true && !string.IsNullOrWhiteSpace(transcript))
            {
                await GeneratePostProcessAsync(dir, transcript);
            }
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Error: {ex.Message}";
            App.Log($"meeting stop exc: {ex}", "Meeting");
        }
        finally
        {
            StartBtn.IsEnabled = true;
        }
    }

    // ── Live transcript polling ───────────────────────────────────

    /// While a meeting is active, poll transcripts.txt every 2 s and
    /// reflect its content in the UI. Cheap (file is monotonically
    /// appended), gives us live captions without piping every chunk
    /// through a Rust event channel.
    private void StartPolling()
    {
        var dq = DispatcherQueue.GetForCurrentThread();
        _pollTimer = dq.CreateTimer();
        _pollTimer.Interval = TimeSpan.FromSeconds(2);
        _pollTimer.IsRepeating = true;
        _pollTimer.Tick += OnPollTick;
        _pollTimer.Start();
    }

    private void StopPolling()
    {
        if (_pollTimer == null) return;
        _pollTimer.Stop();
        _pollTimer.Tick -= OnPollTick;
        _pollTimer = null;
    }

    private void OnPollTick(DispatcherQueueTimer sender, object args)
    {
        // Update timer text.
        var elapsed = DateTime.UtcNow - _startedAt;
        TimerText.Text = $"{(int)elapsed.TotalHours:D2}:{elapsed.Minutes:D2}:{elapsed.Seconds:D2}";

        // Locate the live transcripts.txt — we don't know the exact
        // dir yet (Rust owns it) but we can scan the meetings dir for
        // the most-recently-modified one. Cheap because we only do
        // this once every 2 s.
        try
        {
            var meetings = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "dimmy", "meetings");
            if (!Directory.Exists(meetings)) return;
            var latest = new DirectoryInfo(meetings).GetDirectories()
                .OrderByDescending(d => d.LastWriteTime)
                .FirstOrDefault();
            if (latest == null) return;
            _activeMeetingDir = latest.FullName;
            DirText.Text = latest.FullName;
            var transcriptsPath = Path.Combine(latest.FullName, "transcripts.txt");
            if (!File.Exists(transcriptsPath)) return;
            var content = File.ReadAllText(transcriptsPath);
            TranscriptText.Text = content;
            ChunkCountText.Text = $"{content.Split('\n').Length} chunks";
        }
        catch { /* best-effort live view */ }
    }

    // ── Post-process pipeline (LLM recap + actions) ──────────────

    private async Task GeneratePostProcessAsync(string dir, string transcript)
    {
        StatusText.Text = "Generating recap + actions via LLM…";
        try
        {
            // Single-call structured output: prompt the LLM to return
            // a tagged response with sections, then split client-side.
            // Avoids two round-trips for what's conceptually one task.
            var prompt = BuildPostProcessPrompt(transcript);
            var buf = new byte[1 << 18];
            int rc = await Task.Run(() => DimmyNative.dimmy_process_with_llm(prompt, buf, buf.Length));
            if (rc <= 0)
            {
                StatusText.Text = $"LLM call returned {rc} — see dimmy.log";
                return;
            }
            var raw = System.Text.Encoding.UTF8.GetString(buf, 0, rc);
            var (recap, actions) = ParsePostProcessResponse(raw);

            RecapText.Text = string.IsNullOrEmpty(recap) ? "(LLM produced no recap)" : recap;
            ActionsText.Text = string.IsNullOrEmpty(actions) ? "(no action items detected)" : actions;
            RecapBorder.Visibility = Visibility.Visible;
            ActionsBorder.Visibility = Visibility.Visible;

            DimmyNative.dimmy_meeting_save_post_process(dir, recap, actions, null);
            StatusText.Text = "Recap + actions saved";
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Post-process failed: {ex.Message}";
            App.Log($"post-process exc: {ex}", "Meeting");
        }
    }

    private static string BuildPostProcessPrompt(string transcript)
    {
        return
            "You are summarizing a meeting transcript. Output ONLY the two sections below, " +
            "separated by the exact markers shown.\n\n" +
            "===RECAP===\n" +
            "<3-6 bullet points covering decisions made, topics discussed, and outcomes>\n\n" +
            "===ACTIONS===\n" +
            "<numbered list of action items in the form: N. owner — task — due (or 'unspecified')>\n\n" +
            "Transcript:\n" + transcript;
    }

    private static (string recap, string actions) ParsePostProcessResponse(string raw)
    {
        var recapMarker = "===RECAP===";
        var actionsMarker = "===ACTIONS===";
        int rIdx = raw.IndexOf(recapMarker, StringComparison.OrdinalIgnoreCase);
        int aIdx = raw.IndexOf(actionsMarker, StringComparison.OrdinalIgnoreCase);
        if (rIdx < 0 || aIdx < 0 || aIdx <= rIdx)
        {
            // Best-effort fallback: no markers → entire output goes
            // into recap, actions stays empty so the UI shows "no
            // action items detected".
            return (raw.Trim(), "");
        }
        var recap = raw.Substring(rIdx + recapMarker.Length, aIdx - (rIdx + recapMarker.Length)).Trim();
        var actions = raw.Substring(aIdx + actionsMarker.Length).Trim();
        return (recap, actions);
    }

    private void OpenFolder_Click(object sender, RoutedEventArgs e)
    {
        if (string.IsNullOrEmpty(_activeMeetingDir)) return;
        try { Process.Start(new ProcessStartInfo { FileName = _activeMeetingDir, UseShellExecute = true }); }
        catch (Exception ex) { App.Log($"open folder failed: {ex.Message}", "Meeting"); }
    }
}
