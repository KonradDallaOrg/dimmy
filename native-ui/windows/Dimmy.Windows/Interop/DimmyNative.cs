using System;
using System.Runtime.InteropServices;

namespace Dimmy.Windows.Interop;

/// <summary>
/// P/Invoke declarations for all 15 FFI functions exported by dimmy.dll (Rust cdylib).
/// See src-tauri/src/ffi.rs for the Rust side.
/// </summary>
public static class DimmyNative
{
    private const string DLL = "dimmy_lib";

    // ── Callback delegate ────────────────────────────────────────────
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)]
    public delegate void EventCallback(IntPtr jsonPtr);

    // ── Lifecycle ────────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_init();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_shutdown();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_set_event_callback(EventCallback cb);

    // ── Recording ────────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_start_recording();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_stop_recording(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_cancel_recording();

    // ── Config ───────────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_get_config_json(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_set_config_json(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string json);

    // ── Audio ────────────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern float dimmy_get_amplitude();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_list_devices_json(byte[] outBuf, int bufLen);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_check_audio_health(byte[] outBuf, int bufLen);

    // ── LLM ──────────────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_cycle_llm_style(int direction);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern void dimmy_cycle_llm_tone(int direction);

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_process_with_llm(
        [MarshalAs(UnmanagedType.LPUTF8Str)] string text,
        byte[] outBuf, int bufLen);

    // ── Stats ────────────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_update_stats(int words, double speakingSecs);

    // ── Utility ──────────────────────────────────────────────────────
    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_has_api_key();

    [DllImport(DLL, CallingConvention = CallingConvention.Cdecl)]
    public static extern int dimmy_is_recording();

    // ── Managed helpers ──────────────────────────────────────────────

    /// <summary>Read a buffer-returning FFI call into a C# string.</summary>
    public static string? ReadBuffer(Func<byte[], int, int> ffiCall, int bufSize = 8192)
    {
        var buf = new byte[bufSize];
        int len = ffiCall(buf, buf.Length);
        if (len < 0) return null;
        return System.Text.Encoding.UTF8.GetString(buf, 0, len);
    }

    /// <summary>Marshal the event callback JSON pointer to a C# string.</summary>
    public static string? MarshalEventJson(IntPtr jsonPtr)
    {
        if (jsonPtr == IntPtr.Zero) return null;
        return Marshal.PtrToStringUTF8(jsonPtr);
    }
}
