using Xunit;
using Dimmy.Windows.Services;

namespace Dimmy.Windows.Tests.Services;

/// <summary>
/// Every toast showed for a flat 3 s until 2026-09-04. That suited the
/// dictionary toasts the window was built for and nothing else: the ones
/// added since carry the user's next step, and they vanished before they
/// could be read. Reported from a live meeting.
///
/// These pin the shape of the replacement — short toasts unchanged, long
/// ones readable, and a ceiling so a toast never becomes a dialog.
/// </summary>
public class ToastDurationTests
{
    // The real strings, so a rewrite that doubles a message's length is
    // caught here rather than on someone's screen.
    private const string ShortTitle = "Added to dictionary";
    private const string ShortBody = "“budget” will boost recognition on future transcriptions.";

    private const string BehindTitle = "Transcription is falling behind";
    private const string BehindBody =
        "Your recording is safe and continues normally. Try Parakeet or a smaller "
        + "model in Settings, Transcription. You can also regenerate the transcript "
        + "from the audio after the meeting.";

    [Fact]
    public void Short_toasts_keep_the_three_second_feel()
    {
        // The dictionary toasts were the reason 3 s was chosen. They must
        // not get slower just because other toasts got longer.
        var secs = ToastDuration.For(ShortTitle, ShortBody);
        Assert.InRange(secs, ToastDuration.MinSecs, 5.0);
    }

    [Fact]
    public void The_actionable_toast_stays_up_long_enough_to_read()
    {
        // 34 words at 180 wpm is ~11 s of reading. At the old flat 3 s the
        // user saw roughly the first line.
        var secs = ToastDuration.For(BehindTitle, BehindBody);
        Assert.True(secs >= 8.0,
            $"the toast that tells the user what to do lasts {secs:F1}s — not readable");
    }

    [Fact]
    public void Longer_messages_always_get_at_least_as_long()
    {
        // Monotonic: adding words must never shorten the toast.
        var shortSecs = ToastDuration.For(ShortTitle, ShortBody);
        var longSecs = ToastDuration.For(BehindTitle, BehindBody);
        Assert.True(longSecs > shortSecs);
    }

    [Fact]
    public void Never_below_the_floor_or_above_the_ceiling()
    {
        // Empty must still be visible; a runaway message must not pin a
        // window on screen — past the ceiling it belongs somewhere the user
        // can return to, not in a toast.
        Assert.Equal(ToastDuration.MinSecs, ToastDuration.For("", ""));
        Assert.Equal(ToastDuration.MinSecs, ToastDuration.For(null, null));
        var huge = string.Join(" ", System.Linq.Enumerable.Repeat("parola", 500));
        Assert.Equal(ToastDuration.MaxSecs, ToastDuration.For("Titolo", huge));
    }

    [Fact]
    public void Whitespace_is_not_content()
    {
        // Word counting must not inflate the duration on padding, or a
        // reformatted string would silently change how long it shows.
        Assert.Equal(
            ToastDuration.For("a b c", "d e f"),
            ToastDuration.For("  a   b   c  ", "\n d \t e  f \n"));
    }
}
