using System;
using System.IO;
using System.Linq;
using Dimmy.Windows.Helpers;
using Xunit;

namespace Dimmy.Windows.Tests.Helpers;

/// <summary>
/// Pins the size-capped log rotation added for the audit-2026-07-02
/// blocker (ptt.log grew unbounded — 7.7 MB observed on a real machine).
/// Policy mirrors the Rust core: over the cap → keep the newest half,
/// cut at a line boundary.
/// </summary>
public class LogRotationTests : IDisposable
{
    private readonly string _path = Path.Combine(
        Path.GetTempPath(), $"dimmy-logrot-test-{Guid.NewGuid():N}.log");

    public void Dispose()
    {
        try { File.Delete(_path); } catch { }
    }

    [Fact]
    public void UnderCap_FileUntouched()
    {
        File.WriteAllText(_path, "line1\nline2\n");
        var before = File.ReadAllText(_path);
        LogRotation.TrimToHalfIfOver(_path, maxBytes: 1_000_000);
        Assert.Equal(before, File.ReadAllText(_path));
    }

    [Fact]
    public void MissingFile_NoThrow()
    {
        LogRotation.TrimToHalfIfOver(_path + ".missing", maxBytes: 10);
    }

    [Fact]
    public void OverCap_KeepsNewestHalf_AtLineBoundary()
    {
        // 2000 numbered lines ≈ 22 KB; cap at 1 KB forces a trim.
        var lines = Enumerable.Range(0, 2000).Select(i => $"line-{i:D6}");
        File.WriteAllLines(_path, lines);
        var sizeBefore = new FileInfo(_path).Length;

        LogRotation.TrimToHalfIfOver(_path, maxBytes: 1024);

        var after = File.ReadAllLines(_path);
        Assert.True(new FileInfo(_path).Length < sizeBefore, "file must shrink");
        // Newest content survives, oldest is gone.
        Assert.Equal("line-001999", after[^1]);
        Assert.DoesNotContain("line-000000", after);
        // Cut landed on a line boundary: first surviving line is intact.
        Assert.Matches("^line-\\d{6}$", after[0]);
    }
}
