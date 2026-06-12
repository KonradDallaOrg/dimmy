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
        Assert.Equal(4, vm.TotalSteps);
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
        vm.NextStep();
        Assert.Equal(3, vm.CurrentStep);
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
    public void ShortcutMode_DefaultIsHold_MatchingRustDefaultAndWizardCopy()
    {
        // Rust core defaults shortcut_mode to "hold" and the wizard
        // teaches "Hold to dictate, release to paste". Persisting
        // "toggle" at Finish silently flipped the taught gesture.
        var vm = new OnboardingViewModel();
        Assert.Equal("hold", vm.ShortcutMode);
    }

    [Fact]
    public void ChoiceStepButton_DisabledWhileLocalDownloading()
    {
        var vm = new OnboardingViewModel { Choice = ModelChoice.Local };
        Assert.False(vm.CanAdvanceFromChoiceStep);
        Assert.Equal("Preparing model...", vm.ChoiceContinueLabel);
    }

    [Fact]
    public void ChoiceStepButton_EnabledAsRetryWhenLocalFailed()
    {
        var vm = new OnboardingViewModel { Choice = ModelChoice.Local, IsLocalFailed = true };
        Assert.True(vm.CanAdvanceFromChoiceStep);
        Assert.True(vm.IsLocalRetryable);
        Assert.Equal("Try again", vm.ChoiceContinueLabel);
    }

    [Fact]
    public void ChoiceStepButton_EnabledAsRetryWhenLocalOffline()
    {
        var vm = new OnboardingViewModel { Choice = ModelChoice.Local, IsLocalOffline = true };
        Assert.True(vm.CanAdvanceFromChoiceStep);
        Assert.True(vm.IsLocalRetryable);
        Assert.Equal("Try again", vm.ChoiceContinueLabel);
    }

    [Fact]
    public void ChoiceStepButton_EnabledAsContinueWhenLocalReady()
    {
        var vm = new OnboardingViewModel { Choice = ModelChoice.Local, IsLocalReady = true };
        Assert.True(vm.CanAdvanceFromChoiceStep);
        Assert.False(vm.IsLocalRetryable);
        Assert.Equal("Continue", vm.ChoiceContinueLabel);
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
