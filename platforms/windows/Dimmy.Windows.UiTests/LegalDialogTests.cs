using System;
using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Threading;
using FlaUI.Core;
using FlaUI.Core.AutomationElements;
using FlaUI.UIA3;
using Xunit;
using Xunit.Abstractions;

// Every UI test class kills the running Dimmy, so two of them must never overlap.
[assembly: CollectionBehavior(DisableTestParallelization = true)]

namespace Dimmy.Windows.UiTests;

/// <summary>
/// About → Legal. The dialog once showed only the first line of each text: a
/// TextBox given its text before AcceptsReturn is still single-line and keeps
/// what precedes the first line break, and nothing looked wrong in the code.
/// </summary>
public class LegalDialogTests : IDisposable
{
    private static readonly TimeSpan Timeout = TimeSpan.FromSeconds(30);

    private readonly ITestOutputHelper _output;
    private readonly bool _createdMarker;
    private Application? _app;

    public LegalDialogTests(ITestOutputHelper output)
    {
        _output = output;
        KillDimmy();
        // The command pipe only starts in normal mode, past onboarding.
        var marker = OnboardingMarker();
        if (!File.Exists(marker))
        {
            Directory.CreateDirectory(Path.GetDirectoryName(marker)!);
            File.WriteAllText(marker, "");
            _createdMarker = true;
        }
    }

    public void Dispose()
    {
        KillDimmy();
        if (_createdMarker) File.Delete(OnboardingMarker());
    }

    private static string OnboardingMarker() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData),
        "dimmy", ".onboarding_done");

    private static void KillDimmy()
    {
        foreach (var p in Process.GetProcessesByName("Dimmy.Windows"))
        {
            try { p.Kill(entireProcessTree: true); p.WaitForExit(3000); } catch { }
        }
    }

    private static string LocateDimmyExe()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir != null)
        {
            foreach (var bin in new[] { Path.Combine("bin", "x64", "Debug"), Path.Combine("bin", "Debug"), Path.Combine("bin", "Release") })
            {
                var candidate = Path.Combine(
                    dir.FullName, "platforms", "windows", "Dimmy.Windows", bin,
                    "net8.0-windows10.0.19041.0", "win-x64", "Dimmy.Windows.exe");
                if (File.Exists(candidate)) return candidate;
            }
            dir = dir.Parent;
        }
        throw new FileNotFoundException("Build Dimmy.Windows first.");
    }

    private static T WaitFor<T>(string what, Func<T?> find) where T : class
    {
        var deadline = DateTime.UtcNow + Timeout;
        while (DateTime.UtcNow < deadline)
        {
            try
            {
                var found = find();
                if (found != null) return found;
            }
            catch (Exception e) when (e is IOException or TimeoutException or System.Runtime.InteropServices.COMException)
            {
                // The app is still starting.
            }
            Thread.Sleep(250);
        }
        throw new TimeoutException($"{what} did not appear in {Timeout.TotalSeconds} s");
    }

    private static object? SendCommand(string command)
    {
        using var client = new NamedPipeClientStream(".", "DimmyCommand", PipeDirection.Out);
        client.Connect(1000);
        using var writer = new StreamWriter(client) { AutoFlush = true };
        writer.WriteLine(command);
        return command;
    }

    /// The licence and the notices open in a window of their own, laid out
    /// from the core's blocks (headings, aligned table columns, licence texts)
    /// instead of raw text in a dialog: it resizes, scrolls to the end, and
    /// shows the document's real structure.
    [Theory]
    [InlineData("AboutLicenseViewButton", "Licence (AGPL-3.0)", "GNU AFFERO GENERAL PUBLIC LICENSE", "Preamble")]
    [InlineData("AboutNoticesViewButton", "Open source notices", "Open source notices", "Crate")]
    public void LegalWindowShowsTheLaidOutText(string buttonId, string title, string heading, string structure)
    {
        using var automation = new UIA3Automation();
        _app = Application.Launch(LocateDimmyExe());
        WaitFor("command pipe", () => SendCommand("open-settings:about"));

        var settings = WaitFor("Settings window", () =>
            automation.GetDesktop().FindFirstChild(cf => cf.ByName("Dimmy Settings")));
        WaitFor(buttonId, () => settings.FindFirstDescendant(cf => cf.ByAutomationId(buttonId)))
            .AsButton().Invoke();

        var clock = Stopwatch.StartNew();
        var window = WaitFor($"{title} window", () =>
            automation.GetDesktop().FindFirstChild(cf => cf.ByName(title)));
        WaitFor($"heading '{heading}'", () => window.FindFirstDescendant(cf => cf.ByName(heading)));
        WaitFor($"'{structure}'", () => window.FindFirstDescendant(cf => cf.ByName(structure)));
        _output.WriteLine($"{title}: laid out after {clock.ElapsedMilliseconds} ms");

        Assert.True(window.Patterns.Transform.Pattern.CanResize.Value, "the window resizes");
        var scroll = WaitFor("scroll viewer", () =>
            window.FindFirstDescendant(cf => cf.ByControlType(FlaUI.Core.Definitions.ControlType.Pane)
                .And(cf.ByClassName("ScrollViewer"))));
        var pattern = scroll.Patterns.Scroll.Pattern;
        Assert.True(pattern.VerticallyScrollable.Value, "the text scrolls");
        // The content is virtualized: the scroll extent is estimated from
        // the rows built so far and corrects itself as the jump builds the
        // real ones, so reaching the end takes a few jumps (measured: 4 on
        // the notices), like dragging the thumb down twice.
        for (int i = 0; i < 10 && pattern.VerticalScrollPercent.Value < 99.9; i++)
        {
            pattern.SetScrollPercent(-1, 100);
            Thread.Sleep(500);
        }
        Assert.True(pattern.VerticalScrollPercent.Value >= 99.9, "it scrolls to the end");
    }
}
