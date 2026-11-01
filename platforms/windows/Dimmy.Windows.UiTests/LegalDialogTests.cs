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

    [Theory]
    [InlineData("AboutLicenseViewButton", 30_000, "Version 3, 19 November 2007")]
    [InlineData("AboutNoticesViewButton", 900_000, "# Open source notices")]
    public void LegalDialogShowsTheWholeText(string buttonId, int minLength, string expected)
    {
        using var automation = new UIA3Automation();
        _app = Application.Launch(LocateDimmyExe());
        WaitFor("command pipe", () => SendCommand("open-settings:about"));

        var settings = WaitFor("Settings window", () =>
            automation.GetDesktop().FindFirstChild(cf => cf.ByName("Dimmy Settings")));
        WaitFor(buttonId, () => settings.FindFirstDescendant(cf => cf.ByAutomationId(buttonId)))
            .AsButton().Invoke();

        var clock = Stopwatch.StartNew();
        var box = WaitFor("legal text box", () =>
            settings.FindFirstDescendant(cf => cf.ByAutomationId("LegalTextBox")));
        var text = box.Patterns.Value.Pattern.Value.Value;
        _output.WriteLine($"{buttonId}: {text.Length} characters, read after {clock.ElapsedMilliseconds} ms");

        Assert.True(text.Length >= minLength, $"only {text.Length} characters: '{text}'");
        Assert.Contains(expected, text);
    }
}
