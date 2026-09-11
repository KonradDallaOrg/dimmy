using System;
using System.Text;
using System.Text.Json;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Views;

/// <summary>
/// Confluence integration section of Settings, split out the way the Telegram
/// one is: the wizard owns the setup, this file only paints the card and
/// forwards the three actions.
///
/// Deliberately NOT folded into the Notion card as a "destination" dropdown.
/// The two are not alternatives — a personal Notion and a team wiki can both
/// be on, and a user moving from one to the other wants to see the old one
/// still connected while they try the new one.
/// </summary>
public sealed partial class SettingsWindow
{
    private void ConfluenceRefreshSummary()
    {
        try
        {
            bool connected = DimmyNative.dimmy_confluence_has_token() == 1;
            var cfg = ConfluenceReadConfig();
            // Three states, not two: a token with no space picked is connected
            // but NOT ready, and a tick there would promise sending works when
            // the first recap would fail.
            bool ready = connected && cfg.spaceName.Length > 0;

            if (ConfluenceStatusText != null)
            {
                ConfluenceStatusText.Text = !connected
                    ? "Not connected."
                    : ready
                        ? $"Recaps land in {cfg.spaceName} on {cfg.site}."
                        : $"Connected to {cfg.site}. Pick a space to start sending.";
            }
            if (ConfluenceDisconnectedActions != null)
                ConfluenceDisconnectedActions.Visibility =
                    connected ? Visibility.Collapsed : Visibility.Visible;
            if (ConfluenceConnectedActions != null)
                ConfluenceConnectedActions.Visibility =
                    connected ? Visibility.Visible : Visibility.Collapsed;

            if (ConfluenceStatusGlyph != null)
            {
                ConfluenceStatusGlyph.Glyph = !connected
                    ? "\uEA39"      // dash: nothing configured
                    : ready
                        ? "\uE73E"  // tick
                        : "\uE7BA"; // warning: half configured
                ConfluenceStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)
                    Application.Current.Resources[!connected
                        ? "TextFillColorTertiaryBrush"
                        : ready
                            ? "SystemFillColorSuccessBrush"
                            : "SystemFillColorCautionBrush"];
            }

            // Mirror the saved value WITHOUT going through Toggled: assigning
            // IsOn raises it, and that would write config back on every
            // refresh, including the one that runs while Settings opens.
            if (ConfluenceAutoSendToggle != null)
            {
                _confluenceSuppressToggle = true;
                ConfluenceAutoSendToggle.IsOn = cfg.autoSend;
                ConfluenceAutoSendToggle.IsEnabled = connected;
                _confluenceSuppressToggle = false;
            }
        }
        catch (Exception ex)
        {
            App.Log($"Confluence summary exc: {ex.Message}", "Confluence");
        }
    }

    /// <summary>Guards the Toggled handler while the switch is being set to
    /// match what is already saved.</summary>
    private bool _confluenceSuppressToggle;

    private void ConfluenceAutoSend_Toggled(object sender, RoutedEventArgs e)
    {
        if (_confluenceSuppressToggle || ConfluenceAutoSendToggle == null) return;
        try
        {
            // Only this field: a full ToJson round-trip here would drag every
            // other setting through the if-empty-omit rules for no reason.
            var payload = JsonSerializer.Serialize(new
            {
                confluence_auto_send = ConfluenceAutoSendToggle.IsOn,
            });
            DimmyNative.dimmy_set_config_json(payload);
            App.Instance?.ReloadConfig();
            App.Log($"confluence auto-send -> {ConfluenceAutoSendToggle.IsOn}", "Confluence");
        }
        catch (Exception ex)
        {
            App.Log($"Confluence auto-send exc: {ex.Message}", "Confluence");
        }
    }

    private static (string site, string spaceName, bool autoSend) ConfluenceReadConfig()
    {
        try
        {
            var buf = new byte[1 << 16];
            int n = DimmyNative.dimmy_get_config_json(buf, buf.Length);
            if (n <= 0) return ("", "", false);
            using var doc = JsonDocument.Parse(Encoding.UTF8.GetString(buf, 0, n));
            var root = doc.RootElement;
            string Get(string k) =>
                root.TryGetProperty(k, out var v) ? v.GetString() ?? "" : "";
            return (Get("confluence_site"), Get("confluence_space_name"),
                    root.TryGetProperty("confluence_auto_send", out var a) && a.GetBoolean());
        }
        catch { return ("", "", false); }
    }

    private async Task RunConfluenceWizardAsync(int initialStep)
    {
        var dialog = new ConfluenceConnectDialog
        {
            InitialStep = initialStep,
            XamlRoot = (this.Content as FrameworkElement)?.XamlRoot,
            RequestedTheme = Dimmy.Windows.Helpers.ThemeHelper.ResolvedElementTheme(),
        };
        await dialog.ShowAsync();
        if (dialog.Completed)
        {
            ConfluenceShowMessage(
                $"Connected as {dialog.AccountName}. Recaps will be sent as new pages.",
                isError: false);
        }
        ConfluenceRefreshSummary();
    }

    private async void ConfluenceConnect_Click(object sender, RoutedEventArgs e)
        => await RunConfluenceWizardAsync(
            DimmyNative.dimmy_confluence_has_token() == 1 ? 2 : 1);

    private async void ConfluenceChangeSpace_Click(object sender, RoutedEventArgs e)
        => await RunConfluenceWizardAsync(initialStep: 2);

    /// <summary>Re-walk BOTH steps. Without this the wizard was reachable only
    /// at step 2 once connected, so replacing an expired or wrong API token
    /// meant disconnecting first — losing the destination on the way.</summary>
    private async void ConfluenceRerunWizard_Click(object sender, RoutedEventArgs e)
        => await RunConfluenceWizardAsync(initialStep: 1);

    /// <summary>Check the SAVED credentials against Confluence right now.
    /// Passing empty strings makes the core fall back to what is stored, which
    /// is the whole point: this answers "does what I saved still work?"</summary>
    private async void ConfluenceTest_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button b) b.IsEnabled = false;
        try
        {
            var (ok, message) = await System.Threading.Tasks.Task.Run(() =>
            {
                var buf = new byte[8192];
                int n = DimmyNative.dimmy_confluence_test_connection("", "", "", buf, buf.Length);
                if (n <= 0) return (false, "Could not reach the core.");
                try
                {
                    using var doc = JsonDocument.Parse(Encoding.UTF8.GetString(buf, 0, n));
                    var root = doc.RootElement;
                    bool k = root.TryGetProperty("ok", out var okEl) && okEl.GetBoolean();
                    string acct = root.TryGetProperty("account", out var a) ? a.GetString() ?? "" : "";
                    string err = root.TryGetProperty("error", out var e2) ? e2.GetString() ?? "" : "";
                    return (k, k ? (acct.Length > 0 ? $"Connected as {acct}." : "Credentials still work.") : err);
                }
                catch { return (false, "Unexpected reply from the core."); }
            });
            ConfluenceShowMessage(message, isError: !ok);
        }
        finally
        {
            if (sender is Button b2) b2.IsEnabled = true;
        }
        ConfluenceRefreshSummary();
    }

    private async void ConfluenceDisconnect_Click(object sender, RoutedEventArgs e)
    {
        var confirm = new ContentDialog
        {
            RequestedTheme = Dimmy.Windows.Helpers.ThemeHelper.ResolvedElementTheme(),
            Title = "Disconnect Confluence?",
            Content = "Dimmy will forget your API token and the destination space. "
                    + "Pages already published stay where they are.",
            PrimaryButtonText = "Disconnect",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
            XamlRoot = (this.Content as FrameworkElement)?.XamlRoot,
        };
        if ((await confirm.ShowAsync()) != ContentDialogResult.Primary) return;

        try
        {
            // Empty token clears it in the keystore; the rest goes through the
            // config round-trip so Rust stays the only writer.
            DimmyNative.dimmy_confluence_set_token("");
            var payload = JsonSerializer.Serialize(new
            {
                confluence_site = "",
                confluence_email = "",
                confluence_space_id = "",
                confluence_space_key = "",
                confluence_space_name = "",
                confluence_parent_id = "",
                confluence_auto_send = false,
            });
            DimmyNative.dimmy_set_config_json(payload);
            App.Instance?.ReloadConfig();
            ConfluenceShowMessage("Disconnected. Token and space removed from this device.",
                                  isError: false);
        }
        catch (Exception ex)
        {
            App.Log($"Confluence disconnect exc: {ex.Message}", "Confluence");
            ConfluenceShowMessage($"Couldn't disconnect: {ex.Message}", isError: true);
        }
        ConfluenceRefreshSummary();
    }

    private void ConfluenceShowMessage(string text, bool isError)
    {
        if (ConfluenceMessageBar == null || ConfluenceMessageText == null) return;
        ConfluenceMessageText.Text = text;
        ConfluenceMessageBar.Background = (Microsoft.UI.Xaml.Media.Brush)
            Application.Current.Resources[isError
                ? "SystemFillColorCriticalBackgroundBrush"
                : "SystemFillColorSuccessBackgroundBrush"];
        ConfluenceMessageBar.Visibility = Visibility.Visible;
    }
}
