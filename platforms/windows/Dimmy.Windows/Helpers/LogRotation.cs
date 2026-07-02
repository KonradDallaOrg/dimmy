using System.IO;

namespace Dimmy.Windows.Helpers;

/// <summary>
/// Size-capped rotation for append-only log files (ptt.log,
/// dimmy_startup.log). Before 2026-07-02 these grew unbounded — a real
/// machine showed a 7.7 MB ptt.log (audit blocker). Mirrors the Rust
/// core's dimmy.log policy (core/src/lib.rs): when the file exceeds the
/// cap, keep the newest half, cut at a line boundary. Best-effort —
/// logging must never throw.
/// </summary>
public static class LogRotation
{
    public static void TrimToHalfIfOver(string path, long maxBytes)
    {
        try
        {
            var info = new FileInfo(path);
            if (!info.Exists || info.Length <= maxBytes) return;
            var text = File.ReadAllText(path);
            var cut = text.IndexOf('\n', text.Length / 2);
            if (cut >= 0) File.WriteAllText(path, text[(cut + 1)..]);
        }
        catch { }
    }
}
