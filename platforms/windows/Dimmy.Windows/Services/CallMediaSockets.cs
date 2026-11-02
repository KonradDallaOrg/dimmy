using System;
using System.Collections.Generic;
using System.Linq;
using System.Runtime.InteropServices;

namespace Dimmy.Windows.Services;

/// The call's UDP sockets, as evidence that the call is over.
///
/// Measured 2026-11-02 on a Teams call: 11 UDP sockets during the call, the
/// same 11 throughout an in-app microphone and speaker switch, and 2 within
/// a second of hanging up (the 2 Teams holds all day). So sockets CLOSING
/// say the call ended. Sockets staying open say nothing: a call relayed
/// over TCP 443 (a corporate proxy) has no media sockets at all, and
/// "still open" would then keep a finished call recording for ever.
public static class CallMediaSockets
{
    /// The (pid, local port) UDP endpoints owned by `pids`, IPv4 and IPv6.
    /// Empty when the table cannot be read.
    public static HashSet<(uint pid, ushort port)> Of(IReadOnlyCollection<uint> pids)
    {
        var found = new HashSet<(uint, ushort)>();
        if (pids.Count == 0) return found;
        Read(AF_INET, rowSize: 12, pidOffset: 8, portOffset: 4, pids, found);
        Read(AF_INET6, rowSize: 28, pidOffset: 24, portOffset: 20, pids, found);
        return found;
    }

    /// Did the call's media go away? True when at least half of the sockets
    /// held while the call was last seen talking have closed, and at least
    /// `MinClosed` of them: the sockets an app keeps open outside calls
    /// closing one by one is churn, not a hangup.
    public static bool Closed(
        IReadOnlyCollection<(uint pid, ushort port)> whileTalking,
        IReadOnlyCollection<(uint pid, ushort port)> now)
    {
        var closed = whileTalking.Count(s => !now.Contains(s));
        return closed >= MinClosed && closed * 2 >= whileTalking.Count;
    }

    public const int MinClosed = 3;

    private const int AF_INET = 2;
    private const int AF_INET6 = 23;
    private const int UDP_TABLE_OWNER_PID = 1;

    [DllImport("iphlpapi.dll", SetLastError = true)]
    private static extern uint GetExtendedUdpTable(
        IntPtr table, ref int size, bool order, int af, int tableClass, uint reserved);

    private static void Read(
        int af, int rowSize, int pidOffset, int portOffset,
        IReadOnlyCollection<uint> pids, HashSet<(uint, ushort)> into)
    {
        int size = 0;
        GetExtendedUdpTable(IntPtr.Zero, ref size, false, af, UDP_TABLE_OWNER_PID, 0);
        if (size <= 0) return;
        var buf = Marshal.AllocHGlobal(size);
        try
        {
            if (GetExtendedUdpTable(buf, ref size, false, af, UDP_TABLE_OWNER_PID, 0) != 0) return;
            int rows = Marshal.ReadInt32(buf);
            for (int i = 0; i < rows; i++)
            {
                var row = buf + 4 + i * rowSize;
                uint pid = (uint)Marshal.ReadInt32(row + pidOffset);
                if (!pids.Contains(pid)) continue;
                // Port in network byte order in the low 16 bits.
                int raw = Marshal.ReadInt32(row + portOffset);
                ushort port = (ushort)(((raw & 0xFF) << 8) | ((raw >> 8) & 0xFF));
                into.Add((pid, port));
            }
        }
        finally
        {
            Marshal.FreeHGlobal(buf);
        }
    }
}
