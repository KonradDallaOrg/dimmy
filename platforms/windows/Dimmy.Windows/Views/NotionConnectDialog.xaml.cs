using System;
using System.Collections.Generic;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Dimmy.Windows.Interop;
using Dimmy.Windows.Services;

namespace Dimmy.Windows.Views;

/// <summary>
/// 3-step Notion connection wizard. Linear, modal, re-runnable.
///
/// Flow:
///   Step 1 — instructions + "Open Notion" link (no input).
///   Step 2 — paste token, Verify (FFI ping). Next blocked until ✓.
///   Step 3 — Refresh list, pick destination from ComboBox.
///
/// The ContentDialog buttons (Primary/Secondary/Close) are repurposed
/// per step — the state machine picks labels (Next/Done) and
/// enables/disables Primary based on validation. Cancel is always
/// available; if the user cancels mid-wizard with a token already
/// saved, the token stays — they can Resume by relaunching.
///
/// Re-runnable design: callers pass <see cref="InitialStep"/> before
/// ShowAsync — 1 = full setup (default), 3 = "change destination"
/// (jumps past prepare + token, refreshes list immediately).
///
/// Persistence: token via dimmy_notion_set_token (encrypted keystore).
/// Target via dimmy_set_config_json round-trip — same single-writer
/// rule as the rest of Dimmy.
/// </summary>
public sealed partial class NotionConnectDialog : ContentDialog
{
    /// <summary>1, 2, or 3. Set BEFORE ShowAsync to jump past steps.</summary>
    public int InitialStep { get; set; } = 1;

    /// <summary>True iff the wizard finished with a valid token + picked target.</summary>
    public bool Completed { get; private set; }

    /// <summary>Token verified during Step 2 (or already valid on entry). Drives
    /// the summary card update in SettingsWindow after the dialog closes.</summary>
    public bool TokenVerified { get; private set; }

    /// <summary>Workspace name from the last successful ping. Empty if never verified.</summary>
    public string WorkspaceName { get; private set; } = "";

    /// <summary>Destination picked in Step 3. Empty when wizard is cancelled before pick.</summary>
    public string PickedTargetId { get; private set; } = "";
    public string PickedTargetKind { get; private set; } = "";
    public string PickedTargetTitle { get; private set; } = "";

    private int _currentStep = 1;
    private List<NotionService.SearchResult> _results = new();
    // Pre-existing target id (when re-running for "change destination")
    // so we can pre-select it in the ComboBox after Refresh completes.
    private string _existingTargetId = "";

    public NotionConnectDialog()
    {
        InitializeComponent();
        Opened += OnOpened;
    }

    /// <summary>Optional — caller passes the current target id so we can
    /// pre-select it on entry to Step 3. Avoids the user re-picking the
    /// same destination just to confirm.</summary>
    public void SetExistingTarget(string targetId)
    {
        _existingTargetId = targetId ?? "";
    }

    private void OnOpened(ContentDialog sender, ContentDialogOpenedEventArgs args)
    {
        _currentStep = Math.Clamp(InitialStep, 1, 3);
        // If the caller jumps to step 3, the token is assumed valid
        // (it would have been verified in a previous wizard run). Mark
        // it so OnPrimaryClick knows it's allowed to land on Done.
        if (_currentStep == 3)
        {
            TokenVerified = true;
        }
        ApplyStep();
        if (_currentStep == 3)
        {
            // Auto-refresh on entry so the user lands on a populated list.
            _ = RefreshAsync();
        }
    }

    private void ApplyStep()
    {
        Step1Panel.Visibility = _currentStep == 1 ? Visibility.Visible : Visibility.Collapsed;
        Step2Panel.Visibility = _currentStep == 2 ? Visibility.Visible : Visibility.Collapsed;
        Step3Panel.Visibility = _currentStep == 3 ? Visibility.Visible : Visibility.Collapsed;

        var accent = (Brush)Application.Current.Resources["AccentFillColorDefaultBrush"];
        var idle = (Brush)Application.Current.Resources["ControlStrokeColorDefaultBrush"];
        Dot1.Fill = _currentStep >= 1 ? accent : idle;
        Dot2.Fill = _currentStep >= 2 ? accent : idle;
        Dot3.Fill = _currentStep >= 3 ? accent : idle;

        // Per-step button state. SecondaryButton = Back (hidden on
        // step 1). PrimaryButton = Next on 1/2, Done on 3. Primary's
        // enabled state is recomputed by the relevant text-changed /
        // selection-changed handlers; we set the safe default here.
        SecondaryButtonText = _currentStep == 1 ? "" : "Back";
        IsSecondaryButtonEnabled = _currentStep != 1;
        switch (_currentStep)
        {
            case 1:
                PrimaryButtonText = "Next";
                IsPrimaryButtonEnabled = true; // step 1 is purely informational
                break;
            case 2:
                PrimaryButtonText = "Next";
                IsPrimaryButtonEnabled = TokenVerified;
                break;
            case 3:
                PrimaryButtonText = "Done";
                IsPrimaryButtonEnabled = !string.IsNullOrEmpty(PickedTargetId);
                break;
        }
    }

    // ── Button handlers — ContentDialog buttons ─────────────────

    private void OnPrimaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        if (_currentStep < 3)
        {
            // Defer close; advance to next step instead.
            args.Cancel = true;
            _currentStep++;
            ApplyStep();
            if (_currentStep == 3 && _results.Count == 0)
            {
                _ = RefreshAsync();
            }
            return;
        }
        // Step 3 — Done. Validation already gates Primary; if we got
        // here, PickedTargetId is set. Mark Completed so the caller
        // knows to refresh the summary card + persist via FFI.
        if (string.IsNullOrEmpty(PickedTargetId))
        {
            args.Cancel = true;
            return;
        }
        Completed = true;
    }

    private void OnSecondaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        // Back — never closes the dialog. ContentDialog default would
        // close on any button click; cancel + step back instead.
        args.Cancel = true;
        if (_currentStep > 1)
        {
            _currentStep--;
            ApplyStep();
        }
    }

    private void OnCloseClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        // Cancel — accept default close. Completed stays false so the
        // SettingsWindow caller knows not to celebrate.
    }

    // ── Step 1 ────────────────────────────────────────────────────

    private async void OpenIntegrations_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            await global::Windows.System.Launcher.LaunchUriAsync(
                new Uri("https://www.notion.so/my-integrations"));
        }
        catch (Exception ex)
        {
            App.Log($"NotionWizard OpenIntegrations exc: {ex}", "Notion");
        }
    }

    // ── Step 2 ────────────────────────────────────────────────────

    private void TokenBox_Changed(object sender, RoutedEventArgs e)
    {
        var token = TokenBox.Password?.Trim() ?? "";
        // Editing the token field invalidates any prior verification —
        // user must re-Verify before Next becomes available again.
        VerifyBtn.IsEnabled = token.Length > 0;
        if (TokenVerified)
        {
            TokenVerified = false;
            VerifyOkGlyph.Visibility = Visibility.Collapsed;
            VerifyStatus.Text = "";
            IsPrimaryButtonEnabled = false;
        }
    }

    private async void VerifyToken_Click(object sender, RoutedEventArgs e)
    {
        var token = TokenBox.Password?.Trim() ?? "";
        if (string.IsNullOrEmpty(token))
        {
            VerifyStatus.Text = "Paste your token first.";
            return;
        }
        var setRc = NotionService.SetToken(token);
        if (setRc != 0)
        {
            VerifyStatus.Text = "Couldn't save the token to local storage.";
            return;
        }
        VerifyBtn.IsEnabled = false;
        VerifyRing.IsActive = true;
        VerifyRing.Visibility = Visibility.Visible;
        VerifyOkGlyph.Visibility = Visibility.Collapsed;
        VerifyStatus.Text = "Pinging Notion…";
        try
        {
            var result = await NotionService.TestConnectionAsync();
            App.Log($"NotionWizard verify result ok={result.Ok} ws='{result.WorkspaceName}' err='{result.Error ?? ""}'", "Notion");
            if (result.Ok)
            {
                TokenVerified = true;
                WorkspaceName = result.WorkspaceName;
                VerifyOkGlyph.Visibility = Visibility.Visible;
                VerifyStatus.Text = $"Connected as “{result.BotName}” in “{result.WorkspaceName}”.";
                // Token is good — let Next proceed. We deliberately do
                // NOT overwrite TokenBox.Password to mask: the
                // PasswordBox already visually obscures the text with
                // bullets, and assigning Password fires PasswordChanged
                // → TokenBox_Changed which would race-reset
                // TokenVerified back to false (and disable Next) the
                // moment we just set it true. Burned 2026-05-10.
                IsPrimaryButtonEnabled = true;
            }
            else
            {
                TokenVerified = false;
                VerifyStatus.Text = $"Failed: {result.Error}";
                IsPrimaryButtonEnabled = false;
            }
        }
        catch (Exception ex)
        {
            App.Log($"NotionWizard verify exc: {ex}", "Notion");
            VerifyStatus.Text = $"Error: {ex.Message}";
        }
        finally
        {
            VerifyBtn.IsEnabled = true;
            VerifyRing.IsActive = false;
            VerifyRing.Visibility = Visibility.Collapsed;
        }
    }

    // ── Step 3 ────────────────────────────────────────────────────

    private async void RefreshList_Click(object sender, RoutedEventArgs e)
    {
        await RefreshAsync();
    }

    private async System.Threading.Tasks.Task RefreshAsync()
    {
        RefreshBtn.IsEnabled = false;
        RefreshRing.IsActive = true;
        RefreshRing.Visibility = Visibility.Visible;
        RefreshStatus.Text = "Loading…";
        try
        {
            var (results, error) = await NotionService.SearchAsync("");
            if (error != null)
            {
                RefreshStatus.Text = $"Couldn't load list: {error}";
                return;
            }
            _results = new List<NotionService.SearchResult>(results);
            TargetCombo.Items.Clear();
            foreach (var r in _results)
            {
                var label = string.IsNullOrEmpty(r.Title) ? "(untitled)" : r.Title;
                var kind = r.Object == "database" ? " — database"
                         : (r.Object == "page" ? " — page" : "");
                TargetCombo.Items.Add(new ComboBoxItem
                {
                    Content = $"{label}{kind}",
                    Tag = r.Id,
                });
            }
            // Pre-select the existing target if we have one and it's in
            // the refreshed list — saves the user a click on re-runs.
            if (!string.IsNullOrEmpty(_existingTargetId))
            {
                for (int i = 0; i < TargetCombo.Items.Count; i++)
                {
                    if (TargetCombo.Items[i] is ComboBoxItem cbi
                        && cbi.Tag is string id && id == _existingTargetId)
                    {
                        TargetCombo.SelectedIndex = i;
                        break;
                    }
                }
            }
            if (_results.Count == 0)
            {
                RefreshStatus.Text =
                    "Nothing visible yet. Open a Notion page → ··· → Connections → add Dimmy. Then Refresh again.";
            }
            else
            {
                RefreshStatus.Text = $"Found {_results.Count} item(s).";
            }
        }
        catch (Exception ex)
        {
            App.Log($"NotionWizard refresh exc: {ex}", "Notion");
            RefreshStatus.Text = $"Error: {ex.Message}";
        }
        finally
        {
            RefreshBtn.IsEnabled = true;
            RefreshRing.IsActive = false;
            RefreshRing.Visibility = Visibility.Collapsed;
        }
    }

    private void TargetCombo_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (TargetCombo.SelectedItem is not ComboBoxItem cbi) return;
        if (cbi.Tag is not string id) return;
        var picked = _results.Find(r => r.Id == id);
        if (picked == null) return;
        PickedTargetId = picked.Id;
        PickedTargetKind = picked.Object == "database" ? "database" : "page";
        PickedTargetTitle = picked.Title;
        // Enable Done as soon as a destination is picked.
        if (_currentStep == 3)
            IsPrimaryButtonEnabled = true;
    }
}
