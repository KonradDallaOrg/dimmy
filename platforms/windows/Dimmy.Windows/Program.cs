using System;
using System.IO;
using System.Threading;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Velopack;

namespace Dimmy.Windows;

/// <summary>
/// Custom entry point for Velopack integration.
/// VelopackApp.Build().Run() must execute BEFORE WinUI Application starts.
/// Wraps the entire launch sequence in try/catch that logs to %TEMP%\dimmy_startup.log
/// so silent crashes on clean machines leave a diagnosable trail.
/// </summary>
public static class Program
{
    private static readonly string StartupLogPath = Path.Combine(
        Path.GetTempPath(), "dimmy_startup.log");

    [STAThread]
    public static void Main(string[] args)
    {
        AppDomain.CurrentDomain.UnhandledException += (_, e) =>
            LogCrash("AppDomain.UnhandledException", e.ExceptionObject as Exception);

        try
        {
            LogStage("Program.Main start");

            VelopackApp.Build().Run();
            LogStage("VelopackApp.Run returned");

            WinRT.ComWrappersSupport.InitializeComWrappers();
            LogStage("ComWrappers initialized");

            Application.Start(p =>
            {
                try
                {
                    var context = new DispatcherQueueSynchronizationContext(DispatcherQueue.GetForCurrentThread());
                    SynchronizationContext.SetSynchronizationContext(context);
                    LogStage("Creating App instance");
                    new App();
                    LogStage("App instance created");
                }
                catch (Exception ex)
                {
                    LogCrash("Application.Start callback", ex);
                    throw;
                }
            });

            LogStage("Application.Start returned normally");
        }
        catch (Exception ex)
        {
            LogCrash("Program.Main outer", ex);
            throw;
        }
    }

    private static void LogStage(string stage)
    {
        try
        {
            File.AppendAllText(StartupLogPath,
                $"[{DateTime.Now:yyyy-MM-dd HH:mm:ss.fff}] {stage}{Environment.NewLine}");
        }
        catch { }
    }

    private static void LogCrash(string stage, Exception? ex)
    {
        try
        {
            var msg = $"[{DateTime.Now:yyyy-MM-dd HH:mm:ss.fff}] CRASH at {stage}: " +
                      $"{ex?.GetType().FullName}: {ex?.Message}{Environment.NewLine}" +
                      $"{ex?.StackTrace}{Environment.NewLine}" +
                      $"---{Environment.NewLine}";
            File.AppendAllText(StartupLogPath, msg);
        }
        catch { }
    }
}
