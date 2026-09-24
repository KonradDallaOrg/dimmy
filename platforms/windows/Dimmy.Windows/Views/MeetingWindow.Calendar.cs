using System;
using System.Collections.Generic;
using System.Linq;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading.Tasks;
using Dimmy.Windows.Interop;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace Dimmy.Windows.Views;

/// <summary>
/// Calendar context for the meeting window: which invite was this, and
/// who was in it.
///
/// Split out of MeetingWindow.xaml.cs deliberately — that file is already
/// the largest in the host, and this is a self-contained surface with one
/// entry point (<see cref="BeginCalendarLookup"/>) and one exit (the
/// assignment written next to the audio).
///
/// Three rules, all of them the reason this looks more careful than a
/// dropdown would:
///
/// 1. Never modal. The lookup takes 19-29 s, so it lands while the user
///    is already talking. A dialog there is the auto-record announcement
///    mistake wearing a different hat.
/// 2. Never auto-applied, not even with a single obvious candidate. The
///    roster goes into a recap the user forwards to other people, and a
///    wrong name is worse than no name.
/// 3. "None" is an answer and is remembered. Asking again on every reopen
///    is the nagging loop the call-detect nudge was fixed for.
/// </summary>
public sealed partial class MeetingWindow
{
    private sealed class CalEvent
    {
        [JsonPropertyName("id")] public string Id { get; set; } = "";
        [JsonPropertyName("title")] public string Title { get; set; } = "";
        [JsonPropertyName("start_unix")] public long StartUnix { get; set; }
        [JsonPropertyName("end_unix")] public long EndUnix { get; set; }
        [JsonPropertyName("attendees")] public List<CalAttendee> Attendees { get; set; } = new();
        [JsonPropertyName("attendee_count")] public long? AttendeeCount { get; set; }

        /// How many were invited, from whichever source we have. An org
        /// policy can forbid the names while leaving the head count fine,
        /// and a count is still worth showing.
        public long InvitedCount =>
            Attendees.Count > 0 ? Attendees.Count : Math.Max(0, AttendeeCount ?? 0);
        [JsonPropertyName("organizer")] public string Organizer { get; set; } = "";
    }

    private sealed class CalAttendee
    {
        [JsonPropertyName("name")] public string Name { get; set; } = "";
        [JsonPropertyName("email")] public string Email { get; set; } = "";
    }

    private sealed class CalCandidate
    {
        [JsonPropertyName("event")] public CalEvent Event { get; set; } = new();
        [JsonPropertyName("overlap_mins")] public long OverlapMins { get; set; }
        [JsonPropertyName("coverage_pct")] public long CoveragePct { get; set; }
        [JsonPropertyName("match_kind")] public string MatchKind { get; set; } = "";
    }

    private sealed class CalCandidatesReply
    {
        [JsonPropertyName("ok")] public bool Ok { get; set; }
        [JsonPropertyName("candidates")] public List<CalCandidate> Candidates { get; set; } = new();
        [JsonPropertyName("error")] public string Error { get; set; } = "";
    }

    private sealed class CalAssignmentReply
    {
        [JsonPropertyName("answered")] public bool Answered { get; set; }
        [JsonPropertyName("dismissed")] public bool Dismissed { get; set; }
        [JsonPropertyName("event")] public CalEvent? Event { get; set; }
        [JsonPropertyName("roster_line")] public string RosterLine { get; set; } = "";
    }

    private static readonly JsonSerializerOptions CalJson = new()
    {
        PropertyNameCaseInsensitive = true,
    };

    private List<CalCandidate> _calCandidates = new();
    private int _calIndex;
    private string? _calMeetingDir;

    /// <summary>
    /// Start the lookup for a meeting that has just begun.
    ///
    /// Fire-and-forget on a background thread: the lookup shells out to
    /// the user's `claude` CLI and blocks for tens of seconds. It must
    /// never touch the capture path, and it must never make the window
    /// wait — so nothing here is awaited by the caller.
    /// </summary>
    private void BeginCalendarLookup(string meetingDir)
    {
        if (string.IsNullOrWhiteSpace(meetingDir)) return;
        _calMeetingDir = meetingDir;
        HideCalendarBar();

        _ = Task.Run(() =>
        {
            try
            {
                // Already answered for this meeting? Then the user has
                // spoken and we do not ask a second time.
                var existing = ReadAssignment(meetingDir);
                if (existing is { Answered: true })
                {
                    App.Log("calendar: already answered for this meeting", "Calendar");
                    return;
                }

                var raw = DimmyNative.CalendarCandidates(meetingDir);
                if (string.IsNullOrWhiteSpace(raw))
                {
                    // Was a silent return, which is how a meeting with no
                    // calendar row left no trace at all to diagnose from.
                    App.Log("calendar: no answer from the core", "Calendar");
                    return;
                }
                var reply = JsonSerializer.Deserialize<CalCandidatesReply>(raw, CalJson);
                if (reply is null || !reply.Ok)
                {
                    // Off, unauthorised or unreachable. Silence is correct:
                    // the user did not ask for a calendar just now, they
                    // asked to record a meeting, and that worked.
                    App.Log($"calendar: no context ({reply?.Error})", "Calendar");
                    return;
                }
                if (reply.Candidates.Count == 0)
                {
                    App.Log("calendar: day read, nothing lines up", "Calendar");
                    return;
                }

                // The window may already be gone: its lifecycle is
                // decoupled from the recording, so the user can close it
                // and keep talking. TryEnqueue on a dead dispatcher
                // silently does nothing, which is how the first live run
                // found a candidate and told nobody. The core has parked
                // the candidates on disk, so the reopened window will
                // pick them up either way; all we decide here is whether
                // anyone can see the row right now.
                var delivered = DispatcherQueue.TryEnqueue(() =>
                {
                    // The meeting may have been stopped, or another one
                    // started, while the lookup was out.
                    if (_calMeetingDir != meetingDir) return;
                    _calCandidates = reply.Candidates;
                    _calIndex = 0;
                    ShowCalendarCandidate();
                });
                if (!delivered || !IsWindowOnScreen())
                    NotifyCalendarPending(reply.Candidates.Count);
            }
            catch (Exception ex)
            {
                App.Log($"calendar lookup failed: {ex.Message}", "Calendar");
            }
        });
    }

    /// <summary>
    /// Is this window actually on screen? A closed MeetingWindow can leave
    /// a live C# object behind (its lifecycle is decoupled from the
    /// recording), so "the object exists" proves nothing about whether the
    /// user can see the row.
    /// </summary>
    private bool IsWindowOnScreen()
    {
        try { return AppWindow is not null && AppWindow.IsVisible; }
        catch (Exception) { return false; }
    }

    /// <summary>
    /// Offer the choice again when the window comes back, using the
    /// candidates the core parked on disk. Costs one small file read, not
    /// another 25-second trip through the CLI.
    /// </summary>
    internal void ResumeCalendarPrompt(string meetingDir)
    {
        if (string.IsNullOrWhiteSpace(meetingDir)) return;
        _calMeetingDir = meetingDir;
        var raw = DimmyNative.CalendarPending(meetingDir);
        if (string.IsNullOrWhiteSpace(raw)) return;
        try
        {
            var reply = JsonSerializer.Deserialize<CalCandidatesReply>(raw, CalJson);
            if (reply is null || reply.Candidates.Count == 0) return;
            _calCandidates = reply.Candidates;
            _calIndex = 0;
            ShowCalendarCandidate();
        }
        catch (JsonException) { }
    }

    /// <summary>
    /// Show the linked invite in the Done header, or hide the row when
    /// there is none.
    ///
    /// This is the only place the event is visible without a recap. Until
    /// it existed, Dimmy fetched the invite, saved it next to the audio,
    /// and then showed it to nobody unless a recap happened to run.
    /// </summary>
    private void RefreshDoneCalendarRow(string? meetingDir)
    {
        try
        {
            if (string.IsNullOrWhiteSpace(meetingDir))
            {
                DoneCalendarRow.Visibility = Visibility.Collapsed;
                return;
            }
            var a = ReadAssignment(meetingDir);
            if (a?.Event is null)
            {
                DoneCalendarRow.Visibility = Visibility.Collapsed;
                return;
            }
            var ev = a.Event;
            var start = DateTimeOffset.FromUnixTimeSeconds(ev.StartUnix).ToLocalTime();
            var end = DateTimeOffset.FromUnixTimeSeconds(ev.EndUnix).ToLocalTime();
            var n = ev.InvitedCount;
            // "invited", not "attended": the invite proves invitation and
            // nothing else, and half a list routinely does not join.
            var who = n == 0 ? "" : $"  ·  {n} invited";
            var title = string.IsNullOrWhiteSpace(ev.Title) ? "(no subject)" : ev.Title;
            DoneCalendarText.Text = $"{title}  ·  {start:HH:mm}-{end:HH:mm}{who}";
            DoneCalendarRow.Visibility = Visibility.Visible;
        }
        catch (Exception ex)
        {
            App.Log($"calendar done row failed: {ex.Message}", "Calendar");
            DoneCalendarRow.Visibility = Visibility.Collapsed;
        }
    }

    private static CalAssignmentReply? ReadAssignment(string meetingDir)
    {
        var raw = DimmyNative.CalendarAssignment(meetingDir);
        if (string.IsNullOrWhiteSpace(raw)) return null;
        try { return JsonSerializer.Deserialize<CalAssignmentReply>(raw, CalJson); }
        catch (JsonException) { return null; }
    }

    /// <summary>
    /// The roster line for the recap prompt, or empty when this meeting
    /// has no invite attached. Built in the core so Windows and macOS
    /// word it identically.
    /// </summary>
    internal static string CalendarRosterLine(string meetingDir)
    {
        if (string.IsNullOrWhiteSpace(meetingDir)) return "";
        return ReadAssignment(meetingDir)?.RosterLine ?? "";
    }

    private void ShowCalendarCandidate()
    {
        if (_calCandidates.Count == 0) { HideCalendarBar(); return; }
        _calIndex = Math.Clamp(_calIndex, 0, _calCandidates.Count - 1);
        var c = _calCandidates[_calIndex];

        var title = string.IsNullOrWhiteSpace(c.Event.Title) ? "(no subject)" : c.Event.Title;
        CalendarHeadline.Text = $"Looks like “{title}”";

        var start = DateTimeOffset.FromUnixTimeSeconds(c.Event.StartUnix).ToLocalTime();
        var end = DateTimeOffset.FromUnixTimeSeconds(c.Event.EndUnix).ToLocalTime();
        var n = c.Event.InvitedCount;
        var people = n == 1 ? "1 invited" : $"{n} invited";
        // The wording follows match_kind rather than dressing one number
        // up as three different meanings.
        var why = c.MatchKind switch
        {
            "current" => "happening now",
            "nearby" => "starts shortly",
            _ => $"{c.OverlapMins} min overlap",
        };
        var more = _calCandidates.Count > 1 ? $"  ·  {_calIndex + 1} of {_calCandidates.Count}" : "";
        CalendarSubline.Text = $"{start:HH:mm}-{end:HH:mm}  ·  {people}  ·  {why}{more}";

        CalendarChangeBtn.Visibility = _calCandidates.Count > 1 ? Visibility.Visible : Visibility.Collapsed;
        CalendarBar.Visibility = Visibility.Visible;
    }

    private void HideCalendarBar() => CalendarBar.Visibility = Visibility.Collapsed;

    private void CalendarConfirm_Click(object sender, RoutedEventArgs e)
    {
        if (_calMeetingDir is null || _calCandidates.Count == 0) return;
        var chosen = _calCandidates[_calIndex].Event;
        var json = JsonSerializer.Serialize(chosen, CalJson);
        var rc = DimmyNative.dimmy_calendar_assign(_calMeetingDir, json);
        App.Log($"calendar assign rc={rc}", "Calendar");
        HideCalendarBar();
        // Confirming from the Done view must update the header it is
        // sitting in, not wait for the next reopen.
        RefreshDoneCalendarRow(_calMeetingDir);
        ShowToast(rc == 1 ? "Meeting linked to the calendar event." : "Could not save the link.");
    }

    /// <summary>Cycle to the next candidate. With two it reads as a toggle,
    /// with five as a carousel; either way the user sees one row at a time
    /// instead of a list they have to parse mid-call.</summary>
    private void CalendarChange_Click(object sender, RoutedEventArgs e)
    {
        if (_calCandidates.Count == 0) return;
        _calIndex = (_calIndex + 1) % _calCandidates.Count;
        ShowCalendarCandidate();
    }

    private void CalendarNone_Click(object sender, RoutedEventArgs e)
    {
        if (_calMeetingDir is null) return;
        // Persisted as an ANSWER, not as absence: that is what stops the
        // row coming back every time this meeting is reopened.
        var rc = DimmyNative.dimmy_calendar_assign(_calMeetingDir, null);
        App.Log($"calendar dismissed rc={rc}", "Calendar");
        HideCalendarBar();
    }

    /// <summary>
    /// The meeting window is closed but a choice is waiting. A toast that
    /// asks the question outright cannot work — there may be five invites
    /// and no way to correct a mis-tap — so it only offers to open the
    /// window where the real choice lives.
    /// </summary>
    private void NotifyCalendarPending(int count)
    {
        try
        {
            Services.DictNotificationService.ShowCalendarMatch(count);
        }
        catch (Exception ex)
        {
            App.Log($"calendar notify failed: {ex.Message}", "Calendar");
        }
    }
}
