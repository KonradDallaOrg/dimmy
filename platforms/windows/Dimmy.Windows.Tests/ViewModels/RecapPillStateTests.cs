using Dimmy.Windows.ViewModels;
using Xunit;

namespace Dimmy.Windows.Tests.ViewModels;

/// <summary>
/// A recap started from the pill, from Telegram or from a loaded file runs
/// with the dictation state back at idle. Before this the pill and the
/// taskbar showed nothing at all for the minutes the model was writing, and
/// a 34-minute Telegram audio looked like a frozen app.
/// </summary>
public class RecapPillStateTests
{
    [Fact]
    public void An_idle_app_with_a_recap_running_says_so()
    {
        Assert.True(AppViewModel.ShowsRecap(AppState.Idle, 1));
    }

    [Fact]
    public void Nothing_running_means_nothing_to_say()
    {
        Assert.False(AppViewModel.ShowsRecap(AppState.Idle, 0));
    }

    [Theory]
    [InlineData(AppState.Recording)]
    [InlineData(AppState.Transcribing)]
    [InlineData(AppState.Processing)]
    [InlineData(AppState.Error)]
    public void A_dictation_keeps_the_pill_it_already_owns(AppState state)
    {
        Assert.False(AppViewModel.ShowsRecap(state, 1));
    }

    [Fact]
    public void Overlapping_recaps_are_counted_not_flagged()
    {
        // A regenerate while a stop's recap is still running: the label has
        // to survive the first one finishing.
        var vm = new AppViewModel();
        vm.RecapsRunning++;
        vm.RecapsRunning++;
        vm.RecapsRunning--;
        Assert.True(AppViewModel.ShowsRecap(vm.CurrentState, vm.RecapsRunning));
        vm.RecapsRunning--;
        Assert.False(AppViewModel.ShowsRecap(vm.CurrentState, vm.RecapsRunning));
    }
}
