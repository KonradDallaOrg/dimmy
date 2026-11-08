using System;
using System.Diagnostics;
using System.Threading;
using Dimmy.Windows.Interop;
using Microsoft.UI.Dispatching;

namespace Dimmy.Windows.Helpers;

/// <summary>
/// Sends host exceptions to the core's Sentry pipeline
/// (<c>dimmy_telemetry_capture_host_exception</c>). Until this existed a
/// C# crash reached only %TEMP%\dimmy_startup.log, which nobody but the
/// user can read. The core applies the crash-report consent, so nothing
/// leaves the machine for a user who did not say yes.
///
/// Privacy: the exception MESSAGE is never sent (it carries file paths and
/// user text). Only the type, where it was caught, and the stack frames.
/// </summary>
public static class CrashReporter
{
    /// <summary>
    /// Report an exception. <paramref name="source"/> is a fixed slug that
    /// says where it was caught (<c>appdomain_unhandled</c>,
    /// <c>update_check</c>, ...). <paramref name="fatal"/> blocks up to
    /// 2 s so the event is sent before the process dies. Never throws.
    /// </summary>
    public static void Report(Exception? ex, string source, bool fatal = false)
    {
        if (ex is null) return;
        try
        {
            var root = CrashFrames.Unwrap(ex);
            _ = DimmyNative.dimmy_telemetry_capture_host_exception(
                "windows", source, CrashFrames.TypeName(root),
                CrashFrames.Format(root), fatal ? 1 : 0);
        }
        catch
        {
            // The core DLL may not be loaded yet (a crash before dimmy_init),
            // and a reporter that throws inside a crash handler hides the crash.
        }
    }

    private static Thread? _watchdog;

    /// <summary>
    /// Report the UI thread when it stops answering for
    /// <see cref="HangThreshold"/>. One report per hang. A sleep or
    /// hibernate stalls this thread too, and is not a hang.
    /// </summary>
    public static void StartHangWatchdog(DispatcherQueue ui)
    {
#if DEBUG
        // A breakpoint is not a hang.
        return;
#else
        if (_watchdog is not null) return;
        _watchdog = new Thread(() => WatchdogLoop(ui))
        {
            IsBackground = true,
            Name = "dimmy-ui-watchdog",
            Priority = ThreadPriority.BelowNormal,
        };
        _watchdog.Start();
#endif
    }

    private static readonly TimeSpan HangThreshold = TimeSpan.FromSeconds(5);
    private static readonly TimeSpan Tick = TimeSpan.FromSeconds(1);

    private static void WatchdogLoop(DispatcherQueue ui)
    {
        long lastPong = Environment.TickCount64;
        bool reported = false;
        var clock = Stopwatch.StartNew();
        while (true)
        {
            var before = clock.Elapsed;
            Thread.Sleep(Tick);
            if (clock.Elapsed - before > Tick * 3)
            {
                // The machine was suspended: everything stalled, the UI too.
                lastPong = Environment.TickCount64;
                reported = false;
                continue;
            }
            ui.TryEnqueue(DispatcherQueuePriority.Low,
                () => Interlocked.Exchange(ref lastPong, Environment.TickCount64));
            var silent = Environment.TickCount64 - Interlocked.Read(ref lastPong);
            if (silent < HangThreshold.TotalMilliseconds)
            {
                reported = false;
            }
            else if (!reported)
            {
                reported = true;
                try
                {
                    _ = DimmyNative.dimmy_telemetry_capture_host_exception(
                        "windows", "ui_hang", "AppHang", null, 0);
                }
                catch { }
            }
        }
    }
}
