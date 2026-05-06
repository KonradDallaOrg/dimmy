using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;

namespace Dimmy.Windows.Helpers;

/// Win32 OLE drag-drop bound directly to a Window's HWND. Bypasses
/// WinUI 3 desktop's flaky DragOver/Drop pump (DragEnter never fires
/// on the XAML side even with handledEventsToo=true on the
/// ScrollViewer chain). Calls back with the dropped file paths
/// once the user releases over the registered HWND.
///
/// Usage:
///   var target = new Win32DropTarget(hwnd, paths => OnFilesDropped(paths));
///   target.Register();
///   // ... when window closes:
///   target.Unregister();
///
/// One target per HWND. Re-registering on the same HWND is a no-op.
public sealed class Win32DropTarget : IDisposable
{
    private readonly IntPtr _hwnd;
    private readonly Action<string[]> _onDrop;
    private readonly DropTargetImpl _impl;
    private bool _registered;
    private bool _oleInited;

    public Win32DropTarget(IntPtr hwnd, Action<string[]> onDrop)
    {
        if (hwnd == IntPtr.Zero) throw new ArgumentException("hwnd is null");
        _hwnd = hwnd;
        _onDrop = onDrop ?? throw new ArgumentNullException(nameof(onDrop));
        _impl = new DropTargetImpl(this);
    }

    public bool Register()
    {
        if (_registered) return true;
        // Each thread that calls RegisterDragDrop must have OleInitialize'd
        // (not CoInitialize). Idempotent on the same thread.
        int hr = OleInitialize(IntPtr.Zero);
        _oleInited = (hr == 0 || hr == 1); // S_OK or S_FALSE (already inited)

        hr = RegisterDragDrop(_hwnd, _impl);
        _registered = (hr == 0);
        if (!_registered)
            App.Log($"RegisterDragDrop failed hr=0x{hr:X8}", "FileLoad");
        else
            App.Log($"RegisterDragDrop OK hwnd=0x{_hwnd:X}", "FileLoad");
        return _registered;
    }

    public void Unregister()
    {
        if (_registered)
        {
            RevokeDragDrop(_hwnd);
            _registered = false;
        }
        if (_oleInited)
        {
            OleUninitialize();
            _oleInited = false;
        }
    }

    public void Dispose() => Unregister();

    internal void NotifyDrop(string[] paths) => _onDrop(paths);

    // --- IDropTarget implementation ---

    [ComVisible(true)]
    [Guid("00000122-0000-0000-C000-000000000046")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IDropTarget
    {
        [PreserveSig] int DragEnter(IDataObject pDataObj, uint grfKeyState,
            POINTL pt, ref uint pdwEffect);
        [PreserveSig] int DragOver(uint grfKeyState, POINTL pt, ref uint pdwEffect);
        [PreserveSig] int DragLeave();
        [PreserveSig] int Drop(IDataObject pDataObj, uint grfKeyState,
            POINTL pt, ref uint pdwEffect);
    }

    [ComVisible(true)]
    private sealed class DropTargetImpl : IDropTarget
    {
        private readonly Win32DropTarget _owner;
        public DropTargetImpl(Win32DropTarget owner) => _owner = owner;

        public int DragEnter(IDataObject obj, uint keyState, POINTL pt, ref uint effect)
        {
            App.Log("Win32 DragEnter", "FileLoad");
            effect = HasFiles(obj) ? DROPEFFECT_COPY : DROPEFFECT_NONE;
            return 0;
        }

        public int DragOver(uint keyState, POINTL pt, ref uint effect)
        {
            // Keep the cursor showing "+ copy" while hovering over the
            // window. Effect is set per-frame; without this the OS
            // resets to DROPEFFECT_NONE between move events.
            effect = DROPEFFECT_COPY;
            return 0;
        }

        public int DragLeave() => 0;

        public int Drop(IDataObject obj, uint keyState, POINTL pt, ref uint effect)
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
            try { obj.QueryGetData(ref fmt); return true; }
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

    [StructLayout(LayoutKind.Sequential)]
    private struct POINTL { public int x; public int y; }

    private const uint CF_HDROP = 15;
    private const uint DROPEFFECT_NONE = 0;
    private const uint DROPEFFECT_COPY = 1;

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
