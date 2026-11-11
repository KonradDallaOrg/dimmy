using System;
using System.Threading.Tasks;
using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests.Helpers;

/// <summary>
/// Pins what a host exception sends to Sentry: the type and the frames,
/// never the message (it can carry paths and user text).
/// </summary>
public class CrashFramesTests
{
    private static Exception Thrown(string message)
    {
        try { throw new InvalidOperationException(message); }
        catch (Exception ex) { return ex; }
    }

    [Fact]
    public void Frames_NameTheThrowingMethod_AndNeverTheMessage()
    {
        var ex = Thrown("C:\\Users\\mario\\secret transcript text");
        var frames = CrashFrames.Format(ex);
        Assert.Contains(nameof(Thrown), frames);
        Assert.DoesNotContain("mario", frames);
        Assert.DoesNotContain("secret transcript", frames);
        foreach (var line in frames.Split('\n', StringSplitOptions.RemoveEmptyEntries))
            Assert.Equal(3, line.Split('\t').Length);
    }

    [Fact]
    public void UnobservedTask_ReportsTheInnerException()
    {
        var agg = new AggregateException(new AggregateException(Thrown("x")));
        var root = CrashFrames.Unwrap(agg);
        Assert.Equal("System.InvalidOperationException", CrashFrames.TypeName(root));
    }

    [Fact]
    public void AnExceptionThatWasNeverThrown_HasNoFrames()
    {
        Assert.Equal("", CrashFrames.Format(new Exception("never thrown")));
    }
}
