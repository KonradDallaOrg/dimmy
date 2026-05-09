using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using Dimmy.Windows.Interop;
using Dimmy.Windows.ViewModels;
using Microsoft.UI.Xaml;

namespace Dimmy.Windows.Services;

/// <summary>
/// Mirrors the macOS menu-bar status icon onto the Windows taskbar
/// button. The anchor window owns an HWND registered in the taskbar;
/// this service hangs an `ITaskbarList3` overlay icon + a colored
/// progress bar off that HWND so the recording pipeline state is
/// visible from the taskbar at a glance — exactly the UX the macOS
/// build gets for free with NSStatusBar tinting.
///
/// State → visual mapping (matches the menu-bar icon palette):
/// - Idle:        no overlay, no progress
/// - Recording:   red dot, indeterminate green progress
/// - Transcribing: blue dot, indeterminate progress
/// - Processing:  purple dot, indeterminate progress
/// - Completing:  green check, full normal progress
/// - Error:       yellow warning dot, red error progress
///
/// The overlay icons are drawn once at construction time as 16×16
/// ICOs in `%TEMP%\dimmy_taskbar_icons\` and loaded via `LoadImage`
/// — same pattern the existing `TrayService` uses for its fallback
/// icon. No bundled binary assets, no .csproj changes.
/// </summary>
public sealed class TaskbarService : IDisposable
{
    // ── ITaskbarList3 COM interop ─────────────────────────────────────

    [ComImport, Guid("56FDF344-FD6D-11d0-958A-006097C9A090"), ClassInterface(ClassInterfaceType.None)]
    private class CTaskbarList { }

    [ComImport, Guid("ea1afb91-9e28-4b86-90e9-9e9f8a5eefaf"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface ITaskbarList3
    {
        // ITaskbarList
        [PreserveSig] int HrInit();
        [PreserveSig] int AddTab(IntPtr hwnd);
        [PreserveSig] int DeleteTab(IntPtr hwnd);
        [PreserveSig] int ActivateTab(IntPtr hwnd);
        [PreserveSig] int SetActiveAlt(IntPtr hwnd);
        // ITaskbarList2
        [PreserveSig] int MarkFullscreenWindow(IntPtr hwnd, [MarshalAs(UnmanagedType.Bool)] bool fullscreen);
        // ITaskbarList3
        [PreserveSig] int SetProgressValue(IntPtr hwnd, ulong completed, ulong total);
        [PreserveSig] int SetProgressState(IntPtr hwnd, TBPF state);
        [PreserveSig] int RegisterTab(IntPtr hwndTab, IntPtr hwndMDI);
        [PreserveSig] int UnregisterTab(IntPtr hwndTab);
        [PreserveSig] int SetTabOrder(IntPtr hwndTab, IntPtr hwndInsertBefore);
        [PreserveSig] int SetTabActive(IntPtr hwndTab, IntPtr hwndMDI, uint dwReserved);
        [PreserveSig] int ThumbBarAddButtons(IntPtr hwnd, uint cButtons, IntPtr pButton);
        [PreserveSig] int ThumbBarUpdateButtons(IntPtr hwnd, uint cButtons, IntPtr pButton);
        [PreserveSig] int ThumbBarSetImageList(IntPtr hwnd, IntPtr himl);
        [PreserveSig] int SetOverlayIcon(IntPtr hwnd, IntPtr hIcon, [MarshalAs(UnmanagedType.LPWStr)] string? description);
        [PreserveSig] int SetThumbnailTooltip(IntPtr hwnd, [MarshalAs(UnmanagedType.LPWStr)] string? tip);
        [PreserveSig] int SetThumbnailClip(IntPtr hwnd, IntPtr prcClip);
    }

    [Flags]
    private enum TBPF : uint
    {
        NOPROGRESS = 0,
        INDETERMINATE = 1,
        NORMAL = 2,
        ERROR = 4,
        PAUSED = 8,
    }

    [DllImport("user32.dll")]
    private static extern IntPtr LoadImage(IntPtr hInst, string name, uint type,
        int cx, int cy, uint fuLoad);

    [DllImport("user32.dll")]
    private static extern bool DestroyIcon(IntPtr hIcon);

    private const uint IMAGE_ICON = 1;
    private const uint LR_LOADFROMFILE = 0x0010;
    private const uint LR_DEFAULTSIZE = 0x0040;

    // ── Instance state ───────────────────────────────────────────────

    private readonly ITaskbarList3? _taskbar;
    private readonly IntPtr _hwnd;
    private readonly Dictionary<AppState, IntPtr> _hicons = new();
    private bool _disposed;

    // Amplitude-driven progress bar during Recording. The Rust core
    // exposes the current peak via `dimmy_get_amplitude` (already
    // polled by the pill at 12 Hz for its waveform). We mirror the
    // same cadence here so the taskbar bar pulses with the user's
    // voice — a free VU meter that's visible even when the pill is
    // hidden.
    private DispatcherTimer? _ampTimer;
    private float _ampDisplayPeak = 0.05f;

    public TaskbarService(IntPtr anchorHwnd)
    {
        _hwnd = anchorHwnd;
        try
        {
            _taskbar = (ITaskbarList3)new CTaskbarList();
            _taskbar.HrInit();
        }
        catch (Exception ex)
        {
            // Pre-Win7 or COM init failure — degrade silently. The rest
            // of the app keeps working; the taskbar overlay is a polish
            // feature, not load-bearing.
            System.Diagnostics.Debug.WriteLine($"[TaskbarService] HrInit failed: {ex.Message}");
            _taskbar = null;
        }

        EnsureStateIcons();
    }

    /// <summary>Apply overlay icon + progress bar matching the given state.</summary>
    public void UpdateState(AppState state)
    {
        if (_taskbar is null || _disposed) return;

        // Idle clears the overlay so the user gets the clean Dimmy icon
        // when nothing is happening.
        var hIcon = state == AppState.Idle ? IntPtr.Zero
                  : _hicons.TryGetValue(state, out var v) ? v : IntPtr.Zero;
        var description = state == AppState.Idle ? null : DescribeState(state);

        try { _taskbar.SetOverlayIcon(_hwnd, hIcon, description); }
        catch (Exception ex) { System.Diagnostics.Debug.WriteLine($"[TaskbarService] SetOverlayIcon: {ex.Message}"); }

        try { _taskbar.SetProgressState(_hwnd, ProgressFor(state)); }
        catch (Exception ex) { System.Diagnostics.Debug.WriteLine($"[TaskbarService] SetProgressState: {ex.Message}"); }

        // State-specific progress-value handling:
        // - Recording: amplitude-driven VU meter (timer polls at 12 Hz)
        // - Transcribing / Processing: indeterminate animation owned by Windows
        // - Completing: solid full bar — momentary green flash before
        //   the next transition clears it
        // - Idle / Error: no value (state alone tells the story)
        if (state == AppState.Recording)
        {
            _ampDisplayPeak = 0.05f; // reset display AGC at recording start
            StartAmplitudeTimer();
        }
        else
        {
            StopAmplitudeTimer();
            if (state == AppState.Completing)
            {
                try { _taskbar.SetProgressValue(_hwnd, 100, 100); }
                catch { }
            }
        }
    }

    private static TBPF ProgressFor(AppState s) => s switch
    {
        // Recording uses NORMAL (deterministic green bar) so we can
        // drive the value from amplitude. INDETERMINATE would ignore
        // SetProgressValue and play Windows' default sweep instead.
        AppState.Recording => TBPF.NORMAL,
        AppState.Transcribing => TBPF.INDETERMINATE,
        AppState.Processing => TBPF.INDETERMINATE,
        AppState.Completing => TBPF.NORMAL,
        AppState.Error => TBPF.ERROR,
        _ => TBPF.NOPROGRESS,
    };

    private void StartAmplitudeTimer()
    {
        if (_ampTimer is null)
        {
            _ampTimer = new DispatcherTimer
            {
                // 12 Hz mirrors the pill's amplitude poll so both
                // surfaces stay perceptually in sync. Higher rates
                // burn CPU on no visible benefit at this bar's
                // resolution (Windows 11 taskbar updates at 60 Hz max).
                Interval = TimeSpan.FromMilliseconds(1000.0 / 12),
            };
            _ampTimer.Tick += AmpTimerTick;
        }
        _ampTimer.Start();
    }

    private void StopAmplitudeTimer() => _ampTimer?.Stop();

    private void AmpTimerTick(object? sender, object e)
    {
        if (_taskbar is null || _disposed) return;
        // Always-mix: taskbar bar reacts to whichever source has signal,
        // mirroring the pill's max(mic, sys) behaviour. Without this the
        // user sees a flat taskbar bar during a meeting where they're
        // listening (system audio playing, mic silent) — confusing
        // because the pill IS animating.
        float ampMic, ampSys;
        try
        {
            ampMic = DimmyNative.dimmy_get_amplitude();
            ampSys = DimmyNative.dimmy_get_loopback_amplitude();
        }
        catch { return; }
        if (!float.IsFinite(ampMic)) ampMic = 0;
        if (!float.IsFinite(ampSys)) ampSys = 0;
        float amp = Math.Max(ampMic, ampSys);

        // Display AGC — same algorithm the pill's waveform uses.
        // Fast attack so peaks land immediately; slow release so quiet
        // speech keeps the bar alive instead of pumping.
        if (amp > _ampDisplayPeak)
            _ampDisplayPeak += (amp - _ampDisplayPeak) * 0.3f;
        else
            _ampDisplayPeak *= 0.995f;
        if (_ampDisplayPeak < 0.01f) _ampDisplayPeak = 0.01f;

        var normalized = Math.Clamp(amp / _ampDisplayPeak, 0f, 1f);
        var value = (ulong)(normalized * 100.0);
        try { _taskbar.SetProgressValue(_hwnd, value, 100); }
        catch { }
    }

    private static string DescribeState(AppState s) => s switch
    {
        AppState.Recording => "Dimmy: Recording",
        AppState.Transcribing => "Dimmy: Transcribing",
        AppState.Processing => "Dimmy: LLM",
        AppState.Completing => "Dimmy: Done",
        AppState.Error => "Dimmy: Error",
        _ => "Dimmy",
    };

    // ── HICON generation ─────────────────────────────────────────────

    /// <summary>Generate (or load cached) state icons. Renders at 32×32
    /// with 4×4 super-sampled anti-aliasing — Windows downscales the
    /// 32×32 alpha-blended source to the taskbar's 16×16 overlay slot
    /// with clean bilinear filtering, so the dot reads as a genuine
    /// circle (and not a hexagon, which is what 16×16 rasterised
    /// without AA gives you).</summary>
    private void EnsureStateIcons()
    {
        // Bumped dir name to v2 so existing temp ICOs from the previous
        // (jagged) implementation get regenerated on first launch.
        var dir = Path.Combine(Path.GetTempPath(), "dimmy_taskbar_icons_v2");
        try { Directory.CreateDirectory(dir); } catch { return; }

        // (state, BGR color triplet) — NB: Windows ICO is BGRA, not RGBA.
        // Colors mirror AppViewModel.StyleColors palette.
        var palette = new (AppState state, byte b, byte g, byte r)[]
        {
            (AppState.Recording,   0x44, 0x44, 0xEF), // #EF4444 red
            (AppState.Transcribing, 0xF8, 0xBD, 0x38), // #38BDF8 blue
            (AppState.Processing,  0xFA, 0x8B, 0xA7), // #A78BFA purple
            (AppState.Completing,  0x80, 0xDE, 0x4A), // #4ADE80 green
            (AppState.Error,       0x15, 0xCC, 0xFA), // #FACC15 yellow
        };

        const int RenderSize = 32;
        foreach (var (state, b, g, r) in palette)
        {
            var path = Path.Combine(dir, $"state_{state}.ico");
            try
            {
                if (!File.Exists(path))
                    File.WriteAllBytes(path, BuildCircleIco(RenderSize, b, g, r));
                // Load at 32×32 — Windows scales to the actual overlay
                // slot size (typically 16×16) using bilinear filtering
                // on the alpha channel, which gives a clean round dot.
                var hIcon = LoadImage(IntPtr.Zero, path, IMAGE_ICON, RenderSize, RenderSize,
                    LR_LOADFROMFILE | LR_DEFAULTSIZE);
                if (hIcon != IntPtr.Zero) _hicons[state] = hIcon;
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"[TaskbarService] icon for {state}: {ex.Message}");
            }
        }
    }

    /// <summary>Build a tiny ICO file in memory: an anti-aliased
    /// colored circle on transparent background. Uses 4×4 super-
    /// sampling — for each output pixel we count how many of 16
    /// sub-pixel positions land inside the circle and use that as the
    /// alpha. The result is a smooth round dot that survives the 32→16
    /// downscale Windows applies for the overlay slot.</summary>
    private static byte[] BuildCircleIco(int size, byte b, byte g, byte r)
    {
        var pixels = new byte[size * size * 4];
        double cx = size / 2.0, cy = size / 2.0;
        // Slightly under (size/2) so the dot has ~1px breathing room
        // — at 32×32 this is invisible, but it keeps the AA edge from
        // hitting the icon boundary and getting clipped on downscale.
        double rad = size / 2.0 - 1.0;
        double radSq = rad * rad;

        const int Samples = 4; // 4×4 = 16 subpixel samples per pixel
        for (int y = 0; y < size; y++)
        {
            for (int x = 0; x < size; x++)
            {
                int hits = 0;
                for (int sy = 0; sy < Samples; sy++)
                {
                    for (int sx = 0; sx < Samples; sx++)
                    {
                        double px = x + (sx + 0.5) / Samples - cx;
                        double py = y + (sy + 0.5) / Samples - cy;
                        if (px * px + py * py <= radSq) hits++;
                    }
                }
                if (hits == 0) continue;
                int i = (y * size + x) * 4;
                // Pre-multiplied alpha — Windows expects RGB scaled by
                // alpha for 32-bpp icons, otherwise the AA edge can
                // show as bright halos when blended over dark
                // backgrounds.
                int alpha = (hits * 255) / (Samples * Samples);
                pixels[i] = (byte)(b * alpha / 255);
                pixels[i + 1] = (byte)(g * alpha / 255);
                pixels[i + 2] = (byte)(r * alpha / 255);
                pixels[i + 3] = (byte)alpha;
            }
        }

        using var ms = new MemoryStream();
        using var bw = new BinaryWriter(ms);
        // ICONDIR
        bw.Write((short)0);  // reserved
        bw.Write((short)1);  // type = icon
        bw.Write((short)1);  // image count
        // ICONDIRENTRY
        bw.Write((byte)size);
        bw.Write((byte)size);
        bw.Write((byte)0);   // colors in palette (0 = >256)
        bw.Write((byte)0);   // reserved
        bw.Write((short)1);  // color planes
        bw.Write((short)32); // bits per pixel
        int dataSize = 40 + pixels.Length;
        bw.Write(dataSize);
        bw.Write(22);        // offset of image data
        // BITMAPINFOHEADER
        bw.Write(40);
        bw.Write(size);
        bw.Write(size * 2);  // height = 2× because ICO embeds AND mask
        bw.Write((short)1);
        bw.Write((short)32);
        bw.Write(0);         // BI_RGB
        bw.Write(pixels.Length);
        bw.Write(0); bw.Write(0); bw.Write(0); bw.Write(0);
        // Pixel data, bottom-up
        for (int y = size - 1; y >= 0; y--)
            bw.Write(pixels, y * size * 4, size * 4);
        return ms.ToArray();
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;

        StopAmplitudeTimer();
        if (_ampTimer is not null) _ampTimer.Tick -= AmpTimerTick;
        _ampTimer = null;

        // Clear overlay before releasing the COM ref so the next launch
        // doesn't inherit a stale state icon if Windows kept the
        // taskbar entry warm.
        try { _taskbar?.SetOverlayIcon(_hwnd, IntPtr.Zero, null); } catch { }
        try { _taskbar?.SetProgressState(_hwnd, TBPF.NOPROGRESS); } catch { }

        foreach (var hIcon in _hicons.Values)
        {
            if (hIcon != IntPtr.Zero) DestroyIcon(hIcon);
        }
        _hicons.Clear();

        if (_taskbar is not null) Marshal.ReleaseComObject(_taskbar);

        GC.SuppressFinalize(this);
    }
}
