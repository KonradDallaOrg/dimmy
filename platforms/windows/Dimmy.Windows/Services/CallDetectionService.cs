using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Linq;
using System.Runtime.InteropServices;
using Microsoft.UI.Dispatching;

using Dimmy.Windows.Interop;

namespace Dimmy.Windows.Services;

/// Sample the default capture device once per second and push a
/// mic-active observation to the Rust call-detector state machine.
///
/// Detection signal: audio is primary, app is reinforcement.
/// - `mic_active` = true iff at least one audio session on the default
///   capture endpoint is in `AudioSessionState.Active`. Picks up Teams,
///   Zoom, Discord, browser-based calls (Meet, Whereby) — any process
///   that opened a capture stream.
/// - `app_id` = a lowercase canonical id ("teams" / "zoom" / "slack" /
///   "discord" / "webex") iff a process from the whitelist is currently
///   running. NULL otherwise — the Rust state machine handles the
///   "generic mic in use" case via a single global cooldown slot.
///
/// Polling-vs-events: Windows exposes `IAudioSessionNotification` as
/// the push-based alternative, but registering it from C# requires
/// implementing a COM callback object (CCW) which is fiddly and
/// brittle across COM apartments. Polling at 1 Hz costs ~50 µs per
/// tick and matches the documented exception in CLAUDE.md
/// "Continuous sampling on OS APIs without notifications" — the same
/// precedent Mac uses for TCC trust-state polling.
internal sealed class CallDetectionService : IDisposable
{
    private readonly DispatcherQueue _dispatcher;
    private DispatcherQueueTimer? _timer;
    private bool _disposed;
    private bool _isEnabled;

    // Win process name → canonical app id. Names are matched
    // case-insensitively (ProcessName comparator). Multiple variants
    // collapse to one id so cooldown / exclusion keys stay stable
    // across Teams' / Zoom's installer flavors.
    private static readonly Dictionary<string, string> ProcessToAppId =
        new(StringComparer.OrdinalIgnoreCase)
        {
            ["Teams"] = "teams",
            ["ms-teams"] = "teams",
            ["MSTeams"] = "teams",
            ["Microsoft.Teams"] = "teams",
            ["Zoom"] = "zoom",
            ["Zoomus"] = "zoom",
            ["slack"] = "slack",
            ["Discord"] = "discord",
            ["DiscordCanary"] = "discord",
            ["DiscordPTB"] = "discord",
            ["WebexHost"] = "webex",
            ["Webex"] = "webex",
            ["CiscoSpark"] = "webex",
            ["atmgr"] = "webex",
        };

    public CallDetectionService(DispatcherQueue dispatcher)
    {
        _dispatcher = dispatcher ?? throw new ArgumentNullException(nameof(dispatcher));
    }

    public void Start()
    {
        if (_timer != null) return;
        _timer = _dispatcher.CreateTimer();
        _timer.Interval = TimeSpan.FromSeconds(1);
        _timer.Tick += OnTick;
        _timer.Start();
        _isEnabled = true;
        App.Log("CallDetectionService started (1 Hz capture-session poll)", "CallDetect");
    }

    public void Stop()
    {
        _isEnabled = false;
        if (_timer != null)
        {
            _timer.Stop();
            _timer.Tick -= OnTick;
            _timer = null;
        }
    }

    public void SetEnabled(bool enabled)
    {
        _isEnabled = enabled;
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        Stop();
    }

    private void OnTick(DispatcherQueueTimer sender, object args)
    {
        if (!_isEnabled) return;
        try
        {
            var (active, appId) = SampleMicState();
            DimmyNative.dimmy_call_signal(active ? 1 : 0, appId);
        }
        catch (Exception ex)
        {
            // Never let a transient COM error kill the timer — log
            // and try again next tick. Audio endpoints can disappear
            // briefly during device-switch events.
            App.Log($"call-detection tick failed: {ex.Message}", "CallDetect");
        }
    }

    /// Enumerate active capture sessions and pair with a VoIP app
    /// match. Returns (anyActive, canonicalAppIdOrNull).
    private static (bool active, string? appId) SampleMicState()
    {
        bool anyActive = false;
        // Pull capturing PIDs so we can prefer "Teams that owns the
        // mic stream" over "Teams just running in tray". When PID
        // resolution fails (system sounds session, expired session),
        // we still set anyActive but leave app inference to the
        // running-processes scan below.
        var capturingPids = new HashSet<uint>();

        IMMDeviceEnumerator? enumerator = null;
        IMMDevice? device = null;
        IAudioSessionManager2? manager = null;
        IAudioSessionEnumerator? sessionEnum = null;
        try
        {
            var enumType = Type.GetTypeFromCLSID(CLSID_MMDeviceEnumerator);
            if (enumType == null) return (false, null);
            enumerator = (IMMDeviceEnumerator?)Activator.CreateInstance(enumType);
            if (enumerator == null) return (false, null);

            int hr = enumerator.GetDefaultAudioEndpoint(EDataFlow.eCapture, ERole.eMultimedia, out device);
            if (hr != 0 || device == null) return (false, null);

            var iid = IID_IAudioSessionManager2;
            hr = device.Activate(ref iid, CLSCTX_ALL, IntPtr.Zero, out object pManager);
            if (hr != 0 || pManager == null) return (false, null);
            manager = (IAudioSessionManager2)pManager;

            hr = manager.GetSessionEnumerator(out sessionEnum);
            if (hr != 0 || sessionEnum == null) return (false, null);

            hr = sessionEnum.GetCount(out int count);
            if (hr != 0) return (false, null);

            for (int i = 0; i < count; i++)
            {
                IAudioSessionControl? control = null;
                IAudioSessionControl2? control2 = null;
                try
                {
                    hr = sessionEnum.GetSession(i, out control);
                    if (hr != 0 || control == null) continue;
                    hr = control.GetState(out int stateRaw);
                    if (hr != 0) continue;
                    if (stateRaw != (int)AudioSessionState.Active) continue;

                    anyActive = true;
                    try
                    {
                        control2 = (IAudioSessionControl2)control;
                        if (control2.GetProcessId(out int pid) == 0 && pid > 0)
                        {
                            capturingPids.Add((uint)pid);
                        }
                    }
                    catch
                    {
                        // QI to IAudioSessionControl2 can fail on
                        // synthetic sessions (system sounds, kernel).
                        // Already counted as active — just skip the
                        // PID enrichment for this session.
                    }
                }
                finally
                {
                    if (control2 != null) Marshal.ReleaseComObject(control2);
                    if (control != null) Marshal.ReleaseComObject(control);
                }
            }
        }
        catch
        {
            // Surface as "mic not active" rather than throwing —
            // we'll retry next tick.
            anyActive = false;
        }
        finally
        {
            if (sessionEnum != null) Marshal.ReleaseComObject(sessionEnum);
            if (manager != null) Marshal.ReleaseComObject(manager);
            if (device != null) Marshal.ReleaseComObject(device);
            if (enumerator != null) Marshal.ReleaseComObject(enumerator);
        }

        if (!anyActive) return (false, null);

        // App inference, best-effort. Prefer matching a capturing-PID
        // to a whitelist process. Fall back to "any VoIP process is
        // running" so the popup still gets a hint when PID resolution
        // failed on the active session.
        var appId = InferAppIdFromProcesses(capturingPids);
        return (true, appId);
    }

    private static string? InferAppIdFromProcesses(HashSet<uint> capturingPids)
    {
        Process[]? procs = null;
        try
        {
            procs = Process.GetProcesses();
            // First pass: any capturing PID matches the whitelist?
            foreach (var p in procs)
            {
                try
                {
                    if (capturingPids.Count > 0 && !capturingPids.Contains((uint)p.Id)) continue;
                    if (ProcessToAppId.TryGetValue(p.ProcessName, out var hit)) return hit;
                }
                catch
                {
                    // ProcessName can throw "process has exited" — skip.
                }
            }
            // Second pass: any whitelist process running (PID-agnostic
            // fallback). Useful when QI to IAudioSessionControl2 failed.
            if (capturingPids.Count == 0)
            {
                foreach (var p in procs)
                {
                    try
                    {
                        if (ProcessToAppId.TryGetValue(p.ProcessName, out var hit)) return hit;
                    }
                    catch { }
                }
            }
        }
        catch
        {
            // GetProcesses can momentarily fail under heavy contention.
        }
        finally
        {
            if (procs != null) foreach (var p in procs) p.Dispose();
        }
        return null;
    }

    // ── COM interop: IMMDeviceEnumerator + IAudioSessionManager2 ────────

    private static readonly Guid CLSID_MMDeviceEnumerator =
        new("BCDE0395-E52F-467C-8E3D-C4579291692E");
    private static readonly Guid IID_IAudioSessionManager2 =
        new("77AA99A0-1BD6-484F-8BC7-2C654C9A9B6F");
    private const uint CLSCTX_ALL = 0x17;

    private enum EDataFlow { eRender = 0, eCapture = 1, eAll = 2 }
    private enum ERole { eConsole = 0, eMultimedia = 1, eCommunications = 2 }
    private enum AudioSessionState { Inactive = 0, Active = 1, Expired = 2 }

    [ComImport, Guid("A95664D2-9614-4F35-A746-DE8DB63617E6"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDeviceEnumerator
    {
        [PreserveSig] int EnumAudioEndpoints(EDataFlow dataFlow, int dwStateMask, out IntPtr ppDevices);
        [PreserveSig] int GetDefaultAudioEndpoint(EDataFlow dataFlow, ERole role, out IMMDevice? ppEndpoint);
        [PreserveSig] int GetDevice([MarshalAs(UnmanagedType.LPWStr)] string pwstrId, out IMMDevice? ppDevice);
        [PreserveSig] int RegisterEndpointNotificationCallback(IntPtr pClient);
        [PreserveSig] int UnregisterEndpointNotificationCallback(IntPtr pClient);
    }

    [ComImport, Guid("D666063F-1587-4E43-81F1-B948E807363F"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IMMDevice
    {
        [PreserveSig] int Activate(ref Guid iid, uint dwClsCtx, IntPtr pActivationParams,
            [MarshalAs(UnmanagedType.IUnknown)] out object ppInterface);
        [PreserveSig] int OpenPropertyStore(uint stgmAccess, out IntPtr ppProperties);
        [PreserveSig] int GetId([MarshalAs(UnmanagedType.LPWStr)] out string? ppstrId);
        [PreserveSig] int GetState(out int pdwState);
    }

    [ComImport, Guid("77AA99A0-1BD6-484F-8BC7-2C654C9A9B6F"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioSessionManager2
    {
        [PreserveSig] int GetAudioSessionControl(IntPtr audioSessionGuid, int streamFlags,
            out IntPtr sessionControl);
        [PreserveSig] int GetSimpleAudioVolume(IntPtr audioSessionGuid, int streamFlags,
            out IntPtr audioVolume);
        [PreserveSig] int GetSessionEnumerator(out IAudioSessionEnumerator? sessionEnum);
        [PreserveSig] int RegisterSessionNotification(IntPtr sessionNotification);
        [PreserveSig] int UnregisterSessionNotification(IntPtr sessionNotification);
    }

    [ComImport, Guid("E2F5BB11-0570-40CA-ACDD-3AA01277DEE8"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioSessionEnumerator
    {
        [PreserveSig] int GetCount(out int sessionCount);
        [PreserveSig] int GetSession(int sessionIndex, out IAudioSessionControl? session);
    }

    [ComImport, Guid("F4B1A599-7266-4319-A8CA-E70ACB11E8CD"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioSessionControl
    {
        [PreserveSig] int GetState(out int pRetVal);
        [PreserveSig] int GetDisplayName([MarshalAs(UnmanagedType.LPWStr)] out string? pRetVal);
        [PreserveSig] int SetDisplayName([MarshalAs(UnmanagedType.LPWStr)] string value, IntPtr eventContext);
        [PreserveSig] int GetIconPath([MarshalAs(UnmanagedType.LPWStr)] out string? pRetVal);
        [PreserveSig] int SetIconPath([MarshalAs(UnmanagedType.LPWStr)] string value, IntPtr eventContext);
        [PreserveSig] int GetGroupingParam(out Guid pRetVal);
        [PreserveSig] int SetGroupingParam(ref Guid grouping, IntPtr eventContext);
        [PreserveSig] int RegisterAudioSessionNotification(IntPtr newNotifications);
        [PreserveSig] int UnregisterAudioSessionNotification(IntPtr newNotifications);
    }

    // IAudioSessionControl2 inherits IAudioSessionControl — must list
    // base methods first in vtable order, then the 5 new methods.
    [ComImport, Guid("BFB7FF88-7239-4FC9-8FA2-07C950BE9C6D"),
     InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    private interface IAudioSessionControl2
    {
        [PreserveSig] int GetState(out int pRetVal);
        [PreserveSig] int GetDisplayName([MarshalAs(UnmanagedType.LPWStr)] out string? pRetVal);
        [PreserveSig] int SetDisplayName([MarshalAs(UnmanagedType.LPWStr)] string value, IntPtr eventContext);
        [PreserveSig] int GetIconPath([MarshalAs(UnmanagedType.LPWStr)] out string? pRetVal);
        [PreserveSig] int SetIconPath([MarshalAs(UnmanagedType.LPWStr)] string value, IntPtr eventContext);
        [PreserveSig] int GetGroupingParam(out Guid pRetVal);
        [PreserveSig] int SetGroupingParam(ref Guid grouping, IntPtr eventContext);
        [PreserveSig] int RegisterAudioSessionNotification(IntPtr newNotifications);
        [PreserveSig] int UnregisterAudioSessionNotification(IntPtr newNotifications);
        [PreserveSig] int GetSessionIdentifier([MarshalAs(UnmanagedType.LPWStr)] out string? pRetVal);
        [PreserveSig] int GetSessionInstanceIdentifier([MarshalAs(UnmanagedType.LPWStr)] out string? pRetVal);
        [PreserveSig] int GetProcessId(out int pRetVal);
        [PreserveSig] int IsSystemSoundsSession();
        [PreserveSig] int SetDuckingPreference([MarshalAs(UnmanagedType.Bool)] bool optOut);
    }
}
