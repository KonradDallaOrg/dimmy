using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests;

/// "Dimmy closed unexpectedly last time": Send sends that one report,
/// Don't send deletes it, and "Always send" is about the future only.
public class CrashReportChoiceTests
{
    [Fact]
    public void Send_and_dont_send_map_to_the_core_answer()
    {
        Assert.Equal(1, new CrashReportChoice(true, false).ResolveArgument);
        Assert.Equal(0, new CrashReportChoice(false, false).ResolveArgument);
    }

    [Fact]
    public void Always_send_does_not_send_a_report_the_user_declined()
    {
        var choice = new CrashReportChoice(Send: false, AlwaysSend: true);
        Assert.Equal(0, choice.ResolveArgument);
        Assert.True(choice.AlwaysSend);
    }

    [Fact]
    public void Never_on_screen_together_with_the_consent_question()
    {
        Assert.False(CrashReportChoice.ShouldAsk(statsConsentPending: true));
        Assert.True(CrashReportChoice.ShouldAsk(statsConsentPending: false));
    }
}
