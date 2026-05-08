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

/// Split-pane meeting window: sidebar with past meetings on the left,
/// state-driven main panel on the right (Idle / Recording / Processing
/// / Done). Recording controls live in a persistent bar at the top of
/// the main panel — visible whenever a recording is active, regardless
/// of which content panel is showing — so the user cannot lose access
/// to Stop by clicking around.
public sealed partial class MeetingWindow : Window
{
    private enum MeetingState { Idle, Recording, Processing, Done }

    private DispatcherQueueTimer? _pollTimer;
    private DispatcherQueueTimer? _ampTimer;
    private DispatcherQueueTimer? _toastTimer;
    private DateTime _startedAt;
    private string? _activeMeetingDir;       // dir of the LIVE recording (set on Start)
    private string? _viewingMeetingDir;      // dir currently shown in main panel (may differ)
    private long _lastTranscriptLen = -1;
    private MeetingState _state = MeetingState.Idle;
    // Decoupled from _state so the user can browse past meetings
    // (state == Done) while a recording is still active. RecordingBar
    // visibility, close-blocking and the Back-to-Live affordance all
    // key off this flag rather than the visible UI panel.
    private bool _recordingActive;

    private readonly Queue<float> _ampHistory = new();
    // Second history for the loopback (system) stream so the live
    // waveform can draw mic and system as two distinct bands.
    private readonly Queue<float> _ampHistorySystem = new();
    // Dynamic bar count — keeps each bar at a fixed pixel width so the
    // waveform doesn't stretch when the window is resized to fullscreen.
    // Recomputed in LiveWaveformCanvas_SizeChanged.
    private const double AMP_BAR_PX = 4.0;
    private const double AMP_GAP_PX = 2.0;
    private const int AMP_MIN_HISTORY = 20;
    private const int AMP_MAX_HISTORY = 240;
    private int _ampHistorySize = 40;

    public ObservableCollection<HistoryRow> HistoryItems { get; } = new();

    public MeetingWindow()
    {
        App.Log("ctor enter", "Meeting");
        this.InitializeComponent();
        Title = "Dimmy Meeting";

        // Match settings-window theme (Light/Dark/Auto from UiPreferences)
        try
        {
            var prefs = Services.UiPreferences.Load();
            if (Content is FrameworkElement root)
            {
                root.RequestedTheme = prefs.Theme switch
                {
                    "Light" => ElementTheme.Light,
                    "Dark" => ElementTheme.Dark,
                    _ => ElementTheme.Default,
                };
            }
        }
        catch (Exception ex) { App.Log($"theme apply exc: {ex.Message}", "Meeting"); }

        try
        {
            var appWindow = WindowHelper.GetAppWindow(this);
            WindowHelper.ResizeLogical(this, 980, 720);
            try
            {
                var iconPath = Path.Combine(AppContext.BaseDirectory, "Assets", "dimmy.ico");
                if (File.Exists(iconPath)) appWindow?.SetIcon(iconPath);
            }
            catch { }
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
        LoadHistory(); // sidebar populated immediately so user sees past meetings

        // ── Close-blocking: while a recording is in progress, refuse
        //    to let the user close the window. Otherwise the Rust core
        //    keeps recording (zombie state) and on reopen Start fails
        //    silently with rc=-1. AppWindow.Closing fires BEFORE Closed
        //    and lets us cancel.
        try
        {
            var aw = WindowHelper.GetAppWindow(this);
            if (aw != null)
            {
                aw.Closing += (_, args) =>
                {
                    App.Log(
                        $"Closing event: _recordingActive={_recordingActive} _state={_state}",
                        "Meeting");
                    if (_recordingActive || _state == MeetingState.Processing)
                    {
                        args.Cancel = true;
                        ShowToast("Recording in progress. Click Stop & finish first.");
                    }
                };
                App.Log("Closing handler attached to AppWindow", "Meeting");
            }
            else
            {
                App.Log("WARNING: WindowHelper.GetAppWindow returned null — Closing handler NOT attached", "Meeting");
            }
        }
        catch (Exception ex) { App.Log($"closing-hook exc: {ex.Message}", "Meeting"); }

        Closed += (_, __) =>
        {
            StopPolling();
            StopAmplitudePoll();

            // Stop any audio.wav playback (MediaPlayerElement). Without
            // this, the underlying MediaPlayer keeps holding the audio
            // session and the file plays out in background even though
            // the window is gone — user has no UI to pause it. Standard
            // best practice for media UI: window close = playback stop.
            StopDoneMediaPlayback();

            // Defense-in-depth: if the window is being closed while a
            // RECORDING is still active in the Rust core (different
            // from the playback case — this guards Mix-mode capture,
            // not audio.wav playback), force-stop the meeting in core
            // so we don't leak a zombie. AppWindow.Closing should have
            // cancelled the close upstream when _recordingActive, but
            // if it slipped through (alt+f4 timing, runtime exit, a
            // future API change), this catches it. WAVs + transcripts
            // are already on disk via the streaming writer.
            try
            {
                if (_recordingActive || DimmyNative.dimmy_meeting_is_active() == 1)
                {
                    App.Log(
                        "Closed: recording still active in core — force-stopping to prevent zombie",
                        "Meeting");
                    var buf = new byte[1 << 22];
                    int rc = DimmyNative.dimmy_meeting_stop(buf, buf.Length);
                    App.Log($"Closed: force-stop rc={rc}", "Meeting");
                }
            }
            catch (Exception ex) { App.Log($"Closed force-stop exc: {ex}", "Meeting"); }
        };

        // ── Re-sync UI to the Rust core. If a meeting is already
        //    active (e.g. user previously closed the meeting window
        //    without stopping, or hit it from a different surface),
        //    skip Idle and jump straight to Recording so the Stop
        //    button is reachable instead of leaking a zombie.
        try
        {
            if (DimmyNative.dimmy_meeting_is_active() == 1)
            {
                _startedAt = DateTime.UtcNow;       // best-effort; Rust holds the truth
                _lastTranscriptLen = -1;
                _ampHistory.Clear();
                _ampHistorySystem.Clear();
                _recordingActive = true;
                AppContextName.Text = "Microphone";
                SetState(MeetingState.Recording);
                TranscriptText.Text = "🎙️ Re-attached to ongoing recording…";
                StartPolling();
                StartAmplitudePoll();
                App.Log("ctor: re-attached to active meeting", "Meeting");
            }
        }
        catch (Exception ex) { App.Log($"resync exc: {ex.Message}", "Meeting"); }
    }

    // ── State machine ──────────────────────────────────────────────

    private void SetState(MeetingState s)
    {
        _state = s;
        IdlePanel.Visibility = s == MeetingState.Idle ? Visibility.Visible : Visibility.Collapsed;
        RecordingPanel.Visibility = s == MeetingState.Recording ? Visibility.Visible : Visibility.Collapsed;
        ProcessingPanel.Visibility = s == MeetingState.Processing ? Visibility.Visible : Visibility.Collapsed;
        DonePanel.Visibility = s == MeetingState.Done ? Visibility.Visible : Visibility.Collapsed;
        // RecordingBar lifecycle is keyed off _recordingActive (not the
        // visible panel) so the user can navigate to a past meeting
        // while still seeing — and being able to stop — the live one.
        RecordingBar.Visibility = (_recordingActive || s == MeetingState.Processing)
            ? Visibility.Visible : Visibility.Collapsed;
        StopBtn.IsEnabled = _recordingActive && s != MeetingState.Processing;
        BackToLiveBtn.Visibility = (_recordingActive
            && s != MeetingState.Recording
            && s != MeetingState.Processing)
            ? Visibility.Visible : Visibility.Collapsed;
        // Hide "New meeting" while a capture is in flight — clicking it
        // would only show the toast block. Stop is the actual action.
        TitlebarNewBtn.Visibility = (_recordingActive || s == MeetingState.Processing)
            ? Visibility.Collapsed : Visibility.Visible;
        NewMeetingHeaderBtn.IsEnabled = !_recordingActive && s != MeetingState.Processing;
        TitlebarTitle.Text = s switch
        {
            MeetingState.Idle => HistoryList.SelectedItem is HistoryRow r ? r.Title : "New meeting",
            MeetingState.Recording => "Recording in progress",
            MeetingState.Processing => "Wrapping up…",
            MeetingState.Done => DoneTitle.Text,
            _ => "Meeting",
        };
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
                // Most common cause: a meeting is already active in the
                // Rust core (zombie from a previous close). Recover by
                // re-attaching the UI to it instead of leaving the user
                // stuck with a non-responsive Start button.
                if (DimmyNative.dimmy_meeting_is_active() == 1)
                {
                    _startedAt = DateTime.UtcNow;
                    _lastTranscriptLen = -1;
                    _ampHistory.Clear();
                    _ampHistorySystem.Clear();
                    _recordingActive = true;
                    HistoryList.SelectedItem = null;
                    AppContextName.Text = "Microphone";
                    SetState(MeetingState.Recording);
                    TranscriptText.Text = "🎙️ Re-attached to ongoing recording…";
                    StartPolling();
                    StartAmplitudePoll();
                    ShowToast("Re-attached to an ongoing recording.");
                    return;
                }
                ShowToast("Could not start the meeting. Check the log.");
                StartBtn.IsEnabled = true;
                return;
            }
            var id = System.Text.Encoding.UTF8.GetString(buf, 0, rc);
            _startedAt = DateTime.UtcNow;
            _lastTranscriptLen = -1;
            _ampHistory.Clear();
            _ampHistorySystem.Clear();
            _activeMeetingDir = null;       // poll fills it once Rust creates the dir
            _viewingMeetingDir = null;

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
                AppContextName.Text = "Microphone";
            }

            // Clear any sidebar selection from a previous "browse" session.
            HistoryList.SelectedItem = null;
            _recordingActive = true;
            SetState(MeetingState.Recording);
            TranscriptText.Text = "🎙️ Listening… first transcript appears in ~15 s.";
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
        // Recording is being torn down — flip the flag NOW so close
        // is unblocked even if the Rust stop call hangs.
        _recordingActive = false;
        SetState(MeetingState.Processing);
        ResetProcSteps();
        SetProcStep(1, true);
        try
        {
            var buf = new byte[1 << 22];
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
            _viewingMeetingDir = dir;

            DoneTitle.Text = string.IsNullOrEmpty(dir) ? "Meeting" : Path.GetFileName(dir);
            DoneMeta.Text = $"{FormatDuration(dur)} · {chunks} chunks · {DateTime.Now:yyyy-MM-dd HH:mm}";
            RawTranscriptText.Text = string.IsNullOrEmpty(transcript)
                ? "(no transcript: VAD may have removed all audio)"
                : HumanizeTranscript(transcript);
            await LoadDoneAudioAsync(dir);

            if (GenerateRecapCheck.IsChecked == true && !string.IsNullOrWhiteSpace(transcript))
            {
                SetProcStep(2, false);
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
            LoadHistory(); // freshly-finished meeting appears at top of sidebar
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

    private void Pause_Click(object sender, RoutedEventArgs e)
    {
        // Pause requires AudioCommand::Pause + meeting.rs hooks in
        // the Rust core that aren't there yet. Button stays disabled
        // (IsEnabled="False" in XAML) until the backend is wired —
        // tracked as a follow-up.
        ShowToast("Pause is not yet wired — coming soon.");
    }

    private void NewMeeting_Click(object sender, RoutedEventArgs e)
    {
        // While recording, "New meeting" makes no sense — we'd lose
        // the current capture. Block with a toast.
        if (_recordingActive || _state == MeetingState.Processing)
        {
            ShowToast("Stop the current recording first to start a new one.");
            return;
        }
        StopDoneMediaPlayback();
        HistoryList.SelectedItem = null;
        SetState(MeetingState.Idle);
        StartBtn.IsEnabled = true;
        ClearDoneCards();
        TranscriptText.Text = "🎙️ Listening… first transcript appears in ~15 s.";
        TitlebarTitle.Text = "New meeting";
    }

    /// Stop and release the MediaPlayerElement's playback so audio
    /// doesn't keep playing in the background. Called from:
    ///   - Window Closed handler (window goes away → audio must too)
    ///   - NewMeeting_Click (user navigates back to Idle)
    ///   - HistoryList_SelectionChanged (selecting a different past meeting
    ///     before LoadDoneAudioAsync swaps the source)
    /// Best practice for media UI: closing the surface releases the
    /// audio session. Same pattern Spotify / browsers / podcast apps
    /// use — no surprise lingering audio.
    private void StopDoneMediaPlayback()
    {
        try
        {
            if (DoneAudioPlayer == null) return;
            var mp = DoneAudioPlayer.MediaPlayer;
            if (mp != null)
            {
                // Detach the position-changed listener so we don't get
                // late callbacks after the window/state has moved on.
                try { mp.PlaybackSession.PositionChanged -= OnDonePlaybackPositionChanged; }
                catch { }
                if (mp.PlaybackSession.PlaybackState
                    == global::Windows.Media.Playback.MediaPlaybackState.Playing)
                {
                    mp.Pause();
                }
                // Releasing Source releases the SMTC entry (volume mixer
                // / lockscreen now-playing) and the underlying file
                // handle. Without this, MediaPlayer keeps the audio
                // session alive even after the visual element is gone.
                mp.Source = null;
            }
            DoneAudioPlayer.Source = null;
        }
        catch (Exception ex)
        {
            App.Log($"StopDoneMediaPlayback exc: {ex.Message}", "Meeting");
        }
    }

    private void BackToLive_Click(object sender, RoutedEventArgs e)
    {
        // Return to the live transcript view without touching the
        // recording. Polling never stopped — only the UI panel was
        // hidden — so flipping state back is enough.
        if (!_recordingActive) return;
        HistoryList.SelectedItem = null;
        SetState(MeetingState.Recording);
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

    // ── Polling + amplitude ──────────────────────────────────────

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
        RecTimer.Text = $"{(int)elapsed.TotalHours:D2}:{elapsed.Minutes:D2}:{elapsed.Seconds:D2}";

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
            TranscriptText.Text = HumanizeTranscript(content);
            var nChunks = content.Split('\n', StringSplitOptions.RemoveEmptyEntries).Length;
            RecChunks.Text = $"{nChunks} chunks";
            TranscriptMeta.Text = $"{nChunks} chunks";
            TranscriptScroll?.ChangeView(null, double.MaxValue, null, true);
        }
        catch (Exception ex)
        {
            App.Log($"poll exc: {ex.Message}", "Meeting");
        }
    }

    private void StartAmplitudePoll()
    {
        var dq = DispatcherQueue.GetForCurrentThread();
        _ampTimer = dq.CreateTimer();
        _ampTimer.Interval = TimeSpan.FromMilliseconds(83);
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
            float ampMic = DimmyNative.dimmy_get_amplitude();
            if (!float.IsFinite(ampMic)) ampMic = 0;
            ampMic = (float)Math.Min(1.0, Math.Sqrt(ampMic) * 1.4);
            while (_ampHistory.Count >= _ampHistorySize) _ampHistory.Dequeue();
            _ampHistory.Enqueue(ampMic);

            // Loopback (system) amplitude — populated only in Mix mode.
            // In Mic-only / System-only modes this is 0, which collapses
            // the bottom band to invisible and the canvas effectively
            // shows just the mic band.
            float ampSys = DimmyNative.dimmy_get_loopback_amplitude();
            if (!float.IsFinite(ampSys)) ampSys = 0;
            ampSys = (float)Math.Min(1.0, Math.Sqrt(ampSys) * 1.4);
            while (_ampHistorySystem.Count >= _ampHistorySize) _ampHistorySystem.Dequeue();
            _ampHistorySystem.Enqueue(ampSys);

            DrawLiveWaveform();
        }
        catch { }
    }

    private void LiveWaveformCanvas_SizeChanged(object sender,
        Microsoft.UI.Xaml.SizeChangedEventArgs e)
    {
        // Recompute bar count to keep bars at a fixed pixel width
        // regardless of window size. Without this, going fullscreen
        // stretches the existing 40 bars across the whole canvas.
        double w = LiveWaveformCanvas.ActualWidth;
        if (w <= 0) return;
        int n = (int)(w / (AMP_BAR_PX + AMP_GAP_PX));
        n = Math.Clamp(n, AMP_MIN_HISTORY, AMP_MAX_HISTORY);
        _ampHistorySize = n;
        while (_ampHistory.Count > n) _ampHistory.Dequeue();
        while (_ampHistorySystem.Count > n) _ampHistorySystem.Dequeue();
        DrawLiveWaveform();
    }

    private void DrawLiveWaveform()
    {
        if (LiveWaveformCanvas == null) return;
        LiveWaveformCanvas.Children.Clear();
        double w = LiveWaveformCanvas.ActualWidth;
        double h = LiveWaveformCanvas.ActualHeight;
        if (w <= 0 || h <= 0) return;

        // Two stacked bands so mic and system are clearly readable
        // at a glance. Mic on top half (DodgerBlue), system on bottom
        // half (LimeGreen). Each band centered on its own midline so
        // bars grow up + down equally within their half.
        double pitch = AMP_BAR_PX + AMP_GAP_PX;
        double bandHeight = h / 2.0;
        double midTop = bandHeight / 2.0;
        double midBottom = bandHeight + bandHeight / 2.0;
        var brushMic = new SolidColorBrush(Microsoft.UI.Colors.DodgerBlue);
        var brushSys = new SolidColorBrush(Microsoft.UI.Colors.LimeGreen);

        DrawBand(_ampHistory.ToArray(), brushMic, midTop, bandHeight - 4, w, pitch);
        DrawBand(_ampHistorySystem.ToArray(), brushSys, midBottom, bandHeight - 4, w, pitch);
    }

    private void DrawBand(float[] samples, SolidColorBrush brush, double mid, double maxHeight, double w, double pitch)
    {
        if (samples.Length == 0) return;
        int n = samples.Length;
        for (int i = 0; i < n; i++)
        {
            double x = w - (n - i) * pitch;
            if (x + AMP_BAR_PX < 0) continue;
            double height = Math.Max(2, samples[i] * maxHeight);
            var rect = new Microsoft.UI.Xaml.Shapes.Rectangle
            {
                Width = AMP_BAR_PX,
                Height = height,
                Fill = brush,
                RadiusX = 1,
                RadiusY = 1,
            };
            Microsoft.UI.Xaml.Controls.Canvas.SetLeft(rect, x);
            Microsoft.UI.Xaml.Controls.Canvas.SetTop(rect, mid - height / 2.0);
            LiveWaveformCanvas.Children.Add(rect);
        }
    }

    // ── Done-state audio waveform card ────────────────────────────

    // Done-view waveform peaks. _cachedDonePeaks = mix (audio.wav,
    // legacy / fallback when per-track files are absent). _cachedMicPeaks
    // = audio_mic.wav (Phase 3+ recordings). _cachedSystemPeaks =
    // audio_system.wav (Phase 3+ Mix-mode recordings). DrawDoneWaveform
    // prefers the per-track pair when both exist, drawing them as dual
    // bands matching the live waveform colors.
    private float[]? _cachedDonePeaks;
    private float[]? _cachedMicPeaks;
    private float[]? _cachedSystemPeaks;

    private async Task LoadDoneAudioAsync(string dir)
    {
        if (string.IsNullOrEmpty(dir)) return;
        var wavPath = Path.Combine(dir, "audio.wav");
        if (!File.Exists(wavPath))
        {
            DoneWaveCard.Visibility = Visibility.Collapsed;
            _cachedDonePeaks = null;
            _cachedMicPeaks = null;
            _cachedSystemPeaks = null;
            return;
        }
        // Refuse to load a 0-byte audio.wav — that's the "interrupted
        // recording" scenario where the WAV writer never finalised.
        // Hide the card cleanly instead of feeding the MediaPlayer
        // garbage. (Recording is now shielded from sidebar clicks
        // during capture, so this should be rare.)
        try
        {
            if (new FileInfo(wavPath).Length < 64)
            {
                DoneWaveCard.Visibility = Visibility.Collapsed;
                _cachedDonePeaks = null;
                _cachedMicPeaks = null;
                _cachedSystemPeaks = null;
                return;
            }
        }
        catch { }

        try
        {
            DoneWaveCard.Visibility = Visibility.Visible;
            DoneAudioPlayer.Source = global::Windows.Media.Core.MediaSource.CreateFromUri(new Uri(wavPath));
            var mp = DoneAudioPlayer.MediaPlayer;
            if (mp != null)
            {
                mp.PlaybackSession.PositionChanged -= OnDonePlaybackPositionChanged;
                mp.PlaybackSession.PositionChanged += OnDonePlaybackPositionChanged;
            }

            double width = DoneWaveformCanvas.ActualWidth;
            if (width <= 0) width = 700;
            int buckets = (int)Math.Max(80, Math.Min(500, width / 3));

            // Read all three tracks in parallel where present. Phase 3+
            // recordings have audio_mic.wav and audio_system.wav as
            // separate files; older recordings have only audio.wav.
            var micPath = Path.Combine(dir, "audio_mic.wav");
            var systemPath = Path.Combine(dir, "audio_system.wav");
            var mixTask = Task.Run(() => Helpers.WavPeaks.ReadPeaks(wavPath, buckets));
            var micTask = File.Exists(micPath)
                ? Task.Run(() => Helpers.WavPeaks.ReadPeaks(micPath, buckets))
                : Task.FromResult(System.Array.Empty<float>());
            var sysTask = File.Exists(systemPath)
                ? Task.Run(() => Helpers.WavPeaks.ReadPeaks(systemPath, buckets))
                : Task.FromResult(System.Array.Empty<float>());
            await Task.WhenAll(mixTask, micTask, sysTask);

            _cachedDonePeaks = mixTask.Result;
            _cachedMicPeaks = micTask.Result.Length > 0 ? micTask.Result : null;
            _cachedSystemPeaks = sysTask.Result.Length > 0 ? sysTask.Result : null;
            DrawDoneWaveform();
        }
        catch (Exception ex)
        {
            App.Log($"LoadDoneAudio exc: {ex.Message}", "Meeting");
        }
    }

    private Microsoft.UI.Xaml.Shapes.Rectangle? _donePlayhead;

    private void OnDonePlaybackPositionChanged(
        global::Windows.Media.Playback.MediaPlaybackSession session, object args)
    {
        try
        {
            var total = session.NaturalDuration.TotalSeconds;
            if (total <= 0) return;
            double frac = session.Position.TotalSeconds / total;
            DispatcherQueue.TryEnqueue(() => UpdateDonePlayhead(frac));
        }
        catch { }
    }

    private void UpdateDonePlayhead(double frac)
    {
        if (DoneWaveformCanvas == null) return;
        double w = DoneWaveformCanvas.ActualWidth;
        double h = DoneWaveformCanvas.ActualHeight;
        if (w <= 0 || h <= 0) return;
        if (_donePlayhead == null || !DoneWaveformCanvas.Children.Contains(_donePlayhead))
        {
            _donePlayhead = new Microsoft.UI.Xaml.Shapes.Rectangle
            {
                Width = 2, Height = h,
                Fill = new SolidColorBrush(Microsoft.UI.Colors.OrangeRed),
                IsHitTestVisible = false,
            };
            Microsoft.UI.Xaml.Controls.Canvas.SetTop(_donePlayhead, 0);
            DoneWaveformCanvas.Children.Add(_donePlayhead);
        }
        Microsoft.UI.Xaml.Controls.Canvas.SetLeft(_donePlayhead,
            Math.Max(0, Math.Min(w - 2, w * frac)));
    }

    private void DrawDoneWaveform()
    {
        if (DoneWaveformCanvas == null) return;
        DoneWaveformCanvas.Children.Clear();
        _donePlayhead = null;
        double w = DoneWaveformCanvas.ActualWidth;
        double h = DoneWaveformCanvas.ActualHeight;
        if (w <= 0 || h <= 0) return;

        bool dual = _cachedMicPeaks != null && _cachedSystemPeaks != null;
        if (dual)
        {
            // Phase 3+ recording with both per-track files. Top half =
            // mic (DodgerBlue, AEC-cleaned), bottom half = system
            // (LimeGreen, raw loopback). Same palette as the live
            // waveform so the two views are visually consistent.
            double bandH = h / 2.0;
            DrawDoneBand(_cachedMicPeaks!,
                new SolidColorBrush(Microsoft.UI.Colors.DodgerBlue),
                bandH / 2.0, bandH - 2, w);
            DrawDoneBand(_cachedSystemPeaks!,
                new SolidColorBrush(Microsoft.UI.Colors.LimeGreen),
                bandH + bandH / 2.0, bandH - 2, w);
        }
        else if (_cachedDonePeaks != null && _cachedDonePeaks.Length > 0)
        {
            // Pre-Phase-3 recording (or Mic-only / System-only mode where
            // one track file is absent). Single-band centered.
            DrawDoneBand(_cachedDonePeaks,
                new SolidColorBrush(Microsoft.UI.Colors.DodgerBlue),
                h / 2.0, h - 2, w);
        }

        UpdateDonePlayhead(0);
    }

    private void DrawDoneBand(float[] peaks, SolidColorBrush brush, double mid, double maxHeight, double w)
    {
        if (peaks.Length == 0) return;
        double slot = w / peaks.Length;
        double barW = Math.Max(1, slot - 1);
        for (int i = 0; i < peaks.Length; i++)
        {
            double bh = Math.Max(1, peaks[i] * maxHeight);
            var rect = new Microsoft.UI.Xaml.Shapes.Rectangle
            {
                Width = barW,
                Height = bh,
                Fill = brush,
                RadiusX = 1,
                RadiusY = 1,
            };
            Microsoft.UI.Xaml.Controls.Canvas.SetLeft(rect, i * slot);
            Microsoft.UI.Xaml.Controls.Canvas.SetTop(rect, mid - bh / 2.0);
            DoneWaveformCanvas.Children.Add(rect);
        }
    }

    private void DoneWaveformCanvas_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (_cachedDonePeaks == null && _cachedMicPeaks == null && _cachedSystemPeaks == null) return;
        if (e.NewSize.Width <= 0 || e.NewSize.Height <= 0) return;
        if (DoneWaveformCanvas.Children.Count <= 1)
        {
            DrawDoneWaveform();
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
            UpdateDonePlayhead(frac);
        }
        catch { }
    }

    // ── Processing steps ──────────────────────────────────────────

    private void ResetProcSteps()
    {
        SetProcStep(1, false);
        SetProcStep(2, false);
        SetProcStep(3, false);
    }

    private void SetProcStep(int n, bool done)
    {
        var icon = n switch
        {
            1 => ProcStep1Icon,
            2 => ProcStep2Icon,
            3 => ProcStep3Icon,
            _ => null,
        };
        if (icon == null) return;
        icon.Glyph = done ? "" : "";
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
            // 16000 max_tokens leaves room for the Anthropic extended-thinking
            // budget (10000) + the actual response (~4-6k tokens for a rich
            // Notion-style recap). Without enough headroom Anthropic rejects
            // the request with "max_tokens must be greater than budget_tokens".
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
                ShowDoneFallback(transcript, msg);
                return;
            }
            var raw = System.Text.Encoding.UTF8.GetString(buf, 0, rc);
            var sections = ParseStructuredRecap(raw);

            ApplyDoneSections(sections);

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
        // Persisted recap.md gets ALL sections present in the parser output,
        // in their canonical Notion-style order. The Done-view UI displays
        // a subset (the 7 cards from Phase 4); the new sections (CONTEXT,
        // HIGHLIGHTS, NARRATIVE, FOLLOWUPS) are stored on disk and shown
        // by the upcoming Settings/UI revision (Phase C). Until then they
        // live in recap.md and the user can read them via Open Folder.
        var sb = new System.Text.StringBuilder();
        void AppendSection(string key, string heading)
        {
            if (s.TryGetValue(key, out var v) && !string.IsNullOrWhiteSpace(v) && v.Trim() != "—")
            {
                sb.AppendLine($"## {heading}\n").AppendLine(v.Trim()).AppendLine();
            }
        }
        AppendSection("CONTEXT", "Context");
        AppendSection("TLDR", "TL;DR");
        AppendSection("HIGHLIGHTS", "Highlights");
        AppendSection("NARRATIVE", "Narrative");
        AppendSection("KEY_DECISIONS", "Key decisions");
        AppendSection("TOPICS", "Topics discussed");
        AppendSection("ACTIONS", "Action items");
        AppendSection("OPEN_QUESTIONS", "Open questions");
        AppendSection("RISKS", "Risks & blockers");
        AppendSection("NEXT_STEPS", "Next steps");
        AppendSection("FOLLOWUPS", "Follow-ups");
        return sb.ToString();
    }

    private static string PickRecapModel()
    {
        // Order of precedence:
        //   1. user override from Settings (recap_model_override field)
        //   2. provider-default flagship reasoning model based on llm_api_url
        //   3. empty (Rust falls back to llm_api_model from config)
        try
        {
            var cfgPath = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "dimmy", "config.json");
            if (!File.Exists(cfgPath)) return "";
            using var doc = JsonDocument.Parse(File.ReadAllText(cfgPath));
            // 1. User override
            if (doc.RootElement.TryGetProperty("recap_model_override", out var ovEl))
            {
                var ov = ovEl.GetString();
                if (!string.IsNullOrWhiteSpace(ov))
                    return ov.Trim();
            }
            // 2. Provider-default flagship reasoning model (May 2026)
            if (!doc.RootElement.TryGetProperty("llm_api_url", out var urlEl)) return "";
            var url = urlEl.GetString() ?? "";
            if (url.Contains("anthropic.com", StringComparison.OrdinalIgnoreCase))
                return "claude-opus-4-7";
            if (url.Contains("googleapis.com", StringComparison.OrdinalIgnoreCase))
                return "gemini-3-1-pro";
            if (url.Contains("openai.com", StringComparison.OrdinalIgnoreCase))
                return "gpt-5";
            return "";
        }
        catch
        {
            return "";
        }
    }

    private static string BuildStructuredRecapPrompt(string transcript)
    {
        // Notion-quality meeting recap prompt. Designed for reasoning-tier
        // models (Opus 4.7 + extended thinking, Gemini 3.1 Pro thinkingLevel=high,
        // GPT-5). Asks for richer analysis: context inference, narrative
        // prose summary, sentiment/tone, sentiment-tagged topics, priority
        // inference on actions, time-stamped highlights, follow-ups list.
        return
            "You are a senior meeting analyst writing a polished, Notion-style " +
            "summary of an audio recording. Output ONLY markdown with the EXACT " +
            "marker headings shown — a downstream parser splits on them.\n\n" +

            "## Transcript format\n" +
            "Each line: `[ELAPSED_MS ms] [SPEAKER_LABEL] text`.\n" +
            "Speaker labels: `[mic]` = the user recording (treat as \"you\" / first person " +
            "when the language allows), `[system]` = remote participant(s) coming through " +
            "speakers/loopback (treat as \"the remote party\" / \"interlocutor\" / specific " +
            "name only if explicitly mentioned in the transcript). When only `[mic]` is " +
            "present, the recording is monologue / dictation; when only `[system]` is " +
            "present, the user was a silent listener.\n\n" +

            "## Output language\n" +
            "Auto-detect from the transcript. For mixed languages, pick the dominant one. " +
            "Do NOT translate. If the transcript is in Italian, write the recap in Italian.\n\n" +

            "## Sections (emit ALL of them, in this order)\n\n" +

            "## ===CONTEXT===\n" +
            "2-4 sentences inferring the SETTING of the meeting from cues in the " +
            "transcript: how many distinct voices, the apparent purpose (status sync? " +
            "kickoff? interview? decision-making? brainstorm?), the apparent domain " +
            "(engineering / sales / product / personal / academic). Don't invent names — " +
            "say \"the user\" / \"the remote participant\" unless explicitly named.\n\n" +

            "## ===TLDR===\n" +
            "1-2 sentences. The single most important thing a busy reader needs to know. " +
            "Ruthless filtering: if you removed everything else, what would they still need?\n\n" +

            "## ===HIGHLIGHTS===\n" +
            "3-5 most pivotal moments, one per line:\n" +
            "- `[MM:SS]` **Topic shorthand** — why this matters in one sentence.\n" +
            "Convert ELAPSED_MS to MM:SS. Pick highlights that change the meeting's " +
            "trajectory: a decision, a conflict, a key reveal, a commitment, a moment of " +
            "alignment after debate. Skip routine status updates. If fewer than 3 pivotal " +
            "moments exist, list only what genuinely qualifies.\n\n" +

            "## ===NARRATIVE===\n" +
            "2-4 paragraphs of FLOWING PROSE (NOT bullets). Tell the story of the meeting: " +
            "what was discussed, in which order, with which tone, who pushed for what, " +
            "where alignment came easy and where it was contested. Reference `[mic]` / " +
            "`[system]` inline when attribution matters. Quote a memorable phrase verbatim " +
            "when it captures a moment (`\"...\"`). This is the section a busy stakeholder " +
            "would read INSTEAD of listening to the recording.\n\n" +

            "## ===KEY_DECISIONS===\n" +
            "Bullet list of decisions actually MADE in the meeting (not proposed, not " +
            "considered). Each item:\n" +
            "- **[topic]** : [decision verbatim or paraphrased] — decided by " +
            "[owner if known else \"consensus\"]; impact: [low/medium/high] with one-sentence " +
            "rationale.\n" +
            "Use a single line `—` if no decisions were reached.\n\n" +

            "## ===TOPICS===\n" +
            "Group the discussion into 3-7 distinct topics, sorted by importance (NOT " +
            "chronological). For each:\n" +
            "- ### Topic title (1-3 words)\n" +
            "- 2-4 bullets capturing what was discussed\n" +
            "- _Sentiment_: aligned / debated / blocked / open\n" +
            "- _Quote_: `\"verbatim sentence from transcript\"` — only if a single " +
            "sentence captures the moment; omit otherwise (do NOT approximate).\n\n" +

            "## ===ACTIONS===\n" +
            "Numbered list of action items EXPLICITLY committed to in the transcript. " +
            "Never invent. Each action:\n" +
            "N. **[owner]** : [task] — due: [explicit date / event / \"unspecified\"]; " +
            "priority: [P0/P1/P2 inferred from urgency cues like \"asap\", \"this week\", " +
            "\"eventually\"].\n" +
            "Use `—` if no actions.\n\n" +

            "## ===OPEN_QUESTIONS===\n" +
            "Genuine questions raised but NOT resolved. Phrase as the question itself, " +
            "not as \"they asked about X\". One bullet per question. Use `—` if none.\n\n" +

            "## ===RISKS===\n" +
            "Risks, blockers, dependencies surfaced. Each:\n" +
            "- **[topic]** — [risk in 1 sentence]; likelihood [low/med/high], impact [low/med/high].\n" +
            "Use `—` if none.\n\n" +

            "## ===NEXT_STEPS===\n" +
            "Numbered list of the 3-5 immediate next things that should happen for the " +
            "meeting's trajectory (NOT the same as Actions — those are owned tasks; " +
            "Next Steps is the work's direction). Use `—` if none.\n\n" +

            "## ===FOLLOWUPS===\n" +
            "Things to ASK / CLARIFY offline because the meeting didn't fully resolve them:\n" +
            "- **[topic]** — ask [person if mentioned else \"the relevant party\"] about " +
            "[specific thing].\n" +
            "Use `—` if none.\n\n" +

            "## Hard rules\n" +
            "- Output the sections in the exact order above. ALL section markers must " +
            "appear, even if the section content is just `—`.\n" +
            "- Output language follows the transcript dominant language.\n" +
            "- NEVER invent: participants, dates, amounts, project names, technical terms, " +
            "deadlines, organizational affiliations, or anything not directly evidenced in " +
            "the transcript. If unsure, omit rather than fabricate.\n" +
            "- Quotes (`\"...\"`) must be VERBATIM from the transcript. If you can't find " +
            "an exact match, do NOT include the quote — paraphrase outside of quote marks " +
            "or omit entirely.\n" +
            "- Convert ELAPSED_MS timestamps to MM:SS for display only. Use the original " +
            "[N ms] only for internal reasoning if needed.\n" +
            "- No filler phrases (\"the meeting discussed\", \"various topics were covered\", " +
            "\"in conclusion\", \"overall\").\n" +
            "- No em-dashes (`—`) in prose outside the markers and bullet separators. " +
            "Use periods, commas, or colons instead.\n" +
            "- Be SHARP and CONCISE. Senior leaders read these summaries — every sentence " +
            "must earn its place.\n\n" +

            "## Transcript\n" + transcript;
    }

    private static Dictionary<string, string> ParseStructuredRecap(string raw)
    {
        // Extended set: includes the new Notion-style sections (CONTEXT,
        // HIGHLIGHTS, NARRATIVE, FOLLOWUPS) added in the v2 prompt. Older
        // recaps without these sections still parse cleanly — missing
        // markers just leave their dictionary entries unset.
        var keys = new[]
        {
            "CONTEXT",
            "TLDR",
            "HIGHLIGHTS",
            "NARRATIVE",
            "KEY_DECISIONS",
            "TOPICS",
            "ACTIONS",
            "OPEN_QUESTIONS",
            "RISKS",
            "NEXT_STEPS",
            "FOLLOWUPS",
        };
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
            content = content.TrimStart('#', ' ', '\n', '\r');
            result[key] = content;
        }
        return result;
    }

    // ── History sidebar ────────────────────────────────────────────

    public sealed class HistoryRow
    {
        public string Dir { get; set; } = "";
        public string Title { get; set; } = "";
        public string Subtitle { get; set; } = "";
        public string RightLabel { get; set; } = "";
    }

    private void HistorySearch_Changed(object sender, Microsoft.UI.Xaml.Controls.TextChangedEventArgs e)
        => LoadHistory();

    private void LoadHistory()
    {
        // Snapshot current selection so it survives a refresh.
        var prev = (HistoryList.SelectedItem as HistoryRow)?.Dir;
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
                string title = $"Meeting {d.Name[..Math.Min(8, d.Name.Length)]}";
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
                    RightLabel = File.Exists(Path.Combine(d.FullName, "recap.md")) ? "✓" : "",
                });
            }
            if (!string.IsNullOrEmpty(prev))
            {
                var match = HistoryItems.FirstOrDefault(r => r.Dir == prev);
                if (match != null) HistoryList.SelectedItem = match;
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

        // While a recording is in flight, browsing a past meeting is
        // OK — the recording continues in the background and the
        // RecordingBar (with Stop + Back-to-live) stays pinned at the
        // top of the main panel. Block only during the brief Processing
        // window where the UI is committed to the Stop animation.
        if (_state == MeetingState.Processing)
        {
            ShowToast("Wrapping up the current meeting — wait a moment.");
            HistoryList.SelectedItem = null;
            return;
        }

        try
        {
            // Stop any in-flight playback from the PREVIOUS meeting
            // before we swap in the new audio.wav source. Without this
            // step, the previous track would keep playing out via the
            // MediaPlayer audio session until LoadDoneAudioAsync fully
            // populates the new Source — and on a slow disk that gap
            // can be a couple of seconds.
            StopDoneMediaPlayback();
            _viewingMeetingDir = row.Dir;
            DoneTitle.Text = row.Title;
            DoneMeta.Text = row.Subtitle;
            ClearDoneCards();

            var recapPath = Path.Combine(row.Dir, "recap.md");
            if (File.Exists(recapPath))
            {
                var text = await File.ReadAllTextAsync(recapPath);
                TldrText.Text = text;
                TldrCard.Visibility = Visibility.Visible;
            }
            var txt = Path.Combine(row.Dir, "transcripts.txt");
            if (File.Exists(txt))
            {
                RawTranscriptText.Text = HumanizeTranscript(await File.ReadAllTextAsync(txt));
            }
            await LoadDoneAudioAsync(row.Dir);
            SetState(MeetingState.Done);
        }
        catch (Exception ex)
        {
            App.Log($"history select exc: {ex.Message}", "Meeting");
        }
    }

    // ── Toast (transient notice at bottom of main panel) ─────────

    private void ShowToast(string message)
    {
        ToastText.Text = message;
        ToastBar.Visibility = Visibility.Visible;
        var dq = DispatcherQueue.GetForCurrentThread();
        _toastTimer?.Stop();
        _toastTimer = dq.CreateTimer();
        _toastTimer.Interval = TimeSpan.FromSeconds(3);
        _toastTimer.IsRepeating = false;
        _toastTimer.Tick += (_, _) => ToastBar.Visibility = Visibility.Collapsed;
        _toastTimer.Start();
    }

    // ── Misc actions ──────────────────────────────────────────────

    private void OpenFolder_Click(object sender, RoutedEventArgs e)
    {
        var dir = _viewingMeetingDir ?? _activeMeetingDir;
        if (string.IsNullOrEmpty(dir)) return;
        try { Process.Start(new ProcessStartInfo { FileName = dir, UseShellExecute = true }); }
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

    private static string HumanizeTranscript(string raw)
    {
        if (string.IsNullOrEmpty(raw)) return raw;
        return System.Text.RegularExpressions.Regex.Replace(
            raw,
            @"\[\s*(\d+)\s*ms\s*\]",
            m =>
            {
                if (!long.TryParse(m.Groups[1].Value, out var ms)) return m.Value;
                var ts = TimeSpan.FromMilliseconds(ms);
                return ts.TotalHours >= 1
                    ? $"[{(int)ts.TotalHours}:{ts.Minutes:D2}:{ts.Seconds:D2}]"
                    : $"[{ts.Minutes:D2}:{ts.Seconds:D2}]";
            });
    }
}
