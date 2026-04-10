using System;
using System.Threading;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Velopack;

namespace Dimmy.Windows;

/// <summary>
/// Custom entry point for Velopack integration.
/// VelopackApp.Build().Run() must execute BEFORE WinUI Application starts.
/// </summary>
public static class Program
{
    [STAThread]
    public static void Main(string[] args)
    {
        // Velopack: handle install/uninstall/update hooks before anything else.
        // This returns immediately during normal app launch.
        VelopackApp.Build().Run();

        // Standard WinUI 3 startup
        WinRT.ComWrappersSupport.InitializeComWrappers();
        Application.Start(p =>
        {
            var context = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
            SynchronizationContext.SetSynchronizationContext(context);
            new App();
        });
    }
}
