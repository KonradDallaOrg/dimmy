# Releasing

> **Runbook for cutting a release.** For build commands, see [`BUILD.md`](BUILD.md). For Windows CI invariants, see [`dev/windows-ci.md`](dev/windows-ci.md) — read it before touching any workflow.

## Release pipelines — what triggers what

Three workflows publish artifacts. They look similar but produce binaries that **speak to different licensing endpoints and Stripe accounts**. Pick the wrong trigger and you ship a binary that bills real money in Stripe Live when you meant Stripe Test (or vice versa).

| Workflow | Trigger | flavor | `DIMMY_LICENSE_PUBKEY` source | server URL | packId | config dir | Stripe |
|---|---|---|---|---|---|---|---|
| **`staging-native.yml`** | push to `staging` branch | `staging` | hardcoded inline (`avlM65...`) | **license-staging**.dimmy.app | `Dimmy` (= prod) | `dimmy` (= prod) | Test |
| **`staging-release.yml`** | tag matching `v*-staging*` (e.g. `v0.6.46-staging.1`) | `staging` | hardcoded inline (`avlM65...`) | **license-staging**.dimmy.app | `Dimmy-Staging` (separate) | `dimmy-staging` (via `DIMMY_CONFIG_NAMESPACE`) | Test |
| **`release.yml`** | tag matching `v*` that does NOT contain `-staging` (e.g. `v0.6.46-rc1` OR `v0.6.46`) | prod (env unset → empty) | `${{ secrets.DIMMY_LICENSE_PUBKEY }}` | **license**.dimmy.app | `Dimmy` | `dimmy` | **Live** |

### Pick-the-right-trigger cheat sheet

> Match what you're trying to accomplish to a row. If none of these fit, **stop and re-read** before tagging anything.

| Goal | Trigger to use | How |
|---|---|---|
| Internal smoke build after a `staging` merge (no auto-update visible to users) | `staging-native.yml` | Push to `staging`. Asset URL: `staging-latest`. Download `Dimmy-win-Setup.exe` manually. **Velopack won't see this build** — tag is rolling, not semver. |
| Ship a pre-release to **prerelease channel** users (Stripe Live, real billing) | `release.yml` | `git tag -a v0.6.46-rc1 -m '...'` + `git push origin v0.6.46-rc1`. GitHub Release is marked `prerelease=true`. **Trial / activation-code free in both modes — only "Buy plan" charges.** |
| Ship a stable release to **all** users (channel-stable + prerelease) | `release.yml` | Tag `v0.6.46` (no `-rcN` suffix). GitHub Release is marked `prerelease=false`, becomes "Latest". |
| Test the full pay flow against Stripe Test (Buy → checkout → webhook → license active), side-by-side with a prod install | `staging-release.yml` | `git tag -a v0.6.46-staging.1 -m '...'` + push. Produces installer that lives in `Local\Dimmy-Staging\` + reads `Roaming\dimmy-staging\`. Both packId and config dir are separate from prod. Doesn't touch the prod install. |
| Reproduce a bug against the staging licensing endpoint without going through a CI build | Local Debug build | Set `DIMMY_LICENSE_PUBKEY=avlM65... DIMMY_LICENSE_SERVER_URL=https://license-staging.dimmy.app DIMMY_BUILD_FLAVOR=staging`, then `cargo build --release --lib --features local-stt-vulkan,local-stt-parakeet,local-llm-vulkan,license-client`. Drop the DLL into the Debug bin and run from there. Same caveat as `staging-native.yml`: packId stays prod, install dir is prod — your debug runs share license.json with prod. |

### Why this matters (and what the failure modes look like)

- **A `v*-rcN` tag = PROD endpoint.** This caught us once already (2026-05-16): tag `v0.6.46-rc1` looks "rc-y" but `release.yml` fires, builds against `secrets.DIMMY_LICENSE_PUBKEY`, points to `license.dimmy.app`, and any "Buy plan" click in that binary hits Stripe Live. We cancelled the run + deleted the draft release before it was published. For RC binaries that exercise paid flows, use `staging-release.yml`'s `v*-staging.N` tag instead, OR restrict RC testing to trial/activation-code paths (free in both modes).
- **`staging-latest` rolling tag is invisible to Velopack** because the tag name isn't valid semver. `staging-native.yml` produces useful artifacts for manual sideloading, but no in-app auto-update will deliver them.
- **`license-client` cargo feature is mandatory** in every shipping pipeline. All three workflows pass `--features ...,license-client` explicitly. Without it the Rust core short-circuits to `LicenseStatus::Unrestricted` regardless of the embedded pubkey, and the binary ships with the "DEV / Source build (no licensing)" badge + every scope unlocked. A fresh contributor `cargo build` (no env) deliberately does NOT enable `license-client` so contributors can build without `DIMMY_LICENSE_PUBKEY`.
- **Flavor ≠ config dir since 2026-05-16.** The config dir is keyed off `DIMMY_CONFIG_NAMESPACE` (default `dimmy`), not `DIMMY_BUILD_FLAVOR`. A flavor=staging build that ships under the prod packId (the `staging-native.yml` case) shares the prod `Roaming\dimmy\` config dir so a channel-prerelease auto-update doesn't appear to wipe the user's data. Only `staging-release.yml` sets `DIMMY_CONFIG_NAMESPACE=dimmy-staging`. The C# host learns this at runtime via the `dimmy_config_dir_name()` FFI — **never derive the dir from the flavor in host code**.

### How to test the licensing flow without spending money

- **Trial + magic link + `/api/activate?code=...` redemption** — free in both Stripe Test and Live. Safe to exercise on any binary (prod RC included).
- **Buy / Checkout / subscription created / `/api/refresh`** — bills real money in Stripe Live. Use only `staging-release.yml` builds (or local Debug with staging env), which point to `license-staging.dimmy.app` + Stripe Test.
- **Stripe Customer Portal** — same: free to open, but any "Update payment method" / "Cancel subscription" interaction touches whatever account the binary's endpoint says.

### See also

- [`dev/licensing-flow.md`](dev/licensing-flow.md) — state machine + sequence diagrams (the ground-truth doc for what each `/api/...` endpoint does).
- [`dev/staging-testing.md`](dev/staging-testing.md) — the tester-facing guide for someone who installed the `Dimmy-Staging` build (Stripe test cards, expected watermarks, side-by-side caveats).
- [`dev/licensing-prod.md`](dev/licensing-prod.md) — Cloudflare Worker + Stripe production setup notes (for when a real prod tier needs server-side changes).
- [`dev/licensing-poc.md`](dev/licensing-poc.md) — original PoC: local axum server, 7 test scenarios, design rationale. Useful when changing the Ed25519 envelope shape.

---


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

## Mac auto-update bootstrap (one-time setup)

Sparkle 2 verifies every downloaded DMG with an EdDSA signature embedded in `appcast.xml`. The matching public key lives in `platforms/macos/Dimmy/Info.plist` (`SUPublicEDKey`). Until the keypair is generated and the GitHub secret is set, the workflow ships the DMG but skips signing — auto-update will not function for those releases.

**One-time bootstrap:**

1. **Resolve the Sparkle package locally** so `generate_keys` is available on disk:
   ```bash
   xcodebuild -project platforms/macos/Dimmy.xcodeproj -resolvePackageDependencies
   ```

2. **Generate the keypair.** The binary lives under SwiftPM's resolved checkout:
   ```bash
   find platforms/macos/build/derived/SourcePackages -name generate_keys -type f -perm +111
   # → run it: prints public key, writes private key to Keychain
   ./<that-path>/generate_keys
   ```
   Output is something like:
   ```
   A pair of keys was just created and is now stored in your Keychain.
   In your Info.plist, set the SUPublicEDKey key to:
     <base64-encoded-public-key-32-chars>
   ```

3. **Set `SUPublicEDKey` in `platforms/macos/Dimmy/Info.plist`** — replace `REPLACE_ME_WITH_SPARKLE_PUBLIC_ED_KEY` with the printed value. Commit that change.

4. **Export the private key for CI:**
   ```bash
   find platforms/macos/build/derived/SourcePackages -name generate_keys -type f -perm +111 -exec dirname {} \; | head -1
   # cd into that dir and:
   ./generate_keys -x
   # → prints the base64 private key
   ```
   Set it as a GitHub repository secret named `SPARKLE_PRIVATE_KEY` (Settings → Secrets and variables → Actions → New repository secret). The key **never** leaves Keychain on your dev machine — the secret is the one-time export you paste into the GitHub UI.

5. **Cut a normal release.** From this point on, every `release.yml` run signs the DMG, writes `appcast.xml`, and uploads both. Mac auto-update is live.

### Channel behaviour

GitHub releases marked "prerelease" produce an appcast with `<sparkle:channel>prerelease</sparkle:channel>`. The Mac app's About page exposes a Stable / Prerelease picker; "Stable" users skip prerelease items, "Prerelease" users get both.

## Rolling back

If a release is broken after publication:

1. **Delete the GitHub Release** (mark as draft) to stop the auto-updater from pushing it to users.
2. **Do not delete the tag.** The tag stays as a marker of what shipped (and what's broken). CHANGELOG records the breakage.
3. Cut the next patch with the fix. Users who already updated will get the patch on the next auto-check.

## Release cadence

Historically, Dimmy releases in bursts (v0.6.11 → v0.6.20 shipped in two days resolving the MSVC toolchain saga). When chasing a bug across CI iterations, **each version bump is cheap** — better to ship 9 incremental fixes with clear CHANGELOG entries than one mega-commit with no trail. The changelog is the archaeology.
