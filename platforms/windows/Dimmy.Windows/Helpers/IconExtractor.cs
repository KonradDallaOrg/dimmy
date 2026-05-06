using System;
using System.IO;
using System.Runtime.InteropServices;

namespace Dimmy.Windows.Helpers;

/// Extracts the real Windows icon from an .exe (the same one Explorer
/// shows in the taskbar / Start menu) and caches it as PNG under
/// %LOCALAPPDATA%\Dimmy\app-icons\<stem>.png. The cache is keyed by
/// process basename (e.g. "slack" → "slack.png") so the AppRule list
/// can resolve a rule's icon without keeping the originating exe path.
///
/// Two entry points:
///   • EnsureCachedFromExePath(path) — call when you have a live exe
///     path (foreground capture). Idempotent: returns immediately if
///     the PNG already exists.
///   • TryGetCachedUri(processName) — returns "file:///..." URI for
///     XAML binding, or "" if the PNG isn't there yet.
///
/// We use SHGetFileInfo with SHGFI_ICON|SHGFI_LARGEICON to grab a
/// 32×32 HICON and convert it via GdipCreateBitmapFromHICON +
/// GdipSaveImageToFile (PNG encoder GUID). System.Drawing.Common is
/// avoided to keep the publish footprint small.
public static class IconExtractor
{
    private static readonly string CacheDir = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "Dimmy", "app-icons");

    private static readonly object GdiplusLock = new();
    private static IntPtr _gdiplusToken = IntPtr.Zero;
    private static bool _gdiplusReady;

    static IconExtractor()
    {
        try { Directory.CreateDirectory(CacheDir); } catch { /* best-effort */ }
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

    public static string CachePathFor(string processName)
    {
        var stem = StripExe(processName);
        return Path.Combine(CacheDir, stem + ".png");
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

    private static void ExtractIconToPng(string exePath, string pngPath)
    {
        EnsureGdiplus();
        var sfi = default(SHFILEINFO);
        var ret = SHGetFileInfo(exePath, 0, ref sfi, (uint)Marshal.SizeOf<SHFILEINFO>(),
            SHGFI_ICON | SHGFI_LARGEICON);
        if (ret == IntPtr.Zero || sfi.hIcon == IntPtr.Zero) return;
        try
        {
            if (GdipCreateBitmapFromHICON(sfi.hIcon, out var bitmap) != 0 || bitmap == IntPtr.Zero)
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
            DestroyIcon(sfi.hIcon);
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

    // --- Win32 / GDI+ P/Invoke ---

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct SHFILEINFO
    {
        public IntPtr hIcon;
        public int iIcon;
        public uint dwAttributes;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 260)]
        public string szDisplayName;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 80)]
        public string szTypeName;
    }

    private const uint SHGFI_ICON = 0x000000100;
    private const uint SHGFI_LARGEICON = 0x000000000;

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr SHGetFileInfo(string pszPath, uint dwFileAttributes,
        ref SHFILEINFO psfi, uint cbFileInfo, uint uFlags);

    [DllImport("user32.dll")]
    private static extern bool DestroyIcon(IntPtr hIcon);

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
    private static extern int GdipCreateBitmapFromHICON(IntPtr hicon, out IntPtr bitmap);

    [DllImport("gdiplus.dll", CharSet = CharSet.Unicode)]
    private static extern int GdipSaveImageToFile(IntPtr image, string filename,
        ref Guid clsidEncoder, IntPtr encoderParams);

    [DllImport("gdiplus.dll")]
    private static extern int GdipDisposeImage(IntPtr image);
}
