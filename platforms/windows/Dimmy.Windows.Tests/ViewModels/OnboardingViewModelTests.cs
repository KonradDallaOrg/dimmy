using Dimmy.Windows.ViewModels;
using Xunit;

namespace Dimmy.Windows.Tests.ViewModels;

public class OnboardingViewModelTests
{
    [Fact]
    public void InitialStep_IsZero()
    {
        var vm = new OnboardingViewModel();
        Assert.Equal(0, vm.CurrentStep);
        Assert.Equal(3, vm.TotalSteps);
    }

    [Fact]
    public void NextStep_AdvancesStep()
    {
        var vm = new OnboardingViewModel();
        vm.NextStep();
        Assert.Equal(1, vm.CurrentStep);
    }

    [Fact]
    public void NextStep_DoesNotExceedMax()
    {
        var vm = new OnboardingViewModel();
        vm.NextStep();
        vm.NextStep();
        vm.NextStep();
        Assert.Equal(2, vm.CurrentStep);
    }

    [Fact]
    public void PreviousStep_GoesBack()
    {
        var vm = new OnboardingViewModel();
        vm.NextStep();
        vm.PreviousStep();
        Assert.Equal(0, vm.CurrentStep);
    }

    [Fact]
    public void PreviousStep_DoesNotGoBelowZero()
    {
        var vm = new OnboardingViewModel();
        vm.PreviousStep();
        Assert.Equal(0, vm.CurrentStep);
    }

    [Fact]
    public void CanGoBack_FalseOnFirstStep()
    {
        var vm = new OnboardingViewModel();
        Assert.False(vm.CanGoBack);
    }

    [Fact]
    public void CanGoBack_TrueOnSecondStep()
    {
        var vm = new OnboardingViewModel();
        vm.NextStep();
        Assert.True(vm.CanGoBack);
    }

    [Fact]
    public void IsStep0_TrueInitially()
    {
        var vm = new OnboardingViewModel();
        Assert.True(vm.IsStep0);
        Assert.False(vm.IsStep1);
        Assert.False(vm.IsStep2);
    }

    [Fact]
    public void Shortcut_DefaultIsWinAlt()
    {
        var vm = new OnboardingViewModel();
        Assert.Equal("Win+Alt", vm.Shortcut);
    }

    [Fact]
    public void ShortcutMode_DefaultIsToggle()
    {
        var vm = new OnboardingViewModel();
        Assert.Equal("toggle", vm.ShortcutMode);
    }

    [Fact]
    public void IsTrialSuccess_DefaultFalse()
    {
        var vm = new OnboardingViewModel();
        Assert.False(vm.IsTrialSuccess);
    }

    [Fact]
    public void SetTrialSuccess_UpdatesState()
    {
        var vm = new OnboardingViewModel();
        vm.IsTrialSuccess = true;
        Assert.True(vm.IsTrialSuccess);
    }
}
