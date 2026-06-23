using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using Dimmy.Windows.ViewModels;

namespace Dimmy.Windows.Services;

/// <summary>
/// Populates the right-click jump list on Dimmy's taskbar button so
/// the user gets quick access to common actions without opening the
/// pill or Settings — the Windows analogue of the macOS Dock context
/// menu (Teams, Slack, Spotify use the same affordance).
///
/// Each entry is an `IShellLinkW` that re-launches `Dimmy.Windows.exe`
/// with `--command <name>`; the transient instance forwards the
/// command to the running one via `CommandPipeServer` and exits. So
/// the user effectively talks to the live process even though Windows
/// dispatches the click as a fresh launch.
///
/// Categories surfaced:
///   • Tasks       (Toggle pill, Settings, Quit)
///   • Style       (top-N most useful LLM styles)
///   • Translate   (top-N target languages, mirrors pill/tray submenu)
///
/// Re-registered on every Dimmy launch so the entries always reflect
/// the current EXE path (Velopack updates the binary in-place).
/// </summary>
public static class JumpListService
{
    /// <summary>AppUserModelID — must match the value set via
    /// `SetCurrentProcessExplicitAppUserModelID` in App.OnLaunched
    /// AND the AUMI of the Start-menu shortcut Velopack creates at
    /// install time (which Velopack derives from `--packId`, so we
    /// use the same string Velopack uses: "Dimmy").
    ///
    /// Without a registered AUMI matching a real Start-menu shortcut,
    /// Windows 11 silently drops custom jump-list entries — the
    /// right-click only shows the system defaults (Pin, Close).</summary>
    public const string AumiId = "Dimmy";

    /// <summary>Suffix used for the dev-only Start menu shortcut so
    /// it's distinguishable from a real Velopack-installed entry.
    /// In production builds (CARGO_PROFILE=release / Velopack pack),
    /// the install-time shortcut already exists; we skip creating
    /// the dev one if a Velopack shortcut is detected.</summary>
    private const string DevShortcutFileName = "Dimmy (Dev).lnk";

    [DllImport("shell32.dll")]
    private static extern int SetCurrentProcessExplicitAppUserModelID(
        [MarshalAs(UnmanagedType.LPWStr)] string AppID);

    /// <summary>Tag the current process with our AUMI. Must be called
    /// before any taskbar window is created — Windows derives the
    /// taskbar grouping + jump-list ownership from the AUMI of the
    /// process at first window registration.</summary>
    public static void SetProcessAumi()
    {
        try { SetCurrentProcessExplicitAppUserModelID(AumiId); }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[JumpList] SetAumi failed: {ex.Message}");
        }
    }

    /// <summary>
    /// Ensure a Start-menu shortcut exists with our AUMI so Windows 11
    /// is willing to display custom jump-list entries on the taskbar
    /// button. Velopack creates one at install time in production
    /// builds; in dev (running directly from `bin\Debug\…`) no
    /// shortcut exists, and Win11 silently swallows our jump-list
    /// registration. This helper creates a "Dimmy (Dev).lnk" pointing
    /// at the current EXE if neither it nor a real Velopack shortcut
    /// is present.
    ///
    /// Idempotent: safe to call on every launch. The dev shortcut is
    /// kept around once created — it's harmless and a no-op next time.
    /// </summary>
    public static void EnsureStartMenuShortcut()
    {
        try
        {
            var startMenu = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
                "Microsoft", "Windows", "Start Menu", "Programs");

            var exe = Process.GetCurrentProcess().MainModule?.FileName
                ?? Environment.ProcessPath;
            if (string.IsNullOrEmpty(exe))
            {
                Log("EnsureStartMenuShortcut: cannot resolve current EXE path");
                return;
            }

            var devShortcut = Path.Combine(startMenu, DevShortcutFileName);
            // Always (re)create the dev shortcut and point it at the
            // currently-running EXE — Windows uses path-matching to
            // associate the running process with a registered
            // shortcut for jump-list display. If a Velopack prod
            // shortcut also exists in the same folder, that's fine:
            // both will carry the same AUMI; Windows picks the one
            // whose target path matches the running process.
            if (File.Exists(devShortcut))
            {
                try
                {
                    // Read the existing shortcut's target — if it
                    // already matches our exe, we can skip the
                    // expensive recreation.
                    var existing = (IShellLinkW)new CShellLink();
                    var pf = (IPersistFile)existing;
                    pf.Load(devShortcut, 0);
                    var sb = new System.Text.StringBuilder(260);
                    existing.GetPath(sb, 260, IntPtr.Zero, 0);
                    Marshal.ReleaseComObject(existing);
                    if (string.Equals(sb.ToString(), exe, StringComparison.OrdinalIgnoreCase))
                    {
                        Log("EnsureStartMenuShortcut: dev shortcut already points to current EXE");
                        return;
                    }
                }
                catch (Exception ex)
                {
                    Log($"EnsureStartMenuShortcut: could not inspect existing dev shortcut: {ex.Message}");
                }
            }

            var link = (IShellLinkW)new CShellLink();
            try
            {
                link.SetPath(exe);
                link.SetWorkingDirectory(Path.GetDirectoryName(exe) ?? "");
                link.SetIconLocation(exe, 0);
                link.SetDescription("Dimmy (development build)");

                // Stamp AUMI on the shortcut's property store — this
                // is the bit that lets the shortcut "claim" the running
                // process for jump-list purposes.
                var ps = (IPropertyStore)link;
                var keyAumi = PKEY_AppUserModel_ID;
                var pvAumi = new PROPVARIANT
                {
                    vt = VT_LPWSTR,
                    pwszVal = Marshal.StringToCoTaskMemUni(AumiId),
                };
                try
                {
                    ps.SetValue(ref keyAumi, ref pvAumi);
                    ps.Commit();
                }
                finally
                {
                    if (pvAumi.pwszVal != IntPtr.Zero) Marshal.FreeCoTaskMem(pvAumi.pwszVal);
                }

                // Persist to disk via IPersistFile (same RCW; do NOT release).
                var pf = (IPersistFile)link;
                pf.Save(devShortcut, true);
                Log($"EnsureStartMenuShortcut: created {devShortcut} → {exe} (AUMI={AumiId})");
            }
            finally
            {
                Marshal.ReleaseComObject(link);
            }
        }
        catch (Exception ex)
        {
            Log($"EnsureStartMenuShortcut FAILED: {ex.GetType().Name}: {ex.Message}");
        }
    }

    [ComImport, Guid("0000010B-0000-0000-C000-000000000046"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IPersistFile
    {
        // IPersist
        void GetClassID(out Guid pClassID);
        // IPersistFile
        [PreserveSig] int IsDirty();
        void Load([MarshalAs(UnmanagedType.LPWStr)] string pszFileName, uint dwMode);
        void Save([MarshalAs(UnmanagedType.LPWStr)] string pszFileName,
            [MarshalAs(UnmanagedType.Bool)] bool fRemember);
        void SaveCompleted([MarshalAs(UnmanagedType.LPWStr)] string pszFileName);
        void GetCurFile([Out, MarshalAs(UnmanagedType.LPWStr)] out string ppszFileName);
    }

    private static readonly string DiagLogPath = Path.Combine(
        Path.GetTempPath(), "dimmy_jumplist.log");

    private static void Log(string msg)
    {
        try
        {
            File.AppendAllText(DiagLogPath,
                $"[{DateTime.Now:yyyy-MM-dd HH:mm:ss.fff}] {msg}{Environment.NewLine}");
        }
        catch { }
        System.Diagnostics.Debug.WriteLine($"[JumpList] {msg}");
    }

    public static void Register()
    {
        Log($"Register() ENTER — exe={Process.GetCurrentProcess().MainModule?.FileName}");
        try
        {
            EnsureStyleIcons();
            EnsureFlagIcons();
            ICustomDestinationList? cdl = null;
            try
            {
                cdl = (ICustomDestinationList)new DestinationList();
                Log("DestinationList COM created");
                // Bind this destination list to our AUMI — Explorer
                // looks up the list by AUMI, not by process ID, so
                // this MUST match SetCurrentProcessExplicitAppUserModelID.
                cdl.SetAppID(AumiId);
                Log($"SetAppID({AumiId}) ok");
                cdl.BeginList(out var maxSlots, ref IID_IObjectArray, out _);
                Log($"BeginList ok, maxSlots={maxSlots}");

                // No icons on the action entries — the EXE's Dimmy
                // icon was the default, and repeating the brand on
                // every menu row was visually heavy. Transparent ICO
                // keeps the row clean (Windows renders nothing in the
                // icon slot when the source is fully transparent).
                // Recording actions — mirror the three global shortcuts so the
                // taskbar right-click can drive dictation / command / meeting
                // without the pill. Labels are static (jumplists don't update
                // live), so the meeting entry says "Start/stop"; HandleForwardedCommand
                // toggles based on the live state. start-dictation / arm-command /
                // toggle-meeting are dispatched there.
                AddCategory(cdl, "Recording", new[]
                {
                    Task("Start / stop dictation", "start-dictation", icon: null),
                    Task("Command (next dictation)", "arm-command",   icon: null),
                    Task("Start / stop meeting",   "toggle-meeting",  icon: MeetingIcon()),
                });

                AddCategory(cdl, "Tasks", new[]
                {
                    // Toggle pill carries the brand: it's the action
                    // that brings the visible Dimmy UI back. Default
                    // (null icon) → the EXE's own dimmy.ico.
                    Task("Toggle pill",    "toggle-pill",   icon: null),
                    // Open the dedicated meeting window — streamed-to-disk
                    // recording + LLM recap. Label is "Meetings" (plural,
                    // neutral) because the window shows the past-meetings
                    // sidebar in addition to the start-new affordance —
                    // "Start meeting…" was misleading: clicking the entry
                    // doesn't begin recording, it opens the window. Notes
                    // glyph in neutral gray reads as a distinct action vs
                    // the dots Settings entry below.
                    Task("Meetings",       "open-meeting",  icon: MeetingIcon()),
                    // Settings = three horizontal dots (universal
                    // "more / configure" affordance, reads cleaner
                    // than a custom-drawn gear at 16px).
                    Task("Open Settings…", "open-settings", icon: SettingsIcon()),
                    Task("Quit Dimmy",     "quit",          icon: QuitIcon()),
                });

                AddCategory(cdl, "Style", new[]
                {
                    Task("Off",          "set-style:off",          icon: StyleIcon("off")),
                    Task("Correct",      "set-style:correct",      icon: StyleIcon("correct")),
                    Task("Elaborate",    "set-style:elaborate",    icon: StyleIcon("elaborate")),
                    Task("Summarize",    "set-style:summarize",    icon: StyleIcon("summarize")),
                    Task("Professional", "set-style:professional", icon: StyleIcon("professional")),
                });

                AddCategory(cdl, "Translate to", new[]
                {
                    Task("No translation", "set-translate:",   icon: FlagIcon("")),
                    Task("English",        "set-translate:en", icon: FlagIcon("en")),
                    Task("Italiano",       "set-translate:it", icon: FlagIcon("it")),
                    Task("Français",       "set-translate:fr", icon: FlagIcon("fr")),
                    Task("Deutsch",        "set-translate:de", icon: FlagIcon("de")),
                    Task("Español",        "set-translate:es", icon: FlagIcon("es")),
                });

                cdl.CommitList();
                Log("CommitList ok — jump list registered");
            }
            finally
            {
                if (cdl is not null) Marshal.ReleaseComObject(cdl);
            }
        }
        catch (Exception ex)
        {
            // Best-effort — a missing jump list is purely cosmetic, the
            // tray + pill right-click menus still work.
            Log($"register FAILED: {ex.GetType().Name}: {ex.Message}");
            Log($"  stack: {ex.StackTrace}");
        }
    }

    /// <summary>Stamp the same AUMI on a specific window via the
    /// Shell's per-window property store. Windows 11 sometimes needs
    /// both the process-wide AUMI (set in App.OnLaunched) AND the
    /// per-window AUMI to bind the jump list correctly to the
    /// taskbar entry — process-only is enough on Win10 but flaky on
    /// Win11 unpackaged apps.</summary>
    public static void SetWindowAumi(IntPtr hwnd)
    {
        if (hwnd == IntPtr.Zero) return;
        try
        {
            var iidPropertyStore = new Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99");
            int hr = SHGetPropertyStoreForWindow(hwnd, ref iidPropertyStore, out var ps);
            if (hr != 0 || ps is null)
            {
                Log($"SetWindowAumi: SHGetPropertyStoreForWindow hr=0x{hr:X}");
                return;
            }
            try
            {
                var key = PKEY_AppUserModel_ID;
                var pv = new PROPVARIANT
                {
                    vt = VT_LPWSTR,
                    pwszVal = Marshal.StringToCoTaskMemUni(AumiId),
                };
                try
                {
                    ps.SetValue(ref key, ref pv);
                    ps.Commit();
                    Log($"SetWindowAumi: stamped {AumiId} on hwnd 0x{hwnd:X}");
                }
                finally
                {
                    if (pv.pwszVal != IntPtr.Zero) Marshal.FreeCoTaskMem(pv.pwszVal);
                }
            }
            finally
            {
                Marshal.ReleaseComObject(ps);
            }
        }
        catch (Exception ex)
        {
            Log($"SetWindowAumi failed: {ex.Message}");
        }
    }

    [DllImport("shell32.dll")]
    private static extern int SHGetPropertyStoreForWindow(
        IntPtr hwnd, ref Guid riid,
        [MarshalAs(UnmanagedType.Interface)] out IPropertyStore ppv);

    private static PROPERTYKEY PKEY_AppUserModel_ID = new()
    {
        // {9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3} pid=5
        fmtid = new Guid("9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3"),
        pid = 5,
    };

    /// <summary>Build a single jump-list entry as an
    /// `IShellLinkW`-wrapped re-launch of our own EXE with a unique
    /// `--command` argument. Optional `icon` overrides the default
    /// (EXE icon at index 0); `iconIndex` lets the icon source be a
    /// DLL/EXE with multiple embedded icons (e.g. `imageres.dll,109`
    /// for the Win11 native settings gear).</summary>
    private static IShellLinkW Task(string title, string command, string? icon = null, int iconIndex = 0)
    {
        var link = (IShellLinkW)new CShellLink();
        var exe = Process.GetCurrentProcess().MainModule?.FileName
            ?? Environment.ProcessPath
            ?? throw new InvalidOperationException("could not resolve current EXE path");
        link.SetPath(exe);
        link.SetArguments($"--command {command}");
        link.SetWorkingDirectory(Path.GetDirectoryName(exe) ?? "");
        if (!string.IsNullOrEmpty(icon))
            link.SetIconLocation(icon, iconIndex);
        else
            link.SetIconLocation(exe, 0);
        link.SetDescription(title);

        // Title goes in the property store as PKEY_Title — without
        // this the jump list shows the EXE name, not the friendly text.
        // NB: `ps` and `link` share the same underlying RCW (just
        // different interface views). Releasing `ps` would tear down
        // the RCW and `link` would be a dead pointer when we return.
        // The caller's `Marshal.ReleaseComObject(item)` in AddCategory
        // is the single canonical release for this RCW.
        var ps = (IPropertyStore)link;
        var pv = new PROPVARIANT { vt = VT_LPWSTR, pwszVal = Marshal.StringToCoTaskMemUni(title) };
        try
        {
            ps.SetValue(ref PKEY_Title, ref pv);
            ps.Commit();
        }
        finally
        {
            if (pv.pwszVal != IntPtr.Zero) Marshal.FreeCoTaskMem(pv.pwszVal);
            // Intentionally do NOT release `ps` — see comment above.
        }
        return link;
    }

    private static void AddCategory(ICustomDestinationList cdl, string name, IShellLinkW[] items)
    {
        var array = (IObjectCollection)new EnumerableObjectCollection();
        foreach (var item in items) array.AddObject(item);
        cdl.AppendCategory(name, (IObjectArray)array);
        Marshal.ReleaseComObject(array);
        foreach (var item in items) Marshal.ReleaseComObject(item);
    }

    // ── COM interop ──────────────────────────────────────────────────

    [ComImport, Guid("77F10CF0-3DB5-4966-B520-B7C54FD35ED6"), ClassInterface(ClassInterfaceType.None)]
    private class DestinationList { }

    [ComImport, Guid("00021401-0000-0000-C000-000000000046"), ClassInterface(ClassInterfaceType.None)]
    private class CShellLink { }

    [ComImport, Guid("2D3468C1-36A7-43B6-AC24-D3F02FD9607A"), ClassInterface(ClassInterfaceType.None)]
    private class EnumerableObjectCollection { }

    private static Guid IID_IObjectArray = new("92CA9DCD-5622-4BBA-A805-5E9F541BD8C9");

    [ComImport, Guid("6332DEBF-87B5-4670-90C0-5E57B408A49E"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface ICustomDestinationList
    {
        void SetAppID([MarshalAs(UnmanagedType.LPWStr)] string pszAppID);
        [PreserveSig] int BeginList(out uint pcMaxSlots, ref Guid riid, out IntPtr ppv);
        [PreserveSig] int AppendCategory([MarshalAs(UnmanagedType.LPWStr)] string pszCategory, IObjectArray poa);
        void AppendKnownCategory(int category);
        void AddUserTasks(IObjectArray poa);
        void CommitList();
        void GetRemovedDestinations(ref Guid riid, out IntPtr ppv);
        void DeleteList([MarshalAs(UnmanagedType.LPWStr)] string pszAppID);
        void AbortList();
    }

    [ComImport, Guid("92CA9DCD-5622-4BBA-A805-5E9F541BD8C9"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IObjectArray
    {
        void GetCount(out uint cObjects);
        void GetAt(uint uiIndex, ref Guid riid, [MarshalAs(UnmanagedType.IUnknown)] out object ppv);
    }

    [ComImport, Guid("5632B1A4-E38A-400A-928A-D4CD63230295"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IObjectCollection
    {
        // IObjectArray
        void GetCount(out uint cObjects);
        void GetAt(uint uiIndex, ref Guid riid, [MarshalAs(UnmanagedType.IUnknown)] out object ppv);
        // IObjectCollection
        void AddObject([MarshalAs(UnmanagedType.IUnknown)] object pvObject);
        void AddFromArray(IObjectArray poaSource);
        void RemoveObjectAt(uint uiIndex);
        void Clear();
    }

    [ComImport, Guid("000214F9-0000-0000-C000-000000000046"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IShellLinkW
    {
        void GetPath([Out, MarshalAs(UnmanagedType.LPWStr)] System.Text.StringBuilder pszFile, int cch,
            IntPtr pfd, uint fFlags);
        void GetIDList(out IntPtr ppidl);
        void SetIDList(IntPtr pidl);
        void GetDescription([Out, MarshalAs(UnmanagedType.LPWStr)] System.Text.StringBuilder pszName, int cch);
        void SetDescription([MarshalAs(UnmanagedType.LPWStr)] string pszName);
        void GetWorkingDirectory([Out, MarshalAs(UnmanagedType.LPWStr)] System.Text.StringBuilder pszDir, int cch);
        void SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string pszDir);
        void GetArguments([Out, MarshalAs(UnmanagedType.LPWStr)] System.Text.StringBuilder pszArgs, int cch);
        void SetArguments([MarshalAs(UnmanagedType.LPWStr)] string pszArgs);
        void GetHotkey(out short pwHotkey);
        void SetHotkey(short wHotkey);
        void GetShowCmd(out int piShowCmd);
        void SetShowCmd(int iShowCmd);
        void GetIconLocation([Out, MarshalAs(UnmanagedType.LPWStr)] System.Text.StringBuilder pszIconPath, int cch,
            out int piIcon);
        void SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string pszIconPath, int iIcon);
        void SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string pszPathRel, uint dwReserved);
        void Resolve(IntPtr hwnd, uint fFlags);
        void SetPath([MarshalAs(UnmanagedType.LPWStr)] string pszFile);
    }

    [ComImport, Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IPropertyStore
    {
        void GetCount(out uint cProps);
        void GetAt(uint iProp, out PROPERTYKEY pkey);
        void GetValue(ref PROPERTYKEY key, out PROPVARIANT pv);
        void SetValue(ref PROPERTYKEY key, ref PROPVARIANT pv);
        void Commit();
    }

    [StructLayout(LayoutKind.Sequential, Pack = 4)]
    private struct PROPERTYKEY
    {
        public Guid fmtid;
        public uint pid;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct PROPVARIANT
    {
        public ushort vt;
        public ushort wReserved1;
        public ushort wReserved2;
        public ushort wReserved3;
        public IntPtr pwszVal;
        public IntPtr padding;
    }

    private const ushort VT_LPWSTR = 31;

    private static PROPERTYKEY PKEY_Title = new()
    {
        fmtid = new Guid("F29F85E0-4FF9-1068-AB91-08002B27B3D9"),
        pid = 2,
    };

    // ── Style icons (colored dots next to Style menu entries) ────────

    private static readonly Dictionary<string, string> _styleIconPaths = new();

    private static string? StyleIcon(string style) =>
        _styleIconPaths.TryGetValue(style, out var p) ? p : null;

    /// <summary>Generate one tiny colored-dot ICO per LLM style and
    /// cache the file paths for the jump-list entries to point at via
    /// `IShellLinkW.SetIconLocation`. Colors mirror the pill's
    /// `AppViewModel.StyleColors` palette so the menu, the pill dot,
    /// and the taskbar overlay all read as one consistent visual
    /// language.</summary>
    private static void EnsureStyleIcons()
    {
        var dir = Path.Combine(Path.GetTempPath(), "dimmy_style_icons_v2");
        try { Directory.CreateDirectory(dir); } catch { return; }

        // (style, BGR color triplet) — Windows ICO is BGRA, not RGBA.
        // Colors mirror AppViewModel.StyleColors verbatim; if the
        // palette evolves, update both at once.
        var palette = new (string style, byte b, byte g, byte r)[]
        {
            ("off",          0xB1, 0xB0, 0x41),
            ("correct",      0xBF, 0xD4, 0x2D),
            ("summarize",    0x24, 0xBF, 0xFB),
            ("elaborate",    0x80, 0xDE, 0x4A),
            ("comprehensible", 0xF8, 0xBD, 0x38),
            ("professional", 0xB6, 0x72, 0xF4),
            ("prompt",       0xFA, 0x8B, 0xA7),
            ("genz",         0xF9, 0x79, 0xE8),
            ("boomer",       0x16, 0x73, 0xF9),
            ("emoji",        0x15, 0xCC, 0xFA),
            ("acronyms",     0xEE, 0xD3, 0x22),
            ("imbruttito",   0x44, 0x44, 0xEF),
            ("custom",       0x3C, 0x92, 0xFB),
        };

        foreach (var (style, b, g, r) in palette)
        {
            var path = Path.Combine(dir, $"style_{style}.ico");
            try
            {
                if (!File.Exists(path))
                    File.WriteAllBytes(path, BuildCircleIco(32, b, g, r));
                _styleIconPaths[style] = path;
            }
            catch (Exception ex)
            {
                Log($"EnsureStyleIcons[{style}]: {ex.Message}");
            }
        }
    }

    // ── Translate-to flag icons (vertical stripes) ────────────────────

    private static readonly Dictionary<string, string> _flagIconPaths = new();

    private static string? FlagIcon(string code) =>
        _flagIconPaths.TryGetValue(code, out var p) ? p : null;

    /// <summary>Generate a vertical-stripe flag-ish ICO per supported
    /// translate-to language. We deliberately avoid circles for the
    /// flag entries — flags read as flat-rectangle stripes; using
    /// dots would look identical to the Style entries above.
    ///
    /// For "EN" we don't use the Union Jack (too complex at 16px) —
    /// red/white/blue stripes give us a USA/UK-flavoured palette
    /// that's distinct from the FR stripes (blue/white/red — same
    /// colours, opposite order, so the eye tells them apart).</summary>
    private static void EnsureFlagIcons()
    {
        var dir = Path.Combine(Path.GetTempPath(), "dimmy_flag_icons_v3");
        try { Directory.CreateDirectory(dir); } catch { return; }

        // BGR triplets per stripe (left → right).
        // NB: Real-life Italy/France use vertical stripes; Germany +
        // Spain use horizontals. We render everything vertical for
        // visual consistency in the menu — accuracy < legibility at
        // this rendering size.
        var GREEN  = (b: (byte)0x4D, g: (byte)0x9F, r: (byte)0x00); // tricolore green
        var WHITE  = (b: (byte)0xFF, g: (byte)0xFF, r: (byte)0xFF);
        var RED    = (b: (byte)0x42, g: (byte)0x32, r: (byte)0xCE); // tricolore red
        var BLUE   = (b: (byte)0x93, g: (byte)0x26, r: (byte)0x00); // french blue
        var BLACK  = (b: (byte)0x10, g: (byte)0x10, r: (byte)0x10);
        var YELLOW = (b: (byte)0x00, g: (byte)0xCC, r: (byte)0xFF);
        var GRAY   = (b: (byte)0xA0, g: (byte)0xA0, r: (byte)0xA0);

        // Old-Glory red + navy for the EN entry — USA-specific shades
        // so it reads as "American" even though we render only 3
        // stripes (no stars, no 13 stripes — too small to draw at 16px
        // and the colours alone carry enough signal).
        var US_RED  = (b: (byte)0x34, g: (byte)0x22, r: (byte)0xB2); // #B22234
        var US_BLUE = (b: (byte)0x6E, g: (byte)0x3B, r: (byte)0x3C); // #3C3B6E

        var palette = new (string code, (byte b, byte g, byte r)[] stripes)[]
        {
            ("",   new[] { GRAY, GRAY, GRAY }),         // no translation = neutral grey
            ("it", new[] { GREEN, WHITE, RED }),
            ("en", new[] { US_RED, WHITE, US_BLUE }),    // USA palette, distinct from FR by colour AND order
            ("fr", new[] { BLUE, WHITE, RED }),
            ("de", new[] { BLACK, RED, YELLOW }),
            ("es", new[] { RED, YELLOW, RED }),
        };

        foreach (var (code, stripes) in palette)
        {
            var name = string.IsNullOrEmpty(code) ? "none" : code;
            var path = Path.Combine(dir, $"flag_{name}.ico");
            try
            {
                if (!File.Exists(path))
                {
                    // Special-case EN: render the canonical US flag
                    // composite (navy corner + horizontal stripes)
                    // instead of 3 vertical stripes — much more
                    // recognisable and distinct from FR.
                    var bytes = code == "en"
                        ? BuildUsaFlagIco(32)
                        : BuildStripeIco(32, stripes);
                    File.WriteAllBytes(path, bytes);
                }
                _flagIconPaths[code] = path;
            }
            catch (Exception ex)
            {
                Log($"EnsureFlagIcons[{name}]: {ex.Message}");
            }
        }
    }

    /// <summary>Build a 32×32 simplified USA flag: 7 alternating
    /// red/white horizontal stripes + a navy-blue rectangle in the
    /// canton (top-left, ~⅖ width × top 3 stripes). No stars — at
    /// 16×16 they'd be one-pixel dots and add noise instead of
    /// signal. The proportions follow the real flag's 1.9:1 aspect
    /// scaled to a square — just barely off but reads correctly at
    /// menu-icon size.</summary>
    private static byte[] BuildUsaFlagIco(int size)
    {
        var pixels = new byte[size * size * 4];

        // 7 stripes: R-W-R-W-R-W-R (top to bottom). Real US flag has
        // 13, but at 32px height each stripe would be ~2.5px and the
        // result is unreadable post-downscale. 7 stripes ≈ 4.5 px each
        // at 32 → ~2.3 px at 16, still resolves cleanly.
        const int Stripes = 7;
        int stripeHeight = (size + Stripes - 1) / Stripes;
        int cantonWidth  = (int)(size * 0.40);
        int cantonHeight = stripeHeight * 4 - 1;

        var US_RED  = (b: (byte)0x34, g: (byte)0x22, r: (byte)0xB2);
        var WHITE   = (b: (byte)0xFF, g: (byte)0xFF, r: (byte)0xFF);
        var US_BLUE = (b: (byte)0x6E, g: (byte)0x3B, r: (byte)0x3C);

        for (int y = 0; y < size; y++)
        {
            int stripeIdx = Math.Min(y / stripeHeight, Stripes - 1);
            bool isWhite = stripeIdx % 2 == 1;
            var stripeColor = isWhite ? WHITE : US_RED;

            for (int x = 0; x < size; x++)
            {
                var (b, g, r) = (x < cantonWidth && y < cantonHeight)
                    ? US_BLUE
                    : stripeColor;
                int i = (y * size + x) * 4;
                pixels[i] = b;
                pixels[i + 1] = g;
                pixels[i + 2] = r;
                pixels[i + 3] = 255;
            }
        }
        return EncodeBgraAsIco(size, pixels);
    }

    /// <summary>Build a 32×32 BGRA ICO with N equal vertical stripes.
    /// Pixel-perfect, no anti-aliasing — flags should read as flat
    /// fields, not gradients. The 32×32 source downscales cleanly to
    /// the 16×16 jump-list slot Windows actually displays.</summary>
    private static byte[] BuildStripeIco(int size, (byte b, byte g, byte r)[] stripes)
    {
        var pixels = new byte[size * size * 4];
        // Distribute pixels across stripes: integer division leaves
        // a 1px remainder for sizes that don't evenly divide; we
        // attribute it to the last stripe so colours stay vertically
        // aligned at the left edge.
        int stripeWidth = size / stripes.Length;
        for (int y = 0; y < size; y++)
        {
            for (int x = 0; x < size; x++)
            {
                int idx = Math.Min(x / stripeWidth, stripes.Length - 1);
                var (b, g, r) = stripes[idx];
                int i = (y * size + x) * 4;
                pixels[i] = b;
                pixels[i + 1] = g;
                pixels[i + 2] = r;
                pixels[i + 3] = 255;
            }
        }

        using var ms = new MemoryStream();
        using var bw = new BinaryWriter(ms);
        bw.Write((short)0); bw.Write((short)1); bw.Write((short)1);
        bw.Write((byte)size); bw.Write((byte)size); bw.Write((byte)0); bw.Write((byte)0);
        bw.Write((short)1); bw.Write((short)32);
        int dataSize = 40 + pixels.Length;
        bw.Write(dataSize); bw.Write(22);
        bw.Write(40); bw.Write(size); bw.Write(size * 2);
        bw.Write((short)1); bw.Write((short)32);
        bw.Write(0); bw.Write(pixels.Length);
        bw.Write(0); bw.Write(0); bw.Write(0); bw.Write(0);
        for (int y = size - 1; y >= 0; y--)
            bw.Write(pixels, y * size * 4, size * 4);
        return ms.ToArray();
    }

    // ── Tasks-section icons (transparent + Quit X) ───────────────────

    private static string? _quitIconPath;
    private static string? _settingsIconPath;
    private static string? _meetingIconPath;

    /// <summary>Tone tuple for monochrome jump-list icons. Picked from
    /// the system theme at the moment we (re)build the cache: the
    /// jump-list background follows Windows AppsUseLightTheme — light
    /// menu wants a dark glyph (#525252), dark menu wants a light glyph
    /// (#C8C8C8). Rebuilding when the system flips themes is the
    /// caller's responsibility (we file-suffix the cache by tone so
    /// nothing leaks across).</summary>
    private static (byte b, byte g, byte r, string suffix) MonoTone()
    {
        // The jump-list lives in the TASKBAR chrome, not in our app
        // window — so the relevant Windows theme switch is
        // `SystemUsesLightTheme` (taskbar / Start / action center), not
        // `AppsUseLightTheme` (app surfaces). Users routinely run apps
        // light + taskbar dark; reading the wrong key picked the
        // 0x52 dark-grey glyph against a dark taskbar → invisible.
        // Burned 2026-05-30.
        if (Helpers.ThemeHelper.SystemTaskbarIsDark())
            return (0xC8, 0xC8, 0xC8, "_dark");
        return (0x52, 0x52, 0x52, "_light");
    }

    /// <summary>True if the cached path still matches the current tone
    /// (the suffix `_light` / `_dark` is in the filename). When the
    /// user flips the system theme mid-process the old cache hit
    /// would otherwise return the wrong-tone ICO, leaving icons
    /// invisible until a relaunch.</summary>
    private static bool CacheMatchesCurrentTone(string? path)
    {
        if (string.IsNullOrEmpty(path) || !File.Exists(path)) return false;
        var (_, _, _, suffix) = MonoTone();
        return path.Contains(suffix, System.StringComparison.OrdinalIgnoreCase);
    }

    /// <summary>Path to a 32×32 X close glyph. Mirrors the
    /// "destructive but neutral" affordance Spotify / Slack use for
    /// Quit in their tray menus — red is reserved for state errors,
    /// not for normal user-initiated exits. Tone tracks the system
    /// theme (light bg → dark glyph, dark bg → light glyph) so the
    /// icon stays visible whichever theme the user has Windows in.</summary>
    private static string QuitIcon()
    {
        if (CacheMatchesCurrentTone(_quitIconPath))
            return _quitIconPath!;
        var dir = Path.Combine(Path.GetTempPath(), "dimmy_misc_icons_v2");
        try { Directory.CreateDirectory(dir); } catch { return ""; }
        var (b, g, r, suffix) = MonoTone();
        var path = Path.Combine(dir, $"quit_x{suffix}.ico");
        try
        {
            if (!File.Exists(path))
                File.WriteAllBytes(path, BuildXIco(32, b, g, r));
            _quitIconPath = path;
            return path;
        }
        catch (Exception ex) { Log($"QuitIcon: {ex.Message}"); return ""; }
    }

    /// <summary>Path to a 32×32 notes-document glyph for the "Meetings"
    /// jump-list entry. Document with three horizontal lines reads as
    /// "notes / artifacts" — semantically aligned with what the meeting
    /// window holds (transcripts + recaps), not a recording action. The
    /// red mic was misleading: it implied clicking the entry would
    /// start recording, when in fact the window opens with a list view
    /// and a Start button. Neutral gray matches the Settings dots.</summary>
    private static string MeetingIcon()
    {
        if (CacheMatchesCurrentTone(_meetingIconPath))
            return _meetingIconPath!;
        var dir = Path.Combine(Path.GetTempPath(), "dimmy_misc_icons_v2");
        try { Directory.CreateDirectory(dir); } catch { return ""; }
        // _v5 supersedes _v4 (single-tone gray). Tone now follows the
        // system theme via MonoTone() — file suffix splits the cache.
        var (b, g, r, suffix) = MonoTone();
        var path = Path.Combine(dir, $"meeting_notes_v5{suffix}.ico");
        try
        {
            if (!File.Exists(path))
                File.WriteAllBytes(path, BuildNotesIco(32, b, g, r));
            _meetingIconPath = path;
            return path;
        }
        catch (Exception ex) { Log($"MeetingIcon: {ex.Message}"); return ""; }
    }

    /// <summary>Build a 32×32 document-with-lines glyph: rounded rectangle
    /// outline + three horizontal text lines inside. Anti-aliased via
    /// 4×4 super-sampling. Proportions tuned to read at 16×16 after
    /// Windows downscales the cached 32×32 ICO.</summary>
    private static byte[] BuildNotesIco(int size, byte b, byte g, byte r)
    {
        var pixels = new byte[size * size * 4];
        const int Samples = 4;

        // Outer document rectangle (rounded corners). Slightly off-centre
        // top→bottom so the bottom line has more breathing room — reads
        // less cramped at 16×16 than a perfectly centred rectangle.
        double docLeft = size * 0.20;
        double docRight = size * 0.80;
        double docTop = size * 0.14;
        double docBottom = size * 0.86;
        double cornerR = size * 0.08;
        double strokeW = size * 0.07;
        double innerLeft = docLeft + strokeW;
        double innerRight = docRight - strokeW;
        double innerTop = docTop + strokeW;
        double innerBottom = docBottom - strokeW;

        // Three horizontal text lines inside the document.
        double lineLeft = size * 0.30;
        double lineRight = size * 0.70;
        double lineHeight = size * 0.07;
        double[] lineTops = {
            size * 0.30,
            size * 0.46,
            size * 0.62,
        };

        for (int y = 0; y < size; y++)
        {
            for (int x = 0; x < size; x++)
            {
                int hits = 0;
                for (int sy = 0; sy < Samples; sy++)
                {
                    for (int sx = 0; sx < Samples; sx++)
                    {
                        double px = x + (sx + 0.5) / Samples;
                        double py = y + (sy + 0.5) / Samples;
                        bool inside = false;

                        // Document outline = filled rounded rect minus
                        // its inset rounded rect. Hit-test by checking
                        // outer-in but NOT inner-in.
                        if (PointInRoundRect(px, py, docLeft, docTop, docRight, docBottom, cornerR)
                            && !PointInRoundRect(px, py, innerLeft, innerTop, innerRight, innerBottom, cornerR - strokeW * 0.5))
                        {
                            inside = true;
                        }

                        // Three horizontal text lines.
                        if (!inside && px >= lineLeft && px <= lineRight)
                        {
                            foreach (var top in lineTops)
                            {
                                if (py >= top && py <= top + lineHeight)
                                {
                                    inside = true;
                                    break;
                                }
                            }
                        }

                        if (inside) hits++;
                    }
                }
                if (hits == 0) continue;
                int alpha = (hits * 255) / (Samples * Samples);
                int i = (y * size + x) * 4;
                pixels[i] = (byte)(b * alpha / 255);
                pixels[i + 1] = (byte)(g * alpha / 255);
                pixels[i + 2] = (byte)(r * alpha / 255);
                pixels[i + 3] = (byte)alpha;
            }
        }
        return EncodeBgraAsIco(size, pixels);
    }

    /// <summary>Hit-test a point against an axis-aligned rounded rect.
    /// Treats negative or sub-pixel `r` as a plain rectangle.</summary>
    private static bool PointInRoundRect(double px, double py,
        double left, double top, double right, double bottom, double r)
    {
        if (px < left || px > right || py < top || py > bottom) return false;
        if (r <= 0) return true;
        // Outside the corner caps' bounding boxes → straight edge,
        // automatically inside.
        if ((px >= left + r && px <= right - r) || (py >= top + r && py <= bottom - r))
            return true;
        // Nearest corner centre.
        double cx = px < left + r ? left + r : right - r;
        double cy = py < top + r ? top + r : bottom - r;
        double dx = px - cx, dy = py - cy;
        return dx * dx + dy * dy <= r * r;
    }

    /// <summary>Path to a 32×32 "•••" three-horizontal-dots glyph.
    /// Universal "more / settings / options" affordance — same shape
    /// Discord, Slack, Teams use in their context menus. Reads better
    /// at 16×16 than a custom-drawn gear (gear teeth turn to mush
    /// post-downscale).</summary>
    private static string SettingsIcon()
    {
        if (CacheMatchesCurrentTone(_settingsIconPath))
            return _settingsIconPath!;
        var dir = Path.Combine(Path.GetTempPath(), "dimmy_misc_icons_v2");
        try { Directory.CreateDirectory(dir); } catch { return ""; }
        var (b, g, r, suffix) = MonoTone();
        var path = Path.Combine(dir, $"settings_dots{suffix}.ico");
        try
        {
            if (!File.Exists(path))
                File.WriteAllBytes(path, BuildDotsIco(32, b, g, r));
            _settingsIconPath = path;
            return path;
        }
        catch (Exception ex) { Log($"SettingsIcon: {ex.Message}"); return ""; }
    }

    /// <summary>Build a 32×32 "•••" — three filled circles centred
    /// horizontally across the icon, anti-aliased via 4×4 super-
    /// sampling (same routine as `BuildCircleIco`). Spacing chosen so
    /// the three dots are visually equidistant after the 32→16
    /// downscale Windows applies in the menu.</summary>
    private static byte[] BuildDotsIco(int size, byte b, byte g, byte r)
    {
        var pixels = new byte[size * size * 4];
        double[] cxs = { size * 0.22, size * 0.50, size * 0.78 };
        double cy = size / 2.0;
        double rad = size * 0.11;
        double radSq = rad * rad;
        const int Samples = 4;

        for (int y = 0; y < size; y++)
        {
            for (int x = 0; x < size; x++)
            {
                int hits = 0;
                for (int sy = 0; sy < Samples; sy++)
                {
                    for (int sx = 0; sx < Samples; sx++)
                    {
                        double px = x + (sx + 0.5) / Samples;
                        double py = y + (sy + 0.5) / Samples - cy;
                        bool inside = false;
                        for (int k = 0; k < 3 && !inside; k++)
                        {
                            double dx = px - cxs[k];
                            if (dx * dx + py * py <= radSq) inside = true;
                        }
                        if (inside) hits++;
                    }
                }
                if (hits == 0) continue;
                int alpha = (hits * 255) / (Samples * Samples);
                int i = (y * size + x) * 4;
                pixels[i] = (byte)(b * alpha / 255);
                pixels[i + 1] = (byte)(g * alpha / 255);
                pixels[i + 2] = (byte)(r * alpha / 255);
                pixels[i + 3] = (byte)alpha;
            }
        }
        return EncodeBgraAsIco(size, pixels);
    }

    /// <summary>Build a 32×32 anti-aliased "X" glyph centered on a
    /// transparent background. Two diagonal strokes ~3.5 px thick
    /// (post-downscale to 16×16, that's ~1.7 px — visible but not
    /// chunky). Uses perpendicular-distance-to-line for clean AA.</summary>
    private static byte[] BuildXIco(int size, byte b, byte g, byte r)
    {
        var pixels = new byte[size * size * 4];
        int pad = size / 6;          // less padding so the X reads bigger
        int innerMin = pad;
        int innerMax = size - 1 - pad;
        // 5 px half-width at 32 → ~2.5 px after downscale to 16. Below
        // ~2 px the AA edges eat the whole stroke and the X disappears
        // (which is what the v1 icon did at 3.5).
        double thickness = 5.0;

        for (int y = 0; y < size; y++)
        {
            for (int x = 0; x < size; x++)
            {
                // X bounded inside the [pad, size-1-pad] square.
                if (x < innerMin || x > innerMax || y < innerMin || y > innerMax)
                    continue;

                // Translate so the X is centered around (0, 0) in a
                // square of side `inner`. Then perpendicular distance
                // to either diagonal: |x-y|/√2 and |x+y|/√2 after the
                // translate.
                double cx = x - (innerMin + innerMax) / 2.0;
                double cy = y - (innerMin + innerMax) / 2.0;
                double d1 = Math.Abs(cx - cy) / Math.Sqrt(2);
                double d2 = Math.Abs(cx + cy) / Math.Sqrt(2);
                double d = Math.Min(d1, d2);

                if (d > thickness) continue;

                // Anti-alias the last pixel of each stroke.
                double coverage = d <= thickness - 1 ? 1.0 : (thickness - d);
                int alpha = (int)(coverage * 255);
                if (alpha <= 0) continue;
                int i = (y * size + x) * 4;
                pixels[i] = (byte)(b * alpha / 255);
                pixels[i + 1] = (byte)(g * alpha / 255);
                pixels[i + 2] = (byte)(r * alpha / 255);
                pixels[i + 3] = (byte)alpha;
            }
        }
        return EncodeBgraAsIco(size, pixels);
    }

    /// <summary>Wrap a BGRA pixel array as a single-image ICO file.
    /// Extracted so the circle / stripe / X / transparent builders
    /// don't each repeat the BITMAPINFOHEADER fiddle.</summary>
    private static byte[] EncodeBgraAsIco(int size, byte[] pixels)
    {
        using var ms = new MemoryStream();
        using var bw = new BinaryWriter(ms);
        bw.Write((short)0); bw.Write((short)1); bw.Write((short)1);
        bw.Write((byte)size); bw.Write((byte)size); bw.Write((byte)0); bw.Write((byte)0);
        bw.Write((short)1); bw.Write((short)32);
        int dataSize = 40 + pixels.Length;
        bw.Write(dataSize); bw.Write(22);
        bw.Write(40); bw.Write(size); bw.Write(size * 2);
        bw.Write((short)1); bw.Write((short)32);
        bw.Write(0); bw.Write(pixels.Length);
        bw.Write(0); bw.Write(0); bw.Write(0); bw.Write(0);
        for (int y = size - 1; y >= 0; y--)
            bw.Write(pixels, y * size * 4, size * 4);
        return ms.ToArray();
    }

    /// <summary>Build a 32×32 anti-aliased colored-circle ICO. Same
    /// pattern as `TaskbarService.BuildCircleIco` — duplicated here
    /// to avoid coupling JumpListService to TaskbarService internals
    /// (the two services have unrelated lifecycles).</summary>
    private static byte[] BuildCircleIco(int size, byte b, byte g, byte r)
    {
        var pixels = new byte[size * size * 4];
        double cx = size / 2.0, cy = size / 2.0;
        double rad = size / 2.0 - 1.0;
        double radSq = rad * rad;
        const int Samples = 4;
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
                int alpha = (hits * 255) / (Samples * Samples);
                pixels[i] = (byte)(b * alpha / 255);
                pixels[i + 1] = (byte)(g * alpha / 255);
                pixels[i + 2] = (byte)(r * alpha / 255);
                pixels[i + 3] = (byte)alpha;
            }
        }

        using var ms = new MemoryStream();
        using var bw = new BinaryWriter(ms);
        bw.Write((short)0); bw.Write((short)1); bw.Write((short)1);
        bw.Write((byte)size); bw.Write((byte)size); bw.Write((byte)0); bw.Write((byte)0);
        bw.Write((short)1); bw.Write((short)32);
        int dataSize = 40 + pixels.Length;
        bw.Write(dataSize); bw.Write(22);
        bw.Write(40); bw.Write(size); bw.Write(size * 2);
        bw.Write((short)1); bw.Write((short)32);
        bw.Write(0); bw.Write(pixels.Length);
        bw.Write(0); bw.Write(0); bw.Write(0); bw.Write(0);
        for (int y = size - 1; y >= 0; y--)
            bw.Write(pixels, y * size * 4, size * 4);
        return ms.ToArray();
    }
}
