using System;
using System.IO;
using System.Runtime.InteropServices;

namespace Dimmy.Windows.Helpers;

/// Extracts the real Windows icon from an .exe (the same one Explorer
/// shows in the taskbar / Start menu) and caches it as PNG under
/// %LOCALAPPDATA%\Dimmy\app-icons\<stem>.png.
///
/// Uses IShellItemImageFactory.GetImage — the modern Win7+ API
/// Explorer itself uses to render icons. Returns a 32-bit ARGB
/// HBITMAP with proper alpha (no opaque background), unlike the
/// older SHGetFileInfo + GdipCreateBitmapFromHICON path which
/// flattens transparency on many icons.
///
/// Two entry points:
///   • EnsureCachedFromExePath(path) — call when you have a live
///     exe path. Idempotent: returns immediately if PNG already
///     exists.
///   • TryGetCachedUri(processName) — returns "file:///..." URI
///     for XAML binding, or "" if PNG isn't there yet.
public static class IconExtractor
{
    private static readonly string CacheDir = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "Dimmy", "app-icons");

    private static readonly object GdiplusLock = new();
    private static IntPtr _gdiplusToken = IntPtr.Zero;
    private static bool _gdiplusReady;

    /// Bumped whenever the extraction algorithm changes — old PNGs
    /// from a previous algorithm would otherwise be served forever.
    /// v2 = IShellItemImageFactory (transparent, 64×64).
    private const int CACHE_VERSION = 2;

    static IconExtractor()
    {
        try
        {
            Directory.CreateDirectory(CacheDir);
            EvictStaleCacheVersion();
        }
        catch { /* best-effort */ }
    }

    public static string TryGetCachedUri(string processName)
    {
        if (string.IsNullOrWhiteSpace(processName)) return "";
        var stem = StripExe(processName);
        if (string.IsNullOrEmpty(stem)) return "";
        var path = Path.Combine(CacheDir, stem + ".png");
        if (!File.Exists(path)) return "";
        return new Uri(path).AbsoluteUri;
    }

    public static void EnsureCachedFromExePath(string exePath)
    {
        if (string.IsNullOrWhiteSpace(exePath)) return;
        try
        {
            if (!File.Exists(exePath)) return;
            var stem = StripExe(Path.GetFileName(exePath));
            if (string.IsNullOrEmpty(stem)) return;
            var pngPath = Path.Combine(CacheDir, stem + ".png");
            if (File.Exists(pngPath)) return;
            ExtractIconToPng(exePath, pngPath);
        }
        catch
        {
            // Best-effort: missing icon falls back to FontIcon glyph.
        }
    }

    private static string StripExe(string name)
    {
        var n = (name ?? "").Trim().ToLowerInvariant();
        if (n.EndsWith(".exe")) n = n.Substring(0, n.Length - 4);
        return n;
    }

    private static void EvictStaleCacheVersion()
    {
        // Sentinel file containing the current cache version. When the
        // version on disk doesn't match what we ship, wipe the dir so
        // the next extraction call repopulates with the new algorithm.
        var sentinel = Path.Combine(CacheDir, ".cache-version");
        try
        {
            var current = File.Exists(sentinel) ? File.ReadAllText(sentinel).Trim() : "0";
            if (current != CACHE_VERSION.ToString())
            {
                foreach (var f in Directory.GetFiles(CacheDir, "*.png"))
                {
                    try { File.Delete(f); } catch { }
                }
                File.WriteAllText(sentinel, CACHE_VERSION.ToString());
            }
        }
        catch { }
    }

    private static void ExtractIconToPng(string exePath, string pngPath)
    {
        EnsureGdiplus();

        // ── Modern path: IShellItemImageFactory (Vista+, alpha kept) ──
        IntPtr hbmp = IntPtr.Zero;
        try
        {
            var shellItemGuid = typeof(IShellItem).GUID;
            int hr = SHCreateItemFromParsingName(exePath, IntPtr.Zero,
                ref shellItemGuid, out var item);
            if (hr != 0 || item == null) return;
            try
            {
                var size = new SIZE { cx = 64, cy = 64 };
                // SIIGBF_BIGGERSIZEOK lets the shell return a larger icon
                // than requested if a smaller one would lose detail —
                // matches Explorer's "Large icons" behaviour. Combine
                // with SIIGBF_RESIZETOFIT to scale down if needed.
                hr = ((IShellItemImageFactory)item).GetImage(size,
                    SIIGBF.SIIGBF_BIGGERSIZEOK | SIIGBF.SIIGBF_RESIZETOFIT,
                    out hbmp);
                if (hr != 0 || hbmp == IntPtr.Zero) return;

                if (GdipCreateBitmapFromHBITMAP(hbmp, IntPtr.Zero, out var bitmap) != 0
                    || bitmap == IntPtr.Zero)
                    return;
                try
                {
                    var pngEncoder = new Guid("557CF406-1A04-11D3-9A73-0000F81EF32E");
                    Directory.CreateDirectory(Path.GetDirectoryName(pngPath)!);
                    GdipSaveImageToFile(bitmap, pngPath, ref pngEncoder, IntPtr.Zero);
                }
                finally
                {
                    GdipDisposeImage(bitmap);
                }
            }
            finally
            {
                Marshal.ReleaseComObject(item);
            }
        }
        finally
        {
            if (hbmp != IntPtr.Zero) DeleteObject(hbmp);
        }
    }

    private static void EnsureGdiplus()
    {
        lock (GdiplusLock)
        {
            if (_gdiplusReady) return;
            var input = new GdiplusStartupInput { GdiplusVersion = 1 };
            if (GdiplusStartup(out _gdiplusToken, ref input, IntPtr.Zero) == 0)
                _gdiplusReady = true;
        }
    }

    // --- COM / Win32 P/Invoke ---

    [StructLayout(LayoutKind.Sequential)]
    private struct SIZE { public int cx; public int cy; }

    [Flags]
    private enum SIIGBF : uint
    {
        SIIGBF_RESIZETOFIT = 0x0,
        SIIGBF_BIGGERSIZEOK = 0x1,
        SIIGBF_MEMORYONLY = 0x2,
        SIIGBF_ICONONLY = 0x4,
        SIIGBF_THUMBNAILONLY = 0x8,
        SIIGBF_INCACHEONLY = 0x10,
    }

    [ComImport, Guid("43826D1E-E718-42EE-BC55-A1E261C37BFE"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellItem
    {
        void BindToHandler(IntPtr pbc, ref Guid bhid, ref Guid riid, out IntPtr ppv);
        void GetParent(out IShellItem ppsi);
        void GetDisplayName(uint sigdnName, out IntPtr ppszName);
        void GetAttributes(uint sfgaoMask, out uint psfgaoAttribs);
        void Compare(IShellItem psi, uint hint, out int piOrder);
    }

    [ComImport, Guid("BCC18B79-BA16-442F-80C4-8A59C30C463B"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellItemImageFactory
    {
        [PreserveSig]
        int GetImage(SIZE size, SIIGBF flags, out IntPtr phbm);
    }

    [DllImport("shell32.dll", CharSet = CharSet.Unicode, PreserveSig = false)]
    private static extern int SHCreateItemFromParsingName(
        [MarshalAs(UnmanagedType.LPWStr)] string pszPath,
        IntPtr pbc, ref Guid riid,
        [MarshalAs(UnmanagedType.Interface)] out IShellItem ppv);

    [DllImport("gdi32.dll")]
    private static extern bool DeleteObject(IntPtr hObject);

    [StructLayout(LayoutKind.Sequential)]
    private struct GdiplusStartupInput
    {
        public uint GdiplusVersion;
        public IntPtr DebugEventCallback;
        public bool SuppressBackgroundThread;
        public bool SuppressExternalCodecs;
    }

    [DllImport("gdiplus.dll")]
    private static extern int GdiplusStartup(out IntPtr token,
        ref GdiplusStartupInput input, IntPtr output);

    [DllImport("gdiplus.dll")]
    private static extern int GdipCreateBitmapFromHBITMAP(IntPtr hbm, IntPtr hpal,
        out IntPtr bitmap);

    [DllImport("gdiplus.dll", CharSet = CharSet.Unicode)]
    private static extern int GdipSaveImageToFile(IntPtr image, string filename,
        ref Guid clsidEncoder, IntPtr encoderParams);

    [DllImport("gdiplus.dll")]
    private static extern int GdipDisposeImage(IntPtr image);
}
