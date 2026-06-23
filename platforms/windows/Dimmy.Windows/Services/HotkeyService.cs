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

    /// <summary>Dedicated command-mode hotkey edges (optional, opt-in). Same
    /// Rust hook as the dictation hotkey, so it honours <see cref="PttMode"/>
    /// identically: in toggle mode only Pressed fires (press=start,
    /// press=stop); in PTT mode Pressed starts and Released stops.</summary>
    public event Action? CommandHotkeyPressed;
    public event Action? CommandHotkeyReleased;

    /// <summary>Dedicated meeting start/stop hotkey. TOGGLE-only by design — a
    /// meeting can't be push-to-talk (you can't hold a key for an hour), so
    /// only the pressed edge fires regardless of <see cref="PttMode"/>; the
    /// handler decides start-vs-stop from the live meeting state.</summary>
    public event Action? MeetingHotkeyPressed;

    /// <summary>Whether PTT mode is active. Set by the caller before Register.
    /// Drives BOTH the dictation and the command hotkey release semantics.</summary>
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

    /// <summary>Set (or clear) the optional dedicated command-mode shortcut on
    /// the same Rust hook. Empty/whitespace disables it. Safe to call before
    /// or after <see cref="Register"/> — the poll loop reads its events the
    /// moment they appear.</summary>
    public void SetCommandShortcut(string? combo)
    {
        var c = combo ?? "";
        DimmyNative.dimmy_hotkey_set_command(c);
        Log($"SetCommandShortcut(\"{c}\") via Rust FFI");
    }

    /// <summary>Set (or clear) the optional meeting start/stop shortcut on the
    /// same Rust hook. Empty/whitespace disables it.</summary>
    public void SetMeetingShortcut(string? combo)
    {
        var c = combo ?? "";
        DimmyNative.dimmy_hotkey_set_meeting(c);
        Log($"SetMeetingShortcut(\"{c}\") via Rust FFI");
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

            // Command-mode hotkey edges (same hook, same PTT/toggle rules).
            int cev = DimmyNative.dimmy_hotkey_take_command_event();
            if (cev == 1)
            {
                Log($"CMD EVENT: pressed (PttMode={PttMode})");
                _dispatcher.TryEnqueue(() => CommandHotkeyPressed?.Invoke());
            }
            else if (cev == 2)
            {
                Log($"CMD EVENT: released (PttMode={PttMode})");
                if (PttMode)
                    _dispatcher.TryEnqueue(() => CommandHotkeyReleased?.Invoke());
            }

            // Meeting hotkey: TOGGLE-only — fire on pressed, ignore released
            // regardless of PttMode (a meeting can't be push-to-talk).
            int mev = DimmyNative.dimmy_hotkey_take_meeting_event();
            if (mev == 1)
            {
                Log("MTNG EVENT: pressed (toggle)");
                _dispatcher.TryEnqueue(() => MeetingHotkeyPressed?.Invoke());
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
