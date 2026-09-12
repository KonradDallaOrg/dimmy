using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;

namespace Dimmy.Windows.Views;

/// <summary>
/// Integrations → Google (Gemini CLI) card. Third sibling of the Anthropic
/// and Codex cards, split into its own file the way the Confluence one is:
/// SettingsWindow.xaml.cs is already long, and a self-contained backend
/// belongs next to itself.
///
/// <para>Two things differ from the Codex card, and both come from the CLI
/// rather than from taste:</para>
/// <list type="bullet">
/// <item>The ping can fail with the process exiting <b>successfully</b>.
/// Gemini reports quota and model errors in its JSON envelope, so there are
/// two extra result codes and a message worth showing verbatim.</item>
/// <item>There is no <c>gemini login</c> subcommand. A first run with no
/// credentials shows the auth picker itself, which is what "Sign in" spawns.
/// </item>
/// </list>
/// </summary>
public sealed partial class SettingsWindow
{
    /// <summary>Guards the Toggled handler while the switch is being set to
    /// match what is already saved.</summary>
    private bool _geminiSuppressToggle;

    /// <summary>Whether the user has declared a work account. Read straight
    /// from the config rather than cached: Settings can be reopened after the
    /// wizard, and a stale copy here would show the wrong half of the card.
    /// </summary>
    private static bool GeminiGateOpen()
    {
        try
        {
            var buf = new byte[1 << 16];
            int n = Interop.DimmyNative.dimmy_get_config_json(buf, buf.Length);
            if (n <= 0) return false;
            using var doc = System.Text.Json.JsonDocument.Parse(
                System.Text.Encoding.UTF8.GetString(buf, 0, n));
            return doc.RootElement.TryGetProperty("gemini_cli_enabled", out var v)
                   && v.GetBoolean();
        }
        catch { return false; }
    }

    private void GeminiEnabled_Toggled(object sender, RoutedEventArgs e)
    {
        if (_geminiSuppressToggle || GeminiEnabledToggle == null) return;
        try
        {
            // Only this field: a full ToJson round-trip here would drag every
            // other setting through the if-empty-omit rules for no reason.
            var payload = System.Text.Json.JsonSerializer.Serialize(new
            {
                gemini_cli_enabled = GeminiEnabledToggle.IsOn,
            });
            Interop.DimmyNative.dimmy_set_config_json(payload);
            App.Instance?.ReloadConfig();
            App.Log($"gemini gate -> {GeminiEnabledToggle.IsOn}", "Gemini");
        }
        catch (Exception ex)
        {
            App.Log($"Gemini gate exc: {ex.Message}", "Gemini");
        }
        RefreshGeminiIntegrationStatus();
    }

    private void RefreshGeminiIntegrationStatus()
    {
        // Mirror the saved value WITHOUT going through Toggled: assigning
        // IsOn raises it, and that would write config back on every refresh,
        // including the one that runs while Settings opens.
        bool gateOpen = GeminiGateOpen();
        if (GeminiEnabledToggle != null)
        {
            _geminiSuppressToggle = true;
            GeminiEnabledToggle.IsOn = gateOpen;
            _geminiSuppressToggle = false;
        }
        if (GeminiGateNote != null)
            GeminiGateNote.Visibility = gateOpen ? Visibility.Collapsed : Visibility.Visible;

        if (!gateOpen)
        {
            // Gate shut: say what it is and stop. No install button, no
            // sign-in, nothing that can walk a personal account into a
            // refusal ten minutes from now.
            GeminiIntegrationStatusText.Text =
                "Off. Available for Gemini Code Assist work accounts.";
            GeminiIntegrationStatusGlyph.Glyph = ""; // dash
            GeminiIntegrationStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)
                Application.Current.Resources["TextFillColorTertiaryBrush"];
            GeminiIntegrationDisconnectedActions.Visibility = Visibility.Collapsed;
            GeminiIntegrationConnectedActions.Visibility = Visibility.Collapsed;
            GeminiIntegrationMessageBar.Visibility = Visibility.Collapsed;
            return;
        }

        var status = Interop.DimmyNative.GetGeminiCliStatus();
        switch (status)
        {
            case Interop.DimmyNative.ClaudeCodeStatus.Ready:
                var binaryPath = Interop.DimmyNative.GetGeminiCliBinaryPath() ?? "";
                var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
                var shownPath = (!string.IsNullOrEmpty(home)
                                 && binaryPath.StartsWith(home, StringComparison.OrdinalIgnoreCase))
                    ? "~" + binaryPath[home.Length..]
                    : binaryPath;
                GeminiIntegrationStatusText.Text =
                    $"Connected — using `{shownPath}`. Available for LLM rewrite and meeting recap.";
                GeminiIntegrationStatusGlyph.Glyph = ""; // CheckMark
                GeminiIntegrationStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)
                    Application.Current.Resources["SystemFillColorSuccessBrush"];
                GeminiIntegrationDisconnectedActions.Visibility = Visibility.Collapsed;
                GeminiIntegrationConnectedActions.Visibility = Visibility.Visible;
                GeminiIntegrationTestBtn.IsEnabled = true;
                GeminiIntegrationMessageBar.Visibility = Visibility.Collapsed;
                break;

            case Interop.DimmyNative.ClaudeCodeStatus.NotLoggedIn:
                GeminiIntegrationStatusText.Text =
                    "Gemini CLI installed but not signed in. Click Sign in to authenticate with Google.";
                GeminiIntegrationStatusGlyph.Glyph = ""; // Info
                GeminiIntegrationStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)
                    Application.Current.Resources["TextFillColorTertiaryBrush"];
                GeminiIntegrationDisconnectedActions.Visibility = Visibility.Visible;
                GeminiIntegrationConnectedActions.Visibility = Visibility.Collapsed;
                GeminiIntegrationWizardBtn.Style =
                    (Style)Application.Current.Resources["DefaultButtonStyle"];
                GeminiIntegrationSignInBtn.Style =
                    (Style)Application.Current.Resources["AccentButtonStyle"];
                GeminiIntegrationSignInBtn.IsEnabled = true;
                GeminiIntegrationMessageBar.Visibility = Visibility.Collapsed;
                break;

            case Interop.DimmyNative.ClaudeCodeStatus.NotInstalled:
            default:
                GeminiIntegrationStatusText.Text =
                    "Gemini CLI not detected. Click Set up wizard for a guided install + sign-in.";
                GeminiIntegrationStatusGlyph.Glyph = ""; // Info
                GeminiIntegrationStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)
                    Application.Current.Resources["TextFillColorTertiaryBrush"];
                GeminiIntegrationDisconnectedActions.Visibility = Visibility.Visible;
                GeminiIntegrationConnectedActions.Visibility = Visibility.Collapsed;
                // Binary missing — the wizard is the right entry point.
                // Promote it; disable the bare Sign in (it would just fail).
                GeminiIntegrationWizardBtn.Style =
                    (Style)Application.Current.Resources["AccentButtonStyle"];
                GeminiIntegrationSignInBtn.Style =
                    (Style)Application.Current.Resources["DefaultButtonStyle"];
                GeminiIntegrationSignInBtn.IsEnabled = false;
                GeminiIntegrationMessageText.Text =
                    "Install it with `npm install -g @google/gemini-cli` (needs Node 20+), then click Refresh, or just use the wizard.";
                GeminiIntegrationMessageBar.Visibility = Visibility.Visible;
                break;
        }
    }

    private async void GeminiIntegrationWizard_Click(object sender, RoutedEventArgs e)
        => await ShowGeminiWizardAsync(forceStep1: false);

    /// <summary>"Re-run setup" from the connected state — forces the wizard
    /// to start at step 1 so the user can re-inspect or reinstall.</summary>
    private async void GeminiIntegrationRerunWizard_Click(object sender, RoutedEventArgs e)
        => await ShowGeminiWizardAsync(forceStep1: true);

    private async Task ShowGeminiWizardAsync(bool forceStep1)
    {
        try
        {
            var dialog = new GeminiConnectDialog
            {
                XamlRoot = this.Content.XamlRoot,
                RequestedTheme = Helpers.ThemeHelper.ResolvedElementTheme(),
                ForceStartAtStep1 = forceStep1,
            };
            await dialog.ShowAsync();
            Interop.DimmyNative.RecheckGeminiCli();
            RefreshGeminiIntegrationStatus();
            RefreshAuthIntegrationStatus();
            if (dialog.Completed)
                Interop.DimmyNative.TrackEvent("gemini_cli.wizard_completed");
        }
        catch (Exception ex)
        {
            App.Log($"Gemini wizard launch exc: {ex}", "Gemini");
        }
    }

    private async void GeminiIntegrationSignIn_Click(object sender, RoutedEventArgs e)
    {
        GeminiIntegrationSignInBtn.IsEnabled = false;
        GeminiIntegrationStatusText.Text =
            "Launching the Gemini CLI — pick 'Login with Google' in the new terminal window.";
        try
        {
            var ok = Interop.DimmyNative.SpawnGeminiCliLogin();
            if (!ok)
            {
                GeminiIntegrationStatusText.Text =
                    "Could not start the Gemini CLI. Open a terminal and run `gemini` manually.";
                Interop.DimmyNative.TrackEvent("gemini_cli.login_completed", new { outcome = "spawn_failed" });
                return;
            }
            // Poll rather than wait on the process: the CLI stays open after
            // the browser flow completes (it drops into its own prompt), so
            // its exit is not the signal we want — the credentials file is.
            for (int i = 0; i < 90; i++)
            {
                await Task.Delay(2000);
                if (Interop.DimmyNative.RecheckGeminiCli() == Interop.DimmyNative.ClaudeCodeStatus.Ready)
                {
                    Interop.DimmyNative.TrackEvent("gemini_cli.login_completed", new { outcome = "success" });
                    RefreshGeminiIntegrationStatus();
                    RefreshAuthIntegrationStatus();
                    return;
                }
            }
            GeminiIntegrationStatusText.Text =
                "Sign-in not completed in 3 minutes. Click Refresh when ready.";
            Interop.DimmyNative.TrackEvent("gemini_cli.login_completed", new { outcome = "timeout" });
        }
        catch (Exception ex)
        {
            GeminiIntegrationStatusText.Text = $"Sign-in error: {ex.Message}";
            App.Log($"GeminiIntegration sign-in exc: {ex}", "Gemini");
            Interop.DimmyNative.TrackEvent("gemini_cli.login_completed", new { outcome = "spawn_failed" });
        }
        finally
        {
            GeminiIntegrationSignInBtn.IsEnabled = true;
        }
    }

    private async void GeminiIntegrationTest_Click(object sender, RoutedEventArgs e)
    {
        GeminiIntegrationTestBtn.IsEnabled = false;
        GeminiIntegrationStatusText.Text = "Sending ping…";
        try
        {
            var (result, elapsedMs) = await Task.Run(() => Interop.DimmyNative.PingGeminiCli());
            GeminiIntegrationStatusText.Text = result switch
            {
                Interop.DimmyNative.ClaudeCodePingResult.Ok =>
                    $"✓ Connection OK — {elapsedMs} ms round-trip via the local `gemini` CLI.",
                Interop.DimmyNative.ClaudeCodePingResult.NotInstalled =>
                    "✗ `gemini` binary not found. Install the Gemini CLI first.",
                Interop.DimmyNative.ClaudeCodePingResult.NotLoggedIn =>
                    "✗ Not signed in. Click Sign in to authenticate with Google.",
                Interop.DimmyNative.ClaudeCodePingResult.SpawnFailed =>
                    "✗ Could not spawn the CLI. See dimmy.log.",
                Interop.DimmyNative.ClaudeCodePingResult.Timeout =>
                    "✗ Timed out after 60 s — network or rate-limit issue.",
                Interop.DimmyNative.ClaudeCodePingResult.NonZeroExit =>
                    "✗ `gemini` returned a non-zero exit code. See dimmy.log.",
                Interop.DimmyNative.ClaudeCodePingResult.InvalidUtf8 =>
                    "✗ Unexpected output from `gemini`. See dimmy.log.",
                // The CLI answered and said no. Its own words, because the
                // usual cause is "you have used today's requests" and that
                // is advice, not an error code.
                Interop.DimmyNative.ClaudeCodePingResult.Reported =>
                    $"✗ {FirstNonEmpty(Interop.DimmyNative.GetGeminiCliLastError(), "Gemini refused the request.")}",
                Interop.DimmyNative.ClaudeCodePingResult.EmptyResponse =>
                    "✗ Gemini answered with nothing. Try again, or check your quota.",
                _ => "✗ Unknown error. See dimmy.log.",
            };
        }
        catch (Exception ex)
        {
            GeminiIntegrationStatusText.Text = $"Test error: {ex.Message}";
            App.Log($"GeminiIntegration test exc: {ex}", "Gemini");
        }
        finally
        {
            GeminiIntegrationTestBtn.IsEnabled = true;
        }
    }

    private static string FirstNonEmpty(string a, string fallback) =>
        string.IsNullOrWhiteSpace(a) ? fallback : a;

    private void GeminiIntegrationRefresh_Click(object sender, RoutedEventArgs e)
    {
        Interop.DimmyNative.RecheckGeminiCli();
        RefreshGeminiIntegrationStatus();
        // The Output subscription toggles depend on Gemini readiness — keep
        // them in sync after a recheck.
        RefreshAuthIntegrationStatus();
    }
}
