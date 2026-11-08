using System;
using System.IO;
using System.IO.Pipes;
using System.Threading;
using System.Threading.Tasks;
using Dimmy.Windows.Services;
using Xunit;

namespace Dimmy.Windows.Tests.Services;

public class CommandPipeServerTests
{
    /// A pipe instance accepts a client the moment it is created, before the
    /// server reaches WaitForConnection. A client that writes and closes in
    /// that gap made WaitForConnection throw "The pipe is being closed" and
    /// the command was lost: the FlaUI Settings-open timeout, and a jump-list
    /// or dimmy:// command sent while Dimmy starts up.
    [Fact]
    public async Task ACommandSentBeforeTheServerWaitsIsStillDelivered()
    {
        var name = "DimmyCommandTest-" + Guid.NewGuid().ToString("N");
        using var server = new NamedPipeServerStream(
            name, PipeDirection.In, 1, PipeTransmissionMode.Byte, PipeOptions.Asynchronous,
            inBufferSize: 4096, outBufferSize: 0);

        var send = Task.Run(() => CommandPipeServer.TrySendCommand("open-settings:about", 1500, name));
        await Task.Delay(500);

        using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(5));
        await server.WaitForConnectionAsync(cts.Token);
        using var reader = new StreamReader(server);
        Assert.Equal("open-settings:about", await reader.ReadLineAsync(cts.Token));
        Assert.True(await send);
    }
}
