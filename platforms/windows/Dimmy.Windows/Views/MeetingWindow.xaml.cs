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
using Dimmy.Windows.Services;

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
    private string? _recordingNotesDir;      // meeting dir for live note writes (known at Start, before the first chunk)
    private string? _viewingMeetingDir;      // dir currently shown in main panel (may differ)
    private readonly System.Text.StringBuilder _liveTranscriptBuilder = new();
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

        // Meeting-type override picker: data-bind to the shared taxonomy and
        // default to "Auto-detect" (index 0). Picking a specific type before
        // (re)generating the recap biases its emphasis; the result shows as
        // the chip under the title.
        RecapTypePicker.ItemsSource = Helpers.MeetingRecapHelpers.MeetingTypes;
        RecapTypePicker.SelectedIndex = 0;

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
                    // Always-mix architecture (2026-05-08): the meeting
                    // recording is now decoupled from this window. The
                    // Rust core keeps capturing and transcribing whether
                    // the window is open, hidden, or destroyed; the pill
                    // is the persistent indicator. Closing this window
                    // is a UI-only action — no Cancel, no force-stop.
                    // When the user reopens MeetingWindow, the ctor's
                    // dimmy_meeting_is_active() probe re-attaches the
                    // UI to the in-flight session (existing logic).
                    if (_state == MeetingState.Processing)
                    {
                        // The narrow case where we're mid stop-flow
                        // (LLM recap in progress) — let it finish so we
                        // don't lose the recap output.
                        args.Cancel = true;
                        ShowToast("Wrapping up — wait a moment.");
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

        // Subscribe to the event-driven live-transcript pipe. Rust
        // emits a `meeting_chunk` event for every chunk processed by
        // the meeting worker (~15 s cadence); the AppViewModel re-
        // dispatches to MeetingChunkReceived on the UI thread.
        // Replaces the 2 s DispatcherTimer that previously polled
        // transcripts.txt for changes.
        if (App.Instance?.AppViewModel is { } vm)
        {
            vm.MeetingChunkReceived += OnMeetingChunkReceived;
            // Drive the Done-view regenerate progress bar from the
            // file_transcribe_progress event the core already emits per
            // chunk during dimmy_meeting_retranscribe.
            vm.FileTranscribeProgress += OnRegenTranscribeProgress;
            // React to externally-driven stop (pill stop button, call-detect
            // popup "Stop & recap"). Without this hook, the MeetingWindow
            // stays painted in Recording state for the entire recap LLM
            // duration (~10–30 s), the user assumes "nothing happened" and
            // hits Stop again — which racing the pill-side stop yields the
            // confusing `meeting stop failed rc=-1` log + the second click
            // pings a now-stopped meeting. Mirrors the visible Stop_Click
            // flow (flip to Processing, stop polls) and lets the existing
            // `NotifyMeetingRecapSaved` ⇒ `RefreshAndSelectDir` path land
            // the user on the Done view once the recap finishes.
            vm.PropertyChanged += OnAppVmPropertyChanged;
        }

        Closed += (_, __) =>
        {
            StopPolling();
            StopAmplitudePoll();
            if (App.Instance?.AppViewModel is { } vmClose)
            {
                vmClose.MeetingChunkReceived -= OnMeetingChunkReceived;
                vmClose.FileTranscribeProgress -= OnRegenTranscribeProgress;
                vmClose.PropertyChanged -= OnAppVmPropertyChanged;
            }

            // Stop any audio.wav playback (MediaPlayerElement). Without
            // this, the underlying MediaPlayer keeps holding the audio
            // session and the file plays out in background even though
            // the window is gone — user has no UI to pause it. Standard
            // best practice for media UI: window close = playback stop.
            StopDoneMediaPlayback();

            // Recording is decoupled from this window — DO NOT force-stop
            // the meeting in core. The Rust worker keeps writing the
            // streaming WAVs + transcripts.txt independently. Reopening
            // MeetingWindow re-attaches via dimmy_meeting_is_active() in
            // the ctor. To stop a meeting, the user must reopen this
            // window and click Stop (or, future, use a tray-menu / pill
            // affordance — not yet wired).
            if (_recordingActive)
            {
                App.Log(
                    "Closed: meeting still active in core — leaving capture running, will re-attach on reopen",
                    "Meeting");
            }
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
                _liveTranscriptBuilder.Clear();
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
        // Whenever we enter Done, default the tab strip to "Recap" so
        // the user lands on the summary regardless of which tab was
        // visible the last time they were in Done. Cheap idempotent
        // call — safe even if SelectedTab is already "recap".
        if (s == MeetingState.Done) SelectDoneTab("recap");
        // Entering Recording always lands on the live transcript tab,
        // even after a previous meeting left the Notes tab selected.
        if (s == MeetingState.Recording) SetRecTab("transcript");
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

    /// XamlRoot for hosting modal dialogs (e.g. the consent gate) anchored to
    /// this window. Null until the window's content is in the visual tree.
    public XamlRoot? DialogRoot => this.Content?.XamlRoot;

    /// Awaitable variant: resolves once the window content is loaded and its
    /// XamlRoot is live. The call-detect path opens this window and immediately
    /// wants to show the consent dialog — right after Activate() the XamlRoot
    /// isn't established yet, so a ContentDialog.ShowAsync would throw and the
    /// consent gate would wrongly read as "declined". Wait for Loaded first.
    public async Task<XamlRoot?> EnsureDialogRootAsync()
    {
        if (this.Content is not FrameworkElement fe) return this.Content?.XamlRoot;
        if (fe.IsLoaded && fe.XamlRoot != null) return fe.XamlRoot;
        var tcs = new TaskCompletionSource<XamlRoot?>();
        RoutedEventHandler? handler = null;
        handler = (_, __) =>
        {
            fe.Loaded -= handler;
            tcs.TrySetResult(fe.XamlRoot);
        };
        fe.Loaded += handler;
        if (fe.IsLoaded)
        {
            fe.Loaded -= handler;
            return fe.XamlRoot;
        }
        return await tcs.Task;
    }

    private async void Start_Click(object sender, RoutedEventArgs e)
    {
        await BeginStartAsync(fromCallDetect: false);
    }

    /// Public entry for the call-detect "Record now" path. Runs the SAME
    /// consent + start + UI wiring as the in-window Start button (so the window
    /// actually flips to Recording), but forces recap on. Returns true if a
    /// meeting is now recording. Centralising start here is what fixes the
    /// call-detect path leaving the window idle: App used to start the meeting
    /// in the core directly, bypassing all this wiring.
    public Task<bool> StartFromCallDetectAsync() => BeginStartAsync(fromCallDetect: true);

    private async Task<bool> BeginStartAsync(bool fromCallDetect)
    {
        StartBtn.IsEnabled = false;
        try
        {
            // Recording-consent gate (mandatory). A meeting captures system
            // audio = other people, so we confirm consent and announce before
            // a single sample is recorded. If the user cancels, abort the start.
            var lang = System.Globalization.CultureInfo.CurrentUICulture.TwoLetterISOLanguageName;
            if (!await ConsentFlow.ConfirmAndAnnounceAsync(this.Content?.XamlRoot, lang))
            {
                StartBtn.IsEnabled = true;
                return false;
            }

            // Capture the recap choice NOW (start time) into a shared flag so
            // EVERY stop path honours it — the checkbox lives only in this
            // window and the meeting lifecycle is decoupled (window can close,
            // pill/popup stop independently), so the live checkbox isn't
            // reachable from those paths. Call-detect starts force recap on.
            if (App.Instance?.AppViewModel != null)
                App.Instance.AppViewModel.MeetingGenerateRecap =
                    fromCallDetect || GenerateRecapCheck.IsChecked == true;
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
                    _liveTranscriptBuilder.Clear();
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
                    return true;
                }
                ShowToast("Could not start the meeting. Check the log.");
                StartBtn.IsEnabled = true;
                return false;
            }
            var id = System.Text.Encoding.UTF8.GetString(buf, 0, rc);
            _startedAt = DateTime.UtcNow;
            _liveTranscriptBuilder.Clear();
            _ampHistory.Clear();
            _ampHistorySystem.Clear();
            _activeMeetingDir = null;       // first meeting_chunk event will set it
            // Notes can be jotted before the first chunk lands, so resolve
            // the meeting dir now from the id (meetingsDir/<id>) rather than
            // waiting for the chunk event that fills _activeMeetingDir.
            _recordingNotesDir = string.IsNullOrEmpty(id)
                ? null
                : System.IO.Path.Combine(Services.BuildInfo.MeetingsDirPath, id);
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
            return true;
        }
        catch (Exception ex)
        {
            App.Log($"meeting start exc: {ex}", "Meeting");
            StartBtn.IsEnabled = true;
            return false;
        }
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
            RefreshRecapWithClaudeButton();

            // Surface a finalize failure (disk-full → incomplete audio on
            // disk). The core has always serialized `error`; the host never
            // read it — the user learned about the damage only when the
            // recap failed. Audit 2026-07-02: no silent failures.
            string? stopError = root.TryGetProperty("error", out var errEl)
                && errEl.ValueKind == JsonValueKind.String
                ? errEl.GetString()
                : null;
            if (!string.IsNullOrWhiteSpace(stopError))
            {
                App.Log($"meeting stop reported error: {stopError}", "Meeting");
                Services.DictNotificationService.ShowMeetingAudioIncomplete(stopError!);
            }

            // Prefer the LLM-chosen title from meta.json (the recap's
            // first H1 H1 gets parsed + stored there by save_post_process).
            // Falls back to the meeting id directory name.
            DoneTitle.Text = ResolveDoneTitle(dir);
            DoneMeta.Text = $"{FormatDuration(dur)} · {chunks} chunks · {DateTime.Now:yyyy-MM-dd HH:mm}"
                + (string.IsNullOrWhiteSpace(stopError) ? "" : " · ⚠ audio incomplete");
            Helpers.TranscriptRenderer.Render(RawTranscriptText,
                string.IsNullOrEmpty(transcript)
                    ? "(no transcript: VAD may have removed all audio)"
                    : HumanizeTranscript(transcript));
            await LoadDoneAudioAsync(dir);
            await LoadNotesAsync(dir);

            bool wantRecap = App.Instance?.AppViewModel?.MeetingGenerateRecap ?? true;
            if (wantRecap && !string.IsNullOrWhiteSpace(transcript))
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
        // Toggle pause/resume on the in-flight meeting. The Rust
        // worker keeps cpal capturing in the background but stops
        // writing WAVs / emitting STT chunks while paused. On
        // resume the worker advances past the gap (no zombie audio
        // in audio.wav) and writes a `[paused N ms]` line into
        // transcripts.txt at the seam.
        if (!_recordingActive)
        {
            ShowToast("No active meeting to pause.");
            return;
        }
        try
        {
            int currentlyPaused = DimmyNative.dimmy_meeting_is_paused();
            if (currentlyPaused == 1)
            {
                int rc = DimmyNative.dimmy_meeting_resume();
                App.Log($"meeting resume rc={rc}", "Meeting");
                UpdatePauseButtonUi(paused: false);
                ShowToast("Resumed.");
            }
            else
            {
                int rc = DimmyNative.dimmy_meeting_pause();
                App.Log($"meeting pause rc={rc}", "Meeting");
                UpdatePauseButtonUi(paused: true);
                ShowToast("Paused — audio + transcript skipped until you resume.");
            }
        }
        catch (Exception ex)
        {
            App.Log($"pause/resume exc: {ex.Message}", "Meeting");
        }
    }

    private void UpdatePauseButtonUi(bool paused)
    {
        // E769 = pause glyph, E768 = play glyph. Keep StopBtn separate.
        if (PauseBtnIcon != null)
            PauseBtnIcon.Glyph = paused ? "" : "";
        if (PauseBtnLabel != null)
            PauseBtnLabel.Text = paused ? "Resume" : "Pause";
    }

    // ── Recording-view tabs (Live transcript ↔ Notes) ────────────
    /// Switch the recording card between the live transcript and the
    /// Notes composer. Mirrors the Done view's tab styling.
    private void RecTab_PointerPressed(
        object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        var tag = (sender as FrameworkElement)?.Tag as string ?? "transcript";
        SetRecTab(tag);
    }

    private void SetRecTab(string tab)
    {
        bool notes = tab == "notes";
        if (TranscriptScroll != null)
            TranscriptScroll.Visibility = notes ? Visibility.Collapsed : Visibility.Visible;
        if (RecNotesPanel != null)
            RecNotesPanel.Visibility = notes ? Visibility.Visible : Visibility.Collapsed;
        if (RecTranscriptTabUnderline != null)
            RecTranscriptTabUnderline.Visibility = notes ? Visibility.Collapsed : Visibility.Visible;
        if (RecNotesTabUnderline != null)
            RecNotesTabUnderline.Visibility = notes ? Visibility.Visible : Visibility.Collapsed;
        if (RecTranscriptTabLabel != null) RecTranscriptTabLabel.Opacity = notes ? 0.6 : 1.0;
        if (RecNotesTabLabel != null) RecNotesTabLabel.Opacity = notes ? 1.0 : 0.6;
        if (notes && NoteInput != null) NoteInput.Focus(FocusState.Programmatic);
    }

    // ── Live notes (unified with the Done view's Notes tab) ───────
    // The listener jots a free, multi-line note mid-meeting. "Add note"
    // (or Ctrl+Enter) stamps the current meeting time and APPENDS the
    // block to notes.md — the exact same file the Done view's Notes tab
    // loads/edits, and the one MeetingPostProcessService feeds to the
    // recap as the listener's high-priority emphasis. One notes store,
    // usable during AND after the meeting. Plain Enter = newline.
    private void NoteInput_KeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        if (e.Key != global::Windows.System.VirtualKey.Enter) return;
        var ctrl = Microsoft.UI.Input.InputKeyboardSource
            .GetKeyStateForCurrentThread(global::Windows.System.VirtualKey.Control)
            .HasFlag(global::Windows.UI.Core.CoreVirtualKeyStates.Down);
        if (!ctrl) return;   // plain Enter inserts a newline (multi-line)
        e.Handled = true;
        SubmitNote();
    }

    private void AddNote_Click(object sender, RoutedEventArgs e) => SubmitNote();

    private void SubmitNote()
    {
        if (!_recordingActive)
        {
            ShowToast("Notes attach to an active recording.");
            return;
        }
        var text = NoteInput.Text?.Trim() ?? string.Empty;
        if (string.IsNullOrEmpty(text)) return;
        var dir = _activeMeetingDir ?? _recordingNotesDir;
        if (string.IsNullOrEmpty(dir))
        {
            ShowToast("One moment — the meeting is still spinning up.");
            return;
        }
        try
        {
            var elapsed = DateTime.UtcNow - _startedAt;
            string ts = elapsed.TotalHours >= 1
                ? $"{(int)elapsed.TotalHours}:{elapsed.Minutes:D2}:{elapsed.Seconds:D2}"
                : $"{elapsed.Minutes:D2}:{elapsed.Seconds:D2}";
            // Markdown block: bold timestamp, the note body, blank-line
            // separator. Appended so notes.md is a running, crash-safe log.
            var block = $"**[{ts}]** {text}\n\n";
            System.IO.File.AppendAllText(System.IO.Path.Combine(dir, "notes.md"), block);
            NoteInput.Text = string.Empty;
            ShowToast($"Note added at {ts}.");
        }
        catch (Exception ex)
        {
            App.Log($"add note exc: {ex.Message}", "Meeting");
            ShowToast("Couldn't save the note. Try again.");
        }
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

    /// <summary>
    /// Reveal the AI-generated notice above the recap cards, with the wording
    /// the Rust core owns so Windows, macOS and Linux stay identical. Falls
    /// back to the English already in the XAML if the FFI returns nothing.
    /// </summary>
    private void ShowAiNotice()
    {
        if (AiNoticeCard == null) return;
        try
        {
            // Same source the consent notice uses (App.xaml.cs): the OS UI
            // language, not the STT language. The notice speaks to the person
            // reading the screen, not to the audio.
            var lang = System.Globalization.CultureInfo.CurrentUICulture.TwoLetterISOLanguageName;
            var title = Interop.DimmyNative.AiNoticeText("title", lang);
            var hint = Interop.DimmyNative.AiNoticeText("hint", lang);
            if (!string.IsNullOrWhiteSpace(title)) AiNoticeTitle.Text = title;
            if (!string.IsNullOrWhiteSpace(hint)) AiNoticeHint.Text = hint;
        }
        catch
        {
            // Wording is a nicety; the notice showing at all is the obligation.
        }
        AiNoticeCard.Visibility = Visibility.Visible;
    }

    private void ClearDoneCards()
    {
        AiNoticeCard.Visibility = Visibility.Collapsed;
        ContextCard.Visibility = Visibility.Collapsed;
        TldrCard.Visibility = Visibility.Collapsed;
        HighlightsCard.Visibility = Visibility.Collapsed;
        NarrativeCard.Visibility = Visibility.Collapsed;
        DecisionsCard.Visibility = Visibility.Collapsed;
        TopicsCard.Visibility = Visibility.Collapsed;
        ActionsCard.Visibility = Visibility.Collapsed;
        OpenQuestionsCard.Visibility = Visibility.Collapsed;
        RisksCard.Visibility = Visibility.Collapsed;
        NextStepsCard.Visibility = Visibility.Collapsed;
        FollowupsCard.Visibility = Visibility.Collapsed;
        DoneWaveCard.Visibility = Visibility.Collapsed;
        _lastDoneSections = new();
        ContextText.Blocks.Clear();
        TldrText.Blocks.Clear();
        HighlightsText.Blocks.Clear();
        NarrativeText.Blocks.Clear();
        DecisionsText.Blocks.Clear();
        TopicsText.Blocks.Clear();
        ActionsText.Blocks.Clear();
        OpenQuestionsText.Blocks.Clear();
        RisksText.Blocks.Clear();
        NextStepsText.Blocks.Clear();
        FollowupsText.Blocks.Clear();
    }

    // ── Recording-time tick + amplitude ──────────────────────────
    //
    // The transcript / chunk count UI is driven by the Rust core's
    // `meeting_chunk` event (see OnMeetingChunkReceived). The only
    // thing this timer drives is the elapsed-time label — a 1 Hz
    // tick on a continuously-changing wall clock, which is a
    // CLAUDE.md-documented legitimate exception to the no-polling
    // rule ("continuous sampling that has no event semantics").
    //
    // Burned 2026-05-14: this timer USED to ALSO poll transcripts.txt
    // every 2 s, scan the meetings dir for the latest folder, file-
    // stat it, read its contents, and re-render the whole transcript
    // even when nothing had changed. That's the polling pattern
    // CLAUDE.md explicitly forbids — replaced by the event-driven
    // path so the window now uses zero CPU between chunks.

    private void StartPolling()
    {
        var dq = DispatcherQueue.GetForCurrentThread();
        _pollTimer = dq.CreateTimer();
        _pollTimer.Interval = TimeSpan.FromSeconds(1);
        _pollTimer.IsRepeating = true;
        _pollTimer.Tick += OnRecTimerTick;
        _pollTimer.Start();
    }

    private void StopPolling()
    {
        if (_pollTimer == null) return;
        _pollTimer.Stop();
        _pollTimer.Tick -= OnRecTimerTick;
        _pollTimer = null;
    }

    private void OnRecTimerTick(DispatcherQueueTimer sender, object args)
    {
        var elapsed = DateTime.UtcNow - _startedAt;
        RecTimer.Text = $"{(int)elapsed.TotalHours:D2}:{elapsed.Minutes:D2}:{elapsed.Seconds:D2}";
    }

    /// Subscribed in OnLoaded / unsubscribed in OnClosed. Replaces
    /// the transcripts.txt polling — Rust emits exactly one
    /// `meeting_chunk` per chunk processed; we append the new line
    /// to the in-memory transcript and bump the counter. Idempotent
    /// + race-safe even when the user has navigated to a past
    /// meeting (we ignore events whose `dir` doesn't match the
    /// active recording).
    private void OnMeetingChunkReceived(
        string dir, string speaker, string line, long elapsedMs, int chunkCount)
    {
        // Drop events from a stale recording (e.g. a meeting that
        // ended a moment ago whose final chunks are still landing).
        // Match by suffix so we tolerate path normalisation diffs.
        if (string.IsNullOrEmpty(_activeMeetingDir)) {
            _activeMeetingDir = dir;
        } else if (!dir.Equals(_activeMeetingDir, StringComparison.OrdinalIgnoreCase)) {
            return;
        }

        // Append the new line to the live transcript view. The
        // line already ends with '\n' from Rust.
        _liveTranscriptBuilder.Append(line);
        TranscriptText.Text = HumanizeTranscript(_liveTranscriptBuilder.ToString());
        RecChunks.Text = $"{chunkCount} chunks";
        TranscriptMeta.Text = $"{chunkCount} chunks";
        TranscriptScroll?.ChangeView(null, double.MaxValue, null, true);
    }

    /// Mirror an external meeting-stop (pill, call-detect popup,
    /// future tray menu…) into MeetingWindow's visible state. The
    /// `meeting_state` event is the single source of truth — when
    /// `active` flips false while we still think we're recording,
    /// flip to Processing so the user sees the wrap-up animation
    /// instead of a stuck Recording panel. `NotifyMeetingRecapSaved`
    /// (called by the same external stop path after the recap LLM
    /// returns) will then route us to Done via `RefreshAndSelectDir`.
    private void OnAppVmPropertyChanged(
        object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(ViewModels.AppViewModel.MeetingActive)) return;
        var vm = App.Instance?.AppViewModel;
        if (vm == null) return;
        if (vm.MeetingActive) return;        // start transition — handled elsewhere
        if (!_recordingActive) return;        // already wrapped up locally
        DispatcherQueue?.TryEnqueue(() =>
        {
            try
            {
                App.Log("meeting_state=false from external stop — flipping MeetingWindow to Processing", "Meeting");
                StopPolling();
                StopAmplitudePoll();
                _recordingActive = false;
                StopBtn.IsEnabled = false;
                SetState(MeetingState.Processing);
                ResetProcSteps();
                SetProcStep(1, true);
                // ProcStep 2 (recap) is in flight on the pill side;
                // NotifyMeetingRecapSaved will land us on Done with
                // the final artefacts once the LLM returns.
                SetProcStep(2, false);
            }
            catch (Exception ex) { App.Log($"on-meeting-active-false exc: {ex.Message}", "Meeting"); }
        });
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
        // at a glance. Mic on top half (system accent — tracks the
        // user's Windows accent colour), system on bottom half
        // (LimeGreen — kept as a contrast track since accent might
        // itself be blue/teal). Each band centered on its own midline
        // so bars grow up + down equally within their half.
        double pitch = AMP_BAR_PX + AMP_GAP_PX;
        double bandHeight = h / 2.0;
        double midTop = bandHeight / 2.0;
        double midBottom = bandHeight + bandHeight / 2.0;
        // Mic uses the resolved-theme accent brush (Dark2 on light,
        // Light2 on dark — matches what AccentFillColorDefaultBrush
        // does in XAML ThemeResource lookups but respects the window's
        // RequestedTheme override, which Application.Resources doesn't).
        var brushMic = Helpers.ThemeHelper.ResolvedAccentBrush();
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

    // Resolved per-track paths for the playback source selector (Mix / Voice
    // / System). Without headphones the mix carries the AEC/NS/AGC-processed
    // mic on top of the clean loopback, so the system-only track sounds
    // cleaner for system-audio content — let the user pick which to hear.
    /// Resolve a meeting audio track to its on-disk file: prefer the
    /// compressed `.ogg` (current format), fall back to legacy `.wav`
    /// (older recordings / WAV-fallback when the Vorbis encoder was
    /// unavailable). Null if neither exists.
    private static string? ResolveAudioTrack(string dir, string baseName)
    {
        var ogg = Path.Combine(dir, baseName + ".ogg");
        if (File.Exists(ogg)) return ogg;
        var wav = Path.Combine(dir, baseName + ".wav");
        if (File.Exists(wav)) return wav;
        return null;
    }

    private async Task LoadDoneAudioAsync(string dir)
    {
        if (string.IsNullOrEmpty(dir)) return;
        var mixPath = ResolveAudioTrack(dir, "audio");
        if (mixPath == null)
        {
            DoneWaveCard.Visibility = Visibility.Collapsed;
            _cachedDonePeaks = null;
            _cachedMicPeaks = null;
            _cachedSystemPeaks = null;
            return;
        }
        // Refuse a near-empty file — the "interrupted recording" case
        // where the stream never finalised. Hide the card instead of
        // feeding the MediaPlayer garbage.
        try
        {
            if (new FileInfo(mixPath).Length < 64)
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
            DoneAudioPlayer.Source = global::Windows.Media.Core.MediaSource.CreateFromUri(new Uri(mixPath));
            var mp = DoneAudioPlayer.MediaPlayer;
            if (mp != null)
            {
                mp.PlaybackSession.PositionChanged -= OnDonePlaybackPositionChanged;
                mp.PlaybackSession.PositionChanged += OnDonePlaybackPositionChanged;
            }

            // Fixed bucket count (NOT width-derived): the peaks are decoded
            // once at this resolution, cached on disk keyed by (size,buckets)
            // so re-opens are instant, and DrawDoneWaveform maps them across
            // whatever the current canvas width is (responsive). 400 bars is
            // a good density for any reasonable card width.
            const int buckets = 400;

            // Decode peaks SEQUENTIALLY, not in parallel. Three concurrent
            // full-file Vorbis decodes of a long meeting (each materialises
            // the whole track in memory) hammered CPU/RAM and intermittently
            // returned empty for one track — the waveform then showed a
            // single band (and re-transcription-grade decode is proven fine,
            // see the standalone check). One at a time is reliable; the disk
            // cache makes every later open instant regardless.
            // ReadPeaksAny handles both .ogg (Symphonia FFI) and .wav.
            var micPath = ResolveAudioTrack(dir, "audio_mic");
            var systemPath = ResolveAudioTrack(dir, "audio_system");
            var mixForPeaks = mixPath;

            var micPeaks = micPath != null
                ? await Task.Run(() => Helpers.WavPeaks.ReadPeaksAny(micPath, buckets))
                : System.Array.Empty<float>();
            var sysPeaks = systemPath != null
                ? await Task.Run(() => Helpers.WavPeaks.ReadPeaksAny(systemPath, buckets))
                : System.Array.Empty<float>();

            _cachedMicPeaks = micPeaks.Length > 0 ? micPeaks : null;
            _cachedSystemPeaks = sysPeaks.Length > 0 ? sysPeaks : null;

            // The mix is only the single-band FALLBACK (old single-track
            // recordings, or a per-track decode that came back empty). When
            // both per-track bands are present we don't need it — skip that
            // third decode (faster first open).
            _cachedDonePeaks = (_cachedMicPeaks != null && _cachedSystemPeaks != null)
                ? null
                : await Task.Run(() => Helpers.WavPeaks.ReadPeaksAny(mixForPeaks, buckets));

            _doneFrac = 0; // fresh meeting starts un-played
            DrawDoneWaveform();
        }
        catch (Exception ex)
        {
            App.Log($"LoadDoneAudio exc: {ex.Message}", "Meeting");
        }
    }

    private Microsoft.UI.Xaml.Shapes.Rectangle? _donePlayhead;
    // Redesigned waveform: a faint "unplayed" layer underneath + a full-colour
    // "played" layer on top, clipped to [0..playX]. Progress is an O(1) clip
    // resize (no per-frame redraw). Knob marks the playhead.
    private Microsoft.UI.Xaml.Controls.Canvas? _wavePlayedLayer;
    private Microsoft.UI.Xaml.Shapes.Ellipse? _doneKnob;
    private double _doneFrac;

    // Brand gradient endpoints (logo): green #2ECE8E → violet #6E7DF7.
    private static readonly global::Windows.UI.Color WaveGreen =
        global::Windows.UI.Color.FromArgb(0xFF, 0x2E, 0xCE, 0x8E);
    private static readonly global::Windows.UI.Color WaveViolet =
        global::Windows.UI.Color.FromArgb(0xFF, 0x6E, 0x7D, 0xF7);

    private static global::Windows.UI.Color WaveLerp(double t)
    {
        t = Math.Max(0, Math.Min(1, t));
        byte L(byte a, byte b) => (byte)Math.Round(a + (b - a) * t);
        return global::Windows.UI.Color.FromArgb(0xFF,
            L(WaveGreen.R, WaveViolet.R), L(WaveGreen.G, WaveViolet.G), L(WaveGreen.B, WaveViolet.B));
    }

    /// Resample a peaks array to exactly <paramref name="n"/> bars by taking the
    /// max in each slice (so spikes survive). Handles n &gt; src.Length (repeats)
    /// and n &lt; src.Length (downsamples) — the path that fills the card for any
    /// recording length.
    private static float[] ResampleMax(float[] src, int n)
    {
        var outp = new float[n];
        if (src.Length == 0) return outp;
        double step = (double)src.Length / n;
        for (int i = 0; i < n; i++)
        {
            int a = (int)(i * step), b = (int)((i + 1) * step);
            if (b <= a) b = a + 1;
            if (b > src.Length) b = src.Length;
            float mx = 0;
            for (int j = a; j < b; j++) if (src[j] > mx) mx = src[j];
            outp[i] = mx;
        }
        return outp;
    }

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
        _doneFrac = Math.Max(0, Math.Min(1, frac));
        double playX = w * _doneFrac;

        // Reveal the full-colour "played" layer up to the playhead by
        // resizing its clip. Cheap — no bar rebuild.
        if (_wavePlayedLayer != null)
            _wavePlayedLayer.Clip = new Microsoft.UI.Xaml.Media.RectangleGeometry
            {
                Rect = new global::Windows.Foundation.Rect(0, 0, playX, h),
            };

        // Thin guide line.
        if (_donePlayhead == null || !DoneWaveformCanvas.Children.Contains(_donePlayhead))
        {
            _donePlayhead = new Microsoft.UI.Xaml.Shapes.Rectangle
            {
                Width = 1.5, Height = h,
                Fill = new SolidColorBrush(global::Windows.UI.Color.FromArgb(0x99, 0xC7, 0xCB, 0xD6)),
                IsHitTestVisible = false,
            };
            Microsoft.UI.Xaml.Controls.Canvas.SetTop(_donePlayhead, 0);
            DoneWaveformCanvas.Children.Add(_donePlayhead);
        }
        Microsoft.UI.Xaml.Controls.Canvas.SetLeft(_donePlayhead, Math.Max(0, Math.Min(w - 1.5, playX)));

        // Circular knob (white fill, violet ring + dot).
        const double kr = 7;
        if (_doneKnob == null || !DoneWaveformCanvas.Children.Contains(_doneKnob))
        {
            _doneKnob = new Microsoft.UI.Xaml.Shapes.Ellipse
            {
                Width = kr * 2, Height = kr * 2,
                Fill = new SolidColorBrush(Microsoft.UI.Colors.White),
                Stroke = new SolidColorBrush(WaveViolet),
                StrokeThickness = 2.4,
                IsHitTestVisible = false,
            };
            DoneWaveformCanvas.Children.Add(_doneKnob);
        }
        Microsoft.UI.Xaml.Controls.Canvas.SetLeft(_doneKnob, Math.Max(0, Math.Min(w - kr * 2, playX - kr)));
        Microsoft.UI.Xaml.Controls.Canvas.SetTop(_doneKnob, h / 2.0 - kr);
    }

    // Bar geometry: slim + dense. 3 px bar, 2 px gap → the count adapts to the
    // card width, so any recording length fills the card (long = each bar is the
    // peak over a longer slice ⇒ fuller/smoother; short = finer detail).
    private const double WaveBarW = 3.0;
    private const double WaveBarGap = 2.0;

    private void DrawDoneWaveform()
    {
        if (DoneWaveformCanvas == null) return;
        DoneWaveformCanvas.Children.Clear();
        _donePlayhead = null;
        _doneKnob = null;
        _wavePlayedLayer = null;
        double w = DoneWaveformCanvas.ActualWidth;
        double h = DoneWaveformCanvas.ActualHeight;
        if (w <= 0 || h <= 0) return;

        // Pick the bands. Prefer per-track (mic up / system down); fall back to
        // a single decoded band mirrored, or the combined mix mirrored.
        float[]? mic = (_cachedMicPeaks != null && _cachedMicPeaks.Length > 0) ? _cachedMicPeaks : null;
        float[]? sys = (_cachedSystemPeaks != null && _cachedSystemPeaks.Length > 0) ? _cachedSystemPeaks : null;
        if (mic == null && sys == null)
        {
            var mix = (_cachedDonePeaks != null && _cachedDonePeaks.Length > 0) ? _cachedDonePeaks : null;
            if (mix == null) return;
            mic = mix; sys = mix;
        }
        else { mic ??= sys; sys ??= mic; }

        double slot = WaveBarW + WaveBarGap;
        int n = Math.Max(1, (int)(w / slot));
        var mN = ResampleMax(mic!, n);
        var sN = ResampleMax(sys!, n);

        // Normalise so the loudest bar nearly fills the half-height — keeps a
        // quiet recording from looking flat without clipping a loud one.
        float max = 0.05f;
        for (int i = 0; i < n; i++) { if (mN[i] > max) max = mN[i]; if (sN[i] > max) max = sN[i]; }
        double norm = Math.Min(1.0 / max, 3.2) * 0.92;

        double mid = h / 2.0;
        double maxH = mid - 4;

        // Faint center hairline.
        var hair = new Microsoft.UI.Xaml.Shapes.Rectangle
        {
            Width = w, Height = 1,
            Fill = new SolidColorBrush(global::Windows.UI.Color.FromArgb(0x33, 0xC7, 0xCB, 0xD6)),
            IsHitTestVisible = false,
        };
        Microsoft.UI.Xaml.Controls.Canvas.SetTop(hair, mid - 0.5);
        DoneWaveformCanvas.Children.Add(hair);

        // Two layers: faint "unplayed" base + full-colour "played" on top
        // (clipped to the playhead in UpdateDonePlayhead).
        var unplayed = new Microsoft.UI.Xaml.Controls.Canvas { IsHitTestVisible = false };
        var played = new Microsoft.UI.Xaml.Controls.Canvas { IsHitTestVisible = false };

        void Bar(Microsoft.UI.Xaml.Controls.Canvas layer, double x, double top, double bh,
                 global::Windows.UI.Color color, byte alpha)
        {
            if (bh < 1.5) bh = 1.5;
            var c = global::Windows.UI.Color.FromArgb(alpha, color.R, color.G, color.B);
            var r = new Microsoft.UI.Xaml.Shapes.Rectangle
            {
                Width = WaveBarW, Height = bh, RadiusX = WaveBarW / 2, RadiusY = WaveBarW / 2,
                Fill = new SolidColorBrush(c),
            };
            Microsoft.UI.Xaml.Controls.Canvas.SetLeft(r, x);
            Microsoft.UI.Xaml.Controls.Canvas.SetTop(r, top);
            layer.Children.Add(r);
        }

        for (int i = 0; i < n; i++)
        {
            double x = i * slot;
            double hm = Math.Max(1.5, mN[i] * norm * maxH);
            double hs = Math.Max(1.5, sN[i] * norm * maxH);
            // Vertical gradient illusion: colour each bar by its centre's Y.
            var cm = WaveLerp((mid - hm / 2) / h);
            var cs = WaveLerp((mid + hs / 2) / h);
            // mic grows UP from mid, system grows DOWN.
            Bar(played, x, mid - hm, hm, cm, 0xFF);
            Bar(played, x, mid, hs, cs, 0xFF);
            Bar(unplayed, x, mid - hm, hm, cm, 0x47);   // ~28% — colored, faded
            Bar(unplayed, x, mid, hs, cs, 0x47);
        }

        DoneWaveformCanvas.Children.Add(unplayed);
        DoneWaveformCanvas.Children.Add(played);
        _wavePlayedLayer = played;

        UpdateDonePlayhead(_doneFrac);
    }

    private Microsoft.UI.Dispatching.DispatcherQueueTimer? _waveResizeTimer;

    private void DoneWaveformCanvas_SizeChanged(object sender, SizeChangedEventArgs e)
    {
        if (_cachedDonePeaks == null && _cachedMicPeaks == null && _cachedSystemPeaks == null) return;
        if (e.NewSize.Width <= 0 || e.NewSize.Height <= 0) return;
        // DEBOUNCE the redraw. Redrawing on every SizeChanged during a
        // window drag rebuilds hundreds of Canvas rectangles dozens of
        // times a second and froze the UI thread ("tutto piantato"). Wait
        // ~120 ms of resize silence, then redraw once at the final width.
        if (_waveResizeTimer == null)
        {
            _waveResizeTimer = DispatcherQueue.CreateTimer();
            _waveResizeTimer.Interval = TimeSpan.FromMilliseconds(120);
            _waveResizeTimer.IsRepeating = false;
            _waveResizeTimer.Tick += (s, _) => DrawDoneWaveform();
        }
        _waveResizeTimer.Stop();
        _waveResizeTimer.Start();
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

    private async Task GeneratePostProcessAsync(string dir, string transcript, string meetingType = "")
    {
        try
        {
            var prompt = Helpers.MeetingRecapHelpers.BuildStructuredRecapPrompt(transcript, "", meetingType);
            var modelOverride = PickRecapModel();
            App.Log($"recap with model='{modelOverride}', prompt {prompt.Length} chars", "Meeting");
            BeginLiveRecap();
            var buf = new byte[1 << 18];
            // 16000 max_tokens leaves room for the Anthropic extended-thinking
            // budget (10000) + the actual response (~4-6k tokens for a rich
            // Notion-style recap). Without enough headroom Anthropic rejects
            // the request with "max_tokens must be greater than budget_tokens".
            int rc = await Task.Run(() =>
                DimmyNative.dimmy_llm_call_raw(prompt, modelOverride, 16000, buf, buf.Length));
            if (rc <= 0)
            {
                var msg = Services.MeetingPostProcessService
                    .RecapRcToUserMessage(rc, modelOverride);
                // Recap-specific failures (-5/-6/-7) point at the
                // recap config. Pop a "Open Recap settings" CTA so
                // the user has one click between "recap failed" and
                // the picker they need to fix.
                var actionable = rc == -5 || rc == -6 || rc == -7;
                ShowDoneFallback(transcript, msg, actionable);
                return;
            }
            var raw = System.Text.Encoding.UTF8.GetString(buf, 0, rc);
            var sections = Helpers.MeetingRecapHelpers.ParseStructuredRecap(raw);

            ApplyDoneSections(sections);

            var recapMarkdown = Helpers.MeetingRecapHelpers.BuildMarkdownFromSections(sections);
            var actionsPlain = sections.GetValueOrDefault("ACTIONS", "");
            DimmyNative.dimmy_meeting_save_post_process(dir, recapMarkdown, actionsPlain, null,
                string.IsNullOrWhiteSpace(modelOverride) ? null : modelOverride);

            // Best-effort copy into the user's export folder (Obsidian /
            // Drive / Dropbox sync). No-op when unconfigured; never throws.
            Services.RecapExportService.TryExport(dir, sections.GetValueOrDefault("__TITLE__", ""));
        }
        catch (Exception ex)
        {
            App.Log($"post-process exc: {ex}", "Meeting");
            ShowDoneFallback(transcript, $"Post-process failed: {ex.Message}");
        }
    }

    /// Last parsed/displayed Done-view sections. Kept around so
    /// CopyRecap_Click can rebuild the markdown without having to
    /// re-introspect the rendered RichTextBlocks (which only hold
    /// formatted Inline trees, not the source markdown).
    private Dictionary<string, string> _lastDoneSections = new();

    private void ShowDoneFallback(string transcript, string note)
        => ShowDoneFallback(transcript, note, false);

    private void ShowDoneFallback(string transcript, string note, bool actionable)
    {
        // Failure path too: leaving the live pane up would show a half-written
        // recap next to the error that killed it.
        EndLiveRecap();
        SetPlainText(TldrText, note);
        TldrCard.Visibility = Visibility.Visible;
        // Show the "Open Recap settings" CTA only when the error is
        // something the user can plausibly fix from that page (model
        // mismatch / auth / rate limit). For generic -3/-4/-8 the
        // button would route them to the wrong fix.
        OpenRecapSettingsButton.Visibility = actionable
            ? Visibility.Visible : Visibility.Collapsed;
        // Recovery path: regardless of WHY the configured backend
        // failed, if Claude Desktop + the dimmy MCP bridge are
        // connected the user has a working alternative that needs no
        // API key or CLI. Surface it here so a silent recap failure
        // turns into one click to the path that works.
        RefreshClaudeFallbackCta();
    }

    /// Toggle the "Generate recap with Claude Desktop" recovery CTA in
    /// the Done-view fallback card. Visible only when the dimmy MCP
    /// extension is installed in Claude Desktop. Loads the real Claude
    /// brand mark (same source as the toolbar button) asynchronously.
    private void RefreshClaudeFallbackCta()
    {
        if (RecapWithClaudeFallbackButton == null) return;
        try
        {
            var status = Interop.DimmyNative.GetClaudeDesktopStatus();
            RecapWithClaudeFallbackButton.Visibility = status.ExtensionInstalled
                ? Visibility.Visible
                : Visibility.Collapsed;
            if (status.ExtensionInstalled)
            {
                _ = System.Threading.Tasks.Task.Run(async () =>
                {
                    var iconPath = await Helpers.ClaudeIconExtractor.TryExtractAsync();
                    if (!string.IsNullOrEmpty(iconPath))
                    {
                        DispatcherQueue.TryEnqueue(() =>
                        {
                            if (RecapWithClaudeFallbackIcon != null)
                            {
                                RecapWithClaudeFallbackIcon.Source =
                                    new Microsoft.UI.Xaml.Media.Imaging.BitmapImage(new Uri(iconPath));
                            }
                        });
                    }
                });
            }
        }
        catch (Exception ex)
        {
            App.Log($"RefreshClaudeFallbackCta exc: {ex.Message}", "ClaudeDesktop");
        }
    }

    /// Tab switcher on the Done view. The XAML wires PointerPressed
    /// on each header StackPanel; the `Tag` carries the tab id
    /// ("recap" / "transcript" / "notes"). One panel is visible at a
    /// time + only the selected tab's label is full-emphasis with the
    /// accent underline shown.
    private void DoneTab_PointerPressed(object sender,
        Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (sender is not FrameworkElement fe) return;
        var tag = fe.Tag as string ?? "recap";
        SelectDoneTab(tag);
    }

    /// Apply the selected-tab state — content panel visibility +
    /// label foreground + underline. Default selection on a fresh
    /// Done view is "recap" so the user lands on the summary.
    private void SelectDoneTab(string tab)
    {
        bool recap = tab == "recap";
        bool transcript = tab == "transcript";
        bool notes = tab == "notes";
        RecapTabPanel.Visibility = recap ? Visibility.Visible : Visibility.Collapsed;
        TranscriptTabPanel.Visibility = transcript ? Visibility.Visible : Visibility.Collapsed;
        NotesTabPanel.Visibility = notes ? Visibility.Visible : Visibility.Collapsed;

        RecapTabUnderline.Visibility = recap ? Visibility.Visible : Visibility.Collapsed;
        TranscriptTabUnderline.Visibility = transcript ? Visibility.Visible : Visibility.Collapsed;
        NotesTabUnderline.Visibility = notes ? Visibility.Visible : Visibility.Collapsed;

        // Selected tab → full opacity + SemiBold. Unselected →
        // primary brush at 0.6 opacity (much more legible than the
        // SecondaryBrush which goes near-invisible on Light theme).
        RecapTabLabel.Opacity = recap ? 1.0 : 0.6;
        TranscriptTabLabel.Opacity = transcript ? 1.0 : 0.6;
        NotesTabLabel.Opacity = notes ? 1.0 : 0.6;

        RecapTabLabel.FontWeight = recap
            ? Microsoft.UI.Text.FontWeights.SemiBold
            : Microsoft.UI.Text.FontWeights.Normal;
        TranscriptTabLabel.FontWeight = transcript
            ? Microsoft.UI.Text.FontWeights.SemiBold
            : Microsoft.UI.Text.FontWeights.Normal;
        NotesTabLabel.FontWeight = notes
            ? Microsoft.UI.Text.FontWeights.SemiBold
            : Microsoft.UI.Text.FontWeights.Normal;
    }

    /// Persist the Notes tab textbox to `<meetingDir>/notes.md` on
    /// LostFocus. Local-only for now — not threaded into the recap
    /// LLM prompt yet. Empty + non-existing file = no-op so we don't
    /// litter the meeting dirs with zero-byte files.
    private async void NotesBox_LostFocus(object sender, RoutedEventArgs e)
    {
        var dir = _viewingMeetingDir ?? _activeMeetingDir;
        if (string.IsNullOrEmpty(dir) || !Directory.Exists(dir)) return;
        try
        {
            var path = Path.Combine(dir, "notes.md");
            var text = NotesBox.Text ?? "";
            if (string.IsNullOrWhiteSpace(text))
            {
                if (File.Exists(path)) File.Delete(path);
            }
            else
            {
                await File.WriteAllTextAsync(path, text);
            }
        }
        catch (Exception ex)
        {
            App.Log($"NotesBox_LostFocus exc: {ex.Message}", "Meeting");
        }
    }

    /// Load the saved notes.md (if present) into the Notes textbox
    /// when opening a meeting from the sidebar or finishing one
    /// live. Empty file / missing → empty textbox.
    private async Task LoadNotesAsync(string dir)
    {
        try
        {
            var path = Path.Combine(dir, "notes.md");
            if (NotesBox == null) return;
            NotesBox.Text = File.Exists(path)
                ? await File.ReadAllTextAsync(path)
                : "";
        }
        catch (Exception ex)
        {
            App.Log($"LoadNotesAsync exc: {ex.Message}", "Meeting");
            if (NotesBox != null) NotesBox.Text = "";
        }
    }

    /// "Open Recap settings" CTA on the Done view fallback — fires
    /// when a recap failed with a recap-config-related rc (-5/-6/-7)
    /// and opens Settings scrolled to the Recap section.
    private void OpenRecapSettings_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            // Plain settings open — the user-facing error message
            // names the section ("Settings → Recap") so scrolling
            // them there manually is a 1-click step. A future
            // improvement would be a deep-link parameter.
            App.Instance?.OpenSettingsWindow();
        }
        catch (Exception ex)
        {
            App.Log($"OpenRecapSettings_Click exc: {ex.Message}", "Meeting");
        }
    }

    /// Show the model's text as it is written. Streaming providers can take
    /// half a minute before the first word, and an empty panel for that long
    /// reads as a hang rather than as work in progress.
    ///
    /// Subscribes for the duration of one recap and unsubscribes in
    /// EndLiveRecap — a leaked handler here would keep appending a later
    /// command-mode stream into a stale meeting window.
    private void BeginLiveRecap()
    {
        var vm = App.Instance?.AppViewModel;
        if (vm == null || LiveRecapCard == null) return;
        vm.LlmStreamText = "";
        vm.LlmThinkingText = "";
        if (LiveRecapText != null) LiveRecapText.Text = "";
        LiveRecapCard.Visibility = Visibility.Visible;
        vm.PropertyChanged -= OnLlmStreamChanged;
        vm.PropertyChanged += OnLlmStreamChanged;
    }

    private void EndLiveRecap()
    {
        var vm = App.Instance?.AppViewModel;
        if (vm != null) vm.PropertyChanged -= OnLlmStreamChanged;
        if (LiveRecapCard != null) LiveRecapCard.Visibility = Visibility.Collapsed;
    }

    private void OnLlmStreamChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(ViewModels.AppViewModel.LlmStreamText)
            && e.PropertyName != nameof(ViewModels.AppViewModel.LlmThinkingText)) return;
        var vm = App.Instance?.AppViewModel;
        var text = vm?.LlmStreamText ?? "";
        // Before the first answer token, show the reasoning instead — an opus
        // recap thinks for well over a minute, and an empty pane for that long
        // is what made users ask whether it had died. The moment real text
        // arrives the answer takes the pane over.
        var showingThinking = text.Length == 0;
        if (showingThinking) text = vm?.LlmThinkingText ?? "";
        // The event arrives on the core's thread; the UI has its own.
        DispatcherQueue?.TryEnqueue(() =>
        {
            if (LiveRecapText == null) return;
            // Tail only: the finished recap is rendered by the cards below,
            // so this pane is a progress signal, not the deliverable.
            var tail = text.Length > 1200 ? text[^1200..] : text;
            LiveRecapText.Text = showingThinking && tail.Length > 0
                ? $"Thinking...\n{tail}"
                : tail;
        });
    }

    private void ApplyDoneSections(Dictionary<string, string> sections)
    {
        EndLiveRecap();
        _lastDoneSections = sections;
        // EU AI Act art. 50(5). Only here, not in ShowDoneFallback: that path
        // renders an error message, which is Dimmy talking, not AI-generated
        // content. Marking it would be noise and would train the user to
        // ignore the notice where it actually matters.
        ShowAiNotice();
        // A successful recap clears any leftover failure CTAs from a
        // prior attempt (e.g. a regenerate that finally worked).
        if (OpenRecapSettingsButton != null)
            OpenRecapSettingsButton.Visibility = Visibility.Collapsed;
        if (RecapWithClaudeFallbackButton != null)
            RecapWithClaudeFallbackButton.Visibility = Visibility.Collapsed;
        // Meeting-type chip from the parsed __TYPE__ sentinel. Hidden when
        // unresolved (auto/unknown → FriendlyTypeLabel returns null).
        var typeLabel = Helpers.MeetingRecapHelpers.FriendlyTypeLabel(
            sections.GetValueOrDefault("__TYPE__", ""));
        if (typeLabel != null)
        {
            DoneTypeChipText.Text = typeLabel;
            DoneTypeChip.Visibility = Visibility.Visible;
        }
        else
        {
            DoneTypeChip.Visibility = Visibility.Collapsed;
        }
        // Reflect the resolved type in the override picker so the user sees
        // what the recap currently is and can re-pick + regenerate. Absent /
        // unresolved __TYPE__ → "auto". The parser already normalised the key.
        if (RecapTypePicker != null)
        {
            var rt = sections.GetValueOrDefault("__TYPE__", "");
            RecapTypePicker.SelectedValue = string.IsNullOrWhiteSpace(rt) ? "auto" : rt;
        }
        // CONTEXT + HIGHLIGHTS + NARRATIVE + FOLLOWUPS were defined
        // in the prompt but silently dropped by the UI before — the
        // four cards below were missing. Without them the LLM's
        // best paragraphs (Narrative) never reached the user.
        ApplyDoneSection(sections, "CONTEXT", ContextText, ContextCard);
        ApplyDoneSection(sections, "TLDR", TldrText, TldrCard);
        ApplyDoneSection(sections, "HIGHLIGHTS", HighlightsText, HighlightsCard);
        ApplyDoneSection(sections, "NARRATIVE", NarrativeText, NarrativeCard);
        ApplyDoneSection(sections, "KEY_DECISIONS", DecisionsText, DecisionsCard);
        ApplyDoneSection(sections, "TOPICS", TopicsText, TopicsCard);
        ApplyDoneSection(sections, "ACTIONS", ActionsText, ActionsCard);
        ApplyDoneSection(sections, "OPEN_QUESTIONS", OpenQuestionsText, OpenQuestionsCard);
        ApplyDoneSection(sections, "RISKS", RisksText, RisksCard);
        ApplyDoneSection(sections, "NEXT_STEPS", NextStepsText, NextStepsCard);
        ApplyDoneSection(sections, "FOLLOWUPS", FollowupsText, FollowupsCard);
    }

    private static void ApplyDoneSection(
        Dictionary<string, string> sections, string key,
        Microsoft.UI.Xaml.Controls.RichTextBlock target,
        Microsoft.UI.Xaml.Controls.Border card)
    {
        if (!sections.TryGetValue(key, out var value) || string.IsNullOrWhiteSpace(value) || value.Trim() == "—")
        {
            card.Visibility = Visibility.Collapsed;
            return;
        }
        Helpers.MarkdownRenderer.Render(target, value.Trim());
        card.Visibility = Visibility.Visible;
    }

    /// Set a plain-text string into a RichTextBlock as a single
    /// paragraph — used for fallback / error messages where
    /// markdown rendering would be overkill.
    private static void SetPlainText(Microsoft.UI.Xaml.Controls.RichTextBlock target, string text)
    {
        target.Blocks.Clear();
        var p = new Microsoft.UI.Xaml.Documents.Paragraph();
        p.Inlines.Add(new Microsoft.UI.Xaml.Documents.Run { Text = text ?? "" });
        target.Blocks.Add(p);
    }

    // SplitMarkdownIntoSections moved into the unified parser in
    // Helpers/MeetingRecapHelpers.cs::ParseStructuredRecap — that parser
    // now accepts both the wire (`===NAME===`) and storage (`## Heading`)
    // formats in a single pass, with synonym tolerance. Burned 2026-05-30.

    // BuildMarkdownFromSections moved to Helpers/MeetingRecapHelpers.cs
    // so the pure logic is unit-testable. Internal callers now go via
    // the helper directly.

    /// Cross-class facade for PickRecapModel — `MeetingPostProcessService`
    /// reaches through this entry point. The pure prompt + parser +
    /// markdown helpers now live in <see cref="Helpers.MeetingRecapHelpers"/>;
    /// PickRecapModel stays here because it depends on the AppViewModel
    /// (not pure).
    internal static string PickRecapModelInternal() => PickRecapModel();

    private static string PickRecapModel()
    {
        // Order of precedence:
        //   1. explicit user override (recap_model_override), with a stale-id
        //      migration so old saved Gemini ids still resolve.
        //   2. "Auto" → the best reasoning model among the providers the user
        //      actually has a key for, using the CURATED, valid model ids from
        //      the recap dropdown. Quality order: Anthropic Opus > OpenAI
        //      GPT-5 > Gemini Pro.
        //   3. empty → Rust falls back to llm_api_model (the user's own
        //      dictation model on their own provider).
        //
        // CRITICAL: the has_<vendor>_*key flags only exist in the LIVE FFI
        // snapshot (dimmy_get_config_json), NOT in the config.json FILE — the
        // Rust core computes them and strips them on save. Reading the file
        // would see all-false and Auto would never pick a premium model.
        try
        {
            var buf = new byte[1 << 14];
            int n = Interop.DimmyNative.dimmy_get_config_json(buf, buf.Length);
            if (n <= 0) return "";
            using var doc = JsonDocument.Parse(System.Text.Encoding.UTF8.GetString(buf, 0, n));
            var root = doc.RootElement;

            // 1. Explicit override (migrate stale ids).
            if (root.TryGetProperty("recap_model_override", out var ovEl))
            {
                var ov = ovEl.GetString();
                if (!string.IsNullOrWhiteSpace(ov))
                    return MigrateRecapModelId(ov.Trim());
            }

            // 2. Auto: best model among the user's connected providers. A
            //    provider counts as connected if a key exists in ANY scope.
            bool Has(params string[] flags) =>
                flags.Any(f => root.TryGetProperty(f, out var e)
                               && e.ValueKind == System.Text.Json.JsonValueKind.True);

            if (Has("has_anthropic_llm_key", "has_anthropic_recap_key"))
                return "claude-opus-4-8";
            if (Has("has_openai_key", "has_openai_llm_key", "has_openai_recap_key"))
                return "gpt-5.5";
            if (Has("has_gemini_key", "has_gemini_llm_key", "has_gemini_recap_key"))
                return "gemini-3.1-pro-preview";

            // 3. Only a non-premium provider (e.g. Groq) → empty, so Rust uses
            //    the user's configured llm_api_model. Honest errors now guide
            //    the user if that model/tier can't handle the recap size.
            return "";
        }
        catch
        {
            return "";
        }
    }

    /// Migrate stale recap model ids saved by older builds to their current
    /// valid form. The recap dropdown switched the Gemini Pro id to the
    /// `-preview` suffix; a config that still carries the bare id 404s
    /// ("models/gemini-3.1-pro is not found"). Mirror of the LlmApiModel
    /// migration in SettingsViewModel.LoadFromJson.
    private static string MigrateRecapModelId(string id) => id switch
    {
        "gemini-3.1-pro" => "gemini-3.1-pro-preview",
        "gemini-3-1-pro" => "gemini-3.1-pro-preview",
        "gemini-3-pro" => "gemini-3-pro-preview",
        _ => id,
    };

    // BuildStructuredRecapPrompt + ParseStructuredRecap moved to
    // Helpers/MeetingRecapHelpers.cs so the pure prompt + parser are
    // unit-testable without spinning up a XAML host. Internal callers
    // go via Helpers.MeetingRecapHelpers.X(...).

    // ── History sidebar ────────────────────────────────────────────

    public sealed class HistoryRow : System.ComponentModel.INotifyPropertyChanged
    {
        public string Dir { get; set; } = "";
        public string Title { get; set; } = "";
        public string Subtitle { get; set; } = "";
        public string RightLabel { get; set; } = "";

        // IsActive drives the left-edge accent stripe in the row's
        // DataTemplate — flipped from HistoryList_SelectionChanged because
        // WinUI 3 doesn't support RelativeSource AncestorType bindings
        // (which would otherwise let us bind directly to ListViewItem.IsSelected).
        private bool _isActive;
        public bool IsActive
        {
            get => _isActive;
            set
            {
                if (_isActive == value) return;
                _isActive = value;
                PropertyChanged?.Invoke(this, new System.ComponentModel.PropertyChangedEventArgs(nameof(IsActive)));
            }
        }
        public event System.ComponentModel.PropertyChangedEventHandler? PropertyChanged;
    }

    private void HistorySearch_Changed(object sender, Microsoft.UI.Xaml.Controls.TextChangedEventArgs e)
        => LoadHistory();

    private async void HistoryRowDelete_Click(object sender, RoutedEventArgs e)
    {
        // The Delete button lives inside each row's DataTemplate, so we
        // pick the bound HistoryRow off DataContext rather than the
        // selected item — the user might delete a row they haven't
        // opened. Confirms before destroying the meeting dir on disk.
        if (sender is not Microsoft.UI.Xaml.Controls.Button btn) return;
        if (btn.DataContext is not HistoryRow row) return;
        if (string.IsNullOrEmpty(row.Dir)) return;

        var dlg = new Microsoft.UI.Xaml.Controls.ContentDialog
        {
            RequestedTheme = Dimmy.Windows.Helpers.ThemeHelper.ResolvedElementTheme(),
            Title = "Delete this meeting?",
            Content = $"This will permanently remove:\n\n{Path.GetFileName(row.Dir)}\n\n" +
                      "Includes audio (audio.wav, per-track WAVs), transcripts.txt, and recap.md.",
            PrimaryButtonText = "Delete",
            CloseButtonText = "Cancel",
            DefaultButton = Microsoft.UI.Xaml.Controls.ContentDialogButton.Close,
            XamlRoot = this.Content.XamlRoot,
        };
        var result = await dlg.ShowAsync();
        if (result != Microsoft.UI.Xaml.Controls.ContentDialogResult.Primary) return;

        // Stop in-flight playback if the deleted meeting is the one the
        // MediaPlayer is currently sourcing. Avoids a brief MEDIA_OPEN
        // race when the file disappears mid-decode.
        if (string.Equals(_viewingMeetingDir, row.Dir, StringComparison.OrdinalIgnoreCase))
        {
            StopDoneMediaPlayback();
            ClearDoneCards();
            _viewingMeetingDir = null;
        }

        try
        {
            if (Directory.Exists(row.Dir))
            {
                // Recursive: includes audio_*.wav, transcripts.txt,
                // recap.md, meta.json, plus any future per-meeting
                // artifacts we add (word_timestamps, follow-ups…).
                Directory.Delete(row.Dir, recursive: true);
                App.Log($"deleted meeting dir {row.Dir}", "Meeting");
            }
        }
        catch (Exception ex)
        {
            App.Log($"delete meeting dir failed: {ex.Message}", "Meeting");
            ShowToast($"Delete failed: {ex.Message}");
            return;
        }

        HistoryItems.Remove(row);
        if (HistoryList.SelectedItem == row) HistoryList.SelectedItem = null;
        ShowToast("Meeting deleted.");
    }

    /// Public entry point used by App.NotifyMeetingRecapSaved when the
    /// pill-Stop recap pipeline completes while this window is open.
    /// Refreshes the sidebar from disk and auto-selects the row whose
    /// Dir matches `dir` — driving HistoryList_SelectionChanged which
    /// loads recap.md, transcripts.txt, audio waveform into the Done
    /// view. No-op if the row isn't found (very unlikely — the recap
    /// path just wrote to that dir before calling us).
    public void RefreshAndSelectDir(string dir)
    {
        if (string.IsNullOrEmpty(dir)) return;
        // External stop path leaves us pinned in Processing via
        // OnAppVmPropertyChanged; HistoryList_SelectionChanged would
        // refuse the selection and toast the user. Flip to Done first
        // so the selection-changed handler loads recap.md / audio.wav
        // into the Done view.
        if (_state == MeetingState.Processing)
        {
            ResetProcSteps();
            SetProcStep(1, true);
            SetProcStep(2, true);
            SetProcStep(3, true);
            SetState(MeetingState.Done);
        }
        LoadHistory();
        foreach (var row in HistoryItems)
        {
            if (string.Equals(row.Dir, dir, StringComparison.OrdinalIgnoreCase))
            {
                HistoryList.SelectedItem = row;
                return;
            }
        }
    }

    private void LoadHistory()
    {
        // Snapshot current selection so it survives a refresh.
        var prev = (HistoryList.SelectedItem as HistoryRow)?.Dir;
        HistoryItems.Clear();
        try
        {
            var meetings = Services.BuildInfo.MeetingsDirPath;
            if (!Directory.Exists(meetings)) return;
            var query = (HistorySearchBox.Text ?? "").Trim();
            // Sort by CreationTime, NOT LastWriteTime — the directory's mtime
            // gets bumped every time we regenerate peaks / save a recap edit /
            // touch any sibling file, which used to scramble the list order
            // and put yesterday's meetings at the top whenever a background
            // job touched their folder. Burned 2026-05-30.
            var dirs = new DirectoryInfo(meetings).GetDirectories()
                .OrderByDescending(d => d.CreationTime)
                .ToList();
            foreach (var d in dirs)
            {
                var meta = Path.Combine(d.FullName, "meta.json");
                // Default title is "Meeting <first-8-chars-of-uuid>" —
                // gets overridden below when meta.json carries a real
                // title set by the recap LLM (via save_post_process or
                // the MCP save_recap path).
                string title = $"Meeting {d.Name[..Math.Min(8, d.Name.Length)]}";
                // Date hierarchy (most authoritative → least):
                //   1. meta.json `started_at`    — unix seconds, written by Rust at meeting start
                //   2. meta.json `ended_at - duration_secs` — back-computed for older meta schemas
                //   3. meta.json `ended_at`       — last resort if duration missing
                //   4. directory CreationTime    — folder mkdir at meeting start (NOT LastWrite,
                //                                  which gets bumped every time we regen peaks
                //                                  / save a recap edit / touch any file inside)
                // Burned 2026-05-30: every meeting list row showed today's date because
                // a peaks regen run had updated the dir mtime.
                DateTime? startDt = d.CreationTime;
                if (File.Exists(meta))
                {
                    try
                    {
                        using var doc = JsonDocument.Parse(File.ReadAllText(meta));
                        if (doc.RootElement.TryGetProperty("title", out var t)
                            && t.ValueKind == JsonValueKind.String)
                        {
                            var metaTitle = t.GetString();
                            if (!string.IsNullOrEmpty(metaTitle)) title = metaTitle;
                        }
                        double startedAt = 0, endedAt = 0, durationSecs = 0;
                        if (doc.RootElement.TryGetProperty("started_at", out var sa)
                            && sa.ValueKind == JsonValueKind.Number)
                            startedAt = sa.GetDouble();
                        if (doc.RootElement.TryGetProperty("ended_at", out var ea)
                            && ea.ValueKind == JsonValueKind.Number)
                            endedAt = ea.GetDouble();
                        if (doc.RootElement.TryGetProperty("duration_secs", out var ds)
                            && ds.ValueKind == JsonValueKind.Number)
                            durationSecs = ds.GetDouble();
                        if (startedAt > 0)
                            startDt = DateTimeOffset.FromUnixTimeMilliseconds((long)(startedAt * 1000)).LocalDateTime;
                        else if (endedAt > 0 && durationSecs > 0)
                            startDt = DateTimeOffset.FromUnixTimeMilliseconds((long)((endedAt - durationSecs) * 1000)).LocalDateTime;
                        else if (endedAt > 0)
                            startDt = DateTimeOffset.FromUnixTimeMilliseconds((long)(endedAt * 1000)).LocalDateTime;
                        // duration suffix handled after subtitle string is built.
                    }
                    catch { }
                }
                string subtitle = (startDt ?? d.CreationTime).ToString("yyyy-MM-dd HH:mm");
                if (File.Exists(meta))
                {
                    try
                    {
                        using var doc2 = JsonDocument.Parse(File.ReadAllText(meta));
                        if (doc2.RootElement.TryGetProperty("duration_secs", out var dur)
                            && dur.ValueKind == JsonValueKind.Number)
                        {
                            subtitle += " · " + FormatDuration(dur.GetDouble());
                        }
                    }
                    catch { }
                }
                // Lazy backfill: if meta has no title but recap.md
                // exists, parse the first H1 from the recap and
                // write it back. Mirrors the Rust
                // `meeting::backfill_meeting_title` logic (kept
                // client-side here too so the sidebar gets the title
                // even before the user opens the meeting Done view).
                if (title.StartsWith("Meeting ", StringComparison.Ordinal))
                {
                    var recapPath = Path.Combine(d.FullName, "recap.md");
                    if (File.Exists(recapPath))
                    {
                        try
                        {
                            var firstH1 = ParseFirstH1(File.ReadAllText(recapPath));
                            if (!string.IsNullOrEmpty(firstH1))
                            {
                                title = firstH1;
                                WriteMetaTitle(meta, firstH1);
                            }
                        }
                        catch { }
                    }
                }
                if (!string.IsNullOrEmpty(query) &&
                    !title.Contains(query, StringComparison.OrdinalIgnoreCase) &&
                    !subtitle.Contains(query, StringComparison.OrdinalIgnoreCase) &&
                    // Include the dir name (full UUID) so users who
                    // search by partial UUID still get matches —
                    // before the LLM-chosen title landed in meta.json,
                    // the title field IS the prefixed UUID and search
                    // hit it naturally; after the schema change the
                    // title is human-friendly and UUID search would
                    // otherwise return nothing.
                    !d.Name.Contains(query, StringComparison.OrdinalIgnoreCase))
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
        // Drive the left-edge accent stripe via row.IsActive — sync first
        // (cheap O(n) for n ≤ ~100 rows), then proceed with the heavy
        // work (audio load, transcript open, etc.).
        foreach (var item in HistoryItems)
            item.IsActive = ReferenceEquals(item, HistoryList.SelectedItem);

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
            RefreshRecapWithClaudeButton();
            // Prefer meta.json title (set by save_post_process / MCP
            // save_recap from the first H1 of the recap). Falls back
            // to the sidebar row's pre-computed title which itself
            // already falls back to the meeting id.
            DoneTitle.Text = ResolveDoneTitle(row.Dir, fallback: row.Title);
            DoneMeta.Text = row.Subtitle;
            ClearDoneCards();

            var recapPath = Path.Combine(row.Dir, "recap.md");
            if (File.Exists(recapPath))
            {
                var text = await File.ReadAllTextAsync(recapPath);
                // Try the canonical `===NAME===` marker parser FIRST —
                // that's the format the SDK + MCP path produce. Fall
                // back to the heading-name parser (`## TL;DR` /
                // `## Decisions`) only if no canonical markers were
                // found (covers very old recaps from before the
                // marker-based prompt landed). Without this order the
                // sidebar-load path silently dropped every section of
                // marker-based recaps because the legacy heading
                // parser doesn't recognise `## ===CONTEXT===` etc.
                // Unified, permissive parser — accepts wire format
                // (`===NAME===`), persisted format (`## Heading` + synonyms),
                // hybrids, and falls back to TLDR=full-body if nothing matched.
                // See MeetingRecapHelpers.ParseStructuredRecap.
                var parsed = Helpers.MeetingRecapHelpers.ParseStructuredRecap(text);
                if (parsed.Count > 0)
                {
                    ApplyDoneSections(parsed);
                }
                else
                {
                    Helpers.MarkdownRenderer.Render(TldrText, text);
                    TldrCard.Visibility = Visibility.Visible;
                }
            }
            var txt = Path.Combine(row.Dir, "transcripts.txt");
            if (File.Exists(txt))
            {
                Helpers.TranscriptRenderer.Render(
                    RawTranscriptText,
                    HumanizeTranscript(await File.ReadAllTextAsync(txt)));
            }
            await LoadDoneAudioAsync(row.Dir);
            await LoadNotesAsync(row.Dir);
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

    /// Send the current meeting's recap.md to the user's configured
    /// Notion target. Called from the Done view button. Surfaces a
    /// success / failure dialog and offers "Open in Notion" link on
    /// success. Disabled flow until the user has both a token AND a
    /// target picked (Settings → Integrations).
    private async void SendToNotion_Click(object sender, RoutedEventArgs e)
    {
        var dir = _viewingMeetingDir ?? _activeMeetingDir;
        if (string.IsNullOrEmpty(dir))
        {
            App.Log("SendToNotion: no meeting dir", "Notion");
            return;
        }
        if (!Services.NotionService.HasToken())
        {
            await ShowNotionDialogAsync(
                "Notion not connected",
                "Connect to Notion in Settings → Integrations first. Paste your integration token, pick where recaps should land, then come back here.",
                isError: true,
                pageUrl: null);
            return;
        }
        if (sender is Microsoft.UI.Xaml.Controls.Button btn) { btn.IsEnabled = false; }
        try
        {
            App.Log($"SendToNotion: dir={dir}", "Notion");
            var result = await Services.NotionService.SendRecapAsync(dir);
            if (result.Ok)
            {
                App.Log($"SendToNotion: ok url={result.PageUrl}", "Notion");
                await ShowNotionDialogAsync(
                    "Sent to Notion ✓",
                    "Your recap is live in Notion. Click Open to view it.",
                    isError: false,
                    pageUrl: result.PageUrl);
            }
            else
            {
                App.Log($"SendToNotion: failed {result.Error}", "Notion");
                await ShowNotionDialogAsync(
                    "Couldn't send to Notion",
                    result.Error ?? "Unknown error",
                    isError: true,
                    pageUrl: null);
            }
        }
        catch (Exception ex)
        {
            App.Log($"SendToNotion exc: {ex}", "Notion");
            await ShowNotionDialogAsync(
                "Couldn't send to Notion",
                ex.Message,
                isError: true,
                pageUrl: null);
        }
        finally
        {
            if (sender is Microsoft.UI.Xaml.Controls.Button btn2) { btn2.IsEnabled = true; }
        }
    }

    /// Read `meta.json` from a meeting dir and return the `title`
    /// field. Falls back to `fallback` (default: meeting id from the
    /// dir name). Called whenever DoneTitle is set so the LLM-chosen
    /// title (parsed from the recap's first H1 by save_post_process)
    /// is preferred over the raw uuid the dir is named after. The
    /// title also lazy-backfills from recap.md if meta.json is
    /// missing it but recap.md has a parseable H1 — handled in Rust.
    // ── Title rename (click-to-edit) ──────────────────────────────

    private void DoneTitle_Tapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e)
    {
        if (string.IsNullOrEmpty(_viewingMeetingDir)) return;
        DoneTitleEdit.Text = DoneTitle.Text;
        DoneTitle.Visibility = Visibility.Collapsed;
        DoneTitleEdit.Visibility = Visibility.Visible;
        DoneTitleEdit.SelectAll();
        DoneTitleEdit.Focus(FocusState.Programmatic);
    }

    private void DoneTitleEdit_KeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        if (e.Key == global::Windows.System.VirtualKey.Enter)
        {
            CommitDoneTitleEdit(cancel: false);
            e.Handled = true;
        }
        else if (e.Key == global::Windows.System.VirtualKey.Escape)
        {
            CommitDoneTitleEdit(cancel: true);
            e.Handled = true;
        }
    }

    private void DoneTitleEdit_LostFocus(object sender, RoutedEventArgs e)
    {
        CommitDoneTitleEdit(cancel: false);
    }

    private void CommitDoneTitleEdit(bool cancel)
    {
        var dir = _viewingMeetingDir;
        DoneTitleEdit.Visibility = Visibility.Collapsed;
        DoneTitle.Visibility = Visibility.Visible;
        if (cancel || string.IsNullOrEmpty(dir)) return;
        var newTitle = (DoneTitleEdit.Text ?? "").Trim();
        if (newTitle.Length == 0 || newTitle.Length > 200) return;
        if (newTitle == DoneTitle.Text) return;
        try
        {
            WriteMetaTitle(Path.Combine(dir, "meta.json"), newTitle);
            DoneTitle.Text = newTitle;
            // Sidebar row carries its own Title snapshot AND has no
            // INotifyPropertyChanged, so a plain `row.Title = newTitle`
            // would mutate the model but not redraw the list. We do
            // the canonical fix: REPLACE the HistoryRow instance at
            // its current index — ObservableCollection's Replace
            // notification triggers a ListView re-render for that
            // single row. LoadHistory() as a fallback (covers the
            // case where the rename moved the row in sort order).
            var idx = -1;
            for (int i = 0; i < HistoryItems.Count; i++)
            {
                if (HistoryItems[i].Dir == dir) { idx = i; break; }
            }
            if (idx >= 0)
            {
                var old = HistoryItems[idx];
                HistoryItems[idx] = new HistoryRow
                {
                    Dir = old.Dir,
                    Title = newTitle,
                    Subtitle = old.Subtitle,
                    RightLabel = old.RightLabel,
                };
                HistoryList.SelectedItem = HistoryItems[idx];
            }
            else
            {
                LoadHistory();
            }
        }
        catch (Exception ex)
        {
            App.Log($"DoneTitle rename failed: {ex.Message}", "Meeting");
        }
    }

    /// Best-effort H1 extraction from a Markdown blob. Mirror of the
    /// Rust `meeting::parse_recap_title` rule: first non-empty line
    /// must start with `# `, title 1-200 chars, trailing
    /// punctuation/whitespace stripped.
    private static string? ParseFirstH1(string md)
    {
        if (string.IsNullOrEmpty(md)) return null;
        foreach (var raw in md.Split('\n'))
        {
            var line = raw.Trim();
            if (line.Length == 0) continue;
            if (line.StartsWith("# "))
            {
                var t = line.Substring(2).Trim().TrimEnd('.', ':', ',', ' ', '\t');
                if (t.Length == 0 || t.Length > 200) return null;
                return t;
            }
            return null;
        }
        return null;
    }

    /// Patch (or create) meta.json with a `title` field. Preserves
    /// every other key. Best-effort — title is metadata polish.
    private static void WriteMetaTitle(string metaPath, string title)
    {
        try
        {
            var dict = File.Exists(metaPath)
                ? (System.Text.Json.Nodes.JsonObject?)System.Text.Json.Nodes.JsonNode.Parse(
                    File.ReadAllText(metaPath))
                : null;
            dict ??= new System.Text.Json.Nodes.JsonObject();
            dict["title"] = title;
            File.WriteAllText(metaPath, dict.ToJsonString(
                new System.Text.Json.JsonSerializerOptions { WriteIndented = true }));
        }
        catch { }
    }

    private static string ResolveDoneTitle(string? dir, string? fallback = null)
    {
        if (string.IsNullOrEmpty(dir))
            return string.IsNullOrEmpty(fallback) ? "Meeting" : fallback;
        var idFallback = Path.GetFileName(dir.TrimEnd('\\', '/'));
        try
        {
            var metaPath = Path.Combine(dir, "meta.json");
            if (File.Exists(metaPath))
            {
                using var doc = System.Text.Json.JsonDocument.Parse(File.ReadAllText(metaPath));
                if (doc.RootElement.TryGetProperty("title", out var t)
                    && t.ValueKind == System.Text.Json.JsonValueKind.String)
                {
                    var s = t.GetString();
                    if (!string.IsNullOrEmpty(s)) return s;
                }
            }
        }
        catch
        {
            // Best-effort — meta.json is metadata polish.
        }
        if (!string.IsNullOrEmpty(fallback)) return fallback;
        return string.IsNullOrEmpty(idFallback) ? "Meeting" : idFallback;
    }

    /// Open Claude Desktop with a deeplink that instructs Claude to
    /// use the dimmy MCP tools to fetch the meeting's transcript +
    /// recap template and write a recap back. Visible only when the
    /// MCP extension is installed (set by RefreshRecapWithClaudeButton).
    private async void RecapWithClaudeDesktop_Click(object sender, RoutedEventArgs e)
    {
        var dir = _viewingMeetingDir ?? _activeMeetingDir;
        if (string.IsNullOrEmpty(dir)) return;
        var meetingId = System.IO.Path.GetFileName(dir.TrimEnd('\\', '/'));
        if (string.IsNullOrEmpty(meetingId)) return;

        // Tight prompt — Claude consults `dimmy_get_recap_template`
        // for the structure, so we don't have to inline the entire
        // format here. Saves the URL length and keeps the template
        // editable without re-shipping Dimmy.
        var prompt =
            $"Recap Dimmy meeting `{meetingId}`.\n\n" +
            "1. Call `dimmy_get_recap_template` to fetch Dimmy's house format.\n" +
            $"2. Call `dimmy_get_meeting` with id `{meetingId}` to read the transcript.\n" +
            "3. Produce a recap that follows the template's rules exactly (first line is a Markdown H1 title in the transcript's language).\n" +
            $"4. Call `dimmy_save_recap` with id `{meetingId}` and your recap markdown to persist it back into Dimmy.\n" +
            "5. Confirm to me once saved.";
        var deeplink = $"claude://claude.ai/new?q={System.Uri.EscapeDataString(prompt)}";

        // Always put the prompt on the clipboard first, so it's there no
        // matter how the deeplink below goes.
        try
        {
            var pkg = new global::Windows.ApplicationModel.DataTransfer.DataPackage();
            pkg.SetText(prompt);
            global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(pkg);
        }
        catch { }

        try
        {
            App.Log($"RecapWithClaude: opening deeplink for meeting={meetingId}", "ClaudeDesktop");
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(deeplink)
            {
                UseShellExecute = true,
            });
            ShowToast("Opening Claude Desktop. The prompt is on your clipboard too.");
        }
        catch (Exception ex)
        {
            App.Log($"RecapWithClaude exc: {ex.Message}", "ClaudeDesktop");
            // Deeplink failed: Claude Desktop not installed, or its URI
            // handler isn't registered. The prompt is already copied.
            ShowToast("Couldn't open Claude Desktop. Prompt copied, open Claude and paste it.");
        }
        await System.Threading.Tasks.Task.CompletedTask;
    }

    /// Refresh the visibility + icon of the "Recap with Claude Desktop"
    /// button on the meeting Done view. Called from the same code
    /// paths that swap the viewing meeting (LoadMeetingRow + the
    /// active-meeting attach in the stop handler).
    ///
    /// Visibility: only when the MCP extension is installed (status
    /// queried fresh from Rust; cheap, no FFI loop).
    /// Icon: the real Claude Desktop brand mark extracted from the
    /// installed MSIX package by `ClaudeIconExtractor`. NEVER use a
    /// hand-drawn placeholder — see memory note on this.
    private void RefreshRecapWithClaudeButton()
    {
        if (RecapWithClaudeBtn == null) return;
        try
        {
            var status = Interop.DimmyNative.GetClaudeDesktopStatus();
            RecapWithClaudeBtn.Visibility = status.ExtensionInstalled
                ? Visibility.Visible
                : Visibility.Collapsed;
            if (status.ExtensionInstalled)
            {
                _ = System.Threading.Tasks.Task.Run(async () =>
                {
                    var iconPath = await Helpers.ClaudeIconExtractor.TryExtractAsync();
                    if (!string.IsNullOrEmpty(iconPath))
                    {
                        DispatcherQueue.TryEnqueue(() =>
                        {
                            if (RecapWithClaudeIcon != null)
                            {
                                RecapWithClaudeIcon.Source =
                                    new Microsoft.UI.Xaml.Media.Imaging.BitmapImage(new Uri(iconPath));
                            }
                        });
                    }
                });
            }
        }
        catch (Exception ex)
        {
            App.Log($"RefreshRecapWithClaudeButton exc: {ex.Message}", "ClaudeDesktop");
        }
    }

    private async System.Threading.Tasks.Task ShowNotionDialogAsync(
        string title, string content, bool isError, string? pageUrl)
    {
        var dlg = new Microsoft.UI.Xaml.Controls.ContentDialog
        {
            RequestedTheme = Dimmy.Windows.Helpers.ThemeHelper.ResolvedElementTheme(),
            Title = title,
            Content = content,
            CloseButtonText = "Close",
            XamlRoot = this.Content.XamlRoot,
        };
        if (!isError && !string.IsNullOrEmpty(pageUrl))
        {
            dlg.PrimaryButtonText = "Open in Notion";
            dlg.DefaultButton = Microsoft.UI.Xaml.Controls.ContentDialogButton.Primary;
        }
        var result = await dlg.ShowAsync();
        if (result == Microsoft.UI.Xaml.Controls.ContentDialogResult.Primary && !string.IsNullOrEmpty(pageUrl))
        {
            try { await global::Windows.System.Launcher.LaunchUriAsync(new Uri(pageUrl)); }
            catch (Exception ex) { App.Log($"open notion url failed: {ex.Message}", "Notion"); }
        }
    }

    private void CopyRecap_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            // RichTextBlock doesn't expose a flat .Text — instead we
            // rebuild the markdown from the cached section dict that
            // ApplyDoneSections populated. This keeps the original
            // bullets / bold / italic intact when pasting elsewhere.
            string md;
            if (_lastDoneSections.Count > 0)
            {
                md = Helpers.MeetingRecapHelpers.BuildMarkdownFromSections(_lastDoneSections);
            }
            else
            {
                md = "";
            }
            if (string.IsNullOrWhiteSpace(md))
            {
                ShowToast("Nothing to copy yet.");
                return;
            }
            var dp = new global::Windows.ApplicationModel.DataTransfer.DataPackage();
            dp.SetText(md);
            global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(dp);
            ShowToast("Recap copied as markdown.");
        }
        catch (Exception ex)
        {
            App.Log($"copy recap exc: {ex.Message}", "Meeting");
        }
    }

    // ── Regenerate transcript / recap from on-disk artifacts ─────
    //
    // The Done view is reached two ways: (1) right after a fresh stop,
    // (2) by selecting a row in the sidebar history. In both cases the
    // backing files live in `_viewingMeetingDir`. These handlers reuse
    // the existing `dimmy_transcribe_file` FFI (which itself chunks at
    // ~25 MB internally for cloud providers) and the existing
    // `GeneratePostProcessAsync` recap pipeline — they're "fix-up"
    // affordances when the user catches that one of those steps failed
    // or was disabled at record time.

    private async void RegenerateTranscript_Click(object sender, RoutedEventArgs e)
    {
        var dir = _viewingMeetingDir ?? _activeMeetingDir;
        if (string.IsNullOrEmpty(dir) || !Directory.Exists(dir))
        {
            ShowToast("No meeting selected.");
            return;
        }
        if (_recordingActive)
        {
            ShowToast("Stop the current recording first.");
            return;
        }

        var btn = sender as Microsoft.UI.Xaml.Controls.Button;
        if (btn != null) btn.IsEnabled = false;
        try
        {
            // Determinate bar driven by file_transcribe_progress (per chunk).
            // NB: the percent is per-band, so on a mic+system mix it sweeps
            // 0→100 twice — acceptable; it still clearly shows live progress.
            RegenProgressBar.IsIndeterminate = false;
            RegenProgressBar.Value = 0;
            RegenProgressText.Text = "Transcribing audio…";
            RegenProgressPanel.Visibility = Visibility.Visible;
            var merged = await Task.Run(() => TranscribeMeetingDir(dir));
            if (string.IsNullOrWhiteSpace(merged))
            {
                ShowToast("No audio found in meeting folder.");
                return;
            }

            var txtPath = Path.Combine(dir, "transcripts.txt");
            await File.WriteAllTextAsync(txtPath, merged);
            Helpers.TranscriptRenderer.Render(RawTranscriptText, HumanizeTranscript(merged));
            ShowToast("Transcript regenerated.");
        }
        catch (Exception ex)
        {
            App.Log($"regen transcript exc: {ex}", "Meeting");
            ShowToast($"Regenerate transcript failed: {ex.Message}");
        }
        finally
        {
            if (btn != null) btn.IsEnabled = true;
            RegenProgressPanel.Visibility = Visibility.Collapsed;
        }
    }

    private async void RegenerateRecap_Click(object sender, RoutedEventArgs e)
    {
        var dir = _viewingMeetingDir ?? _activeMeetingDir;
        if (string.IsNullOrEmpty(dir) || !Directory.Exists(dir))
        {
            ShowToast("No meeting selected.");
            return;
        }

        var txtPath = Path.Combine(dir, "transcripts.txt");
        if (!File.Exists(txtPath))
        {
            ShowToast("No transcript yet — regenerate transcript first.");
            return;
        }

        var btn = sender as Microsoft.UI.Xaml.Controls.Button;
        if (btn != null) btn.IsEnabled = false;
        try
        {
            var transcript = await File.ReadAllTextAsync(txtPath);
            if (string.IsNullOrWhiteSpace(transcript))
            {
                ShowToast("Transcript is empty — regenerate transcript first.");
                return;
            }
            ClearDoneCards();
            // Honour the meeting-type override picker: "auto" (index 0) →
            // empty so the model classifies; a specific key biases emphasis.
            var meetingType = (RecapTypePicker?.SelectedValue as string) ?? "";
            if (meetingType == "auto") meetingType = "";
            // Recap is a single opaque LLM call — no progress events — so an
            // indeterminate bar that simply stays up for the whole call.
            RegenProgressBar.IsIndeterminate = true;
            RegenProgressText.Text = "Generating recap…";
            RegenProgressPanel.Visibility = Visibility.Visible;
            await GeneratePostProcessAsync(dir, transcript, meetingType);
            ShowToast("Recap regenerated.");
        }
        catch (Exception ex)
        {
            App.Log($"regen recap exc: {ex}", "Meeting");
            ShowToast($"Regenerate recap failed: {ex.Message}");
        }
        finally
        {
            if (btn != null) btn.IsEnabled = true;
            RegenProgressPanel.Visibility = Visibility.Collapsed;
        }
    }

    // Ticks the regenerate progress bar from the core's per-chunk
    // file_transcribe_progress event. Only acts while the panel is visible
    // and determinate (transcript regen) — the recap path uses an
    // indeterminate bar that ignores percent.
    private void OnRegenTranscribeProgress(double processedSecs, double totalSecs, double percent)
    {
        if (RegenProgressPanel.Visibility != Visibility.Visible) return;
        if (RegenProgressBar.IsIndeterminate) return;
        RegenProgressBar.Value = percent;
        RegenProgressText.Text = $"Transcribing audio… {(int)percent}%";
    }

    /// Run STT over whatever audio files exist in `dir`, returning the
    /// merged transcript. Mix recordings keep mic and system as separate
    /// WAVs and we label them so the LLM downstream can attribute lines;
    /// older recordings have only `audio.wav` and we transcribe that.
    /// Returns "" if no audio file is present.
    private static string TranscribeMeetingDir(string dir)
    {
        // Chunked, per-band re-transcription in the Rust core: rebuilds
        // transcripts.txt with `[<ms> ms] [mic|system] text` lines (speaker
        // turns + timestamps) like the live worker — instead of the old
        // blob-per-track path that collapsed each track into one `[mic]` /
        // `[system]` block with no turns or times. The FFI writes
        // transcripts.txt itself; the returned string is the merged text.
        var buf = new byte[1 << 22]; // 4 MB transcript ceiling
        int rc = DimmyNative.dimmy_meeting_retranscribe(dir, buf, buf.Length);
        if (rc <= 0)
        {
            App.Log($"meeting_retranscribe rc={rc}", "Meeting");
            return "";
        }
        return System.Text.Encoding.UTF8.GetString(buf, 0, rc);
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
