# Runs a Nemotron meeting and the pass after it at several levels of free commit
# charge. CLOSE OTHER PROGRAMS FIRST: near zero free, any program can crash.
#
#   .\scripts\dev\nemotron-memory-test.ps1 -Meeting <meeting dir> [-Seconds 45] [-Runs "12,9,6,5,3"]
#
# Prefix a level with f to run F32, with r to squeeze memory while the meeting loads.
# Build first, with the frozen feature set plus:
#   cargo build --release --bin commit_hog --bin nemotron_meeting_replay --features ...
param(
    [Parameter(Mandatory)][string]$Meeting,
    [int]$Seconds = 45,
    [string]$Runs = "12,f12,9,f9,6,5,3,r1"
)
$bin = Join-Path $PSScriptRoot "..\..\core\target\release"
$env:ORT_DYLIB_PATH = (Resolve-Path (Join-Path $bin "..\onnxruntime-win-x64-1.22.0\lib\onnxruntime.dll")).Path
$log = "$env:APPDATA\dimmy\dimmy.log"
$marker = "$env:APPDATA\dimmy\last_abort.txt"

function Start-Hog([double]$FreeGb) {
    $out = New-TemporaryFile
    $hog = Start-Process "$bin\commit_hog.exe" -ArgumentList "--free-gb $FreeGb --hold-secs 900" -PassThru -NoNewWindow -RedirectStandardOutput $out
    return @{ Process = $hog; Out = $out }
}

foreach ($run in $Runs.Split(",")) {
    $f32 = $run.StartsWith("f")
    $race = $run.StartsWith("r")
    $gb = [double]($run.TrimStart("f", "r"))
    Remove-Item $marker -ErrorAction SilentlyContinue
    if ($f32) { $env:DIMMY_NEMOTRON_F32 = "1" } else { Remove-Item Env:DIMMY_NEMOTRON_F32 -ErrorAction SilentlyContinue }

    $hog = $null
    if (-not $race) {
        $hog = Start-Hog $gb
        while (-not (Select-String -Path $hog.Out -Pattern "at target|disk floor" -Quiet) -and -not $hog.Process.HasExited) {
            Start-Sleep -Milliseconds 200
        }
    }
    $logStart = (Get-Item $log).Length
    $out = New-TemporaryFile
    $p = Start-Process "$bin\nemotron_meeting_replay.exe" -ArgumentList "`"$Meeting`" --seconds $Seconds --retranscribe" -PassThru -NoNewWindow -RedirectStandardOutput $out -RedirectStandardError "$out.err"
    $null = $p.Handle
    if ($race) {
        Start-Sleep -Milliseconds 300
        $hog = Start-Hog $gb
    }
    $max = 0L
    while (-not $p.HasExited) {
        try { $p.Refresh(); if ($p.PagedMemorySize64 -gt $max) { $max = $p.PagedMemorySize64 } } catch {}
        Start-Sleep -Milliseconds 500
    }
    $free = (Get-CimInstance Win32_OperatingSystem).FreeVirtualMemory / 1MB
    Stop-Process -Id $hog.Process.Id -Force -ErrorAction SilentlyContinue

    "`n=== $run  exit=$($p.ExitCode)  peak commit=$([math]::Round($max / 1GB, 1)) GB  free at end=$([math]::Round($free, 1)) GB"
    Get-Content $hog.Out | Select-Object -First 1
    Get-Content $out | Where-Object { $_ -match '^===|^audio_(mic|system)' } | ForEach-Object { $_.Substring(0, [math]::Min(170, $_.Length)) }
    $fs = [IO.File]::Open($log, 'Open', 'Read', 'ReadWrite'); $null = $fs.Seek($logStart, 'Begin')
    $r = New-Object IO.StreamReader($fs); $text = $r.ReadToEnd(); $r.Close()
    ($text -split "`n") | Where-Object { $_ -match 'Nemotron\] (loaded|GPU engine)|track unavailable|nemotron live|cannot keep up|retranscribe.*(fail|error)' } |
        ForEach-Object { ($_ -replace '^\[[^\]]+\] ', '').Trim() } | Group-Object | ForEach-Object { "  $($_.Count)x $($_.Name.Substring(0, [math]::Min(120, $_.Name.Length)))" }
    if (Test-Path $marker) { "  ABORT marker: $(Get-Content $marker)" }
    Remove-Item $out, "$out.err", $hog.Out -ErrorAction SilentlyContinue
}
