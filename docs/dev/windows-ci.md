# Windows CI — 10 invariants (read before editing any workflow)

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

Hitting any "no" above = high probability you're reintroducing a shipped bug. The corresponding CHANGELOG entry (v0.6.11–v0.6.20) describes the symptom; this file describes the cure.

---

## Adjacent doc

For the transcription-gap issue (test-install.yml only verifies 15 s startup — does NOT exercise `dimmy_stop_recording` with synthetic audio, which is why v0.6.10's FFI ABI mismatch shipped): tracked as a follow-up in [`../RELEASING.md`](../RELEASING.md). When you have bandwidth, extend `test-install.yml` to feed a silent WAV through the FFI before declaring green.
