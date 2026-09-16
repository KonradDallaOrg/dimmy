<#
.SYNOPSIS
  Reject a dimmy_lib.dll that carries 512-bit AVX-512 instructions.

.DESCRIPTION
  GGML_NATIVE=OFF in the build step is the fix; this gate is the proof. A DLL
  built with -march=native on an AVX-512 runner crashes 0xc000001d on any CPU
  without AVX-512 (Alder Lake, Zen 2, ...). Every other CI gate misses it,
  because they all execute on the runner's own CPU. Measured on v0.6.71-rc10:
  4820 zmm instructions in the shipped DLL, 0 in a local build of the same
  source.

  512-BIT ONLY, ON PURPOSE (2026-09-16). dimmy_lib.dll legitimately CONTAINS
  AVX-512: the statically linked UCRT carries 10 EVEX instructions
  (`vpmullq xmm1,xmm0,xmm7`, `vpminuq xmm1,xmm0,xmm2` - EVEX-only, no VEX
  encoding exists), CPUID-dispatched through `__isa_available` and never
  executed on a CPU without the feature. Widening this to "any EVEX" or to
  k-mask forms would fail EVERY build on Microsoft's own runtime. 512-bit
  width is what `/arch:AVX512` and `-march=native` emit and what the CRT's
  dispatched paths never use. The narrowness is load-bearing.

  NO SECOND DECODER, NO THRESHOLD (2026-09-16). Both were tried and measured
  against the real bytes; do not re-derive them:
   - Two decoders agreeing does NOT discriminate. They are deterministic
     linear sweeps over the same bytes, and x86 self-synchronises. dumpbin
     14.50 decodes the disputed `62 F5 7C 4E 68 FD` as
     `vcvttph2ibs zmm7{k6},zmm5` exactly like 14.51, and so does
     llvm-objdump 22 - so corroboration would have AGREED on v0.7.3's hit and
     failed the release anyway, while claiming more certainty than it had.
   - A ">= 20 hits" threshold fails open: one small function compiled
     /arch:AVX512 yields 8 zmm instructions.
   - `.pdata` containment does not separate code from data: the data table at
     0x180DCD508 sits INSIDE a function's unwind range.
   - Re-decoding in phase from a `.pdata` function entry reproduces the sweep
     byte for byte whenever the sweep was already in phase, which it usually
     is. Printed below as EVIDENCE, never as a verdict.

  So the gate stops classifying and goes back to being a detector that cannot
  fail open. ANY hit aborts. If the bytes it prints show a data table rather
  than code, cut the next patch version - that is cheap. Weakening this is
  not: it guards the crash that shipped in v0.6.71-rc9/rc10.

.PARAMETER Dll
  The DLL to scan.

.PARAMETER Dumpbin
  Disassembler. Defaults to the toolset that COMPILED the DLL,
  $env:VS2026_PATH\VC\Tools\MSVC\$env:VCVARS_VER, falling back to the newest
  MSVC installed there.

.PARAMETER EvidenceDir
  Where to stage the DLL and the hit context when the gate fires. Defaults to
  $env:RUNNER_TEMP\avx512-evidence, which release.yml uploads on failure.

.EXAMPLE
  pwsh scripts/dev/avx512-gate.ps1 -Dll D:\t\release\dimmy_lib.dll
#>
param(
  [Parameter(Mandatory = $true)][string]$Dll,
  [string]$Dumpbin,
  [string]$EvidenceDir
)

$ErrorActionPreference = 'Stop'
$PSNativeCommandUseErrorActionPreference = $false

# dumpbin puts the address at the very start of an instruction line, behind
# exactly two spaces. Anchoring to that shape means a symbol name containing
# "zmm" can never be counted as a hit.
$INSN = '^\s\s([0-9A-F]{16}): '
$WIDE = $INSN + '.*(zmm[0-9]|zmmword)'

function Fail([string]$m) {
  Write-Host "::error title=AVX-512 gate::$m"
  Write-Host "GATE FAIL: $m"
  exit 1
}

if (-not (Test-Path $Dll)) { Fail "dll not found at $Dll" }
$name = Split-Path $Dll -Leaf
if (-not $EvidenceDir) {
  $root = if ($env:RUNNER_TEMP) { $env:RUNNER_TEMP } else { $env:TEMP }
  $EvidenceDir = Join-Path $root 'avx512-evidence'
}
New-Item -ItemType Directory -Force $EvidenceDir | Out-Null

if (-not $Dumpbin) {
  $msvcRoot = if ($env:VS2026_PATH) { Join-Path $env:VS2026_PATH 'VC\Tools\MSVC' } else { $null }
  if (-not $msvcRoot -or -not (Test-Path $msvcRoot)) { Fail "VC\Tools\MSVC not found under VS2026_PATH='$env:VS2026_PATH'" }
  $pinned = if ($env:VCVARS_VER) { Join-Path $msvcRoot "$env:VCVARS_VER\bin\Hostx64\x64\dumpbin.exe" } else { $null }
  if ($pinned -and (Test-Path $pinned)) {
    $Dumpbin = $pinned
  } else {
    # I9: filesystem order is not version order.
    $newest = Get-ChildItem $msvcRoot -Directory | Sort-Object Name -Descending | Select-Object -First 1
    if (-not $newest) { Fail "no MSVC toolset under $msvcRoot" }
    Write-Host "::warning title=AVX gate::VCVARS_VER='$env:VCVARS_VER' dumpbin absent; using newest $($newest.Name)"
    $Dumpbin = Join-Path $newest.FullName 'bin\Hostx64\x64\dumpbin.exe'
  }
}
if (-not (Test-Path $Dumpbin)) { Fail "dumpbin.exe not found at '$Dumpbin'" }
Write-Host "disassembler: $Dumpbin"

function Invoke-Dumpbin([string[]]$dumpArgs, [string]$what) {
  $out = & $Dumpbin @dumpArgs
  if ($LASTEXITCODE -ne 0) { Fail "dumpbin $what exited $LASTEXITCODE. A failed dump must never be read as 'no AVX-512'." }
  return $out
}

$hdr = Invoke-Dumpbin @('/headers', '/nologo', $Dll) 'headers'
$bl = $hdr | Select-String 'image base' | Select-Object -First 1
if (-not $bl -or $bl.Line -notmatch '([0-9A-F]+) image base') { Fail 'could not parse image base from dumpbin /headers' }
$BASE = [Convert]::ToUInt64($Matches[1], 16)
$cl = $hdr | Select-String 'size of code' | Select-Object -First 1
if (-not $cl -or $cl.Line -notmatch '([0-9A-F]+) size of code') { Fail "could not parse 'size of code' from dumpbin /headers" }
$CODE = [Convert]::ToUInt64($Matches[1], 16)
if ($CODE -eq 0) { Fail 'PE reports 0 bytes of code' }
$bc = $hdr | Select-String 'base of code' | Select-Object -First 1
if (-not $bc -or $bc.Line -notmatch '([0-9A-F]+) base of code') { Fail "could not parse 'base of code' from dumpbin /headers" }
$CODEBASE = [Convert]::ToUInt64($Matches[1], 16)
Write-Host ("image base 0x{0:X}, code 0x{1:X}..0x{2:X} ({3} bytes)" -f $BASE, ($BASE + $CODEBASE), ($BASE + $CODEBASE + $CODE), $CODE)

# /disasm, NOT /disasm:nobytes: the raw encoding IS the evidence. v0.7.3 was
# argued over for a day about one line whose six bytes would have named it.
$sweep = Join-Path $EvidenceDir 'disasm.txt'
Invoke-Dumpbin @('/disasm', '/nologo', "/out:$sweep", $Dll) 'disasm' | Out-Null
if (-not (Test-Path $sweep)) { Fail 'dumpbin /disasm produced no output file' }

# Coverage assert: prove the sweep reached the END of the code section.
# Without it, a dumpbin that dies mid-file yields zero matches and the gate
# PASSES. That is the oldest hole in this step, older than any decoder
# dispute, and the only one that can silently ship the crash it exists to
# stop. Reading the tail costs 0.05 s.
$reach = [UInt64]0
foreach ($l in (Get-Content $sweep -Tail 2000)) {
  $m = [regex]::Match($l, $INSN)
  if ($m.Success) { $reach = [Convert]::ToUInt64($m.Groups[1].Value, 16) }
}
$need = $BASE + $CODEBASE + [uint64]($CODE * 0.9)
Write-Host ("disassembly reached 0x{0:X} (needs >= 0x{1:X})" -f $reach, $need)
if ($reach -lt $need) { Fail ("dumpbin /disasm stopped at 0x{0:X}, short of the end of the code section (needs >= 0x{1:X}). Refusing to read a truncated dump as clean." -f $reach, $need) }

$hits = @(Select-String -Path $sweep -Pattern $WIDE)
Write-Host "512-bit AVX-512 instructions in ${name}: $($hits.Count)"
if ($hits.Count -eq 0) { Remove-Item $sweep -Force -ErrorAction SilentlyContinue; Write-Host 'clean.'; exit 0 }

# ---- From here the release is already dead. Everything below is evidence. ----
Write-Host ''
Write-Host '--- hits: raw bytes and +/-4 instructions of context ---'
Select-String -Path $sweep -Pattern $WIDE -Context 4, 4 | Select-Object -First 20 | ForEach-Object {
  $_.Context.PreContext | ForEach-Object { Write-Host "        $_" }
  Write-Host "  >>>   $($_.Line)"
  $_.Context.PostContext | ForEach-Object { Write-Host "        $_" }
  Write-Host ''
}

# Stage the evidence BEFORE the optional diagnostics below, so a hiccup there
# can never cost us the DLL. Keep the DLL and the hit context, not the ~330 MB
# dump: the DLL is ground truth and anyone can re-dump from it. v0.7.3 was
# unanswerable precisely because the gate threw before keeping anything.
Select-String -Path $sweep -Pattern $WIDE -Context 12, 12 |
  Select-Object -First 20 |
  ForEach-Object { $_.Context.PreContext; "  >>>   $($_.Line)"; $_.Context.PostContext; '' } |
  Set-Content (Join-Path $EvidenceDir 'hits-context.txt') -Encoding utf8
Remove-Item $sweep -Force -ErrorAction SilentlyContinue
Copy-Item $Dll (Join-Path $EvidenceDir $name) -Force
Write-Host "evidence staged in ${EvidenceDir}: $name + hits-context.txt"
Write-Host ''

# Re-decode each hit's enclosing function from its ENTRY POINT, a boundary
# .pdata says is real. EVIDENCE, never a verdict: measured on a real
# dimmy_lib.dll, a sweep already in phase entering the function reproduces the
# in-phase decode byte for byte, including over the data tables parked inside
# .text (0x180DCD508). It only disagrees when the sweep entered out of phase.
# So "not a boundary" proves an artifact; "is a boundary" proves nothing
# either way. Read the bytes.
try {
  $uw = Join-Path $EvidenceDir 'unwind.txt'
  Invoke-Dumpbin @('/unwindinfo', '/nologo', "/out:$uw", $Dll) 'unwindinfo' | Out-Null
  $fb = [System.Collections.Generic.List[UInt64]]::new(); $fe = [System.Collections.Generic.List[UInt64]]::new()
  foreach ($l in [IO.File]::ReadLines($uw)) {
    if ($l -match '^  [0-9A-F]{8} ([0-9A-F]{8}) ([0-9A-F]{8}) ') {
      $fb.Add([Convert]::ToUInt64($Matches[1], 16)); $fe.Add([Convert]::ToUInt64($Matches[2], 16))
    }
  }
  Write-Host "--- in-phase re-decode (evidence only; .pdata entries: $($fb.Count)) ---"
  foreach ($h in ($hits | Select-Object -First 20)) {
    if ($h.Line -notmatch $INSN) { continue }
    $va = [Convert]::ToUInt64($Matches[1], 16); $rva = $va - $BASE
    $i = -1; for ($k = 0; $k -lt $fb.Count; $k++) { if ($fb[$k] -le $rva -and $rva -lt $fe[$k]) { $i = $k; break } }
    if ($i -lt 0) { Write-Host ("  0x{0:X}  outside every RUNTIME_FUNCTION (leaf code or data)" -f $va); continue }
    $s = $BASE + $fb[$i]; $t = $BASE + $fe[$i]
    $an = Invoke-Dumpbin @('/disasm', '/nologo', ("/range:0x{0:X},0x{1:X}" -f $s, $t), $Dll) 'anchored range'
    $prev = [UInt64]0; $exact = $false
    foreach ($al in $an) { if ($al -match $INSN) { $a = [Convert]::ToUInt64($Matches[1], 16); if ($a -eq $va) { $exact = $true; break }; if ($a -lt $va) { $prev = $a } } }
    if ($exact) { Write-Host ("  0x{0:X}  IS an instruction boundary when decoded in phase from 0x{1:X} - consistent with real code" -f $va, $s) }
    else { Write-Host ("  0x{0:X}  is NOT a boundary in phase from 0x{1:X} (inside the instruction at 0x{2:X}) - sweep artifact" -f $va, $s, $prev) }
  }
} catch {
  Write-Host "::warning title=AVX gate::evidence pass failed ($($_.Exception.Message)); the verdict below is unaffected."
}

Write-Host ''
Fail "$name contains $($hits.Count) 512-bit AVX-512 instruction(s). They crash 0xc000001d on any CPU without AVX-512 (Alder Lake, Zen 2). Aborting release. If the bytes above show this is a linear-sweep decode of a data table, cut the next patch version - do NOT weaken this gate."
