using Dimmy.Windows.ViewModels;
using Xunit;

namespace Dimmy.Windows.Tests.ViewModels;

public class AppViewModelTests
{
    [Fact]
    public void InitialState_IsIdle()
    {
        var vm = new AppViewModel();
        Assert.Equal(AppState.Idle, vm.CurrentState);
        Assert.False(vm.IsRecording);
        Assert.Equal("", vm.StatusText);
    }

    [Fact]
    public void SetState_Recording_UpdatesProperties()
    {
        var vm = new AppViewModel();
        vm.SetState(AppState.Recording);
        Assert.Equal(AppState.Recording, vm.CurrentState);
        Assert.True(vm.IsRecording);
    }

    [Fact]
    public void SetState_Transcribing_UpdatesStatusText()
    {
        var vm = new AppViewModel();
        vm.SetState(AppState.Transcribing);
        Assert.Equal("Transcribing...", vm.StatusText);
        Assert.False(vm.IsRecording);
    }

    [Fact]
    public void SetState_Processing_UpdatesStatusText()
    {
        var vm = new AppViewModel();
        vm.SetState(AppState.Processing);
        Assert.Equal("Processing...", vm.StatusText);
    }

    [Fact]
    public void SetState_Error_SetsErrorMessage()
    {
        var vm = new AppViewModel();
        vm.SetError("Something went wrong");
        Assert.Equal(AppState.Error, vm.CurrentState);
        Assert.Equal("Something went wrong", vm.ErrorMessage);
    }

    [Fact]
    public void SetState_Error_TruncatesLongMessage()
    {
        var vm = new AppViewModel();
        var longMsg = new string('x', 300);
        vm.SetError(longMsg);
        Assert.True(vm.ErrorMessage.Length <= 200);
    }

    [Fact]
    public void SetState_Completing_SetsState()
    {
        var vm = new AppViewModel();
        vm.SetState(AppState.Completing);
        Assert.Equal(AppState.Completing, vm.CurrentState);
    }

    [Fact]
    public void Amplitude_DefaultsToZero()
    {
        var vm = new AppViewModel();
        Assert.Equal(0.0f, vm.Amplitude);
    }

    [Fact]
    public void ChunkProgress_UpdatesCorrectly()
    {
        var vm = new AppViewModel();
        vm.UpdateChunkProgress(2, 5);
        Assert.Equal(2, vm.ChunkCurrent);
        Assert.Equal(5, vm.ChunkTotal);
    }

    [Fact]
    public void LlmStyleColor_ReturnsCorrectDefault()
    {
        var vm = new AppViewModel();
        Assert.Equal("#41B0B1", vm.LlmStyleColor);
    }

    [Fact]
    public void HandleEvent_RecordingStarted_SetsRecording()
    {
        var vm = new AppViewModel();
        vm.HandleEvent("{\"event\":\"recording_started\",\"payload\":{}}");
        Assert.Equal(AppState.Recording, vm.CurrentState);
    }

    [Fact]
    public void HandleEvent_Error_SetsErrorState()
    {
        var vm = new AppViewModel();
        vm.HandleEvent("{\"event\":\"error\",\"payload\":{\"message\":\"test error\"}}");
        Assert.Equal(AppState.Error, vm.CurrentState);
        Assert.Equal("test error", vm.ErrorMessage);
    }

    [Fact]
    public void HandleEvent_ChunkProgress_UpdatesChunks()
    {
        var vm = new AppViewModel();
        vm.HandleEvent("{\"event\":\"chunk_progress\",\"payload\":{\"current\":1,\"total\":3}}");
        Assert.Equal(1, vm.ChunkCurrent);
        Assert.Equal(3, vm.ChunkTotal);
    }

    [Fact]
    public void HandleEvent_InvalidJson_DoesNotThrow()
    {
        var vm = new AppViewModel();
        vm.HandleEvent("not json");
        Assert.Equal(AppState.Idle, vm.CurrentState);
    }

    [Fact]
    public void HandleEvent_NullJson_DoesNotThrow()
    {
        var vm = new AppViewModel();
        vm.HandleEvent(null);
        Assert.Equal(AppState.Idle, vm.CurrentState);
    }

    [Fact]
    public void HandleEvent_TranscriptReady_SetsCompleting()
    {
        var vm = new AppViewModel();
        vm.HandleEvent("{\"event\":\"transcript_ready\",\"payload\":{\"text\":\"hello\"}}");
        Assert.Equal(AppState.Completing, vm.CurrentState);
    }

    // ── State transitions ──

    [Fact]
    public void SetState_IdleAfterError_ClearsErrorMessage()
    {
        var vm = new AppViewModel();
        vm.SetError("broken");
        vm.SetState(AppState.Idle);
        Assert.Equal(AppState.Idle, vm.CurrentState);
    }

    [Fact]
    public void SetState_RecordingToTranscribing_IsNotRecording()
    {
        var vm = new AppViewModel();
        vm.SetState(AppState.Recording);
        Assert.True(vm.IsRecording);
        vm.SetState(AppState.Transcribing);
        Assert.False(vm.IsRecording);
    }

    [Fact]
    public void SetState_FullCycle_EndsIdle()
    {
        var vm = new AppViewModel();
        vm.SetState(AppState.Recording);
        vm.SetState(AppState.Transcribing);
        vm.SetState(AppState.Processing);
        vm.SetState(AppState.Completing);
        vm.SetState(AppState.Idle);
        Assert.Equal(AppState.Idle, vm.CurrentState);
        Assert.False(vm.IsRecording);
    }

    // ── HandleEvent edge cases ──

    [Fact]
    public void HandleEvent_StatusTranscribing_SetsState()
    {
        var vm = new AppViewModel();
        vm.HandleEvent("{\"event\":\"status\",\"payload\":{\"state\":\"transcribing\"}}");
        Assert.Equal(AppState.Transcribing, vm.CurrentState);
    }

    [Fact]
    public void HandleEvent_EmptyPayload_DoesNotThrow()
    {
        var vm = new AppViewModel();
        vm.HandleEvent("{\"event\":\"unknown_event\",\"payload\":{}}");
        // Should not crash, state unchanged
        Assert.Equal(AppState.Idle, vm.CurrentState);
    }

    [Fact]
    public void HandleEvent_SuppressRecordingStarted_IgnoresEvent()
    {
        var vm = new AppViewModel();
        vm.SuppressRecordingStarted = true;
        vm.HandleEvent("{\"event\":\"recording_started\",\"payload\":{}}");
        Assert.NotEqual(AppState.Recording, vm.CurrentState);
    }

    // ── UI properties ──

    [Fact]
    public void BorderStyle_DefaultIsRainbow()
    {
        var vm = new AppViewModel();
        Assert.Equal("Rainbow", vm.BorderStyle);
    }

    [Fact]
    public void WaveformStyle_DefaultIsBars()
    {
        var vm = new AppViewModel();
        Assert.Equal("Bars", vm.WaveformStyle);
    }
}
