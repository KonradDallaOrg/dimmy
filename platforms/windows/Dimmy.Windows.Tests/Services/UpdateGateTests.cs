using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests.Services;

public class UpdateGateTests
{
    [Fact]
    public void An_idle_app_may_be_replaced()
    {
        Assert.True(UpdateGate.MayEndProcess(0));
    }

    [Fact]
    public void A_recording_meeting_blocks_the_update()
    {
        Assert.False(UpdateGate.MayEndProcess(1));
    }

    [Fact]
    public void A_lock_failure_counts_as_recording()
    {
        // We could not read the meeting state. The safe answer is the one
        // that cannot destroy a recording: an update waits, a lost meeting
        // does not come back.
        Assert.False(UpdateGate.MayEndProcess(-1));
    }

    [Fact]
    public void Any_unexpected_return_counts_as_recording()
    {
        // Only an explicit 0 means "nothing is recording". Everything else
        // — a future rc, a garbled value — errs toward keeping the process
        // alive.
        Assert.False(UpdateGate.MayEndProcess(2));
        Assert.False(UpdateGate.MayEndProcess(int.MinValue));
    }
}
