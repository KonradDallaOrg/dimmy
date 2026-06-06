using System;
using System.Runtime.InteropServices;
using System.Threading.Tasks;

namespace Dimmy.Windows.Services;

public static class TextInjectionService
{
    [DllImport("user32.dll", SetLastError = true)]
    private static extern uint SendInput(uint nInputs, INPUT[] pInputs, int cbSize);

    [DllImport("user32.dll")]
    private static extern uint MapVirtualKey(uint uCode, uint uMapType);

    [DllImport("user32.dll")]
    private static extern short GetAsyncKeyState(int vKey);

    /// MAPVK_VK_TO_VSC — translate a virtual key to a scan code.
    private const uint MAPVK_VK_TO_VSC = 0;

    /// All modifier virtual-key codes we want to defensively release
    /// before sending a synthetic Ctrl+V. Hotkey hooks (PTT) sometimes
    /// leave the OS thinking a modifier is still pressed even after
    /// the user has released it — when that happens, our Ctrl+V gets
    /// merged with the held modifier and the chord (e.g. Ctrl+Win+Alt+V)
    /// isn't recognised by the target app, producing the Windows
    /// "default beep" instead of a paste.
    private static readonly ushort[] ModifierVks = new ushort[]
    {
        0x5B, // VK_LWIN
        0x5C, // VK_RWIN
        0xA0, // VK_LSHIFT
        0xA1, // VK_RSHIFT
        0xA2, // VK_LCONTROL
        0xA3, // VK_RCONTROL
        0xA4, // VK_LMENU (Left Alt)
        0xA5, // VK_RMENU (Right Alt / AltGr)
    };

    [DllImport("user32.dll")]
    private static extern bool OpenClipboard(IntPtr hWndNewOwner);

    [DllImport("user32.dll")]
    private static extern bool CloseClipboard();

    [DllImport("user32.dll")]
    private static extern bool EmptyClipboard();

    [DllImport("user32.dll")]
    private static extern IntPtr SetClipboardData(uint uFormat, IntPtr hMem);

    [DllImport("user32.dll")]
    private static extern IntPtr GetClipboardData(uint uFormat);

    [DllImport("user32.dll")]
    private static extern bool IsClipboardFormatAvailable(uint format);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GlobalAlloc(uint uFlags, UIntPtr dwBytes);

    [DllImport("kernel32.dll")]
    private static extern IntPtr GlobalLock(IntPtr hMem);

    [DllImport("kernel32.dll")]
    private static extern bool GlobalUnlock(IntPtr hMem);

    [DllImport("kernel32.dll")]
    private static extern UIntPtr GlobalSize(IntPtr hMem);

    private const uint CF_UNICODETEXT = 13;
    private const uint GMEM_MOVEABLE = 0x0002;

    private const int INPUT_KEYBOARD = 1;
    private const ushort VK_CONTROL = 0x11;
    private const ushort VK_V = 0x56;
    private const uint KEYEVENTF_KEYUP = 0x0002;
    private const uint KEYEVENTF_UNICODE = 0x0004;

    // Correct struct layout for SendInput on x64
    [StructLayout(LayoutKind.Sequential)]
    private struct INPUT
    {
        public uint type;
        public INPUTUNION u;
    }

    // Union must be sized to the largest member (MOUSEINPUT = 32 bytes on x64)
    [StructLayout(LayoutKind.Explicit)]
    private struct INPUTUNION
    {
        [FieldOffset(0)] public KEYBDINPUT ki;
        [FieldOffset(0)] public MOUSEINPUT mi;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct KEYBDINPUT
    {
        public ushort wVk;
        public ushort wScan;
        public uint dwFlags;
        public uint time;
        public IntPtr dwExtraInfo;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct MOUSEINPUT
    {
        public int dx;
        public int dy;
        public uint mouseData;
        public uint dwFlags;
        public uint time;
        public IntPtr dwExtraInfo;
    }

    /// <summary>
    /// Paste text using Win32 clipboard + SendInput(Ctrl+V).
    /// Same approach as the Tauri version: save clipboard → set text → Ctrl+V → restore.
    /// </summary>
    public static async Task PasteText(string text, bool keepInClipboard = false)
    {
        // Save current clipboard text (only if we plan to restore it)
        string? previousText = null;
        if (!keepInClipboard && OpenClipboard(IntPtr.Zero))
        {
            try
            {
                if (IsClipboardFormatAvailable(CF_UNICODETEXT))
                {
                    var hData = GetClipboardData(CF_UNICODETEXT);
                    if (hData != IntPtr.Zero)
                    {
                        var ptr = GlobalLock(hData);
                        if (ptr != IntPtr.Zero)
                        {
                            previousText = Marshal.PtrToStringUni(ptr);
                            GlobalUnlock(hData);
                        }
                    }
                }
            }
            finally { CloseClipboard(); }
        }

        // Set our text to clipboard
        SetClipboardText(text);

        // Small delay for clipboard to settle
        await Task.Delay(50);

        // Defensively release any modifier key the OS might still
        // consider held. PTT hotkey hooks intermittently swallow the
        // KEY_UP event of a Win+Alt-style chord, so when our Ctrl+V
        // arrives Windows merges it with the phantom-held modifier
        // and the target app sees a chord it doesn't know — producing
        // the user-reported "Windows beep + clipboard has the text
        // but Notepad++ didn't paste". GetAsyncKeyState's 0x8000 bit
        // tells us if the key is currently down.
        var modifierReleases = new System.Collections.Generic.List<INPUT>();
        var releasedMods = new System.Collections.Generic.List<string>();
        foreach (var vk in ModifierVks)
        {
            if ((GetAsyncKeyState(vk) & 0x8000) != 0)
            {
                modifierReleases.Add(MakeKeyUp(vk));
                releasedMods.Add($"0x{vk:X2}");
            }
        }
        if (modifierReleases.Count > 0)
        {
            var mods = modifierReleases.ToArray();
            SendInput((uint)mods.Length, mods, Marshal.SizeOf<INPUT>());
            // Tiny pause for the synthetic releases to land before
            // we send Ctrl+V; without it the new chord can race the
            // previous release in the input queue.
            await Task.Delay(20);
            try
            {
                var line = $"[{DateTime.Now:HH:mm:ss.fff}] [PTT] released phantom-held modifiers: " +
                           string.Join(",", releasedMods);
                var path = System.IO.Path.Combine(
                    Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                    "dimmy", "ptt.log");
                System.IO.File.AppendAllText(path, line + Environment.NewLine);
            }
            catch { }
        }

        // Send Ctrl+V
        var inputs = new INPUT[]
        {
            MakeKeyDown(VK_CONTROL),
            MakeKeyDown(VK_V),
            MakeKeyUp(VK_V),
            MakeKeyUp(VK_CONTROL),
        };
        uint sent = SendInput((uint)inputs.Length, inputs, Marshal.SizeOf<INPUT>());
        var lastErr = sent < inputs.Length ? Marshal.GetLastWin32Error() : 0;
        // Mirror to ptt.log so the diagnostic is visible in the same
        // file as PRESS/PASTE markers (Debug output is invisible without
        // a debugger attached).
        try
        {
            var line = $"[{DateTime.Now:HH:mm:ss.fff}] [PTT] SendInput Ctrl+V: sent={sent}/{inputs.Length}" +
                       (lastErr != 0 ? $" lastError={lastErr}" : "");
            var path = System.IO.Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "dimmy", "ptt.log");
            System.IO.File.AppendAllText(path, line + Environment.NewLine);
        }
        catch { }

        // Restore after 150ms (only if keepInClipboard is false)
        if (!keepInClipboard)
        {
            await Task.Delay(150);
            if (previousText != null)
            {
                SetClipboardText(previousText);
            }
        }
    }

    /// <summary>
    /// Type text directly at the cursor via SendInput with
    /// KEYEVENTF_UNICODE — no clipboard involved. Used by realtime
    /// streaming dictation to inject each finalised segment as you speak,
    /// where the clipboard save/set/restore churn of <see cref="PasteText"/>
    /// per segment would thrash the user's clipboard and feel laggy.
    /// Each UTF-16 code unit becomes a down+up Unicode key event (wVk=0,
    /// wScan=char), which correctly handles surrogate pairs. Synchronous —
    /// SendInput returns once the events are queued.
    /// </summary>
    public static void TypeUnicodeText(string text)
    {
        if (string.IsNullOrEmpty(text)) return;

        var inputs = new INPUT[text.Length * 2];
        int idx = 0;
        foreach (char c in text)
        {
            inputs[idx++] = MakeUnicodeKey(c, keyUp: false);
            inputs[idx++] = MakeUnicodeKey(c, keyUp: true);
        }
        SendInput((uint)inputs.Length, inputs, Marshal.SizeOf<INPUT>());
    }

    private static INPUT MakeUnicodeKey(char c, bool keyUp) => new()
    {
        type = (uint)INPUT_KEYBOARD,
        u = new INPUTUNION
        {
            ki = new KEYBDINPUT
            {
                wVk = 0,
                wScan = c,
                dwFlags = KEYEVENTF_UNICODE | (keyUp ? KEYEVENTF_KEYUP : 0),
            }
        }
    };

    private static void SetClipboardText(string text)
    {
        if (!OpenClipboard(IntPtr.Zero)) return;
        try
        {
            EmptyClipboard();
            var chars = text.ToCharArray();
            var bytes = (chars.Length + 1) * 2; // UTF-16 + null terminator
            var hGlobal = GlobalAlloc(GMEM_MOVEABLE, (UIntPtr)bytes);
            if (hGlobal == IntPtr.Zero) return;

            var ptr = GlobalLock(hGlobal);
            if (ptr != IntPtr.Zero)
            {
                Marshal.Copy(chars, 0, ptr, chars.Length);
                // Null terminator
                Marshal.WriteInt16(ptr + chars.Length * 2, 0);
                GlobalUnlock(hGlobal);
            }
            SetClipboardData(CF_UNICODETEXT, hGlobal);
            // Don't free hGlobal — clipboard owns it now
        }
        finally { CloseClipboard(); }
    }

    /// <summary>
    /// Build a synthetic key event with BOTH virtual-key code and scan
    /// code populated. Sending only `wVk` (no `wScan`) is silently
    /// dropped by some apps that filter "fake" injected events:
    /// Electron-based (VS Code, Slack, Discord), Chrome/Edge with raw
    /// input enabled, several IME-aware apps. The hardware-driver
    /// pathway always sets BOTH fields, so doing the same maximises
    /// compatibility. Setting only one was the root cause of the
    /// "paste fails in random apps" bug.
    /// </summary>
    private static INPUT MakeKeyDown(ushort vk) => new()
    {
        type = (uint)INPUT_KEYBOARD,
        u = new INPUTUNION { ki = new KEYBDINPUT
        {
            wVk = vk,
            wScan = (ushort)MapVirtualKey(vk, MAPVK_VK_TO_VSC),
        }}
    };

    private static INPUT MakeKeyUp(ushort vk) => new()
    {
        type = (uint)INPUT_KEYBOARD,
        u = new INPUTUNION { ki = new KEYBDINPUT
        {
            wVk = vk,
            wScan = (ushort)MapVirtualKey(vk, MAPVK_VK_TO_VSC),
            dwFlags = KEYEVENTF_KEYUP,
        }}
    };

    /// <summary>Synthesize a Ctrl+C keystroke against the currently-
    /// focused app. Used by the dictionary hotkey to copy whatever the
    /// user has selected so we can read it from the clipboard.
    ///
    /// **Phantom-modifier release.** This runs WHILE the user is
    /// physically holding the dict combo (Ctrl+Shift+D by default) —
    /// WM_HOTKEY fires on the press edge. If we just SendInput
    /// Ctrl+C, the OS merges our synthetic Ctrl with the user's
    /// already-down Shift and the target app sees `Ctrl+Shift+C`,
    /// which most apps do NOT interpret as copy (Notepad++:
    /// comment-toggle; VS: comment-line). Result: clipboard stays
    /// stale and the caller reads garbage from an earlier copy.
    /// Same root cause as the Win+Alt paste-bug fixed for Ctrl+V in
    /// SendInput above (line 150 comment). The defense is identical:
    /// release every modifier the OS currently considers down, wait
    /// 20 ms for the releases to land, then send a clean Ctrl+C.
    ///
    /// Returns true iff all SendInput calls succeeded (sent == count
    /// for both the phantom-release batch AND the Ctrl+C batch).</summary>
    public static bool SendCtrlC()
    {
        var held = new System.Collections.Generic.List<ushort>(ModifierVks.Length);
        foreach (var vk in ModifierVks)
        {
            if ((GetAsyncKeyState(vk) & 0x8000) != 0) held.Add(vk);
        }
        var plan = PlanCtrlCBatch(held);

        // Single atomic SendInput batch: phantom-modifier releases
        // FIRST, then a clean Ctrl+C. The Win32 SendInput docs
        // guarantee "these events are not interspersed with other
        // keyboard or mouse input events" — so the target app sees
        // every release land before Ctrl-down arrives, no race, no
        // wall-clock sleep between phases. Replaces the previous
        // two-SendInput + Thread.Sleep(20) pattern.
        var arr = new INPUT[plan.Count];
        for (int i = 0; i < plan.Count; i++)
        {
            var (vk, isUp) = plan[i];
            arr[i] = isUp ? MakeKeyUp(vk) : MakeKeyDown(vk);
        }
        uint sent = SendInput((uint)arr.Length, arr, Marshal.SizeOf<INPUT>());
        return sent == (uint)arr.Length;
    }

    /// <summary>
    /// Plan the Ctrl+C SendInput batch given the modifier VKs the OS
    /// currently considers held. Pure function — no Win32 calls — so
    /// the ordering invariants the dictionary-hotkey path depends on
    /// can be unit tested without standing up a window or a hook:
    ///
    ///   1. EVERY held modifier is released FIRST (else our synthetic
    ///      Ctrl gets merged with the user's still-down Shift / Win /
    ///      etc. — the chord most apps reject as copy).
    ///   2. Releases preserve caller order (so callers can choose a
    ///      stable order if they want; we feed them in <c>ModifierVks</c>
    ///      array order which is L-modifiers before R-modifiers).
    ///   3. Then exactly Ctrl-down, C-down, C-up, Ctrl-up — every key
    ///      is balanced (no dangling presses left behind).
    /// </summary>
    public static System.Collections.Generic.IReadOnlyList<(ushort Vk, bool IsUp)>
        PlanCtrlCBatch(System.Collections.Generic.IEnumerable<ushort> heldModifierVks)
    {
        const ushort VK_C = 0x43;
        var list = new System.Collections.Generic.List<(ushort, bool)>(12);
        foreach (var vk in heldModifierVks ?? System.Array.Empty<ushort>())
            list.Add((vk, true));
        list.Add((VK_CONTROL, false));
        list.Add((VK_C, false));
        list.Add((VK_C, true));
        list.Add((VK_CONTROL, true));
        return list;
    }

    [DllImport("user32.dll")]
    private static extern uint GetClipboardSequenceNumber();

    /// <summary>Current OS clipboard sequence number. Increments on
    /// every write (any process). Used to deterministically wait for
    /// a clipboard update instead of sleeping a fixed wall-clock
    /// duration. Returns 0 if the caller lacks GENERIC_READ on the
    /// clipboard window station — practically zero on user desktop
    /// processes, but the caller should handle 0 as "polling
    /// unsupported, fall back to a bounded wait".</summary>
    public static uint ClipboardSequenceNumber() => GetClipboardSequenceNumber();
}
