using System;
using System.Diagnostics;
using System.IO;
using System.IO.Pipes;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using FlaUI.Core;
using FlaUI.UIA3;
using Microsoft.Diagnostics.Runtime;
using Microsoft.Win32.SafeHandles;
using Xunit;
using Xunit.Abstractions;

namespace Dimmy.Windows.UiTests;

// TEMPORARY diagnostics for the "Settings window did not appear" hang.
public static class HangDiag
{
    [DllImport("dbghelp.dll", SetLastError = true)]
    private static extern bool MiniDumpWriteDump(IntPtr hProcess, int processId, SafeFileHandle hFile,
        int dumpType, IntPtr exceptionParam, IntPtr userStreamParam, IntPtr callbackParam);

    public static string OutDir()
    {
        var dir = Path.GetFullPath(Path.Combine(AppContext.BaseDirectory, "..", "..", "..", "TestResults", "hangdiag"));
        Directory.CreateDirectory(dir);
        return dir;
    }

    public static void Capture(string tag, Action<string> log)
    {
        var sb = new StringBuilder();
        foreach (var p in Process.GetProcessesByName("Dimmy.Windows"))
        {
            sb.AppendLine($"=== {tag} pid={p.Id} threads={p.Threads.Count} cpu={p.TotalProcessorTime}");
            try
            {
                foreach (ProcessThread t in p.Threads)
                    sb.AppendLine($"  tid={t.Id} state={t.ThreadState} wait={(t.ThreadState == System.Diagnostics.ThreadState.Wait ? t.WaitReason.ToString() : "")} cpu={t.TotalProcessorTime.TotalMilliseconds:F0}ms");
            }
            catch (Exception e) { sb.AppendLine($"thread list: {e.Message}"); }

            // Native stacks first via cdb (noninvasive), if the runner has it.
            var cdb = new[]
            {
                @"C:\Program Files (x86)\Windows Kits\10\Debuggers\x64\cdb.exe",
                @"C:\Program Files\Windows Kits\10\Debuggers\x64\cdb.exe",
            }.FirstOrDefault(File.Exists);
            sb.AppendLine($"cdb: {cdb ?? "not found"}");
            if (cdb != null)
            {
                try
                {
                    var exeDir = Path.GetDirectoryName(p.MainModule!.FileName)!;
                    var psi = new ProcessStartInfo(cdb,
                        $"-pv -p {p.Id} -y \"{exeDir};srv*C:\\symcache*https://msdl.microsoft.com/download/symbols\" -c \".reload /f dimmy_lib.dll; ~*kn 60; qd\"")
                    {
                        RedirectStandardOutput = true,
                        RedirectStandardError = true,
                        UseShellExecute = false,
                    };
                    using var c = Process.Start(psi)!;
                    var outTask = c.StandardOutput.ReadToEndAsync();
                    if (!c.WaitForExit(240_000)) { try { c.Kill(); } catch { } }
                    sb.AppendLine("--- cdb ---");
                    sb.AppendLine(outTask.Wait(5000) ? outTask.Result : "(cdb output timeout)");
                }
                catch (Exception e) { sb.AppendLine($"cdb failed: {e.Message}"); }
            }

            // Managed stacks via ClrMD (suspends the target briefly).
            try
            {
                using var dt = DataTarget.AttachToProcess(p.Id, suspend: true);
                var runtime = dt.ClrVersions[0].CreateRuntime();
                foreach (var t in runtime.Threads)
                {
                    if (!t.IsAlive) continue;
                    sb.AppendLine($"--- managed tid={t.OSThreadId} mtid={t.ManagedThreadId}");
                    int n = 0;
                    foreach (var f in t.EnumerateStackTrace())
                    {
                        sb.AppendLine($"    {f.Kind} {f.FrameName} {f.Method?.Signature ?? ""}");
                        if (++n > 60) break;
                    }
                }
            }
            catch (Exception e) { sb.AppendLine($"clrmd failed: {e}"); }

            try
            {
                var path = Path.Combine(OutDir(), $"dimmy-{tag}-{p.Id}.dmp");
                using var fs = new FileStream(path, FileMode.Create, FileAccess.ReadWrite, FileShare.None);
                // MiniDumpWithFullMemory | MiniDumpWithHandleData | MiniDumpWithThreadInfo
                var ok = MiniDumpWriteDump(p.Handle, p.Id, fs.SafeFileHandle, 0x2 | 0x4 | 0x1000, IntPtr.Zero, IntPtr.Zero, IntPtr.Zero);
                sb.AppendLine($"dump {(ok ? "written" : "FAILED " + Marshal.GetLastWin32Error())}: {path}");
            }
            catch (Exception e) { sb.AppendLine($"dump failed: {e.Message}"); }
        }
        var text = sb.ToString();
        File.WriteAllText(Path.Combine(OutDir(), $"stacks-{tag}.log"), text);
        log(text);
        Console.WriteLine(text);
    }
}

public class SettingsHangRepro : IDisposable
{
    private readonly ITestOutputHelper _output;

    public SettingsHangRepro(ITestOutputHelper output) { _output = output; }

    public void Dispose() { Kill(); }

    private static void Kill()
    {
        foreach (var p in Process.GetProcessesByName("Dimmy.Windows"))
        {
            try { p.Kill(entireProcessTree: true); p.WaitForExit(3000); } catch { }
        }
    }

    private static string Marker() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "dimmy", ".onboarding_done");

    private static string LocateExe()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir != null)
        {
            foreach (var bin in new[] { Path.Combine("bin", "x64", "Debug"), Path.Combine("bin", "Debug"), Path.Combine("bin", "Release") })
            {
                var c = Path.Combine(dir.FullName, "platforms", "windows", "Dimmy.Windows", bin,
                    "net8.0-windows10.0.19041.0", "win-x64", "Dimmy.Windows.exe");
                if (File.Exists(c)) return c;
            }
            dir = dir.Parent;
        }
        throw new FileNotFoundException("Build Dimmy.Windows first.");
    }

    private static bool TrySend(string cmd)
    {
        try
        {
            using var client = new NamedPipeClientStream(".", "DimmyCommand", PipeDirection.Out);
            client.Connect(1000);
            using var w = new StreamWriter(client) { AutoFlush = true };
            w.WriteLine(cmd);
            client.WaitForPipeDrain();
            return true;
        }
        catch { return false; }
    }

    [Fact]
    public void SettingsOpensEveryTime()
    {
        int failures = 0;
        var exe = LocateExe();
        for (int i = 0; i < 30; i++)
        {
            Kill();
            // Every third round runs through the onboarding window first, like the
            // onboarding tests that precede the failing Legal test.
            bool onboarding = i % 3 == 2;
            if (onboarding) { try { File.Delete(Marker()); } catch { } }
            else { Directory.CreateDirectory(Path.GetDirectoryName(Marker())!); File.WriteAllText(Marker(), ""); }

            using var automation = new UIA3Automation();
            var app = Application.Launch(exe);
            var sw = Stopwatch.StartNew();
            if (onboarding)
            {
                Thread.Sleep(3000);
                try { app.Close(); } catch { }
                Thread.Sleep(1500);
                continue;
            }
            bool sent = false;
            while (!sent && sw.Elapsed < TimeSpan.FromSeconds(30)) { sent = TrySend("open-settings:about"); if (!sent) Thread.Sleep(250); }
            var sentAt = sw.ElapsedMilliseconds;
            bool found = false;
            while (sw.Elapsed < TimeSpan.FromSeconds(30))
            {
                try
                {
                    if (automation.GetDesktop().FindFirstChild(cf => cf.ByName("Dimmy Settings")) != null) { found = true; break; }
                }
                catch { }
                Thread.Sleep(250);
            }
            _output.WriteLine($"round {i}: sent={sent} at {sentAt} ms, settings={(found ? sw.ElapsedMilliseconds + " ms" : "TIMEOUT")}");
            Console.WriteLine($"[HangRepro] round {i}: sent={sent} at {sentAt} ms, settings={(found ? sw.ElapsedMilliseconds + " ms" : "TIMEOUT")}");
            if (!found)
            {
                failures++;
                HangDiag.Capture($"round{i}", _output.WriteLine);
                if (failures >= 2) break;
            }
        }
        Assert.Equal(0, failures);
    }
}
