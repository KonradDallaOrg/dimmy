using System;
using System.IO;
using System.Threading;
using Dimmy.Windows.Interop;
using Microsoft.UI.Dispatching;

namespace Dimmy.Windows.Services;

/// <summary>
/// Global hotkey service powered by the Rust core's low-level keyboard hook.
/// Polls dimmy_hotkey_take_event() on a background thread for press/release events.
/// Supports modifier-only combos (Win+Alt) and modifier+key combos (Ctrl+Shift+X).
/// </summary>
public class HotkeyService : IDisposable
{
    private readonly DispatcherQueue _dispatcher;
    private Thread? _pollThread;
    private volatile bool _polling;
    private bool _installed;

    public event Action? HotkeyPressed;
    public event Action? HotkeyReleased;

    /// <summary>Whether PTT mode is active. Set by the caller before Register.</summary>
    public bool PttMode { get; set; }

    private static readonly string LogPath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "dimmy", "ptt.log");

    private static void Log(string msg)
    {
        var line = $"[{DateTime.Now:HH:mm:ss.fff}] [Hotkey] {msg}";
        Console.WriteLine(line);
        Console.Out.Flush();
        try { File.AppendAllText(LogPath, line + Environment.NewLine); } catch { }
    }

    public HotkeyService(DispatcherQueue dispatcher)
    {
        _dispatcher = dispatcher;
    }

    public void Register(string shortcut)
    {
        // Install the Rust keyboard hook once
        if (!_installed)
        {
            DimmyNative.dimmy_hotkey_install();
            _installed = true;
            Log("Rust keyboard hook installed");
        }

        // Set the shortcut combo in Rust
        DimmyNative.dimmy_hotkey_set(shortcut);
        Log($"Register(\"{shortcut}\") via Rust FFI");

        // Start polling for events
        StopPolling();
        _polling = true;
        _pollThread = new Thread(PollLoop) { IsBackground = true, Name = "DimmyHotkeyPoll" };
        _pollThread.Start();
    }

    private void StopPolling()
    {
        _polling = false;
        _pollThread = null;
    }

    private void PollLoop()
    {
        Log("Poll loop started");
        while (_polling)
        {
            int ev = DimmyNative.dimmy_hotkey_take_event();
            if (ev == 1)
            {
                Log($"EVENT: pressed (PttMode={PttMode})");
                _dispatcher.TryEnqueue(() => HotkeyPressed?.Invoke());
            }
            else if (ev == 2)
            {
                Log($"EVENT: released (PttMode={PttMode})");
                if (PttMode)
                    _dispatcher.TryEnqueue(() => HotkeyReleased?.Invoke());
            }
            Thread.Sleep(10);
        }
        Log("Poll loop exited");
    }

    // ── Shortcut parsing (kept for compatibility with settings UI) ──
    public static (uint modifiers, uint vk) ParseShortcut(string shortcut)
        => HotkeyParser.ParseShortcut(shortcut);

    public void Dispose()
    {
        StopPolling();
        GC.SuppressFinalize(this);
    }
}
