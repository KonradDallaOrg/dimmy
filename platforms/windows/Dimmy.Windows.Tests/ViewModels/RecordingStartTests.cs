using Dimmy.Windows.ViewModels;
using Xunit;

namespace Dimmy.Windows.Tests.ViewModels;

/// The start instant that scales the transcription timeout. It came from the
/// host's `recording_started`, which a late callback skips and nothing ever
/// cleared, so a dictation could be timed from the one before it.
public class RecordingStartTests
{
    [Fact]
    public void TheCoreRecordingPhaseSetsTheStart()
    {
        var vm = new AppViewModel();
        vm.HandleEvent("{\"event\":\"dictation_phase\",\"payload\":{\"phase\":\"recording\",\"id\":1}}");
        Assert.NotNull(vm.RecordingStartedUtc);
    }

    [Fact]
    public void TheStartLastsThroughFinishingAndClearsAtIdle()
    {
        var vm = new AppViewModel();
        vm.HandleEvent("{\"event\":\"dictation_phase\",\"payload\":{\"phase\":\"recording\",\"id\":1}}");
        vm.HandleEvent("{\"event\":\"dictation_phase\",\"payload\":{\"phase\":\"finishing\",\"id\":1}}");
        Assert.NotNull(vm.RecordingStartedUtc);
        vm.HandleEvent("{\"event\":\"dictation_phase\",\"payload\":{\"phase\":\"idle\",\"id\":1}}");
        Assert.Null(vm.RecordingStartedUtc);
    }

    [Fact]
    public void ASkippedLateCallbackLeavesNoOldStart()
    {
        var vm = new AppViewModel();
        vm.HandleEvent("{\"event\":\"dictation_phase\",\"payload\":{\"phase\":\"recording\",\"id\":1}}");
        vm.HandleEvent("{\"event\":\"dictation_phase\",\"payload\":{\"phase\":\"idle\",\"id\":1}}");
        vm.SuppressRecordingStarted = true;
        vm.HandleEvent("{\"event\":\"recording_started\",\"payload\":{}}");
        Assert.Null(vm.RecordingStartedUtc);
    }
}
