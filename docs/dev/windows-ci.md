# Windows CI — 11 invariants (read before editing any workflow)

> Every rule here is paid for in blood. Between v0.6.11 and v0.6.20, eight iterations were burned getting the Windows installer building cleanly on `windows-2025` with MSVC 14.50+. Stamping on one of these = reintroducing that specific shipped bug. Read this page before editing:
>
> - `.github/workflows/release.yml`
> - `.github/workflows/staging-auto-update.yml`
> - `.github/workflows/test-install.yml` (on BOTH `staging` AND `main`)
> - `platforms/windows/verify-self-contained.ps1`

The CHANGELOG entries for v0.6.11–v0.6.20 are the per-rule archaeology. Read the block next to each invariant if you need to understand *how* this bug actually manifested.

> **Note on `.github/workflows/e2e-tests.yml`** — this is the additive tier-1/tier-2 testing pipeline (see [`testing.md`](testing.md)). It runs on `pull_request` only, produces no release artifacts, and is NOT one of the four files above. You can edit it freely without triggering the invariants.

---

## I1. `dimmy_lib.dll` is built with MSVC linker ≥ 14.50

**Why.** MSVC 14.44 (the `windows-2025` runner's default VS 2022 toolchain) miscompiles the whisper.cpp `ggml-vulkan` state-init path. The installer crashes silently on the first transcription inside `whisper_backend_init_gpu` during `create_state()`. Pre-v0.6.20 releases shipped this bug. Empirical proof: swapping the installer's DLL for a locally-built one (linker 14.50) on the same machine resolved the crash.

**How to check.** Both `release.yml` and `staging-auto-update.yml` must:
1. Install `visualstudio2026buildtools-preview` via chocolatey with `--pre`
2. Locate it via `vswhere -version "[18.0,19.0)" -prerelease`
3. Activate its `vcvars64.bat` inside the Rust build step (shell: cmd)
4. Gate the built DLL with `dumpbin /headers | findstr "linker version"` and fail on < 14.50

Step names to preserve (don't rename, CI logs reference them by name):
- **"Install VS 2026 BuildTools side-by-side (MSVC 14.50+)"**
- **"Build Rust DLL (VS 2026 MSVC env)"**
- **"Gate — verify dimmy_lib.dll linker version"**

**The 14.50 pin is a race, not an availability problem.** The runner image ships 14.51 / 14.44 / 14.29, so the step adds 14.50 through `vs_installer modify`, probing component ids because they are not queryable. `vs_installer` **returns before the toolset is on disk**, and until 2026-09-16 the step waited a single 10 seconds before deciding. Measured on two runs of the same commit, two hours apart on the same image: both tried `…VC.14.50.18.4…`, `…18.0…`, `…18.8…`; `v0.7.4` found 14.50 eleven seconds after the third probe and `v0.7.3` did not, and fell back to 14.51 with a warning. So: **when a release fails in a way that smells of the toolchain, check which toolset it actually built with before theorising.** The loop now polls for up to 90 s per probe and tries `18.8` first, which is the id that lands on this image.

Losing the race is a real defect on its own terms — it silently ships the toolchain this invariant exists to avoid. It is **not**, however, what caused the `v0.7.3` gate failure: all three decoders tested render the disputed bytes identically, so the fallback changed which binary got built, not what the gate could see (I11).

---

## I2. VS 2026 BuildTools is installed side-by-side — VS 2022 is NOT removed

**Why.** VS 2026 BuildTools SKU lacks the UWP / AppxPackage workloads that `dotnet publish` needs for **MrtCore PRI generation**. Without the app's `resources.pri`, every WinUI window throws `XamlParseException` at `InitializeComponent()` and the app runs headless (no visible window, process alive, user confused). VS 2022 Enterprise on the runner image has those workloads. Keep both.

**How to check.** The "Locate VS AppxPackage tools" step uses `vswhere -version "[17.0,18.0)"` **explicitly** — not `-latest`. `-latest` returns VS 2026 (newer install date) and the probe for `Microsoft.Build.Packaging.Pri.Tasks.dll` would fail.

---

## I3. VS 2026 activation is scoped to the Rust build step only

**Why.** `vcvars64.bat` sets `PATH` / `INCLUDE` / `LIB` / `CL` for the subshell. If it leaked to subsequent pwsh steps, `vswhere -latest` and other VS queries would resolve to VS 2026, breaking the UWP/PRI generation chain (I2).

**How to check.** "Build Rust DLL (VS 2026 MSVC env)" uses `shell: cmd` and calls `vcvars64.bat` in-process. Every other Windows step uses `shell: pwsh`. The activation stays in that one cmd subshell.

---

## I4. No co-located `vcruntime140.dll` / `msvcp140.dll` in the publish folder

**Why.** Velopack's `--framework vcredist143-x64` (in the `vpk pack` call) installs the official Microsoft VC Redist to System32 at setup time. Bundling a second copy next to `dimmy_lib.dll` is at best redundant; historically, it caused the v0.6.10 ABI mismatch crash — Windows' DLL search loaded the co-located older `msvcp140` (14.29.30157 from 2021, picked by `Get-ChildItem | Select -First 1` without a version sort — see I9) before the System32 one, and our DLL was compiled against the newer ABI. System32 alone is the correct path.

**How to check.**
- `release.yml` + `staging-auto-update.yml` "Prepare distribution" step must NOT copy `vcruntime140.dll` / `msvcp140.dll` into the publish folder
- `verify-self-contained.ps1` must NOT list them in `$requiredFiles`
- `test-install.yml` must NOT list them in `$critical`

---

## I5. `test-install.yml` on `main` must match `staging`'s check logic

**Why.** GitHub evaluates `workflow_run` triggers from the **default branch** (`main`), not the branch that produced the upstream workflow. A staging push triggers `Staging Release` on `staging`'s workflow file — but the `Test Install (Clean Windows)` follow-up uses `main`'s `test-install.yml`. If they disagree (e.g. `staging` dropped a check but `main` still has it), the workflow_run shows a red X on every release even though `release.yml`'s inline test-install job passed. Users notice.

**How to check.** After changing `test-install.yml`'s bundle-integrity assertions on `staging`, cherry-pick or mirror the same change to `main`. Don't let them diverge.

---

## I6. PowerShell steps that invoke native installers end with `exit 0`

**Why.** GitHub's pwsh wrapper sets `$PSNativeCommandUseErrorActionPreference = $true` implicitly. `choco install` returns **3010** on success-with-reboot-required (which is what VS installers do). Without an explicit `exit 0`, pwsh propagates `$LASTEXITCODE=3010` as the step's exit code even after the follow-up logic succeeded. Previously this manifested as: script prints "DONE", step exits 1, no exception, no stack trace. Hours lost to staring at a silent failure.

**How to check.** The "Install VS 2026 BuildTools" step and the "Gate — verify linker" step both end with `exit 0`. Any new pwsh step that runs `choco install`, `setup.exe`, or similar must do the same.

---

## I7. `$env:GITHUB_ENV` writes use `Add-Content -Encoding utf8`, never `>>`

**Why.** PowerShell's `>>` defaults to UTF-16 LE with BOM. The `GITHUB_ENV` parser expects UTF-8. Mixed encodings corrupt the file, breaking every `env:` propagation for the rest of the job silently.

```powershell
# WRONG — UTF-16 LE with BOM
"KEY=$value" >> $env:GITHUB_ENV

# RIGHT — explicit UTF-8
Add-Content -Path $env:GITHUB_ENV -Value "KEY=$value" -Encoding utf8
```

**How to check.** Grep workflows for `>> $env:GITHUB_ENV` or `>> $GITHUB_ENV` — there should be none.

---

## I8. The `windows-2025` runner label is preserved

**Why.** `windows-latest` is still `windows-2022` as of 2026-04. `windows-2022` ships MSVC 14.44 — the miscompile is guaranteed. Any CI that uses `windows-latest` for the Rust build is broken by default. Use `windows-2025` for `build-windows` and `test-install` jobs.

**How to check.** `runs-on: windows-2025` in both `build-windows` jobs. The `test-install.yml` job may stay on `windows-latest` because it exercises the shipped installer on a clean runner — it is not building.

---

## I9. `Get-ChildItem ... | Select -First 1` over version-named dirs is banned without `Sort-Object`

**Why.** Filesystem enumeration order is NOT version order. For `VC\Redist\MSVC\`, it often returns the oldest retained version. The v0.6.10 ABI mismatch crash came from `Get-ChildItem VC -Recurse -Filter vcruntime140.dll | Select -First 1` picking 14.29.30157 from 2021. Same trap applies to `VC\Tools\MSVC\` subfolders and anywhere else numeric suffixes matter.

```powershell
# WRONG — returns oldest on NTFS
Get-ChildItem ... | Select-Object -First 1

# RIGHT — explicit sort, newest first
Get-ChildItem ... | Sort-Object Name -Descending | Select-Object -First 1

# If unequal version segments, cast to [version] for sort
Get-ChildItem ... | Sort-Object { [version]$_.Name } -Descending | Select-Object -First 1
```

**How to check.** Grep workflows for `-First 1` without a preceding `Sort-Object ... -Descending`.

---

## I10. `vpk pack ... --framework vcredist143-x64` must stay

**Why.** The `--framework vcredist143-x64` flag is what delegates VC Redist installation to Velopack Setup. Invariant I4 relies on this — System32 is where `msvcp140` / `vcruntime140` live. If someone "cleans this up" because the app seems self-contained, clean-install users will have no VC Redist in System32 and the app will fail to load `dimmy_lib.dll` with a cryptic "missing dependency" error.

**How to check.** `vpk pack ... --framework vcredist143-x64` is present in the "Package with Velopack" step of both `release.yml` and `staging-auto-update.yml`.

---

## I11. The AVX-512 gate is a detector, not a classifier — any hit aborts

**Why.** `dumpbin /disasm` is a **linear sweep** over the executable sections: it decodes whatever bytes it finds as instructions, including the data tables the compiler parks inside `.text`. `dimmy_lib.dll` is full of them — measured at `0x180DCD508`: a couple of 32-bit RVAs (`70 D4 DC 00` = `0x00DCD470`) followed by a long run of `08 08 08 …`. Nothing there is code, and the sweep renders it as instructions anyway. On `v0.7.3` one such blob came out as `vcvttph2ibs zmm7{k6},zmm5` and killed the STABLE release; `v0.7.3-rc.4` on the same commit reported 0. A real `-march=native` regression looks nothing like this: v0.6.71-rc10 carried **4820**, spread over hundreds of functions.

**Why rc and stable differ on the same commit.** `core/build.rs` `resolve_build_id()` reads the ambient `GITHUB_REF_NAME` and bakes it in as `DIMMY_BUILD_ID` (`sentry_pipeline.rs:29`). No workflow sets it, which is why it is absent from every printed `env:` block and why "the tag does not enter the Windows build" looks true and is not. `v0.7.3-rc.4` baked an 11-byte string, `v0.7.3` a 6-byte one; different constant, different layout, the sweep desyncs somewhere else. **The build is reproducible per build-id** — which is why re-running `v0.7.3` reproduced the identical hit at the identical address — and changing the tag rolls the dice again.

**What does NOT work. All four were built and measured; do not re-derive them.**

1. **Two decoders agreeing.** They are both deterministic linear sweeps over the same bytes, and x86 self-synchronises. Assemble `62 F5 7C 4E 68 FD` and **dumpbin 14.50, dumpbin 14.51 and llvm-objdump 22 all decode it as `vcvttph2ibs zmm7{k6},zmm5`**. Corroboration would have agreed on the `v0.7.3` hit and blocked the release anyway, while printing a more confident claim than it had earned.
2. **A hit threshold.** One small function compiled `/arch:AVX512` yields **8** zmm instructions — a genuine leak sails under any "a handful is noise" rule.
3. **`.pdata` containment.** Looks like a structural code-vs-data test and is not one: the data table above sits *inside* a function's unwind range.
4. **Re-decoding in phase from a `.pdata` function entry.** It reproduces the sweep byte for byte whenever the sweep was already in phase entering the function, which it usually is. The gate prints it as **evidence**, never as a verdict: "not a boundary" proves an artifact, "is a boundary" proves nothing either way.

**512-bit only, and that is load-bearing — not a gap.** `dimmy_lib.dll` legitimately *contains* AVX-512: the statically linked UCRT carries 10 EVEX instructions in the `snprintf` family (`vpmullq xmm1,xmm0,xmm7`, `vpminuq xmm1,xmm0,xmm2` — EVEX-only, no VEX encoding exists), CPUID-dispatched through `__isa_available` and never executed on a CPU without the feature. Widening the pattern to "any EVEX" or to k-mask forms **fails every build on Microsoft's own runtime**. 512-bit width is exactly what `/arch:AVX512` and `-march=native` emit and what the CRT's dispatched paths never use.

**How it works now.** [`scripts/dev/avx512-gate.ps1`](../../scripts/dev/avx512-gate.ps1), called from the **"Gate — reject AVX-512 in dimmy_lib.dll"** step of `release.yml`. In order: resolve dumpbin (missing toolchain is a hard fail, never a pass); parse image base / base of code / size of code, asserting every field; sweep with `/disasm` (**not** `:nobytes` — the raw encoding is the evidence); **assert the sweep reached >= 90% of the code section**; then count. Any hit aborts, and before aborting it stages the DLL plus `hits-context.txt`, which the next step uploads as an artifact.

That coverage assert closes the oldest hole here, older than any decoder dispute and the only one that could silently ship the crash: **a `dumpbin` that dies mid-file produces zero matches, and every earlier version of this gate read that as "clean".** Fault-injected on the real DLL: the truncated dump stops at `0x1801B73CE` against a required `0x181041B33` and is rejected.

**When it fires, read the bytes — then cut the next patch version.** Do not re-run (the build reproduces), do not raise a threshold, do not add a corroborator. A new tag changes `DIMMY_BUILD_ID`, changes the layout, and the artifact moves. That is the cheap escape; weakening the gate is not.

**Verify it locally before touching it** — it is a script, not an inline step, precisely so you can:

```powershell
# clean production DLL -> exit 0
pwsh scripts/dev/avx512-gate.ps1 -Dll E:\d\release\dimmy_lib.dll -EvidenceDir E:\tmp\ev
# fixture with genuine AVX-512 -> exit 1
cl /nologo /O2 /arch:AVX512 /LD avx.c && pwsh scripts/dev/avx512-gate.ps1 -Dll avx.dll -EvidenceDir E:\tmp\ev
```

**Never remove it.** It guards 0xc000001d on the first transcription for every user without AVX-512 (Alder Lake, Zen 2), which is what v0.6.71-rc9/rc10 shipped. Every other CI gate misses it because they all execute on the runner's own CPU.

**One real gap remains.** The gate scans only `dimmy_lib.dll`. The installer also ships `ggml.dll`, `ggml-base.dll`, `ggml-cpu.dll`, `ggml-vulkan.dll`, `llama*.dll` and `mtmd.dll`, and `ggml-cpu.dll` is exactly where a `-march=native` regression would land. All measured 0 locally, so `GGML_NATIVE=OFF` does reach them, but **CI does not look**.

---

## Pre-push checklist (any Windows-CI-touching change)

1. Did you grep for `windows-latest` in `build-windows` jobs? If yes → revert to `windows-2025` (I8).
2. Did you add or keep `exit 0` at the end of any pwsh step that runs a native installer? (I6)
3. Did you use `Add-Content -Encoding utf8` for GITHUB_ENV writes, not `>>`? (I7)
4. Did you use `vswhere -prerelease` for VS 2026 lookups and explicit `-version` ranges for VS 2022? (I1, I2)
5. If you changed `test-install.yml` bundle checks, did you mirror the change to BOTH branches? (I5)
6. Did you preserve the `dumpbin /headers` linker-version gate? (I1)
7. Is VS 2026 activation still scoped via `shell: cmd` + `call vcvars64.bat`, not leaking to other steps? (I3)
8. Is `--framework vcredist143-x64` still in the `vpk pack` command? (I10)
9. Did any `Select -First 1` sneak in without a preceding `Sort-Object`? (I9)
10. Did you keep the AVX-512 gate two-decoder, and resist "just re-run it" when it fires on a handful of hits? (I11)

Hitting any "no" above = high probability you're reintroducing a shipped bug. The corresponding CHANGELOG entry (v0.6.11–v0.6.20) describes the symptom; this file describes the cure.

---

## Adjacent doc

For the transcription-gap issue (test-install.yml only verifies 15 s startup — does NOT exercise `dimmy_stop_recording` with synthetic audio, which is why v0.6.10's FFI ABI mismatch shipped): tracked as a follow-up in [`../RELEASING.md`](../RELEASING.md). When you have bandwidth, extend `test-install.yml` to feed a silent WAV through the FFI before declaring green.
