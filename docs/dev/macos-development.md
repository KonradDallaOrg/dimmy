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

It builds with `xcodebuild`, `rm -rf`'s `/Applications/Dimmy.app`, copies the
fresh bundle, verifies the code signature (`codesign --verify --deep --strict`),
and launches the app.

## First-run TCC reset

If you want a truly clean state (e.g., to test the onboarding from scratch):

```
tccutil reset Microphone com.dimmy.app
tccutil reset Accessibility com.dimmy.app
tccutil reset ListenEvent com.dimmy.app
defaults delete com.dimmy.app isOnboardingComplete 2>/dev/null || true
```

(Replace `com.dimmy.app` with the actual bundle ID shown in the Diagnostics
pane if it has diverged.)

## Hotkey diagnostics

- Tail `/tmp/dimmy-hotkey.log` for the event-tap lifecycle and every
  `flagsChanged` event the tap sees.
- Settings → Advanced → Diagnostics shows live TCC state, hotkey install
  status, recording state, and quick-action buttons.
