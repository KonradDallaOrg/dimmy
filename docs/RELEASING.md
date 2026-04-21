# Releasing

> **Runbook for cutting a release.** For build commands, see [`BUILD.md`](BUILD.md). For Windows CI invariants, see [`dev/windows-ci.md`](dev/windows-ci.md) — read it before touching any workflow.

## Versioning

Semantic versioning. Bump in `core/Cargo.toml` → `version = "x.y.z"`.

- **patch** (0.6.20 → 0.6.21): bug fixes, CI tweaks, no behaviour change for end users
- **minor** (0.6.x → 0.7.0): new feature (still backward compatible with existing configs)
- **major** (0.x.y → 1.0.0): reserved for v1.0 MVP ship

## Branches

- `staging` — active development; every push triggers `staging-native.yml` → `staging-latest` pre-release
- `main` — tracks the last published release; tag here

`feat/native-ui` is the current default branch on origin (historical; may be merged away in the future).

## Step-by-step

1. **Verify clean working tree on `staging`.**
   ```bash
   git status
   git log --oneline origin/staging..staging   # should be empty or only the commits you're about to ship
   ```

2. **Run the pre-push checklist locally** (from [`BUILD.md`](BUILD.md#core-rust--everyone-runs-these)):
   ```bash
   cd core
   cargo fmt --check
   cargo clippy --features local-stt,local-llm -- -D warnings
   cargo test --lib --features local-stt,local-llm
   ```

3. **Bump version in `core/Cargo.toml`.** Commit with message `chore: bump to vX.Y.Z`.

4. **Update `CHANGELOG.md`.** Move items from `[Unreleased]` to a new `[X.Y.Z] - YYYY-MM-DD` block. Use the Keep a Changelog sections: `### Fixed`, `### Added`, `### Changed`. For each line, write the **root cause**, not just the symptom — that's what makes the changelog useful when a future bug looks familiar.

5. **Push `staging`.**
   ```bash
   git push origin staging
   ```
   `staging-native.yml` runs (~15 min). Wait for it green before tagging. If `test-install.yml` fails, the installer is broken — **do not tag**. Fix first.

6. **Fast-forward `main` to match `staging`** (assuming staging is a superset of main):
   ```bash
   git push origin staging:main
   ```
   If non-fast-forward, `main` has something staging doesn't. Investigate before force-pushing.

7. **Tag and push the tag.**
   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```
   `release.yml` runs. Produces signed Windows Setup.exe (Velopack), macOS DMG, Linux AppImage. Publishes a GitHub Release.

8. **Verify the installer.** Download `Dimmy-Setup.exe` from the release, run it on a clean Windows machine (or a VM snapshot). Launch the app, hit the hotkey, speak, confirm the text pastes. Same smoke test on macOS and Linux if possible.

9. **Update users.** The app's update check polls GitHub Releases; users will see the notification from Settings → About within 24 hours.

## What `test-install.yml` does (automated smoke test)

After every staging / release build, this job downloads the Windows Setup.exe from the upload artifact, installs it silently on a fresh `windows-latest` runner that has **no WinAppSDK and no VC++ Redist preinstalled**, launches the app for 15 seconds, then fails if:

- `dimmy_startup.log` contains `CRASH`
- `resources.pri` is missing from the install folder (signals MrtCore build regression — see [`dev/windows-ci.md`](dev/windows-ci.md) I2)
- Critical files are missing from the bundle (whitelist in `test-install.yml`; **never** list `vcruntime140.dll` / `msvcp140.dll` — Velopack's `--framework vcredist143-x64` handles those via System32)

**Known gap:** the test does not yet exercise `dimmy_stop_recording` with synthetic audio. v0.6.10 shipped an ABI mismatch that only triggered on first transcription. Before the next release cycle, extend `test-install.yml` to feed a silent WAV through the FFI and assert no crash.

## Rolling back

If a release is broken after publication:

1. **Delete the GitHub Release** (mark as draft) to stop the auto-updater from pushing it to users.
2. **Do not delete the tag.** The tag stays as a marker of what shipped (and what's broken). CHANGELOG records the breakage.
3. Cut the next patch with the fix. Users who already updated will get the patch on the next auto-check.

## Release cadence

Historically, Dimmy releases in bursts (v0.6.11 → v0.6.20 shipped in two days resolving the MSVC toolchain saga). When chasing a bug across CI iterations, **each version bump is cheap** — better to ship 9 incremental fixes with clear CHANGELOG entries than one mega-commit with no trail. The changelog is the archaeology.
