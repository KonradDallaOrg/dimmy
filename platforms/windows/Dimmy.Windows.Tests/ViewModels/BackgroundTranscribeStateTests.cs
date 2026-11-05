using Dimmy.Windows.ViewModels;
using Xunit;

namespace Dimmy.Windows.Tests.ViewModels;

/// <summary>
/// Regenerate transcript, the speaker relabel after a stop and a loaded file
/// all transcribe with the dictation state at idle. The taskbar dot and the
/// tray stayed on "ready" for the minutes that took.
/// </summary>
public class BackgroundTranscribeStateTests
{
    [Fact]
    public void An_idle_app_transcribing_a_file_shows_the_transcribing_dot()
    {
        Assert.Equal(AppState.Transcribing, AppViewModel.IndicatedState(AppState.Idle, 0, 1));
    }

    [Fact]
    public void Nothing_running_stays_idle()
    {
        Assert.Equal(AppState.Idle, AppViewModel.IndicatedState(AppState.Idle, 0, 0));
    }

    [Fact]
    public void A_recap_wins_over_a_transcription()
    {
        Assert.Equal(AppState.Processing, AppViewModel.IndicatedState(AppState.Idle, 1, 1));
    }

    [Theory]
    [InlineData(AppState.Recording)]
    [InlineData(AppState.Processing)]
    [InlineData(AppState.Error)]
    public void A_dictation_keeps_its_own_state(AppState state)
    {
        Assert.Equal(state, AppViewModel.IndicatedState(state, 1, 1));
    }

    [Fact]
    public void Overlapping_transcriptions_are_counted_not_flagged()
    {
        var vm = new AppViewModel();
        vm.TranscribesRunning++;
        vm.TranscribesRunning++;
        vm.TranscribesRunning--;
        Assert.Equal(AppState.Transcribing,
            AppViewModel.IndicatedState(vm.CurrentState, vm.RecapsRunning, vm.TranscribesRunning));
        vm.TranscribesRunning--;
        Assert.Equal(AppState.Idle,
            AppViewModel.IndicatedState(vm.CurrentState, vm.RecapsRunning, vm.TranscribesRunning));
    }
}
