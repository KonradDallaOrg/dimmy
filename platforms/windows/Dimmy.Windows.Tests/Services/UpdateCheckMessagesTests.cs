using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests.Services;

/// <summary>
/// Regression cover for the "Check for updates does nothing" report:
/// every outcome the check can reach must produce a non-empty line for
/// the user. The old code only reacted when an update was FOUND, so
/// "already current", "dev build", and "network down" all left the UI
/// byte-for-byte unchanged.
/// </summary>
public class UpdateCheckMessagesTests
{
    [Theory]
    [InlineData(UpdateCheckOutcome.UpToDate)]
    [InlineData(UpdateCheckOutcome.UpdateReady)]
    [InlineData(UpdateCheckOutcome.NoLicense)]
    [InlineData(UpdateCheckOutcome.DevBuild)]
    [InlineData(UpdateCheckOutcome.Failed)]
    public void EveryOutcomeProducesAMessage(UpdateCheckOutcome outcome)
    {
        var (_, message) = UpdateCheckMessages.For(outcome, "0.6.73", "stable");
        Assert.False(string.IsNullOrWhiteSpace(message));
    }

    [Theory]
    [InlineData(UpdateCheckOutcome.UpToDate, true)]
    [InlineData(UpdateCheckOutcome.UpdateReady, true)]
    [InlineData(UpdateCheckOutcome.NoLicense, false)]
    [InlineData(UpdateCheckOutcome.DevBuild, false)]
    [InlineData(UpdateCheckOutcome.Failed, false)]
    public void OkFlagSeparatesNothingToDoFromSomethingWentWrong(
        UpdateCheckOutcome outcome, bool expectedOk)
    {
        var (ok, _) = UpdateCheckMessages.For(outcome, "0.6.73", "stable");
        Assert.Equal(expectedOk, ok);
    }

    [Fact]
    public void UpToDateNamesTheRunningVersion()
    {
        var (_, message) = UpdateCheckMessages.For(UpdateCheckOutcome.UpToDate, "0.6.73", "stable");
        Assert.Contains("0.6.73", message);
    }

    [Fact]
    public void UpdateReadyNamesThePendingVersion()
    {
        var (_, message) = UpdateCheckMessages.For(UpdateCheckOutcome.UpdateReady, "0.6.74", "stable");
        Assert.Contains("0.6.74", message);
    }

    // A pre-release user shown the stable version number reads it as a
    // stuck check, so "you're on the latest" has to say WHICH latest.
    [Fact]
    public void UpToDateNamesTheChannel()
    {
        var (_, stable) = UpdateCheckMessages.For(UpdateCheckOutcome.UpToDate, "0.6.73", "stable");
        var (_, pre) = UpdateCheckMessages.For(UpdateCheckOutcome.UpToDate, "0.6.73", "prerelease");
        Assert.NotEqual(stable, pre);
        Assert.Contains("pre-release", pre);
    }

    [Theory]
    [InlineData(null, "stable")]
    [InlineData("", "stable")]
    [InlineData("stable", "stable")]
    [InlineData("prerelease", "stable + pre-release")]
    public void ChannelLabelDefaultsToStable(string? channel, string expected)
    {
        Assert.Equal(expected, UpdateCheckMessages.ChannelLabel(channel));
    }

    // Velopack can hand back an empty version string; the line must
    // still read as a sentence rather than "You're on the latest (v)."
    [Fact]
    public void EmptyVersionStillReadsAsASentence()
    {
        var (_, upToDate) = UpdateCheckMessages.For(UpdateCheckOutcome.UpToDate, "", "stable");
        var (_, ready) = UpdateCheckMessages.For(UpdateCheckOutcome.UpdateReady, "", "stable");
        Assert.DoesNotContain("(v)", upToDate);
        Assert.DoesNotContain("v.", ready);
    }

    // House rule: no em-dashes in UI copy (they break PS 5.1 pipelines
    // and read badly in the Settings font). See CLAUDE.md.
    [Theory]
    [InlineData(UpdateCheckOutcome.UpToDate)]
    [InlineData(UpdateCheckOutcome.UpdateReady)]
    [InlineData(UpdateCheckOutcome.NoLicense)]
    [InlineData(UpdateCheckOutcome.DevBuild)]
    [InlineData(UpdateCheckOutcome.Failed)]
    public void MessagesUseNoEmDashes(UpdateCheckOutcome outcome)
    {
        var (_, message) = UpdateCheckMessages.For(outcome, "0.6.73", "stable");
        Assert.DoesNotContain('—', message);
        Assert.DoesNotContain('–', message);
    }
}
