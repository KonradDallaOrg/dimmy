using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;

namespace Dimmy.Windows.Helpers;

/// Win32 OLE drag-drop bound directly to a WinUI 3 Window's HWND
/// chain. Bypasses WinUI 3 desktop's flaky DragOver/Drop pump
/// (DragEnter never fires on the XAML side even with
/// handledEventsToo=true on the ScrollViewer chain).
///
/// Critical for WinUI 3: registering on the OUTER Window HWND
/// is not enough — the visible content lives inside a child
/// HWND ("Microsoft.UI.Content.DesktopChildSiteBridge"), and
/// drops over that area go to the child first. We walk the
/// entire HWND tree and register the same IDropTarget on every
/// node. Each child gets RevokeDragDrop'd first to clear any
/// internal registration WinUI 3 may have set up.
public sealed class Win32DropTarget : IDisposable
{
    private readonly IntPtr _rootHwnd;
    private readonly Action<string[]> _onDrop;
    private readonly DropTargetImpl _impl;
    private readonly System.Collections.Generic.List<IntPtr> _registeredHwnds = new();
    private bool _oleInited;

    public Win32DropTarget(IntPtr hwnd, Action<string[]> onDrop)
    {
        if (hwnd == IntPtr.Zero) throw new ArgumentException("hwnd is null");
        _rootHwnd = hwnd;
        _onDrop = onDrop ?? throw new ArgumentNullException(nameof(onDrop));
        _impl = new DropTargetImpl(this);
    }

    public bool Register()
    {
        if (_registeredHwnds.Count > 0) return true;
        // Each thread that calls RegisterDragDrop must have
        // OleInitialize'd (not CoInitialize). Idempotent.
        int hr = OleInitialize(IntPtr.Zero);
        _oleInited = (hr == 0 || hr == 1); // S_OK or S_FALSE (already inited)

        // Collect the root + every descendant HWND, then register
        // on each. Drops can target any of them depending on which
        // window is under the cursor at release time.
        var hwnds = new System.Collections.Generic.List<IntPtr> { _rootHwnd };
        EnumChildWindows(_rootHwnd, (h, _) => { hwnds.Add(h); return true; }, IntPtr.Zero);

        foreach (var h in hwnds)
        {
            var cls = GetClassName(h);
            // Clear any existing target (XAML installs its own on
            // the content-bridge HWND). RevokeDragDrop returns
            // DRAGDROP_E_NOTREGISTERED (0x80040100) when there's
            // nothing to revoke — ignore.
            RevokeDragDrop(h);
            int rh = RegisterDragDrop(h, _impl);
            if (rh == 0)
            {
                _registeredHwnds.Add(h);
                App.Log($"  + drop on hwnd=0x{h:X} class={cls}", "FileLoad");
            }
            else
            {
                // Some service-only HWNDs (tooltip thunks, etc.)
                // refuse the registration. Don't bail on the first
                // failure — keep walking the tree.
                App.Log($"  - drop FAIL hwnd=0x{h:X} class={cls} hr=0x{rh:X8}",
                    "FileLoad");
            }
        }
        App.Log($"Win32 drop registered on {_registeredHwnds.Count}/{hwnds.Count} HWNDs",
            "FileLoad");
        return _registeredHwnds.Count > 0;
    }

    public void Unregister()
    {
        foreach (var h in _registeredHwnds) RevokeDragDrop(h);
        _registeredHwnds.Clear();
        if (_oleInited)
        {
            OleUninitialize();
            _oleInited = false;
        }
    }

    private static string GetClassName(IntPtr hwnd)
    {
        var sb = new System.Text.StringBuilder(256);
        int n = GetClassNameW(hwnd, sb, sb.Capacity);
        return n > 0 ? sb.ToString(0, n) : "?";
    }

    public void Dispose() => Unregister();

    internal void NotifyDrop(string[] paths) => _onDrop(paths);

    // --- IDropTarget implementation ---

    // POINTL is passed BY VALUE in COM and the .NET CCW marshaller
    // can corrupt its layout on x64 — declare as `long` (8 bytes
    // packed: low 32 = x, high 32 = y) to bypass struct marshalling
    // entirely. We never read the coords anyway.
    [ComVisible(true)]
    [Guid("00000122-0000-0000-C000-000000000046")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IDropTarget
    {
        [PreserveSig] int DragEnter(IDataObject pDataObj, uint grfKeyState,
            long pt, ref uint pdwEffect);
        [PreserveSig] int DragOver(uint grfKeyState, long pt, ref uint pdwEffect);
        [PreserveSig] int DragLeave();
        [PreserveSig] int Drop(IDataObject pDataObj, uint grfKeyState,
            long pt, ref uint pdwEffect);
    }

    [ComVisible(true)]
    private sealed class DropTargetImpl : IDropTarget
    {
        private readonly Win32DropTarget _owner;
        public DropTargetImpl(Win32DropTarget owner) => _owner = owner;

        public int DragEnter(IDataObject obj, uint keyState, long pt, ref uint effect)
        {
            App.Log("Win32 DragEnter", "FileLoad");
            effect = HasFiles(obj) ? DROPEFFECT_COPY : DROPEFFECT_NONE;
            return 0;
        }

        public int DragOver(uint keyState, long pt, ref uint effect)
        {
            // Keep the cursor showing "+ copy" while hovering over the
            // window. Effect is set per-frame; without this the OS
            // resets to DROPEFFECT_NONE between move events.
            effect = DROPEFFECT_COPY;
            return 0;
        }

        public int DragLeave() => 0;

        public int Drop(IDataObject obj, uint keyState, long pt, ref uint effect)
        {
            App.Log("Win32 Drop fired", "FileLoad");
            try
            {
                var paths = ExtractFilePaths(obj);
                effect = paths.Length > 0 ? DROPEFFECT_COPY : DROPEFFECT_NONE;
                if (paths.Length > 0) _owner.NotifyDrop(paths);
            }
            catch (Exception ex)
            {
                App.Log($"Win32 Drop exc: {ex.Message}", "FileLoad");
                effect = DROPEFFECT_NONE;
            }
            return 0;
        }

        private static bool HasFiles(IDataObject obj)
        {
            var fmt = new FORMATETC
            {
                cfFormat = (short)CF_HDROP,
                dwAspect = DVASPECT.DVASPECT_CONTENT,
                lindex = -1,
                tymed = TYMED.TYMED_HGLOBAL,
            };
            // QueryGetData on System.Runtime.InteropServices.ComTypes
            // is [PreserveSig] — returns HRESULT, never throws. S_OK=0
            // means the format is available.
            try { return obj.QueryGetData(ref fmt) == 0; }
            catch { return false; }
        }

        private static string[] ExtractFilePaths(IDataObject obj)
        {
            var fmt = new FORMATETC
            {
                cfFormat = (short)CF_HDROP,
                dwAspect = DVASPECT.DVASPECT_CONTENT,
                lindex = -1,
                tymed = TYMED.TYMED_HGLOBAL,
            };
            STGMEDIUM medium = default;
            obj.GetData(ref fmt, out medium);
            try
            {
                if (medium.unionmember == IntPtr.Zero) return Array.Empty<string>();
                IntPtr hDrop = medium.unionmember;
                uint count = DragQueryFile(hDrop, 0xFFFFFFFF, null, 0);
                var paths = new List<string>((int)count);
                for (uint i = 0; i < count; i++)
                {
                    uint chars = DragQueryFile(hDrop, i, null, 0);
                    var sb = new System.Text.StringBuilder((int)chars + 1);
                    DragQueryFile(hDrop, i, sb, chars + 1);
                    paths.Add(sb.ToString());
                }
                return paths.ToArray();
            }
            finally
            {
                ReleaseStgMedium(ref medium);
            }
        }
    }

    private const uint CF_HDROP = 15;
    private const uint DROPEFFECT_NONE = 0;
    private const uint DROPEFFECT_COPY = 1;

    private delegate bool EnumChildProc(IntPtr hwnd, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern bool EnumChildWindows(IntPtr parent,
        EnumChildProc lpEnumFunc, IntPtr lParam);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "GetClassNameW")]
    private static extern int GetClassNameW(IntPtr hwnd,
        System.Text.StringBuilder lpClassName, int nMaxCount);

    [DllImport("ole32.dll")]
    private static extern int OleInitialize(IntPtr pvReserved);

    [DllImport("ole32.dll")]
    private static extern void OleUninitialize();

    [DllImport("ole32.dll")]
    private static extern int RegisterDragDrop(IntPtr hwnd,
        [MarshalAs(UnmanagedType.Interface)] IDropTarget pDropTarget);

    [DllImport("ole32.dll")]
    private static extern int RevokeDragDrop(IntPtr hwnd);

    [DllImport("ole32.dll")]
    private static extern void ReleaseStgMedium(ref STGMEDIUM pmedium);

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern uint DragQueryFile(IntPtr hDrop, uint iFile,
        System.Text.StringBuilder? lpszFile, uint cch);
}
