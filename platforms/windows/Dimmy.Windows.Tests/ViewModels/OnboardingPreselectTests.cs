using Dimmy.Windows.ViewModels;
using Xunit;

namespace Dimmy.Windows.Tests.ViewModels;

public class OnboardingPreselectTests
{
    [Theory]
    [InlineData("good")]
    [InlineData("tight")]
    public void Hardware_that_can_run_models_starts_on_Local(string fitness)
    {
        Assert.Equal(ModelChoice.Local, OnboardingPreselect.For(fitness));
    }

    [Fact]
    public void Hardware_that_cannot_starts_on_Cloud()
    {
        Assert.Equal(ModelChoice.Cloud, OnboardingPreselect.For("poor"));
    }

    [Fact]
    public void An_unreadable_machine_is_not_pushed_to_the_cloud()
    {
        // "unknown" means we could not read the GPU, which is not the same
        // as knowing it is weak. Local needs no account and works offline.
        Assert.Equal(ModelChoice.Local, OnboardingPreselect.For("unknown"));
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("GOOD")]
    [InlineData("something we never ship")]
    public void Nothing_ever_leaves_the_wizard_unselected(string? fitness)
    {
        // The actual bug being fixed: arriving with ModelChoice.None left
        // users staring at two cards that look like information and a
        // disabled Continue button. Whatever detection says — including
        // saying nothing — a card must be selected.
        Assert.NotEqual(ModelChoice.None, OnboardingPreselect.For(fitness));
    }
}
