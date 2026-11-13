# macOS Development Loop

## Why `/Applications/Dimmy.app` matters for dev

macOS TCC (Transparency, Consent, Control — the permissions database behind
Privacy & Security) keys its records on the combination of team ID, bundle
identifier, and the on-disk code signature. When you rebuild Dimmy from Xcode
and launch from `~/Library/Developer/Xcode/DerivedData/.../Dimmy.app`, that
path is stable *enough*, but the code signature hash changes on every build.
For certain TCC entries (Accessibility, Input Monitoring) the kernel compares
the running binary's code directory against the recorded one; a mismatch
presents as "I granted it in System Settings but the app still sees it as
denied".

Always test from `/Applications/Dimmy.app`, rebuilt in place via the install
script below. Path stability is *necessary but not sufficient*: when ad-hoc
signing (`codesign --sign -`, as the install script does) the signature still
depends on the binary hash, so **changing source code invalidates the TCC
grant** even though the path is the same. If you see "grants look good in
Settings but Dimmy acts like they're missing", run the TCC reset below and
re-grant once for the new binary. Stable grants across builds would require a
proper Developer ID cert.

## The script

```
scripts/macos/install-to-applications.sh           # Debug build
scripts/macos/install-to-applications.sh --release # Release build
```

It builds with `xcodebuild`, bundles the llama/ggml dylibs from
`core/target/aarch64-apple-darwin/release` into the app's `Contents/Frameworks`
(skipped when there are none), `rm -rf`'s `/Applications/Dimmy.app`, copies the
fresh bundle, re-signs it ad hoc (`codesign --force --deep --sign -`), verifies
the code signature (`codesign --verify --deep --strict`), and launches the app.
The script does not run the launch-time `SelfTests`; for that use
`scripts/dev/preflight-mac.sh` (see `CLAUDE.md`).

## First-run TCC reset

If you want a truly clean state (e.g., to test the onboarding from scratch):

```
tccutil reset Microphone com.dimmy.app
tccutil reset Accessibility com.dimmy.app
tccutil reset ListenEvent com.dimmy.app
defaults delete com.dimmy.app isOnboardingComplete 2>/dev/null || true
rm -f "$HOME/Library/Application Support/dimmy/.onboarding_done"
```

Onboarding completion lives in the core's `.onboarding_done` marker in the
config dir (`dimmy-staging` instead of `dimmy` for the staging tester build);
the `isOnboardingComplete` UserDefaults key only records that the wizard ran on
this Mac. The `rm` line is therefore required for a clean onboarding. The
legacy Diagnostics pane has a "Reset onboarding" button that does both.

(`com.dimmy.app` is the `PRODUCT_BUNDLE_IDENTIFIER` in `Dimmy.xcodeproj`.)

## Hotkey diagnostics

- Tail `/tmp/dimmy-hotkey.log` for the event-tap lifecycle and every
  `flagsChanged` event the tap sees.
- The Tahoe v3 Settings have no live hotkey/TCC panel in Debug: Settings →
  Permissions shows the access status and a "Stuck?" help block, and Settings →
  Debug → Diagnostics only holds the "LLM log enabled" and "Audio debug"
  toggles. The older Diagnostics pane (`DiagnosticsSettingsView`, shown when
  `useTahoeSettings` is off) has the "Refresh permissions now", "Open
  /tmp/dimmy-hotkey.log" and "Reset onboarding" buttons.
