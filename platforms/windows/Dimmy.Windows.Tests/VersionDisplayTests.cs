using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests;

/// <summary>
/// The About page has now told a release-candidate user twice that they were
/// on the stable release. Both times the mechanism was the same: the version
/// that knows about the "-rc.N" suffix was unavailable at the moment the page
/// drew itself, so the code quietly used the one that does not.
///
/// <para>These pin the rule that makes that impossible to reintroduce by
/// accident: the packaged version wins whenever there is one, and the core's
/// answer is a fallback for the single case where it is actually correct — an
/// unpackaged dev build.</para>
/// </summary>
public class VersionDisplayTests
{
    [Fact]
    public void Packaged_version_wins_and_keeps_its_prerelease_suffix()
    {
        Assert.Equal("0.7.3-rc.1", VersionDisplay.Resolve("0.7.3-rc.1", "0.7.3"));
    }

    [Fact]
    public void Core_version_is_used_only_when_there_is_no_package()
    {
        // An unpackaged dev build: no Velopack identity, and "0.7.3" is the
        // honest answer there because no tag was involved.
        Assert.Equal("0.7.3", VersionDisplay.Resolve("", "0.7.3"));
        Assert.Equal("0.7.3", VersionDisplay.Resolve(null, "0.7.3"));
        Assert.Equal("0.7.3", VersionDisplay.Resolve("   ", "0.7.3"));
    }

    [Fact]
    public void Never_returns_empty_so_the_hero_title_cannot_read_Dimmy_nothing()
    {
        Assert.Equal("0.0.0", VersionDisplay.Resolve("", ""));
        Assert.Equal("0.0.0", VersionDisplay.Resolve(null, null));
    }

    [Fact]
    public void Whitespace_is_trimmed_off_both_sources()
    {
        Assert.Equal("0.7.3-rc.1", VersionDisplay.Resolve(" 0.7.3-rc.1 ", "0.7.3"));
        Assert.Equal("0.7.3", VersionDisplay.Resolve(null, " 0.7.3 "));
    }

    /// The exact shape of the bug: two sources that agree on the number and
    /// disagree on whether this is a release candidate.
    [Theory]
    [InlineData("0.7.3-rc.1", "0.7.3")]
    [InlineData("0.7.2-rc.4", "0.7.2")]
    [InlineData("1.0.0-staging.12", "1.0.0")]
    public void A_dropped_suffix_is_detectable_and_never_actually_dropped(
        string packaged, string core)
    {
        Assert.True(VersionDisplay.WouldLoseSuffix(packaged, core));
        Assert.Equal(packaged, VersionDisplay.Resolve(packaged, core));
    }

    [Theory]
    [InlineData("0.7.3", "0.7.3")]      // stable: nothing to lose
    [InlineData("0.7.4", "0.7.3")]      // a real mismatch, not a suffix
    [InlineData("", "0.7.3")]           // dev build
    [InlineData("0.7.3-rc.1", "")]      // core unavailable
    public void Everything_else_is_not_a_dropped_suffix(string packaged, string core)
    {
        Assert.False(VersionDisplay.WouldLoseSuffix(packaged, core));
    }
}
