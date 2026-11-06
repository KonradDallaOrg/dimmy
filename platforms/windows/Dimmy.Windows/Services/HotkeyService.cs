using System;
using System.IO;
using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

/// <summary>
/// Global hotkey service powered by the Rust core's low-level keyboard hook.
/// Every press and release arrives as one <c>hotkey</c> event through the
/// core's event callback (<see cref="HotkeyEvents"/>), on the UI thread: no
/// polling, no edge lost between two reads. The PowerToys shape: the hook
/// only queues, a thread of its own hands the edge over.
/// Supports modifier-only combos (Win+Alt) and modifier+key combos (Ctrl+Shift+X).
/// </summary>
public class HotkeyService : IDisposable
{
    private bool _installed;
    private bool _subscribed;

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

        if (!_subscribed)
        {
            HotkeyEvents.Received += OnEdge;
            _subscribed = true;
        }
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

    /// <summary>One press or release from the core. Runs on the UI thread
    /// (AppViewModel.HandleEvent), so handlers are invoked directly.</summary>
    private void OnEdge(string binding, bool pressed)
    {
        switch (binding)
        {
            case "dictation":
                if (pressed && !IgnoredOnLockScreen("dictation"))
                {
                    Log($"EVENT: pressed (PttMode={PttMode})");
                    HotkeyPressed?.Invoke();
                }
                else if (!pressed)
                {
                    Log($"EVENT: released (PttMode={PttMode})");
                    if (PttMode) HotkeyReleased?.Invoke();
                }
                break;
            case "command":
                if (pressed && !IgnoredOnLockScreen("command"))
                {
                    Log($"CMD EVENT: pressed (PttMode={PttMode})");
                    CommandHotkeyPressed?.Invoke();
                }
                else if (!pressed)
                {
                    Log($"CMD EVENT: released (PttMode={PttMode})");
                    if (PttMode) CommandHotkeyReleased?.Invoke();
                }
                break;
            case "meeting":
                // TOGGLE-only: fire on pressed, ignore released regardless of
                // PttMode (a meeting can't be push-to-talk).
                if (pressed && !IgnoredOnLockScreen("meeting"))
                {
                    Log("MTNG EVENT: pressed (toggle)");
                    MeetingHotkeyPressed?.Invoke();
                }
                break;
        }
    }

    /// <summary>A press while Windows is locked never starts anything: it
    /// would open the microphone behind the lock screen and paste into it.
    /// Burned 2026-10-20 (2.5 min recorded on the lock screen, then Ctrl+V
    /// into lockapp.exe). Releases still flow so a running PTT can stop.</summary>
    private static bool IgnoredOnLockScreen(string which)
    {
        var proc = Helpers.AppContextCapture.GetForegroundProcessName();
        if (proc != "lockapp.exe" && proc != "logonui.exe")
            return false;
        Log($"{which} hotkey ignored on the lock screen ({proc})");
        return true;
    }

    // ── Shortcut parsing (kept for compatibility with settings UI) ──
    public static (uint modifiers, uint vk) ParseShortcut(string shortcut)
        => HotkeyParser.ParseShortcut(shortcut);

    public void Dispose()
    {
        if (_subscribed)
        {
            HotkeyEvents.Received -= OnEdge;
            _subscribed = false;
        }
        GC.SuppressFinalize(this);
    }
}
