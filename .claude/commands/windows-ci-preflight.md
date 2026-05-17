---
description: Verify the 6 text-checkable Windows CI invariants (I4, I5, I7, I8, I9, I10) before pushing a workflow change. The other 4 (I1, I2, I3, I6) require a Windows runner.
allowed-tools: Bash
---

Run each check and report OK or FAIL with the invariant ID from `docs/dev/windows-ci.md`.

**I8** — `windows-2025` runner in `build-windows` jobs:
```
grep -n 'runs-on: windows-latest' .github/workflows/release.yml .github/workflows/staging-auto-update.yml || echo OK
```
Any match = FAIL (test-install.yml is allowed to use `windows-latest`; do not grep it).

**I7** — no `>> $env:GITHUB_ENV` redirects (they default to UTF-16):
```
grep -rn '>> *\$env:GITHUB_ENV\|>> *\$GITHUB_ENV' .github/workflows/ || echo OK
```
Any match = FAIL. Use `Add-Content -Encoding utf8` instead.

**I9** — `Select -First 1` must be preceded by `Sort-Object`:
```
grep -rn -B1 'Select-Object -First 1\|Select -First 1' .github/workflows/ platforms/windows/ | grep -v 'Sort-Object' || echo OK
```
Any match = FAIL (print the offending line).

**I10** — `--framework vcredist143-x64` present in `vpk pack`:
```
grep -n 'vcredist143-x64' .github/workflows/release.yml .github/workflows/staging-auto-update.yml
```
Must match in BOTH files. Missing in either = FAIL.

**I4** — no co-located `vcruntime140.dll` / `msvcp140.dll`:
```
grep -rn 'vcruntime140\.dll\|msvcp140\.dll' platforms/windows/verify-self-contained.ps1 .github/workflows/test-install.yml || echo OK
```
Any match = FAIL. VC Redist must be delegated to Velopack (I10).

**I5** — `test-install.yml` must match between `main` and `staging`:
```
git fetch origin main staging --quiet 2>/dev/null; diff <(git show origin/main:.github/workflows/test-install.yml 2>/dev/null) <(git show origin/staging:.github/workflows/test-install.yml 2>/dev/null)
```
Empty diff = OK. Any diff = WARN (mirror per I5).

Print a single-line summary: `I4 ok | I5 ok | I7 ok | I8 ok | I9 ok | I10 ok`. For any FAIL, cite the invariant section of `docs/dev/windows-ci.md` and point at the offending line.

Note: I1, I2, I3, I6 require a Windows runner and gate at CI time — they are NOT checked here.
