using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;

namespace Dimmy.Windows.Helpers;

/// Win32 IFileOpenDialog wrapper. The WinRT FileOpenPicker on
/// unpackaged WinUI 3 desktop apps is documented-flaky — on some
/// machines PickSingleFileAsync returns null immediately without
/// ever showing a dialog (no exception, no log). IFileOpenDialog is
/// the same COM API Explorer uses; it's bulletproof, requires no
/// manifest capabilities and no async plumbing.
///
/// Usage:
///   var path = Win32FileDialog.PickFile(hwnd, "Pick a WAV", ".wav");
///   if (path != null) await TranscribeFileAsync(path);
public static class Win32FileDialog
{
    /// Shows a modal "open file" dialog parented to ownerHwnd. Returns
    /// the picked path, or null if the user cancelled or the call
    /// failed. Filter is a list of (name, spec) pairs:
    ///   ("WAV audio", "*.wav"), ("All files", "*.*")
    public static string? PickFile(IntPtr ownerHwnd, string title,
        params (string name, string spec)[] filters)
    {
        IFileOpenDialog? dialog = null;
        try
        {
            dialog = (IFileOpenDialog)Activator.CreateInstance(
                Type.GetTypeFromCLSID(CLSID_FileOpenDialog)!)!;

            dialog.GetOptions(out uint opts);
            dialog.SetOptions(opts | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | FOS_FILEMUSTEXIST);

            if (filters is { Length: > 0 })
            {
                var spec = new COMDLG_FILTERSPEC[filters.Length];
                for (int i = 0; i < filters.Length; i++)
                {
                    spec[i].pszName = filters[i].name;
                    spec[i].pszSpec = filters[i].spec;
                }
                dialog.SetFileTypes((uint)spec.Length, spec);
                dialog.SetFileTypeIndex(1);
            }

            if (!string.IsNullOrEmpty(title)) dialog.SetTitle(title);

            int hr = dialog.Show(ownerHwnd);
            if (hr != 0) return null; // ERROR_CANCELLED == 0x800704C7 too

            dialog.GetResult(out IShellItem item);
            try
            {
                item.GetDisplayName(SIGDN.FILESYSPATH, out IntPtr pszPath);
                try
                {
                    return Marshal.PtrToStringUni(pszPath);
                }
                finally
                {
                    Marshal.FreeCoTaskMem(pszPath);
                }
            }
            finally
            {
                Marshal.ReleaseComObject(item);
            }
        }
        catch (Exception ex)
        {
            App.Log($"Win32FileDialog.PickFile: {ex.Message}", "FileLoad");
            return null;
        }
        finally
        {
            if (dialog != null) Marshal.ReleaseComObject(dialog);
        }
    }

    /// Shows a modal folder-picker dialog parented to ownerHwnd. Returns
    /// the picked directory path, or null on cancel / failure. Same COM
    /// API as <see cref="PickFile"/> with FOS_PICKFOLDERS set. Optionally
    /// seeds the initial folder when <paramref name="initialDir"/> exists.
    public static string? PickFolder(IntPtr ownerHwnd, string title, string? initialDir = null)
    {
        IFileOpenDialog? dialog = null;
        try
        {
            dialog = (IFileOpenDialog)Activator.CreateInstance(
                Type.GetTypeFromCLSID(CLSID_FileOpenDialog)!)!;

            dialog.GetOptions(out uint opts);
            dialog.SetOptions(opts | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST | FOS_PICKFOLDERS);

            if (!string.IsNullOrEmpty(title)) dialog.SetTitle(title);

            if (!string.IsNullOrEmpty(initialDir) && System.IO.Directory.Exists(initialDir))
            {
                int sh = SHCreateItemFromParsingName(initialDir!, IntPtr.Zero,
                    typeof(IShellItem).GUID, out var startItem);
                if (sh == 0 && startItem != null)
                {
                    try { dialog.SetFolder(startItem); }
                    finally { Marshal.ReleaseComObject(startItem); }
                }
            }

            int hr = dialog.Show(ownerHwnd);
            if (hr != 0) return null;

            dialog.GetResult(out IShellItem item);
            try
            {
                item.GetDisplayName(SIGDN.FILESYSPATH, out IntPtr pszPath);
                try { return Marshal.PtrToStringUni(pszPath); }
                finally { Marshal.FreeCoTaskMem(pszPath); }
            }
            finally
            {
                Marshal.ReleaseComObject(item);
            }
        }
        catch (Exception ex)
        {
            App.Log($"Win32FileDialog.PickFolder: {ex.Message}", "Settings");
            return null;
        }
        finally
        {
            if (dialog != null) Marshal.ReleaseComObject(dialog);
        }
    }

    // --- COM definitions ---

    private static readonly Guid CLSID_FileOpenDialog
        = new("DC1C5A9C-E88A-4DDE-A5A1-60F82A20AEF7");

    [DllImport("shell32.dll", CharSet = CharSet.Unicode, ExactSpelling = true, PreserveSig = true)]
    private static extern int SHCreateItemFromParsingName(
        string pszPath, IntPtr pbc, [In] Guid riid, out IShellItem ppv);

    private const uint FOS_PICKFOLDERS     = 0x00000020;
    private const uint FOS_FORCEFILESYSTEM = 0x00000040;
    private const uint FOS_PATHMUSTEXIST   = 0x00000800;
    private const uint FOS_FILEMUSTEXIST   = 0x00001000;

    private enum SIGDN : uint
    {
        FILESYSPATH = 0x80058000,
    }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct COMDLG_FILTERSPEC
    {
        [MarshalAs(UnmanagedType.LPWStr)] public string pszName;
        [MarshalAs(UnmanagedType.LPWStr)] public string pszSpec;
    }

    [ComImport, Guid("d57c7288-d4ad-4768-be02-9d969532d960"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IFileOpenDialog
    {
        // IModalWindow
        [PreserveSig] int Show([In] IntPtr parent);
        // IFileDialog
        void SetFileTypes(uint cFileTypes,
            [MarshalAs(UnmanagedType.LPArray)] COMDLG_FILTERSPEC[] rgFilterSpec);
        void SetFileTypeIndex(uint iFileType);
        void GetFileTypeIndex(out uint piFileType);
        void Advise(IntPtr pfde, out uint pdwCookie);
        void Unadvise(uint dwCookie);
        void SetOptions(uint fos);
        void GetOptions(out uint pfos);
        void SetDefaultFolder(IShellItem psi);
        void SetFolder(IShellItem psi);
        void GetFolder(out IShellItem ppsi);
        void GetCurrentSelection(out IShellItem ppsi);
        void SetFileName([MarshalAs(UnmanagedType.LPWStr)] string pszName);
        void GetFileName([MarshalAs(UnmanagedType.LPWStr)] out string pszName);
        void SetTitle([MarshalAs(UnmanagedType.LPWStr)] string pszTitle);
        void SetOkButtonLabel([MarshalAs(UnmanagedType.LPWStr)] string pszText);
        void SetFileNameLabel([MarshalAs(UnmanagedType.LPWStr)] string pszLabel);
        void GetResult(out IShellItem ppsi);
        void AddPlace(IShellItem psi, uint fdap);
        void SetDefaultExtension([MarshalAs(UnmanagedType.LPWStr)] string pszDefaultExtension);
        void Close([MarshalAs(UnmanagedType.Error)] int hr);
        void SetClientGuid(ref Guid guid);
        void ClearClientData();
        void SetFilter(IntPtr pFilter);
        // IFileOpenDialog
        void GetResults(out IntPtr ppenum);
        void GetSelectedItems(out IntPtr ppsai);
    }

    [ComImport, Guid("43826D1E-E718-42EE-BC55-A1E261C37BFE"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellItem
    {
        void BindToHandler(IntPtr pbc, ref Guid bhid, ref Guid riid, out IntPtr ppv);
        void GetParent(out IShellItem ppsi);
        void GetDisplayName(SIGDN sigdnName, out IntPtr ppszName);
        void GetAttributes(uint sfgaoMask, out uint psfgaoAttribs);
        void Compare(IShellItem psi, uint hint, out int piOrder);
    }
}
