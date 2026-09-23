using System;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading.Tasks;
using Dimmy.Windows.Interop;
using Microsoft.UI.Xaml;

namespace Dimmy.Windows.Views;

/// <summary>
/// The Integrations card for calendar context.
///
/// This is the one integration Dimmy cannot connect for you. Notion and
/// Confluence take a token you paste; this one borrows the OAuth already
/// inside your `claude` CLI, and that authorisation only happens in an
/// interactive session. Verified the hard way on 2026-09-23: authorising
/// the connector on claude.ai left the CLI reporting zero connector
/// tools, and only `/mcp` locally fixed it.
///
/// So the card does the two things it actually can — say truthfully what
/// state the connector is in, and open the session where the user can fix
/// it — and never pretends to own the handshake.
/// </summary>
public sealed partial class SettingsWindow
{
    private sealed class CalStatus
    {
        [JsonPropertyName("available")] public bool Available { get; set; }
        [JsonPropertyName("reason")] public string Reason { get; set; } = "";
    }

    private bool _calendarStatusInFlight;

    private void CalendarContextToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (!_loaded) return;
        var on = CalendarContextToggle.IsOn;
        ViewModel.CalendarContextEnabled = on;
        ScheduleAutoSaveConfig();
        if (on) _ = RefreshCalendarStatusAsync();
        else
        {
            CalendarStatusText.Text = "Off";
            CalendarSetupRow.Visibility = Visibility.Collapsed;
        }
    }

    private void CalendarSetup_Click(object sender, RoutedEventArgs e)
    {
        var rc = DimmyNative.dimmy_calendar_spawn_setup();
        App.Log($"calendar setup spawn rc={rc}", "Calendar");
        CalendarStatusText.Text = rc == 1
            // Naming the command matters: the session may open on the
            // connector list already, and if it does not, this sentence is
            // the difference between a working feature and a dead button.
            ? "A Claude window is open. Type /mcp, pick your calendar, then Check again."
            : "Could not open Claude. Is the CLI installed?";
    }

    private void CalendarRecheck_Click(object sender, RoutedEventArgs e) =>
        _ = RefreshCalendarStatusAsync();

    /// <summary>
    /// Ask the core whether the connector is usable. Off the UI thread
    /// without exception: the probe spawns a CLI turn and takes seconds.
    /// </summary>
    private async Task RefreshCalendarStatusAsync()
    {
        if (_calendarStatusInFlight) return;
        _calendarStatusInFlight = true;
        CalendarStatusText.Text = "Checking...";
        try
        {
            var raw = await Task.Run(() => DimmyNative.CalendarStatus());
            var st = string.IsNullOrWhiteSpace(raw)
                ? null
                : JsonSerializer.Deserialize<CalStatus>(raw);

            if (st is { Available: true })
            {
                CalendarStatusText.Text = "Connected";
                CalendarSetupRow.Visibility = Visibility.Collapsed;
                return;
            }

            // Each cause gets its own sentence because each has a
            // different fix, and "not available" would send the user
            // hunting for the wrong one.
            var (text, showSetup) = (st?.Reason ?? "probe_failed") switch
            {
                "disabled" => ("Off", false),
                "cli_missing" => ("Needs the Claude Code CLI, which is not installed.", false),
                "not_logged_in" => ("Sign in to your Claude subscription first, under Output.", false),
                "connector_not_authorised" =>
                    ("Almost there: the calendar connector still has to be authorised.", true),
                _ => ("Could not check right now.", true),
            };
            CalendarStatusText.Text = text;
            CalendarSetupRow.Visibility = showSetup ? Visibility.Visible : Visibility.Collapsed;
        }
        catch (Exception ex)
        {
            App.Log($"calendar status failed: {ex.Message}", "Calendar");
            CalendarStatusText.Text = "Could not check right now.";
            CalendarSetupRow.Visibility = Visibility.Visible;
        }
        finally
        {
            _calendarStatusInFlight = false;
        }
    }
}
