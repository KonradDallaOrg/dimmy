using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;

namespace Dimmy.Windows.Views;

/// <summary>
/// Integrations → Kimi (Kimi Code CLI) card. Fourth sibling of the Anthropic,
/// Codex and Gemini cards, in its own file like the Gemini one.
///
/// <para>Kimi needs Git for Windows next to the CLI. The card says so when it
/// is missing; the wizard installs both.</para>
/// </summary>
public sealed partial class SettingsWindow
{
    /// <summary>Signed in, so the Output / Recap "Use Kimi subscription"
    /// toggles can route through the CLI.</summary>
    private static bool KimiSubscriptionUsable() =>
        Interop.DimmyNative.GetKimiStatus() == Interop.DimmyNative.ClaudeCodeStatus.Ready;

    /// <summary>The Kimi Code membership endpoint (global or CN), or the
    /// synthetic CLI scheme. Sibling of IsOpenAIUrl.</summary>
    private static bool IsKimiUrl(string url) =>
        url.Contains("api.kimi.ai", StringComparison.OrdinalIgnoreCase)
        || url.Contains("api.kimi.com", StringComparison.OrdinalIgnoreCase)
        || url.StartsWith("kimi-cli://", StringComparison.Ordinal);

    private void RefreshKimiIntegrationStatus()
    {
        var status = Interop.DimmyNative.GetKimiStatus();
        var gitOk = Interop.DimmyNative.KimiGitBashPresent();
        switch (status)
        {
            case Interop.DimmyNative.ClaudeCodeStatus.Ready:
                var binaryPath = Interop.DimmyNative.GetKimiBinaryPath() ?? "";
                var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
                var shownPath = (!string.IsNullOrEmpty(home)
                                 && binaryPath.StartsWith(home, StringComparison.OrdinalIgnoreCase))
                    ? "~" + binaryPath[home.Length..]
                    : binaryPath;
                KimiIntegrationStatusText.Text =
                    $"Connected — using `{shownPath}`. Available for LLM rewrite and meeting recap.";
                KimiIntegrationStatusGlyph.Glyph = ""; // CheckMark
                KimiIntegrationStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)
                    Application.Current.Resources["SystemFillColorSuccessBrush"];
                KimiIntegrationDisconnectedActions.Visibility = Visibility.Collapsed;
                KimiIntegrationConnectedActions.Visibility = Visibility.Visible;
                KimiIntegrationTestBtn.IsEnabled = true;
                break;

            case Interop.DimmyNative.ClaudeCodeStatus.NotLoggedIn:
                KimiIntegrationStatusText.Text =
                    "Kimi Code CLI installed but not signed in. Click Sign in to use your Kimi account.";
                KimiIntegrationStatusGlyph.Glyph = ""; // Info
                KimiIntegrationStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)
                    Application.Current.Resources["TextFillColorTertiaryBrush"];
                KimiIntegrationDisconnectedActions.Visibility = Visibility.Visible;
                KimiIntegrationConnectedActions.Visibility = Visibility.Collapsed;
                KimiIntegrationWizardBtn.Style =
                    (Style)Application.Current.Resources["DefaultButtonStyle"];
                KimiIntegrationSignInBtn.Style =
                    (Style)Application.Current.Resources["AccentButtonStyle"];
                KimiIntegrationSignInBtn.IsEnabled = true;
                break;

            case Interop.DimmyNative.ClaudeCodeStatus.NotInstalled:
            default:
                KimiIntegrationStatusText.Text =
                    "Kimi Code CLI not detected. Click Set up wizard: it installs everything for you.";
                KimiIntegrationStatusGlyph.Glyph = ""; // Info
                KimiIntegrationStatusGlyph.Foreground = (Microsoft.UI.Xaml.Media.Brush)
                    Application.Current.Resources["TextFillColorTertiaryBrush"];
                KimiIntegrationDisconnectedActions.Visibility = Visibility.Visible;
                KimiIntegrationConnectedActions.Visibility = Visibility.Collapsed;
                KimiIntegrationWizardBtn.Style =
                    (Style)Application.Current.Resources["AccentButtonStyle"];
                KimiIntegrationSignInBtn.Style =
                    (Style)Application.Current.Resources["DefaultButtonStyle"];
                KimiIntegrationSignInBtn.IsEnabled = false;
                break;
        }

        // The CLI starts its shell through Git Bash: without it every call
        // fails at spawn, even when the status above says "ready".
        if (!gitOk)
        {
            KimiIntegrationMessageText.Text =
                "Kimi Code needs Git for Windows. The Set up wizard installs it with one click.";
            KimiIntegrationMessageBar.Visibility = Visibility.Visible;
            KimiIntegrationDisconnectedActions.Visibility = Visibility.Visible;
            KimiIntegrationWizardBtn.Style =
                (Style)Application.Current.Resources["AccentButtonStyle"];
        }
        else
        {
            KimiIntegrationMessageBar.Visibility = Visibility.Collapsed;
        }

        RefreshKimiMcpStatus();
    }

    /// Guards the Toggled handler while the switch mirrors the saved state.
    private bool _kimiMcpSuppressToggle;

    private void RefreshKimiMcpStatus()
    {
        var (found, registered) = Interop.DimmyNative.GetKimiMcpStatus();
        KimiMcpCard.Visibility = found ? Visibility.Visible : Visibility.Collapsed;
        _kimiMcpSuppressToggle = true;
        KimiMcpToggle.IsOn = registered;
        _kimiMcpSuppressToggle = false;
    }

    private void KimiMcpToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_kimiMcpSuppressToggle) return;
        string? error = null;
        if (KimiMcpToggle.IsOn)
        {
            var binary = ClaudeDesktopConnectDialog.ResolveMcpBinaryPath();
            var rc = binary == null ? -2 : Interop.DimmyNative.InstallKimiMcp(binary);
            error = rc switch
            {
                0 => null,
                -2 => "dimmy-mcp.exe was not found next to Dimmy.exe.",
                -3 => "~/.kimi-code/mcp.json is not valid JSON. Dimmy left it untouched; fix it and try again.",
                _ => "Could not write ~/.kimi-code/mcp.json. See dimmy.log.",
            };
        }
        else
        {
            var rc = Interop.DimmyNative.UninstallKimiMcp();
            if (rc < 0) error = "Could not update ~/.kimi-code/mcp.json. See dimmy.log.";
        }
        App.Log($"kimi mcp -> {KimiMcpToggle.IsOn} error={error != null}", "Kimi");
        RefreshKimiMcpStatus();
        KimiMcpStatusText.Text = error
            ?? (KimiMcpToggle.IsOn
                ? "Connected. Open a new Kimi Code session to use Dimmy's tools."
                : "Adds Dimmy to ~/.kimi-code/mcp.json, for Kimi Code CLI and Kimi Code Desktop. Other entries stay as they are.");
    }

    private async void KimiIntegrationWizard_Click(object sender, RoutedEventArgs e)
        => await ShowKimiWizardAsync(forceStep1: false);

    private async void KimiIntegrationRerunWizard_Click(object sender, RoutedEventArgs e)
        => await ShowKimiWizardAsync(forceStep1: true);

    private async Task ShowKimiWizardAsync(bool forceStep1)
    {
        try
        {
            var dialog = new KimiConnectDialog
            {
                XamlRoot = this.Content.XamlRoot,
                RequestedTheme = Helpers.ThemeHelper.ResolvedElementTheme(),
                ForceStartAtStep1 = forceStep1,
            };
            await dialog.ShowAsync();
            Interop.DimmyNative.RecheckKimi();
            RefreshKimiIntegrationStatus();
            RefreshAuthIntegrationStatus();
        }
        catch (Exception ex)
        {
            App.Log($"Kimi wizard launch exc: {ex}", "Kimi");
        }
    }

    private async void KimiIntegrationSignIn_Click(object sender, RoutedEventArgs e)
    {
        KimiIntegrationSignInBtn.IsEnabled = false;
        KimiIntegrationStatusText.Text =
            "Launching `kimi login` — confirm the code in your browser.";
        try
        {
            if (!Interop.DimmyNative.SpawnKimiLogin())
            {
                KimiIntegrationStatusText.Text =
                    "Could not start `kimi login`. Open a terminal and run it manually.";
                Interop.DimmyNative.TrackEvent("kimi_cli.login_completed", new { outcome = "spawn_failed" });
                return;
            }
            // Documented polling exception: the sign-in happens in another
            // process; the credentials file appearing is the signal.
            for (int i = 0; i < 90; i++)
            {
                await Task.Delay(2000);
                if (Interop.DimmyNative.RecheckKimi() == Interop.DimmyNative.ClaudeCodeStatus.Ready)
                {
                    Interop.DimmyNative.TrackEvent("kimi_cli.login_completed", new { outcome = "success" });
                    RefreshKimiIntegrationStatus();
                    RefreshAuthIntegrationStatus();
                    return;
                }
            }
            KimiIntegrationStatusText.Text =
                "Sign-in not completed in 3 minutes. Click Refresh when ready.";
            Interop.DimmyNative.TrackEvent("kimi_cli.login_completed", new { outcome = "timeout" });
        }
        catch (Exception ex)
        {
            KimiIntegrationStatusText.Text = $"Sign-in error: {ex.Message}";
            App.Log($"KimiIntegration sign-in exc: {ex}", "Kimi");
            Interop.DimmyNative.TrackEvent("kimi_cli.login_completed", new { outcome = "spawn_failed" });
        }
        finally
        {
            KimiIntegrationSignInBtn.IsEnabled = true;
        }
    }

    private async void KimiIntegrationTest_Click(object sender, RoutedEventArgs e)
    {
        KimiIntegrationTestBtn.IsEnabled = false;
        KimiIntegrationStatusText.Text = "Sending ping…";
        try
        {
            var (result, elapsedMs) = await Task.Run(Interop.DimmyNative.PingKimi);
            KimiIntegrationStatusText.Text = KimiConnectDialog.KimiPingText(result, elapsedMs);
        }
        catch (Exception ex)
        {
            KimiIntegrationStatusText.Text = $"Test error: {ex.Message}";
            App.Log($"KimiIntegration test exc: {ex}", "Kimi");
        }
        finally
        {
            KimiIntegrationTestBtn.IsEnabled = true;
        }
    }

    private void KimiIntegrationRefresh_Click(object sender, RoutedEventArgs e)
    {
        Interop.DimmyNative.RecheckKimi();
        RefreshKimiIntegrationStatus();
        RefreshAuthIntegrationStatus();
    }
}
