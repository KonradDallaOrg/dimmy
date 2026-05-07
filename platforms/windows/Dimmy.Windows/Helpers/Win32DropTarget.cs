using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;

namespace Dimmy.Windows.Helpers;

/// File-drop on a WinUI 3 Window via the legacy WM_DROPFILES path.
///
/// We tried OLE RegisterDragDrop (with IDropTarget COM impl, on every
/// HWND in the child chain — verified via class-name logs that the
/// outer Window, InputNonClientPointerSource, DesktopChildSiteBridge
/// and InputSiteWindowClass were all registered). DragEnter never
/// fired. WinUI 3's input system (Microsoft.UI.Input + the
/// InputSiteWindowClass shim) intercepts OLE drops before they reach
/// any IDropTarget the app installs.
///
/// WM_DROPFILES is processed at the wndproc level — strictly before
/// the WinUI 3 input pump — so subclassing each HWND and reacting to
/// the message bypasses the entire WinUI input layer. Only file
/// drops work this way (not arbitrary OLE), which is exactly what we
/// need.
public sealed class Win32DropTarget : IDisposable
{
    private readonly IntPtr _rootHwnd;
    private readonly Action<string[]> _onDrop;
    private readonly List<SubclassEntry> _subclassed = new();
    // Pinned: SetWindowSubclass holds a function pointer to the
    // delegate. Without GCHandle the delegate may be collected and
    // we get a CallbackOnCollectedDelegate exception on first WM.
    private GCHandle _subclassProcHandle;
    private SUBCLASSPROC? _subclassProc;
    private static UIntPtr _subclassIdCounter = (UIntPtr)0xD17D17;

    public Win32DropTarget(IntPtr hwnd, Action<string[]> onDrop)
    {
        if (hwnd == IntPtr.Zero) throw new ArgumentException("hwnd is null");
        _rootHwnd = hwnd;
        _onDrop = onDrop ?? throw new ArgumentNullException(nameof(onDrop));
    }

    public bool Register()
    {
        if (_subclassed.Count > 0) return true;

        // Keep the delegate alive — SetWindowSubclass stores the raw
        // function pointer without holding a managed ref.
        _subclassProc = WndProcHook;
        _subclassProcHandle = GCHandle.Alloc(_subclassProc);

        var hwnds = new List<IntPtr> { _rootHwnd };
        EnumChildWindows(_rootHwnd, (h, _) => { hwnds.Add(h); return true; }, IntPtr.Zero);

        foreach (var h in hwnds)
        {
            var cls = GetClassName(h);
            // WinUI 3 uses InputSiteWindowClass + DesktopChildSiteBridge
            // for its internal OLE drag-drop (XAML drag events,
            // ListView reorder). RevokeDragDrop'ing those HWNDs kills
            // ListView CanReorderItems entirely — observed by the user
            // when rule rows refused to drag-reorder. Only revoke on
            // the outer Win32 window class which doesn't host XAML
            // content-level drag/drop.
            if (cls != "WinUIDesktopWin32WindowClass" && cls != "InputNonClientPointerSource")
            {
                // For content-host HWNDs we still install
                // DragAcceptFiles + WM_DROPFILES subclass below, but
                // we LEAVE WinUI 3's IDropTarget alone. The OS routes
                // a file drop to whichever responds first; ListView
                // reorder uses XAML drag types that aren't CF_HDROP
                // so they don't conflict.
            }
            else
            {
                RevokeDragDrop(h);
            }
            // Enable WM_DROPFILES delivery on this HWND. Without this
            // the OS doesn't even tell the wndproc about file drops.
            DragAcceptFiles(h, true);
            // UIPI bypass: when Dimmy runs at higher integrity level
            // than the drag source (Explorer = Medium IL, Dimmy =
            // High IL when launched from an elevated shell), Windows
            // silently filters drag/drop messages from the lower-IL
            // sender. ChangeWindowMessageFilterEx whitelists the
            // specific drop messages so they get through. Returns
            // false harmlessly on non-elevated processes.
            ChangeWindowMessageFilterEx(h, WM_DROPFILES, MSGFLT_ALLOW, IntPtr.Zero);
            ChangeWindowMessageFilterEx(h, WM_COPYDATA, MSGFLT_ALLOW, IntPtr.Zero);
            ChangeWindowMessageFilterEx(h, WM_COPYGLOBALDATA, MSGFLT_ALLOW, IntPtr.Zero);
            // Each subclass gets a unique idSubclass so multiple
            // installations on the same HWND don't collide.
            var id = NextSubclassId();
            // dwRefData carries a GCHandle into our owner so the
            // wndproc can invoke our managed callback without a
            // static lookup.
            var ownerHandle = GCHandle.Alloc(this, GCHandleType.Weak);
            bool ok = SetWindowSubclass(h, _subclassProc, id, GCHandle.ToIntPtr(ownerHandle));
            if (ok)
            {
                _subclassed.Add(new SubclassEntry(h, id, ownerHandle));
                App.Log($"  + WM_DROPFILES on hwnd=0x{h:X} class={cls}", "FileLoad");
            }
            else
            {
                ownerHandle.Free();
                App.Log($"  - WM_DROPFILES FAIL hwnd=0x{h:X} class={cls}", "FileLoad");
            }
        }
        App.Log($"WM_DROPFILES installed on {_subclassed.Count}/{hwnds.Count} HWNDs",
            "FileLoad");
        return _subclassed.Count > 0;
    }

    public void Unregister()
    {
        foreach (var e in _subclassed)
        {
            try
            {
                DragAcceptFiles(e.Hwnd, false);
                if (_subclassProc != null)
                    RemoveWindowSubclass(e.Hwnd, _subclassProc, e.Id);
            }
            catch { }
            try { e.OwnerHandle.Free(); } catch { }
        }
        _subclassed.Clear();
        if (_subclassProcHandle.IsAllocated) _subclassProcHandle.Free();
        _subclassProc = null;
    }

    public void Dispose() => Unregister();

    private static UIntPtr NextSubclassId()
    {
        unchecked { _subclassIdCounter = (UIntPtr)((ulong)_subclassIdCounter + 1); }
        return _subclassIdCounter;
    }

    /// Subclass procedure that runs FOR EVERY message on the
    /// subclassed HWND. We only intercept WM_DROPFILES — everything
    /// else is forwarded to the next handler in the subclass chain
    /// (DefSubclassProc).
    private static IntPtr WndProcHook(IntPtr hwnd, uint msg, IntPtr wParam,
        IntPtr lParam, UIntPtr idSubclass, IntPtr dwRefData)
    {
        // Diagnostic: log any message in the OLE drag-drop / file-drop
        // range (0x0230–0x023F) to see if WinUI 3's shim is sending
        // anything at all to our subclassed HWNDs during a drag.
        if (msg >= 0x0230 && msg < 0x0240)
        {
            App.Log($"msg=0x{msg:X4} hwnd=0x{hwnd:X} wParam=0x{wParam:X}", "FileLoad");
        }
        if (msg == WM_DROPFILES && wParam != IntPtr.Zero)
        {
            App.Log($"WM_DROPFILES on hwnd=0x{hwnd:X}", "FileLoad");
            try
            {
                var owner = GCHandle.FromIntPtr(dwRefData).Target as Win32DropTarget;
                if (owner != null)
                {
                    var paths = ExtractPaths(wParam);
                    if (paths.Length > 0) owner._onDrop(paths);
                }
            }
            catch (Exception ex)
            {
                App.Log($"WM_DROPFILES exc: {ex.Message}", "FileLoad");
            }
            finally
            {
                DragFinish(wParam);
            }
            return IntPtr.Zero;
        }
        return DefSubclassProc(hwnd, msg, wParam, lParam);
    }

    private static string[] ExtractPaths(IntPtr hDrop)
    {
        uint count = DragQueryFile(hDrop, 0xFFFFFFFF, null, 0);
        var paths = new string[count];
        for (uint i = 0; i < count; i++)
        {
            uint chars = DragQueryFile(hDrop, i, null, 0);
            var sb = new System.Text.StringBuilder((int)chars + 1);
            DragQueryFile(hDrop, i, sb, chars + 1);
            paths[i] = sb.ToString();
        }
        return paths;
    }

    private static string GetClassName(IntPtr hwnd)
    {
        var sb = new System.Text.StringBuilder(256);
        int n = GetClassNameW(hwnd, sb, sb.Capacity);
        return n > 0 ? sb.ToString(0, n) : "?";
    }

    private readonly record struct SubclassEntry(IntPtr Hwnd, UIntPtr Id, GCHandle OwnerHandle);

    // --- Win32 P/Invoke ---

    private const uint WM_DROPFILES = 0x0233;
    private const uint WM_COPYDATA = 0x004A;
    private const uint WM_COPYGLOBALDATA = 0x0049;
    private const uint MSGFLT_ALLOW = 1;

    [DllImport("user32.dll", SetLastError = true)]
    private static extern bool ChangeWindowMessageFilterEx(IntPtr hWnd, uint message,
        uint action, IntPtr pChangeFilterStruct);

    private delegate IntPtr SUBCLASSPROC(IntPtr hwnd, uint msg, IntPtr wParam,
        IntPtr lParam, UIntPtr idSubclass, IntPtr dwRefData);

    private delegate bool EnumChildProc(IntPtr hwnd, IntPtr lParam);

    [DllImport("user32.dll")]
    private static extern bool EnumChildWindows(IntPtr parent,
        EnumChildProc lpEnumFunc, IntPtr lParam);

    [DllImport("user32.dll", CharSet = CharSet.Unicode, EntryPoint = "GetClassNameW")]
    private static extern int GetClassNameW(IntPtr hwnd,
        System.Text.StringBuilder lpClassName, int nMaxCount);

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern void DragAcceptFiles(IntPtr hwnd, bool fAccept);

    [DllImport("shell32.dll", CharSet = CharSet.Unicode)]
    private static extern uint DragQueryFile(IntPtr hDrop, uint iFile,
        System.Text.StringBuilder? lpszFile, uint cch);

    [DllImport("shell32.dll")]
    private static extern void DragFinish(IntPtr hDrop);

    [DllImport("comctl32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool SetWindowSubclass(IntPtr hwnd, SUBCLASSPROC proc,
        UIntPtr idSubclass, IntPtr dwRefData);

    [DllImport("comctl32.dll", CharSet = CharSet.Unicode)]
    private static extern bool RemoveWindowSubclass(IntPtr hwnd, SUBCLASSPROC proc,
        UIntPtr idSubclass);

    [DllImport("comctl32.dll", CharSet = CharSet.Unicode)]
    private static extern IntPtr DefSubclassProc(IntPtr hwnd, uint msg, IntPtr wParam,
        IntPtr lParam);

    [DllImport("ole32.dll")]
    private static extern int RevokeDragDrop(IntPtr hwnd);
}
