using System;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Threading;
using FlaUI.Core;
using FlaUI.Core.AutomationElements;
using FlaUI.Core.Conditions;
using FlaUI.Core.Definitions;
using FlaUI.UIA3;
using Xunit;
using Xunit.Abstractions;

namespace Dimmy.Windows.UiTests;

/// <summary>
/// End-to-end UI smoke tests for the Dimmy onboarding wizard.
///
/// Each test kills any running Dimmy, resets onboarding state (deletes
/// the .onboarding_done marker), launches the built Dimmy.Windows.exe,
/// and drives the flow via FlaUI + UIA3. Assertions are on automation
/// tree state — AutomationIds added to the key XAML controls are the
/// contract these tests rely on.
///
/// These tests do NOT hit the network (no model download) and do NOT
/// complete onboarding (so they don't write .onboarding_done). The
/// build pipeline restores prior state in teardown as best-effort.
/// </summary>
public class OnboardingSmokeTests : IDisposable
{
    private readonly ITestOutputHelper _output;
    private Application? _app;

    private static readonly TimeSpan WindowTimeout = TimeSpan.FromSeconds(15);
    private static readonly TimeSpan ElementTimeout = TimeSpan.FromSeconds(10);

    public OnboardingSmokeTests(ITestOutputHelper output)
    {
        _output = output;
        KillDimmy();
        ResetOnboardingMarker();
    }

    public void Dispose()
    {
        try
        {
            _app?.Close();
            Thread.Sleep(400);
            if (_app != null && !_app.HasExited)
                _app.Kill();
        }
        catch { /* best-effort */ }
        KillDimmy();
    }

    // ── Helpers ───────────────────────────────────────────────────────

    private static void KillDimmy()
    {
        foreach (var p in Process.GetProcessesByName("Dimmy.Windows"))
        {
            try { p.Kill(entireProcessTree: true); p.WaitForExit(3000); } catch { }
        }
    }

    private static string OnboardingMarker()
    {
        var roamingAppData = Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData);
        return Path.Combine(roamingAppData, "dimmy", ".onboarding_done");
    }

    private static void ResetOnboardingMarker()
    {
        var marker = OnboardingMarker();
        if (File.Exists(marker))
        {
            try { File.Delete(marker); } catch { /* race — ignore */ }
        }
    }

    private static string LocateDimmyExe()
    {
        var here = AppContext.BaseDirectory;
        var dir = new DirectoryInfo(here);
        while (dir != null)
        {
            foreach (var cfg in new[] { "Debug", "Release" })
            {
                var candidate = Path.Combine(
                    dir.FullName,
                    "platforms", "windows", "Dimmy.Windows", "bin", cfg,
                    "net8.0-windows10.0.19041.0", "win-x64", "Dimmy.Windows.exe");
                if (File.Exists(candidate))
                    return candidate;
            }
            dir = dir.Parent;
        }
        throw new FileNotFoundException(
            "Could not find Dimmy.Windows.exe — build first with " +
            "'dotnet build platforms/windows/Dimmy.Windows/Dimmy.Windows.csproj -c Debug'.");
    }

    private Window LaunchAndGetMainWindow(UIA3Automation automation)
    {
        var exe = LocateDimmyExe();
        _output.WriteLine($"Launching: {exe}");
        _app = Application.Launch(exe);
        var window = _app.GetMainWindow(automation, WindowTimeout)
            ?? throw new InvalidOperationException("main window did not appear");
        _output.WriteLine($"Main window: title='{window.Title}' class={window.ClassName}");
        return window;
    }

    /// <summary>Find a descendant by AutomationId, waiting up to ElementTimeout.</summary>
    private static AutomationElement? FindByIdWithRetry(AutomationElement root, string automationId)
    {
        var deadline = DateTime.UtcNow + ElementTimeout;
        while (DateTime.UtcNow < deadline)
        {
            var found = root.FindFirstDescendant(cf => cf.ByAutomationId(automationId));
            if (found != null) return found;
            Thread.Sleep(200);
        }
        return null;
    }

    // ── Tests ─────────────────────────────────────────────────────────

    [Fact]
    public void Step0_WelcomeShowsGetStartedButton()
    {
        using var automation = new UIA3Automation();
        var window = LaunchAndGetMainWindow(automation);

        var getStarted = FindByIdWithRetry(window, "OnboardingGetStartedButton");

        Assert.NotNull(getStarted);
        Assert.True(getStarted!.IsEnabled, "Get Started button must be enabled on first launch");
    }

    [Fact]
    public void Step0_ToStep1_GetStartedRevealsLocalAndCloudCards()
    {
        using var automation = new UIA3Automation();
        var window = LaunchAndGetMainWindow(automation);

        var getStarted = FindByIdWithRetry(window, "OnboardingGetStartedButton");
        Assert.NotNull(getStarted);
        getStarted!.AsButton().Invoke();

        var local = FindByIdWithRetry(window, "OnboardingLocalCard");
        var cloud = FindByIdWithRetry(window, "OnboardingCloudCard");

        Assert.NotNull(local);
        Assert.NotNull(cloud);
    }

    [Fact]
    public void Step1_ExposesGroqKeyBoxForCloudPath()
    {
        // The PasswordBox is always present in the Step1 XAML tree (selection
        // state drives IsEnabled, not Visibility). We assert presence rather
        // than mouse-clicking the Border card — Border doesn't expose
        // InvokePattern, and a real mouse click requires visual rendering
        // that GHA headless runners don't reliably provide.
        using var automation = new UIA3Automation();
        var window = LaunchAndGetMainWindow(automation);

        FindByIdWithRetry(window, "OnboardingGetStartedButton")!.AsButton().Invoke();

        var keyBox = FindByIdWithRetry(window, "OnboardingGroqKeyBox");
        Assert.NotNull(keyBox);
    }

    [Fact]
    public void Step1_ContinueButtonIsDisabledUntilChoiceValid()
    {
        using var automation = new UIA3Automation();
        var window = LaunchAndGetMainWindow(automation);

        FindByIdWithRetry(window, "OnboardingGetStartedButton")!.AsButton().Invoke();

        var cont = FindByIdWithRetry(window, "OnboardingChoiceContinueButton");
        Assert.NotNull(cont);
        // Without any card selected, Continue should be disabled.
        Assert.False(
            cont!.IsEnabled,
            "Continue must be disabled before Local is ready or Cloud is validated");
    }

    [Fact]
    public void Step1_BackButtonReturnsToWelcome()
    {
        using var automation = new UIA3Automation();
        var window = LaunchAndGetMainWindow(automation);

        FindByIdWithRetry(window, "OnboardingGetStartedButton")!.AsButton().Invoke();

        var back = FindByIdWithRetry(window, "OnboardingChoiceBackButton")
            ?? throw new InvalidOperationException("back button not found");
        back.AsButton().Invoke();

        // After Back, Welcome's Get Started should be visible again (Step0).
        var getStarted = FindByIdWithRetry(window, "OnboardingGetStartedButton");
        Assert.NotNull(getStarted);
        Assert.True(getStarted!.IsEnabled);
    }
}
