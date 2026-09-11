using System;
using System.Collections.Generic;
using System.Text;
using System.Text.Json;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Views;

/// <summary>
/// Two-step Confluence connection wizard.
///
/// Deliberately one step shorter than <see cref="NotionConnectDialog"/>. Notion
/// spends a whole screen teaching the user to create an integration and share a
/// page with it; an Atlassian API token needs no such setup and already carries
/// the user's own permissions, so "prepare" and "paste" are one screen here.
///
///   Step 1 — site, email, token. Next stays disabled until Check has actually
///            reached the API. A wizard that lets a bad credential through only
///            moves the failure to the first recap, where it is far less
///            obvious what went wrong.
///   Step 2 — destination space, personal preselected, plus the auto-send
///            choice. One decision, already defaulted.
///
/// Persistence follows the house rule: the token goes to the encrypted keystore
/// via its own FFI entry, everything else through dimmy_set_config_json so the
/// Rust core stays the only writer of config.json.
/// </summary>
public sealed partial class ConfluenceConnectDialog : ContentDialog
{
    /// <summary>1 = full setup (default), 2 = "change destination" — credentials
    /// are already known good, jump straight to the picker.</summary>
    public int InitialStep { get; set; } = 1;

    /// <summary>True when the wizard finished with a verified link and a space.</summary>
    public bool Completed { get; private set; }

    /// <summary>Display name from the last successful check, for the Settings card.</summary>
    public string AccountName { get; private set; } = "";

    private sealed record SpaceRow(string Id, string Key, string Name, string Kind, bool IsMine)
    {
        /// Only the space the CORE identified as ours may say so. Deriving
        /// it from Kind labelled all 530 of a tenant's personal spaces
        /// "your space", which is worse than saying nothing.
        public override string ToString() =>
            IsMine ? $"{Name} (your space)"
            : Kind == "personal" ? $"{Name} (personal)"
            : $"{Name} ({Key})";
    }

    private int _step = 1;
    private bool _verified;
    private string _site = "";
    private string _email = "";
    private string _token = "";
    private readonly List<SpaceRow> _spaces = new();
    /// <summary>True while the dialog fills its own fields.
    ///
    /// Prefilling raises TextChanged, and WinUI delivers it AFTER Opened has
    /// returned — by which point _verified is already true, so
    /// OnCredentialChanged read the prefill as a user edit, invalidated the
    /// check and disabled Done. Entering at step 2 from "Change space" then
    /// looked like a dead button that only Back → Check could revive.</summary>
    private bool _prefilling;

    public ConfluenceConnectDialog()
    {
        InitializeComponent();
        Opened += OnOpened;
    }

    private void OnOpened(ContentDialog sender, ContentDialogOpenedEventArgs args)
    {
        // Prefill from whatever is already configured so a re-run is a
        // confirmation, not a re-typing exercise. The token is never read back
        // out of the keystore — an empty box with a stored token is the normal
        // state, and Check treats empty as "use the saved one".
        _prefilling = true;
        try
        {
            var cfg = ReadConfig();
            // Into the FIELDS as well as the boxes: entering at step 2 skips
            // Verify_Click, which is the only other place these are set, and
            // Save would then have written empty strings over a working site
            // and email.
            _site = cfg.GetValueOrDefault("confluence_site", "");
            _email = cfg.GetValueOrDefault("confluence_email", "");
            SiteBox.Text = _site;
            EmailBox.Text = _email;
        }
        catch (Exception ex) { App.Log($"Confluence prefill: {ex.Message}", "Confluence"); }

        // A stored token is never read back out of the keystore, so the box
        // starts empty even when one exists. Say so: the Atlassian page shows a
        // token exactly once, so "paste it again" is advice a user re-running
        // this cannot follow, and an empty field with no explanation reads as
        // "it forgot my token".
        bool hasToken = DimmyNative.dimmy_confluence_has_token() == 1;
        if (hasToken)
        {
            TokenBox.Header = "API token — already saved";
            TokenBox.PlaceholderText = "Leave empty to keep it, or paste a new one to replace it";
        }

        if (InitialStep >= 2 && hasToken)
        {
            _verified = true;
            GoToStep(2);
        }
        else
        {
            GoToStep(1);
        }

        // Released on the dispatcher, not here: the TextChanged raised by the
        // prefill above is still queued and would otherwise land after the
        // flag was already down.
        DispatcherQueue.TryEnqueue(() => _prefilling = false);
    }

    // ── Step machine ────────────────────────────────────────────────

    private void GoToStep(int step)
    {
        _step = step;
        Step1Panel.Visibility = step == 1 ? Visibility.Visible : Visibility.Collapsed;
        Step2Panel.Visibility = step == 2 ? Visibility.Visible : Visibility.Collapsed;

        var on = (SolidColorBrush)Application.Current.Resources["AccentFillColorDefaultBrush"];
        var off = (SolidColorBrush)Application.Current.Resources["ControlStrokeColorDefaultBrush"];
        Dot1.Fill = on;
        Dot2.Fill = step >= 2 ? on : off;

        SecondaryButtonText = step == 1 ? "" : "Back";
        PrimaryButtonText = step == 1 ? "Next" : "Done";
        UpdatePrimaryEnabled();

        if (step == 2) _ = LoadSpacesAsync();
    }

    /// <summary>Recompute whether the primary button may be pressed.
    ///
    /// Derived rather than assigned at each transition, because assigning it
    /// once in GoToStep lost the race with whatever raised a field event
    /// afterwards: entering step 2 from "Change space" showed Done greyed out
    /// with a space plainly selected. Recomputing from the model on every
    /// event means no ordering can leave it wrong — and the rule is now the
    /// honest one: step 1 needs a checked credential, step 2 needs a space.</summary>
    private void UpdatePrimaryEnabled()
    {
        bool allowed = _step == 1 ? _verified : SpaceCombo?.SelectedItem != null;
        IsPrimaryButtonEnabled = allowed;
    }

    private void OnSpaceSelectionChanged(object sender, SelectionChangedEventArgs e)
        => UpdatePrimaryEnabled();

    private void OnPrimaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        if (_step == 1)
        {
            // Keep the dialog open and advance instead of closing on Primary.
            args.Cancel = true;
            GoToStep(2);
            return;
        }
        if (!Save()) args.Cancel = true;
    }

    private void OnSecondaryClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        args.Cancel = true;
        GoToStep(1);
    }

    private void OnCloseClick(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        Completed = false;
    }

    // ── Step 1 ──────────────────────────────────────────────────────

    /// <summary>Any edit invalidates the previous check: the credentials on
    /// screen are no longer the ones that were proven to work.</summary>
    private void OnCredentialChanged(object sender, RoutedEventArgs e)
    {
        if (_prefilling || !_verified) return;
        _verified = false;
        VerifyStatus.Text = "";
        UpdatePrimaryEnabled();
    }

    private void OpenTokenPage_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
            {
                FileName = "https://id.atlassian.com/manage-profile/security/api-tokens",
                UseShellExecute = true,
            });
        }
        catch (Exception ex) { App.Log($"open token page: {ex.Message}", "Confluence"); }
    }

    private async void Verify_Click(object sender, RoutedEventArgs e)
    {
        _site = SiteBox.Text.Trim();
        _email = EmailBox.Text.Trim();
        _token = TokenBox.Password;

        if (_site.Length == 0 || _email.Length == 0)
        {
            SetStatus(VerifyStatus, false, "Site and email are both needed.");
            return;
        }

        VerifyBtn.IsEnabled = false;
        VerifyRing.IsActive = true;
        VerifyRing.Visibility = Visibility.Visible;
        VerifyStatus.Text = "";

        var (ok, message, account) = await Task.Run(() =>
        {
            var buf = new byte[8192];
            int n = DimmyNative.dimmy_confluence_test_connection(_site, _email, _token, buf, buf.Length);
            if (n <= 0) return (false, "Could not reach the core.", "");
            return ParseEnvelope(Encoding.UTF8.GetString(buf, 0, n), "account");
        });

        VerifyRing.IsActive = false;
        VerifyRing.Visibility = Visibility.Collapsed;
        VerifyBtn.IsEnabled = true;
        _verified = ok;
        UpdatePrimaryEnabled();
        AccountName = account;
        SetStatus(VerifyStatus, ok, ok ? $"Connected as {account}" : message);

        // Save the token as soon as it is proven, so a user who closes the
        // wizard here and comes back does not have to fetch it again.
        if (ok && _token.Length > 0)
        {
            try { DimmyNative.dimmy_confluence_set_token(_token); }
            catch (Exception ex) { App.Log($"set token: {ex.Message}", "Confluence"); }
        }
    }

    // ── Step 2 ──────────────────────────────────────────────────────

    private async Task LoadSpacesAsync()
    {
        SpaceCombo.ItemsSource = null;
        SpaceCombo.PlaceholderText = "Loading spaces...";
        SpaceStatus.Text = "";

        var (json, ok) = await Task.Run(() =>
        {
            var buf = new byte[1 << 18];
            int n = DimmyNative.dimmy_confluence_spaces(_site, _email, buf, buf.Length);
            return n <= 0 ? ("", false) : (Encoding.UTF8.GetString(buf, 0, n), true);
        });

        _spaces.Clear();
        if (ok)
        {
            try
            {
                using var doc = JsonDocument.Parse(json);
                var root = doc.RootElement;
                if (root.TryGetProperty("ok", out var okEl) && okEl.GetBoolean()
                    && root.TryGetProperty("spaces", out var arr))
                {
                    foreach (var s in arr.EnumerateArray())
                    {
                        _spaces.Add(new SpaceRow(
                            s.GetProperty("id").GetString() ?? "",
                            s.GetProperty("key").GetString() ?? "",
                            s.GetProperty("name").GetString() ?? "",
                            s.GetProperty("kind").GetString() ?? "",
                            s.TryGetProperty("is_mine", out var m) && m.GetBoolean()));
                    }
                }
                else if (root.TryGetProperty("error", out var err))
                {
                    SetStatus(SpaceStatus, false, err.GetString() ?? "Could not list spaces.");
                }
            }
            catch (Exception ex) { App.Log($"spaces parse: {ex.Message}", "Confluence"); }
        }

        ApplySpaceFilter();
        SpaceCombo.PlaceholderText = _spaces.Count == 0 ? "No spaces available" : "Pick a space";

        // Preselect: whatever was configured before, else the personal space —
        // the core sorts those first, so index 0 is it when one exists.
        // Preselect what was configured before, else the space the core
        // positively identified as ours. Never "the first personal one":
        // on a big tenant that is a colleague.
        var existing = ReadConfig().GetValueOrDefault("confluence_space_id", "");
        var shown = (List<SpaceRow>)SpaceCombo.ItemsSource;
        int idx = shown.FindIndex(s => s.Id == existing);
        if (idx < 0) idx = shown.FindIndex(s => s.IsMine);
        if (idx >= 0) SpaceCombo.SelectedIndex = idx;
        UpdatePrimaryEnabled();
    }

    private void OnSpaceFilterChanged(object sender, TextChangedEventArgs e)
        => ApplySpaceFilter();

    /// <summary>Narrow the dropdown to what the user typed, matching name or
    /// key. Keeps the current pick if it still matches, so typing does not
    /// silently change the destination under the user.</summary>
    private void ApplySpaceFilter()
    {
        var q = (SpaceFilterBox?.Text ?? "").Trim();
        var selected = SpaceCombo.SelectedItem as SpaceRow;
        var shown = q.Length == 0
            ? new List<SpaceRow>(_spaces)
            : _spaces.FindAll(s =>
                s.Name.Contains(q, StringComparison.OrdinalIgnoreCase)
                || s.Key.Contains(q, StringComparison.OrdinalIgnoreCase));
        SpaceCombo.ItemsSource = shown;
        if (selected != null)
        {
            int keep = shown.FindIndex(s => s.Id == selected.Id);
            if (keep >= 0) SpaceCombo.SelectedIndex = keep;
        }
        UpdatePrimaryEnabled();
    }

    private bool Save()
    {
        if (SpaceCombo.SelectedItem is not SpaceRow picked)
        {
            SetStatus(SpaceStatus, false, "Pick a space first.");
            return false;
        }
        try
        {
            // Never write these away: empty here means the step-1 fields were
            // never visited, not that the user cleared them.
            var cfg = ReadConfig();
            var site = _site.Length > 0 ? _site : cfg.GetValueOrDefault("confluence_site", "");
            var email = _email.Length > 0 ? _email : cfg.GetValueOrDefault("confluence_email", "");
            var payload = JsonSerializer.Serialize(new
            {
                confluence_site = site,
                confluence_email = email,
                confluence_space_id = picked.Id,
                confluence_space_key = picked.Key,
                confluence_space_name = picked.Name,
            });
            DimmyNative.dimmy_set_config_json(payload);
            App.Instance?.ReloadConfig();
            Completed = true;
            return true;
        }
        catch (Exception ex)
        {
            App.Log($"Confluence save: {ex.Message}", "Confluence");
            SetStatus(SpaceStatus, false, "Could not save the destination.");
            return false;
        }
    }

    // ── Helpers ─────────────────────────────────────────────────────

    private static (bool ok, string message, string extra) ParseEnvelope(string json, string extraKey)
    {
        try
        {
            using var doc = JsonDocument.Parse(json);
            var root = doc.RootElement;
            bool ok = root.TryGetProperty("ok", out var okEl) && okEl.GetBoolean();
            string extra = root.TryGetProperty(extraKey, out var x) ? x.GetString() ?? "" : "";
            string msg = root.TryGetProperty("error", out var e) ? e.GetString() ?? "" : "";
            return (ok, msg, extra);
        }
        catch { return (false, "Unexpected reply from the core.", ""); }
    }

    private static Dictionary<string, string> ReadConfig()
    {
        var map = new Dictionary<string, string>();
        try
        {
            var buf = new byte[1 << 16];
            int n = DimmyNative.dimmy_get_config_json(buf, buf.Length);
            if (n <= 0) return map;
            using var doc = JsonDocument.Parse(Encoding.UTF8.GetString(buf, 0, n));
            foreach (var prop in doc.RootElement.EnumerateObject())
            {
                map[prop.Name] = prop.Value.ValueKind switch
                {
                    JsonValueKind.String => prop.Value.GetString() ?? "",
                    JsonValueKind.True => "True",
                    JsonValueKind.False => "False",
                    _ => prop.Value.ToString(),
                };
            }
        }
        catch { }
        return map;
    }

    private static void SetStatus(TextBlock target, bool ok, string text)
    {
        target.Text = text;
        target.Foreground = new SolidColorBrush(
            ok ? Microsoft.UI.Colors.SeaGreen : Microsoft.UI.Colors.IndianRed);
    }
}
