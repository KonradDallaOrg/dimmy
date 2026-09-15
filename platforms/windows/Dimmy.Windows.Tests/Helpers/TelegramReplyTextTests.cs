using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests.Helpers;

/// <summary>
/// The reply Dimmy sends back into Telegram is the only thing the sender
/// sees: they shared a voice note from a phone and walked away from the PC.
/// An empty or malformed message there is worse than no feature at all, so
/// the shapes a real recap.md can take are pinned here.
/// </summary>
public class TelegramReplyTextTests
{
    private const string Recap =
        "# Integration flows for Lobuten\n" +
        "<!-- dimmy-ai-generated: true; by: Dimmy; model: claude-opus-5 -->\n" +
        "<!-- dimmy-type: technical -->\n" +
        "\n" +
        "## Context\n" +
        "Three voices, one of them remote.\n" +
        "\n" +
        "## TL;DR\n" +
        "The flows are settled.\n" +
        "One architectural question is still open.\n" +
        "\n" +
        "## Highlights\n" +
        "- something else entirely\n";

    [Fact]
    public void Title_and_summary_are_quoted_and_nothing_after_them()
    {
        var reply = TelegramReplyText.FromRecapMarkdown(Recap);
        Assert.Equal(
            "Integration flows for Lobuten\n\nThe flows are settled. One architectural question is still open.",
            reply);
    }

    [Fact]
    public void The_ai_marking_comment_is_never_sent_back()
    {
        var reply = TelegramReplyText.FromRecapMarkdown(Recap);
        Assert.DoesNotContain("dimmy-ai-generated", reply);
        Assert.DoesNotContain("<!--", reply);
    }

    [Fact]
    public void A_recap_in_another_language_still_yields_its_summary()
    {
        // Only the heading is translated; the marker the renderer writes is
        // what this matches on.
        var reply = TelegramReplyText.FromRecapMarkdown(
            "# Riunione tecnica\n\n## TL;DR\nTutto deciso.\n\n## Punti chiave\n- altro\n");
        Assert.Equal("Riunione tecnica\n\nTutto deciso.", reply);
    }

    [Fact]
    public void A_recap_without_a_summary_falls_back_to_the_title()
    {
        Assert.Equal("Just a title",
            TelegramReplyText.FromRecapMarkdown("# Just a title\n\n## Context\nnope\n"));
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("   \n  \n")]
    [InlineData("## Context\nno title, no summary\n")]
    public void Nothing_worth_quoting_returns_null_so_the_caller_can_say_something_else(string? markdown)
    {
        Assert.Null(TelegramReplyText.FromRecapMarkdown(markdown));
    }
}
