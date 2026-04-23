using System;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Threading;
using FlaUI.Core;
using FlaUI.Core.AutomationElements;
using FlaUI.UIA3;
using Xunit;
using Xunit.Abstractions;

namespace Dimmy.Windows.UiTests;

/// <summary>
/// Spike: verify FlaUI + UIA3 can attach to the built Dimmy.Windows.exe
/// and enumerate its visible windows. This is the GO/NO-GO probe before
/// writing real automation tests.
///
/// Not a regression test — a smoke check. If this fails, UIA3 doesn't see
/// WinUI 3 controls on this machine and we need a different approach.
/// </summary>
public class SpikeTests
{
    private readonly ITestOutputHelper _output;

    public SpikeTests(ITestOutputHelper output) => _output = output;

    private static string LocateDimmyExe()
    {
        // Search from the solution root outward. Tests run from
        // bin/Debug/net8.0-windows/Dimmy.Windows.UiTests.dll so we walk up.
        var here = AppContext.BaseDirectory;
        var dir = new DirectoryInfo(here);
        while (dir != null)
        {
            var candidate = Path.Combine(
                dir.FullName,
                "platforms", "windows", "Dimmy.Windows", "bin", "Debug",
                "net8.0-windows10.0.19041.0", "win-x64", "Dimmy.Windows.exe");
            if (File.Exists(candidate))
                return candidate;

            // also try Release config
            var release = candidate.Replace($"{Path.DirectorySeparatorChar}Debug{Path.DirectorySeparatorChar}",
                                             $"{Path.DirectorySeparatorChar}Release{Path.DirectorySeparatorChar}");
            if (File.Exists(release))
                return release;

            dir = dir.Parent;
        }
        throw new FileNotFoundException(
            "Could not find Dimmy.Windows.exe — build it first via " +
            "'dotnet build platforms/windows/Dimmy.Windows/Dimmy.Windows.csproj -c Debug'.");
    }

    [Fact]
    public void FlaUI_attaches_to_dimmy_and_enumerates_at_least_one_window()
    {
        var exePath = LocateDimmyExe();
        _output.WriteLine($"Launching: {exePath}");

        Application? app = null;
        try
        {
            app = Application.Launch(exePath);

            // WinUI 3 apps take a moment to show the first window
            using var automation = new UIA3Automation();
            var window = app.GetMainWindow(automation, TimeSpan.FromSeconds(15));
            Assert.NotNull(window);

            _output.WriteLine($"Main window title: {window.Title}");
            _output.WriteLine($"Main window ClassName: {window.ClassName}");
            _output.WriteLine($"Main window FrameworkType: {window.FrameworkType}");
            _output.WriteLine($"Main window ControlType: {window.ControlType}");

            // Dump the first two levels of the automation tree
            // AutomationId can throw if the provider doesn't expose it — use safe accessor
            static string SafeAutomationId(AutomationElement e)
            {
                try { return e.AutomationId ?? "(null)"; }
                catch { return "(unsupported)"; }
            }
            static string SafeName(AutomationElement e)
            {
                try { return e.Name ?? "(null)"; }
                catch { return "(unsupported)"; }
            }

            int count = 0;
            foreach (var child in window.FindAllChildren())
            {
                _output.WriteLine($"  child: [{child.ControlType}] '{SafeName(child)}' AutomationId='{SafeAutomationId(child)}'");
                count++;
                foreach (var grandchild in child.FindAllChildren())
                {
                    _output.WriteLine($"    - [{grandchild.ControlType}] '{SafeName(grandchild)}' AutomationId='{SafeAutomationId(grandchild)}'");
                }
                if (count > 30) break; // cap output
            }

            Assert.True(count >= 0, "expected to enumerate children without exception");
        }
        finally
        {
            try
            {
                app?.Close();
                Thread.Sleep(500);
                if (app != null && !app.HasExited)
                    app.Kill();
            }
            catch { /* best-effort */ }
        }
    }
}
