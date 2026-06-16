using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;
using System.Threading;
using FlaUI.Core;
using FlaUI.Core.AutomationElements;
using FlaUI.Core.Definitions;
using FlaUI.Core.Input;
using FlaUI.Core.WindowsAPI;
using FlaUI.UIA3;
using Xunit;
using Xunit.Abstractions;

namespace Dimmy.Windows.UiTests;

/// <summary>
/// Marketing / docs screenshot harness. NOT a test — it drives the real app
/// and writes PNGs for the website (dimmy-website/assets/help/...). Gated by
/// env var DIMMY_CAPTURE=1 so a normal `dotnet test` run skips it (it pops
/// windows, kills the user's running app, and needs a real desktop session).
///
/// Run:
///   $env:DIMMY_CAPTURE=1
///   dotnet test platforms/windows/Dimmy.Windows.UiTests --filter FullyQualifiedName~MarketingCapture
///
/// Output: docs/ui-shots/win/help/ (gitignored). Filenames mirror the website
/// screenshot manifest (slug/name.png) where a clean 1:1 capture exists; a
/// manifest-map.json records every produced file + which manifest src it
/// satisfies + a status (ready | needs-interaction).
///
/// What it captures:
///   - Onboarding: every step (welcome, local/cloud, shortcut, try-it).
///   - Settings: every tab, with Advanced mode ON, window resized tall so the
///     full page fits in one PrintWindow shot, in light AND dark theme.
///
/// Capture uses PrintWindow(PW_RENDERFULLCONTENT) by HWND, so it works even
/// when the window is occluded / larger than the screen (WinUI/DWM composed).
/// Interaction-heavy manifest slots (open dropdowns, expanded rows, modal
/// wizards, pill states) are flagged needs-interaction in the map for a
/// follow-up pass — they are not produced here.
/// </summary>
public class MarketingCapture
{
    private readonly ITestOutputHelper _output;
    public MarketingCapture(ITestOutputHelper output) => _output = output;

    // ── Win32 ────────────────────────────────────────────────────────────
    private const uint PW_RENDERFULLCONTENT = 2;
    private const uint SWP_NOMOVE = 0x0002, SWP_NOZORDER = 0x0004, SWP_NOACTIVATE = 0x0010;

    [DllImport("user32.dll")] private static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
    [DllImport("user32.dll")] private static extern bool GetWindowRect(IntPtr h, out RECT r);
    [DllImport("user32.dll")] private static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr h);
    [DllImport("user32.dll")] private static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
    [DllImport("user32.dll")] private static extern int GetWindowLong(IntPtr h, int idx);
    private delegate bool EnumProc(IntPtr h, IntPtr l);
    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumProc cb, IntPtr l);
    private const int GWL_STYLE = -16;
    private const long WS_CAPTION = 0x00C00000;

    [StructLayout(LayoutKind.Sequential)]
    private struct RECT { public int Left, Top, Right, Bottom; }

    // ── Paths ────────────────────────────────────────────────────────────
    private static string RepoRoot()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir != null)
        {
            if (Directory.Exists(Path.Combine(dir.FullName, ".git"))) return dir.FullName;
            dir = dir.Parent;
        }
        throw new DirectoryNotFoundException("repo root (.git) not found");
    }

    private static string ExePath(string root) => Path.Combine(
        root, "platforms", "windows", "Dimmy.Windows", "bin", "x64", "Debug",
        "net8.0-windows10.0.19041.0", "win-x64", "Dimmy.Windows.exe");

    private static string PrefsPath() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "dimmy", "ui_prefs.json");

    private static string OnboardingMarker() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "dimmy", ".onboarding_done");

    // ── Helpers ──────────────────────────────────────────────────────────
    private void Log(string m) => _output.WriteLine(m);

    private static void KillDimmy()
    {
        foreach (var p in Process.GetProcessesByName("Dimmy.Windows"))
        {
            try { p.Kill(true); p.WaitForExit(3000); } catch { }
        }
        Thread.Sleep(500);
    }

    private static string _exe = "";

    private static void SendCommand(string cmd)
    {
        Process.Start(new ProcessStartInfo(_exe, $"--command {cmd}") { UseShellExecute = false });
    }

    private static void SetTheme(string mode)
    {
        var path = PrefsPath();
        Dictionary<string, JsonElement> obj = new();
        if (File.Exists(path))
        {
            try
            {
                var doc = JsonDocument.Parse(File.ReadAllText(path));
                foreach (var p in doc.RootElement.EnumerateObject()) obj[p.Name] = p.Value.Clone();
            }
            catch { }
        }
        // Re-serialize with Theme overridden. Store a string value.
        using var ms = new MemoryStream();
        using (var w = new Utf8JsonWriter(ms))
        {
            w.WriteStartObject();
            bool wroteTheme = false;
            foreach (var kv in obj)
            {
                if (kv.Key == "Theme") { w.WriteString("Theme", mode); wroteTheme = true; }
                else { w.WritePropertyName(kv.Key); kv.Value.WriteTo(w); }
            }
            if (!wroteTheme) w.WriteString("Theme", mode);
            w.WriteEndObject();
        }
        File.WriteAllBytes(path, ms.ToArray());  // no BOM
    }

    private static string ReadTheme()
    {
        try
        {
            var doc = JsonDocument.Parse(File.ReadAllText(PrefsPath()));
            if (doc.RootElement.TryGetProperty("Theme", out var t)) return t.GetString() ?? "Default";
        }
        catch { }
        return "Default";
    }

    private static IntPtr FindWindow(string titlePattern, bool wantTitled)
    {
        var pids = Process.GetProcessesByName("Dimmy.Windows").Select(p => p.Id).ToHashSet();
        if (pids.Count == 0) return IntPtr.Zero;
        IntPtr best = IntPtr.Zero; long bestArea = 0;
        EnumWindows((h, l) =>
        {
            if (!IsWindowVisible(h)) return true;
            GetWindowThreadProcessId(h, out var pid);
            if (!pids.Contains((int)pid)) return true;
            var sb = new StringBuilder(256); GetWindowText(h, sb, 256);
            var title = sb.ToString();
            GetWindowRect(h, out var r);
            long area = (long)(r.Right - r.Left) * (r.Bottom - r.Top);
            if (wantTitled)
            {
                if (string.IsNullOrWhiteSpace(title)) return true;
                if (titlePattern.Length > 0 && !title.Contains(titlePattern, StringComparison.OrdinalIgnoreCase)) return true;
                if (area > bestArea) { bestArea = area; best = h; }
            }
            return true;
        }, IntPtr.Zero);
        return best;
    }

    private bool CaptureHwnd(IntPtr h, string outFile)
    {
        if (h == IntPtr.Zero) { Log($"  no window for {Path.GetFileName(outFile)}"); return false; }
        GetWindowRect(h, out var r);
        int w = r.Right - r.Left, ht = r.Bottom - r.Top;
        if (w <= 0 || ht <= 0) return false;
        using var bmp = new Bitmap(w, ht);
        using var g = Graphics.FromImage(bmp);
        var hdc = g.GetHdc();
        bool ok = PrintWindow(h, hdc, PW_RENDERFULLCONTENT);
        g.ReleaseHdc(hdc);
        if (!ok) { Log($"  PrintWindow failed for {Path.GetFileName(outFile)}"); return false; }
        Directory.CreateDirectory(Path.GetDirectoryName(outFile)!);
        bmp.Save(outFile, ImageFormat.Png);
        return true;
    }

    private void ResizeTall(IntPtr h, int width = 1180, int height = 2400)
    {
        SetWindowPos(h, IntPtr.Zero, 0, 0, width, height, SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE);
        Thread.Sleep(500);
    }

    // ── The capture run ──────────────────────────────────────────────────
    private readonly List<object> _map = new();

    [Fact]
    public void Capture()
    {
        if (Environment.GetEnvironmentVariable("DIMMY_CAPTURE") != "1")
        {
            _output.WriteLine("DIMMY_CAPTURE!=1 — skipping marketing capture (set env var to run).");
            return;
        }

        var root = RepoRoot();
        _exe = ExePath(root);
        Assert.True(File.Exists(_exe), $"Build the x64 Debug host first: {_exe}");
        var outDir = Path.Combine(root, "docs", "ui-shots", "win", "help");
        Directory.CreateDirectory(outDir);

        var originalTheme = ReadTheme();
        try
        {
            CaptureOnboarding(outDir);
            foreach (var theme in new[] { "Light", "Dark" })
            {
                CaptureSettings(outDir, theme);
                CaptureMeeting(outDir, theme);
            }
            CaptureModels(outDir);
            CaptureWizards(outDir);
            CapturePillStates(outDir);
        }
        finally
        {
            KillDimmy();
            SetTheme(originalTheme);
        }

        File.WriteAllText(Path.Combine(outDir, "manifest-map.json"),
            JsonSerializer.Serialize(_map, new JsonSerializerOptions { WriteIndented = true }));
        Log($"Done. {_map.Count} shots. Map: {Path.Combine(outDir, "manifest-map.json")}");
    }

    private void Record(string file, string slug, string name, string manifestSrc, string status, string theme)
    {
        _map.Add(new { file, slug, name, manifest_src = manifestSrc, status, theme, os = "windows" });
    }

    // Onboarding: walk every step, capturing the window each time.
    private void CaptureOnboarding(string outDir)
    {
        Log("=== Onboarding ===");
        KillDimmy();
        SetTheme("Light");
        var marker = OnboardingMarker();
        try { if (File.Exists(marker)) File.Delete(marker); } catch { }

        using var automation = new UIA3Automation();
        var app = Application.Launch(_exe);
        var window = app.GetMainWindow(automation, TimeSpan.FromSeconds(20));
        Thread.Sleep(2500);

        // step labels -> manifest src under first-dictation/
        var obDir = Path.Combine(outDir, "first-dictation");

        // Step 0: Welcome
        var hwnd = window.Properties.NativeWindowHandle.Value;
        if (CaptureHwnd(hwnd, Path.Combine(obDir, "win-welcome.png")))
            Record("first-dictation/win-welcome.png", "first-dictation", "win-welcome", "/assets/help/first-dictation/win-welcome.png", "ready", "light");

        // -> Step 1: Local / Cloud
        var getStarted = FindById(window, "OnboardingGetStartedButton");
        getStarted?.AsButton().Invoke();
        Thread.Sleep(1500);
        if (CaptureHwnd(hwnd, Path.Combine(obDir, "win-model.png")))
            Record("first-dictation/win-model.png", "first-dictation", "win-model", "/assets/help/first-dictation/win-model.png", "ready", "light");

        // Select Local card so we can advance, then -> Step 2: Shortcut
        try { FindById(window, "OnboardingLocalCard")?.Click(); Thread.Sleep(600); } catch { }
        var choiceContinue = FindById(window, "OnboardingChoiceContinueButton");
        choiceContinue?.AsButton().Invoke();
        Thread.Sleep(1500);
        if (CaptureHwnd(hwnd, Path.Combine(obDir, "win-shortcut.png")))
            Record("first-dictation/win-shortcut.png", "first-dictation", "win-shortcut", "/assets/help/first-dictation/win-shortcut.png", "ready", "light");

        // -> Step 3: Try It (Step2 "Next" button has no AutomationId; find by name)
        var next = window.FindFirstDescendant(cf => cf.ByName("Next").And(cf.ByControlType(ControlType.Button)));
        next?.AsButton().Invoke();
        Thread.Sleep(1500);
        if (CaptureHwnd(hwnd, Path.Combine(obDir, "win-tryit.png")))
            Record("first-dictation/win-tryit.png", "first-dictation", "win-tryit", "/assets/help/first-dictation/win-tryit.png", "ready", "light");

        KillDimmy();
        // Leave the marker deleted? Re-create so we don't force the user through
        // onboarding again on their next normal launch.
        try { File.WriteAllText(marker, ""); } catch { }
    }

    private static AutomationElement? FindById(AutomationElement root, string id)
    {
        var deadline = DateTime.UtcNow + TimeSpan.FromSeconds(8);
        while (DateTime.UtcNow < deadline)
        {
            var f = root.FindFirstDescendant(cf => cf.ByAutomationId(id));
            if (f != null) return f;
            Thread.Sleep(200);
        }
        return null;
    }

    // (tag, manifestSlug/name, manifestSrc, status). status=ready => clean 1:1;
    // needs-interaction => the tab is captured but the manifest slot really wants
    // an opened dropdown / expanded row / modal wizard we don't drive yet.
    // interaction: ""=none, "dropdown"=expand first ComboBox, "row"=select first
    // ListView item (expands a history row).
    private static readonly (string tag, string slug, string name, string src, string status, string interaction)[] SettingsMap =
    {
        ("home",         "stats",                     "dashboard",  "/assets/help/stats/dashboard.png",                       "approx", ""),
        ("voice",        "audio-input",               "device-vu",  "/assets/help/audio-input/device-vu.png",                 "ready",  "dropdown"),
        ("output",       "styles",                    "output-grid","/assets/help/styles/output-grid.png",                    "ready",  ""),
        ("providers",    "api-keys",                  "providers",  "/assets/help/api-keys/providers.png",                    "ready",  ""),
        ("pill",         "pill",                      "opacity-slider","/assets/help/pill/opacity-slider.png",                "approx", ""),
        ("rules",        "extras",                    "app-rules",  "",                                                       "extra",  ""),
        ("shortcut",     "hotkey-change",             "shortcut-tab","/assets/help/hotkey-change/shortcut-tab.png",           "ready",  ""),
        ("history",      "history-search",            "list",       "/assets/help/history-search/list.png",                   "ready",  "row"),
        ("integrations", "integrations-claude-cli",   "card",       "/assets/help/integrations-claude-cli/card.png",          "ready",  ""),
        ("privacy",      "telemetry",                 "toggles",    "/assets/help/telemetry/toggles.png",                     "ready",  ""),
        ("license",      "extras",                    "license",    "",                                                       "extra",  ""),
        ("about",        "about-and-updates",         "about-tab",  "/assets/help/about-and-updates/about-tab.png",           "ready",  ""),
    };

    // Manifest slots served by COPYING an already-captured page (same screen,
    // different article). target manifest src <- source slug/name.
    private static readonly (string slug, string name, string srcSlug, string srcName, string manifestSrc)[] SettingsCopies =
    {
        ("tones",                  "selector",     "styles",                   "output-grid", "/assets/help/tones/selector.png"),
        ("filler-removal",         "toggle",       "styles",                   "output-grid", "/assets/help/filler-removal/toggle.png"),
        ("integrations-codex-cli", "card",         "integrations-claude-cli",  "card",        "/assets/help/integrations-codex-cli/card.png"),
        ("settings-overview",      "sidebar",      "api-keys",                 "providers",   "/assets/help/settings-overview/sidebar.png"),
        ("transcription-issues",   "stats",        "stats",                    "dashboard",   "/assets/help/transcription-issues/stats.png"),
    };

    private void CaptureSettings(string outDir, string theme)
    {
        Log($"=== Settings ({theme}) ===");
        KillDimmy();
        SetTheme(theme);
        using var automation = new UIA3Automation();
        var app = Application.Launch(_exe);
        app.GetMainWindow(automation, TimeSpan.FromSeconds(20));
        Thread.Sleep(4000);

        SendCommand("open-settings");
        Thread.Sleep(2500);

        // Turn Advanced mode ON via the tagged toggle.
        try
        {
            var settingsHwnd0 = FindWindow("Settings", true);
            var settingsWin = automation.FromHandle(settingsHwnd0).AsWindow();
            var toggle = FindById(settingsWin, "SettingsAdvancedToggle")?.AsToggleButton();
            if (toggle != null && toggle.ToggleState != FlaUI.Core.Definitions.ToggleState.On)
            {
                toggle.Toggle();
                Thread.Sleep(800);
            }
        }
        catch (Exception ex) { Log($"  advanced toggle failed: {ex.Message}"); }

        var tl = theme.ToLowerInvariant();
        foreach (var (tag, slug, name, src, status, interaction) in SettingsMap)
        {
            SendCommand($"open-settings:{tag}");
            Thread.Sleep(1100);
            var hwnd = FindWindow("Settings", true);
            ResizeTall(hwnd);

            // Optional interaction to reach the state the manifest slot wants.
            if (interaction.Length > 0)
            {
                try
                {
                    var win = automation.FromHandle(hwnd).AsWindow();
                    if (interaction == "dropdown")
                    {
                        var combo = win.FindFirstDescendant(cf => cf.ByControlType(ControlType.ComboBox))?.AsComboBox();
                        combo?.Expand();
                        Thread.Sleep(700);
                    }
                    else if (interaction == "row")
                    {
                        var list = win.FindFirstDescendant(cf => cf.ByAutomationId("DictListView"))?.AsListBox()
                                   ?? win.FindFirstDescendant(cf => cf.ByControlType(ControlType.List))?.AsListBox();
                        var item = list?.Items.FirstOrDefault();
                        item?.Select();
                        Thread.Sleep(700);
                    }
                }
                catch (Exception ex) { Log($"  interaction '{interaction}' on {tag} failed: {ex.Message}"); }
            }

            var rel = Path.Combine(slug, $"{name}-{tl}.png");
            if (CaptureHwnd(hwnd, Path.Combine(outDir, rel)))
            {
                Log($"  captured {rel}");
                if (status != "extra")
                    Record(rel.Replace('\\', '/'), slug, name, src, status, tl);
            }
        }

        // Copy already-captured pages into the manifest slots they also satisfy.
        foreach (var (slug, name, srcSlug, srcName, manifestSrc) in SettingsCopies)
        {
            var from = Path.Combine(outDir, srcSlug, $"{srcName}-{tl}.png");
            var to = Path.Combine(outDir, slug, $"{name}-{tl}.png");
            if (File.Exists(from))
            {
                Directory.CreateDirectory(Path.GetDirectoryName(to)!);
                File.Copy(from, to, overwrite: true);
                Record($"{slug}/{name}-{tl}.png", slug, name, manifestSrc, "copy", tl);
                Log($"  copied {slug}/{name}-{tl}.png  <- {srcSlug}/{srcName}");
            }
        }
    }

    // Meeting done-view: open the window, select the first past meeting in the
    // sidebar, and capture the recap + transcript + waveform.
    private void CaptureMeeting(string outDir, string theme)
    {
        Log($"=== Meeting ({theme}) ===");
        SendCommand("open-meeting");
        Thread.Sleep(2500);
        var tl = theme.ToLowerInvariant();
        var hwnd = FindWindow("Meeting", true);
        if (hwnd == IntPtr.Zero) { Log("  meeting window not found"); return; }

        try
        {
            using var automation = new UIA3Automation();
            var win = automation.FromHandle(hwnd).AsWindow();
            var list = win.FindFirstDescendant(cf => cf.ByAutomationId("HistoryList"))?.AsListBox();
            var first = list?.Items.FirstOrDefault();
            first?.Select();
            Thread.Sleep(2000);  // let recap + transcript + waveform render
        }
        catch (Exception ex) { Log($"  meeting select failed: {ex.Message}"); }

        var rel = Path.Combine("meeting", $"done-view-{tl}.png");
        if (CaptureHwnd(hwnd, Path.Combine(outDir, rel)))
        {
            Log($"  captured {rel}");
            Record(rel.Replace('\\', '/'), "meeting", "done-view", "", "extra-meeting", tl);
        }
    }

    // whisper-models/list: Providers page, On-device card expanded to reveal the
    // local model list + download buttons.
    private void CaptureModels(string outDir)
    {
        Log("=== Models (On-device card) ===");
        foreach (var theme in new[] { "Light", "Dark" })
        {
            KillDimmy();
            SetTheme(theme);
            using var automation = new UIA3Automation();
            var app = Application.Launch(_exe);
            app.GetMainWindow(automation, TimeSpan.FromSeconds(20));
            Thread.Sleep(4000);
            SendCommand("open-settings:providers");
            // Wait for the Settings window AND let the async provider-card build
            // (PopulateLocalModels) settle — enumerating mid-rebuild throws
            // ElementNotAvailableException.
            IntPtr hwnd = IntPtr.Zero;
            for (int i = 0; i < 20 && hwnd == IntPtr.Zero; i++) { Thread.Sleep(400); hwnd = FindWindow("Settings", true); }
            Thread.Sleep(3000);

            // Best-effort expand the On-device card at NORMAL size (clickable
            // point on-screen). Retry across UIA tree churn.
            for (int attempt = 0; attempt < 3; attempt++)
            {
                try
                {
                    var win = automation.FromHandle(hwnd).AsWindow();
                    var host = win.FindFirstDescendant(cf => cf.ByAutomationId("ProviderCardsHost"));
                    var scope = (AutomationElement?)host ?? win;
                    AutomationElement? card = null;
                    foreach (var e in scope.FindAllDescendants())
                    {
                        string nm; try { nm = e.Name ?? ""; } catch { continue; }
                        if (nm.Contains("On-device", StringComparison.OrdinalIgnoreCase) || nm.Contains("On device", StringComparison.OrdinalIgnoreCase)) { card = e; break; }
                    }
                    if (card == null) { Log("  on-device card not located"); break; }
                    try { card.AsButton().Invoke(); } catch { card.Click(); }
                    Log("  on-device card clicked");
                    Thread.Sleep(1300);
                    break;
                }
                catch (Exception ex) { Log($"  on-device attempt {attempt} {ex.GetType().Name}; retry"); Thread.Sleep(1200); }
            }
            ResizeTall(hwnd, 1180, 2600);
            Thread.Sleep(400);
            var tl = theme.ToLowerInvariant();
            var rel = Path.Combine("whisper-models", $"list-{tl}.png");
            if (CaptureHwnd(hwnd, Path.Combine(outDir, rel)))
            {
                Log($"  captured {rel}");
                Record(rel.Replace('\\', '/'), "whisper-models", "list", "/assets/help/whisper-models/list.png", "ready", tl);
            }
        }
    }

    // integrations-{notion,claude-desktop}/wizard: open the modal Connect wizard
    // and capture it. Only works when the integration is NOT already connected
    // (the Connect button is hidden otherwise) — logged + skipped if missing.
    private void CaptureWizards(string outDir)
    {
        Log("=== Wizards ===");
        KillDimmy();
        SetTheme("Light");
        using var automation = new UIA3Automation();
        var app = Application.Launch(_exe);
        app.GetMainWindow(automation, TimeSpan.FromSeconds(20));
        Thread.Sleep(4000);
        SendCommand("open-settings:integrations");
        IntPtr swin = IntPtr.Zero;
        for (int i = 0; i < 20 && swin == IntPtr.Zero; i++) { Thread.Sleep(400); swin = FindWindow("Settings", true); }
        Thread.Sleep(1500);

        // Each integration may be disconnected (Connect button) OR connected
        // (re-open / change-destination button) — try both so the wizard is
        // reachable regardless of the machine's connection state.
        // candidates are tried as AutomationId first, then Name — so the two
        // colliding "Set up wizard" buttons (Anthropic vs Codex) are disambiguated
        // by their x:Name (= AutomationId in WinUI).
        TryWizard(automation, new[] { "Connect Notion", "Change destination" }, outDir, "integrations-notion", "wizard", "/assets/help/integrations-notion/wizard.png");
        TryWizard(automation, new[] { "Connect Claude Desktop", "Re-run wizard" }, outDir, "integrations-claude-desktop", "wizard", "/assets/help/integrations-claude-desktop/wizard.png");
        // Claude Code subscription: AnthropicIntegrationWizardBtn (disconnected) or "Re-run setup" (connected).
        TryWizard(automation, new[] { "AnthropicIntegrationWizardBtn", "Re-run setup" }, outDir, "integrations-claude-cli", "wizard", "/assets/help/integrations-claude-cli/wizard.png");
        // Codex (ChatGPT): CodexIntegrationWizardBtn — only present when disconnected.
        TryWizard(automation, new[] { "CodexIntegrationWizardBtn" }, outDir, "integrations-codex-cli", "wizard", "/assets/help/integrations-codex-cli/wizard.png");
    }

    private void TryWizard(UIA3Automation automation, string[] candidates, string outDir, string slug, string name, string src)
    {
        var hwnd = FindWindow("Settings", true);
        if (hwnd == IntPtr.Zero) { Log($"  wizard '{slug}' — settings window not found, skip"); return; }
        var win = automation.FromHandle(hwnd).AsWindow();
        AutomationElement? btn = null;
        foreach (var c in candidates)
        {
            btn = win.FindFirstDescendant(cf => cf.ByAutomationId(c)) ?? win.FindFirstDescendant(cf => cf.ByName(c));
            if (btn != null) break;
        }
        if (btn == null) { Log($"  wizard '{slug}' button {string.Join("/", candidates)} not found — skip"); return; }
        try { btn.AsButton().Invoke(); } catch { try { btn.Click(); } catch { } }
        Thread.Sleep(2200);
        var rel = Path.Combine(slug, $"{name}-light.png");
        if (CaptureHwnd(hwnd, Path.Combine(outDir, rel)))
        {
            Log($"  captured {rel}");
            Record(rel.Replace('\\', '/'), slug, name, src, "ready", "light");
        }
        // Close the modal so the next wizard's button is reachable.
        try { Keyboard.Press(VirtualKeyShort.ESCAPE); Thread.Sleep(800); } catch { }
    }

    private static readonly string[] PillStates = { "Idle", "Recording", "Transcribing", "Processing", "Completing", "Error" };

    // Pill states: force each visual state via demo-pill, capture each, then
    // compose a horizontal states-strip. Pill is borderless/transparent — the
    // opaque inner capsule captures fine via PrintWindow (black margin around it).
    private void CapturePillStates(string outDir)
    {
        Log("=== Pill states ===");
        KillDimmy();
        SetTheme("Dark");
        Application.Launch(_exe);
        Thread.Sleep(6000);
        SendCommand("toggle-pill");
        Thread.Sleep(1500);

        var pillDir = Path.Combine(outDir, "pill");
        var frames = new List<string>();
        foreach (var s in PillStates)
        {
            SendCommand($"demo-pill:{s}");
            Thread.Sleep(1300);
            var hwnd = FindPill();
            var file = Path.Combine(pillDir, $"state-{s.ToLowerInvariant()}.png");
            if (CaptureScreenRect(hwnd, file)) { frames.Add(file); Log($"  captured pill {s}"); }
            else Log($"  pill {s}: window not found");
        }

        var idle = Path.Combine(pillDir, "state-idle.png");
        if (File.Exists(idle))
        {
            File.Copy(idle, Path.Combine(pillDir, "idle.png"), true);
            Record("pill/idle.png", "pill", "idle", "/assets/help/pill/idle.png", "ready", "dark");
        }
        if (frames.Count > 0 && ComposeStrip(frames, Path.Combine(pillDir, "states-strip.png")))
        {
            Record("pill/states-strip.png", "pill", "states-strip", "/assets/help/pill/states-strip.png", "composed", "dark");
            // first-dictation/pill-states is the same strip in another article.
            File.Copy(Path.Combine(pillDir, "states-strip.png"),
                      Path.Combine(outDir, "first-dictation", "pill-states.png"), true);
            Record("first-dictation/pill-states.png", "first-dictation", "pill-states", "/assets/help/first-dictation/pill-states.png", "composed", "dark");
        }
    }

    // The pill is the small BORDERLESS (no WS_CAPTION) Dimmy window. The earlier
    // size-only finder grabbed a small captioned helper window by mistake.
    private static IntPtr FindPill()
    {
        var pids = Process.GetProcessesByName("Dimmy.Windows").Select(p => p.Id).ToHashSet();
        IntPtr best = IntPtr.Zero; long bestArea = long.MaxValue;
        EnumWindows((h, l) =>
        {
            if (!IsWindowVisible(h)) return true;
            GetWindowThreadProcessId(h, out var pid);
            if (!pids.Contains((int)pid)) return true;
            if ((GetWindowLong(h, GWL_STYLE) & WS_CAPTION) == WS_CAPTION) return true; // skip chromed windows
            GetWindowRect(h, out var r);
            int w = r.Right - r.Left, ht = r.Bottom - r.Top;
            long area = (long)w * ht;
            if (w < 40 || ht < 20 || w > 700 || ht > 320) return true;  // pill-sized only
            if (area < bestArea) { bestArea = area; best = h; }
            return true;
        }, IntPtr.Zero);
        return best;
    }

    // Screen-region BitBlt — required for the pill, which is a layered/transparent
    // window that PrintWindow renders as garbage. Captures what is on screen at
    // the window's rect (so the pill must be visible and unobscured).
    private bool CaptureScreenRect(IntPtr h, string outFile)
    {
        if (h == IntPtr.Zero) { Log($"  no window for {Path.GetFileName(outFile)}"); return false; }
        GetWindowRect(h, out var r);
        int w = r.Right - r.Left, ht = r.Bottom - r.Top;
        if (w <= 0 || ht <= 0) return false;
        using var bmp = new Bitmap(w, ht);
        using (var g = Graphics.FromImage(bmp))
            g.CopyFromScreen(r.Left, r.Top, 0, 0, new Size(w, ht));
        Directory.CreateDirectory(Path.GetDirectoryName(outFile)!);
        bmp.Save(outFile, ImageFormat.Png);
        return true;
    }

    private bool ComposeStrip(List<string> files, string outFile)
    {
        try
        {
            var imgs = files.Where(File.Exists).Select(Image.FromFile).ToList();
            if (imgs.Count == 0) return false;
            const int pad = 24;
            int h = imgs.Max(i => i.Height);
            int w = imgs.Sum(i => i.Width) + pad * (imgs.Count + 1);
            using var strip = new Bitmap(w, h + pad * 2);
            using (var g = Graphics.FromImage(strip))
            {
                g.Clear(Color.FromArgb(255, 18, 18, 18));
                int x = pad;
                foreach (var im in imgs)
                {
                    g.DrawImage(im, x, pad + (h - im.Height) / 2);
                    x += im.Width + pad;
                }
            }
            Directory.CreateDirectory(Path.GetDirectoryName(outFile)!);
            strip.Save(outFile, ImageFormat.Png);
            foreach (var im in imgs) im.Dispose();
            Log($"  composed {Path.GetFileName(outFile)} ({imgs.Count} states)");
            return true;
        }
        catch (Exception ex) { Log($"  strip compose failed: {ex.Message}"); return false; }
    }
}
