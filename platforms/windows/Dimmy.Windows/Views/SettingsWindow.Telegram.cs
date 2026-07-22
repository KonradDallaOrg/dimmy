using System;
using System.Text.Json;
using Microsoft.UI.Xaml;

using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Views;

/// Telegram audio-inbox section of the Settings window: the enable toggle,
/// the phone -> code -> 2FA login form, the connected status, and the
/// auto-process toggle. The login state machine is driven entirely by the
/// Rust worker's `telegram_state` events (surfaced as
/// AppViewModel.TelegramStateChanged) — this only paints panels and forwards
/// the user's inputs into the FFI. The worker owns the account + session.
public sealed partial class SettingsWindow
{
    // Called from the ctor's AppViewModel subscription block. Re-marshals
    // to the UI thread defensively (HandleEvent already dispatches there).
    private void OnTelegramStateChanged(string phase, string account, int pending)
    {
        this.DispatcherQueue.TryEnqueue(() =>
        {
            try { TelegramRefreshFromState(phase, account, pending); }
            catch (Exception ex) { App.Log($"OnTelegramStateChanged exc: {ex.Message}", "Telegram"); }
        });
    }

    private void OnTelegramError(string message)
    {
        this.DispatcherQueue.TryEnqueue(() =>
        {
            try
            {
                if (TelegramPhoneRing != null)
                {
                    TelegramPhoneRing.IsActive = false;
                    TelegramPhoneRing.Visibility = Visibility.Collapsed;
                }
                TelegramShowMessage(message, isError: true);
            }
            catch (Exception ex) { App.Log($"OnTelegramError exc: {ex.Message}", "Telegram"); }
        });
    }

    /// Read the current worker status and render. Called on window load and
    /// whenever the enable toggle flips.
    private void RefreshTelegramSection()
    {
        try
        {
            var json = DimmyNative.ReadBuffer(DimmyNative.dimmy_telegram_status, 4096);
            string phase = "logged_out";
            string account = "";
            int pending = 0;
            if (!string.IsNullOrEmpty(json))
            {
                using var doc = JsonDocument.Parse(json);
                var root = doc.RootElement;
                phase = root.TryGetProperty("phase", out var ph) ? (ph.GetString() ?? "logged_out") : "logged_out";
                account = root.TryGetProperty("account", out var ac) ? (ac.GetString() ?? "") : "";
                pending = root.TryGetProperty("pending", out var pe) && pe.TryGetInt32(out var pv) ? pv : 0;
            }
            TelegramRefreshFromState(phase, account, pending);
        }
        catch (Exception ex)
        {
            App.Log($"RefreshTelegramSection exc: {ex.Message}", "Telegram");
        }
    }

    private void TelegramRefreshFromState(string phase, string account, int pending)
    {
        if (TelegramCard == null) return; // page not realized yet

        // The card (icon + name + enable toggle) is always shown, like the other
        // integrations. Everything below the header is state-driven.
        TelegramPhonePanel.Visibility = Visibility.Collapsed;
        TelegramCodePanel.Visibility = Visibility.Collapsed;
        TelegramPasswordPanel.Visibility = Visibility.Collapsed;
        TelegramConnectedActions.Visibility = Visibility.Collapsed;
        TelegramAutoProcessCard.Visibility = Visibility.Collapsed;
        TelegramStatusText.Visibility = Visibility.Visible; // header subtitle; hidden only when connected
        TelegramPhoneRing.IsActive = false;
        TelegramPhoneRing.Visibility = Visibility.Collapsed;

        if (!ViewModel.TelegramEnabled)
        {
            TelegramStatusText.Text = "Off";
            return;
        }

        switch (phase)
        {
            case "no_credentials":
                TelegramStatusText.Text = "This build has no Telegram API key.";
                break;

            case "wait_code":
                TelegramStatusText.Text = "Enter the code we sent to your Telegram app.";
                TelegramCodePanel.Visibility = Visibility.Visible;
                break;

            case "wait_password":
                TelegramStatusText.Text = "Two-step verification. Enter your Telegram password.";
                TelegramPasswordPanel.Visibility = Visibility.Visible;
                break;

            case "connected":
                {
                    var who = string.IsNullOrEmpty(account) ? "Connected" : $"Connected as {account}";
                    // Status + green check live in the Log out row (B layout), not the header.
                    TelegramConnectedAccount.Text = pending > 0 ? $"{who} - {pending} waiting" : who;
                    TelegramStatusText.Visibility = Visibility.Collapsed;
                    TelegramConnectedActions.Visibility = Visibility.Visible;
                    TelegramAutoProcessCard.Visibility = Visibility.Visible;
                }
                break;

            case "error":
                TelegramStatusText.Text = "Something went wrong. Try again.";
                TelegramPhonePanel.Visibility = Visibility.Visible;
                break;

            case "logged_out":
            case "disabled":
            default:
                TelegramStatusText.Text = "Not connected. Log in to start.";
                TelegramPhonePanel.Visibility = Visibility.Visible;
                break;
        }
    }

    private void TelegramShowMessage(string text, bool isError)
    {
        if (TelegramMessageText == null) return;
        TelegramMessageText.Text = text;
        TelegramMessageBar.Background = isError
            ? (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["SystemFillColorCriticalBackgroundBrush"]
            : (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["SystemFillColorSuccessBackgroundBrush"];
        TelegramMessageBar.Visibility = Visibility.Visible;
    }

    // ── Handlers ─────────────────────────────────────────────────────

    private void TelegramEnabled_Toggled(object sender, RoutedEventArgs e)
    {
        if (!_loaded) return;
        try
        {
            var json = ViewModel.ToJson();
            DimmyNative.dimmy_set_config_json(json);
            (Application.Current as App)?.ApplySettings(ViewModel);
            TelegramMessageBar.Visibility = Visibility.Collapsed;
            RefreshTelegramSection();
        }
        catch (Exception ex)
        {
            App.Log($"Telegram enable toggle exc: {ex.Message}", "Telegram");
        }
    }

    private void TelegramAutoProcess_Toggled(object sender, RoutedEventArgs e)
    {
        if (!_loaded) return;
        try
        {
            var json = ViewModel.ToJson();
            DimmyNative.dimmy_set_config_json(json);
            (Application.Current as App)?.ApplySettings(ViewModel);
        }
        catch (Exception ex)
        {
            App.Log($"Telegram auto-process toggle exc: {ex.Message}", "Telegram");
        }
    }

    private void TelegramSendCode_Click(object sender, RoutedEventArgs e)
    {
        var phone = (TelegramPhoneBox.Text ?? "").Trim();
        if (string.IsNullOrEmpty(phone))
        {
            TelegramShowMessage("Enter your phone number first, with country code.", isError: true);
            return;
        }
        TelegramMessageBar.Visibility = Visibility.Collapsed;
        // The worker requests the code asynchronously; keep the spinner up
        // until the next telegram_state event (wait_code / error) clears it.
        TelegramPhoneRing.IsActive = true;
        TelegramPhoneRing.Visibility = Visibility.Visible;
        try
        {
            int rc = DimmyNative.dimmy_telegram_start_login(phone);
            if (rc != 0)
            {
                TelegramPhoneRing.IsActive = false;
                TelegramPhoneRing.Visibility = Visibility.Collapsed;
                TelegramShowMessage(
                    rc == -100 ? "This build has no Telegram support." : "Could not start login. Try again.",
                    isError: true);
            }
        }
        catch (Exception ex)
        {
            TelegramPhoneRing.IsActive = false;
            TelegramPhoneRing.Visibility = Visibility.Collapsed;
            App.Log($"Telegram start login exc: {ex.Message}", "Telegram");
            TelegramShowMessage("Could not start login.", isError: true);
        }
    }

    private void TelegramSubmitCode_Click(object sender, RoutedEventArgs e)
    {
        var code = (TelegramCodeBox.Text ?? "").Trim();
        if (string.IsNullOrEmpty(code))
        {
            TelegramShowMessage("Enter the code first.", isError: true);
            return;
        }
        TelegramMessageBar.Visibility = Visibility.Collapsed;
        try
        {
            int rc = DimmyNative.dimmy_telegram_submit_code(code);
            if (rc != 0)
                TelegramShowMessage("Could not submit the code. Try again.", isError: true);
        }
        catch (Exception ex)
        {
            App.Log($"Telegram submit code exc: {ex.Message}", "Telegram");
            TelegramShowMessage("Could not submit the code.", isError: true);
        }
    }

    private void TelegramSubmitPassword_Click(object sender, RoutedEventArgs e)
    {
        var pw = TelegramPasswordBox.Password ?? "";
        if (string.IsNullOrEmpty(pw))
        {
            TelegramShowMessage("Enter your Telegram password.", isError: true);
            return;
        }
        TelegramMessageBar.Visibility = Visibility.Collapsed;
        try
        {
            int rc = DimmyNative.dimmy_telegram_submit_password(pw);
            if (rc != 0)
                TelegramShowMessage("Could not submit the password. Try again.", isError: true);
        }
        catch (Exception ex)
        {
            App.Log($"Telegram submit password exc: {ex.Message}", "Telegram");
            TelegramShowMessage("Could not submit the password.", isError: true);
        }
    }

    private void TelegramLogout_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            DimmyNative.dimmy_telegram_logout();
            TelegramShowMessage("Logged out of Telegram.", isError: false);
        }
        catch (Exception ex)
        {
            App.Log($"Telegram logout exc: {ex.Message}", "Telegram");
        }
    }
}
