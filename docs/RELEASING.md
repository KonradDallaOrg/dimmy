# Releasing

> **Runbook for cutting a release.** For build commands, see [`BUILD.md`](BUILD.md). For Windows CI invariants, see [`dev/windows-ci.md`](dev/windows-ci.md) — read it before touching any workflow.

## Release pipelines — what triggers what

Three workflows publish the app. (Two more create releases of their own: `agent-preview.yml` on `agent-preview.*` tags, a macOS-only test build with staging licensing, and `engine-nemotron.yml` on `engine-nemotron-*` tags, the Nemotron engine libraries. Neither is an app release, and `v*` tags never match them.) They look similar but produce binaries that **speak to different licensing endpoints and Stripe accounts**. Pick the wrong trigger and you ship a binary that bills real money in Stripe Live when you meant Stripe Test (or vice versa).

| Workflow | Trigger | flavor | `DIMMY_LICENSE_PUBKEY` source | server URL | packId / Velopack channel | config dir | Stripe |
|---|---|---|---|---|---|---|---|
| **`staging-auto-update.yml`** | push to `staging` branch (not for docs-only pushes), or manual dispatch | `staging` | hardcoded inline (`avlM65...`) | **license-staging**.dimmy.app | `Dimmy-Staging` / channel `staging` (version `X.Y.Z-staging.<run_number>`) | `dimmy-staging` (via `DIMMY_CONFIG_NAMESPACE`) | Test |
| **`staging-tester.yml`** | tag matching `v*-staging*` (e.g. `v0.7.15-staging.1`) | `staging` | `${{ secrets.DIMMY_LICENSE_PUBKEY_STAGING }}` | **license-staging**.dimmy.app | `Dimmy-Staging` / channel `staging` | `dimmy-staging` (via `DIMMY_CONFIG_NAMESPACE`) | Test |
| **`release.yml`** | tag matching `v*` that does NOT contain `-staging` (e.g. `v0.7.15-rc.1` OR `v0.7.15`) | prod (env unset → empty) | `${{ secrets.DIMMY_LICENSE_PUBKEY }}` | **license**.dimmy.app | `Dimmy` / default channel | `dimmy` | **Live** |

Both staging pipelines use packId `Dimmy-Staging` and channel `staging`, so a prod `Dimmy` install can never see a staging build and the other way round (the Velopack GitHub source filters releases by packId and channel). The two differ in what they ship: `staging-auto-update.yml` is the rolling build that staging installs auto-update to; `staging-tester.yml` is the signed, tag-pinned snapshot.

### Pick-the-right-trigger cheat sheet

> Match what you're trying to accomplish to a row. If none of these fit, **stop and re-read** before tagging anything.

| Goal | Trigger to use | How |
|---|---|---|
| Internal smoke build after a `staging` merge (rolling, staging installs only) | `staging-auto-update.yml` | Push to `staging`. Asset URL: `staging-latest`. Download `Dimmy-Staging-staging-Setup.exe` manually, or let an installed `Dimmy-Staging` auto-update (packId `Dimmy-Staging`, channel `staging`). **Prod `Dimmy` installs never see this build** — different packId and channel. |
| Ship a pre-release to **prerelease channel** users (Stripe Live, real billing) | `release.yml` | `git tag -a v0.7.15-rc.1 -m '...'` + `git push origin v0.7.15-rc.1`. **The dot in `-rc.1` is load-bearing — see "Tag shape" below.** GitHub Release is marked `prerelease=true`. **Trial / activation-code free in both modes — only "Buy plan" charges.** |
| Ship a stable release to **all** users (channel-stable + prerelease) | `release.yml` | Tag `v0.7.15` (no `-rc.N` suffix). It needs a published `v0.7.15-rc.N` first (promotion ladder, see "Tag shape"). GitHub Release is marked `prerelease=false`; stable tags also publish the signed Setup.exe to `dl.dimmy.app`. |
| Test the full pay flow against Stripe Test (Buy → checkout → webhook → license active), side-by-side with a prod install | `staging-tester.yml` | `git tag -a v0.7.15-staging.1 -m '...'` + push. Produces installer that lives in `Local\Dimmy-Staging\` + reads `Roaming\dimmy-staging\`. Both packId and config dir are separate from prod. Doesn't touch the prod install. |
| Reproduce a bug against the staging licensing endpoint without going through a CI build | Local Debug build | Set `DIMMY_LICENSE_PUBKEY=avlM65... DIMMY_LICENSE_SERVER_URL=https://license-staging.dimmy.app DIMMY_BUILD_FLAVOR=staging DIMMY_CONFIG_NAMESPACE=dimmy-staging`, then `cargo build --release --lib --features local-stt-vulkan,local-stt-parakeet,local-llm-vulkan,denoise-gtcrn,local-stt-qwen,license-client`. Drop the DLL into the Debug bin and run from there. Without `DIMMY_CONFIG_NAMESPACE` the config dir defaults to `dimmy` — your debug runs share `license.json` with a prod install. |

### Tag shape: `-rc.N`, with the dot

SemVer compares a pre-release identifier containing letters character by
character, so `rc10` is `'1'` against `'8'` and **`0.7.0-rc10` ranks BELOW
`0.7.0-rc8`**. Published, downloadable, and invisible: every updater keeps
offering rc8. The dot makes `10` its own all-digit identifier, which SemVer
compares numerically.

Burned twice — `v0.6.73` reached `rc11`, the fix (dots) was applied to the
v0.6.74 line and never documented, and `v0.7.0` went back to `rcN` and shipped
rc10 and rc11 to nobody. Once a line has passed nine without dots, no tag
inside it can outrank the earlier ones: bump the patch instead.

`scripts/check-release-tag.sh <tag>` checks both shape and ordering, and
`release.yml` and `staging-tester.yml` run it (step "Refuse a tag SemVer ranks
below a published release" of `create-release`) before building.
`scripts/check-release-version.sh` does NOT cover this: it compares only the
base version of `core/Cargo.toml` with the git tags and GitHub releases, and the
pre-commit hook runs it when `core/Cargo.toml` is staged.

Each track is ranked only against itself: a staging tag against published
staging releases, an rc or stable tag against published prod releases (rc and
stable share the prod packId). Staging and prod never compete, so publishing
`0.7.12-staging.1` does not block `0.7.12-rc.1`, even though SemVer puts
"rc" below "staging". On top of that, a promotion ladder: staging may run
ahead, an rc's X.Y.Z may not exceed the highest staging X.Y.Z, and a stable
X.Y.Z needs a published X.Y.Z-rc.N. Only the three shapes `vX.Y.Z`,
`vX.Y.Z-rc.N`, `vX.Y.Z-staging.N` are accepted. Tests:
`bash scripts/ci/test_check_release_tag.sh`.


### Why this matters (and what the failure modes look like)

- **A `v*-rc.N` tag = PROD endpoint.** This caught us once already (2026-05-16, when tags were still `-rcN`): tag `v0.6.46-rc1` looks "rc-y" but `release.yml` fires, builds against `secrets.DIMMY_LICENSE_PUBKEY`, points to `license.dimmy.app`, and any "Buy plan" click in that binary hits Stripe Live. We cancelled the run + deleted the draft release before it was published. For RC binaries that exercise paid flows, use `staging-tester.yml`'s `v*-staging.N` tag instead, OR restrict RC testing to trial/activation-code paths (free in both modes).
- **`staging-auto-update.yml` is a separate Velopack track, not a leak into prod.** It packs packId `Dimmy-Staging` on channel `staging`, with a monotonic version `X.Y.Z-staging.<run_number>`, and uploads the full Velopack set (`Setup.exe`, `Portable.zip`, `Dimmy-Staging-*-full.nupkg`, `releases*.json`) to the rolling `staging-latest` release. A staging install auto-updates across that channel; the prod `Dimmy` client filters by packId and channel and never discovers these files. The old setup (staging build under the prod packId) leaked into the prod prerelease channel on 2026-06-16 — never ship a `flavor=staging` build under packId `Dimmy`. (Mac/Sparkle staging is not on this model: `staging-auto-update.yml`'s DMG is a manual download.)
- **`license-client` cargo feature is mandatory** in every shipping pipeline. All three workflows pass `license-client` explicitly in every platform build they run (the Windows and macOS `--features` lists, and the Linux `--features license-client`). Without it the Rust core short-circuits to `LicenseStatus::Unrestricted` regardless of the embedded pubkey, and the binary ships with the "DEV / Source build (no licensing)" badge + every scope unlocked. A fresh contributor `cargo build` (no env) deliberately does NOT enable `license-client` (it is not in `default`) so contributors can build without `DIMMY_LICENSE_PUBKEY`.
- **Flavor ≠ config dir since 2026-05-16.** The config dir is keyed off `DIMMY_CONFIG_NAMESPACE` (default `dimmy`), not `DIMMY_BUILD_FLAVOR`. Both staging workflows (`staging-auto-update.yml` and `staging-tester.yml`) set `DIMMY_CONFIG_NAMESPACE=dimmy-staging`, paired with packId `Dimmy-Staging`; `release.yml` leaves it unset (config dir `dimmy`). The C# host learns the dir at runtime via the `dimmy_config_dir_name()` FFI — **never derive the dir from the flavor in host code**.

### How to test the licensing flow without spending money

- **Trial + magic link + `/api/activate?code=...` redemption** — free in both Stripe Test and Live. Safe to exercise on any binary (prod RC included).
- **Buy / Checkout / subscription created / `/api/refresh`** — bills real money in Stripe Live. Use only `staging-tester.yml` builds (or local Debug with staging env), which point to `license-staging.dimmy.app` + Stripe Test.
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

- `staging` — active development; every push (except docs-only pushes: `docs/**`, `mockup/**`, `*.md`, `CLAUDE.md`) triggers `staging-auto-update.yml` → `staging-latest` pre-release
- `main` — tracks the last published release. `-staging.N` and `-rc.N` tags are cut on the `staging` tip (for example `v0.7.15-rc.1`); the stable tag goes on the same commit as its final rc (`v0.7.14` and `v0.7.14-rc.5` share one commit), with `main` fast-forwarded to it

The `feat/native-ui` branch no longer exists on origin, and `origin/HEAD` points at `main`.

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
   cargo test --release --test ffi_e2e --features local-stt,test-ffi -- --test-threads=1
   ```

3. **Bump version in `core/Cargo.toml`.** Run the three version checks first (see `CLAUDE.md` "Versioning"). Commit with message `chore(release): bump to X.Y.Z` (the form in the git history, for example `chore(release): bump to 0.7.15`). When `core/Cargo.toml` is staged, the pre-commit hook (`./scripts/install-hooks.sh` sets it up) runs `scripts/check-release-version.sh`.

4. **Update `CHANGELOG.md`.** Move items from `[Unreleased]` to a new `[X.Y.Z] - YYYY-MM-DD` block. Use the Keep a Changelog sections: `### Fixed`, `### Added`, `### Changed`. For each line, write the **root cause**, not just the symptom — that's what makes the changelog useful when a future bug looks familiar.

5. **Push `staging`.**
   ```bash
   git push origin staging
   ```
   `staging-auto-update.yml` runs (it ignores docs-only pushes, and a newer push cancels an older run). Wait for it green before tagging. If its `test-install` job fails, the installer is broken — **do not tag**. Fix first.

6. **Fast-forward `main` to match `staging`** (assuming staging is a superset of main):
   ```bash
   git push origin staging:main
   ```
   If non-fast-forward, `main` has something staging doesn't. Investigate before force-pushing.

7. **Tag and push the tags, in ladder order.** `scripts/check-release-tag.sh` (run by `release.yml` and `staging-tester.yml` before they build) accepts only `vX.Y.Z-staging.N`, `vX.Y.Z-rc.N` and `vX.Y.Z`; an rc needs a staging tag with the same or a higher X.Y.Z, and a stable tag needs a published `vX.Y.Z-rc.N`. So a stable release is the third tag of a line:
   ```bash
   git tag -a vX.Y.Z-staging.1 -m '...'   # staging-tester.yml (Stripe Test) — skip if the line already has one
   git tag -a vX.Y.Z-rc.1 -m '...'        # release.yml, prerelease channel
   git tag -a vX.Y.Z -m '...'             # release.yml, stable
   git push origin <tag>                  # one tag per push
   ```
   **Before you push a `v*` tag, the self-hosted runner `dimmy-sign` must be online and SimplySign Desktop must be open and logged in** (the `scripts/git-hooks/pre-push` hook asks you to confirm). `release.yml` creates the GitHub Release as a draft, builds all three platforms, signs the Windows binaries in the `sign-windows` job on that runner, and flips the release to public only in `publish-release`, after every job (including `test-install` and the macOS launch check) is green. If `sign-windows` fails, the draft stays unpublished: log into SimplySign and re-run the failed jobs.

8. **Verify the installer.** Download `Dimmy-win-Setup.exe` from the release, run it on a clean Windows machine (or a VM snapshot). Launch the app, hit the hotkey, speak, confirm the text pastes. Same smoke test on macOS and Linux if possible.

9. **Update users.** Windows installs re-check GitHub Releases through Velopack every 6 hours (`UpdateService.ReCheckInterval`); the Mac app checks the Sparkle appcast every 6 hours (21600 s) while the `auto_update` license scope is active (`UpdateService.swift`). Users see the notification from Settings → About after that.

## What `test-install.yml` does (automated smoke test)

`staging-auto-update.yml` and `release.yml` call this reusable workflow (`workflow_call`) after the Windows build; `staging-tester.yml` does not. The job downloads the Windows Setup.exe from the release tag (`gh release download`, drafts included; `setup_pattern` / `install_dir` inputs default to the prod `Dimmy-win-Setup.exe` / `Dimmy`, and `staging-auto-update.yml` passes the `Dimmy-Staging` names), installs it silently on a fresh `windows-latest` runner that is expected to have **no WinAppSDK runtime and no VC++ Redist preinstalled**, launches the app for 15 seconds, then fails if:

- `dimmy_startup.log` contains `CRASH`
- `%APPDATA%\dimmy\crash.log` exists (the UI crashed but the process stayed alive) or the app exits before the 15 seconds are over
- `resources.pri` is missing from the install folder (signals MrtCore build regression — see [`dev/windows-ci.md`](dev/windows-ci.md) I2)
- Critical files are missing from the bundle (whitelist in `test-install.yml`; **never** list `vcruntime140.dll` / `msvcp140.dll` — Velopack's `--framework vcredist143-x64` handles those via System32)

**Known gap:** the test does not yet exercise `dimmy_stop_recording` with synthetic audio. v0.6.10 shipped an ABI mismatch that only triggered on first transcription. Before the next release cycle, extend `test-install.yml` to feed a silent WAV through the FFI and assert no crash. (Partial cover elsewhere: when the core DLL is rebuilt, `release.yml`'s `build-windows` runs `tests/ffi_e2e` on a real whisper transcription, step "Gate — whisper transcription smoke test", against a `test-ffi` build, not the installed DLL.)

## Mac auto-update bootstrap (one-time setup)

Sparkle 2 verifies every downloaded DMG with an EdDSA signature embedded in `appcast.xml`. The matching public key lives in `platforms/macos/Dimmy/Info.plist` (`SUPublicEDKey`). If the `SPARKLE_PRIVATE_KEY` GitHub secret is not set, `release.yml` ships the DMG but skips signing and the appcast — auto-update will not function for that release.

**The bootstrap is already done for this repo:** `Info.plist` holds a real `SUPublicEDKey` (its comment says it was set on 2026-05-12) and `release.yml` reads `SPARKLE_PRIVATE_KEY`. The steps below are for creating a new keypair (a new fork, or a key rotation); in step 3 you then replace the existing `SUPublicEDKey` value instead of a placeholder.

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

3. **Set `SUPublicEDKey` in `platforms/macos/Dimmy/Info.plist`** — put the printed value in the `<string>` after the `SUPublicEDKey` key. Commit that change.

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

A release tag that contains a `-` (an `-rc.N` tag; the same test sets the GitHub `prerelease` flag) produces an appcast item with `<sparkle:channel>prerelease</sparkle:channel>`; a stable tag produces an item with no channel tag. The feed always carries the newest item of each channel. The Mac app's About page exposes a Stable / Prerelease picker; "Stable" users skip prerelease items, "Prerelease" users get both.

## A half-finished run leaves the Mac feed pointing at a draft

**The Mac appcast is published by `build-macos`, which runs BEFORE `publish-release`.** The "Mirror appcast.xml to durable auto-update channel" step `--clobber`s `releases/download/auto-update/appcast.xml` — the fixed URL every installed Mac reads — the moment the Mac job goes green. The DMG it advertises, though, lives in the release, and the release is still a **draft** until `publish-release` runs at the very end.

So between those two moments the production feed advertises a version whose download returns **HTTP 404**, and every Mac user on the stable channel who checks for updates in that window gets a failed update. If the run never reaches `publish-release` — a red Windows job, say — **it stays broken indefinitely, silently.**

Burned 2026-09-16: `v0.7.3` died on the AVX-512 gate (I11) after the Mac job had already published. The prod feed advertised `v0.7.3` against a draft release; every Mac stable user 404'd until the feed was restored by hand. Nothing in the run was red except Windows, and nothing anywhere said users were affected.

**Check it whenever a release run does not finish green:**

```bash
curl -fsSL https://github.com/KonradDallaOrg/dimmy/releases/download/auto-update/appcast.xml \
  | grep -oE 'url="[^"]*\.dmg"'
# then curl -sS -o /dev/null -w '%{http_code}\n' -L <each url>   # anything but 200 is a live outage
```

**Restore it from the last good release:**

```bash
gh release download v0.7.2 -p appcast.xml -D /tmp     # last tag that actually published
gh release upload auto-update /tmp/appcast.xml --clobber
```

Nothing else is needed afterwards: `scripts/ci/build_appcast.py` always **replaces** the slot of the channel it is publishing and carries over only the other channel's newest item, so the next successful Mac build drops the broken entry on its own.

The durable fix is to mirror the appcast from `publish-release` instead of from `build-macos`, so the feed can never name a draft. Until that lands, treat the check above as part of every failed-release post-mortem.

## Rolling back

If a release is broken after publication:

1. **Delete the GitHub Release** (mark as draft) to stop the auto-updater from pushing it to users. **Restore the Mac appcast in the same breath** — drafting the release is exactly what makes its DMG 404 while the feed still advertises it (see the section above). Windows is safe here: Velopack reads `releases.win.json` from the release itself, so drafting it simply stops offering the update.
2. **Do not delete the tag.** The tag stays as a marker of what shipped (and what's broken). CHANGELOG records the breakage.
3. Cut the next patch with the fix. Users who already updated will get the patch on the next auto-check.

## Release cadence

Historically, Dimmy releases in bursts (v0.6.11 → v0.6.20 shipped in two days resolving the MSVC toolchain saga). When chasing a bug across CI iterations, **each version bump is cheap** — better to ship 9 incremental fixes with clear CHANGELOG entries than one mega-commit with no trail. The changelog is the archaeology.
