using Dimmy.Windows.ViewModels;
using Xunit;

namespace Dimmy.Windows.Tests.ViewModels;

/// Where the pill goes when the checkmark, an error or a background job lets
/// go of it. Each of these used to write Idle on its own, and a timer armed by
/// an earlier dictation set Idle in the middle of the next one.
public class PillRestStateTests
{
    [Theory]
    // The core is recording: the red dot, whatever was showing.
    [InlineData(true, false, false, AppState.Error, AppState.Recording)]
    [InlineData(true, false, false, AppState.Completing, AppState.Recording)]
    // A meeting is recording.
    [InlineData(false, true, false, AppState.Error, AppState.Recording)]
    // The dictation pipeline is still delivering: keep its stage.
    [InlineData(false, false, true, AppState.Processing, AppState.Processing)]
    [InlineData(false, false, true, AppState.Transcribing, AppState.Transcribing)]
    [InlineData(false, false, true, AppState.Error, AppState.Transcribing)]
    // Nothing running.
    [InlineData(false, false, false, AppState.Completing, AppState.Idle)]
    [InlineData(false, false, false, AppState.Error, AppState.Idle)]
    public void RestFollowsWhatIsReallyRunning(bool coreRecording, bool meeting, bool busy, AppState current, AppState want)
    {
        Assert.Equal(want, AppViewModel.RestState(coreRecording, meeting, busy, current));
    }

    [Fact]
    public void AnErrorDuringARecordingKeepsTheRedDot()
    {
        var vm = new AppViewModel();
        string? toast = null;
        vm.ErrorWhileRecording += m => toast = m;
        vm.HandleEvent("{\"event\":\"recording_started\",\"payload\":{}}");
        vm.HandleEvent("{\"event\":\"dictation_phase\",\"payload\":{\"phase\":\"recording\",\"id\":3}}");
        vm.SetError("Finish the dictation first");
        Assert.Equal(AppState.Recording, vm.CurrentState);
        Assert.Equal("Finish the dictation first", toast);
    }

    [Fact]
    public void AnErrorAfterTheRecordingStillShowsOnThePill()
    {
        var vm = new AppViewModel();
        vm.HandleEvent("{\"event\":\"dictation_phase\",\"payload\":{\"phase\":\"finishing\",\"id\":3}}");
        vm.SetState(AppState.Transcribing);
        vm.SetError("HTTP 401");
        Assert.Equal(AppState.Error, vm.CurrentState);
    }

    [Fact]
    public void ReturnToRestAfterTheNextPressIsRecordingNotIdle()
    {
        // The 2026-11-09 bug: the checkmark's 1.2 s timer fired after the
        // next press and set Idle while the core recorded.
        var vm = new AppViewModel();
        vm.HandleEvent("{\"event\":\"dictation_phase\",\"payload\":{\"phase\":\"recording\",\"id\":4}}");
        vm.SetState(AppState.Completing);
        vm.ReturnToRest();
        Assert.Equal(AppState.Recording, vm.CurrentState);
    }
}
