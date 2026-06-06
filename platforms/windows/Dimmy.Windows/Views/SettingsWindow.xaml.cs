using System;
using System.Collections.Generic;
using System.Linq;
using System.Net.Http;
using System.Text.Json;
using System.Threading.Tasks;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Dimmy.Windows.Helpers;
using Dimmy.Windows.Interop;
using Dimmy.Windows.Services;
using Dimmy.Windows.ViewModels;

namespace Dimmy.Windows.Views;

public sealed partial class SettingsWindow : Window
{
    public SettingsViewModel ViewModel { get; } = new();
    private string _currentTag = "home";
    private bool _loaded; // suppress SelectionChanged during init

    // File-load → meeting bridge. Captured at successful file-load
    // completion so the "Run recap as meeting" button can mint a
    // meeting dir off the same WAV + transcript. Cleared when the
    // user picks another file so we never recap a stale pair.
    private string? _lastFileLoadPath;
    private string? _lastFileLoadTranscript;

    public SettingsWindow()
    {
        this.InitializeComponent();

        if (Content is FrameworkElement root)
        {
            // Theme is applied later from UiPreferences via
            // SyncThemeRadioButtons; setting it here would override
            // the saved value before LoadConfig runs.
            root.DataContext = ViewModel;
        }

        Title = "Dimmy Settings";

        var appWindow = WindowHelper.GetAppWindow(this);
        WindowHelper.ResizeLogical(this, 920, 640);

        // Set window icon
        try
        {
            var iconPath = System.IO.Path.Combine(
                System.AppContext.BaseDirectory, "Assets", "dimmy.ico");
            if (System.IO.File.Exists(iconPath))
                appWindow?.SetIcon(iconPath);
        }
        catch { }

        // Refresh License page when the running Dimmy redeems via dimmy:// URL.
        // The URL-scheme dispatch hits App.xaml.cs which calls LicenseService.RedeemAsync
        // off-thread; firing LicenseChanged lets us update the visible status card
        // without polling. We marshal back to UI thread via DispatcherQueue.
        LicenseService.LicenseChanged += OnLicenseChangedExternal;
        this.Closed += (_, __) => LicenseService.LicenseChanged -= OnLicenseChangedExternal;

        // Auto-save on window close. The "Saved" InfoBar pulse already
        // promises Win11-native auto-save UX — make it real, so closing
        // the window via the X (or ESC) doesn't silently drop the user's
        // edits. Save_Click stays for the explicit "Save & close" path.
        this.Closed += (_, __) => AutoSaveOnClose();

        // Subscribe to Parakeet download progress events routed through
        // the App-level FFI callback. AppViewModel.HandleEvent already
        // marshals onto the UI thread before invoking the event.
        if (Application.Current is App app)
        {
            app.AppViewModel.ParakeetDownloadProgress += OnParakeetProgress;
            app.AppViewModel.FileTranscribeProgress += OnFileTranscribeProgress;
            app.AppViewModel.SttModelDownloadProgress += OnSttModelProgress;
            app.AppViewModel.LlmModelDownloadProgress += OnLlmModelProgress;
            this.Closed += (_, __) =>
            {
                app.AppViewModel.ParakeetDownloadProgress -= OnParakeetProgress;
                app.AppViewModel.FileTranscribeProgress -= OnFileTranscribeProgress;
                app.AppViewModel.SttModelDownloadProgress -= OnSttModelProgress;
                app.AppViewModel.LlmModelDownloadProgress -= OnLlmModelProgress;
            };
        }

        LoadConfig();
        ViewModel.LoadGpuStatus();
        // Build the STT + LLM pickers from the single-source catalog BEFORE
        // syncing the selection to the saved config.
        PopulateSttProviderPicker();
        PopulateLlmProviderPicker();
        SyncProviderComboBox();
        SyncLlmProviderComboBox();
        // Reveal/refresh the Claude Code status card if the user's
        // saved LLM provider is the synthetic claude-code:// URL.
        RefreshClaudeCodeStatus();
        SyncLanguageComboBox();
        SyncThemeRadioButtons();
        SyncAudioSourceRadio();
        PopulateLocalModels();
        SyncSttMode();
        PopulateLocalLlmModels();
        // Build the recap picker (Auto + cloud recap models + custom sentinel)
        // from the catalog, THEN inject the local "Local · …" options, THEN
        // snap the selection — so a persisted local: id already exists.
        PopulateRecapModelPicker();
        PopulateRecapLocalModels();
        SyncRecapModelPicker();
        SyncLlmMode();
        PopulateStats();
        PopulateVersion();

        // A model download started in a previous Settings session keeps
        // running on its background thread; restore the in-flight bar so the
        // user sees progress instead of an idle "Download" button.
        if (Application.Current is App reopenApp)
        {
            RestoreActiveDownloads(reopenApp);
        }

        // Default to Home tab. Without this the NavigationView starts with no
        // selection, so the user sees the Home panel content (Visibility=
        // Visible in XAML) but no sidebar highlight, which is jarring.
        if (NavView.MenuItems.Count > 0 && NavView.MenuItems[0] is NavigationViewItem first)
        {
            NavView.SelectedItem = first;
            // Explicit tint in case Nav_SelectionChanged didn't fire — XAML
            // template not always loaded at this point so we belt-and-brace.
            TintSelectedNavIcon(first);
        }

        // Pulse "Saved" InfoBar on any ViewModel field change (Win11 auto-save
        // pattern). The Save button still flushes to disk; this is purely
        // a visual hint that the form is dirty.
        ViewModel.PropertyChanged += (_, _) => PulseSavedInfoBar();

        // Render waveform + load audio whenever the History selection
        // changes. Hooked here (not in XAML) because rendering needs
        // the Canvas's actual ActualWidth which isn't known until
        // layout — ViewModel can't reach it from a binding.
        ViewModel.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == nameof(ViewModel.SelectedHistoryItem))
            {
                _ = RefreshHistoryAudioAsync();
                RefreshHistoryTextRendering();
            }
        };

        // App rules: pulse on collection change (add/remove/reorder) AND
        // on any per-row property edit (pattern, style, ...). Without
        // hooking the inner ObservableObjects' PropertyChanged the user
        // wouldn't see the "Saved" hint when editing a row in place.
        ViewModel.AppRules.CollectionChanged += (_, e) =>
        {
            PulseSavedInfoBar();
            UpdateAppRulesEmptyHint();
            // Persist on every collection change, including a drag-
            // reorder Move (which would otherwise stay UI-only and get
            // lost on next ReloadConfig). _loaded gate prevents the
            // initial bulk-add during config load from triggering a
            // save storm.
            if (_loaded) App.Instance?.ApplySettings(ViewModel);
        };
        foreach (var r in ViewModel.AppRules)
            r.PropertyChanged += (_, _) => PulseSavedInfoBar();
        ViewModel.AppRules.CollectionChanged += (_, e) =>
        {
            // Only hook PropertyChanged on NEWLY-added rules. A Move
            // event also sets NewItems but the moved instance is the
            // same one we hooked at Add time — re-hooking would leak
            // duplicate subscriptions on every drag-reorder.
            if (e.Action != System.Collections.Specialized.NotifyCollectionChangedAction.Add)
                return;
            if (e.NewItems != null)
                foreach (AppRuleViewModel r in e.NewItems)
                    r.PropertyChanged += (_, _) => PulseSavedInfoBar();
        };
        UpdateAppRulesEmptyHint();

        // WinUI 3 desktop's DragOver/Drop events never fire even with
        // handledEventsToo=true (verified empirically — DragEnter logger
        // never showed up in ptt.log on a real drop). Bypass with Win32
        // OLE RegisterDragDrop bound to the Window's HWND. The drop
        // target accepts file drops anywhere on the Settings window;
        // we filter to the supported audio formats at TranscribeFileAsync time.
        Activated += SettingsWindow_Activated;
        Closed += (_, _) =>
        {
            try { _win32Drop?.Unregister(); } catch { }
            _win32Drop = null;
        };

        // Parallel attempt #2: XAML-side handlers on the absolute
        // root of the Window's Content (above any ScrollViewer that
        // could intercept). handledEventsToo=true so even if some
        // intermediate element marks the event handled, ours runs.
        if (RootGrid != null)
        {
            RootGrid.AddHandler(UIElement.DragOverEvent,
                new DragEventHandler(RootGrid_DragOver), handledEventsToo: true);
            RootGrid.AddHandler(UIElement.DropEvent,
                new DragEventHandler(RootGrid_Drop), handledEventsToo: true);
            RootGrid.AddHandler(UIElement.DragEnterEvent,
                new DragEventHandler((_, _) => App.Log("XAML root DragEnter", "FileLoad")),
                handledEventsToo: true);
        }

        // Warm-up: extract real icons from currently-running processes
        // so the App Rules list shows them immediately instead of the
        // FontIcon fallback. Runs on a background thread; refreshes
        // each row's IconAssetUri on the UI thread when done.
        _ = WarmUpAppIconsAsync();

        // Initial Notion summary card state — reflect HasNotionToken +
        // NotionTargetTitle from LoadConfig. Without this the card
        // shows the XAML default ("Not connected" / Connect button)
        // even when a token was already saved.
        NotionRefreshSummary();
        RefreshMcpCard();

        // Custom-dict ListView source-of-truth lives in the Rust
        // AppState; pull it now so the UI matches reality, AND
        // restore the user's hotkey preference from UiPreferences so
        // the ShortcutRecorder shows the right combo.
        ReloadUserDict();
        try
        {
            var prefs = Services.UiPreferences.Load();
            ViewModel.DictHotkey = string.IsNullOrEmpty(prefs.DictHotkey)
                ? "ctrl+shift+d" : prefs.DictHotkey;
            // The command hotkey now runs on the Rust hook (like dictation),
            // so modifier-only combos (Win+Alt) are valid — load it verbatim.
            ViewModel.CommandHotkey = prefs.CommandHotkey ?? "";
        }
        catch { }

        // Persist DictHotkey / CommandHotkey changes + re-register the global
        // hotkey on the App's services so the new combo binds without a
        // restart.
        ViewModel.PropertyChanged += (_, e) =>
        {
            if (!_loaded) return;
            try
            {
                if (e.PropertyName == nameof(ViewModel.DictHotkey))
                {
                    var prefs = Services.UiPreferences.Load();
                    prefs.DictHotkey = ViewModel.DictHotkey;
                    prefs.Save();
                    App.Instance?.ReregisterDictHotkey(ViewModel.DictHotkey);
                    App.Log($"dict hotkey updated to '{ViewModel.DictHotkey}'", "Dict");
                }
                else if (e.PropertyName == nameof(ViewModel.CommandHotkey))
                {
                    var cmd = ViewModel.CommandHotkey ?? "";
                    // Reject a command combo that overlaps the dictation or
                    // dictionary shortcut — pressing it would trigger both on
                    // the shared hook. Clearing to "" re-enters this handler
                    // with an empty combo (never conflicts) → persists disabled.
                    if (!string.IsNullOrWhiteSpace(cmd)
                        && (Interop.DimmyNative.dimmy_hotkey_combos_conflict(cmd, ViewModel.Shortcut ?? "") != 0
                            || Interop.DimmyNative.dimmy_hotkey_combos_conflict(cmd, ViewModel.DictHotkey ?? "") != 0))
                    {
                        App.Log($"command hotkey '{cmd}' overlaps dictation/dict — rejecting", "Hotkey");
                        Services.DictNotificationService.ShowHotkeyConflict(cmd);
                        ViewModel.CommandHotkey = "";
                        return;
                    }
                    var prefs = Services.UiPreferences.Load();
                    prefs.CommandHotkey = cmd;
                    prefs.Save();
                    App.Instance?.ReregisterCommandHotkey(cmd);
                    App.Log($"command hotkey updated to '{cmd}'", "Hotkey");
                }
            }
            catch (Exception ex)
            {
                App.Log($"Hotkey save exc: {ex.Message}", "Hotkey");
            }
        };

        RefreshMeetingFolderDisplay();

        _loaded = true;
        // Now that selections are established + _loaded is true, run one auth
        // refresh so the provider/model pickers get filtered to the connected
        // providers (RebuildCombo no-ops while loading).
        RefreshAuthIntegrationStatus();
    }

    /// <summary>Disable the optional command-mode hotkey. Empties the combo
    /// → the PropertyChanged handler persists "" and unregisters it.</summary>
    private void CommandHotkeyClear_Click(object sender, RoutedEventArgs e)
    {
        ViewModel.CommandHotkey = "";
    }

    /// File extensions accepted by `dimmy_transcribe_file`. WAV is the
    /// fast path (hound decoder); everything else goes through
    /// Symphonia on the Rust side. Keep in sync with the file dialog
    /// filter on `FileLoadPick` (~line 670).
    private static readonly string[] SupportedFileLoadExtensions =
        new[] { ".wav", ".m4a", ".mp4", ".mp3", ".aac", ".flac", ".ogg" };

    private const string FileLoadCaption =
        "Drop an audio file to transcribe";
    private const string FileLoadUnsupportedMessage =
        "Unsupported file — accepted: .wav .m4a .mp3 .aac .flac .ogg";

    private static bool IsSupportedAudioExtension(string ext)
    {
        foreach (var s in SupportedFileLoadExtensions)
        {
            if (ext.Equals(s, StringComparison.OrdinalIgnoreCase)) return true;
        }
        return false;
    }

    private void RootGrid_DragOver(object sender, Microsoft.UI.Xaml.DragEventArgs e)
    {
        App.Log("XAML root DragOver", "FileLoad");
        if (e.DataView.Contains(global::Windows.ApplicationModel.DataTransfer.StandardDataFormats.StorageItems))
        {
            e.AcceptedOperation = global::Windows.ApplicationModel.DataTransfer.DataPackageOperation.Copy;
            if (e.DragUIOverride != null)
            {
                e.DragUIOverride.Caption = FileLoadCaption;
                e.DragUIOverride.IsCaptionVisible = true;
            }
            e.Handled = true;
        }
    }

    private async void RootGrid_Drop(object sender, Microsoft.UI.Xaml.DragEventArgs e)
    {
        App.Log("XAML root Drop fired", "FileLoad");
        if (!e.DataView.Contains(global::Windows.ApplicationModel.DataTransfer.StandardDataFormats.StorageItems))
            return;
        e.Handled = true;
        try
        {
            var items = await e.DataView.GetStorageItemsAsync();
            var first = items.FirstOrDefault(i => i is global::Windows.Storage.StorageFile sf
                && IsSupportedAudioExtension(sf.FileType))
                as global::Windows.Storage.StorageFile;
            if (first == null)
            {
                FileLoadStatus.Text = FileLoadUnsupportedMessage;
                return;
            }
            await TranscribeFileAsync(first.Path);
        }
        catch (Exception ex)
        {
            FileLoadStatus.Text = $"Drop error: {ex.Message}";
            App.Log($"RootGrid Drop exc: {ex}", "FileLoad");
        }
    }

    private async System.Threading.Tasks.Task WarmUpAppIconsAsync()
    {
        try
        {
            await System.Threading.Tasks.Task.Run(() =>
            {
                foreach (var proc in System.Diagnostics.Process.GetProcesses())
                {
                    try
                    {
                        var path = proc.MainModule?.FileName;
                        if (!string.IsNullOrEmpty(path))
                            Helpers.IconExtractor.EnsureCachedFromExePath(path);
                    }
                    catch
                    {
                        // MainModule throws on UAC-elevated processes
                        // when we're non-elevated; skip and move on.
                    }
                    finally
                    {
                        proc.Dispose();
                    }
                }
            });
            // Hop back to UI thread to re-evaluate bindings.
            DispatcherQueue.TryEnqueue(() =>
            {
                foreach (var r in ViewModel.AppRules)
                    r.RefreshIconAssetUri();
            });
        }
        catch (Exception ex)
        {
            App.Log($"WarmUpAppIcons exc: {ex.Message}", "AppCtx");
        }
    }

    private void OpenMeeting_Click(object sender, RoutedEventArgs e)
    {
        App.Log("OpenMeeting_Click fired", "Meeting");
        App.Instance?.OpenMeetingWindow();
    }

    // ── App rules ─────────────────────────────────────────────────

    private void UpdateAppRulesEmptyHint()
    {
        if (AppRulesEmptyHint != null)
            AppRulesEmptyHint.Visibility = ViewModel.AppRules.Count == 0
                ? Visibility.Visible : Visibility.Collapsed;
    }

    private void AppRuleAdd_Click(object sender, RoutedEventArgs e)
    {
        ViewModel.AppRules.Add(new AppRuleViewModel(
            matchPattern: "",
            matchType: "process_name",
            llmStyle: "off",
            llmTranslateTo: "",
            label: "",
            enabled: true));
    }

    private void AppRuleLoadDefaults_Click(object sender, RoutedEventArgs e)
    {
        ViewModel.LoadAppRulesDefaults();
    }

    private void AppRuleDelete_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement fe && fe.Tag is AppRuleViewModel rule)
            ViewModel.AppRules.Remove(rule);
    }

    // ── App-rule reorder (raw pointer events) ──────────────────────
    // WinUI 3 v3.1.7's drag pipeline crashes (Microsoft.UI.Xaml.dll
    // +0x39ce55 → combase.dll +0x37fc4 → E_UNEXPECTED) regardless of
    // entry point — CanReorderItems, CanDrag/DragStarting on a child,
    // ListView-level Drop handler all reproduce it. The unifying
    // factor is the IDataObject / IDropTarget code path inside
    // Microsoft.UI.Xaml.dll. Workaround: drive reorder from raw
    // PointerPressed / Released / CaptureLost only — that pipeline
    // never touches drag-drop. See microsoft-ui-xaml issues #5607,
    // #7690 — no fix in WindowsAppSDK through 2.0.1.

    private AppRuleViewModel? _appRuleDragSource;
    private ListViewItem? _appRuleDragSourceContainer;
    // Cached source-row bitmap (rendered once at PointerPressed) for
    // the floating ghost preview. RenderTargetBitmap.RenderAsync is
    // ~5–30 ms on a typical row — fast enough to be perceived as
    // synchronous when fired off as fire-and-forget from Pressed.
    private Microsoft.UI.Xaml.Media.Imaging.RenderTargetBitmap? _appRuleDragGhostBmp;
    // Auto-scroll while dragging near the top / bottom edge of the
    // ListView. Built-in CanReorderItems would do this for free; manual
    // reorder needs us to drive the ScrollViewer ourselves.
    private Microsoft.UI.Xaml.DispatcherTimer? _appRuleDragScrollTimer;
    private double _appRuleDragScrollDelta;
    private ScrollViewer? _appRulesScrollViewer;
    // Last pointer position in ListView coordinates — needed by the
    // scroll timer tick so the drop indicator can refresh after each
    // auto-scroll step (PointerMoved alone wouldn't fire when the
    // cursor is stationary at the edge).
    private global::Windows.Foundation.Point _appRuleLastPointerInLV;

    private void AppRuleHandle_PointerPressed(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (sender is not FrameworkElement fe) return;
        if (fe.DataContext is not AppRuleViewModel rule) return;
        // Left click only. Touch / pen / right click ignored — keeps
        // context menus and other gestures unaffected.
        var pt = e.GetCurrentPoint(fe);
        if (pt.PointerDeviceType == Microsoft.UI.Input.PointerDeviceType.Mouse
            && !pt.Properties.IsLeftButtonPressed)
            return;
        _appRuleDragSource = rule;
        _appRuleDragSourceContainer = AppRulesListView.ContainerFromItem(rule) as ListViewItem;
        if (_appRuleDragSourceContainer != null)
        {
            // Dim the source row so the user sees which row they're
            // moving. Restored in FinishAppRuleDrag.
            _appRuleDragSourceContainer.Opacity = 0.45;
        }
        if (fe.CapturePointer(e.Pointer))
        {
            App.Log($"app-rule drag start: rule='{rule.MatchPattern}' idx={ViewModel.AppRules.IndexOf(rule)}",
                "Settings");
            // Fire-and-forget: render the source row to a bitmap, then
            // open the ghost Popup at the pointer. The ghost mirrors
            // the row visually so the user sees what's being moved.
            _ = ShowAppRuleDragGhostAsync(e);
        }
        else
        {
            // Capture refused — bail rather than enter a half-tracked state.
            _appRuleDragSource = null;
            if (_appRuleDragSourceContainer != null)
                _appRuleDragSourceContainer.Opacity = 1.0;
            _appRuleDragSourceContainer = null;
        }
        e.Handled = true;
    }

    private async System.Threading.Tasks.Task ShowAppRuleDragGhostAsync(
        Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        var container = _appRuleDragSourceContainer;
        if (container == null) return;
        try
        {
            var rtb = new Microsoft.UI.Xaml.Media.Imaging.RenderTargetBitmap();
            await rtb.RenderAsync(container);
            // Released before the snapshot finished — bail without
            // opening the popup; FinishAppRuleDrag has already cleaned up.
            if (_appRuleDragSource == null) return;
            _appRuleDragGhostBmp = rtb;
            AppRuleDragGhostImage.Source = rtb;
            AppRuleDragGhostImage.Width = container.ActualWidth;
            AppRuleDragGhostImage.Height = container.ActualHeight;
            UpdateAppRuleDragGhostPosition(e);
            AppRuleDragGhostPopup.IsOpen = true;
        }
        catch (System.Exception ex)
        {
            App.Log($"ghost render exc: {ex.Message}", "Settings");
        }
    }

    private void UpdateAppRuleDragGhostPosition(
        Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (this.Content is not UIElement root) return;
        var pt = e.GetCurrentPoint(root).Position;
        // Offset slightly so the ghost doesn't sit dead-center under the
        // cursor (cursor stays visible at top-left of the ghost block).
        AppRuleDragGhostPopup.HorizontalOffset = pt.X + 12;
        AppRuleDragGhostPopup.VerticalOffset = pt.Y + 8;
    }

    private void AppRuleHandle_PointerMoved(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        if (_appRuleDragSource == null) return;
        // Move the ghost popup with the cursor (works even outside the
        // ListView bounds — Popup.ShouldConstrainToRootBounds=False).
        if (AppRuleDragGhostPopup.IsOpen)
            UpdateAppRuleDragGhostPosition(e);
        var lv = AppRulesListView;
        var pt = e.GetCurrentPoint(lv).Position;
        _appRuleLastPointerInLV = pt;
        UpdateAppRuleDropIndicator(pt);
        UpdateAppRuleDragAutoScroll(pt);
    }

    private System.Collections.Generic.List<Helpers.AppRuleReorderMath.Slot> GatherAppRuleSlots()
    {
        var lv = AppRulesListView;
        var slots = new System.Collections.Generic.List<Helpers.AppRuleReorderMath.Slot>(ViewModel.AppRules.Count);
        for (int i = 0; i < ViewModel.AppRules.Count; i++)
        {
            if (lv.ContainerFromIndex(i) is not ListViewItem container) continue;
            var transform = container.TransformToVisual(lv);
            var topLeft = transform.TransformPoint(new global::Windows.Foundation.Point(0, 0));
            slots.Add(new Helpers.AppRuleReorderMath.Slot(
                topLeft.Y, topLeft.Y + container.ActualHeight));
        }
        return slots;
    }

    private void UpdateAppRuleDropIndicator(global::Windows.Foundation.Point pt)
    {
        var lv = AppRulesListView;
        var slots = GatherAppRuleSlots();
        var (_, indicatorY) = Helpers.AppRuleReorderMath.HitTest(pt.Y, slots, lv.ActualHeight);
        // The indicator's parent Grid pads against the ListView's outer
        // edges; clamp Y to keep the 2-px line inside.
        indicatorY = System.Math.Max(0, System.Math.Min(indicatorY, lv.ActualHeight - 2));
        AppRuleDropIndicator.Margin = new Thickness(0, indicatorY, 0, 0);
        AppRuleDropIndicator.Visibility = Visibility.Visible;
    }

    private void UpdateAppRuleDragAutoScroll(global::Windows.Foundation.Point _pointerInLv)
    {
        // Edge-zone test must happen against the VIEWPORT, not the
        // ListView. AppRulesListView is nested inside the page-level
        // ScrollViewer (xaml.cs:1223 inside xaml:103), so the ListView's
        // own ScrollViewer is disabled and its content grows freely —
        // pt.Y in ListView coords reaches lv.ActualHeight only when the
        // cursor is near the LAST item, not when it's at the bottom of
        // the visible area. We compute pointer Y relative to the outer
        // ScrollViewer + use its ViewportHeight as the edge baseline.
        const double EDGE_ZONE = 40.0;
        const double SCROLL_PER_TICK = 14.0;
        double delta = 0;
        var sv = GetAppRulesScrollViewer();
        if (sv != null)
        {
            try
            {
                var transform = AppRulesListView.TransformToVisual(sv);
                var pInSv = transform.TransformPoint(_pointerInLv);
                double viewportH = sv.ViewportHeight;
                if (viewportH > 0)
                {
                    if (pInSv.Y < EDGE_ZONE) delta = -SCROLL_PER_TICK;
                    else if (pInSv.Y > viewportH - EDGE_ZONE) delta = SCROLL_PER_TICK;
                }
            }
            catch { /* TransformToVisual can race during layout; skip this tick */ }
        }
        _appRuleDragScrollDelta = delta;
        if (delta != 0 && _appRuleDragScrollTimer == null)
        {
            _appRuleDragScrollTimer = new Microsoft.UI.Xaml.DispatcherTimer
            {
                Interval = System.TimeSpan.FromMilliseconds(40),
            };
            _appRuleDragScrollTimer.Tick += AppRuleDragScrollTimer_Tick;
            _appRuleDragScrollTimer.Start();
        }
        else if (delta == 0 && _appRuleDragScrollTimer != null)
        {
            StopAppRuleDragScrollTimer();
        }
    }

    private void AppRuleDragScrollTimer_Tick(object? sender, object e)
    {
        if (_appRuleDragSource == null || _appRuleDragScrollDelta == 0)
        {
            StopAppRuleDragScrollTimer();
            return;
        }
        var sv = GetAppRulesScrollViewer();
        if (sv == null) return;
        double newOffset = sv.VerticalOffset + _appRuleDragScrollDelta;
        sv.ChangeView(null, newOffset, null, disableAnimation: true);
        // The cursor stayed in the same screen position but the ListView
        // translated within its parent ScrollViewer by SCROLL_PER_TICK —
        // so in LV-relative coords the cursor effectively moved by the
        // same amount. Update the cached pointer so the drop indicator
        // refresh below picks the correct slot for the now-shifted layout.
        _appRuleLastPointerInLV = new global::Windows.Foundation.Point(
            _appRuleLastPointerInLV.X,
            _appRuleLastPointerInLV.Y + _appRuleDragScrollDelta);
        UpdateAppRuleDropIndicator(_appRuleLastPointerInLV);
    }

    private void StopAppRuleDragScrollTimer()
    {
        if (_appRuleDragScrollTimer != null)
        {
            _appRuleDragScrollTimer.Stop();
            _appRuleDragScrollTimer.Tick -= AppRuleDragScrollTimer_Tick;
            _appRuleDragScrollTimer = null;
        }
        _appRuleDragScrollDelta = 0;
    }

    private ScrollViewer? GetAppRulesScrollViewer()
    {
        if (_appRulesScrollViewer != null) return _appRulesScrollViewer;
        // Walk UP the visual tree, not down. AppRulesListView lives
        // inside the page-level ScrollViewer (xaml line 103) — its own
        // inner ScrollViewer (templated child) is disabled when the
        // host has no fixed height, so descending finds a no-op
        // scroller. The first ScrollViewer ancestor is the one that
        // actually moves content under the cursor.
        _appRulesScrollViewer = FindFirstAncestor<ScrollViewer>(AppRulesListView);
        return _appRulesScrollViewer;
    }

    private static T? FindFirstAncestor<T>(DependencyObject d) where T : DependencyObject
    {
        var p = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetParent(d);
        while (p != null)
        {
            if (p is T t) return t;
            p = Microsoft.UI.Xaml.Media.VisualTreeHelper.GetParent(p);
        }
        return null;
    }

    private void AppRuleHandle_PointerReleased(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        FinishAppRuleDrag(sender as FrameworkElement, e);
    }

    private void AppRuleHandle_PointerCaptureLost(object sender, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        FinishAppRuleDrag(sender as FrameworkElement, e);
    }

    private void FinishAppRuleDrag(FrameworkElement? handle, Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        var rule = _appRuleDragSource;
        var srcContainer = _appRuleDragSourceContainer;
        _appRuleDragSource = null;
        _appRuleDragSourceContainer = null;

        // Always hide the indicator + ghost + restore opacity, even
        // if we bail early — the visual feedback should not stick
        // around after the pointer is released.
        StopAppRuleDragScrollTimer();
        AppRuleDropIndicator.Visibility = Visibility.Collapsed;
        if (AppRuleDragGhostPopup != null && AppRuleDragGhostPopup.IsOpen)
            AppRuleDragGhostPopup.IsOpen = false;
        if (AppRuleDragGhostImage != null)
            AppRuleDragGhostImage.Source = null;
        _appRuleDragGhostBmp = null;
        if (srcContainer != null) srcContainer.Opacity = 1.0;

        if (rule == null) return;

        try
        {
            handle?.ReleasePointerCapture(e.Pointer);
        }
        catch { /* ignore — capture may already be gone */ }

        int srcIdx = ViewModel.AppRules.IndexOf(rule);
        if (srcIdx < 0) return;

        var lv = AppRulesListView;
        var pt = e.GetCurrentPoint(lv).Position;
        var slots = GatherAppRuleSlots();
        var (rawTarget, _) = Helpers.AppRuleReorderMath.HitTest(pt.Y, slots, lv.ActualHeight);
        int dstIdx = Helpers.AppRuleReorderMath.AdjustedDstIndex(srcIdx, rawTarget);
        if (dstIdx < 0)
        {
            App.Log($"app-rule drag end: no-op (src={srcIdx} rawTarget={rawTarget})", "Settings");
            return;
        }
        App.Log($"app-rule reorder src={srcIdx} dst={dstIdx} count={ViewModel.AppRules.Count}", "Settings");
        ViewModel.AppRules.Move(srcIdx, dstIdx);
    }

    // ── File-load (offline transcribe) ────────────────────────────

    private async void FileLoadPick_Click(object sender, RoutedEventArgs e)
    {
        App.Log("FileLoadPick_Click fired", "FileLoad");
        // WinRT FileOpenPicker.PickSingleFileAsync silently returns
        // null on some unpackaged WinUI 3 desktop builds. Win32
        // IFileOpenDialog (the same dialog Explorer uses) is
        // bulletproof — no async, no manifest capabilities, no STA
        // gymnastics. Pump it on a background thread so Show() can
        // run its modal loop without freezing the UI thread.
        try
        {
            var hwnd = WindowHelper.GetHwnd(this);
            App.Log($"FileLoadPick: opening Win32 picker (hwnd=0x{hwnd:X})", "FileLoad");
            string? path = await System.Threading.Tasks.Task.Run(() =>
                Helpers.Win32FileDialog.PickFile(hwnd, "Pick an audio file to transcribe",
                    ("Audio files", "*.wav;*.m4a;*.mp4;*.mp3;*.aac;*.flac;*.ogg"),
                    ("WAV audio", "*.wav"),
                    ("All files", "*.*")));
            App.Log($"FileLoadPick: picker returned {(path == null ? "null" : path)}", "FileLoad");
            if (string.IsNullOrEmpty(path))
            {
                FileLoadStatus.Text = "Cancelled";
                return;
            }
            await TranscribeFileAsync(path);
        }
        catch (Exception ex)
        {
            FileLoadStatus.Text = $"Picker error: {ex.Message}";
            App.Log($"FileLoadPick exc: {ex}", "FileLoad");
        }
    }

    /// Active Win32 drop target. Created lazily on first Activated
    /// because RegisterDragDrop wants the HWND already alive.
    private Helpers.Win32DropTarget? _win32Drop;

    private void SettingsWindow_Activated(object sender,
        Microsoft.UI.Xaml.WindowActivatedEventArgs e)
    {
        // Idempotent: only register once across the window's lifetime.
        if (_win32Drop != null) return;
        try
        {
            var hwnd = WindowHelper.GetHwnd(this);
            _win32Drop = new Helpers.Win32DropTarget(hwnd, OnFilesDroppedFromWin32);
            _win32Drop.Register();
        }
        catch (Exception ex)
        {
            App.Log($"Win32 drop register exc: {ex.Message}", "FileLoad");
        }
    }

    private async void OnFilesDroppedFromWin32(string[] paths)
    {
        // Marshal back to UI thread — RegisterDragDrop callbacks run
        // on the OLE STA but XAML wants the dispatcher.
        var first = Array.Find(paths, p =>
        {
            var ext = System.IO.Path.GetExtension(p);
            return !string.IsNullOrEmpty(ext) && IsSupportedAudioExtension(ext);
        });
        if (first == null)
        {
            DispatcherQueue.TryEnqueue(() =>
                FileLoadStatus.Text = FileLoadUnsupportedMessage);
            return;
        }
        // Hop to UI dispatcher so TranscribeFileAsync can touch
        // FileLoadProgress / FileLoadResult safely.
        var tcs = new System.Threading.Tasks.TaskCompletionSource<bool>();
        DispatcherQueue.TryEnqueue(async () =>
        {
            try { await TranscribeFileAsync(first); }
            finally { tcs.TrySetResult(true); }
        });
        await tcs.Task;
    }

    private void FileLoadDropTarget_DragOver(object sender, Microsoft.UI.Xaml.DragEventArgs e)
    {
        // Accept any drag that carries a file-system item — we filter
        // to the supported audio extensions at Drop time. Setting both
        // AcceptedOperation AND marking Handled=true is required on
        // WinUI 3 desktop; omitting Handled lets the parent
        // ScrollViewer steal the event back and the cursor shows a
        // "no-entry" sign.
        if (e.DataView.Contains(global::Windows.ApplicationModel.DataTransfer.StandardDataFormats.StorageItems))
        {
            e.AcceptedOperation = global::Windows.ApplicationModel.DataTransfer.DataPackageOperation.Copy;
            if (e.DragUIOverride != null)
            {
                e.DragUIOverride.Caption = FileLoadCaption;
                e.DragUIOverride.IsCaptionVisible = true;
                e.DragUIOverride.IsContentVisible = true;
                e.DragUIOverride.IsGlyphVisible = true;
            }
            e.Handled = true;
        }
    }

    private async void FileLoadDropTarget_Drop(object sender, Microsoft.UI.Xaml.DragEventArgs e)
    {
        App.Log("FileLoadDropTarget_Drop fired", "FileLoad");
        if (!e.DataView.Contains(global::Windows.ApplicationModel.DataTransfer.StandardDataFormats.StorageItems))
        {
            App.Log("Drop: no StorageItems in DataView", "FileLoad");
            return;
        }
        e.Handled = true;
        try
        {
            var items = await e.DataView.GetStorageItemsAsync();
            App.Log($"Drop: got {items.Count} items", "FileLoad");
            var first = items.FirstOrDefault(i => i is global::Windows.Storage.StorageFile sf
                && IsSupportedAudioExtension(sf.FileType))
                as global::Windows.Storage.StorageFile;
            if (first == null)
            {
                FileLoadStatus.Text = FileLoadUnsupportedMessage;
                return;
            }
            await TranscribeFileAsync(first.Path);
        }
        catch (Exception ex)
        {
            FileLoadStatus.Text = $"Drop error: {ex.Message}";
            App.Log($"Drop exc: {ex}", "FileLoad");
        }
    }

    /// Returns approximate file metrics (duration in seconds, size in
    /// bytes) by reading just the WAV RIFF/fmt header — no full decode.
    /// Used by the "this file is large, continue?" confirmation gate.
    private static (double durationSecs, long sizeBytes) PeekWavMetrics(string path)
    {
        try
        {
            var fi = new System.IO.FileInfo(path);
            using var fs = fi.OpenRead();
            using var br = new System.IO.BinaryReader(fs);
            if (new string(br.ReadChars(4)) != "RIFF") return (0, fi.Length);
            br.ReadUInt32(); // file size
            if (new string(br.ReadChars(4)) != "WAVE") return (0, fi.Length);
            uint sampleRate = 0; ushort channels = 1; ushort bits = 0;
            while (fs.Position + 8 <= fs.Length)
            {
                var id = new string(br.ReadChars(4));
                var sz = br.ReadUInt32();
                if (id == "fmt ")
                {
                    br.ReadUInt16(); // fmt tag
                    channels = br.ReadUInt16();
                    sampleRate = br.ReadUInt32();
                    br.ReadUInt32(); // byte rate
                    br.ReadUInt16(); // block align
                    bits = br.ReadUInt16();
                    if (sz > 16) fs.Seek(sz - 16, System.IO.SeekOrigin.Current);
                }
                else if (id == "data")
                {
                    int frameSize = (bits / 8) * channels;
                    if (frameSize > 0 && sampleRate > 0)
                    {
                        long frames = sz / (uint)frameSize;
                        return (frames / (double)sampleRate, fi.Length);
                    }
                    break;
                }
                else fs.Seek(sz, System.IO.SeekOrigin.Current);
            }
            return (0, fi.Length);
        }
        catch { return (0, 0); }
    }

    /// Decides whether the user should be warned before running an
    /// expensive transcription. Threshold: 5 minutes of audio OR 50 MB
    /// on disk. For cloud mode we warn at any duration ≥ 5 min so the
    /// user is aware of API cost before submitting.
    private async System.Threading.Tasks.Task<bool> ConfirmLargeFileAsync(string path)
    {
        var (durationSecs, sizeBytes) = PeekWavMetrics(path);
        bool isCloud = !string.Equals(ViewModel.SttMode, "local", StringComparison.OrdinalIgnoreCase);
        bool large = durationSecs >= 300 || sizeBytes >= 50L * 1024 * 1024;
        if (!large && !isCloud) return true; // small + local: no warning
        if (!large && isCloud && durationSecs < 60) return true; // small cloud: no warning either

        var mins = (int)Math.Round(durationSecs / 60.0);
        var sizeMb = sizeBytes / (1024.0 * 1024.0);
        var costHint = isCloud
            ? "\nThis will be sent to your configured cloud provider. Billing applies."
            : "\nThis runs locally and may take a few minutes.";
        var dlg = new ContentDialog
        {
            Title = "Long file",
            Content = $"{System.IO.Path.GetFileName(path)}\n" +
                      $"≈ {mins} min · {sizeMb:F1} MB{costHint}\n\nProceed?",
            PrimaryButtonText = "Transcribe",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = (this.Content as FrameworkElement)?.XamlRoot,
        };
        var result = await dlg.ShowAsync();
        return result == ContentDialogResult.Primary;
    }

    private async System.Threading.Tasks.Task TranscribeFileAsync(string path)
    {
        if (!await ConfirmLargeFileAsync(path))
        {
            FileLoadStatus.Text = "Cancelled";
            return;
        }
        FileLoadProgress.IsActive = true;
        FileLoadProgress.Visibility = Visibility.Visible;
        FileLoadResult.Visibility = Visibility.Collapsed;
        // Determinate bar starts at 0 — the first
        // file_transcribe_progress event from Rust will land within ~30 s
        // (one chunk window) and tick the bar forward.
        FileLoadBar.Value = 0;
        FileLoadBar.Visibility = Visibility.Visible;
        FileLoadStatus.Text = $"Transcribing {System.IO.Path.GetFileName(path)}...";
        FileLoadPickBtn.IsEnabled = false;
        // Telemetry: file load started. `source` is "picker" — drag-
        // drop fires a different code path that hits this same
        // function only AFTER the dropped file has been validated, so
        // we conservatively tag everything reaching here as picker.
        // Drop path adds its own event below at the AddHandler entry
        // (separate emit follow-up).
        var audioSecs = (long)Helpers.WavPeaks.ReadDurationSecs(path);
        DimmyNative.TrackEvent("file_load.started", new
        {
            source = "picker",
            audio_secs_bucket = Services.TelemetryBuckets.AudioSecs(audioSecs),
        });
        var fileLoadStopwatch = System.Diagnostics.Stopwatch.StartNew();
        try
        {
            const int BufLen = 1 << 22;
            var buf = new byte[BufLen];
            int rc = await System.Threading.Tasks.Task.Run(() =>
                DimmyNative.dimmy_transcribe_file(path, buf, BufLen));
            if (rc < 0)
            {
                FileLoadStatus.Text = rc switch
                {
                    -1 => "Bad arguments",
                    -2 => "Could not open / decode the WAV",
                    -3 => "VAD removed all audio (file silent?)",
                    -4 => "Cloud mode rejected (should not happen)",
                    -5 => "Backend produced empty transcript",
                    -6 => "Cloud config incomplete. Set provider URL, key, and model in Settings.",
                    -7 => "Tokio runtime failed to start",
                    -8 => "Cloud transcribe failed (see dimmy.log)",
                    _ => $"Failed (code {rc})",
                };
                FileLoadResult.Visibility = Visibility.Collapsed;
                DimmyNative.TrackEvent("file_load.completed", new
                {
                    audio_secs_bucket = Services.TelemetryBuckets.AudioSecs(audioSecs),
                    processing_ms_bucket = Services.TelemetryBuckets.ProcessingMs(fileLoadStopwatch.ElapsedMilliseconds),
                    words_bucket = "0",
                    success = false,
                });
            }
            else
            {
                var text = System.Text.Encoding.UTF8.GetString(buf, 0, rc);
                FileLoadResult.Text = text;
                FileLoadResult.Visibility = Visibility.Visible;
                FileLoadBar.Value = 100;
                var wordCount = text.Split(' ', StringSplitOptions.RemoveEmptyEntries).Length;
                FileLoadStatus.Text = $"{wordCount} words. Saved to History.";

                // Stash for the "Run recap as meeting" button below.
                // Cleared on the next file pick so we don't recap a
                // stale pair if the user starts a second transcribe.
                _lastFileLoadPath = path;
                _lastFileLoadTranscript = text;
                FileLoadRecapPanel.Visibility = Visibility.Visible;
                FileLoadRecapStatus.Text = "";

                DimmyNative.TrackEvent("file_load.completed", new
                {
                    audio_secs_bucket = Services.TelemetryBuckets.AudioSecs(audioSecs),
                    processing_ms_bucket = Services.TelemetryBuckets.ProcessingMs(fileLoadStopwatch.ElapsedMilliseconds),
                    words_bucket = Services.TelemetryBuckets.WordCount(wordCount),
                    success = true,
                });
            }
        }
        catch (Exception ex)
        {
            FileLoadStatus.Text = $"Error: {ex.Message}";
            App.Log($"file-load failed: {ex}", "FileLoad");
        }
        finally
        {
            FileLoadProgress.IsActive = false;
            FileLoadProgress.Visibility = Visibility.Collapsed;
            FileLoadBar.Visibility = Visibility.Collapsed;
            FileLoadPickBtn.IsEnabled = true;
        }
    }

    /// Progress event from Rust during chunked file transcribe.
    /// Updates the determinate ProgressBar + status text. Fires on
    /// the UI thread (AppViewModel.HandleEvent already marshals).
    private void OnFileTranscribeProgress(double processedSecs, double totalSecs, double percent)
    {
        if (FileLoadBar.Visibility != Visibility.Visible) return;
        FileLoadBar.Value = percent;
        FileLoadStatus.Text =
            $"Transcribing… {processedSecs:F0} / {totalSecs:F0} s ({percent:F0}%)";
    }

    /// "Run recap as meeting" — promotes the last successful file-load
    /// transcript into a synthetic meeting dir, runs the same LLM
    /// recap pipeline that live meetings use, and surfaces the
    /// resulting Meeting in History so the user can open it from
    /// Meeting → History the way they would any recorded meeting.
    /// The button is only visible when a transcript is sitting in
    /// the result box (fields stashed in the success branch above);
    /// re-disabled mid-call so the user can't double-fire.
    private async void FileLoadRunRecap_Click(object sender, RoutedEventArgs e)
    {
        if (string.IsNullOrEmpty(_lastFileLoadTranscript))
        {
            FileLoadRecapStatus.Text = "No transcript loaded.";
            return;
        }
        FileLoadRunRecapBtn.IsEnabled = false;
        FileLoadRecapProgress.IsActive = true;
        FileLoadRecapProgress.Visibility = Visibility.Visible;
        FileLoadRecapStatus.Text = "Building meeting + running recap…";
        try
        {
            var result = await Services.FileLoadToMeetingService.RunAsync(
                _lastFileLoadPath ?? "",
                _lastFileLoadTranscript);
            if (result.Success)
            {
                FileLoadRecapStatus.Text = "✓ Recap saved. Open Meeting → History.";
                App.Log($"file-load recap success at {result.Dir}", "FileLoad");
            }
            else
            {
                FileLoadRecapStatus.Text = $"Failed: {result.Error}";
                App.Log($"file-load recap failed: {result.Error}", "FileLoad");
            }
        }
        catch (Exception ex)
        {
            FileLoadRecapStatus.Text = $"Error: {ex.Message}";
            App.Log($"file-load recap exc: {ex}", "FileLoad");
        }
        finally
        {
            FileLoadRecapProgress.IsActive = false;
            FileLoadRecapProgress.Visibility = Visibility.Collapsed;
            FileLoadRunRecapBtn.IsEnabled = true;
        }
    }

    // ── History ──────────────────────────────────────────────────────

    /// Refresh the History list. Called on tab activation, on Refresh
    /// click, on search submit, after delete. The FFI returns up to 200
    /// rows newest-first; pagination beyond that is a follow-up.
    private void LoadHistoryItems()
    {
        try
        {
            string? json;
            var query = ViewModel.HistorySearchQuery?.Trim() ?? "";
            const int Limit = 200;
            // 4 MB. The previous 256 KB ceiling was overflowed by a single
            // long file-load row: 95-min meeting → 50 KB transcript +
            // 13 KB word_timestamps in one row, so 200 such rows easily
            // top 256 KB and the FFI returns a TRUNCATED JSON blob that
            // System.Text.Json then fails to parse with "Expected end of
            // string … BytePositionInLine: 262143" → empty list.
            const int BufLen = 4 << 20;
            var buf = new byte[BufLen];
            int len;
            if (string.IsNullOrEmpty(query))
                len = DimmyNative.dimmy_history_recent(Limit, buf, BufLen);
            else
                len = DimmyNative.dimmy_history_search(query, Limit, buf, BufLen);
            if (len <= 0)
            {
                ViewModel.HistoryItems.Clear();
                return;
            }
            json = System.Text.Encoding.UTF8.GetString(buf, 0, len);
            using var doc = System.Text.Json.JsonDocument.Parse(json);
            ViewModel.HistoryItems.Clear();
            foreach (var el in doc.RootElement.EnumerateArray())
            {
                var item = new ViewModels.HistoryItemViewModel
                {
                    Id = el.GetProperty("id").GetInt64(),
                    Text = el.GetProperty("text").GetString() ?? "",
                    Language = el.GetProperty("language").GetString() ?? "",
                    Timestamp = el.GetProperty("timestamp").GetDouble(),
                    Duration = el.GetProperty("duration").GetDouble(),
                    WordCount = el.GetProperty("word_count").GetInt32(),
                };
                if (el.TryGetProperty("enhanced_text", out var et) && et.ValueKind == System.Text.Json.JsonValueKind.String)
                    item.EnhancedText = et.GetString();
                if (el.TryGetProperty("audio_path", out var ap) && ap.ValueKind == System.Text.Json.JsonValueKind.String)
                    item.AudioPath = ap.GetString();
                if (el.TryGetProperty("app_process_name", out var apn) && apn.ValueKind == System.Text.Json.JsonValueKind.String)
                    item.AppProcessName = apn.GetString();
                if (el.TryGetProperty("app_bundle_id", out var abi) && abi.ValueKind == System.Text.Json.JsonValueKind.String)
                    item.AppBundleId = abi.GetString();
                if (el.TryGetProperty("llm_style", out var ls) && ls.ValueKind == System.Text.Json.JsonValueKind.String)
                    item.LlmStyle = ls.GetString();
                if (el.TryGetProperty("llm_translate_to", out var ltt) && ltt.ValueKind == System.Text.Json.JsonValueKind.String)
                    item.LlmTranslateTo = ltt.GetString();
                if (el.TryGetProperty("size_bytes", out var sb))
                    item.SizeBytes = sb.GetInt64();
                ViewModel.HistoryItems.Add(item);
            }
            App.Log($"History: loaded {ViewModel.HistoryItems.Count} rows (query='{query}')", "History");
        }
        catch (Exception ex)
        {
            App.Log($"History load failed: {ex.Message}", "History");
        }
    }

    private void HistorySearchBox_KeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        if (e.Key == global::Windows.System.VirtualKey.Enter)
        {
            LoadHistoryItems();
            e.Handled = true;
        }
    }

    private void HistorySearch_Click(object sender, RoutedEventArgs e) => LoadHistoryItems();
    private void HistoryRefresh_Click(object sender, RoutedEventArgs e)
    {
        ViewModel.HistorySearchQuery = "";
        LoadHistoryItems();
    }

    private void HistoryCopyRaw_Click(object sender, RoutedEventArgs e)
    {
        if (ViewModel.SelectedHistoryItem is { } item)
            CopyToClipboard(item.Text);
    }

    private void HistoryCopyEnhanced_Click(object sender, RoutedEventArgs e)
    {
        if (ViewModel.SelectedHistoryItem is { } item && !string.IsNullOrEmpty(item.EnhancedText))
            CopyToClipboard(item.EnhancedText!);
    }

    private static void CopyToClipboard(string text)
    {
        try
        {
            var dp = new global::Windows.ApplicationModel.DataTransfer.DataPackage();
            dp.SetText(text);
            global::Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(dp);
        }
        catch (Exception ex) { App.Log($"clipboard set failed: {ex.Message}", "History"); }
    }

    private void HistoryDelete_Click(object sender, RoutedEventArgs e)
    {
        if (ViewModel.SelectedHistoryItem is not { } item) return;
        var rc = DimmyNative.dimmy_history_delete((int)item.Id);
        if (rc == 0)
        {
            ViewModel.HistoryItems.Remove(item);
            ViewModel.SelectedHistoryItem = null;
        }
        else
        {
            App.Log($"history_delete returned {rc} for id {item.Id}", "History");
        }
    }

    /// Render the selected History row's transcript + enhanced text into
    /// their RichTextBlocks. Called whenever SelectedHistoryItem changes.
    ///
    /// Why not bind directly to .Text like the v1 layout did: a single
    /// TextBlock with TextWrapping=Wrap and a 50 KB string of paragraph
    /// text (e.g. a 95-min meeting transcript) chokes WinUI's text
    /// engine — the layout pass either takes minutes or shows blank.
    /// TranscriptRenderer breaks the text into paragraphs at line
    /// breaks and styles [mic]/[system] markers + timestamps, which
    /// WinUI handles smoothly even at hundreds of kilobytes.
    private void RefreshHistoryTextRendering()
    {
        var item = ViewModel.SelectedHistoryItem;
        if (item == null)
        {
            HistoryRawTextRich?.Blocks.Clear();
            HistoryEnhancedTextRich?.Blocks.Clear();
            return;
        }
        if (HistoryRawTextRich != null)
            Helpers.TranscriptRenderer.Render(HistoryRawTextRich, item.Text ?? "");
        if (HistoryEnhancedTextRich != null)
            Helpers.TranscriptRenderer.Render(
                HistoryEnhancedTextRich,
                item.EnhancedText ?? "");
    }

    /// Render a waveform of the selected History row's audio file and
    /// point the MediaPlayerElement at it. Called on every selection
    /// change. No-ops cleanly when the row has no audio (HasAudio=false
    /// hides the whole Grid via XAML binding) or the file is missing.
    private async System.Threading.Tasks.Task RefreshHistoryAudioAsync()
    {
        try
        {
            HistoryWaveformCanvas?.Children.Clear();
            if (HistoryAudioPlayer != null) HistoryAudioPlayer.Source = null;

            var item = ViewModel.SelectedHistoryItem;
            if (item == null || !item.HasAudio) return;
            var path = item.AudioPath;
            if (string.IsNullOrEmpty(path) || !System.IO.File.Exists(path)) return;

            // Audio source — let WinUI's MediaPlayer handle decoding/seek.
            try
            {
                var uri = new Uri(path);
                if (HistoryAudioPlayer != null)
                {
                    HistoryAudioPlayer.Source =
                        global::Windows.Media.Core.MediaSource.CreateFromUri(uri);
                    // Hook PositionChanged once per source — drives the
                    // orange playhead overlay on top of the waveform.
                    var mp = HistoryAudioPlayer.MediaPlayer;
                    if (mp != null)
                    {
                        mp.PlaybackSession.PositionChanged -= OnPlaybackPositionChanged;
                        mp.PlaybackSession.PositionChanged += OnPlaybackPositionChanged;
                    }
                }
            }
            catch (Exception ex)
            {
                App.Log($"MediaPlayer setSource exc: {ex.Message}", "History");
            }

            // Waveform — read peaks on a background thread, then draw.
            // Bucket count tracks the canvas width so bars are ~3px wide.
            if (HistoryWaveformCanvas == null) return;
            double width = HistoryWaveformCanvas.ActualWidth;
            if (width <= 0) width = 600; // fallback before first layout pass
            int buckets = (int)Math.Max(60, Math.Min(400, width / 3));
            var peaks = await System.Threading.Tasks.Task.Run(()
                => Helpers.WavPeaks.ReadPeaksAny(path, buckets));
            if (peaks.Length == 0) return;
            // Re-check selection: user may have switched to a different
            // row while we were reading the WAV.
            if (ViewModel.SelectedHistoryItem != item) return;

            DrawWaveform(peaks);
        }
        catch (Exception ex)
        {
            App.Log($"RefreshHistoryAudioAsync exc: {ex.Message}", "History");
        }
    }

    /// Vertical line that tracks current playback position. Created
    /// on first DrawWaveform, repositioned in HistoryAudioPlayer's
    /// PositionChanged handler. Kept as a field so it survives the
    /// per-frame Canvas re-render.
    private Microsoft.UI.Xaml.Shapes.Rectangle? _waveformPlayhead;

    private void DrawWaveform(float[] peaks)
    {
        if (HistoryWaveformCanvas == null || peaks.Length == 0) return;
        HistoryWaveformCanvas.Children.Clear();
        double w = HistoryWaveformCanvas.ActualWidth;
        double h = HistoryWaveformCanvas.ActualHeight;
        if (w <= 0 || h <= 0) return;
        double barWidth = Math.Max(1, w / peaks.Length - 1);
        double mid = h / 2.0;
        var brush = new Microsoft.UI.Xaml.Media.SolidColorBrush(
            (Microsoft.UI.Colors.DodgerBlue));
        for (int i = 0; i < peaks.Length; i++)
        {
            double barH = Math.Max(1, peaks[i] * (h - 2));
            var rect = new Microsoft.UI.Xaml.Shapes.Rectangle
            {
                Width = barWidth,
                Height = barH,
                Fill = brush,
                RadiusX = 1, RadiusY = 1,
            };
            Microsoft.UI.Xaml.Controls.Canvas.SetLeft(rect, i * (w / peaks.Length));
            Microsoft.UI.Xaml.Controls.Canvas.SetTop(rect, mid - barH / 2.0);
            HistoryWaveformCanvas.Children.Add(rect);
        }

        // Re-add the playhead on top of the bars. Solid orange, 2 px
        // wide, positioned at x=0 until the player starts emitting
        // PositionChanged events.
        _waveformPlayhead = new Microsoft.UI.Xaml.Shapes.Rectangle
        {
            Width = 2,
            Height = h,
            Fill = new Microsoft.UI.Xaml.Media.SolidColorBrush(
                Microsoft.UI.Colors.OrangeRed),
            IsHitTestVisible = false,
        };
        Microsoft.UI.Xaml.Controls.Canvas.SetLeft(_waveformPlayhead, 0);
        Microsoft.UI.Xaml.Controls.Canvas.SetTop(_waveformPlayhead, 0);
        HistoryWaveformCanvas.Children.Add(_waveformPlayhead);
    }

    /// Tap on the waveform → seek the MediaPlayer to that fractional
    /// position. Bound in XAML via Canvas.PointerPressed so the
    /// affordance is "click anywhere on the bars to jump there".
    private void HistoryWaveformCanvas_PointerPressed(object sender,
        Microsoft.UI.Xaml.Input.PointerRoutedEventArgs e)
    {
        try
        {
            if (HistoryWaveformCanvas == null || HistoryAudioPlayer?.MediaPlayer == null)
                return;
            var pt = e.GetCurrentPoint(HistoryWaveformCanvas).Position;
            double w = HistoryWaveformCanvas.ActualWidth;
            if (w <= 0) return;
            double frac = Math.Max(0, Math.Min(1, pt.X / w));
            var session = HistoryAudioPlayer.MediaPlayer.PlaybackSession;
            var total = session.NaturalDuration;
            if (total.TotalSeconds <= 0) return;
            session.Position = TimeSpan.FromSeconds(total.TotalSeconds * frac);
            UpdatePlayheadPosition(frac);
        }
        catch (Exception ex)
        {
            App.Log($"WaveformPointer exc: {ex.Message}", "History");
        }
    }

    private void UpdatePlayheadPosition(double frac)
    {
        if (_waveformPlayhead == null || HistoryWaveformCanvas == null) return;
        double w = HistoryWaveformCanvas.ActualWidth;
        if (w <= 0) return;
        Microsoft.UI.Xaml.Controls.Canvas.SetLeft(_waveformPlayhead,
            Math.Max(0, Math.Min(w - 2, w * frac)));
    }

    /// MediaPlayer fires PositionChanged off the UI thread — hop back
    /// to the dispatcher before touching the Canvas. Computes the
    /// fractional position from the session's NaturalDuration and
    /// drives the orange playhead overlay.
    private void OnPlaybackPositionChanged(
        global::Windows.Media.Playback.MediaPlaybackSession session, object args)
    {
        try
        {
            var total = session.NaturalDuration.TotalSeconds;
            if (total <= 0) return;
            double frac = session.Position.TotalSeconds / total;
            DispatcherQueue.TryEnqueue(() => UpdatePlayheadPosition(frac));
        }
        catch { /* MediaPlayer races on source-swap; ignore. */ }
    }

    private void LoadConfig()
    {
        // Read from config.json file first — it has all fields including UI-only ones.
        // Cartella DEVE matchare quella che il Rust core usa: prod →
        // `dimmy/`, staging-flavor → `dimmy-staging/`. Hardcoding
        // "dimmy" qui faceva sì che un binario con
        // DIMMY_BUILD_FLAVOR=staging leggesse dalla cartella prod, poi
        // sovrascrivesse la cartella staging con UI state vuoto via
        // dimmy_set_config_json — silently distruggendo `app_rules`,
        // `user_dict`, ecc. salvati dal Rust. Burned 2026-05-12.
        string? fileJson = null;
        try
        {
            var path = System.IO.Path.Combine(Services.BuildInfo.ConfigDirPath, "config.json");
            if (System.IO.File.Exists(path))
                fileJson = System.IO.File.ReadAllText(path);
        }
        catch { }

        if (!string.IsNullOrEmpty(fileJson))
            ViewModel.LoadFromJson(fileJson);

        // Win-only UI prefs live outside config.json (CLAUDE.md
        // single-writer rule). Pull them in so the toggles in the Pill
        // section reflect the on-disk state.
        var uiPrefs = Services.UiPreferences.Load();
        ViewModel.PillShowOnStartup = uiPrefs.PillShowOnStartup;
        ViewModel.PillShowOnHotkey = uiPrefs.PillShowOnHotkey;
        ViewModel.ShowTaskbarIcon = uiPrefs.ShowTaskbarIcon;
        ViewModel.TrayIconAlwaysVisible = uiPrefs.TrayIconAlwaysVisible;
        // Theme lives in UiPreferences, not config.json — see
        // UiPreferences.Theme docstring for the bug history. Override
        // any value LoadFromJson may have produced (which would always
        // be the "Default" fallback because Rust strips unknown keys).
        ViewModel.Theme = uiPrefs.Theme;

        // Also read from FFI for runtime-only fields (has_key, has_llm_key, devices)
        // that are NOT in config.json (Rust computes them from keystore).
        // We also cache the per-provider has_*_key flags so a dropdown
        // change can refresh the green-check without first persisting
        // config + waiting for Rust to round-trip.
        try
        {
            var ffiJson = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_config_json, 16384);
            if (!string.IsNullOrEmpty(ffiJson))
            {
                using var doc = System.Text.Json.JsonDocument.Parse(ffiJson);
                var r = doc.RootElement;
                if (r.TryGetProperty("has_key", out var hk))
                    ViewModel.HasApiKey = hk.GetBoolean();
                if (r.TryGetProperty("has_llm_key", out var hlk))
                    ViewModel.HasLlmKey = hlk.GetBoolean();
                // Notion token presence is a runtime-only flag derived
                // from the keystore (same as has_key / has_llm_key).
                // Rust strips it on config.json save, so LoadFromJson
                // never sees it — we MUST pull it from the FFI snapshot
                // here, otherwise the Integrations summary card stays
                // "Not connected" on every relaunch even though the
                // token sits in keys.enc. Burned 2026-05-10.
                if (r.TryGetProperty("has_notion_token", out var hnt))
                    ViewModel.HasNotionToken = hnt.GetBoolean();
                _sttKeyByProvider.Clear();
                _llmKeyByProvider.Clear();
                CacheProviderKeyFlags(r);
                if (r.TryGetProperty("devices", out var devArr) &&
                    devArr.ValueKind == System.Text.Json.JsonValueKind.Array)
                {
                    var list = new System.Collections.Generic.List<string>();
                    foreach (var d in devArr.EnumerateArray())
                        if (d.GetString() is string s) list.Add(s);
                    ViewModel.Devices = list;
                }
            }
        }
        catch { }

        System.Diagnostics.Debug.WriteLine(
            $"[Settings] Loaded: has_key={ViewModel.HasApiKey}, has_llm_key={ViewModel.HasLlmKey}, " +
            $"llm_enabled={ViewModel.LlmStyle != "off"}, llm_style={ViewModel.LlmStyle}, " +
            $"llm_url={ViewModel.LlmApiUrl}, use_same_key={ViewModel.LlmUseSameKey}");
    }

    /// <summary>
    /// Sync the Provider ComboBox selection to match the current ApiUrl from config.
    /// </summary>
    // ── Single-source model pickers ──────────────────────────────────
    // STT / LLM / recap picker items are built here from the embedded model
    // catalog (ModelCatalog, fed by assets/model-catalog.json over FFI) so a
    // model add/remove touches only the JSON. The item Tag is the synthetic
    // preset Name (STT/LLM) or the bare model id (recap) — exactly what the
    // existing SelectionChanged + Sync handlers already key on, so the
    // handler logic is untouched. Mac builds the same lists from the same
    // bytes. Local models are injected separately (PopulateRecapLocalModels).

    private static StackPanel ProviderItemPanel(string icon, string label)
    {
        var sp = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        sp.Children.Add(new Image
        {
            Width = 16,
            Height = 16,
            Source = new Microsoft.UI.Xaml.Media.Imaging.SvgImageSource(
                new Uri($"ms-appx:///Assets/Providers/{icon}.svg")),
        });
        sp.Children.Add(new TextBlock { Text = label, VerticalAlignment = VerticalAlignment.Center });
        return sp;
    }

    private static ComboBoxItem CustomEndpointItem()
    {
        var sp = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        sp.Children.Add(new FontIcon { Glyph = "", FontSize = 14, VerticalAlignment = VerticalAlignment.Center });
        sp.Children.Add(new TextBlock { Text = "Custom endpoint", VerticalAlignment = VerticalAlignment.Center });
        return new ComboBoxItem { Tag = "custom", Content = sp };
    }

    private void PopulateSttProviderPicker()
    {
        ProviderComboBox.Items.Clear();
        foreach (var e in ModelCatalog.SttEntries())
            ProviderComboBox.Items.Add(new ComboBoxItem { Tag = e.Name, Content = ProviderItemPanel(e.Icon, e.Label) });
        ProviderComboBox.Items.Add(CustomEndpointItem());
    }

    private void PopulateLlmProviderPicker()
    {
        LlmProviderComboBox.Items.Clear();
        foreach (var e in ModelCatalog.LlmEntries())
            LlmProviderComboBox.Items.Add(new ComboBoxItem { Tag = e.Name, Content = ProviderItemPanel(e.Icon, e.Label) });
        LlmProviderComboBox.Items.Add(CustomEndpointItem());
    }

    private void PopulateRecapModelPicker()
    {
        RecapModelComboBox.Items.Clear();
        RecapModelComboBox.Items.Add(new ComboBoxItem
        {
            Tag = "",
            Content = new TextBlock { Text = "Auto · pick best for your provider", VerticalAlignment = VerticalAlignment.Center },
        });
        // Cloud recap models — Anthropic / OpenAI / Gemini only; those are the
        // vendors recap dispatch can route by model id (Provider::from_model_id).
        foreach (var r in ModelCatalog.RecapEntries())
            RecapModelComboBox.Items.Add(new ComboBoxItem { Tag = r.Id, Content = ProviderItemPanel(r.Icon, r.Label) });
        // Trailing custom sentinel. Local "Local · …" entries are injected just
        // before it by PopulateRecapLocalModels().
        RecapModelComboBox.Items.Add(new ComboBoxItem
        {
            Tag = "__custom__",
            Content = new TextBlock { Text = "Custom model id…", VerticalAlignment = VerticalAlignment.Center },
        });
    }

    private void SyncProviderComboBox()
    {
        // Match by URL + model (multiple presets can share the same URL, e.g. Groq turbo vs v3)
        var preset = SettingsViewModel.ProviderPresets.FirstOrDefault(p =>
            !string.IsNullOrEmpty(p.Url) && p.Url == ViewModel.ApiUrl && p.DefaultModel == ViewModel.ApiModel)
            ?? SettingsViewModel.ProviderPresets.FirstOrDefault(p =>
            !string.IsNullOrEmpty(p.Url) && p.Url == ViewModel.ApiUrl);

        if (preset != null)
        {
            var tag = preset.Name.ToLowerInvariant();
            for (int i = 0; i < ProviderComboBox.Items.Count; i++)
            {
                if (ProviderComboBox.Items[i] is ComboBoxItem item && item.Tag is string t && t == tag)
                {
                    ProviderComboBox.SelectedIndex = i;
                    return;
                }
            }
        }

        // No match — select "Custom endpoint" (last item)
        ProviderComboBox.SelectedIndex = ProviderComboBox.Items.Count - 1;
        CustomUrlBox.Visibility = Visibility.Visible;
        CustomModelBox.Visibility = Visibility.Visible;
    }

    private void SyncLlmProviderComboBox()
    {
        // Match by URL + model (multiple presets can share the same URL, e.g. Anthropic Haiku vs Sonnet)
        var preset = SettingsViewModel.LlmProviderPresets.FirstOrDefault(p =>
            !string.IsNullOrEmpty(p.Url) && p.Url == ViewModel.LlmApiUrl && p.DefaultModel == ViewModel.LlmApiModel)
            ?? SettingsViewModel.LlmProviderPresets.FirstOrDefault(p =>
            !string.IsNullOrEmpty(p.Url) && p.Url == ViewModel.LlmApiUrl);

        if (preset != null)
        {
            var tag = preset.Name.ToLowerInvariant();
            for (int i = 0; i < LlmProviderComboBox.Items.Count; i++)
            {
                if (LlmProviderComboBox.Items[i] is ComboBoxItem item && item.Tag is string t && t == tag)
                {
                    LlmProviderComboBox.SelectedIndex = i;
                    return;
                }
            }
        }

        // No match — select "Custom endpoint" (last item)
        LlmProviderComboBox.SelectedIndex = LlmProviderComboBox.Items.Count - 1;
        LlmCustomUrlBox.Visibility = Visibility.Visible;
        LlmCustomModelBox.Visibility = Visibility.Visible;
    }

    private void LlmProvider_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (sender is ComboBox cb && cb.SelectedItem is ComboBoxItem item && item.Tag is string tag)
        {
            var preset = SettingsViewModel.LlmProviderPresets.FirstOrDefault(p =>
                p.Name.ToLowerInvariant() == tag);

            if (preset != null && !string.IsNullOrEmpty(preset.Url))
            {
                ViewModel.LlmApiUrl = preset.Url;
                ViewModel.LlmApiModel = preset.DefaultModel;
                // Refresh the green ✓ for the newly-picked provider — without
                // this the badge shows the previous provider's state until the
                // user clicks Save (and Rust echoes back a fresh config).
                ViewModel.HasLlmKey = ViewModel.HasLlmKeyForUrl(preset.Url);
                LlmCustomUrlBox.Visibility = Visibility.Collapsed;
                LlmCustomModelBox.Visibility = Visibility.Collapsed;
            }
            else
            {
                LlmCustomUrlBox.Visibility = Visibility.Visible;
                LlmCustomModelBox.Visibility = Visibility.Visible;
                ViewModel.HasLlmKey = ViewModel.HasLlmKeyForUrl(ViewModel.LlmApiUrl);
            }
            // Refresh the green-check badge for the newly-selected provider.
            // Without this, switching to a provider with a saved key still
            // showed "no key" until the user closed + reopened Settings.
            ViewModel.HasLlmKey = LookupLlmKeyForTag(tag);
            // Reveal the Claude Code subscription status card when
            // that provider is picked. RefreshClaudeCodeStatus probes
            // the binary + credentials state via FFI; safe to call
            // repeatedly because the Rust side is cached.
            RefreshClaudeCodeStatus();
        }
    }

    // ── Claude Code subscription card ─────────────────────────────

    /// Re-read the Claude Code install state + credentials and
    /// reflect into the SettingsCard. Called on tab activation, on
    /// provider switch, and after the Sign-in button completes (or
    /// the user clicks the manual refresh ↻).
    /// <summary>
    /// Refresh the Settings auth-related UI surfaces in one pass.
    /// Called on every Settings open + after any state change that
    /// affects auth (provider switch, integration status flip,
    /// toggle flip). Owns:
    ///
    ///   - INTEGRATIONS → Anthropic card: status text, glyph,
    ///     Connect/Re-sign in/Test/Refresh button rows.
    ///   - OUTPUT → LLM section: visibility of
    ///     LlmUseSubscriptionCard, LlmUseSameKeyCard, LlmApiKeyCard.
    ///   - OUTPUT → Recap section: visibility of
    ///     RecapUseSubscriptionCard.
    ///
    /// Driven by three booleans:
    ///   - integrationReady — claude CLI installed + logged in
    ///   - isAnthropic      — current LLM provider is Anthropic
    ///   - providerParity   — STT provider == LLM provider
    ///
    /// The LLM-section subscription toggle is visible only when
    /// (isAnthropic AND integrationReady). The same-key toggle is
    /// visible only when providerParity AND the API-key path is
    /// live somewhere. The recap toggle is visible only when
    /// integrationReady.
    private void RefreshAuthIntegrationStatus()
    {
        // ── INPUT STATE ──────────────────────────────────────────
        var llmUrl = ViewModel.LlmApiUrl ?? "";
        var sttUrl = ViewModel.ApiUrl ?? "";
        var isAnthropic = IsAnthropicUrl(llmUrl);
        var providerParity = SameProviderFamily(sttUrl, llmUrl);

        var status = Interop.DimmyNative.GetClaudeCodeStatus();
        var binaryPath = Interop.DimmyNative.GetClaudeCodeBinaryPath() ?? "";
        var integrationReady = status == Interop.DimmyNative.ClaudeCodeStatus.Ready;

        // Coerce LLM subscription off when the provider is not Anthropic —
        // subscription is Claude Code CLI only (Anthropic-only). Without this
        // a leftover llm_auth_method="subscription" from a previous Anthropic
        // session keeps `llmUseSub=true` even after switching to Groq/OpenAI/
        // etc., which collapses `llmKeyPathLive=false` and hides BOTH the
        // "Use saved api key" toggle AND the api-key input — stuck state, no
        // way to save a key. Burned 2026-05-18 on the first-time Groq pick
        // from a fresh Anthropic+subscription baseline. Mirror of the recap
        // auth-method coercion below.
        if (!isAnthropic
            && string.Equals(ViewModel.LlmAuthMethod, "subscription", StringComparison.Ordinal))
        {
            ViewModel.LlmAuthMethod = "api_key";
            App.Log(
                $"[Auth] coerced llm_auth_method='subscription' → 'api_key' (provider not Anthropic: '{llmUrl}')",
                "Auth");
        }
        var llmUseSub = string.Equals(ViewModel.LlmAuthMethod, "subscription",
            StringComparison.Ordinal);
        // Recap auth is INDEPENDENT of the dictation knob. Empty =
        // api_key (the dedicated toggle below decides subscription).
        // The previous inherit semantics (empty → copy llm_auth_method)
        // produced the 2026-05-16 "HTTP 1" bug: a Gemini recap inherited
        // `subscription` from an Anthropic dictation config and was
        // routed to the `claude` CLI which doesn't know Gemini models.
        var recapForceSub = string.Equals(ViewModel.RecapAuthMethod, "subscription",
            StringComparison.Ordinal);

        // Per-tier "key path live?" flags. The previous shared `keyPathLive`
        // OR'd LLM and Recap together, which leaked one tier's subscription
        // choice into the other's UI: switching LLM to Anthropic subscription
        // would NOT hide the LLM key input if Recap was still on api-key.
        // The user-reported bug 2026-05-18 — when LLM = Anthropic +
        // subscription ON, the "Use saved api key" toggle + key input should
        // disappear, regardless of Recap state. Same logic mirrors for Recap.
        var llmKeyPathLive = !llmUseSub;
        var recapKeyPathLive = !recapForceSub;
        // Legacy combined flag kept for the Recap section below until that
        // block is refactored to use `recapKeyPathLive` directly.
        var keyPathLive = llmKeyPathLive || recapKeyPathLive;

        // ── INTEGRATIONS → Anthropic card ────────────────────────
        // Always rendered when the user navigates to Integrations;
        // we only update its content here.
        switch (status)
        {
            case Interop.DimmyNative.ClaudeCodeStatus.Ready:
                var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
                var shownPath = (!string.IsNullOrEmpty(home)
                                 && binaryPath.StartsWith(home, StringComparison.OrdinalIgnoreCase))
                    ? "~" + binaryPath[home.Length..]
                    : binaryPath;
                AnthropicIntegrationStatusText.Text =
                    $"Connected — using `{shownPath}`. Available for LLM rewrite and meeting recap.";
                // \u escape: literal high-codepoint Segoe Fluent
                // glyphs get stripped by some editor / git CRLF
                // round-trips → empty string → no icon. Use the
                // escape so the source is robust to any encoding.
                AnthropicIntegrationStatusGlyph.Glyph = "\uE73E"; // CheckMark
                AnthropicIntegrationStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)
                    Application.Current.Resources["SystemFillColorSuccessBrush"];
                AnthropicIntegrationDisconnectedActions.Visibility = Visibility.Collapsed;
                AnthropicIntegrationConnectedActions.Visibility = Visibility.Visible;
                AnthropicIntegrationTestBtn.IsEnabled = true;
                break;
            case Interop.DimmyNative.ClaudeCodeStatus.NotLoggedIn:
                AnthropicIntegrationStatusText.Text =
                    "Claude Code CLI installed but not signed in. Click Sign in to authenticate via browser.";
                AnthropicIntegrationStatusGlyph.Glyph = "\uEA39"; // Info
                AnthropicIntegrationStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)
                    Application.Current.Resources["TextFillColorTertiaryBrush"];
                AnthropicIntegrationDisconnectedActions.Visibility = Visibility.Visible;
                AnthropicIntegrationConnectedActions.Visibility = Visibility.Collapsed;
                // Binary present, only login missing — direct Sign in
                // is faster than the wizard. Hide the wizard CTA.
                AnthropicIntegrationWizardBtn.Visibility = Visibility.Collapsed;
                AnthropicIntegrationSignInBtn.Visibility = Visibility.Visible;
                AnthropicIntegrationSignInBtn.Style =
                    (Microsoft.UI.Xaml.Style)Application.Current.Resources["AccentButtonStyle"];
                AnthropicIntegrationSignInLabel.Text = "Sign in via browser";
                AnthropicIntegrationSignInBtn.IsEnabled = true;
                break;
            case Interop.DimmyNative.ClaudeCodeStatus.NotInstalled:
            default:
                AnthropicIntegrationStatusText.Text =
                    "Claude Code CLI not detected. Click Set up wizard for a guided Node.js + CLI + sign-in walkthrough.";
                AnthropicIntegrationStatusGlyph.Glyph = "\uEA39"; // Info
                AnthropicIntegrationStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)
                    Application.Current.Resources["TextFillColorTertiaryBrush"];
                AnthropicIntegrationDisconnectedActions.Visibility = Visibility.Visible;
                AnthropicIntegrationConnectedActions.Visibility = Visibility.Collapsed;
                // Binary missing — wizard is the right entry point.
                // Hide the dead-end Sign in button (it would fail
                // with NotInstalled anyway) and promote the wizard.
                AnthropicIntegrationWizardBtn.Visibility = Visibility.Visible;
                AnthropicIntegrationWizardBtn.Style =
                    (Microsoft.UI.Xaml.Style)Application.Current.Resources["AccentButtonStyle"];
                AnthropicIntegrationSignInBtn.Visibility = Visibility.Collapsed;
                break;
        }

        // ── OUTPUT → LLM section ─────────────────────────────────
        // The "Use Anthropic subscription" toggle only makes sense
        // when (a) the LLM provider is Anthropic and (b) the
        // integration is connected. Otherwise hide entirely.
        LlmUseSubscriptionCard.Visibility = (isAnthropic && integrationReady)
            ? Visibility.Visible : Visibility.Collapsed;
        LlmUseSubscriptionToggle.IsOn = llmUseSub;

        // Same-key toggle visibility — VENDOR-KEYED design (mirror of
        // the recap section):
        //   1. Derive the LLM vendor from llm_url.
        //   2. Check if ANY upstream scope has a key for that vendor
        //      (Stt-scope OR Llm-scope, see `_sttKeyByProvider` +
        //      `_llmKeyByProvider`). This decouples "reuse saved key"
        //      from "STT and LLM must share the vendor" — a user
        //      with a Gemini STT key and Anthropic LLM gets NO
        //      toggle (no Anthropic key exists), but with a Gemini
        //      STT key and Gemini LLM they DO get the toggle even
        //      if STT is currently set to a third provider.
        //   3. Toggle visible when upstream key exists + keyPathLive.
        //   4. Coerce LlmUseSameKey=false when toggle hidden so the
        //      LLM key input appears (mirror of recap section).
        //
        // The dispatcher (Rust `dimmy_process_with_llm`) reads the
        // matching scope at call time: with toggle ON it tries
        // `Llm(vendor)` first then `Stt(vendor)`.
        var llmVendor = VendorTagFromAnyUrl(llmUrl);
        var hasLlmScopeKey = !string.IsNullOrEmpty(llmVendor)
                             && _llmKeyByProvider.TryGetValue(llmVendor, out var llmKey) && llmKey;
        var hasSttScopeKey = !string.IsNullOrEmpty(llmVendor)
                             && _sttKeyByProvider.TryGetValue(llmVendor, out var sttKey) && sttKey;
        var hasUpstreamKey = hasLlmScopeKey || hasSttScopeKey;
        App.Log(
            $"[Auth] refresh: sttUrl='{sttUrl}' llmUrl='{llmUrl}' llmVendor='{llmVendor}' hasLlmScope={hasLlmScopeKey} hasSttScope={hasSttScopeKey} hasUpstreamKey={hasUpstreamKey} keyPathLive={keyPathLive} llmUseSub={llmUseSub}",
            "Auth");
        if (!hasUpstreamKey && ViewModel.LlmUseSameKey)
        {
            ViewModel.LlmUseSameKey = false;
            App.Log("[Auth] coerced llm_use_same_key=false (no upstream key for LLM vendor)", "Auth");
        }
        // The "use my saved key" inheritance toggle is gone: keys now live
        // ONLY in the Providers & keys page, so there's no second place to
        // reuse a key FROM. The dispatcher still reads Llm(vendor) then
        // Stt(vendor) at call time (llm_use_same_key stays true), so a key
        // saved in Providers is picked up automatically.
        LlmUseSameKeyCard.Visibility = Visibility.Collapsed;
        // Inline LLM key input is for the Custom endpoint ONLY (catalog
        // providers are keyed from Providers & keys). Still hidden under
        // subscription (llmKeyPathLive == false).
        var isLlmCustom = string.Equals(llmVendor, "custom", StringComparison.OrdinalIgnoreCase);
        LlmApiKeyCard.Visibility = (llmKeyPathLive && isLlmCustom)
            ? Visibility.Visible : Visibility.Collapsed;
        // Surface the green ✓ when a key for the LLM vendor already
        // exists in Llm-scope (drives `HasLlmKey` binding on the
        // PasswordBox placeholder + check icon).
        ViewModel.HasLlmKey = hasLlmScopeKey;

        // ── OUTPUT → Recap section ───────────────────────────────
        // The recap subscription toggle is INDEPENDENT of the LLM
        // provider choice. Recap dispatch reads its own
        // `recap_auth_method` from config: "subscription" → claude
        // CLI, otherwise → same HTTP path as the LLM rewrite. So
        // valid combinations include:
        //   - LLM=Groq Llama (key) + Recap=Anthropic subscription
        //   - LLM=local Gemma + Recap=Anthropic subscription
        //   - LLM=Anthropic (key) + Recap=Anthropic subscription
        //
        // The only gate is `integrationReady` — without the `claude`
        // CLI signed in, there's nothing to dispatch through. Coerce
        // RecapAuthMethod to "" when the integration goes away so a
        // saved "subscription" can't silently keep routing.
        // Subscription path is Anthropic-only (Claude CLI does Anthropic).
        // Hide the "Use Anthropic subscription for recap" toggle when:
        //   - the integration isn't connected (CLI not signed in), OR
        //   - the chosen recap model isn't Anthropic (CLI can't dispatch
        //     to Gemini / OpenAI, so the toggle would be inactionable)
        //
        // Coerce a saved `subscription` value to `""` when the toggle
        // becomes invisible — otherwise the dispatcher would keep
        // routing through the CLI even when the user picked a
        // non-Anthropic recap model.
        var recapVendorForSubGate = RecapVendorFromModel(ViewModel.RecapModelOverride, llmUrl);
        var recapModelIsAnthropic = string.Equals(
            recapVendorForSubGate, "anthropic", StringComparison.OrdinalIgnoreCase);
        var canShowRecapSub = integrationReady && recapModelIsAnthropic;
        if (!canShowRecapSub && recapForceSub)
        {
            App.Log(
                $"[Auth] coerced recap_auth_method='subscription' → '' (integration={integrationReady}, model_is_anthropic={recapModelIsAnthropic})",
                "Auth");
            ViewModel.RecapAuthMethod = "";
            recapForceSub = false;
            // Push the coerced value into the Rust core immediately —
            // otherwise the next meeting recap reads the stale
            // `subscription` value from `st.recap_auth_method` and
            // dispatches a Gemini model through the `claude` CLI
            // (the 2026-05-16 "HTTP 1" bug we're fixing).
            if (_loaded) App.Instance?.ApplySettings(ViewModel);
        }
        RecapUseSubscriptionCard.Visibility = canShowRecapSub
            ? Visibility.Visible : Visibility.Collapsed;
        RecapUseSubscriptionToggle.IsOn = recapForceSub;

        // The recap model picker shows ALL curated entries (any
        // vendor) — the user can pick a Gemini model with an
        // Anthropic dictation provider; the dispatcher derives the
        // recap vendor from the model and looks up its keystore key.
        // No vendor filter needed anymore.
        //
        // The chosen recap vendor for the model picker's "current
        // selection coercion" + the conditional Recap-key card is
        // derived from the model id directly.
        var recapVendor = recapForceSub
            ? "anthropic"
            : RecapVendorFromModel(ViewModel.RecapModelOverride, llmUrl);
        // Refresh the conditional UI for the "different provider" case:
        // - PasswordBox + Save visible only when the model's vendor
        //   differs from the dictation vendor AND auth != subscription.
        UpdateRecapKeyCardVisibility(llmUrl);

        // Filter the cloud STT + LLM provider pickers down to the providers
        // the user actually has a key for (keys live in Providers & keys now).
        // Custom is always offered (inline URL+key); Anthropic stays offered
        // when the Claude CLI subscription is connected (keyless path).
        FilterCloudProviderPickers(integrationReady);
        FilterRecapModelPicker(integrationReady);

        // The recap picker IS filtered now (FilterRecapModelPicker above) to
        // the vendors the user connected — Auto/Custom/local always stay,
        // Anthropic stays under subscription. The dispatcher still derives the
        // recap vendor from the model id and pulls its key from the keystore.
        _ = recapVendor; // retained for the subscription gate math above
    }

    /// <summary>Map a URL to a vendor family tag for filtering the
    /// recap model picker. Mirrors the Rust-side `Provider::from_url`
    /// logic + extends with the recap-specific labels (anthropic /
    /// openai / gemini / other). For unknown URLs returns "anthropic"
    /// as a safe default — most users start with the Anthropic preset
    /// and the Anthropic-only models are the most universally tested
    /// recap targets.</summary>
    private static string RecapVendorFromUrl(string url)
    {
        if (string.IsNullOrEmpty(url)) return "anthropic";
        var lower = url.ToLowerInvariant();
        if (lower.Contains("anthropic.com") || lower.StartsWith("claude-code://", StringComparison.Ordinal))
            return "anthropic";
        if (lower.Contains("openai.com")) return "openai";
        if (lower.Contains("googleapis.com")) return "gemini";
        // Unknown vendors (custom URLs, Groq, OpenRouter, etc.) get
        // "anthropic" as a default for the picker — but the user can
        // always pick Auto or type a Custom model id, so this is a
        // soft fallback, not a hard restriction.
        return "anthropic";
    }

    /// <summary>True iff the model id matches a curated entry that
    /// does NOT belong to the target vendor. "Auto" ("") and Custom
    /// (any string not in `_recapModelKnownTags`) are vendor-agnostic
    /// and never coerced.</summary>
    private static bool RecapModelBelongsToWrongVendor(string? modelId, string targetVendor)
    {
        if (string.IsNullOrEmpty(modelId)) return false;
        var lower = modelId.ToLowerInvariant();
        // Custom values are not curated — leave alone.
        bool isCurated = false;
        foreach (var tag in _recapModelKnownTags)
        {
            if (string.Equals(tag, modelId, StringComparison.OrdinalIgnoreCase))
            {
                isCurated = true;
                break;
            }
        }
        if (!isCurated) return false;
        // Derive the model's vendor.
        string modelVendor;
        if (lower.StartsWith("claude-")) modelVendor = "anthropic";
        else if (lower.StartsWith("gpt-")) modelVendor = "openai";
        else if (lower.Length >= 2 && lower[0] == 'o' && char.IsDigit(lower[1])) modelVendor = "openai";
        else if (lower.StartsWith("gemini-")) modelVendor = "gemini";
        else modelVendor = "other";
        return modelVendor != targetVendor && modelVendor != "other";
    }

    /// <summary>True iff the URL points at the Anthropic API or the
    /// legacy synthetic `claude-code://` scheme kept for back-compat.</summary>
    private static bool IsAnthropicUrl(string url)
    {
        return url.Contains("anthropic.com", StringComparison.OrdinalIgnoreCase)
            || url.StartsWith("claude-code://", StringComparison.Ordinal);
    }

    /// <summary>True iff STT and LLM URLs map to the same provider
    /// family. Used to gate the "Use same API key as STT" toggle:
    /// reusing the key only makes sense for same-vendor pairs.</summary>
    /// Map an API URL to a vendor tag matching the keystore cache
    /// keys (groq / openai / anthropic / gemini / openrouter /
    /// deepgram / fireworks / together / custom). Empty string for
    /// unknown URLs so callers can distinguish "this is a custom
    /// endpoint, no upstream key" from "matches anthropic" etc.
    /// Used by the LLM section to look up `_sttKeyByProvider` and
    /// `_llmKeyByProvider` per the chosen LLM URL.
    /// <summary>Trim the cloud STT + LLM provider pickers to providers the user
    /// actually has a key for. A vendor counts as connected when it has a key in
    /// ANY scope (STT or LLM) — the same provider API key works for all of that
    /// vendor's capabilities, so a Groq key saved for speech also unlocks Groq's
    /// rewrite + recap models. Custom is always offered; Anthropic stays under
    /// the Claude CLI subscription; the selected item is never removed.</summary>
    private void FilterCloudProviderPickers(bool integrationReady)
    {
        RebuildCombo(ProviderComboBox, item =>
        {
            var v = ((item.Tag as string) ?? "").Split('-')[0].ToLowerInvariant();
            return v == "custom" || HasAnyKeyForVendor(v);
        });
        RebuildCombo(LlmProviderComboBox, item =>
        {
            var v = ((item.Tag as string) ?? "").Split('-')[0].ToLowerInvariant();
            return v == "custom" || HasAnyKeyForVendor(v)
                   || (v == "anthropic" && integrationReady);
        });
    }

    /// <summary>Trim the recap-model picker the same way. Recap tags are model
    /// IDs (claude-opus-4-7, gpt-5, gemini-3.1-pro), so the vendor is derived via
    /// RecapVendorFromModel. Auto, Custom, and local models are always kept.</summary>
    private void FilterRecapModelPicker(bool integrationReady)
    {
        RebuildCombo(RecapModelComboBox, item =>
        {
            var tag = (item.Tag as string) ?? "";
            if (tag.Length == 0 || tag == "__custom__"
                || tag.StartsWith("local:", StringComparison.Ordinal))
                return true; // Auto / custom / local LLM need no cloud key
            var v = RecapVendorFromModel(tag, "");
            return HasAnyKeyForVendor(v) || (v == "anthropic" && integrationReady);
        });
    }

    // SINGLE source of truth = the LIVE keystore snapshot cached by
    // CacheProviderKeyFlags (dimmy_get_config_json). The ViewModel's
    // _sttHasKeyByProvider is populated from config.json which does NOT carry
    // the has_*_key flags (Rust strips them on save) → it's all-false and must
    // NOT drive this filter. The Providers & keys page is routed through the
    // same _sttKeyByProvider/_llmKeyByProvider below, so the two never
    // disagree. "Has a key in ANY scope" because the same provider key works
    // for all of that vendor's capabilities.
    private bool HasAnyKeyForVendor(string vendor)
        => (_sttKeyByProvider.TryGetValue(vendor, out var s) && s)
           || (_llmKeyByProvider.TryGetValue(vendor, out var l) && l);

    // Master (full, ordered) item set per filtered combo, snapshotted once
    // before any item is removed, so the filter can add items back when the
    // key set changes. Keyed by the combo instance.
    private readonly System.Collections.Generic.Dictionary<ComboBox,
        System.Collections.Generic.List<ComboBoxItem>> _comboMasters = new();
    private bool _rebuildingCombos;

    /// <summary>Filter a static ComboBox by REMOVING items rather than
    /// collapsing them — collapsed ComboBox items wreck the dropdown's scroll
    /// measurement (the "jumpy scroll"). Idempotent + cheap: no-ops when the
    /// surviving set is unchanged, so the frequent auth refreshes don't churn
    /// the dropdown. Skips before load (selections must be set first), never
    /// drops the current selection, and guards against the re-entrant
    /// SelectionChanged that Items.Clear() raises.</summary>
    private void RebuildCombo(ComboBox? combo, Func<ComboBoxItem, bool> keep)
    {
        if (combo == null || !_loaded || _rebuildingCombos) return;
        if (!_comboMasters.TryGetValue(combo, out var master))
        {
            master = combo.Items.OfType<ComboBoxItem>().ToList();
            _comboMasters[combo] = master;
        }
        var selected = combo.SelectedItem as ComboBoxItem;
        var wanted = master
            .Where(it => keep(it) || ReferenceEquals(it, selected))
            .ToList();
        if (combo.Items.Count == wanted.Count
            && combo.Items.Cast<object>().SequenceEqual(wanted.Cast<object>()))
            return; // nothing changed — don't touch the dropdown
        try
        {
            _rebuildingCombos = true;
            combo.Items.Clear();
            foreach (var it in wanted) combo.Items.Add(it);
            if (selected != null && combo.Items.Contains(selected))
                combo.SelectedItem = selected;
        }
        finally { _rebuildingCombos = false; }
    }

    private static string VendorTagFromAnyUrl(string url)
    {
        if (string.IsNullOrEmpty(url)) return "";
        if (url.Contains("groq.com", StringComparison.OrdinalIgnoreCase)) return "groq";
        if (url.Contains("openai.com", StringComparison.OrdinalIgnoreCase)) return "openai";
        if (url.Contains("anthropic.com", StringComparison.OrdinalIgnoreCase)) return "anthropic";
        if (url.StartsWith("claude-code://", StringComparison.Ordinal)) return "anthropic";
        if (url.Contains("googleapis.com", StringComparison.OrdinalIgnoreCase)) return "gemini";
        if (url.Contains("openrouter.ai", StringComparison.OrdinalIgnoreCase)) return "openrouter";
        if (url.Contains("deepgram.com", StringComparison.OrdinalIgnoreCase)) return "deepgram";
        if (url.Contains("fireworks.ai", StringComparison.OrdinalIgnoreCase)) return "fireworks";
        if (url.Contains("together.xyz", StringComparison.OrdinalIgnoreCase)
            || url.Contains("together.ai", StringComparison.OrdinalIgnoreCase)) return "together";
        return "custom";
    }

    private static bool SameProviderFamily(string sttUrl, string llmUrl)
    {
        static string Family(string url) => url switch
        {
            var u when u.Contains("groq.com", StringComparison.OrdinalIgnoreCase) => "groq",
            var u when u.Contains("openai.com", StringComparison.OrdinalIgnoreCase) => "openai",
            var u when u.Contains("anthropic.com", StringComparison.OrdinalIgnoreCase) => "anthropic",
            var u when u.StartsWith("claude-code://", StringComparison.Ordinal) => "anthropic",
            var u when u.Contains("googleapis.com", StringComparison.OrdinalIgnoreCase) => "gemini",
            var u when u.Contains("openrouter.ai", StringComparison.OrdinalIgnoreCase) => "openrouter",
            var u when u.Contains("deepgram.com", StringComparison.OrdinalIgnoreCase) => "deepgram",
            var u when u.Contains("fireworks.ai", StringComparison.OrdinalIgnoreCase) => "fireworks",
            var u when u.Contains("together.xyz", StringComparison.OrdinalIgnoreCase) => "together",
            var u when u.Contains("together.ai", StringComparison.OrdinalIgnoreCase) => "together",
            _ => "other",
        };
        return Family(sttUrl) == Family(llmUrl)
            && !string.IsNullOrEmpty(Family(sttUrl))
            && Family(sttUrl) != "other";
    }

    // Backward-compat alias for code paths that still call the old
    // refresh name (e.g. SyncLlmProviderComboBox after a provider
    // dropdown change). Forwards to the new unified method.
    private void RefreshClaudeCodeStatus() => RefreshAuthIntegrationStatus();

    // ── Integrations → Anthropic card handlers ──────────────────

    /// <summary>
    /// Open the multi-step setup wizard. Smart-skips to whichever step
    /// matches the current install state (so users re-running the
    /// wizard after a partial install land on the right step). On
    /// successful completion the wizard reports <c>Completed=true</c>
    /// — we then trigger a status refresh + flip
    /// <c>llm_auth_method=subscription</c> on the user's behalf so the
    /// integration is live without needing a second click in
    /// Output → LLM.
    /// </summary>
    private async void AnthropicIntegrationWizard_Click(object sender, RoutedEventArgs e)
    {
        await ShowClaudeWizardAsync(forceStep1: false);
    }

    /// <summary>
    /// "Re-run setup" — open the wizard with smart-skip disabled so
    /// the user walks all 3 steps regardless of current state. Used
    /// to verify an existing install, update Node, or just preview
    /// the wizard's copy.
    /// </summary>
    private async void AnthropicIntegrationRerunWizard_Click(object sender, RoutedEventArgs e)
    {
        await ShowClaudeWizardAsync(forceStep1: true);
    }

    private async System.Threading.Tasks.Task ShowClaudeWizardAsync(bool forceStep1)
    {
        try
        {
            var dialog = new ClaudeConnectDialog
            {
                XamlRoot = this.Content.XamlRoot,
                RequestedTheme = Helpers.ThemeHelper.ResolvedElementTheme(),
                ForceStartAtStep1 = forceStep1,
            };
            await dialog.ShowAsync();
            // Caches were cleared during the wizard; refresh the card.
            Interop.DimmyNative.RecheckClaudeCode();
            RefreshAuthIntegrationStatus();
            if (dialog.Completed)
            {
                // Flip the LLM auth method to subscription so the user
                // immediately sees the change in Output → LLM.
                ViewModel.LlmAuthMethod = "subscription";
                Interop.DimmyNative.TrackEvent("claude_code.wizard_completed");
            }
        }
        catch (Exception ex)
        {
            App.Log($"Wizard launch exc: {ex}", "ClaudeCode");
        }
    }

    private async void AnthropicIntegrationSignIn_Click(object sender, RoutedEventArgs e)
    {
        AnthropicIntegrationSignInBtn.IsEnabled = false;
        AnthropicIntegrationStatusText.Text =
            "Launching `claude /login` — complete the OAuth flow in the new terminal window.";
        try
        {
            var ok = Interop.DimmyNative.SpawnClaudeCodeLogin();
            if (!ok)
            {
                AnthropicIntegrationStatusText.Text =
                    "Could not start `claude /login`. Open a terminal and run it manually.";
                Interop.DimmyNative.TrackEvent("claude_code.login_completed",
                    new { outcome = "spawn_failed" });
                return;
            }
            // Poll status every 2 s for 3 min — long enough for the
            // user to complete the OAuth dance even on a slow link.
            for (int i = 0; i < 90; i++)
            {
                await System.Threading.Tasks.Task.Delay(2000);
                var status = Interop.DimmyNative.GetClaudeCodeStatus();
                if (status == Interop.DimmyNative.ClaudeCodeStatus.Ready)
                {
                    Interop.DimmyNative.TrackEvent("claude_code.login_completed",
                        new { outcome = "success" });
                    RefreshAuthIntegrationStatus();
                    return;
                }
            }
            AnthropicIntegrationStatusText.Text =
                "Sign-in not completed in 3 minutes. Click Refresh when ready.";
            Interop.DimmyNative.TrackEvent("claude_code.login_completed",
                new { outcome = "timeout" });
        }
        catch (Exception ex)
        {
            AnthropicIntegrationStatusText.Text = $"Sign-in error: {ex.Message}";
            App.Log($"AnthropicIntegration sign-in exc: {ex}", "ClaudeCode");
            Interop.DimmyNative.TrackEvent("claude_code.login_completed",
                new { outcome = "spawn_failed" });
        }
        finally
        {
            AnthropicIntegrationSignInBtn.IsEnabled = true;
        }
    }

    private async void AnthropicIntegrationTest_Click(object sender, RoutedEventArgs e)
    {
        AnthropicIntegrationTestBtn.IsEnabled = false;
        var prevText = AnthropicIntegrationStatusText.Text;
        AnthropicIntegrationStatusText.Text = "Sending ping…";
        try
        {
            var (result, elapsedMs) = await System.Threading.Tasks.Task.Run(
                () => Interop.DimmyNative.PingClaudeCode());
            AnthropicIntegrationStatusText.Text = result switch
            {
                Interop.DimmyNative.ClaudeCodePingResult.Ok =>
                    $"✓ Connection OK — {elapsedMs} ms round-trip via the local `claude` CLI.",
                Interop.DimmyNative.ClaudeCodePingResult.NotInstalled =>
                    "✗ `claude` binary not found. Install Claude Code first.",
                Interop.DimmyNative.ClaudeCodePingResult.NotLoggedIn =>
                    "✗ Not logged in. Click Sign in to authenticate.",
                Interop.DimmyNative.ClaudeCodePingResult.SpawnFailed =>
                    "✗ Could not spawn the CLI. See dimmy.log.",
                Interop.DimmyNative.ClaudeCodePingResult.Timeout =>
                    "✗ Timed out after 15 s — network or rate-limit issue.",
                Interop.DimmyNative.ClaudeCodePingResult.NonZeroExit =>
                    "✗ `claude` returned a non-zero exit code. See dimmy.log.",
                Interop.DimmyNative.ClaudeCodePingResult.InvalidUtf8 =>
                    "✗ Unexpected output from `claude`. See dimmy.log.",
                _ => "✗ Unknown error. See dimmy.log.",
            };
            _ = prevText;
        }
        catch (Exception ex)
        {
            AnthropicIntegrationStatusText.Text = $"Test error: {ex.Message}";
            App.Log($"AnthropicIntegration test exc: {ex}", "ClaudeCode");
        }
        finally
        {
            AnthropicIntegrationTestBtn.IsEnabled = true;
        }
    }

    private void AnthropicIntegrationRefresh_Click(object sender, RoutedEventArgs e)
    {
        RefreshAuthIntegrationStatus();
    }

    // ── Output → LLM section: subscription toggle ───────────────

    private void LlmUseSubscription_Toggled(object sender, RoutedEventArgs e)
    {
        if (!_loaded) return;
        var newAuth = LlmUseSubscriptionToggle.IsOn ? "subscription" : "api_key";
        if (ViewModel.LlmAuthMethod == newAuth) return;
        ViewModel.LlmAuthMethod = newAuth;
        RefreshAuthIntegrationStatus();
    }

    // "Use my saved API key" toggle — flip immediately so the LLM
    // API key input appears/disappears without waiting for the next
    // RefreshAuthIntegrationStatus tick. Mirror of the recap toggle.
    private void LlmUseSameKey_Toggled(object sender, RoutedEventArgs e)
    {
        if (!_loaded) return;
        RefreshAuthIntegrationStatus();
        App.Instance?.ApplySettings(ViewModel);
    }

    // ── Output → Recap section: subscription toggle ─────────────

    private void RecapUseSubscription_Toggled(object sender, RoutedEventArgs e)
    {
        if (!_loaded) return;
        // ON  → "subscription" (force, regardless of LLM toggle)
        // OFF → ""             (inherit from LLM toggle)
        var newAuth = RecapUseSubscriptionToggle.IsOn ? "subscription" : "";
        if (ViewModel.RecapAuthMethod == newAuth) return;
        ViewModel.RecapAuthMethod = newAuth;
        RefreshAuthIntegrationStatus();
    }

    // Per-provider has_key flags cached at Settings open. Maps the
    // ComboBox tag (provider Name lowercased) to true/false. Filled by
    // CacheProviderKeyFlags() on Settings load.
    private readonly System.Collections.Generic.Dictionary<string, bool> _sttKeyByProvider = new(StringComparer.OrdinalIgnoreCase);
    private readonly System.Collections.Generic.Dictionary<string, bool> _llmKeyByProvider = new(StringComparer.OrdinalIgnoreCase);

    /// <summary>Re-read the live keystore key flags from the FFI and refresh
    /// the provider/model-picker filters. Called after a Connect/Remove on the
    /// Providers page so both the Connected pills and the dropdowns reflect the
    /// change immediately (no relaunch). Also invalidates the combo masters so
    /// a newly-connected provider's items come back.</summary>
    private void RefreshKeyFlagsFromFfi()
    {
        try
        {
            var ffiJson = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_config_json, 16384);
            if (string.IsNullOrEmpty(ffiJson)) return;
            using var doc = System.Text.Json.JsonDocument.Parse(ffiJson);
            _sttKeyByProvider.Clear();
            _llmKeyByProvider.Clear();
            CacheProviderKeyFlags(doc.RootElement);
            if (_loaded) RefreshAuthIntegrationStatus(); // re-filters the pickers
        }
        catch (Exception ex)
        {
            App.Log($"RefreshKeyFlagsFromFfi: {ex.Message}", "Providers");
        }
    }

    private void CacheProviderKeyFlags(System.Text.Json.JsonElement r)
    {
        // STT — `has_<provider>_key` flag set per Provider variant.
        // The dropdown tag is the lowercase provider name; the ProviderPreset
        // table holds the exact Tag→Name mapping. We hash both forms so a
        // tag like "groq-v3" still resolves to has_groq_key.
        foreach (var (key, prov) in new[] {
            ("has_groq_key", "groq"),
            ("has_openai_key", "openai"),
            ("has_openrouter_key", "openrouter"),
            ("has_gemini_key", "gemini"),
            ("has_deepgram_key", "deepgram"),
            ("has_fireworks_key", "fireworks"),
            ("has_together_key", "together"),
            ("has_custom_key", "custom"),
        })
        {
            if (r.TryGetProperty(key, out var v)) _sttKeyByProvider[prov] = v.GetBoolean();
        }
        foreach (var (key, prov) in new[] {
            ("has_groq_llm_key", "groq"),
            ("has_openai_llm_key", "openai"),
            ("has_anthropic_llm_key", "anthropic"),
            ("has_gemini_llm_key", "gemini"),
            ("has_openrouter_llm_key", "openrouter"),
            ("has_fireworks_llm_key", "fireworks"),
            ("has_together_llm_key", "together"),
            ("has_custom_llm_key", "custom"),
        })
        {
            if (r.TryGetProperty(key, out var v)) _llmKeyByProvider[prov] = v.GetBoolean();
        }
    }

    private bool LookupSttKeyForTag(string tag)
    {
        // Tags like "groq-v3" / "groq-distil" resolve to has_groq_key.
        // Custom is the catch-all.
        var baseProv = tag.Split('-')[0];
        if (_sttKeyByProvider.TryGetValue(baseProv, out var v)) return v;
        return _sttKeyByProvider.TryGetValue("custom", out var c) && c;
    }

    private bool LookupLlmKeyForTag(string tag)
    {
        var baseProv = tag.Split('-')[0];
        if (_llmKeyByProvider.TryGetValue(baseProv, out var v)) return v;
        return _llmKeyByProvider.TryGetValue("custom", out var c) && c;
    }

    private void SyncLanguageComboBox()
    {
        for (int i = 0; i < SettingsViewModel.Languages.Count; i++)
        {
            if (SettingsViewModel.Languages[i].Key == ViewModel.Language)
            {
                LanguageComboBox.SelectedIndex = i;
                return;
            }
        }
    }

    private void SyncThemeRadioButtons()
    {
        foreach (var child in ThemeRadioPanel.Children)
        {
            if (child is RadioButton rb && rb.Tag is string tag && tag == ViewModel.Theme)
            {
                rb.IsChecked = true;
                // Also apply to settings window appearance
                if (Content is FrameworkElement root)
                    root.RequestedTheme = tag switch
                    {
                        "Light" => ElementTheme.Light,
                        "Dark" => ElementTheme.Dark,
                        _ => ElementTheme.Default,
                    };
                return;
            }
        }
    }

    /// <summary>Model info parsed from FFI JSON.</summary>
    private record LocalModelInfo(string Name, string Filename, int SizeMb, string Description, bool Downloaded);

    private System.Collections.Generic.List<LocalModelInfo> _localModels = new();

    /// Sentinel ComboBox tag identifying the Parakeet entry. Not a real
    /// whisper-model filename — distinguished from `*.bin` by prefix.
    private const string ParakeetTag = "parakeet:fp32";

    private void PopulateLocalModels()
    {
        App.Log(
            $"PopulateLocalModels enter: VM.LocalSttBackend={ViewModel.LocalSttBackend}, VM.LocalModel={ViewModel.LocalModel}",
            "Settings");
        try
        {
            var json = DimmyNative.ListLocalModels();
            if (string.IsNullOrEmpty(json)) return;

            using var doc = JsonDocument.Parse(json);
            _localModels.Clear();
            LocalModelComboBox.Items.Clear();

            int idx = 0;
            int selectedIdx = -1;
            foreach (var el in doc.RootElement.EnumerateArray())
            {
                var name = el.GetProperty("name").GetString() ?? "";
                var filename = el.GetProperty("filename").GetString() ?? "";
                var sizeMb = el.GetProperty("size_mb").GetInt32();
                var desc = el.GetProperty("description").GetString() ?? "";
                var downloaded = el.GetProperty("downloaded").GetBoolean();

                _localModels.Add(new LocalModelInfo(name, filename, sizeMb, desc, downloaded));

                var status = downloaded ? "Ready" : $"{sizeMb}MB";
                var item = new ComboBoxItem
                {
                    Content = $"{name}: {desc} ({status})",
                    Tag = filename
                };
                LocalModelComboBox.Items.Add(item);

                if (ViewModel.LocalSttBackend != "parakeet" && filename == ViewModel.LocalModel)
                    selectedIdx = idx;
                idx++;
            }

            // Append Parakeet as a virtual entry. The backend it picks
            // is implicit — selecting this item flips ViewModel.LocalSttBackend
            // to "parakeet"; selecting any whisper item flips it to "whisper".
            bool parakeetDownloaded = false;
            try { parakeetDownloaded = DimmyNative.dimmy_parakeet_bundle_present() == 1; }
            catch { }
            var parakeetStatus = parakeetDownloaded ? "Ready" : "2.5GB";
            LocalModelComboBox.Items.Add(new ComboBoxItem
            {
                Content = $"Parakeet TDT v3 FP32: fast, EU-language friendly ({parakeetStatus})",
                Tag = ParakeetTag,
            });
            if (ViewModel.LocalSttBackend == "parakeet")
                selectedIdx = idx;

            if (LocalModelComboBox.Items.Count > 0)
            {
                var finalIdx = selectedIdx >= 0 ? selectedIdx : 0;
                App.Log(
                    $"PopulateLocalModels SET SelectedIndex={finalIdx} (count={LocalModelComboBox.Items.Count}, parakeetIdx={idx})",
                    "Settings");
                LocalModelComboBox.SelectedIndex = finalIdx;
            }
        }
        catch (Exception ex) { App.Log($"PopulateLocalModels EXC: {ex.Message}", "Settings"); }
    }

    private void LocalModel_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        App.Log(
            $"LocalModel_SelectionChanged: _loaded={_loaded}, SelectedIndex={LocalModelComboBox.SelectedIndex}, " +
            $"SelectedItem.Tag={(LocalModelComboBox.SelectedItem as ComboBoxItem)?.Tag}, " +
            $"VM.LocalSttBackend={ViewModel.LocalSttBackend}, VM.LocalModel={ViewModel.LocalModel}",
            "Settings");
        if (!_loaded) return;
        if (LocalModelComboBox.SelectedItem is ComboBoxItem item && item.Tag is string tag)
        {
            if (tag == ParakeetTag)
            {
                ViewModel.LocalSttBackend = "parakeet";
                // Auto-enable chunk streaming on the backend switch — the
                // chunked engine is backend-agnostic now (Parakeet OR whisper)
                // and the low-latency live-caption experience is the reason to
                // run a local backend. Set only on the explicit user pick, so
                // a later manual toggle-off is respected until the next pick.
                ViewModel.ChunkStreamingEnabled = true;
                App.Log("→ set LocalSttBackend=parakeet, ChunkStreamingEnabled=true", "Settings");
                // Keep LocalModel pointing at the previous whisper choice
                // so flipping back to a whisper entry restores it.
            }
            else
            {
                ViewModel.LocalSttBackend = "whisper";
                ViewModel.LocalModel = tag;
                // Mirror the Parakeet convenience: whisper now drives the same
                // chunked live-caption path (it keeps up only on GPU, but the
                // user asked for parity). Persisted on close like the Parakeet
                // branch; the streaming/caption toggles also apply live.
                ViewModel.ChunkStreamingEnabled = true;
                App.Log($"→ set LocalSttBackend=whisper, LocalModel={tag}, ChunkStreamingEnabled=true", "Settings");
            }
            CheckModelStatus();
        }
    }

    private void SyncSttMode()
    {
        bool isLocal = ViewModel.SttMode == "local";
        SttModeLocal.IsChecked = isLocal;
        SttModeCloud.IsChecked = !isLocal;
        ApplySttModeVisibility(isLocal);
        if (isLocal)
            CheckModelStatus();
    }

    private void SttMode_Checked(object sender, RoutedEventArgs e)
    {
        if (!_loaded) return;
        if (sender is RadioButton rb && rb.Tag is string tag)
        {
            ViewModel.SttMode = tag;
            bool isLocal = tag == "local";
            ApplySttModeVisibility(isLocal);
            if (isLocal)
                CheckModelStatus();
        }
    }

    private void ApplySttModeVisibility(bool isLocal)
    {
        LocalSttPanel.Visibility = isLocal ? Visibility.Visible : Visibility.Collapsed;
        // Cloud STT panel now bundles Provider + API key + Custom
        // URL/Model together (mirror of Output → LLM layout). Toggled
        // off in Local mode. Vocabulary panel has the same gating.
        CloudSttPanel.Visibility = isLocal ? Visibility.Collapsed : Visibility.Visible;
        CloudVocabularyPanel.Visibility = isLocal ? Visibility.Collapsed : Visibility.Visible;
    }

    /// Convenience: progress callback from Rust during a Parakeet
    /// bundle download. Updates the same DownloadProgress bar used by
    /// the whisper-model download path. Total is 0 when one of the
    /// HEAD calls didn't return a Content-Length — fall back to
    /// indeterminate in that case.
    private void OnParakeetProgress(long downloaded, long total)
    {
        if (total <= 0)
        {
            DownloadProgress.IsIndeterminate = true;
            LocalModelStatus.Text = $"Downloading... {FormatBytes(downloaded)} so far";
            return;
        }
        DownloadProgress.IsIndeterminate = false;
        double percent = Math.Min(100, downloaded * 100.0 / total);
        DownloadProgress.Value = percent;
        LocalModelStatus.Text =
            $"Downloading... {FormatBytes(downloaded)} / {FormatBytes(total)} ({percent:F0}%)";
    }

    /// Live progress for the Whisper/STT model download (dimmy_download_model).
    private void OnSttModelProgress(string filename, long downloaded, long total)
    {
        DownloadProgress.Visibility = Visibility.Visible;
        if (total <= 0)
        {
            DownloadProgress.IsIndeterminate = true;
            LocalModelStatus.Text = $"Downloading {filename}… {FormatBytes(downloaded)} so far";
            return;
        }
        DownloadProgress.IsIndeterminate = false;
        double percent = Math.Min(100, downloaded * 100.0 / total);
        DownloadProgress.Value = percent;
        LocalModelStatus.Text =
            $"Downloading {filename}… {FormatBytes(downloaded)} / {FormatBytes(total)} ({percent:F0}%)";
    }

    /// Live progress for the local-LLM model download (dimmy_download_llm_model).
    private void OnLlmModelProgress(string filename, long downloaded, long total)
    {
        DownloadLlmProgress.Visibility = Visibility.Visible;
        if (total <= 0)
        {
            DownloadLlmProgress.IsIndeterminate = true;
            LocalLlmModelStatus.Text = $"Downloading {filename}… {FormatBytes(downloaded)} so far";
            return;
        }
        DownloadLlmProgress.IsIndeterminate = false;
        double percent = Math.Min(100, downloaded * 100.0 / total);
        DownloadLlmProgress.Value = percent;
        LocalLlmModelStatus.Text =
            $"Downloading {filename}… {FormatBytes(downloaded)} / {FormatBytes(total)} ({percent:F0}%)";
    }

    /// Re-attach the progress bar to a download already running from a
    /// previous Settings session (state lives on the singleton AppViewModel).
    private void RestoreActiveDownloads(App app)
    {
        var vm = app.AppViewModel;
        if (vm.SttModelDownloadActive)
        {
            DownloadModelBtn.IsEnabled = false;
            DownloadModelBtn.Content = "Downloading...";
            DownloadModelBtn.Visibility = Visibility.Visible;
            DownloadProgress.Visibility = Visibility.Visible;
            if (vm.SttModelDownloadPercent < 0)
            {
                DownloadProgress.IsIndeterminate = true;
            }
            else
            {
                DownloadProgress.IsIndeterminate = false;
                DownloadProgress.Value = vm.SttModelDownloadPercent;
            }
            if (!string.IsNullOrEmpty(vm.SttModelDownloadLabel))
                LocalModelStatus.Text = vm.SttModelDownloadLabel;
        }
        if (vm.LlmModelDownloadActive)
        {
            DownloadLlmModelBtn.IsEnabled = false;
            DownloadLlmModelBtn.Content = "Downloading...";
            DownloadLlmModelBtn.Visibility = Visibility.Visible;
            DownloadLlmProgress.Visibility = Visibility.Visible;
            if (vm.LlmModelDownloadPercent < 0)
            {
                DownloadLlmProgress.IsIndeterminate = true;
            }
            else
            {
                DownloadLlmProgress.IsIndeterminate = false;
                DownloadLlmProgress.Value = vm.LlmModelDownloadPercent;
            }
            if (!string.IsNullOrEmpty(vm.LlmModelDownloadLabel))
                LocalLlmModelStatus.Text = vm.LlmModelDownloadLabel;
        }
    }

    private static string FormatBytes(long bytes)
    {
        if (bytes >= 1024L * 1024L * 1024L)
            return $"{bytes / 1024.0 / 1024.0 / 1024.0:F2} GB";
        return $"{bytes / 1024.0 / 1024.0:F0} MB";
    }

    private void CheckModelStatus()
    {
        bool isParakeet = ViewModel.LocalSttBackend == "parakeet";
        try
        {
            int exists = isParakeet
                ? DimmyNative.dimmy_parakeet_bundle_present()
                : DimmyNative.dimmy_model_exists(ViewModel.LocalModel);
            if (exists == 1)
            {
                LocalModelStatus.Text = "Ready";
                DownloadModelBtn.Visibility = Visibility.Collapsed;
            }
            else
            {
                string sizeInfo;
                if (isParakeet)
                {
                    sizeInfo = " (2.5GB)";
                }
                else
                {
                    var model = _localModels.Find(m => m.Filename == ViewModel.LocalModel);
                    sizeInfo = model != null ? $" ({model.SizeMb}MB)" : "";
                }
                LocalModelStatus.Text = $"Not downloaded{sizeInfo}";
                DownloadModelBtn.Content = $"Download{sizeInfo}";
                DownloadModelBtn.IsEnabled = true;
                DownloadModelBtn.Visibility = Visibility.Visible;
            }
        }
        catch
        {
            LocalModelStatus.Text = "Unable to check";
            DownloadModelBtn.Visibility = Visibility.Visible;
        }
    }

    private async void DownloadModel_Click(object sender, RoutedEventArgs e)
    {
        bool isParakeet = ViewModel.LocalSttBackend == "parakeet";
        DownloadModelBtn.IsEnabled = false;
        DownloadModelBtn.Content = "Downloading...";
        DownloadProgress.IsIndeterminate = true;
        DownloadProgress.Value = 0;
        DownloadProgress.Visibility = Visibility.Visible;
        LocalModelStatus.Text = "Starting download...";

        try
        {
            int result = await Task.Run(() => isParakeet
                ? DimmyNative.dimmy_parakeet_download_bundle()
                : DimmyNative.dimmy_download_model(ViewModel.LocalModel));
            if (result == 0)
            {
                LocalModelStatus.Text = "Ready";
                DownloadModelBtn.Visibility = Visibility.Collapsed;
                // Refresh the ComboBox so the status suffix flips to (Ready).
                PopulateLocalModels();
            }
            else
            {
                LocalModelStatus.Text = "Download failed";
                DownloadModelBtn.Content = "Retry Download";
                DownloadModelBtn.IsEnabled = true;
            }
        }
        catch
        {
            LocalModelStatus.Text = "Download failed";
            DownloadModelBtn.Content = "Retry Download";
            DownloadModelBtn.IsEnabled = true;
        }
        finally
        {
            DownloadProgress.Visibility = Visibility.Collapsed;
            if (Application.Current is App doneApp)
                doneApp.AppViewModel.SttModelDownloadActive = false;
        }
    }

    // ── Local LLM mode ────────────────────────────────────────────

    private System.Collections.Generic.List<LocalModelInfo> _localLlmModels = new();

    private void SyncLlmMode()
    {
        bool isLocal = ViewModel.LlmMode == "local";
        LlmModeLocal.IsChecked = isLocal;
        LlmModeCloud.IsChecked = !isLocal;
        LocalLlmPanel.Visibility = isLocal ? Visibility.Visible : Visibility.Collapsed;
        CloudLlmPanel.Visibility = isLocal ? Visibility.Collapsed : Visibility.Visible;

        if (isLocal)
            CheckLlmModelStatus();
    }

    private void LlmMode_Checked(object sender, RoutedEventArgs e)
    {
        if (!_loaded) return;
        if (sender is RadioButton rb && rb.Tag is string tag)
        {
            ViewModel.LlmMode = tag;
            SyncLlmMode();
        }
    }

    private void PopulateLocalLlmModels()
    {
        try
        {
            var json = DimmyNative.ListLocalLlmModels();
            if (string.IsNullOrEmpty(json)) return;

            using var doc = JsonDocument.Parse(json);
            _localLlmModels.Clear();
            LocalLlmModelComboBox.Items.Clear();

            int selectedIdx = 0;
            int idx = 0;
            foreach (var el in doc.RootElement.EnumerateArray())
            {
                var name = el.GetProperty("name").GetString() ?? "";
                var filename = el.GetProperty("filename").GetString() ?? "";
                var sizeMb = el.GetProperty("size_mb").GetInt32();
                var desc = el.GetProperty("description").GetString() ?? "";
                var downloaded = el.GetProperty("downloaded").GetBoolean();

                _localLlmModels.Add(new LocalModelInfo(name, filename, sizeMb, desc, downloaded));

                var status = downloaded ? "Ready" : $"{sizeMb} MB";
                var item = new ComboBoxItem
                {
                    // Concise: name + state only. The long catalogue
                    // description is not shown in the picker.
                    Content = $"{name} ({status})",
                    Tag = filename
                };
                LocalLlmModelComboBox.Items.Add(item);

                if (filename == ViewModel.LocalLlmModel)
                    selectedIdx = idx;
                idx++;
            }

            if (LocalLlmModelComboBox.Items.Count > 0)
                LocalLlmModelComboBox.SelectedIndex = selectedIdx;
        }
        catch { }
    }

    private void LocalLlmModel_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!_loaded) return;
        if (LocalLlmModelComboBox.SelectedItem is ComboBoxItem item && item.Tag is string filename)
        {
            ViewModel.LocalLlmModel = filename;
            CheckLlmModelStatus();
        }
    }

    private void CheckLlmModelStatus()
    {
        try
        {
            int exists = DimmyNative.dimmy_llm_model_exists(ViewModel.LocalLlmModel);
            if (exists == 1)
            {
                LocalLlmModelStatus.Text = "Ready";
                DownloadLlmModelBtn.Visibility = Visibility.Collapsed;
            }
            else
            {
                var model = _localLlmModels.Find(m => m.Filename == ViewModel.LocalLlmModel);
                var sizeInfo = model != null ? $" ({model.SizeMb}MB)" : "";
                LocalLlmModelStatus.Text = $"Not downloaded{sizeInfo}";
                DownloadLlmModelBtn.Content = $"Download{sizeInfo}";
                DownloadLlmModelBtn.Visibility = Visibility.Visible;
            }
        }
        catch
        {
            LocalLlmModelStatus.Text = "Unable to check";
            DownloadLlmModelBtn.Visibility = Visibility.Visible;
        }
    }

    private async void DownloadLlmModel_Click(object sender, RoutedEventArgs e)
    {
        DownloadLlmModelBtn.IsEnabled = false;
        DownloadLlmModelBtn.Content = "Downloading...";
        DownloadLlmProgress.Visibility = Visibility.Visible;
        LocalLlmModelStatus.Text = "Downloading...";

        try
        {
            int result = await Task.Run(() => DimmyNative.dimmy_download_llm_model(ViewModel.LocalLlmModel));
            if (result == 0)
            {
                LocalLlmModelStatus.Text = "Ready";
                DownloadLlmModelBtn.Visibility = Visibility.Collapsed;
                PopulateLocalLlmModels();
            }
            else
            {
                LocalLlmModelStatus.Text = "Download failed";
                DownloadLlmModelBtn.Content = "Retry Download";
                DownloadLlmModelBtn.IsEnabled = true;
            }
        }
        catch
        {
            LocalLlmModelStatus.Text = "Download failed";
            DownloadLlmModelBtn.Content = "Retry Download";
            DownloadLlmModelBtn.IsEnabled = true;
        }
        finally
        {
            DownloadLlmProgress.Visibility = Visibility.Collapsed;
            if (Application.Current is App doneApp)
                doneApp.AppViewModel.LlmModelDownloadActive = false;
        }
    }

    private void Language_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (LanguageComboBox.SelectedItem is System.Collections.Generic.KeyValuePair<string, string> kvp)
            ViewModel.Language = kvp.Key;
    }

    private void Provider_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (sender is ComboBox cb && cb.SelectedItem is ComboBoxItem item && item.Tag is string tag)
        {
            var preset = SettingsViewModel.ProviderPresets.FirstOrDefault(p =>
                p.Name.ToLowerInvariant() == tag);

            var isCustom = preset == null || string.IsNullOrEmpty(preset.Url);
            if (!isCustom)
            {
                ViewModel.ApiUrl = preset!.Url;
                ViewModel.ApiModel = preset.DefaultModel;
                // Hide custom fields for known presets
                CustomUrlBox.Visibility = Visibility.Collapsed;
                CustomModelBox.Visibility = Visibility.Collapsed;
            }
            else
            {
                // "Custom" selected — show URL/model TextBoxes
                CustomUrlBox.Visibility = Visibility.Visible;
                CustomModelBox.Visibility = Visibility.Visible;
            }
            // The inline STT key field is for the Custom endpoint ONLY —
            // catalog providers are keyed from the Providers & keys page.
            if (SttApiKeyCard != null)
                SttApiKeyCard.Visibility = isCustom ? Visibility.Visible : Visibility.Collapsed;
            // Refresh the green-check badge for the newly-selected provider.
            // Without this, switching to a provider with a saved key still
            // showed "no key" until the user closed + reopened Settings.
            ViewModel.HasApiKey = LookupSttKeyForTag(tag);
            // ALSO refresh the LLM section visibility — the
            // "Use my saved API key" toggle and the LLM key input
            // depend on STT's HasApiKey + provider parity. Without
            // this call, switching STT to Gemini (while LLM was
            // already Gemini) wouldn't reveal the toggle until the
            // user touched the LLM dropdown too. Burned 2026-05-15.
            RefreshAuthIntegrationStatus();
        }
    }

    private void Nav_SelectionChanged(NavigationView sender, NavigationViewSelectionChangedEventArgs args)
    {
        if (args.SelectedItem is NavigationViewItem item && item.Tag is string tag)
        {
            _currentTag = tag;
            // V2 IA: home / voice / output / pill / rules / shortcut / privacy / about / advanced.
            // Legacy tags (general / overlay / debug / stats) accepted for back-compat with any
            // saved nav-state path elsewhere — they map to the v2 panels behind the scenes.
            HomePanel.Visibility = Visibility.Collapsed;
            ProvidersPanel.Visibility = Visibility.Collapsed;
            GeneralPanel.Visibility = Visibility.Collapsed;
            ShortcutPanel.Visibility = Visibility.Collapsed;
            OutputPanel.Visibility = Visibility.Collapsed;
            OverlayPanel.Visibility = Visibility.Collapsed;
            RulesPanel.Visibility = Visibility.Collapsed;
            HistoryPanel.Visibility = Visibility.Collapsed;
            IntegrationsPanel.Visibility = Visibility.Collapsed;
            AboutPanel.Visibility = Visibility.Collapsed;
            PrivacyPanel.Visibility = Visibility.Collapsed;
            LicensePanel.Visibility = Visibility.Collapsed;
            StatsPanel.Visibility = Visibility.Collapsed;
            DebugPanel.Visibility = Visibility.Collapsed;

            var panel = tag switch
            {
                "home" => HomePanel,
                "providers" => ProvidersPanel,
                "voice" or "general" => GeneralPanel,
                "output" => OutputPanel,
                "pill" or "overlay" => OverlayPanel,
                "rules" => RulesPanel,
                "history" => HistoryPanel,
                "integrations" => IntegrationsPanel,
                "shortcut" => ShortcutPanel,
                "privacy" => PrivacyPanel,
                "license" => LicensePanel,
                "about" => AboutPanel,
                "advanced" or "debug" => DebugPanel,
                "stats" => StatsPanel,
                _ => HomePanel,
            };
            panel.Visibility = Visibility.Visible;
            if (tag == "history") LoadHistoryItems();
            if (tag == "providers") EnsureProviderCards();

            if (tag == "privacy")
            {
                RefreshAnonymousIdText();
            }
            else if (tag == "license")
            {
                RefreshLicenseStatus();
            }
        }

        TintSelectedNavIcon(args.SelectedItem);
    }

    /// <summary>
    /// Win11 Settings pattern: only the *icon* of the selected nav-pane
    /// entry takes the system accent colour — the label keeps the default
    /// TextFillColorPrimary. Walks every NavigationViewItem on each
    /// selection change, sets `Foreground` on the FontIcon child when
    /// selected, clears it (back to inherit) otherwise. Cheap: 12 items.
    ///
    /// Uses ClearValue rather than `Foreground = null` for the reset: in
    /// WinUI a null assignment locks the DP at null instead of releasing
    /// it to the style / inherited fallback, so the icon would render
    /// invisible (or black on dark theme) once you ever navigated away
    /// from it.
    /// </summary>
    private void TintSelectedNavIcon(object? selected)
    {
        // ResolvedAccentBrush picks the right accent shade for the
        // Window's RESOLVED theme (UiPreferences override → system
        // fallback). Fetching `AccentFillColorDefaultBrush` from
        // Application.Resources gives the SYSTEM-theme variant only,
        // which is wrong when the user has flipped Dimmy's theme.
        var accent = Helpers.ThemeHelper.ResolvedAccentBrush();
        foreach (var entry in NavView.MenuItems)
        {
            if (entry is NavigationViewItem nvi && nvi.Icon is FontIcon icon)
            {
                if (ReferenceEquals(entry, selected))
                    icon.Foreground = accent;
                else
                    icon.ClearValue(IconElement.ForegroundProperty);
            }
        }
    }

    // ── License ────────────────────────────────────────────────────────

    private void RefreshLicenseStatus()
    {
        try
        {
            var s = LicenseService.GetStatus();
            (string head, string detail) = s.Kind switch
            {
                "Unrestricted" => ("Source build (no licensing)",
                                    "This binary was built without a licensing public key. All features are unlocked."),
                "NotFound"     => ("No license on this device",
                                    "Start a trial below or paste an activation code from your email."),
                "TrialActive"  => ($"Trial: {s.DaysRemaining} day(s) left",
                                    "You're in your free 14-day trial. Cloud + auto-update are enabled."),
                "TrialExpired" => ("Trial expired",
                                    "Your trial has ended. Cloud features are paused. Purchase a license to continue."),
                "Active"       => ($"Active: {s.Tier} ({s.DaysRemaining} day(s) left)",
                                    s.CancelsAt is long ca
                                        ? $"Subscription scheduled to cancel on {DateTimeOffset.FromUnixTimeSeconds(ca).LocalDateTime:MMM d, yyyy}. You keep cloud features until then."
                                        : "Thanks for supporting Dimmy. All cloud features are enabled."),
                "Expired"      => ("License expired",
                                    "Renew to re-enable cloud features."),
                "Suspended"    => ($"Suspended: offline {s.DaysOffline} day(s)",
                                    "Reconnect this device to refresh your license."),
                "Invalid"      => ("License file invalid",
                                    s.Error ?? "Re-activate this device."),
                _              => (s.Kind, s.Error ?? string.Empty),
            };
            LicenseStatusHeadline.Text = head;
            LicenseStatusDetail.Text = detail;

            // Tier pill + trailing days + tinted border — mirrors the
            // statusHero design from macOS MacLicensePage.swift. Drives
            // four signals at a glance: state (color), tier (badge text),
            // remaining time (trailing number), category (border tint).
            ApplyLicenseHero(s);

            // Stripe Customer Portal button — only meaningful for paid
            // licenses. Trials and source-build have no Stripe billing
            // attached. Lifetime DOES — the portal still surfaces
            // invoices and lets the user update their payment method
            // for future purchases on the same Stripe customer.
            LicenseManageSubButton.Visibility =
                s.Kind == "Active" && s.Tier is "monthly" or "annual" or "lifetime"
                    ? Visibility.Visible
                    : Visibility.Collapsed;
            ApplyBuyCardForStatus(s);
            PopulateScopeGrid(s);
            // Devices are server-side data — refresh asynchronously, only when
            // we actually have a license to query against.
            if (s.Kind is "TrialActive" or "Active" or "Suspended")
                _ = RefreshDevicesAsync();
            else
            {
                LicenseDevicesList.Children.Clear();
                LicenseDeviceCountLabel.Text = string.Empty;
            }
        }
        catch (Exception ex)
        {
            LicenseStatusHeadline.Text = "Status check failed";
            LicenseStatusDetail.Text = ex.Message;
        }
    }

    /// <summary>
    /// Compute and render the "status hero" decorations: tier pill text +
    /// color, trailing days counter, tinted border around the whole card.
    /// Mirrors macOS MacLicensePage statusHero so both platforms feel
    /// identical at a glance.
    /// </summary>
    private void ApplyLicenseHero(LicenseService.Status s)
    {
        // ── Tint per state ─────────────────────────────────────────────
        // Color is reserved for *positive* state. TrialActive=orange
        // ("act soon"), Active=green ("you're paid"), Unrestricted=purple
        // ("dev"). Everything else (NotFound, TrialExpired, Expired,
        // Invalid, Suspended) collapses to neutral gray — a license
        // problem isn't an error to alarm about, it's just a state.
        // Red was reading as "something is broken", wrong vibe for a
        // user who simply hasn't activated yet.
        global::Windows.UI.Color tint = s.Kind switch
        {
            "TrialActive"  => global::Windows.UI.Color.FromArgb(0xFF, 0xFF, 0x9F, 0x0A),
            "Active"       => global::Windows.UI.Color.FromArgb(0xFF, 0x34, 0xC7, 0x59),
            "Unrestricted" => global::Windows.UI.Color.FromArgb(0xFF, 0x9C, 0x5B, 0xFF),
            _              => global::Windows.UI.Color.FromArgb(0xFF, 0x90, 0x90, 0x99),
        };

        // ── Badge text ─────────────────────────────────────────────────
        // Active branches by tier so a paying user sees the SKU, not just
        // a generic "PRO". Trial / Suspended / Expired collapse to the
        // state name.
        string badge = s.Kind switch
        {
            "Unrestricted" => "DEV",
            "TrialActive" or "TrialExpired" => "TRIAL",
            "Active" => s.Tier switch
            {
                "monthly"  => "PRO • MONTHLY",
                "annual"   => "PRO • ANNUAL",
                "lifetime" => "PRO • LIFETIME",
                _          => "PRO",
            },
            "Expired"   => "EXPIRED",
            "Suspended" => "SUSPENDED",
            "Invalid"   => "INVALID",
            "NotFound"  => "INACTIVE",
            _           => s.Kind?.ToUpperInvariant() ?? "INACTIVE",
        };
        LicenseTierBadge.Text = badge;
        // Win11 Fluent badge: 14% tinted fill, 35% tinted stroke, matching
        // tint-colored text. Subtler than the prior solid fill + white
        // text, matches the visual weight of native InfoBadge / accent
        // pills in Settings / Edge / Store.
        var badgeFill = tint;   badgeFill.A   = 0x24; // ~14%
        var badgeStroke = tint; badgeStroke.A = 0x59; // ~35%
        LicenseTierBadgeBorder.Background  = new Microsoft.UI.Xaml.Media.SolidColorBrush(badgeFill);
        LicenseTierBadgeBorder.BorderBrush = new Microsoft.UI.Xaml.Media.SolidColorBrush(badgeStroke);
        LicenseTierBadge.Foreground        = new Microsoft.UI.Xaml.Media.SolidColorBrush(tint);
        LicenseTierBadgeBorder.Visibility = Visibility.Visible;

        // ── Hero card ─────────────────────────────────────────────────
        // Fluent prefers theme-aware surfaces over saturated colored
        // borders. We use a faint 5% tint background and the standard
        // ControlStrokeColorDefaultBrush for the border — the colored
        // accent stays in the badge + trailing days counter.
        var heroFill = tint; heroFill.A = 0x0D; // ~5%
        LicenseStatusBorder.Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(heroFill);
        LicenseStatusBorder.BorderBrush =
            (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["ControlStrokeColorDefaultBrush"];

        // ── Trailing big-number ───────────────────────────────────────
        // TrialActive / Active → days remaining. Suspended → days offline.
        // Everything else → no trailing column.
        if (s.Kind is "TrialActive" or "Active" && s.DaysRemaining is long d)
        {
            LicenseTrailingValue.Text = d.ToString();
            LicenseTrailingLabel.Text = d == 1 ? "DAY LEFT" : "DAYS LEFT";
            LicenseTrailingPanel.Visibility = Visibility.Visible;
        }
        else if (s.Kind == "Suspended" && s.DaysOffline is int o)
        {
            LicenseTrailingValue.Text = o.ToString();
            LicenseTrailingLabel.Text = o == 1 ? "DAY OFFLINE" : "DAYS OFFLINE";
            LicenseTrailingPanel.Visibility = Visibility.Visible;
        }
        else
        {
            LicenseTrailingPanel.Visibility = Visibility.Collapsed;
        }
    }

    private void PopulateScopeGrid(LicenseService.Status s)
    {
        LicenseScopeGrid.Children.Clear();
        var active = new System.Collections.Generic.HashSet<string>(s.Scopes,
            StringComparer.OrdinalIgnoreCase);
        foreach (var (key, display, descr) in LicenseService.ScopeNames.All)
        {
            bool granted = active.Contains(key);

            // One row per capability. Three columns: glyph, name + description, status.
            var row = new Grid { ColumnSpacing = 12 };
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(28) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

            var glyph = new FontIcon
            {
                Glyph = granted ? "\uE73E" : "\uE711", // CheckMark vs Cancel
                FontSize = 16,
                // System accent brushes are theme-aware AND high-contrast in
                // both modes. Secondary at FontSize 16 was still rendering
                // as ghost-faint on the LayerOnAcrylic light card. Critical
                // (red) carries "missing/locked" without a wall of text.
                Foreground = granted
                    ? (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["SystemFillColorSuccessBrush"]
                    : (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["SystemFillColorCriticalBrush"],
                VerticalAlignment = VerticalAlignment.Top,
                Margin = new Thickness(0, 2, 0, 0),
            };
            Grid.SetColumn(glyph, 0);

            var labelStack = new StackPanel { Spacing = 1 };
            labelStack.Children.Add(new TextBlock
            {
                Text = display,
                FontSize = 13,
                FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            });
            labelStack.Children.Add(new TextBlock
            {
                Text = descr,
                FontSize = 12,
                // Per-row description was 11/Secondary which renders as
                // ghost on the LayerOnAcrylic light card. Bump to 12 +
                // Primary so it actually reads in light mode.
                Foreground = (Microsoft.UI.Xaml.Media.Brush)
                    Application.Current.Resources["TextFillColorPrimaryBrush"],
                Opacity = 0.85,
                TextWrapping = TextWrapping.Wrap,
            });
            Grid.SetColumn(labelStack, 1);

            var statusText = new TextBlock
            {
                Text = granted ? "Included" : "Not included",
                FontSize = 11,
                FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
                // Match the glyph: green Success for Included, red Critical
                // for missing. Plain Secondary was invisible in light theme.
                Foreground = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources[
                    granted ? "SystemFillColorSuccessBrush" : "SystemFillColorCriticalBrush"],
                VerticalAlignment = VerticalAlignment.Top,
                Margin = new Thickness(0, 4, 0, 0),
            };
            Grid.SetColumn(statusText, 2);

            row.Children.Add(glyph);
            row.Children.Add(labelStack);
            row.Children.Add(statusText);
            LicenseScopeGrid.Children.Add(row);
        }
    }

    private async void License_StartTrial_Click(object sender, RoutedEventArgs e)
    {
        var email = (LicenseTrialEmailBox.Text ?? string.Empty).Trim();
        if (string.IsNullOrEmpty(email) || !email.Contains('@'))
        {
            ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Error, "Enter a valid email address.");
            return;
        }
        LicenseTrialButton.IsEnabled = false;
        try
        {
            ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Informational, "Requesting magic link…");
            var r = await LicenseService.RequestTrialAsync(email);
            if (!r.Ok)
            {
                ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Error, r.Error ?? "Request failed.");
                return;
            }

            // Production: server emails the magic link, UI shows "check your inbox".
            // Dev (local server): server returns the link directly — we open it via the
            // OS to exercise the same dimmy:// path the email click would trigger.
            var link = r.MagicLink;
            if (string.IsNullOrEmpty(link))
            {
                ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Success,
                    "Check your email. Click the magic link from the device you want to activate.");
                return;
            }

            if (link.StartsWith("dimmy://", StringComparison.OrdinalIgnoreCase))
            {
                ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Informational,
                    "Activating via magic link…");
                System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(link)
                {
                    UseShellExecute = true,
                });

                // Poll status — the URL-scheme dispatch hands the code to the
                // running Dimmy via named pipe; redeem completes asynchronously.
                if (await WaitForActivationAsync(TimeSpan.FromSeconds(8)))
                {
                    RefreshLicenseStatus();
                    ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Success, "Activated. Welcome to Dimmy.");
                }
                else
                {
                    var fallback = r.Code ?? ExtractCode(link) ?? string.Empty;
                    ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Warning,
                        "Auto-activation didn't complete. Open the fallback below and paste this code: " + fallback);
                }
            }
            else
            {
                // Production case — server returned an HTTPS link (e.g. license.dimmy.app/m/...).
                // Email is the canonical delivery; just confirm.
                ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Success,
                    "Magic link sent to " + email + ". Click it from this device to activate.");
            }
        }
        catch (Exception ex)
        {
            ShowInfoBar(LicenseTrialInfoBar, InfoBarSeverity.Error, ex.Message);
        }
        finally
        {
            LicenseTrialButton.IsEnabled = true;
        }
    }

    private void OnLicenseChangedExternal()
    {
        // Always refresh, regardless of which tab is currently visible.
        // Pre-fix the check was `_currentTag == "license"` — that
        // skipped the refresh whenever the dimmy:// activate flow fired
        // NotifyChanged BEFORE NavigateToTag had set the tag to
        // "license" (the order in App.xaml.cs HandleForwardedCommand:
        // NotifyChanged → OpenSettingsWindowAt → NavigateToTag). The
        // user landed on the License page with stale "NotFound" data
        // and concluded activation hadn't worked.
        // RefreshLicenseStatus is cheap (one FFI call + a few bindings)
        // so always-running it on any LicenseChanged notification is
        // correct and idempotent.
        DispatcherQueue.TryEnqueue(RefreshLicenseStatus);
    }

    /// Navigate the SettingsWindow's NavigationView to the given tag.
    /// Called by App.xaml.cs after a successful URL-scheme activation so
    /// the user lands on the License page with the post-redeem status.
    /// When navigating to "license" we also force-refresh the status
    /// view so the badge reflects whatever the most recent activate /
    /// refresh wrote to disk — even if the LicenseChanged event raced
    /// past the page before this navigation completed.
    public void NavigateToTag(string tag)
    {
        DispatcherQueue.TryEnqueue(() =>
        {
            foreach (var item in NavView.MenuItems)
            {
                if (item is NavigationViewItem nv && nv.Tag is string t && t == tag)
                {
                    NavView.SelectedItem = nv;
                    if (tag == "license")
                    {
                        try { RefreshLicenseStatus(); }
                        catch { /* page bindings might not be ready yet */ }
                    }
                    return;
                }
            }
        });
    }

    /// Poll dimmy_license_status until kind transitions into TrialActive/Active,
    /// or timeout. Used right after triggering a dimmy:// magic link to surface
    /// the activation result inline instead of forcing the user to click Refresh.
    private static async Task<bool> WaitForActivationAsync(TimeSpan budget)
    {
        var start = DateTime.UtcNow;
        while (DateTime.UtcNow - start < budget)
        {
            await Task.Delay(400);
            var s = LicenseService.GetStatus();
            if (s.Kind == "TrialActive" || s.Kind == "Active") return true;
        }
        return false;
    }

    private async void License_Activate_Click(object sender, RoutedEventArgs e)
    {
        var raw = (LicenseCodeBox.Text ?? string.Empty).Trim();
        if (string.IsNullOrEmpty(raw))
        {
            ShowInfoBar(LicenseActivateInfoBar, InfoBarSeverity.Error, "Paste a code or magic-link URL.");
            return;
        }
        var code = ExtractCode(raw) ?? raw;
        var label = (LicenseDeviceLabelBox.Text ?? string.Empty).Trim();
        if (string.IsNullOrEmpty(label)) label = Environment.MachineName;
        try
        {
            ShowInfoBar(LicenseActivateInfoBar, InfoBarSeverity.Informational, "Activating…");
            var r = await LicenseService.RedeemAsync(code, label);
            if (r.Ok)
            {
                ShowInfoBar(LicenseActivateInfoBar, InfoBarSeverity.Success, "Activated. Welcome to Dimmy.");
                RefreshLicenseStatus();
            }
            else
            {
                ShowInfoBar(LicenseActivateInfoBar, InfoBarSeverity.Error, r.Error ?? "Activation failed.");
            }
        }
        catch (Exception ex)
        {
            ShowInfoBar(LicenseActivateInfoBar, InfoBarSeverity.Error, ex.Message);
        }
    }

    private async void License_Refresh_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var r = await LicenseService.RefreshAsync();
            // Status either way — the user wants to see the result.
            RefreshLicenseStatus();
            if (!r.Ok)
                ShowInfoBar(LicenseActivateInfoBar, InfoBarSeverity.Warning, r.Error ?? "Refresh failed.");
        }
        catch (Exception ex)
        {
            ShowInfoBar(LicenseActivateInfoBar, InfoBarSeverity.Error, ex.Message);
        }
    }

    private async void License_Clear_Click(object sender, RoutedEventArgs e)
    {
        // Confirm dialog with the message users keep missing: signing out
        // removes the license from THIS device only — the Stripe sub
        // stays alive and keeps billing. To stop billing, use Manage
        // subscription. Without this dialog, users sign out then click
        // Buy expecting to "start over" and end up with a duplicate
        // charge that the server-side gate has to refund.
        var dlg = new ContentDialog
        {
            Title = "Sign out from this device?",
            Content =
                "Your subscription on Stripe will stay active and will keep " +
                "billing on its renewal date. To cancel billing, use 'Manage subscription' instead.\n\n" +
                "If you sign out, your activation token on this device is removed. " +
                "You can sign in again from the same email; we'll resend the magic link.",
            PrimaryButtonText = "Sign out",
            SecondaryButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Secondary,
            XamlRoot = this.Content?.XamlRoot,
        };
        if ((await dlg.ShowAsync()) != ContentDialogResult.Primary) return;
        LicenseService.Clear();
        RefreshLicenseStatus();
    }

    /// <summary>
    /// "Manage subscription" — POST /api/billing-portal with the on-disk
    /// token, open the returned Stripe Customer Portal URL in the
    /// system browser. Read the token directly from the license file
    /// rather than going through FFI: this is a one-shot operation
    /// (no need for the Rust HTTP wrapper) and keeps the rebuild
    /// surface to a UI-only change.
    /// </summary>
    private async void License_ManageSubscription_Click(object sender, RoutedEventArgs e)
    {
        // Goes through the licensing FFI so the call hits whichever
        // server URL was embedded at build time via
        // DIMMY_LICENSE_SERVER_URL (staging → license-staging.dimmy.app,
        // prod → license.dimmy.app, debug → localhost mock). The
        // runtime override that used to live behind a Settings text
        // box has been removed for safety.
        LicenseManageSubButton.IsEnabled = false;
        try
        {
            var r = await LicenseService.BillingPortalUrlAsync();
            if (!r.Ok || string.IsNullOrEmpty(r.Url))
            {
                ShowInfoBar(LicenseManageSubInfoBar, InfoBarSeverity.Error,
                    r.Error ?? "Portal response missing URL.");
                return;
            }
            await global::Windows.System.Launcher.LaunchUriAsync(new Uri(r.Url));
        }
        catch (Exception ex)
        {
            ShowInfoBar(LicenseManageSubInfoBar, InfoBarSeverity.Error,
                $"Cannot open portal: {ex.Message}");
        }
        finally
        {
            LicenseManageSubButton.IsEnabled = true;
        }
    }

    private async void License_DevicesReload_Click(object sender, RoutedEventArgs e)
    {
        await RefreshDevicesAsync();
    }

    /// Show / hide + relabel the Buy card and individual tier buttons
    /// based on the current status. Tier-aware: hide buttons at-or-below
    /// the active tier so an Active{Monthly} user only sees Annual +
    /// Lifetime as upgrade options, an Active{Annual} user only sees
    /// Lifetime, and an Active{Lifetime} user sees no Buy at all
    /// (lifetime is the ceiling). Trial → all three (legitimate
    /// trial→paid). NotFound / TrialExpired / Expired / Suspended → all
    /// three (first purchase or repurchase). Unrestricted / Invalid →
    /// the whole card stays hidden.
    ///
    /// Plan-change for active users (downgrade, cancel) goes through
    /// the Stripe Customer Portal, NOT through these Buy buttons —
    /// the portal hint TextBlock surfaces this when relevant so the
    /// user doesn't accidentally start a duplicate subscription.
    private void ApplyBuyCardForStatus(LicenseService.Status s)
    {
        string headline = string.Empty;
        string detail = string.Empty;
        bool show = false;
        bool showMonthly = true, showAnnual = true, showLifetime = true;
        string monthlyLabel = "Monthly";
        string annualLabel = "Annual";
        string lifetimeLabel = "Lifetime";
        bool showPortalHint = false;

        switch (s.Kind)
        {
            case "NotFound":
                headline = "Buy a license";
                detail = "Pick a plan and Stripe will email you a magic link to activate immediately. No trial required.";
                show = true;
                break;
            case "TrialActive":
                headline = "Upgrade to Pro";
                detail = "Skip the trial and unlock cloud features without interruption.";
                show = true;
                break;
            case "TrialExpired":
                headline = "Trial ended. Buy to continue.";
                detail = "Cloud features are paused. Pick a plan to re-activate. Local + BYOK keep working free either way.";
                show = true;
                break;
            case "Expired":
                headline = "Renew your license";
                detail = "Cloud features are paused. Pick a plan to re-activate.";
                show = true;
                break;
            case "Suspended":
                headline = "Resume your license";
                detail = "Pick a plan to restore cloud features.";
                show = true;
                break;
            case "Active":
                switch (s.Tier?.ToLowerInvariant())
                {
                    case "monthly":
                        headline = "Upgrade your plan";
                        detail = "Switch to Annual for the best value, or Lifetime to skip renewals entirely.";
                        show = true;
                        showMonthly = false;
                        annualLabel = "Switch to Annual";
                        lifetimeLabel = "Upgrade to Lifetime";
                        showPortalHint = true;
                        break;
                    case "annual":
                        headline = "Change your plan";
                        detail = "Drop to Monthly (you'll get a credit on the next invoice) or jump to Lifetime for one final payment.";
                        show = true;
                        showMonthly = true;          // ← reveal Switch to Monthly
                        showAnnual = false;
                        monthlyLabel = "Switch to Monthly";
                        lifetimeLabel = "Upgrade to Lifetime";
                        showPortalHint = true;
                        break;
                    case "lifetime":
                        // Ceiling tier — nothing above it.
                        show = false;
                        break;
                    default:
                        // Unknown tier (future-proof): stay hidden,
                        // operator decides via portal.
                        show = false;
                        break;
                }
                break;
            // Unrestricted, Invalid, anything else → hidden.
        }

        LicenseBuyCard.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
        if (!show) return;

        LicenseBuyHeadline.Text = headline;
        LicenseBuyDetail.Text = detail;
        LicenseBuyMonthlyButton.Visibility = showMonthly ? Visibility.Visible : Visibility.Collapsed;
        LicenseBuyAnnualButton.Visibility = showAnnual ? Visibility.Visible : Visibility.Collapsed;
        LicenseBuyLifetimeButton.Visibility = showLifetime ? Visibility.Visible : Visibility.Collapsed;
        // Collapse the column too so a hidden button doesn't leave a
        // gap in the row layout.
        LicenseBuyButtonsGrid.ColumnDefinitions[0].Width =
            showMonthly ? new GridLength(1, GridUnitType.Star) : new GridLength(0);
        LicenseBuyButtonsGrid.ColumnDefinitions[1].Width =
            showAnnual ? new GridLength(1, GridUnitType.Star) : new GridLength(0);
        LicenseBuyButtonsGrid.ColumnDefinitions[2].Width =
            showLifetime ? new GridLength(1, GridUnitType.Star) : new GridLength(0);
        LicenseBuyMonthlyLabel.Text = monthlyLabel;
        LicenseBuyAnnualLabel.Text = annualLabel;
        LicenseBuyLifetimeLabel.Text = lifetimeLabel;
        LicenseBuyPortalHint.Visibility = showPortalHint ? Visibility.Visible : Visibility.Collapsed;
    }

    private void License_BuyMonthly_Click(object sender, RoutedEventArgs e) => _ = BuyTierAsync("monthly");
    private void License_BuyAnnual_Click(object sender, RoutedEventArgs e)  => _ = BuyTierAsync("annual");
    private void License_BuyLifetime_Click(object sender, RoutedEventArgs e) => _ = BuyTierAsync("lifetime");

    private static string ToTitleCase(string s) =>
        string.IsNullOrEmpty(s) ? s : char.ToUpperInvariant(s[0]) + s.Substring(1);

    /// <summary>
    /// Modal asking the user for the email tied to their Stripe purchase.
    /// Used by the pre-checkout gate to look up an existing license server-side
    /// before spinning up a Checkout session — closes the post-sign-out
    /// double-charge edge case. Returns the trimmed lowercase email on
    /// confirm, or null on cancel.
    /// </summary>
    private async Task<string?> PromptBuyerEmailAsync(string prefilled, string tier)
    {
        var emailBox = new TextBox
        {
            PlaceholderText = "you@example.com",
            Text = prefilled ?? string.Empty,
            MinWidth = 280,
        };
        var helpText = new TextBlock
        {
            Text =
                "Used to look up an existing license + match the Stripe customer. " +
                "If you've bought before with this email, we'll resend the magic link " +
                "instead of charging you again.",
            FontSize = 12,
            TextWrapping = TextWrapping.Wrap,
            Foreground = (Microsoft.UI.Xaml.Media.Brush)
                Application.Current.Resources["TextFillColorSecondaryBrush"],
            Margin = new Thickness(0, 8, 0, 0),
        };
        var stack = new StackPanel { Spacing = 4 };
        stack.Children.Add(emailBox);
        stack.Children.Add(helpText);

        var dlg = new ContentDialog
        {
            Title = $"Continue to {ToTitleCase(tier)} checkout",
            Content = stack,
            PrimaryButtonText = "Continue",
            SecondaryButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            XamlRoot = this.Content?.XamlRoot,
        };
        // Disable Continue until the input looks like an email.
        dlg.IsPrimaryButtonEnabled = LooksLikeEmail(emailBox.Text);
        emailBox.TextChanged += (_, _) =>
            dlg.IsPrimaryButtonEnabled = LooksLikeEmail(emailBox.Text);

        var result = await dlg.ShowAsync();
        if (result != ContentDialogResult.Primary) return null;
        var trimmed = (emailBox.Text ?? string.Empty).Trim().ToLowerInvariant();
        return LooksLikeEmail(trimmed) ? trimmed : null;
    }

    private static bool LooksLikeEmail(string s) =>
        !string.IsNullOrWhiteSpace(s)
        && s.Contains('@')
        && s.Contains('.')
        && s.Length < 254;

    private async Task BuyTierAsync(string tier)
    {
        // Distinguish "plan change" (Active monthly⇄annual) from "first
        // purchase / lifetime upgrade". Plan change goes through
        // /api/plan-change (subscription update + proration) so the user
        // is NOT charged a fresh full-price invoice on top of their
        // existing sub. Anything else (first purchase, trial→paid,
        // sub→lifetime, expired/suspended renew) goes through Stripe
        // Checkout as before.
        try
        {
            DisableBuyButtons(true);
            var status = LicenseService.GetStatus();
            bool isPlanChange =
                status.Kind == "Active" &&
                (status.Tier == "monthly" || status.Tier == "annual") &&
                (tier == "monthly" || tier == "annual");

            if (isPlanChange)
            {
                // Confirm dialog so the user knows the click mutates an
                // existing sub (proration on next invoice, no second
                // card prompt) rather than opening a fresh Checkout.
                // Without it, the click 'flagga istantaneamente' the
                // new tier and the silent UX feels off.
                var confirmDialog = new ContentDialog
                {
                    Title = $"Switch plan to {ToTitleCase(tier)}?",
                    Content =
                        $"You're already subscribed (current: {ToTitleCase(status.Tier ?? "")}). " +
                        $"Switching to {tier} mutates your existing subscription:\n\n" +
                        "• No new payment now. Stripe reuses your saved card.\n" +
                        "• Stripe issues a prorated invoice on the next billing date " +
                        "(credit for unused days of the old plan, debit for the new one).\n" +
                        "• No magic-link email; your license stays active, just the tier changes.",
                    PrimaryButtonText = $"Switch to {ToTitleCase(tier)}",
                    SecondaryButtonText = "Cancel",
                    DefaultButton = ContentDialogButton.Primary,
                    XamlRoot = this.Content?.XamlRoot,
                };
                var result = await confirmDialog.ShowAsync();
                if (result != ContentDialogResult.Primary)
                {
                    ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Informational,
                        "Plan change cancelled.");
                    return;
                }
                ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Informational,
                    $"Switching plan to {tier}…");
                var r = await LicenseService.PlanChangeAsync(tier);
                if (!r.Ok)
                {
                    ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Error,
                        r.Error ?? "Plan change failed.");
                    return;
                }
                // Stripe webhook will fire customer.subscription.updated;
                // give it a beat, then refresh + re-render UI. The
                // refresh-and-render loop also renames the badge.
                await Task.Delay(1500);
                var refresh = await LicenseService.RefreshAsync();
                RefreshLicenseStatus();
                if (refresh.Ok)
                {
                    ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Success,
                        $"Plan switched to {tier}. Stripe will issue a prorated invoice automatically.");
                }
                else
                {
                    ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Informational,
                        $"Plan switched to {tier}. Refresh in a moment to see the new badge.");
                }
                return;
            }

            // Pre-checkout email gate: ask the user for their email
            // before minting the Stripe Checkout URL. The server uses
            // the email to look up an existing license and 409 BEFORE
            // Stripe charges the card. Pre-fill with whatever we have
            // saved in UiPreferences (survives Sign out — no auth, just
            // UX convenience).
            var prefs = Services.UiPreferences.Load();
            var promptedEmail = await PromptBuyerEmailAsync(
                prefilled: prefs.BuyerEmail ?? string.Empty,
                tier: tier);
            if (promptedEmail is null)
            {
                ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Informational,
                    "Purchase cancelled.");
                return;
            }
            // Persist for next time.
            prefs.BuyerEmail = promptedEmail;
            prefs.Save();

            ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Informational, $"Opening Stripe checkout for {tier}…");
            var c = await LicenseService.CreateCheckoutAsync(tier, promptedEmail);
            if (!c.Ok || string.IsNullOrEmpty(c.Url))
            {
                // 409 path → license already exists for this email. Offer
                // 'Send magic link instead' fallback (re-issues activation
                // email for the existing license via /api/trial/start).
                if (c.StatusCode == 409 && !string.IsNullOrEmpty(c.CurrentTier))
                {
                    var dlg = new ContentDialog
                    {
                        Title = $"You already have a {c.CurrentTier} license",
                        Content =
                            $"The email {promptedEmail} is already linked to an active {c.CurrentTier} license. " +
                            "We can resend the activation magic link for that license. " +
                            "no new payment, no second sub.",
                        PrimaryButtonText = "Send magic link",
                        SecondaryButtonText = "Cancel",
                        DefaultButton = ContentDialogButton.Primary,
                        XamlRoot = this.Content?.XamlRoot,
                    };
                    if ((await dlg.ShowAsync()) == ContentDialogResult.Primary)
                    {
                        var t = await LicenseService.RequestTrialAsync(promptedEmail);
                        ShowInfoBar(LicenseBuyInfoBar,
                            t.Ok ? InfoBarSeverity.Success : InfoBarSeverity.Error,
                            t.Ok
                                ? $"Magic link sent to {promptedEmail}. Check your inbox to activate."
                                : (t.Error ?? "Could not resend magic link."));
                    }
                    else
                    {
                        ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Informational,
                            "Cancelled.");
                    }
                    return;
                }
                ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Error, c.Error ?? "Could not start checkout.");
                return;
            }
            await global::Windows.System.Launcher.LaunchUriAsync(new Uri(c.Url));
            ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Success,
                "Checkout opened in your browser. After payment, check your email for the magic link to activate.");
        }
        catch (Exception ex)
        {
            ShowInfoBar(LicenseBuyInfoBar, InfoBarSeverity.Error, ex.Message);
        }
        finally
        {
            DisableBuyButtons(false);
        }
    }

    private void DisableBuyButtons(bool disabled)
    {
        LicenseBuyMonthlyButton.IsEnabled = !disabled;
        LicenseBuyAnnualButton.IsEnabled = !disabled;
        LicenseBuyLifetimeButton.IsEnabled = !disabled;
    }

    private async Task RefreshDevicesAsync()
    {
        try
        {
            LicenseDeviceCountLabel.Text = "Loading…";
            var list = await LicenseService.ListDevicesAsync();
            if (!list.Ok)
            {
                LicenseDeviceCountLabel.Text = list.Error ?? "Failed to fetch devices";
                LicenseDevicesList.Children.Clear();
                return;
            }
            int activeCount = list.Devices.Count(d => d.Status == "active");
            LicenseDeviceCountLabel.Text =
                $"{activeCount} active / {list.MaxDevices} max";
            LicenseDevicesList.Children.Clear();
            foreach (var d in list.Devices)
            {
                LicenseDevicesList.Children.Add(BuildDeviceRow(d));
            }
        }
        catch (Exception ex)
        {
            LicenseDeviceCountLabel.Text = ex.Message;
        }
    }

    private FrameworkElement BuildDeviceRow(LicenseService.DeviceInfo d)
    {
        bool active = d.Status == "active";
        var row = new Border
        {
            Padding = new Thickness(12, 8, 12, 8),
            CornerRadius = new CornerRadius(4),
            Background = (Microsoft.UI.Xaml.Media.Brush)
                Application.Current.Resources["SubtleFillColorSecondaryBrush"],
        };
        var grid = new Grid { ColumnSpacing = 12 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var labelStack = new StackPanel { Spacing = 1 };
        var nameRun = new TextBlock
        {
            Text = string.IsNullOrEmpty(d.Label) ? "(unnamed device)" : d.Label,
            FontSize = 13,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
        };
        if (d.IsSelf)
        {
            nameRun.Inlines.Add(new Microsoft.UI.Xaml.Documents.Run
            {
                Text = "  · this device",
                FontWeight = Microsoft.UI.Text.FontWeights.Normal,
                Foreground = (Microsoft.UI.Xaml.Media.Brush)
                    Application.Current.Resources["TextFillColorTertiaryBrush"],
            });
        }
        labelStack.Children.Add(nameRun);
        labelStack.Children.Add(new TextBlock
        {
            Text = active
                ? $"Last seen: {DateTimeOffset.FromUnixTimeSeconds(d.LastSeen).LocalDateTime:g}"
                : $"Status: {d.Status}",
            FontSize = 11,
            Foreground = (Microsoft.UI.Xaml.Media.Brush)
                Application.Current.Resources["TextFillColorSecondaryBrush"],
        });
        Grid.SetColumn(labelStack, 0);

        var btn = new Button
        {
            Content = d.IsSelf ? "Sign out this device" : "Sign out",
            IsEnabled = active,
            VerticalAlignment = VerticalAlignment.Center,
        };
        btn.Click += async (_, _) => await DeactivateDeviceAsync(d);
        Grid.SetColumn(btn, 1);

        grid.Children.Add(labelStack);
        grid.Children.Add(btn);
        row.Child = grid;
        return row;
    }

    private async Task DeactivateDeviceAsync(LicenseService.DeviceInfo d)
    {
        try
        {
            var r = await LicenseService.DeactivateDeviceAsync(d.IsSelf ? null : d.DeviceId);
            if (!r.Ok)
            {
                LicenseDeviceCountLabel.Text = r.Error ?? "Deactivation failed";
                return;
            }
            // Self-sign-out clears the local license file; refresh status will
            // flip the page back to NotFound. Otherwise just reload the list.
            RefreshLicenseStatus();
        }
        catch (Exception ex)
        {
            LicenseDeviceCountLabel.Text = ex.Message;
        }
    }

    private static string? ExtractCode(string? input)
    {
        if (string.IsNullOrEmpty(input)) return null;
        var idx = input.IndexOf("code=", StringComparison.OrdinalIgnoreCase);
        if (idx < 0) return input.Trim();
        var rest = input.Substring(idx + 5);
        var amp = rest.IndexOf('&');
        return amp >= 0 ? rest.Substring(0, amp) : rest;
    }

    private static void ShowInfoBar(InfoBar bar, InfoBarSeverity sev, string msg)
    {
        bar.Severity = sev;
        bar.Message = msg;
        bar.IsOpen = true;
    }

    /// <summary>
    /// Filter NavigationView items by user-typed query in the AutoSuggestBox.
    /// Hidden items are simply collapsed; the user clears the query to see
    /// everything again. Case-insensitive substring match on Content text.
    /// </summary>
    private void NavSearchBox_TextChanged(AutoSuggestBox sender, AutoSuggestBoxTextChangedEventArgs args)
    {
        var query = (sender.Text ?? string.Empty).Trim().ToLowerInvariant();
        foreach (var item in NavView.MenuItems)
        {
            if (item is NavigationViewItem navItem)
            {
                var label = (navItem.Content as string ?? string.Empty).ToLowerInvariant();
                navItem.Visibility = string.IsNullOrEmpty(query) || label.Contains(query)
                    ? Visibility.Visible
                    : Visibility.Collapsed;
            }
        }
    }

    /// <summary>
    /// Brief "Saved" InfoBar pulse triggered by any setting change. Win11-
    /// native auto-save UX. The actual persistence still happens on Save
    /// click for now; this is a visual hint that the form has been edited.
    /// </summary>
    private DispatcherQueueTimer? _savedPulseTimer;
    private void PulseSavedInfoBar()
    {
        if (!_loaded) return;
        SavedInfoBar.Visibility = Visibility.Visible;
        _savedPulseTimer ??= DispatcherQueue.CreateTimer();
        _savedPulseTimer.Stop();
        _savedPulseTimer.Interval = TimeSpan.FromMilliseconds(1500);
        _savedPulseTimer.IsRepeating = false;
        _savedPulseTimer.Tick -= OnSavedPulseTick;
        _savedPulseTimer.Tick += OnSavedPulseTick;
        _savedPulseTimer.Start();
    }
    private void OnSavedPulseTick(DispatcherQueueTimer sender, object args)
    {
        SavedInfoBar.Visibility = Visibility.Collapsed;
        sender.Stop();
    }

    private void Save_Click(object sender, RoutedEventArgs e)
    {
        // Pull password values from PasswordBoxes into ViewModel before serializing
        if (!string.IsNullOrEmpty(CloudApiKeyBox.Password))
            ViewModel.ApiKey = CloudApiKeyBox.Password;
        if (!string.IsNullOrEmpty(LlmApiKeyBox.Password))
            ViewModel.LlmApiKey = LlmApiKeyBox.Password;
        // Recap key drain — saves to KeyringScope::Recap(<vendor>)
        // via the dedicated FFI. Empty PasswordBox = no-op, so a
        // user closing Settings without touching this field can't
        // wipe an existing Recap-scope key.
        DrainRecapKeyToKeystore();

        // includeLlm + includeRecap: this is the Settings → Save
        // path — the user has been editing everything (incl. LLM
        // provider + Recap section) and explicitly clicks Save, so
        // we MUST persist those fields. Other save sites (per-field
        // LostFocus, theme toggle, …) call ToJson() default and so
        // omit the gated blocks → no accidental wipe on transient VM.
        var json = ViewModel.ToJson(includeLlm: true, includeRecap: true);

        // Tell Rust to update in-memory state and save config.json
        // Rust now knows all fields (including UI appearance), so one writer only.
        try { DimmyNative.dimmy_set_config_json(json); } catch { }

        App.Instance?.ReloadConfig();
        App.Instance?.ApplySettings(ViewModel);
        // Skip the Closed-handler save since we already wrote the
        // current state — flag prevents a redundant FFI round-trip.
        _autoSaveDone = true;
        this.Close();
    }

    private bool _autoSaveDone;

    /// Catch-all auto-save fired by the Closed event so users who
    /// dismiss Settings with the X (or ESC) don't silently lose their
    /// edits. Same code path Save_Click uses, minus the Close() call.
    private void AutoSaveOnClose()
    {
        if (_autoSaveDone) return;
        try
        {
            if (!string.IsNullOrEmpty(CloudApiKeyBox?.Password))
                ViewModel.ApiKey = CloudApiKeyBox.Password;
            if (!string.IsNullOrEmpty(LlmApiKeyBox?.Password))
                ViewModel.LlmApiKey = LlmApiKeyBox.Password;
            // Recap key drain — same as Save_Click. Empty box = no-op.
            DrainRecapKeyToKeystore();
            // Same rationale as Save_Click — closing Settings with X/ESC
            // is the user's "save what I touched" intent; include LLM
            // + Recap so a user who edited those blocks doesn't lose
            // their work, and a user who DIDN'T also doesn't get wipe-d
            // because the VM still holds the loaded values.
            var json = ViewModel.ToJson(includeLlm: true, includeRecap: true);
            DimmyNative.dimmy_set_config_json(json);
            App.Instance?.ReloadConfig();
            App.Instance?.ApplySettings(ViewModel);
            App.Log("AutoSaveOnClose: persisted ViewModel via X/ESC", "Settings");
        }
        catch (Exception ex)
        {
            App.Log($"AutoSaveOnClose failed: {ex.Message}", "Settings");
        }
    }

    /// <summary>Apply the pill-visibility prefs immediately on toggle
    /// so the user sees the effect (next hotkey press / next launch)
    /// without having to click Save. Persistence to ui_prefs.json is
    /// triggered indirectly through App.Instance.ApplySettings →
    /// AppViewModel.PropertyChanged → OnUiPrefsRelevantPropertyChanged.</summary>
    private void PillVisibilityToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (!_loaded) return;
        App.Instance?.ApplySettings(ViewModel);
    }

    /// <summary>Persist a plain config bool to the Rust core IMMEDIATELY on
    /// toggle so it takes effect on the very next dictation without closing
    /// Settings. The streaming / chunk / live-caption switches are read by
    /// the Rust core at start_recording, so without this they only applied
    /// after AutoSaveOnClose — the "Saved" pulse showed but nothing took
    /// effect (reported 2026-06-06). These are non-gated config fields, so
    /// default ToJson() is safe (no LLM/Recap-block wipe).</summary>
    private void LiveConfigToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (!_loaded) return;
        try
        {
            DimmyNative.dimmy_set_config_json(ViewModel.ToJson());
            App.Instance?.ReloadConfig();
        }
        catch (Exception ex)
        {
            App.Log($"LiveConfigToggle_Toggled persist failed: {ex.Message}", "Settings");
        }
    }

    /// 2026-05-08: AudioSource radio buttons removed from Settings —
    /// always-mix architecture means the user no longer picks between
    /// Mic / System / Mix. Stub kept (and called from the loader) so a
    /// stale binding doesn't NRE; sets a sensible default on the
    /// view model for backward compat with config.json that still has
    /// an `audio_source` field.
    private void SyncAudioSourceRadio()
    {
        if (string.IsNullOrEmpty(ViewModel.AudioSource))
            ViewModel.AudioSource = "mix";
    }

    /// Curated recap-model ids ("" Auto + every cloud recap model), derived
    /// from the single-source catalog (ModelCatalog). Used to tell a curated
    /// pick from a free-form custom id. A model add/remove touches only
    /// assets/model-catalog.json; this and the picker follow automatically.
    private static readonly string[] _recapModelKnownTags =
        ModelCatalog.RecapKnownTags().ToArray();

    /// Snap the recap-model picker + custom textbox to whatever the
    /// view-model currently holds. Called once after config load.
    /// If the persisted model id matches a curated entry, select it;
    /// otherwise mark the picker as "__custom__" and reveal the
    /// textbox. Empty = "Auto" (first item).
    private void SyncRecapModelPicker()
    {
        if (RecapModelComboBox == null) return;
        var current = ViewModel.RecapModelOverride ?? "";
        // Match by Tag across the ACTUAL items — the curated cloud
        // entries, the dynamically-injected "Local · …" entries, and the
        // "Auto" ("") / "Custom" ("__custom__") sentinels all live here.
        // Tag-based (not index-based) so injecting local items before
        // Custom can't desync the selection.
        ComboBoxItem? customItem = null;
        foreach (var obj in RecapModelComboBox.Items)
        {
            if (obj is not ComboBoxItem item) continue;
            var tag = item.Tag as string ?? "";
            if (tag == "__custom__") customItem = item;
            if (string.Equals(tag, current, StringComparison.OrdinalIgnoreCase))
            {
                RecapModelComboBox.SelectedItem = item;
                RecapModelCustomCard.Visibility = Visibility.Collapsed;
                return;
            }
        }
        // No match → free-form custom id: select the Custom sentinel and
        // reveal the textbox so the user can edit it.
        if (customItem != null) RecapModelComboBox.SelectedItem = customItem;
        RecapModelCustomCard.Visibility = Visibility.Visible;
    }

    /// Append the local-LLM models to the recap-model picker as
    /// "Local · &lt;name&gt;" entries with Tag `local:&lt;filename&gt;`, inserted
    /// just before the "Custom…" sentinel. The recap dispatch
    /// (`parse_recap_override`) already understands the `local:` prefix
    /// and routes to llama.cpp (Vulkan on Win — any GPU with enough
    /// VRAM), so a cloud-dictation user can pick a private/offline
    /// recap. ALL catalogue models are listed (not just downloaded) so
    /// the option is discoverable; the label carries the state and
    /// selecting a not-yet-downloaded one nudges the user to fetch it
    /// in Settings → LLM. Idempotent: clears previously-injected rows.
    private void PopulateRecapLocalModels()
    {
        if (RecapModelComboBox == null) return;
        try
        {
            for (int i = RecapModelComboBox.Items.Count - 1; i >= 0; i--)
            {
                if (RecapModelComboBox.Items[i] is ComboBoxItem ci
                    && ci.Tag is string t
                    && t.StartsWith("local:", StringComparison.Ordinal))
                {
                    RecapModelComboBox.Items.RemoveAt(i);
                }
            }

            int customIdx = -1;
            for (int i = 0; i < RecapModelComboBox.Items.Count; i++)
            {
                if (RecapModelComboBox.Items[i] is ComboBoxItem ci
                    && (ci.Tag as string) == "__custom__")
                {
                    customIdx = i;
                    break;
                }
            }
            int insertAt = customIdx >= 0 ? customIdx : RecapModelComboBox.Items.Count;

            foreach (var m in _localLlmModels)
            {
                var state = m.Downloaded ? "Ready" : $"{m.SizeMb} MB — download in LLM";
                var item = new ComboBoxItem
                {
                    Content = $"Local · {m.Name} ({state})",
                    Tag = $"local:{m.Filename}",
                };
                RecapModelComboBox.Items.Insert(insertAt, item);
                insertAt++;
            }
        }
        catch (Exception ex)
        {
            App.Log($"PopulateRecapLocalModels: {ex.Message}", "Settings");
        }
    }

    /// Derive the recap vendor from the chosen recap_model_override.
    /// Mirrors Rust's `Provider::from_model_id`:
    ///   claude-*                → "anthropic"
    ///   gpt-*, o1, o3, o4(-...) → "openai"
    ///   gemini-*                → "gemini"
    ///   "" (Auto) or unknown    → fall back to the dictation vendor
    ///
    /// The fallback for empty / unknown is what makes "Auto" mean
    /// "inherit dictation provider" without needing a separate enum
    /// override field.
    private static string RecapVendorFromModel(string? model, string llmUrl)
    {
        if (string.IsNullOrEmpty(model)) return RecapVendorFromUrl(llmUrl);
        var m = model.ToLowerInvariant();
        // Local recap (llama.cpp Vulkan) — no cloud vendor, no key /
        // subscription. Empty vendor → UpdateRecapKeyCardVisibility
        // hides both the key card and the subscription toggle.
        if (m.StartsWith("local:", StringComparison.Ordinal)) return "";
        if (m.StartsWith("claude-", StringComparison.Ordinal)) return "anthropic";
        if (m.StartsWith("gpt-", StringComparison.Ordinal)) return "openai";
        if (m == "o1" || m == "o3" || m == "o4"
            || m.StartsWith("o1-", StringComparison.Ordinal)
            || m.StartsWith("o3-", StringComparison.Ordinal)
            || m.StartsWith("o4-", StringComparison.Ordinal)) return "openai";
        if (m.StartsWith("gemini-", StringComparison.Ordinal)) return "gemini";
        return RecapVendorFromUrl(llmUrl);
    }

    /// Refresh the conditional Recap section UI (UseSameKey toggle +
    /// PasswordBox) based on the derived recap vendor + upstream key
    /// availability + subscription state. Mirrors the LLM section's
    /// LlmUseSameKeyCard / LlmApiKeyCard pattern one layer down.
    ///
    /// Visibility matrix:
    ///   recap_vendor empty / unknown
    ///     → toggle hidden, key card hidden (inherits LLM dispatch)
    ///   recap_vendor == anthropic AND subscription active
    ///     → toggle hidden, key card hidden (CLI handles auth)
    ///   upstream key exists (LLM scope OR STT scope for recap_vendor)
    ///     → toggle visible (default ON)
    ///         toggle ON  → key card hidden, dispatcher reuses upstream
    ///         toggle OFF → key card visible, write to Recap(vendor) scope
    ///   no upstream key
    ///     → toggle hidden + coerced false, key card visible (need a key)
    ///
    /// Snapshot reads: `has_<vendor>_llm_key` (LLM scope) +
    /// `has_<vendor>_key` (STT scope) + `has_<vendor>_recap_key`
    /// (Recap scope, for the "already saved" check icon).
    private void UpdateRecapKeyCardVisibility(string llmUrl)
    {
        if (RecapKeyCard == null || RecapUseSameKeyCard == null) return;

        var recapVendor = RecapVendorFromModel(ViewModel.RecapModelOverride, llmUrl);
        // Recap subscription is INDEPENDENT of dictation. Empty
        // recap_auth_method = api_key path (the dedicated toggle is
        // what flips this on). Mirror of the change in
        // RefreshAuthIntegrationStatus.
        var subscriptionActive =
            string.Equals(ViewModel.RecapAuthMethod, "subscription", StringComparison.OrdinalIgnoreCase);

        // Read the full snapshot once — we need three has_* fields.
        bool hasLlmScopeKey = false;
        bool hasSttScopeKey = false;
        bool hasRecapScopeKey = false;
        if (!string.IsNullOrEmpty(recapVendor))
        {
            try
            {
                var json = Interop.DimmyNative.ReadBuffer(
                    Interop.DimmyNative.dimmy_get_config_json, 16384);
                if (!string.IsNullOrEmpty(json))
                {
                    using var doc = System.Text.Json.JsonDocument.Parse(json);
                    var root = doc.RootElement;
                    if (root.TryGetProperty($"has_{recapVendor}_llm_key", out var vl)
                        && vl.ValueKind == System.Text.Json.JsonValueKind.True) hasLlmScopeKey = true;
                    if (root.TryGetProperty($"has_{recapVendor}_key", out var vs)
                        && vs.ValueKind == System.Text.Json.JsonValueKind.True) hasSttScopeKey = true;
                    if (root.TryGetProperty($"has_{recapVendor}_recap_key", out var vr)
                        && vr.ValueKind == System.Text.Json.JsonValueKind.True) hasRecapScopeKey = true;
                }
            }
            catch { /* keep all booleans false on parse / FFI errors */ }
        }

        _ = hasLlmScopeKey | hasSttScopeKey; // upstream presence no longer gates UI here

        // Recap keys come from the Providers & keys page (recap scope) or the
        // subscription path — there's no inline recap key field or inheritance
        // toggle anymore. The dispatcher derives the recap vendor from the
        // model and pulls its recap/llm-scope key from the keystore.
        RecapUseSameKeyCard.Visibility = Visibility.Collapsed;
        RecapKeyCard.Visibility = Visibility.Collapsed;

        // Update the bound `HasRecapKey` so the PasswordBox shows the
        // ✓ badge + "Already saved · paste to replace" placeholder
        // when a Recap-scope key for the derived vendor exists.
        // Mirror of the LLM's `HasLlmKey` binding.
        ViewModel.HasRecapKey = hasRecapScopeKey;
    }

    /// Toggled handler for the Recap UseSameKey switch — flips the
    /// PasswordBox visibility immediately without waiting for the
    /// next auth refresh, and persists via the auto-save pipeline.
    private void RecapUseSameKey_Toggled(object sender, RoutedEventArgs e)
    {
        if (!_loaded) return;
        // Visibility recompute happens here so the user sees the key
        // field appear/disappear under the toggle without latency.
        UpdateRecapKeyCardVisibility(ViewModel.LlmApiUrl ?? "");
        // ApplySettings persists the new bool to Rust + disk.
        App.Instance?.ApplySettings(ViewModel);
    }

    /// Drain the Recap PasswordBox into the Rust keystore under the
    /// `Recap(<vendor>)` scope. Called from Save_Click + AutoSaveOnClose
    /// to mirror the inline LLM key drain pattern (no per-card Save
    /// button — the global Settings save persists everything).
    ///
    /// Vendor is DERIVED from the currently-selected recap model.
    /// Empty Password = no-op (don't clear an existing entry just
    /// because the user opened Settings without typing).
    private void DrainRecapKeyToKeystore()
    {
        var key = RecapKeyBox?.Password ?? "";
        if (string.IsNullOrEmpty(key)) return;
        var llmUrl = ViewModel.LlmApiUrl ?? "";
        var vendor = RecapVendorFromModel(ViewModel.RecapModelOverride, llmUrl);
        if (string.IsNullOrEmpty(vendor)) return;
        int rc;
        try
        {
            // scope="recap" → dedicated Recap(<vendor>) keystore entry
            // distinct from the LLM-scope key for the same vendor.
            rc = Interop.DimmyNative.dimmy_save_llm_provider_key("recap", vendor, key);
        }
        catch (Exception ex)
        {
            App.Log($"[Auth] save_llm_provider_key threw: {ex.Message}", "Auth");
            return;
        }
        if (rc == 0)
        {
            App.Log($"[Auth] saved recap key for provider={vendor}", "Auth");
            RecapKeyBox!.Password = "";
            ViewModel.HasRecapKey = true;
        }
        else
        {
            App.Log($"[Auth] save_llm_provider_key failed rc={rc} provider={vendor}", "Auth");
        }
    }

    private void RecapModel_SelectionChanged(object sender,
        Microsoft.UI.Xaml.Controls.SelectionChangedEventArgs e)
    {
        if (RecapModelComboBox?.SelectedItem is not Microsoft.UI.Xaml.Controls.ComboBoxItem item) return;
        var tag = item.Tag as string ?? "";
        if (tag == "__custom__")
        {
            RecapModelCustomCard.Visibility = Visibility.Visible;
            // Don't overwrite the existing custom value — let the user
            // pick up where they left off in the textbox.
            return;
        }
        RecapModelCustomCard.Visibility = Visibility.Collapsed;
        ViewModel.RecapModelOverride = tag;
        if (_loaded)
        {
            App.Instance?.ApplySettings(ViewModel);
            // Recompute the full conditional Recap section (subscription
            // toggle visibility depends on whether the new model is
            // Anthropic; key card visibility depends on the derived
            // vendor and upstream key state).
            RefreshAuthIntegrationStatus();

            // Local recap picked but the .gguf isn't on disk → the recap
            // would fail with rc -4. Nudge the user to download it.
            if (tag.StartsWith("local:", StringComparison.Ordinal))
            {
                var filename = tag.Substring("local:".Length);
                var m = _localLlmModels.FirstOrDefault(x => x.Filename == filename);
                if (m != null && !m.Downloaded)
                    _ = NudgeDownloadLocalRecapModelAsync(m.Name);
            }
        }
    }

    private async System.Threading.Tasks.Task NudgeDownloadLocalRecapModelAsync(string modelName)
    {
        try
        {
            var dlg = new ContentDialog
            {
                Title = "Model not downloaded yet",
                Content = $"\"{modelName}\" runs the recap locally (offline, private) but isn't on disk yet.\n\nGo to Settings → LLM, switch to Local, and download it. Until then the recap will fall back / fail.",
                CloseButtonText = "Got it",
                XamlRoot = this.Content.XamlRoot,
            };
            await dlg.ShowAsync();
        }
        catch (Exception ex)
        {
            App.Log($"NudgeDownloadLocalRecapModel: {ex.Message}", "Settings");
        }
    }

    private void RecapModelCustom_LostFocus(object sender, RoutedEventArgs e)
    {
        // The TextBox is Two-Way bound, so RecapModelOverride is already
        // up to date by the time we get here. Just push the config.
        if (_loaded) App.Instance?.ApplySettings(ViewModel);
    }

    // ── Meetings storage folder ──────────────────────────────────────

    /// Refresh the path label from the *effective* meeting dir (resolved
    /// by the Rust core, honouring the override). Appends "(default)"
    /// when no custom override is set so the user knows which state
    /// they're in. Called on load + after pick / reset.
    private void RefreshMeetingFolderDisplay()
    {
        try
        {
            var effective = Services.BuildInfo.MeetingsDirPath;
            bool isDefault = string.IsNullOrEmpty(ViewModel.MeetingStoragePath);
            MeetingFolderPathText.Text = isDefault ? $"{effective}  (default)" : effective;
            ToolTipService.SetToolTip(MeetingFolderPathText, effective);
            MeetingFolderResetBtn.IsEnabled = !isDefault;
        }
        catch (Exception ex)
        {
            App.Log($"RefreshMeetingFolderDisplay: {ex.Message}", "Settings");
        }
    }

    private void MeetingFolderBrowse_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            var hwnd = WindowHelper.GetHwnd(this);
            var current = Services.BuildInfo.MeetingsDirPath;
            var picked = Helpers.Win32FileDialog.PickFolder(
                hwnd, "Choose where to store meeting recordings", current);
            if (string.IsNullOrEmpty(picked)) return; // cancelled

            // Validate writability before committing — a read-only or
            // disappeared share would otherwise silently route meetings
            // to a dir the core later rejects + falls back from.
            if (!IsDirectoryWritable(picked!))
            {
                _ = ShowMeetingFolderErrorAsync(picked!);
                return;
            }

            PersistMeetingStoragePath(picked!);
        }
        catch (Exception ex)
        {
            App.Log($"MeetingFolderBrowse: {ex.Message}", "Settings");
        }
    }

    private void MeetingFolderReset_Click(object sender, RoutedEventArgs e)
    {
        PersistMeetingStoragePath(string.Empty);
    }

    /// Single-writer rule: send a targeted {"meeting_storage_path": "..."}
    /// straight to the Rust core (NOT a full ToJson save — that omits the
    /// field when empty, which would defeat a reset). Empty string = reset
    /// to default. Updates the VM + display after the round-trip.
    private void PersistMeetingStoragePath(string path)
    {
        try
        {
            var payload = System.Text.Json.JsonSerializer.Serialize(
                new Dictionary<string, object?> { ["meeting_storage_path"] = path });
            int rc = DimmyNative.dimmy_set_config_json(payload);
            if (rc != 0)
            {
                App.Log($"meeting_storage_path set returned rc={rc}", "Settings");
                return;
            }
            ViewModel.MeetingStoragePath = path;
            RefreshMeetingFolderDisplay();
            App.Log($"meeting_storage_path {(string.IsNullOrEmpty(path) ? "reset to default" : "= " + path)}", "Settings");
        }
        catch (Exception ex)
        {
            App.Log($"PersistMeetingStoragePath: {ex.Message}", "Settings");
        }
    }

    private static bool IsDirectoryWritable(string dir)
    {
        try
        {
            System.IO.Directory.CreateDirectory(dir);
            var probe = System.IO.Path.Combine(dir, ".dimmy_write_probe");
            System.IO.File.WriteAllText(probe, "");
            System.IO.File.Delete(probe);
            return true;
        }
        catch
        {
            return false;
        }
    }

    private async System.Threading.Tasks.Task ShowMeetingFolderErrorAsync(string dir)
    {
        try
        {
            var dlg = new ContentDialog
            {
                Title = "Folder not writable",
                Content = $"Dimmy can't write to:\n{dir}\n\nPick a different folder.",
                CloseButtonText = "OK",
                XamlRoot = this.Content.XamlRoot,
            };
            await dlg.ShowAsync();
        }
        catch (Exception ex)
        {
            App.Log($"ShowMeetingFolderError: {ex.Message}", "Settings");
        }
    }

    private void Theme_Checked(object sender, RoutedEventArgs e)
    {
        if (sender is RadioButton rb && rb.Tag is string tag && Content is FrameworkElement root)
        {
            root.RequestedTheme = tag switch
            {
                "Light" => ElementTheme.Light,
                "Dark" => ElementTheme.Dark,
                _ => ElementTheme.Default,
            };
            // Save theme choice and apply to pill (Light → glass, Dark/Default → dark)
            ViewModel.Theme = tag;
            if (_loaded)
            {
                // Persist to UiPreferences (NOT config.json — Rust core
                // has no Theme field and would silently drop it). The
                // pill-prefs save path is already wired separately so
                // we just touch the Theme key here.
                try
                {
                    var prefs = Services.UiPreferences.Load();
                    prefs.Theme = tag;
                    prefs.Save();
                }
                catch (Exception ex)
                {
                    App.Log($"Theme persist failed: {ex.Message}", "Settings");
                }
                App.Instance?.ApplySettings(ViewModel);
            }
        }
    }

    private void BorderStyle_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_loaded && sender is ComboBox cb && cb.SelectedItem is string)
        {
            App.Instance?.ApplySettings(ViewModel);
        }
    }

    private void WaveformStyle_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_loaded && sender is ComboBox cb && cb.SelectedItem is string)
        {
            App.Instance?.ApplySettings(ViewModel);
        }
    }

    private void OverlayPosition_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_loaded && sender is ComboBox cb && cb.SelectedItem is string pos)
            App.Instance?.ApplySettings(ViewModel);
    }

    private void OverlayPositionCell_Checked(object sender, RoutedEventArgs e)
    {
        // Position grid cells: each RadioButton's Tag is the canonical
        // position string. The Tag→ViewModel write happens via the TwoWay
        // binding on IsChecked + StringEqualityConverter; this handler
        // just notifies the live overlay so the pill jumps immediately.
        if (_loaded) App.Instance?.ApplySettings(ViewModel);
    }

    private void ResetPosition_Click(object sender, RoutedEventArgs e)
    {
        ViewModel.OverlayPosition = "Bottom Right";
        App.Instance?.ApplySettings(ViewModel);
    }

    /// <summary>
    /// Clear the sticky GPU known-bad marker. Effect takes hold on the next
    /// process launch since the GPU backend status is cached for the life of
    /// the current process. We refresh the displayed status so the user sees
    /// the marker disappear immediately.
    /// </summary>
    private void RetryGpu_Click(object sender, RoutedEventArgs e)
    {
        DimmyNative.dimmy_gpu_clear_known_bad();
        ViewModel.LoadGpuStatus();
    }

    private void PopulateStats()
    {
        var secs = ViewModel.StatsTotalSpeakingSecs;
        var mins = (int)(secs / 60);
        var hours = mins / 60;
        var remainMins = mins % 60;

        SpeakingTimeText.Text = hours > 0
            ? $"{hours}h {remainMins}m"
            : $"{mins}m {(int)(secs % 60)}s";

        var saved = ViewModel.TimeSavedEstimate;
        var savedMins = (int)(saved / 60);
        var savedHours = savedMins / 60;
        var savedRemainMins = savedMins % 60;

        TimeSavedText.Text = savedHours > 0
            ? $"~{savedHours}h {savedRemainMins}m"
            : $"~{savedMins}m";
    }

    private string _currentVersion = "0.0.0";

    private void PopulateVersion()
    {
        _currentVersion = DimmyNative.ReadBuffer(DimmyNative.dimmy_get_version, 64) ?? "0.0.0";
        VersionText.Text = $"v{_currentVersion}";
        // Append " · STAGING" suffix on staging builds. The sidebar banner
        // already announces the flavor loudly; this just makes sure About
        // page screenshots can never be mistaken for prod ones.
        var flavorSuffix = BuildInfo.IsStaging ? " · STAGING" : string.Empty;
        HeroTitleText.Text = $"Dimmy {_currentVersion}{flavorSuffix}";
        HeroSubText.Text = $"Version {_currentVersion}{flavorSuffix}";
        // Sidebar staging banner — flip on once we know the flavor. Done
        // here (rather than in the constructor) because XAML elements
        // are initialised lazily.
        if (StagingBanner is not null)
            StagingBanner.Visibility = BuildInfo.IsStaging
                ? Microsoft.UI.Xaml.Visibility.Visible
                : Microsoft.UI.Xaml.Visibility.Collapsed;

        // Update UI gate — auto-update is a paid feature
        // (LicenseService.ScopeNames.AutoUpdate). Free users see
        // neither the channel selector nor the "Update ready" card;
        // the UpdateService background loop also skips the poll.
        // Source builds (HasScope=true for everything) get the full
        // UI for dev testing. Subscribe to LicenseChanged so
        // redemption mid-session flips the card on without restart.
        bool autoUpdateEnabled = Services.LicenseService.HasScope(
            Services.LicenseService.ScopeNames.AutoUpdate);
        UpdateChannelCard.Visibility = autoUpdateEnabled
            ? Visibility.Visible : Visibility.Collapsed;
        Action onLicenseChanged = () => DispatcherQueue.TryEnqueue(() =>
        {
            bool nowEnabled = Services.LicenseService.HasScope(
                Services.LicenseService.ScopeNames.AutoUpdate);
            UpdateChannelCard.Visibility = nowEnabled
                ? Visibility.Visible : Visibility.Collapsed;
            // Hide the "Update ready" card too if scope was dropped
            // (e.g. license expired). Show stays controlled by the
            // UpdateReady event when scope returns.
            if (!nowEnabled) UpdateCard.Visibility = Visibility.Collapsed;
        });
        Services.LicenseService.LicenseChanged += onLicenseChanged;
        Closed += (_, _) =>
        {
            try { Services.LicenseService.LicenseChanged -= onLicenseChanged; } catch { }
        };

        // Channel selector — restore last pick.
        try
        {
            var prefs = Services.UiPreferences.Load();
            for (int i = 0; i < UpdateChannelCombo.Items.Count; i++)
            {
                if (UpdateChannelCombo.Items[i] is ComboBoxItem cbi
                    && cbi.Tag is string tag && tag == prefs.UpdateChannel)
                {
                    UpdateChannelCombo.SelectedIndex = i;
                    break;
                }
            }
            if (UpdateChannelCombo.SelectedIndex < 0) UpdateChannelCombo.SelectedIndex = 0;
        }
        catch { }

        // Subscribe + reflect current state. UpdateService background
        // loop may have already downloaded an update before Settings
        // opened — handle both "already ready" and "ready while
        // Settings is open" without leaking subscriptions: detach on
        // Closed below.
        if (Services.UpdateService.Instance is { } upd)
        {
            upd.UpdateReady += UpdateService_UpdateReady;
            if (upd.IsUpdateReady) ReflectUpdateReady(upd.PendingVersion);
            Closed += (_, _) => { try { upd.UpdateReady -= UpdateService_UpdateReady; } catch { } };
        }
    }

    private void UpdateService_UpdateReady(object? sender, EventArgs e)
    {
        var version = Services.UpdateService.Instance?.PendingVersion ?? "";
        DispatcherQueue.TryEnqueue(() => ReflectUpdateReady(version));
    }

    private void ReflectUpdateReady(string version)
    {
        UpdateCard.Description = string.IsNullOrEmpty(version)
            ? "A new version of Dimmy is downloaded and ready to install."
            : $"Dimmy v{version} is downloaded and ready to install.";
        UpdateCard.Visibility = Visibility.Visible;
        UpdateLink.NavigateUri = new Uri(
            "https://github.com/KonradDallaOrg/dimmy/releases" +
            (string.IsNullOrEmpty(version) ? "" : $"/tag/v{version}"));
    }

    private void UpdateInstallNow_Click(object sender, RoutedEventArgs e)
    {
        // ApplyAndRestart never returns: Velopack spawns its updater,
        // tears down the current EXE, and re-launches the new build.
        // No need to dispose anything here — the OS reclaims it.
        Services.UpdateService.Instance?.ApplyAndRestart();
    }

    private void UpdateChannel_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!_loaded) return;
        if (UpdateChannelCombo.SelectedItem is not ComboBoxItem cbi) return;
        if (cbi.Tag is not string tag) return;
        try
        {
            var prefs = Services.UiPreferences.Load();
            var previousTag = prefs.UpdateChannel ?? "stable";
            prefs.UpdateChannel = tag;
            prefs.Save();
            App.Log($"channel set to {tag}, forcing re-check", "Update");
            if (!string.Equals(previousTag, tag, StringComparison.Ordinal))
            {
                Interop.DimmyNative.TrackEvent("update.channel_changed", new
                {
                    from = previousTag,
                    to = tag,
                });
            }
            // Channel change must re-check immediately — the user
            // expects toggling "Pre-release" to surface a staging
            // build right away if one exists, not at the next 6h tick.
            _ = Services.UpdateService.Instance?.CheckAndDownloadAsync();
        }
        catch (Exception ex)
        {
            App.Log($"channel save failed: {ex.Message}", "Update");
        }
    }

    private async void CheckUpdates_Click(object sender, RoutedEventArgs e)
    {
        // Force-fire the Velopack background check. Useful for users
        // who don't want to wait up to 6 h for the next periodic tick
        // (e.g. just saw a release announcement) and for our own
        // staging-to-staging test cycle where the second build needs
        // to be discovered as soon as it's published.
        //
        // Falls back to opening the download page in the browser when
        // the UpdateService isn't enabled — that's the auto_update
        // license scope being absent (free user) OR a dev source
        // build where Velopack metadata isn't present. Either way the
        // user still gets a way to get the latest version.
        if (sender is Button btn) btn.IsEnabled = false;
        try
        {
            var svc = Services.UpdateService.Instance;
            if (svc is null
                || !Services.LicenseService.HasScope(Services.LicenseService.ScopeNames.AutoUpdate))
            {
                await global::Windows.System.Launcher.LaunchUriAsync(
                    new Uri("https://dimmy.app/download"));
                return;
            }
            await svc.CheckAndDownloadAsync();
        }
        catch (Exception ex)
        {
            App.Log($"CheckUpdates_Click exc: {ex.Message}", "Update");
        }
        finally
        {
            if (sender is Button btn2) btn2.IsEnabled = true;
        }
    }

    private async void ReleaseNotes_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            await global::Windows.System.Launcher.LaunchUriAsync(
                new Uri("https://github.com/KonradDallaOrg/dimmy/releases"));
        }
        catch { }
    }

    // CheckForUpdateAsync + IsNewerVersion removed 2026-05-11 — the
    // GitHub-Releases poll has been replaced by UpdateService (Velopack
    // wrapper). The new path silently downloads the delta + surfaces
    // UpdateReady, so this Settings window only needs to reflect state
    // raised by that service.

    private void Cancel_Click(object sender, RoutedEventArgs e)
    {
        this.Close();
    }

    // ── Custom dictionary handlers ──────────────────────────────
    // The dict lives in the Rust AppState (round-tripped through
    // config.json). We refresh the ObservableCollection on Settings
    // open + after every add / remove so the ListView reflects
    // reality even if a global-hotkey Add fired while Settings was
    // already open.

    private void ReloadUserDict()
    {
        try
        {
            var list = Services.DictionaryService.List();
            ViewModel.UserDict.Clear();
            foreach (var w in list) ViewModel.UserDict.Add(w);
            if (DictEmptyHint is not null)
                DictEmptyHint.Visibility = list.Count == 0
                    ? Visibility.Visible : Visibility.Collapsed;
        }
        catch (Exception ex)
        {
            App.Log($"ReloadUserDict exc: {ex.Message}", "Dict");
        }
    }

    private void DictAdd_Click(object sender, RoutedEventArgs e)
    {
        var word = (DictAddTextBox?.Text ?? "").Trim();
        if (string.IsNullOrEmpty(word))
        {
            DictStatusText.Text = "Type a word first.";
            return;
        }
        int rc = Services.DictionaryService.Add(word);
        switch (rc)
        {
            case 0:
                DictStatusText.Text = $"Added “{word}”.";
                DictAddTextBox!.Text = "";
                break;
            case 1:
                DictStatusText.Text = $"“{word}” is already in the dictionary.";
                break;
            default:
                DictStatusText.Text = $"Couldn't add (rc={rc}).";
                break;
        }
        ReloadUserDict();
    }

    private void DictAddTextBox_KeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        if (e.Key == global::Windows.System.VirtualKey.Enter)
        {
            DictAdd_Click(sender, e);
            e.Handled = true;
        }
    }

    private void RemoveExcludedApp_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement fe) return;
        if (fe.Tag is not string appId || string.IsNullOrEmpty(appId)) return;
        ViewModel.CallDetectExcludedApps.Remove(appId);
        // Push the updated list so Rust deletes its exclusion + the
        // detector picks up the change on the next signal. ToJson is
        // the canonical save path (single-writer rule).
        try
        {
            int rc = Interop.DimmyNative.dimmy_set_config_json(
                System.Text.Json.JsonSerializer.Serialize(ViewModel.ToJson()));
            App.Log($"[CallDetect] removed exclusion '{appId}' rc={rc}", "CallDetect");
        }
        catch (Exception ex)
        {
            App.Log($"[CallDetect] remove exclusion EXC: {ex.Message}", "CallDetect");
        }
    }

    private void DictRemove_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement fe) return;
        if (fe.Tag is not string word || string.IsNullOrEmpty(word)) return;
        int removed = Services.DictionaryService.Remove(word);
        if (removed > 0)
        {
            DictStatusText.Text = $"Removed “{word}”.";
        }
        else
        {
            DictStatusText.Text = $"“{word}” was not in the dictionary.";
        }
        ReloadUserDict();
    }

    // ── Privacy panel handlers ──────────────────────────────────

    private void RefreshAnonymousIdText()
    {
        try
        {
            var id = DimmyNative.TelemetryAnonymousId() ?? "(unavailable)";
            // Display only the first 8 chars + ellipsis to avoid overwhelming
            // the UI with the full UUID. The full ID never needs to be shown.
            var preview = id.Length >= 8 ? $"{id[..8]}…" : id;
            AnonymousIdText.Text = preview;
        }
        catch
        {
            AnonymousIdText.Text = "(unavailable)";
        }
    }

    private void ResetAnonymousId_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            DimmyNative.TelemetryResetAnonymousId();
            AnonymousIdText.Text = "(reset; restart Dimmy to apply)";
        }
        catch { /* best-effort, this is a privacy action */ }
    }

    private void SendFeedback_Click(object sender, RoutedEventArgs e)
        => TrySendFeedback();

    /// One-click "Enable & send": the previous attempt returned -2
    /// (telemetry disabled). Turn sending on, then retry the same
    /// message. The explicit click is the user's consent.
    ///
    /// Feedback rides the Sentry pipeline, which is gated by the
    /// CRASH-reporting flag (dimmy_telemetry_set_crash_enabled), NOT
    /// the analytics flag. Flipping TelemetryEnabled here left the
    /// feedback gate closed → capture_feedback kept returning -2 and
    /// the button "did nothing". Set the VM's CrashReportsEnabled
    /// instead: its OnChanged forwards to the FFI and the bound
    /// "Send crash reports" toggle updates with it (no UI lie).
    private void EnableAndSendFeedback_Click(object sender, RoutedEventArgs e)
    {
        try { ViewModel.CrashReportsEnabled = true; }
        catch { }
        EnableAndSendFeedbackBtn.Visibility = Visibility.Collapsed;
        TrySendFeedback();
    }

    /// Send the feedback form and surface the REAL rc — no blanket
    /// "Sent!". rc: 1 sent · 0 empty · -1 bad input · -2 disabled
    /// (offer Enable &amp; send) · -3 not configured (dev build).
    private void TrySendFeedback()
    {
        var message = FeedbackText.Text?.Trim() ?? string.Empty;
        if (string.IsNullOrWhiteSpace(message))
        {
            FeedbackStatus.Text = "Type something first.";
            return;
        }
        var kind = (FeedbackKindCombo.SelectedItem as ComboBoxItem)?.Tag as string ?? "general";
        var email = FeedbackEmail.Text?.Trim();
        if (string.IsNullOrWhiteSpace(email)) email = null;

        int rc;
        try
        {
            rc = DimmyNative.CaptureFeedback(kind, message, email);
        }
        catch
        {
            FeedbackStatus.Text = "Couldn't send right now. Try again later.";
            return;
        }

        switch (rc)
        {
            case 1:
                FeedbackStatus.Text = "Thanks! Sent.";
                FeedbackText.Text = string.Empty;
                FeedbackEmail.Text = string.Empty;
                EnableAndSendFeedbackBtn.Visibility = Visibility.Collapsed;
                break;
            case -2:
                // Telemetry disabled — offer one-click enable + send.
                FeedbackStatus.Text = "Sending is off — use “Enable & send”.";
                EnableAndSendFeedbackBtn.Visibility = Visibility.Visible;
                break;
            case -3:
                FeedbackStatus.Text = "Feedback isn't available in this build.";
                break;
            default:
                FeedbackStatus.Text = "Couldn't send right now. Try again later.";
                break;
        }
    }

    // ── Notion integration ────────────────────────────────────────
    // Setup is delegated to NotionConnectDialog (3-step wizard). This
    // surface only owns the summary card + auto-send toggle + the
    // launchers that open the wizard at the right step.

    private void NotionShowMessage(string text, bool isError)
    {
        NotionMessageText.Text = text;
        NotionMessageBar.Background = isError
            ? (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["SystemFillColorCriticalBackgroundBrush"]
            : (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["SystemFillColorSuccessBackgroundBrush"];
        NotionMessageBar.Visibility = Visibility.Visible;
    }

    /// <summary>Refresh the Integrations summary card from the current
    /// ViewModel state. Idempotent — safe to call from initial load
    /// and after every wizard close.</summary>
    private void NotionRefreshSummary()
    {
        bool connected = ViewModel.HasNotionToken;
        if (connected)
        {
            var dest = string.IsNullOrEmpty(ViewModel.NotionTargetTitle)
                ? "no destination yet"
                : $"recaps land in “{ViewModel.NotionTargetTitle}”";
            NotionStatusText.Text = $"Connected — {dest}";
            NotionStatusGlyph.Glyph = "\uE73E"; // CheckMark
            NotionStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)
                Application.Current.Resources["SystemFillColorSuccessBrush"];
            NotionDisconnectedActions.Visibility = Visibility.Collapsed;
            NotionConnectedActions.Visibility = Visibility.Visible;
        }
        else
        {
            NotionStatusText.Text = "Not connected";
            NotionStatusGlyph.Glyph = "\uEA39"; // Info
            NotionStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)
                Application.Current.Resources["TextFillColorTertiaryBrush"];
            NotionDisconnectedActions.Visibility = Visibility.Visible;
            NotionConnectedActions.Visibility = Visibility.Collapsed;
        }
    }

    private async System.Threading.Tasks.Task RunNotionWizardAsync(int initialStep)
    {
        var dlg = new NotionConnectDialog
        {
            XamlRoot = (this.Content as FrameworkElement)?.XamlRoot,
            InitialStep = initialStep,
        };
        dlg.SetExistingTarget(ViewModel.NotionTargetId ?? "");
        try
        {
            await dlg.ShowAsync();
        }
        catch (Exception ex)
        {
            App.Log($"Notion wizard exc: {ex}", "Notion");
            NotionShowMessage($"Wizard error: {ex.Message}", isError: true);
            return;
        }
        // Whatever the wizard's exit reason, sync our state with what
        // the user actually did inside it. The wizard wrote the token
        // to keystore on Verify and may have picked a target.
        if (dlg.TokenVerified)
        {
            ViewModel.HasNotionToken = true;
        }
        if (dlg.Completed && !string.IsNullOrEmpty(dlg.PickedTargetId))
        {
            ViewModel.NotionTargetId = dlg.PickedTargetId;
            ViewModel.NotionTargetKind = dlg.PickedTargetKind;
            ViewModel.NotionTargetTitle = dlg.PickedTargetTitle;
            try
            {
                // includeNotion: true — this is the only "explicit
                // Notion intent" save site. Generic Settings saves
                // skip these fields so a transient empty VM never
                // wipes a valid destination from disk.
                var json = ViewModel.ToJson(includeNotion: true);
                int rc = DimmyNative.dimmy_set_config_json(json);
                App.Log($"Notion wizard saved rc={rc} id={dlg.PickedTargetId} title='{dlg.PickedTargetTitle}'", "Notion");
                if (rc == 0)
                {
                    App.Instance?.ApplySettings(ViewModel);
                    NotionShowMessage(
                        $"Recaps will land in “{dlg.PickedTargetTitle}” ({dlg.PickedTargetKind}). All set.",
                        isError: false);
                }
                else
                {
                    NotionShowMessage(
                        $"Saved locally but Rust set_config_json returned {rc}.",
                        isError: true);
                }
            }
            catch (Exception ex)
            {
                App.Log($"Notion wizard persist exc: {ex}", "Notion");
                NotionShowMessage($"Couldn't save destination: {ex.Message}", isError: true);
            }
        }
        NotionRefreshSummary();
    }

    private async void NotionConnect_Click(object sender, RoutedEventArgs e)
    {
        // Token already valid? Skip prepare + token, jump to picker.
        // The connect button should only be visible when HasNotionToken
        // is false, but a stale binding could race.
        int initial = ViewModel.HasNotionToken ? 3 : 1;
        await RunNotionWizardAsync(initial);
    }

    private async void NotionChangeDestination_Click(object sender, RoutedEventArgs e)
    {
        await RunNotionWizardAsync(initialStep: 3);
    }

    private async void NotionDisconnect_Click(object sender, RoutedEventArgs e)
    {
        var confirm = new ContentDialog
        {
            Title = "Disconnect Notion?",
            Content = "Dimmy will forget your token and destination. Your Notion content stays untouched. You can reconnect any time.",
            PrimaryButtonText = "Disconnect",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
            XamlRoot = (this.Content as FrameworkElement)?.XamlRoot,
        };
        if ((await confirm.ShowAsync()) != ContentDialogResult.Primary) return;
        try
        {
            // Empty token = clear in keystore (notion.rs handles the
            // empty-string branch as "forget").
            var rcToken = Services.NotionService.SetToken("");
            ViewModel.HasNotionToken = false;
            ViewModel.NotionTargetId = "";
            ViewModel.NotionTargetKind = "";
            ViewModel.NotionTargetTitle = "";
            ViewModel.NotionAutoSend = false;
            // Explicit Disconnect — caller intends to clear the
            // destination; pass includeNotion: true so the empty
            // strings reach Rust and wipe the state.
            var json = ViewModel.ToJson(includeNotion: true);
            int rcCfg = DimmyNative.dimmy_set_config_json(json);
            App.Log($"Notion disconnected rcToken={rcToken} rcCfg={rcCfg}", "Notion");
            App.Instance?.ApplySettings(ViewModel);
            NotionShowMessage("Disconnected. Token and destination removed from this device.", isError: false);
        }
        catch (Exception ex)
        {
            App.Log($"Notion disconnect exc: {ex}", "Notion");
            NotionShowMessage($"Couldn't disconnect: {ex.Message}", isError: true);
        }
        NotionRefreshSummary();
    }

    private void NotionAutoSend_Toggled(object sender, RoutedEventArgs e)
    {
        // Persist immediately so the next meeting honours the toggle
        // even if the user never clicks Save.
        if (!_loaded) return;
        try
        {
            var json = ViewModel.ToJson();
            int rc = DimmyNative.dimmy_set_config_json(json);
            App.Log($"Notion auto-send toggled rc={rc} new={ViewModel.NotionAutoSend}", "Notion");
        }
        catch (Exception ex)
        {
            App.Log($"NotionAutoSend save exc: {ex.Message}", "Notion");
        }
    }

    // ── Claude Desktop MCP bridge ─────────────────────────────────

    private async void McpConnect_Click(object sender, RoutedEventArgs e)
    {
        var dialog = new ClaudeDesktopConnectDialog
        {
            XamlRoot = (this.Content as FrameworkElement)?.XamlRoot,
            // Jump past detect/patch if already done — saves clicks on re-runs.
            InitialStep = DecideMcpInitialStep(),
        };
        await dialog.ShowAsync();
        RefreshMcpCard();
    }

    private async void McpDisconnect_Click(object sender, RoutedEventArgs e)
    {
        var confirm = new ContentDialog
        {
            Title = "Disconnect Claude Desktop?",
            Content = "Dimmy will remove its extension from Claude Desktop. Other extensions stay in place. Restart Claude Desktop afterwards so it forgets the connection.",
            PrimaryButtonText = "Disconnect",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
            XamlRoot = (this.Content as FrameworkElement)?.XamlRoot,
        };
        if ((await confirm.ShowAsync()) != ContentDialogResult.Primary) return;
        var (removed, rc) = await System.Threading.Tasks.Task.Run(() => DimmyNative.UninstallClaudeDesktopExtension());
        App.Log($"MCP disconnect removed={removed} rc={rc}", "ClaudeDesktop");
        RefreshMcpCard();
    }

    private void McpRefresh_Click(object sender, RoutedEventArgs e)
    {
        RefreshMcpCard();
    }

    private static int DecideMcpInitialStep()
    {
        var status = DimmyNative.GetClaudeDesktopStatus();
        if (status.ExtensionInstalled) return 3; // jump to heartbeat verify
        if (status.Installed) return 2;          // skip detect, go straight to install
        return 1;
    }

    private async void RefreshMcpCard()
    {
        var status = await System.Threading.Tasks.Task.Run(() => DimmyNative.GetClaudeDesktopStatus());
        if (McpStatusText == null) return; // page not yet realized

        // Real Claude icon if we can extract it from the MSIX install;
        // otherwise the bundled SVG (already set as XAML default) stays.
        // Background-task style — UI thread re-binds via DispatcherQueue
        // when the file path lands.
        _ = System.Threading.Tasks.Task.Run(async () =>
        {
            var path = await Helpers.ClaudeIconExtractor.TryExtractAsync();
            if (!string.IsNullOrEmpty(path))
            {
                DispatcherQueue.TryEnqueue(() =>
                {
                    if (McpProviderIcon != null)
                    {
                        McpProviderIcon.Source = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage(
                            new System.Uri(path));
                    }
                });
            }
        });

        // Three rendering states: missing → patched-but-cold → alive.
        // "alive" requires a heartbeat fresher than 90 s (the binary
        // writes every 30 s; we leave headroom for the polling slack).
        // Segoe Fluent Icon code points (use \u escapes — literal
        // chars get stripped by some editors and end up as empty
        // strings, leaving the card with no glyph at all).
        const string GlyphCheck = "\uE73E";   // CheckMark
        const string GlyphCircle = "\uEA39";  // StatusCircleBlock outline

        bool alive = status.HeartbeatAgeSecs.HasValue && status.HeartbeatAgeSecs.Value < 90;
        if (alive)
        {
            McpStatusText.Text = status.LastCallTool != null
                ? $"Connected — last call {status.LastCallAgoSecs ?? 0}s ago ({status.LastCallTool})."
                : $"Connected — heartbeat {status.HeartbeatAgeSecs!.Value}s ago.";
            McpStatusGlyph.Glyph = GlyphCheck;
            McpStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
            McpDisconnectedActions.Visibility = Visibility.Collapsed;
            McpConnectedActions.Visibility = Visibility.Visible;
        }
        else if (status.ExtensionInstalled)
        {
            // Heartbeat ages out fast (90 s) because Claude Desktop
            // kills idle MCP servers on its own schedule. That's not
            // an error state — it means "registered, Claude will
            // respawn on demand". Keep the phrasing neutral but use
            // the green check so the card reads as "✓ connected" at
            // a glance.
            McpStatusText.Text = status.ExtensionVersion != null
                ? $"Installed (v{status.ExtensionVersion}) — Claude spawns on demand."
                : "Installed — Claude spawns on demand.";
            McpStatusGlyph.Glyph = GlyphCheck;
            McpStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["SystemFillColorSuccessBrush"];
            McpDisconnectedActions.Visibility = Visibility.Collapsed;
            McpConnectedActions.Visibility = Visibility.Visible;
        }
        else
        {
            McpStatusText.Text = status.Installed
                ? "Not connected to Claude Desktop yet."
                : "Claude Desktop isn't installed on this machine.";
            McpStatusGlyph.Glyph = GlyphCircle;
            McpStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["TextFillColorTertiaryBrush"];
            McpDisconnectedActions.Visibility = Visibility.Visible;
            McpConnectedActions.Visibility = Visibility.Collapsed;
        }
    }
}
