using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
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
/// Rust side (MEETING static) — this window is the front-end with a
/// 4-state machine (Idle / Recording / Processing / Done).
///
/// Layout mirrors the standalone HTML mockup at
/// docs/dev/refs/meeting-standalone.html.
public sealed partial class MeetingWindow : Window
{
    private enum MeetingState { Idle, Recording, Processing, Done }

    private DispatcherQueueTimer? _pollTimer;
    private DispatcherQueueTimer? _ampTimer;
    private DateTime _startedAt;
    private string? _activeMeetingDir;
    private long _lastTranscriptLen = -1;
    private MeetingState _state = MeetingState.Idle;

    // Live waveform rolling buffer (40 samples = ~3 s at 12 Hz poll).
    private readonly Queue<float> _ampHistory = new();
    private const int AMP_HISTORY = 40;

    public ObservableCollection<HistoryRow> HistoryItems { get; } = new();

    public MeetingWindow()
    {
        App.Log("ctor enter", "Meeting");
        this.InitializeComponent();
        Title = "Dimmy Meeting";
        try
        {
            var appWindow = WindowHelper.GetAppWindow(this);
            WindowHelper.ResizeLogical(this, 800, 720);
            if (appWindow?.Presenter is OverlappedPresenter presenter)
            {
                presenter.IsResizable = true;
                presenter.IsMaximizable = true;
                presenter.Restore();
            }
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

        HistoryList.ItemsSource = HistoryItems;
        Closed += (_, __) =>
        {
            StopPolling();
            StopAmplitudePoll();
        };
    }

    // ── State machine ──────────────────────────────────────────────

    private void SetState(MeetingState s)
    {
        _state = s;
        IdlePanel.Visibility = s == MeetingState.Idle ? Visibility.Visible : Visibility.Collapsed;
        RecordingPanel.Visibility = s == MeetingState.Recording ? Visibility.Visible : Visibility.Collapsed;
        ProcessingPanel.Visibility = s == MeetingState.Processing ? Visibility.Visible : Visibility.Collapsed;
        DonePanel.Visibility = s == MeetingState.Done ? Visibility.Visible : Visibility.Collapsed;

        TitlebarHud.Visibility = s == MeetingState.Recording || s == MeetingState.Processing
            ? Visibility.Visible : Visibility.Collapsed;
        StopBtn.Visibility = s == MeetingState.Recording ? Visibility.Visible : Visibility.Collapsed;
        StopBtn.IsEnabled = s == MeetingState.Recording;
        TitlebarSubText.Text = s switch
        {
            MeetingState.Idle => "Capture, transcribe, recap",
            MeetingState.Recording => "Recording — talk freely, transcript builds in the background",
            MeetingState.Processing => "Saving + generating recap…",
            MeetingState.Done => "Recap ready",
            _ => "",
        };
    }

    // ── Tabs ───────────────────────────────────────────────────────

    private void TabLive_Click(object sender, RoutedEventArgs e)
    {
        TabLive.IsChecked = true;
        TabHistory.IsChecked = false;
        TabLive.BorderBrush = (Brush)Application.Current.Resources["AccentFillColorDefaultBrush"];
        TabHistory.BorderBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        LiveTab.Visibility = Visibility.Visible;
        HistoryTab.Visibility = Visibility.Collapsed;
    }

    private void TabHistory_Click(object sender, RoutedEventArgs e)
    {
        TabLive.IsChecked = false;
        TabHistory.IsChecked = true;
        TabHistory.BorderBrush = (Brush)Application.Current.Resources["AccentFillColorDefaultBrush"];
        TabLive.BorderBrush = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        LiveTab.Visibility = Visibility.Collapsed;
        HistoryTab.Visibility = Visibility.Visible;
        LoadHistory();
    }

    // ── Lifecycle ─────────────────────────────────────────────────

    private async void Start_Click(object sender, RoutedEventArgs e)
    {
        StartBtn.IsEnabled = false;
        try
        {
            var buf = new byte[256];
            int rc = DimmyNative.dimmy_meeting_start(buf, buf.Length);
            if (rc <= 0)
            {
                App.Log($"meeting start failed rc={rc}", "Meeting");
                StartBtn.IsEnabled = true;
                return;
            }
            var id = System.Text.Encoding.UTF8.GetString(buf, 0, rc);
            _startedAt = DateTime.UtcNow;
            _lastTranscriptLen = -1;
            _ampHistory.Clear();

            // Capture foreground app context BEFORE switching state so
            // user sees what the meeting "belongs to". The capture is
            // best-effort; missing data falls back to the FontIcon
            // placeholder.
            var fg = Helpers.AppContextCapture.SnapshotForeground();
            if (!fg.IsEmpty)
            {
                AppContextName.Text = fg.ProcessName;
                if (!string.IsNullOrEmpty(fg.ExecutablePath))
                {
                    Helpers.IconExtractor.EnsureCachedFromExePath(fg.ExecutablePath);
                    var iconUri = Helpers.IconExtractor.TryGetCachedUri(fg.ProcessName);
                    if (!string.IsNullOrEmpty(iconUri))
                    {
                        AppContextIcon.Source = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage(new Uri(iconUri));
                        AppContextIcon.Visibility = Visibility.Visible;
                        AppContextFallback.Visibility = Visibility.Collapsed;
                    }
                }
            }
            else
            {
                AppContextName.Text = "(no foreground app)";
            }

            SetState(MeetingState.Recording);
            TranscriptText.Text = "🎙️ Listening… first transcript appears in ~15 s.";
            TranscriptText.Foreground = (Brush)
                Application.Current.Resources["TextFillColorTertiaryBrush"];
            StartPolling();
            StartAmplitudePoll();
            App.Log($"meeting started: {id}", "Meeting");
        }
        catch (Exception ex)
        {
            App.Log($"meeting start exc: {ex}", "Meeting");
            StartBtn.IsEnabled = true;
        }
        await Task.CompletedTask;
    }

    private async void Stop_Click(object sender, RoutedEventArgs e)
    {
        StopBtn.IsEnabled = false;
        StopPolling();
        StopAmplitudePoll();
        SetState(MeetingState.Processing);
        ResetProcSteps();
        SetProcStep(1, true);  // saving step done — stop is synchronous
        try
        {
            var buf = new byte[1 << 22]; // 4 MB — supports very long transcripts
            int rc = await Task.Run(() => DimmyNative.dimmy_meeting_stop(buf, buf.Length));
            if (rc <= 0)
            {
                App.Log($"meeting stop failed rc={rc}", "Meeting");
                SetState(MeetingState.Idle);
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
            _activeMeetingDir = dir;

            DoneTitle.Text = string.IsNullOrEmpty(dir) ? "Meeting" : Path.GetFileName(dir);
            DoneMeta.Text = $"{FormatDuration(dur)} · {chunks} chunks · {DateTime.Now:yyyy-MM-dd HH:mm}";
            RawTranscriptText.Text = string.IsNullOrEmpty(transcript)
                ? "(no transcript: VAD may have removed all audio)"
                : transcript;

            // Show audio waveform card if audio.wav exists
            await LoadDoneAudioAsync(dir);

            if (GenerateRecapCheck.IsChecked == true && !string.IsNullOrWhiteSpace(transcript))
            {
                SetProcStep(2, false); // active
                await GeneratePostProcessAsync(dir, transcript);
                SetProcStep(2, true);
                SetProcStep(3, true);
            }
            else
            {
                SetProcStep(2, true);
                SetProcStep(3, true);
            }

            SetState(MeetingState.Done);
        }
        catch (Exception ex)
        {
            App.Log($"meeting stop exc: {ex}", "Meeting");
            SetState(MeetingState.Idle);
        }
        finally
        {
            StartBtn.IsEnabled = true;
        }
    }

    private void NewMeeting_Click(object sender, RoutedEventArgs e)
    {
        // Reset to Idle for a fresh meeting. Don't touch _activeMeetingDir
        // from the previous session — the Done view remains accessible
        // via the History tab.
        SetState(MeetingState.Idle);
        StartBtn.IsEnabled = true;
        TranscriptText.Text = "🎙️ Listening… first transcript appears in ~15 s.";
        ClearDoneCards();
    }

    private void ClearDoneCards()
    {
        TldrCard.Visibility = Visibility.Collapsed;
        DecisionsCard.Visibility = Visibility.Collapsed;
        TopicsCard.Visibility = Visibility.Collapsed;
        ActionsCard.Visibility = Visibility.Collapsed;
        OpenQuestionsCard.Visibility = Visibility.Collapsed;
        RisksCard.Visibility = Visibility.Collapsed;
        NextStepsCard.Visibility = Visibility.Collapsed;
        DoneWaveCard.Visibility = Visibility.Collapsed;
    }

    // ── Live transcript polling ───────────────────────────────────

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
        var elapsed = DateTime.UtcNow - _startedAt;
        var t = $"{(int)elapsed.TotalHours:D2}:{elapsed.Minutes:D2}:{elapsed.Seconds:D2}";
        RecTimer.Text = t;
        HudTimer.Text = t;

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
            var transcriptsPath = Path.Combine(latest.FullName, "transcripts.txt");
            if (!File.Exists(transcriptsPath)) return;
            var fi = new FileInfo(transcriptsPath);
            if (fi.Length == _lastTranscriptLen) return;
            _lastTranscriptLen = fi.Length;

            string content;
            using (var fs = new FileStream(transcriptsPath, FileMode.Open,
                FileAccess.Read, FileShare.ReadWrite))
            using (var sr = new StreamReader(fs))
            {
                content = sr.ReadToEnd();
            }
            if (string.IsNullOrWhiteSpace(content)) return;

            App.Log($"poll: {fi.Length} bytes from {latest.Name[..8]}", "Meeting");
            TranscriptText.Foreground = (Brush)
                Application.Current.Resources["TextFillColorPrimaryBrush"];
            TranscriptText.Text = content;
            var nChunks = content.Split('\n', StringSplitOptions.RemoveEmptyEntries).Length;
            RecChunks.Text = $"{nChunks} chunks";
            HudChunks.Text = $"{nChunks} chunks";
            TranscriptScroll?.ChangeView(null, double.MaxValue, null, true);
        }
        catch (Exception ex)
        {
            App.Log($"poll exc: {ex.Message}", "Meeting");
        }
    }

    // ── Live waveform amplitude poll ──────────────────────────────

    private void StartAmplitudePoll()
    {
        var dq = DispatcherQueue.GetForCurrentThread();
        _ampTimer = dq.CreateTimer();
        _ampTimer.Interval = TimeSpan.FromMilliseconds(83); // ~12 Hz
        _ampTimer.IsRepeating = true;
        _ampTimer.Tick += OnAmpTick;
        _ampTimer.Start();
    }

    private void StopAmplitudePoll()
    {
        if (_ampTimer == null) return;
        _ampTimer.Stop();
        _ampTimer.Tick -= OnAmpTick;
        _ampTimer = null;
    }

    private void OnAmpTick(DispatcherQueueTimer sender, object args)
    {
        try
        {
            float amp = DimmyNative.dimmy_get_amplitude();
            if (!float.IsFinite(amp)) amp = 0;
            // Soft compression so quiet voices still show movement.
            amp = (float)Math.Min(1.0, Math.Sqrt(amp) * 1.4);
            if (_ampHistory.Count >= AMP_HISTORY) _ampHistory.Dequeue();
            _ampHistory.Enqueue(amp);
            DrawLiveWaveform();
        }
        catch { /* amplitude is best-effort */ }
    }

    private void DrawLiveWaveform()
    {
        if (LiveWaveformCanvas == null || _ampHistory.Count == 0) return;
        LiveWaveformCanvas.Children.Clear();
        double w = LiveWaveformCanvas.ActualWidth;
        double h = LiveWaveformCanvas.ActualHeight;
        if (w <= 0 || h <= 0) return;
        var samples = _ampHistory.ToArray();
        double bar = w / AMP_HISTORY;
        double mid = h / 2.0;
        var brush = new SolidColorBrush(Microsoft.UI.Colors.Crimson);
        for (int i = 0; i < samples.Length; i++)
        {
            double height = Math.Max(2, samples[i] * (h - 4));
            var rect = new Microsoft.UI.Xaml.Shapes.Rectangle
            {
                Width = Math.Max(1, bar - 2),
                Height = height,
                Fill = brush,
                RadiusX = 1, RadiusY = 1,
            };
            Microsoft.UI.Xaml.Controls.Canvas.SetLeft(rect, i * bar);
            Microsoft.UI.Xaml.Controls.Canvas.SetTop(rect, mid - height / 2.0);
            LiveWaveformCanvas.Children.Add(rect);
        }
    }

    // ── Done audio waveform card ──────────────────────────────────

    private async Task LoadDoneAudioAsync(string dir)
    {
        if (string.IsNullOrEmpty(dir)) return;
        var wavPath = Path.Combine(dir, "audio.wav");
        if (!File.Exists(wavPath)) return;
        try
        {
            DoneWaveCard.Visibility = Visibility.Visible;
            DoneAudioPlayer.Source = global::Windows.Media.Core.MediaSource.CreateFromUri(new Uri(wavPath));

            double width = DoneWaveformCanvas.ActualWidth;
            if (width <= 0) width = 700;
            int buckets = (int)Math.Max(80, Math.Min(500, width / 3));
            var peaks = await Task.Run(() => Helpers.WavPeaks.ReadPeaks(wavPath, buckets));
            if (peaks.Length > 0) DrawDoneWaveform(peaks);
        }
        catch (Exception ex)
        {
            App.Log($"LoadDoneAudio exc: {ex.Message}", "Meeting");
        }
    }

    private void DrawDoneWaveform(float[] peaks)
    {
        if (DoneWaveformCanvas == null || peaks.Length == 0) return;
        DoneWaveformCanvas.Children.Clear();
        double w = DoneWaveformCanvas.ActualWidth;
        double h = DoneWaveformCanvas.ActualHeight;
        if (w <= 0 || h <= 0) return;
        double barW = Math.Max(1, w / peaks.Length - 1);
        double mid = h / 2.0;
        var brush = new SolidColorBrush(Microsoft.UI.Colors.DodgerBlue);
        for (int i = 0; i < peaks.Length; i++)
        {
            double bh = Math.Max(1, peaks[i] * (h - 2));
            var rect = new Microsoft.UI.Xaml.Shapes.Rectangle
            {
                Width = barW, Height = bh, Fill = brush, RadiusX = 1, RadiusY = 1,
            };
            Microsoft.UI.Xaml.Controls.Canvas.SetLeft(rect, i * (w / peaks.Length));
            Microsoft.UI.Xaml.Controls.Canvas.SetTop(rect, mid - bh / 2.0);
            DoneWaveformCanvas.Children.Add(rect);
        }
    }

    private void DoneWaveform_PointerPressed(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        try
        {
            var pt = e.GetCurrentPoint(DoneWaveformCanvas).Position;
            double w = DoneWaveformCanvas.ActualWidth;
            if (w <= 0) return;
            double frac = Math.Max(0, Math.Min(1, pt.X / w));
            var session = DoneAudioPlayer?.MediaPlayer?.PlaybackSession;
            if (session == null) return;
            var total = session.NaturalDuration;
            if (total.TotalSeconds <= 0) return;
            session.Position = TimeSpan.FromSeconds(total.TotalSeconds * frac);
        }
        catch { }
    }

    // ── Processing step indicators ────────────────────────────────

    private void ResetProcSteps()
    {
        SetProcStep(1, false);
        SetProcStep(2, false);
        SetProcStep(3, false);
    }

    private void SetProcStep(int n, bool done)
    {
        var (icon, _) = n switch
        {
            1 => (ProcStep1Icon, ProcStep1Text),
            2 => (ProcStep2Icon, ProcStep2Text),
            3 => (ProcStep3Icon, ProcStep3Text),
            _ => (null!, null!),
        };
        if (icon == null) return;
        icon.Glyph = done ? "" : "";
        icon.Foreground = done
            ? new SolidColorBrush(Microsoft.UI.Colors.ForestGreen)
            : (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"];
    }

    // ── Post-process pipeline (LLM recap + actions) ──────────────

    private async Task GeneratePostProcessAsync(string dir, string transcript)
    {
        try
        {
            var prompt = BuildStructuredRecapPrompt(transcript);
            var modelOverride = PickRecapModel();
            App.Log($"recap with model='{modelOverride}', prompt {prompt.Length} chars", "Meeting");
            var buf = new byte[1 << 18];
            int rc = await Task.Run(() =>
                DimmyNative.dimmy_llm_call_raw(prompt, modelOverride, 4096, buf, buf.Length));
            if (rc <= 0)
            {
                var msg = rc switch
                {
                    -2 => "Configure an LLM API key + URL first.",
                    -3 => "LLM HTTP call failed (see dimmy.log).",
                    _ => $"LLM call returned {rc}",
                };
                ShowDoneFallback(transcript, msg);
                return;
            }
            var raw = System.Text.Encoding.UTF8.GetString(buf, 0, rc);
            var sections = ParseStructuredRecap(raw);

            ApplyDoneSections(sections);

            // Save to disk via the existing FFI: we serialise the
            // sections back into recap.md / actions.json. Recap is
            // markdown (TLDR + Decisions + Topics + ...); actions is
            // a plain-text numbered list (as today).
            var recapMarkdown = BuildMarkdownFromSections(sections);
            var actionsPlain = sections.GetValueOrDefault("ACTIONS", "");
            DimmyNative.dimmy_meeting_save_post_process(dir, recapMarkdown, actionsPlain, null);
        }
        catch (Exception ex)
        {
            App.Log($"post-process exc: {ex}", "Meeting");
            ShowDoneFallback(transcript, $"Post-process failed: {ex.Message}");
        }
    }

    private void ShowDoneFallback(string transcript, string note)
    {
        TldrText.Text = note;
        TldrCard.Visibility = Visibility.Visible;
    }

    private void ApplyDoneSections(Dictionary<string, string> sections)
    {
        ApplyDoneSection(sections, "TLDR", TldrText, TldrCard);
        ApplyDoneSection(sections, "KEY_DECISIONS", DecisionsText, DecisionsCard);
        ApplyDoneSection(sections, "TOPICS", TopicsText, TopicsCard);
        ApplyDoneSection(sections, "ACTIONS", ActionsText, ActionsCard);
        ApplyDoneSection(sections, "OPEN_QUESTIONS", OpenQuestionsText, OpenQuestionsCard);
        ApplyDoneSection(sections, "RISKS", RisksText, RisksCard);
        ApplyDoneSection(sections, "NEXT_STEPS", NextStepsText, NextStepsCard);
    }

    private static void ApplyDoneSection(
        Dictionary<string, string> sections, string key,
        Microsoft.UI.Xaml.Controls.TextBlock target,
        Microsoft.UI.Xaml.Controls.Border card)
    {
        if (!sections.TryGetValue(key, out var value) || string.IsNullOrWhiteSpace(value) || value.Trim() == "—")
        {
            card.Visibility = Visibility.Collapsed;
            return;
        }
        target.Text = value.Trim();
        card.Visibility = Visibility.Visible;
    }

    private static string BuildMarkdownFromSections(Dictionary<string, string> s)
    {
        var sb = new System.Text.StringBuilder();
        if (s.TryGetValue("TLDR", out var t) && !string.IsNullOrWhiteSpace(t))
            sb.AppendLine("## TL;DR\n").AppendLine(t.Trim()).AppendLine();
        if (s.TryGetValue("KEY_DECISIONS", out var k) && !string.IsNullOrWhiteSpace(k))
            sb.AppendLine("## Key decisions\n").AppendLine(k.Trim()).AppendLine();
        if (s.TryGetValue("TOPICS", out var top) && !string.IsNullOrWhiteSpace(top))
            sb.AppendLine("## Topics discussed\n").AppendLine(top.Trim()).AppendLine();
        if (s.TryGetValue("OPEN_QUESTIONS", out var oq) && !string.IsNullOrWhiteSpace(oq))
            sb.AppendLine("## Open questions\n").AppendLine(oq.Trim()).AppendLine();
        if (s.TryGetValue("RISKS", out var r) && !string.IsNullOrWhiteSpace(r))
            sb.AppendLine("## Risks & blockers\n").AppendLine(r.Trim()).AppendLine();
        if (s.TryGetValue("NEXT_STEPS", out var n) && !string.IsNullOrWhiteSpace(n))
            sb.AppendLine("## Next steps\n").AppendLine(n.Trim()).AppendLine();
        return sb.ToString();
    }

    /// Pick the strongest LLM available given what's configured. Only
    /// the model NAME is overridden — the URL + key still come from
    /// the user's main LLM config.
    private static string PickRecapModel()
    {
        try
        {
            var cfgPath = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "dimmy", "config.json");
            if (!File.Exists(cfgPath)) return "";
            using var doc = JsonDocument.Parse(File.ReadAllText(cfgPath));
            if (!doc.RootElement.TryGetProperty("llm_api_url", out var urlEl)) return "";
            var url = urlEl.GetString() ?? "";
            if (url.Contains("anthropic.com", StringComparison.OrdinalIgnoreCase))
                return "claude-opus-4-7";
            if (url.Contains("googleapis.com", StringComparison.OrdinalIgnoreCase))
                return "gemini-2.5-pro";
            return "";
        }
        catch
        {
            return "";
        }
    }

    /// New 7-section structured recap prompt. Replaces the previous
    /// 2-section RECAP+ACTIONS shape. Sized to beat Notion / Granola
    /// recap quality — see docs/dev/meeting-ui-port-plan.md for the
    /// design rationale.
    private static string BuildStructuredRecapPrompt(string transcript)
    {
        return
            "You are an expert meeting analyst. Output ONLY the markdown sections " +
            "below in the SAME LANGUAGE as the transcript (auto-detect, do not translate). " +
            "Use the EXACT marker headings shown so a downstream parser can split the response.\n\n" +

            "## ===TLDR===\n" +
            "1-2 sentence executive summary of the meeting.\n\n" +

            "## ===KEY_DECISIONS===\n" +
            "Bullet list. Each item: \"**[topic]** : [decision verbatim or paraphrased] " +
            "([owner] decided)\". Skip the section with \"—\" if no decisions were made.\n\n" +

            "## ===TOPICS===\n" +
            "Group the discussion into 3-7 topics. For each:\n" +
            "- ### Topic title (1-3 words)\n" +
            "- 2-4 bullet points capturing what was discussed\n" +
            "- Quote the most important sentence verbatim (\"> ...\") if one exists.\n\n" +

            "## ===ACTIONS===\n" +
            "Numbered list of action items. Each: \"N. **[owner]** : [task] (due: " +
            "[date / event / 'unspecified'])\". Include only actions explicitly spoken; " +
            "do NOT invent. Use \"—\" if none.\n\n" +

            "## ===OPEN_QUESTIONS===\n" +
            "Bullet list. Things raised but not resolved. Use \"—\" if none.\n\n" +

            "## ===RISKS===\n" +
            "Bullet list. Risks, blockers, dependencies surfaced. Use \"—\" if none.\n\n" +

            "## ===NEXT_STEPS===\n" +
            "Numbered list of immediate next steps (different from Actions: these are " +
            "the meeting's overall trajectory, not assigned tasks). Use \"—\" if none.\n\n" +

            "Hard rules:\n" +
            "- Output the sections in the exact order above.\n" +
            "- Same language as the transcript; if mixed, pick the dominant one.\n" +
            "- Never invent participants, dates, amounts, project names, or technical " +
            "terms not in the transcript.\n" +
            "- Skip a section entirely (still emit the marker + \"—\") when the " +
            "transcript has no content for it.\n" +
            "- No filler: \"the meeting discussed\", \"various topics were covered\", etc.\n" +
            "- No em-dashes (—) in prose outside the markers — they read as AI slop. " +
            "Use periods or commas.\n\n" +

            "Transcript:\n" + transcript;
    }

    /// Parse the LLM response into a section→content map. Tolerates
    /// minor variation in marker formatting (with or without ## prefix,
    /// extra whitespace) — but the markers themselves must be present.
    private static Dictionary<string, string> ParseStructuredRecap(string raw)
    {
        var keys = new[] { "TLDR", "KEY_DECISIONS", "TOPICS", "ACTIONS", "OPEN_QUESTIONS", "RISKS", "NEXT_STEPS" };
        var result = new Dictionary<string, string>();
        var indices = new SortedDictionary<int, string>();
        foreach (var k in keys)
        {
            var marker = $"===" + k + "===";
            int idx = raw.IndexOf(marker, StringComparison.OrdinalIgnoreCase);
            if (idx >= 0) indices[idx] = k;
        }
        if (indices.Count == 0)
        {
            // No markers — best-effort: dump everything into TLDR so
            // the user at least sees something.
            result["TLDR"] = raw.Trim();
            return result;
        }
        var ordered = indices.ToList();
        for (int i = 0; i < ordered.Count; i++)
        {
            var (start, key) = (ordered[i].Key, ordered[i].Value);
            var marker = $"===" + key + "===";
            int contentStart = start + marker.Length;
            int contentEnd = i + 1 < ordered.Count ? ordered[i + 1].Key : raw.Length;
            var content = raw.Substring(contentStart, contentEnd - contentStart).Trim();
            // Strip leading "##" if the LLM kept the markdown-header form.
            content = content.TrimStart('#', ' ', '\n', '\r');
            result[key] = content;
        }
        return result;
    }

    // ── History tab ────────────────────────────────────────────────

    /// History list row — class (not record) so XAML binding's reflection
    /// can read the public mutable getters; init-only records trigger
    /// CS8852 inside the generated XamlTypeInfo.
    public sealed class HistoryRow
    {
        public string Dir { get; set; } = "";
        public string Title { get; set; } = "";
        public string Subtitle { get; set; } = "";
        public string RightLabel { get; set; } = "";
    }

    private void HistoryRefresh_Click(object sender, RoutedEventArgs e) => LoadHistory();

    private void HistorySearch_Changed(object sender, Microsoft.UI.Xaml.Controls.TextChangedEventArgs e)
        => LoadHistory();

    private void LoadHistory()
    {
        HistoryItems.Clear();
        try
        {
            var meetings = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "dimmy", "meetings");
            if (!Directory.Exists(meetings)) return;
            var query = (HistorySearchBox.Text ?? "").Trim();
            var dirs = new DirectoryInfo(meetings).GetDirectories()
                .OrderByDescending(d => d.LastWriteTime)
                .ToList();
            foreach (var d in dirs)
            {
                var meta = Path.Combine(d.FullName, "meta.json");
                string title = d.Name[..Math.Min(8, d.Name.Length)];
                string subtitle = d.LastWriteTime.ToString("yyyy-MM-dd HH:mm");
                if (File.Exists(meta))
                {
                    try
                    {
                        using var doc = JsonDocument.Parse(File.ReadAllText(meta));
                        if (doc.RootElement.TryGetProperty("started_at", out var sa))
                        {
                            var parsed = sa.GetString() ?? "";
                            if (!string.IsNullOrEmpty(parsed)) subtitle = parsed;
                        }
                        if (doc.RootElement.TryGetProperty("duration_secs", out var dur))
                        {
                            subtitle += " · " + FormatDuration(dur.GetDouble());
                        }
                    }
                    catch { }
                }
                if (!string.IsNullOrEmpty(query) &&
                    !title.Contains(query, StringComparison.OrdinalIgnoreCase) &&
                    !subtitle.Contains(query, StringComparison.OrdinalIgnoreCase))
                    continue;
                HistoryItems.Add(new HistoryRow
                {
                    Dir = d.FullName,
                    Title = title,
                    Subtitle = subtitle,
                    RightLabel = File.Exists(Path.Combine(d.FullName, "recap.md")) ? "Recap ✓" : "",
                });
            }
        }
        catch (Exception ex)
        {
            App.Log($"history load exc: {ex.Message}", "Meeting");
        }
    }

    private async void HistoryList_SelectionChanged(object sender,
        Microsoft.UI.Xaml.Controls.SelectionChangedEventArgs e)
    {
        if (HistoryList.SelectedItem is not HistoryRow row) return;
        try
        {
            _activeMeetingDir = row.Dir;
            DoneTitle.Text = Path.GetFileName(row.Dir);
            DoneMeta.Text = row.Subtitle;
            ClearDoneCards();
            // Load recap.md if present
            var recapPath = Path.Combine(row.Dir, "recap.md");
            if (File.Exists(recapPath))
            {
                var text = await File.ReadAllTextAsync(recapPath);
                TldrText.Text = text;
                TldrCard.Visibility = Visibility.Visible;
            }
            // Load transcripts.txt
            var txt = Path.Combine(row.Dir, "transcripts.txt");
            if (File.Exists(txt))
            {
                RawTranscriptText.Text = await File.ReadAllTextAsync(txt);
            }
            await LoadDoneAudioAsync(row.Dir);
            // Switch to Live tab + Done state to show details
            TabLive_Click(this, new RoutedEventArgs());
            SetState(MeetingState.Done);
        }
        catch (Exception ex)
        {
            App.Log($"history select exc: {ex.Message}", "Meeting");
        }
    }

    // ── Misc actions ──────────────────────────────────────────────

    private void OpenFolder_Click(object sender, RoutedEventArgs e)
    {
        if (string.IsNullOrEmpty(_activeMeetingDir)) return;
        try { Process.Start(new ProcessStartInfo { FileName = _activeMeetingDir, UseShellExecute = true }); }
        catch (Exception ex) { App.Log($"open folder failed: {ex.Message}", "Meeting"); }
    }

    private void CopyRecap_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var sb = new System.Text.StringBuilder();
            void Append(Microsoft.UI.Xaml.Controls.Border card, string heading,
                Microsoft.UI.Xaml.Controls.TextBlock body)
            {
                if (card.Visibility != Visibility.Visible) return;
                sb.AppendLine($"## {heading}").AppendLine(body.Text).AppendLine();
            }
            Append(TldrCard, "TL;DR", TldrText);
            Append(DecisionsCard, "Key decisions", DecisionsText);
            Append(TopicsCard, "Topics", TopicsText);
            Append(ActionsCard, "Actions", ActionsText);
            Append(OpenQuestionsCard, "Open questions", OpenQuestionsText);
            Append(RisksCard, "Risks", RisksText);
            Append(NextStepsCard, "Next steps", NextStepsText);
            var dp = new global::Windows.ApplicationModel.DataTransfer.DataPackage();
            dp.SetText(sb.ToString());
            global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(dp);
        }
        catch (Exception ex)
        {
            App.Log($"copy recap exc: {ex.Message}", "Meeting");
        }
    }

    // ── Helpers ────────────────────────────────────────────────────

    private static string FormatDuration(double secs)
    {
        var ts = TimeSpan.FromSeconds(secs);
        if (ts.TotalHours >= 1)
            return $"{(int)ts.TotalHours}h {ts.Minutes}m";
        if (ts.TotalMinutes >= 1)
            return $"{(int)ts.TotalMinutes}m {ts.Seconds}s";
        return $"{(int)ts.TotalSeconds}s";
    }
}
