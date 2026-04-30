using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using Dimmy.Windows.ViewModels;

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

        // Completing state shows a full progress bar so the user gets a
        // tiny green flash before the bar disappears on the next state
        // transition. Recording / Transcribing / Processing use
        // INDETERMINATE which animates on its own.
        if (state == AppState.Completing)
        {
            try { _taskbar.SetProgressValue(_hwnd, 100, 100); }
            catch { }
        }
    }

    private static TBPF ProgressFor(AppState s) => s switch
    {
        AppState.Recording => TBPF.INDETERMINATE,
        AppState.Transcribing => TBPF.INDETERMINATE,
        AppState.Processing => TBPF.INDETERMINATE,
        AppState.Completing => TBPF.NORMAL,
        AppState.Error => TBPF.ERROR,
        _ => TBPF.NOPROGRESS,
    };

    private static string DescribeState(AppState s) => s switch
    {
        AppState.Recording => "Dimmy — Recording",
        AppState.Transcribing => "Dimmy — Transcribing",
        AppState.Processing => "Dimmy — LLM",
        AppState.Completing => "Dimmy — Done",
        AppState.Error => "Dimmy — Error",
        _ => "Dimmy",
    };

    // ── HICON generation ─────────────────────────────────────────────

    /// <summary>Generate (or load cached) state icons. Each is a 16×16
    /// ICO with a solid color circle — no anti-aliasing needed at this
    /// size, the Windows taskbar overlay slot is tiny and renders crisp
    /// pixel circles fine.</summary>
    private void EnsureStateIcons()
    {
        var dir = Path.Combine(Path.GetTempPath(), "dimmy_taskbar_icons");
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

        foreach (var (state, b, g, r) in palette)
        {
            var path = Path.Combine(dir, $"state_{state}.ico");
            try
            {
                if (!File.Exists(path))
                    File.WriteAllBytes(path, BuildCircleIco(16, b, g, r));
                var hIcon = LoadImage(IntPtr.Zero, path, IMAGE_ICON, 16, 16,
                    LR_LOADFROMFILE | LR_DEFAULTSIZE);
                if (hIcon != IntPtr.Zero) _hicons[state] = hIcon;
            }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"[TaskbarService] icon for {state}: {ex.Message}");
            }
        }
    }

    /// <summary>Build a tiny ICO file in memory: a colored anti-circle
    /// on transparent background. Returns the raw bytes ready to write.
    /// The ICO format is well-documented (header + 1 ICONDIRENTRY + 1
    /// BITMAPINFOHEADER + pixel data); no external image library needed.
    /// </summary>
    private static byte[] BuildCircleIco(int size, byte b, byte g, byte r)
    {
        var pixels = new byte[size * size * 4];
        double cx = (size - 1) / 2.0, cy = (size - 1) / 2.0;
        // Slightly under (size/2) so the circle has 1px breathing room
        // from the icon edge — important so the overlay doesn't get
        // clipped at the corners of the taskbar's overlay slot.
        double rad = size / 2.0 - 0.5;
        for (int y = 0; y < size; y++)
        {
            for (int x = 0; x < size; x++)
            {
                double dx = x - cx, dy = y - cy;
                if (dx * dx + dy * dy <= rad * rad)
                {
                    int i = (y * size + x) * 4;
                    pixels[i] = b;
                    pixels[i + 1] = g;
                    pixels[i + 2] = r;
                    pixels[i + 3] = 255;
                }
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
