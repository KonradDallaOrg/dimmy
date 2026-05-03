using System;
using System.Text.Json;
using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests.Services;

/// <summary>
/// Tests for AppContextCapture — focused on the data-shape contracts
/// (privacy boundary, JSON shape) rather than the Win32 P/Invoke calls,
/// which require a real foreground window and can't be reproduced
/// reliably in a unit test process.
/// </summary>
public class AppContextCaptureTests
{
    [Fact]
    public void Empty_isMarkedEmpty()
    {
        Assert.True(AppContextCapture.CapturedTargetContext.Empty.IsEmpty);
    }

    [Fact]
    public void NonEmptyContext_isNotEmpty()
    {
        var ctx = new AppContextCapture.CapturedTargetContext(
            Hwnd: (IntPtr)0x1234,
            Pid: 4242,
            ProcessName: "notepad++.exe",
            ExecutablePath: @"C:\Program Files\Notepad++\notepad++.exe",
            ClassName: "Notepad++",
            WindowTitle: "main.cpp",
            CapturedAt: DateTimeOffset.UtcNow);

        Assert.False(ctx.IsEmpty);
    }

    /// <summary>
    /// Privacy contract — ToCoreJson must include ONLY categorical
    /// fields safe to cross the FFI boundary into the Rust core (which
    /// may surface them in telemetry signals later). It must NOT include
    /// the window title or the executable path because those can leak
    /// PII (file paths, email subjects, usernames).
    /// </summary>
    [Fact]
    public void ToCoreJson_includesOnlyCategoricalFields()
    {
        var ctx = new AppContextCapture.CapturedTargetContext(
            Hwnd: (IntPtr)0x1234,
            Pid: 4242,
            ProcessName: "slack.exe",
            ExecutablePath: @"C:\Users\konradd\AppData\Local\slack\app.exe",
            ClassName: "Chrome_WidgetWin_1",
            WindowTitle: "Re: Q3 budget — konrad@example.com",
            CapturedAt: DateTimeOffset.UtcNow);

        var json = ctx.ToCoreJson();
        using var doc = JsonDocument.Parse(json);
        var r = doc.RootElement;

        // Required: process_name (matches Rust AppContext.process_name)
        Assert.Equal("slack.exe", r.GetProperty("process_name").GetString());
        // Cross-platform fields: empty on Windows, set on Mac/Linux
        Assert.Equal("", r.GetProperty("bundle_id").GetString());
        Assert.Equal("", r.GetProperty("wm_class").GetString());

        // PII fields must NOT be present in the FFI payload
        Assert.False(r.TryGetProperty("window_title", out _),
            "ToCoreJson must NEVER include window_title — privacy boundary");
        Assert.False(r.TryGetProperty("executable_path", out _),
            "ToCoreJson must NEVER include executable_path — privacy boundary");
        Assert.False(r.TryGetProperty("hwnd", out _),
            "hwnd is process-local, has no meaning to the Rust core");
        Assert.False(r.TryGetProperty("pid", out _),
            "pid is process-local, has no meaning to the Rust core");
    }

    [Fact]
    public void ToCoreJson_emptyContext_producesEmptyProcessName()
    {
        var json = AppContextCapture.CapturedTargetContext.Empty.ToCoreJson();
        using var doc = JsonDocument.Parse(json);
        Assert.Equal("", doc.RootElement.GetProperty("process_name").GetString());
    }

    /// <summary>
    /// ToLogString is for ptt.log only — title is allowed (debugging
    /// the Notepad++ paste bug needs to know which window had focus),
    /// but truncated to 80 chars so a malicious window title can't
    /// flood the log.
    /// </summary>
    [Fact]
    public void ToLogString_includesAllFieldsForDebug()
    {
        var ctx = new AppContextCapture.CapturedTargetContext(
            Hwnd: (IntPtr)0xABCD,
            Pid: 1234,
            ProcessName: "notepad++.exe",
            ExecutablePath: @"C:\notepad.exe",
            ClassName: "Notepad++",
            WindowTitle: "config.toml",
            CapturedAt: DateTimeOffset.UtcNow);

        var s = ctx.ToLogString();
        Assert.Contains("ABCD", s);                  // hwnd hex
        Assert.Contains("1234", s);                  // pid
        Assert.Contains("notepad++.exe", s);         // proc
        Assert.Contains("Notepad++", s);             // class
        Assert.Contains("config.toml", s);           // title
    }

    [Fact]
    public void ToLogString_truncatesLongTitle()
    {
        var longTitle = new string('A', 200);
        var ctx = new AppContextCapture.CapturedTargetContext(
            Hwnd: (IntPtr)1, Pid: 1, ProcessName: "x.exe",
            ExecutablePath: "", ClassName: "X", WindowTitle: longTitle,
            CapturedAt: DateTimeOffset.UtcNow);

        var s = ctx.ToLogString();
        // Truncated form uses the ellipsis character at the cap (max=80).
        Assert.Contains("…", s);
        // The full 200-char title MUST NOT survive — log lines should
        // stay readable and not get flooded by a hostile window title.
        Assert.DoesNotContain(longTitle, s);
    }

    /// <summary>
    /// Smoke test: SnapshotForeground must never throw, even when the
    /// test process happens to have no usable foreground window. Worst
    /// case it returns a partially-filled record. The hot-path contract
    /// is "best effort, never break recording."
    /// </summary>
    [Fact]
    public void SnapshotForeground_neverThrows()
    {
        var snap = AppContextCapture.SnapshotForeground();
        Assert.NotNull(snap);
        // We can't assert specific values (depends on test runner UI
        // state), but the call must complete without exception.
    }
}
