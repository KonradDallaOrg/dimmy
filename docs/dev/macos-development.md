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

The fix is trivial: always test from `/Applications/Dimmy.app`, rebuilt in
place. TCC associates the permission with that path once and forgets about
DerivedData.

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
tccutil reset Microphone com.konrad.dimmy
tccutil reset Accessibility com.konrad.dimmy
tccutil reset ListenEvent com.konrad.dimmy
defaults delete com.konrad.dimmy isOnboardingComplete 2>/dev/null || true
```

(Replace `com.konrad.dimmy` with the actual bundle ID shown in the Diagnostics
pane if it has diverged.)

## Hotkey diagnostics

- Tail `/tmp/dimmy-hotkey.log` for the event-tap lifecycle and every
  `flagsChanged` event the tap sees.
- Settings → Advanced → Diagnostics shows live TCC state, hotkey install
  status, recording state, and quick-action buttons.
