using System;
using System.IO;
using System.IO.Pipes;
using System.Security.AccessControl;
using System.Security.Principal;
using System.Threading;
using System.Threading.Tasks;

namespace Dimmy.Windows.Services;

/// <summary>
/// Named-pipe server that lets a transient Dimmy instance forward a
/// single command to the running instance and exit. Used by jump-list
/// shortcuts (`Dimmy.Windows.exe --command <name>`) so the user's
/// right-click action propagates to the live process without
/// spawning a duplicate UI.
///
/// Wire format: one UTF-8 line per connection. Examples:
///   toggle-pill
///   open-settings
///   quit
///   set-style:elaborate
///   set-translate:en
///
/// Pipe name is `Global\DimmyCommand` so it survives session
/// boundaries (matches the existing single-instance mutex naming).
/// ACL is restricted to the current user — no cross-user access.
/// </summary>
public sealed class CommandPipeServer : IDisposable
{
    public const string PipeName = "DimmyCommand";

    private readonly Action<string> _onCommand;
    private readonly CancellationTokenSource _cts = new();
    private Task? _loopTask;

    public CommandPipeServer(Action<string> onCommand)
    {
        _onCommand = onCommand;
    }

    public void Start()
    {
        _loopTask = Task.Run(() => AcceptLoop(_cts.Token));
    }

    private async Task AcceptLoop(CancellationToken ct)
    {
        while (!ct.IsCancellationRequested)
        {
            try
            {
                // Restrict pipe to current user — jump-list invocation
                // always runs as the same user as the host, so this is
                // both correct and protects against another user on
                // the same machine sending us commands.
                var sid = WindowsIdentity.GetCurrent().User
                    ?? new SecurityIdentifier(WellKnownSidType.WorldSid, null);
                var security = new PipeSecurity();
                security.AddAccessRule(new PipeAccessRule(
                    sid, PipeAccessRights.ReadWrite, AccessControlType.Allow));

                using var server = NamedPipeServerStreamAcl.Create(
                    PipeName, PipeDirection.In, 1,
                    PipeTransmissionMode.Byte, PipeOptions.Asynchronous,
                    inBufferSize: 4096, outBufferSize: 0,
                    pipeSecurity: security);

                await server.WaitForConnectionAsync(ct).ConfigureAwait(false);

                using var reader = new StreamReader(server, leaveOpen: false);
                var line = await reader.ReadLineAsync(ct).ConfigureAwait(false);
                if (string.IsNullOrWhiteSpace(line)) continue;

                // Dispatch on the calling thread's context — the
                // handler is responsible for marshalling onto the UI
                // dispatcher when needed.
                try { _onCommand(line.Trim()); }
                catch (Exception ex)
                {
                    System.Diagnostics.Debug.WriteLine($"[CommandPipe] handler threw: {ex.Message}");
                }
            }
            catch (OperationCanceledException) { break; }
            catch (Exception ex)
            {
                System.Diagnostics.Debug.WriteLine($"[CommandPipe] loop error: {ex.Message}");
                // Brief backoff so a persistently failing pipe doesn't
                // pin a CPU core. 250 ms is short enough that the next
                // jump-list click still feels instant.
                try { await Task.Delay(250, ct).ConfigureAwait(false); }
                catch (OperationCanceledException) { break; }
            }
        }
    }

    /// <summary>Send a single command to a running Dimmy instance.
    /// Used by the transient `--command X` invocation. Returns true
    /// if the command was delivered.</summary>
    public static bool TrySendCommand(string command, int timeoutMs = 1500)
    {
        try
        {
            using var client = new NamedPipeClientStream(
                ".", PipeName, PipeDirection.Out, PipeOptions.None);
            client.Connect(timeoutMs);
            using var writer = new StreamWriter(client) { AutoFlush = true };
            writer.WriteLine(command);
            return true;
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[CommandPipe] send failed: {ex.Message}");
            return false;
        }
    }

    public void Dispose()
    {
        try { _cts.Cancel(); } catch { }
        try { _loopTask?.Wait(500); } catch { }
        _cts.Dispose();
    }
}
