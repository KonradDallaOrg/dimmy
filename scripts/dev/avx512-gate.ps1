<#
.SYNOPSIS
  Reject a dimmy_lib.dll that carries AVX-512 instructions.

.DESCRIPTION
  GGML_NATIVE=OFF in the build step is the fix; this gate is the proof. A DLL
  built with -march=native on an AVX-512 runner crashes 0xc000001d on any CPU
  without AVX-512 (Alder Lake, Zen 2, ...). It looks random because the runner
  CPU varies run to run, and every other CI gate passes because they all run on
  the runner's own CPU. Measured on v0.6.71-rc10: 4820 zmm instructions in the
  shipped DLL, 0 in a local build of the same source.

  WHY TWO DISASSEMBLERS (2026-09-16). `dumpbin /disasm` is a LINEAR SWEEP over
  the executable sections: it decodes data blobs embedded in .text as if they
  were instructions. dimmy_lib.dll contains such blobs - at 0x180DCD508 in a
  local build the bytes read `D5 D4 DC 00 ...`, repeating table data that
  dumpbin 14.50 cannot decode at all while llvm-objdump 22 renders as
  `paddusb (%r16), %mm0`, an APX+MMX instruction no compiler on earth emits.
  The richer a decoder's instruction set, the more of that data it swallows:
  MSVC 14.51 knows AVX10.2 and turned one such blob into
  `vcvttph2ibs zmm7{k6},zmm5`, which failed the v0.7.3 STABLE release while
  v0.7.3-rc.4 - same commit, same image, same toolset - reported 0. The earlier
  disputed failures were the same shape: 4 hits, then 9 decoding as APX `r26`
  with 8 of them inside a 140-byte window.

  So a bare count cannot tell a real regression from a decoding artifact. Two
  independent decoders can: they desync at DIFFERENT offsets but agree exactly
  on real code. Measured: on a clean 80 MB dimmy_lib.dll dumpbin finds 0 zmm
  and llvm-objdump finds 0; on a DLL built with /arch:AVX512 both find the same
  5 instructions at the same 5 addresses.

  The gate FAILS when:
    - both disassemblers report zmm at the SAME address, or
    - the primary reports >= 20 hits (a -march=native regression is thousands,
      spread over hundreds of functions; a sweep desync is local), or
    - no second disassembler is available to corroborate a hit.
  It never fails open: a hit nobody can corroborate aborts the release.

.PARAMETER Dll
  The DLL to scan.

.PARAMETER Dumpbin
  Primary disassembler. Defaults to the toolset that COMPILED the DLL,
  $env:VS2026_PATH\VC\Tools\MSVC\$env:VCVARS_VER, falling back to the newest
  MSVC installed there.

.PARAMETER Objdump
  Second, independent disassembler. Defaults to the LLVM on the runner image -
  the same install that provides libclang to bindgen. When absent, falls back
  to an MSVC toolset of a DIFFERENT version than the primary; an older one is
  the better corroborator here because it predates the instruction sets whose
  decoders turn table data into exotic vector instructions.

.EXAMPLE
  pwsh scripts/dev/avx512-gate.ps1 -Dll D:\t\release\dimmy_lib.dll
#>
param(
  [Parameter(Mandatory = $true)][string]$Dll,
  [string]$Dumpbin,
  [string]$Objdump
)

$ErrorActionPreference = 'Stop'
$ZMM = 'zmm[0-9]'
$HARD_FAIL_AT = 20

function Fail([string]$msg) {
  Write-Host "::error title=AVX-512 gate::$msg"
  Write-Host $msg
  exit 1
}

# Address formats differ between the tools - dumpbin prints
# `0000000180001000:`, llvm-objdump prints `180001000:` - so normalise both to
# a number before comparing them.
function Get-Addr([string]$line) {
  if ($line -match '^\s*([0-9A-Fa-f]{6,16})[:\s]') { return [Convert]::ToUInt64($Matches[1], 16) }
  return $null
}

if (-not (Test-Path $Dll)) { Fail "dll not found at $Dll" }
$name = Split-Path $Dll -Leaf

$msvcRoot = if ($env:VS2026_PATH) { Join-Path $env:VS2026_PATH "VC\Tools\MSVC" } else { $null }
$installed = @()
if ($msvcRoot -and (Test-Path $msvcRoot)) {
  $installed = @(Get-ChildItem $msvcRoot -Directory | Sort-Object Name -Descending)
}

if (-not $Dumpbin) {
  $pinned = if ($env:VCVARS_VER) { Join-Path $msvcRoot "$env:VCVARS_VER\bin\Hostx64\x64\dumpbin.exe" } else { $null }
  if ($pinned -and (Test-Path $pinned)) {
    $Dumpbin = $pinned
  } elseif ($installed.Count -gt 0) {
    Write-Host "::warning title=AVX gate::VCVARS_VER=$env:VCVARS_VER dumpbin not found; falling back to newest MSVC"
    $Dumpbin = Join-Path $installed[0].FullName "bin\Hostx64\x64\dumpbin.exe"
  }
}
if (-not $Dumpbin -or -not (Test-Path $Dumpbin)) { Fail "dumpbin.exe not found at '$Dumpbin'" }

Write-Host "primary disassembler: $Dumpbin"
$hits = & $Dumpbin /disasm:nobytes $Dll | Select-String -Pattern $ZMM
$n = @($hits).Count
Write-Host "AVX-512 (zmm) instructions in ${name}: $n"
if ($n -eq 0) { exit 0 }

# Report WHERE and WITH WHICH BYTES, not just how many, so the next failure is
# diagnosed by reading the log instead of by bisecting the build.
Write-Host "--- hits, with raw bytes and +/-4 lines of context ---"
& $Dumpbin /disasm $Dll | Select-String -Pattern $ZMM -Context 4, 4 |
  Select-Object -First 20 | ForEach-Object {
    $_.Context.PreContext | ForEach-Object { Write-Host "      $_" }
    Write-Host "  >>> $($_.Line.Trim())"
    $_.Context.PostContext | ForEach-Object { Write-Host "      $_" }
    Write-Host ""
  }

if ($n -ge $HARD_FAIL_AT) {
  Fail "$name contains $n AVX-512 instructions. It would crash 0xc000001d on any CPU without AVX-512. GGML_NATIVE=OFF is not taking effect. Aborting release."
}

if (-not $Objdump) {
  $llvm = Join-Path $env:ProgramFiles "LLVM\bin\llvm-objdump.exe"
  if (Test-Path $llvm) {
    $Objdump = $llvm
  } else {
    $other = $installed |
      Where-Object { (Join-Path $_.FullName "bin\Hostx64\x64\dumpbin.exe") -ne $Dumpbin } |
      Sort-Object Name | Select-Object -First 1
    if ($other) { $Objdump = Join-Path $other.FullName "bin\Hostx64\x64\dumpbin.exe" }
  }
}
if (-not $Objdump -or -not (Test-Path $Objdump)) {
  Fail "$n AVX-512 hit(s) and no second disassembler available to corroborate them. Refusing to ship an unverified DLL."
}

Write-Host "second disassembler: $Objdump"
$rc = 0
$secondAddrs = [System.Collections.Generic.HashSet[UInt64]]::new()
try {
  if ((Split-Path $Objdump -Leaf) -like 'llvm-objdump*') {
    $second = & $Objdump -d --no-show-raw-insn $Dll 2>$null
  } else {
    $second = & $Objdump /disasm:nobytes $Dll
  }
  $rc = $LASTEXITCODE
  $second | Select-String -Pattern $ZMM | ForEach-Object {
    $a = Get-Addr $_.Line
    if ($null -ne $a) { [void]$secondAddrs.Add($a) }
  }
} catch {
  Fail "second disassembler failed ($($_.Exception.Message)); cannot corroborate $n hit(s). Refusing to ship an unverified DLL."
}
if ($rc -ne 0) { Fail "second disassembler exited $rc; cannot corroborate $n hit(s). Refusing to ship an unverified DLL." }
Write-Host "second disassembler zmm hits: $($secondAddrs.Count)"

$agreed = @()
foreach ($h in $hits) {
  $a = Get-Addr $h.Line
  if ($null -ne $a -and $secondAddrs.Contains($a)) { $agreed += ('0x{0:X}' -f $a) }
}
if ($agreed.Count -gt 0) {
  Fail "$($agreed.Count) AVX-512 instruction(s) confirmed by BOTH disassemblers at: $($agreed -join ', '). It would crash 0xc000001d on any CPU without AVX-512. Aborting release."
}

Write-Host "::warning title=AVX gate::$n zmm hit(s) from the primary disassembler, none corroborated by the second at the same address - linear-sweep artifact over data embedded in .text, not shipped AVX-512."
exit 0
