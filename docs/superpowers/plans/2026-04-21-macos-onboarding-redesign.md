# macOS Onboarding Redesign Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a stable, developer-debuggable macOS onboarding + hotkey flow on top of the v0.6.2 baseline, matching the design in `docs/superpowers/specs/2026-04-21-macos-onboarding-redesign-design.md`.

**Architecture:** SwiftUI app delegate keeps Rust-core init deferred until permissions are granted. A single `CGEventTap`-based `HotkeyManager` publishes `AppState.hotkeyStatus` so the UI can surface failures instead of silently no-opping. Onboarding drops from 5 to 4 steps; Fn-only shortcut moves behind an advanced disclosure; a new Diagnostics pane in Settings exposes every piece of live state a user or developer needs.

**Tech Stack:** Swift 5.9 / SwiftUI / AppKit on macOS 12+. Tests use the existing `SelfTests` crash-on-fail pattern from `platforms/macos/Dimmy/Utilities/SelfTests.swift` (Negative Space Programming). Build via Xcode command-line.

**Branch:** `feat/macos-onboarding-redesign` (already created).

---

## File Structure

### Modify
- `platforms/macos/Dimmy/State/AppState.swift` — add `HotkeyStatus` enum + `@Published hotkeyStatus`; `PermissionsManager` polling cadence + `refreshNow()`.
- `platforms/macos/Dimmy/Managers/HotkeyManager.swift` — delete passive NSEvent fallback; write `hotkeyStatus` at transitions.
- `platforms/macos/Dimmy/Controllers/StatusBarController.swift` — observe `hotkeyStatus`; set tooltip + warning icon when != `.installed`.
- `platforms/macos/Dimmy/Views/PillView.swift` — add warning overlay when hotkey not installed.
- `platforms/macos/Dimmy/Views/Onboarding/OnboardingContainerView.swift` — 4 steps; `startStep` init param; drop Model Download.
- `platforms/macos/Dimmy/Views/Onboarding/PermissionsStepView.swift` — conditional Input Monitoring row; call `refreshNow()` on button clicks.
- `platforms/macos/Dimmy/Views/Onboarding/ShortcutStepView.swift` — drop Fn preset from default row; add advanced disclosure + confirmation modal.
- `platforms/macos/Dimmy/Views/Onboarding/TryItStepView.swift` — non-blocking setup card for missing key/model.
- `platforms/macos/Dimmy/AppDelegate.swift` — on perms-missing relaunch, open onboarding at Permissions step via `startStep: 1`.
- `platforms/macos/Dimmy/Views/Settings/SettingsContainerView.swift` — add `.diagnostics` tab case, gated on `showAdvanced`.
- `platforms/macos/Dimmy/Utilities/SelfTests.swift` — new assertions for `HotkeyStatus` and onboarding step count.
- `platforms/macos/Dimmy.xcodeproj/project.pbxproj` — register `DiagnosticsSettingsView.swift` in the Dimmy target.

### Create
- `platforms/macos/Dimmy/Views/Settings/DiagnosticsSettingsView.swift` — new diagnostics pane.
- `scripts/macos/install-to-applications.sh` — dev-loop build+install+launch script.
- `docs/dev/macos-development.md` — documents the dev loop and the TCC path-stability rationale.

### Delete
- None.

---

## Build & verification commands

Throughout the plan, the standard build check is:

```bash
cd platforms/macos && xcodebuild -project Dimmy.xcodeproj -scheme Dimmy -configuration Debug -destination 'platform=macOS,arch=arm64' build 2>&1 | tail -40
```

Expected: last line contains `** BUILD SUCCEEDED **`. Any line containing `error:` is a failure.

Runtime self-tests run on app launch in DEBUG; they crash on failure. The runtime check after a Debug build is:

```bash
# Launch the freshly built .app and wait 4 s. If it's still alive, SelfTests passed.
APP_PATH=$(cd platforms/macos && xcodebuild -project Dimmy.xcodeproj -scheme Dimmy -configuration Debug -showBuildSettings 2>/dev/null | awk -F' = ' '/^ *BUILT_PRODUCTS_DIR/ {print $2}' | head -1)/Dimmy.app
open -n "$APP_PATH"
sleep 4
pgrep -x Dimmy >/dev/null && echo "PASS: app alive" || echo "FAIL: app crashed (SelfTests)"
pkill -x Dimmy || true
```

---

## Task 1: Add HotkeyStatus enum and AppState property

**Purpose:** Foundation type. Everything else depends on this.

**Files:**
- Modify: `platforms/macos/Dimmy/State/AppState.swift` (end of file — add enum + property in `AppState` class)
- Modify: `platforms/macos/Dimmy/Utilities/SelfTests.swift` (add a test method)

- [ ] **Step 1.1: Add `HotkeyStatus` enum and `hotkeyStatus` property**

Open `platforms/macos/Dimmy/State/AppState.swift`. Above the `@MainActor final class AppState` declaration (around line 394), add:

```swift
// MARK: - Hotkey Status (surfaces CGEventTap install state to the UI)

/// Tracks whether the global shortcut interception is live.
/// Drives pill/menu-bar warning overlays and the Diagnostics pane.
enum HotkeyStatus: Equatable {
    case uninstalled            // app just launched, not yet attempted
    case installed              // CGEventTap active, shortcut works
    case accessibilityMissing   // Accessibility permission not granted
    case tapFailed(reason: String)  // unexpected install failure
}
```

Inside `AppState`, immediately after the `@Published var lastError: String?` line (around line 404), add:

```swift
    @Published var hotkeyStatus: HotkeyStatus = .uninstalled
```

- [ ] **Step 1.2: Add SelfTests assertion**

Open `platforms/macos/Dimmy/Utilities/SelfTests.swift`. Inside `static func runAll()` (line 8), append before `print("[SelfTests] All …")`:

```swift
        testHotkeyStatusCases()
```

Then add the method body before the closing `}` of `enum SelfTests` (around line 188):

```swift
    // MARK: - HotkeyStatus

    private static func testHotkeyStatusCases() {
        assert(HotkeyStatus.installed == .installed, "installed == installed")
        assert(HotkeyStatus.uninstalled != .installed, "uninstalled != installed")
        assert(HotkeyStatus.accessibilityMissing != .installed, "accessibilityMissing != installed")
        assert(HotkeyStatus.tapFailed(reason: "a") != HotkeyStatus.tapFailed(reason: "b"), "tapFailed differs by reason")
        assert(HotkeyStatus.tapFailed(reason: "x") == HotkeyStatus.tapFailed(reason: "x"), "tapFailed equals by reason")
    }
```

- [ ] **Step 1.3: Build**

Run:

```bash
cd platforms/macos && xcodebuild -project Dimmy.xcodeproj -scheme Dimmy -configuration Debug -destination 'platform=macOS,arch=arm64' build 2>&1 | tail -20
```

Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 1.4: Commit**

```bash
git add platforms/macos/Dimmy/State/AppState.swift platforms/macos/Dimmy/Utilities/SelfTests.swift
git commit -m "$(cat <<'EOF'
feat(macos): introduce HotkeyStatus enum on AppState

Adds the published state surface that the pill, menu bar, and
Diagnostics pane will observe to show hotkey health. Covered by a
new SelfTests assertion.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Reduce PermissionsManager polling + add refreshNow()

**Purpose:** Lower idle-CPU wake-ups, give UI call sites an explicit refresh API.

**Files:**
- Modify: `platforms/macos/Dimmy/State/AppState.swift` (`PermissionsManager` class)

- [ ] **Step 2.1: Change polling cadence and expose refreshNow**

In `PermissionsManager` (starts around line 16 of `AppState.swift`), locate the line:

```swift
        pollTimer = Timer.scheduledTimer(withTimeInterval: 1.5, repeats: true) { [weak self] _ in
```

Replace `1.5` with `5.0`. The line becomes:

```swift
        pollTimer = Timer.scheduledTimer(withTimeInterval: 5.0, repeats: true) { [weak self] _ in
```

Immediately after the existing `func refresh()` method (ends around line 62), add:

```swift
    /// Explicit refresh intended for user-action sites (button clicks, post-dialog).
    /// Identical to `refresh()`; separate name signals intent at the call site.
    func refreshNow() {
        refresh()
    }
```

- [ ] **Step 2.2: Build**

```bash
cd platforms/macos && xcodebuild -project Dimmy.xcodeproj -scheme Dimmy -configuration Debug -destination 'platform=macOS,arch=arm64' build 2>&1 | tail -10
```

Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 2.3: Commit**

```bash
git add platforms/macos/Dimmy/State/AppState.swift
git commit -m "$(cat <<'EOF'
refactor(macos): slow PermissionsManager poll to 5s, add refreshNow()

The 1.5s polling cadence was a band-aid that papered over missing
explicit refresh hooks; didBecomeActive is the real driver. Drops
timer wake-ups by ~3x and exposes refreshNow() so call sites
document their intent.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: HotkeyManager — remove passive fallback, publish hotkeyStatus

**Purpose:** Single mechanism. If Accessibility is missing, the user must see it — not have it hidden by a passive monitor that cannot consume events.

**Files:**
- Modify: `platforms/macos/Dimmy/Managers/HotkeyManager.swift`

- [ ] **Step 3.1: Remove passive fallback fields and methods**

In `HotkeyManager`, delete the following lines (in `HotkeyManager.swift` around lines 47–49):

```swift
    // Fallback passive monitor (cannot override other apps). Used only if event tap install fails.
    private var globalFlagsMonitor: Any?
    private var localFlagsMonitor: Any?
```

Delete the entire `installPassiveFallback()` method (around lines 126–140) and `teardownPassiveFallback()` method (around lines 142–147).

- [ ] **Step 3.2: Rewrite `start()` to set hotkeyStatus**

Replace the existing `func start(appState: AppState)` body (around lines 71–112) with:

```swift
    func start(appState: AppState) {
        self.appState = appState
        hkLog("[HotkeyManager] start() trusted=\(AXIsProcessTrusted())")

        if AXIsProcessTrustedWithOptions(nil) {
            tryInstallEventTap()
            appState.hotkeyStatus = eventTap != nil
                ? .installed
                : .tapFailed(reason: "CGEvent.tapCreate returned nil despite Accessibility being trusted")
        } else {
            appState.hotkeyStatus = .accessibilityMissing
            startAccessibilityPolling()
        }

        // macOS disables event taps during sleep — reinstall on wake.
        wakeObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didWakeNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor in
                guard let self else { return }
                hkLog("[HotkeyManager] system woke — refreshing event tap")
                if self.eventTap != nil {
                    CGEvent.tapEnable(tap: self.eventTap!, enable: true)
                } else if AXIsProcessTrustedWithOptions(nil) {
                    self.tryInstallEventTap()
                    if self.eventTap != nil { self.appState?.hotkeyStatus = .installed }
                }
            }
        }
    }
```

- [ ] **Step 3.3: Extract accessibility polling into its own method**

Still in `HotkeyManager`, add a private helper (place it right before the `// MARK: - CGEventTap (active, consumes events globally)` marker around line 158):

```swift
    private func startAccessibilityPolling() {
        guard accessibilityPollTimer == nil else { return }
        accessibilityPollTimer = Timer.scheduledTimer(withTimeInterval: 2.0, repeats: true) { [weak self] _ in
            Task { @MainActor in
                guard let self, self.eventTap == nil else { return }
                if AXIsProcessTrustedWithOptions(nil) {
                    hkLog("[HotkeyManager] Accessibility now trusted — installing event tap")
                    self.tryInstallEventTap()
                    if self.eventTap != nil {
                        self.appState?.hotkeyStatus = .installed
                        self.accessibilityPollTimer?.invalidate()
                        self.accessibilityPollTimer = nil
                    }
                }
            }
        }
    }
```

- [ ] **Step 3.4: Clean up stop()**

Replace the existing `func stop()` (around lines 114–124) with:

```swift
    func stop() {
        stopAmplitudePolling()
        uninstallEventTap()
        accessibilityPollTimer?.invalidate()
        accessibilityPollTimer = nil
        if let wakeObserver {
            NSWorkspace.shared.notificationCenter.removeObserver(wakeObserver)
        }
        wakeObserver = nil
        appState?.hotkeyStatus = .uninstalled
    }
```

- [ ] **Step 3.5: Build**

```bash
cd platforms/macos && xcodebuild -project Dimmy.xcodeproj -scheme Dimmy -configuration Debug -destination 'platform=macOS,arch=arm64' build 2>&1 | tail -20
```

Expected: `** BUILD SUCCEEDED **`. If a compiler error references the deleted `globalFlagsMonitor`/`localFlagsMonitor` identifiers, remove the remaining reference.

- [ ] **Step 3.6: Commit**

```bash
git add platforms/macos/Dimmy/Managers/HotkeyManager.swift
git commit -m "$(cat <<'EOF'
refactor(macos): drop passive NSEvent fallback, publish hotkeyStatus

Passive fallback hid Accessibility failures — the shortcut silently
did nothing instead of telling the user what was wrong. Now the
single CGEventTap path publishes an explicit state to AppState so
the pill and menu bar can show a warning, and the Diagnostics pane
can show the reason.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: StatusBar + Pill overlays for hotkey status

**Purpose:** Make hotkey health visible without opening Settings.

**Files:**
- Modify: `platforms/macos/Dimmy/Controllers/StatusBarController.swift`
- Modify: `platforms/macos/Dimmy/Views/PillView.swift`

- [ ] **Step 4.1: Observe hotkeyStatus in StatusBarController**

In `StatusBarController.swift`, in `observeRecordingState()` (line 42), rename it to `observeState()` and add an observer for `hotkeyStatus`. Replace the whole method with:

```swift
    private func observeState() {
        appState.$recordingState
            .receive(on: DispatchQueue.main)
            .sink { [weak self] state in
                self?.updateIcon(for: state, hotkey: self?.appState.hotkeyStatus ?? .uninstalled)
            }
            .store(in: &cancellables)

        appState.$hotkeyStatus
            .receive(on: DispatchQueue.main)
            .sink { [weak self] status in
                self?.updateIcon(for: self?.appState.recordingState ?? .idle, hotkey: status)
            }
            .store(in: &cancellables)
    }
```

Update the `init` (line 12) to call `observeState()` instead of `observeRecordingState()`.

Replace `updateIcon(for state: RecordingState)` signature and body with:

```swift
    private func updateIcon(for state: RecordingState, hotkey: HotkeyStatus) {
        guard let button = statusItem?.button else { return }
        let size = NSImage.SymbolConfiguration(pointSize: 16, weight: .regular)

        // Hotkey health overrides idle icon so users see the problem at a glance.
        if case .idle = state, hotkey != .installed {
            let warn = size.applying(NSImage.SymbolConfiguration(paletteColors: [.systemOrange]))
            button.image = NSImage(systemSymbolName: "exclamationmark.triangle.fill",
                                   accessibilityDescription: "Dimmy - Hotkey disabled")?
                .withSymbolConfiguration(warn)
            button.image?.isTemplate = false
            button.toolTip = Self.tooltip(for: hotkey)
            return
        }

        button.toolTip = nil

        switch state {
        case .idle:
            button.image = NSImage(systemSymbolName: "waveform.circle", accessibilityDescription: "Dimmy - Ready")?
                .withSymbolConfiguration(size)
            button.image?.isTemplate = true
        case .recording:
            let config = size.applying(NSImage.SymbolConfiguration(paletteColors: [.systemRed]))
            button.image = NSImage(systemSymbolName: "waveform.circle.fill", accessibilityDescription: "Dimmy - Recording")?
                .withSymbolConfiguration(config)
            button.image?.isTemplate = false
        case .transcribing:
            let config = size.applying(NSImage.SymbolConfiguration(paletteColors: [.systemBlue]))
            button.image = NSImage(systemSymbolName: "ellipsis.circle.fill", accessibilityDescription: "Dimmy - Transcribing")?
                .withSymbolConfiguration(config)
            button.image?.isTemplate = false
        case .processing:
            let config = size.applying(NSImage.SymbolConfiguration(paletteColors: [.systemPurple]))
            button.image = NSImage(systemSymbolName: "sparkles", accessibilityDescription: "Dimmy - Processing")?
                .withSymbolConfiguration(config)
            button.image?.isTemplate = false
        case .completing:
            let config = size.applying(NSImage.SymbolConfiguration(paletteColors: [.systemGreen]))
            button.image = NSImage(systemSymbolName: "checkmark.circle.fill", accessibilityDescription: "Dimmy - Done")?
                .withSymbolConfiguration(config)
            button.image?.isTemplate = false
        }
    }

    private static func tooltip(for hotkey: HotkeyStatus) -> String {
        switch hotkey {
        case .installed: return ""
        case .uninstalled: return "Dimmy: hotkey not yet initialized"
        case .accessibilityMissing: return "Dimmy: shortcut disabled — grant Accessibility in System Settings"
        case .tapFailed(let reason): return "Dimmy: shortcut disabled (\(reason))"
        }
    }
```

- [ ] **Step 4.2: Add warning overlay to PillView**

Read the first 40 lines of `platforms/macos/Dimmy/Views/PillView.swift` to locate the `body` declaration. Inside the outermost `ZStack` or container of the pill body, append a `.overlay` modifier that shows a warning when `appState.hotkeyStatus != .installed`. If `PillView` does not currently use `@ObservedObject var appState`, it already should; confirm the property exists near the top of the struct.

Append this `.overlay` after the pill's existing frame modifier:

```swift
        .overlay(alignment: .topTrailing) {
            if appState.hotkeyStatus != .installed {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 11, weight: .bold))
                    .foregroundColor(.orange)
                    .padding(6)
                    .background(Circle().fill(Color.black.opacity(0.6)))
                    .offset(x: 4, y: -4)
                    .help(Self.warningText(for: appState.hotkeyStatus))
            }
        }
```

Add to `PillView` a static helper:

```swift
    private static func warningText(for status: HotkeyStatus) -> String {
        switch status {
        case .installed, .uninstalled: return ""
        case .accessibilityMissing: return "Shortcut disabled: grant Accessibility in System Settings"
        case .tapFailed(let reason): return "Shortcut disabled: \(reason)"
        }
    }
```

- [ ] **Step 4.3: Build**

```bash
cd platforms/macos && xcodebuild -project Dimmy.xcodeproj -scheme Dimmy -configuration Debug -destination 'platform=macOS,arch=arm64' build 2>&1 | tail -20
```

Expected: `** BUILD SUCCEEDED **`. If `PillView` doesn't have `appState` accessible, fix the overlay to reference the existing state name.

- [ ] **Step 4.4: Commit**

```bash
git add platforms/macos/Dimmy/Controllers/StatusBarController.swift platforms/macos/Dimmy/Views/PillView.swift
git commit -m "$(cat <<'EOF'
feat(macos): surface hotkeyStatus in menu bar and pill

Menu-bar idle icon becomes an orange triangle with an explanatory
tooltip when the CGEventTap isn't live; the pill grows a small
warning badge with the same explanation. No more silent "shortcut
does nothing" state.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: OnboardingContainerView — 4 steps + startStep param

**Purpose:** Collapse onboarding from 5 to 4 steps; allow re-opening at a specific step after permissions revocation.

**Files:**
- Modify: `platforms/macos/Dimmy/Views/Onboarding/OnboardingContainerView.swift`
- Modify: `platforms/macos/Dimmy/Utilities/SelfTests.swift`

- [ ] **Step 5.1: Replace the whole file contents**

Overwrite `OnboardingContainerView.swift` with:

```swift
import SwiftUI

struct OnboardingContainerView: View {
    static let totalSteps = 4

    @ObservedObject var appState: AppState
    @State private var currentStep: Int

    init(appState: AppState, startStep: Int = 0) {
        self.appState = appState
        let clamped = max(0, min(startStep, Self.totalSteps - 1))
        self._currentStep = State(initialValue: clamped)
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                ForEach(0..<Self.totalSteps, id: \.self) { index in
                    Circle()
                        .fill(index <= currentStep ? Color.accentColor : Color.secondary.opacity(0.3))
                        .frame(width: 8, height: 8)
                }
            }
            .padding(.top, 20)

            Group {
                switch currentStep {
                case 0:
                    WelcomeStepView {
                        withAnimation { currentStep = 1 }
                    }
                case 1:
                    PermissionsStepView(appState: appState) {
                        withAnimation { currentStep = 2 }
                    }
                case 2:
                    ShortcutStepView(appState: appState) {
                        appState.showPillIntro = true
                        withAnimation { currentStep = 3 }
                    }
                case 3:
                    TryItStepView(appState: appState) {
                        appState.isOnboardingComplete = true
                    }
                default:
                    EmptyView()
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .transition(.asymmetric(
                insertion: .move(edge: .trailing).combined(with: .opacity),
                removal: .move(edge: .leading).combined(with: .opacity)
            ))
        }
        .frame(width: 520, height: 440)
        .boldUI()
    }
}
```

- [ ] **Step 5.2: Add SelfTests assertion for totalSteps**

In `SelfTests.swift`, append inside `runAll()`:

```swift
        testOnboardingStepCount()
```

And append the method before the closing brace of `enum SelfTests`:

```swift
    // MARK: - Onboarding

    private static func testOnboardingStepCount() {
        assert(OnboardingContainerView.totalSteps == 4, "Onboarding has 4 steps, got \(OnboardingContainerView.totalSteps)")
    }
```

- [ ] **Step 5.3: Build**

```bash
cd platforms/macos && xcodebuild -project Dimmy.xcodeproj -scheme Dimmy -configuration Debug -destination 'platform=macOS,arch=arm64' build 2>&1 | tail -20
```

Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 5.4: Commit**

```bash
git add platforms/macos/Dimmy/Views/Onboarding/OnboardingContainerView.swift platforms/macos/Dimmy/Utilities/SelfTests.swift
git commit -m "$(cat <<'EOF'
feat(macos): collapse onboarding to 4 steps with startStep parameter

Drops the Model Download step from the linear flow (model download
moves into the Try It step's non-blocking setup card). Adds a
startStep: Int parameter so AppDelegate can reopen onboarding
directly at Permissions when TCC state is lost.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: ShortcutStepView — Fn behind advanced disclosure + confirm modal

**Purpose:** Prevent new users from picking Fn — which can't work reliably without manual Keyboard Settings tweaks — while keeping it available for power users who know the tradeoffs.

**Files:**
- Modify: `platforms/macos/Dimmy/Views/Onboarding/ShortcutStepView.swift`

- [ ] **Step 6.1: Remove Fn from default presets and add advanced disclosure + modal state**

Replace the existing `presets` array (around line 16) with the three non-Fn entries:

```swift
    private let presets: [(label: String, shortcut: ModifierShortcut)] = [
        ("⌃⌥", ModifierShortcut(fn: false, control: true, option: true, command: false, shift: false)),
        ("⌃⇧", ModifierShortcut(fn: false, control: true, option: false, command: false, shift: true)),
        ("⌥⇧", ModifierShortcut(fn: false, control: false, option: true, command: false, shift: true)),
    ]

    @State private var showAdvanced = false
    @State private var pendingFnConfirmation = false
```

- [ ] **Step 6.2: Insert the disclosure below the preset row**

In `body`, after the preset row's closing `}` (currently ends around line 127 — right after the `HStack(spacing: 12)` block ends and before the `if activeShortcut.isFnOnly` check), insert:

```swift
            DisclosureGroup(isExpanded: $showAdvanced) {
                VStack(alignment: .leading, spacing: 10) {
                    Button(action: {
                        pendingFnConfirmation = true
                    }) {
                        HStack(spacing: 8) {
                            Text("fn")
                                .font(.system(size: 16, weight: .medium, design: .rounded))
                                .padding(.horizontal, 14)
                                .padding(.vertical, 8)
                                .background(
                                    RoundedRectangle(cornerRadius: 8)
                                        .fill(activeShortcut.isFnOnly
                                              ? Color.accentColor.opacity(0.15)
                                              : Color(nsColor: .controlBackgroundColor))
                                )
                                .overlay(
                                    RoundedRectangle(cornerRadius: 8)
                                        .stroke(activeShortcut.isFnOnly
                                                ? Color.accentColor
                                                : Color.primary.opacity(0.12),
                                                lineWidth: activeShortcut.isFnOnly ? 1.5 : 1)
                                )
                            Text("Use Fn key (requires macOS tweaks)")
                                .font(.system(size: 12))
                                .foregroundColor(.secondary)
                        }
                    }
                    .buttonStyle(.plain)
                }
                .padding(.top, 6)
            } label: {
                Text("Advanced")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundColor(.secondary)
            }
            .padding(.horizontal, 20)
```

- [ ] **Step 6.3: Add the confirmation alert and wire it**

At the very end of the outer `VStack(spacing: 20)` block but still inside `body`'s `ScrollView`, attach an `.alert` modifier to the outer `VStack` (after the last `Spacer`/`.padding` inside that VStack). Specifically, on the line that reads `.padding(.horizontal, 32)` inside `body` (around line 150), add immediately above it:

```swift
        .alert("Use Fn as your shortcut?", isPresented: $pendingFnConfirmation) {
            Button("Cancel", role: .cancel) { }
            Button("Use Fn") {
                withAnimation(.easeInOut(duration: 0.2)) {
                    pendingShortcut = ModifierShortcut.fnOnly
                }
            }
        } message: {
            Text("Fn requires disabling macOS Dictation/Globe shortcuts in Keyboard Settings and granting Input Monitoring. Without those, the Fn key will be intercepted by macOS and Dimmy will not trigger.")
        }
```

- [ ] **Step 6.4: Build**

```bash
cd platforms/macos && xcodebuild -project Dimmy.xcodeproj -scheme Dimmy -configuration Debug -destination 'platform=macOS,arch=arm64' build 2>&1 | tail -20
```

Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 6.5: Commit**

```bash
git add platforms/macos/Dimmy/Views/Onboarding/ShortcutStepView.swift
git commit -m "$(cat <<'EOF'
feat(macos): move Fn shortcut into an advanced disclosure with warning

Fn is architecturally unreliable on macOS without Keyboard Settings
tweaks and Input Monitoring. Removes it from the default preset row
and gates it behind an expandable Advanced section plus a confirm
modal spelling out the tradeoffs. New users cannot pick Fn by
accident; power users still can.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 7: PermissionsStepView — conditional Input Monitoring + refreshNow() on clicks

**Purpose:** Input Monitoring isn't required for the default ⌃⌥ path; only surface it when the user's chosen shortcut needs it. Also, guarantee UI reflects TCC state immediately after a user action.

**Files:**
- Modify: `platforms/macos/Dimmy/Views/Onboarding/PermissionsStepView.swift`

- [ ] **Step 7.1: Gate the Input Monitoring row on Fn shortcut**

Replace the existing `permissionRow(icon: "keyboard" ...)` block (around lines 44–51) with a conditional:

```swift
                if appState.shortcut.isFnOnly {
                    permissionRow(
                        icon: "keyboard",
                        title: "Input Monitoring",
                        description: "Required for your Fn-key shortcut",
                        granted: perms.inputMonitoringGranted,
                        pending: perms.inputMonitoring == kIOHIDAccessTypeUnknown && !inputMonitoringPromptShown,
                        action: requestInputMonitoring
                    )
                }
```

And gate the hint banner (around lines 62–68) the same way:

```swift
                if appState.shortcut.isFnOnly && inputMonitoringPromptShown && !perms.inputMonitoringGranted {
                    hintBanner(
                        icon: "arrow.up.right.square",
                        color: .orange,
                        text: "Toggle **Dimmy** ON in System Settings → Privacy & Security → Input Monitoring"
                    )
                }
```

- [ ] **Step 7.2: Update intro copy to match**

Replace the subtitle string (around line 19) with:

```swift
            Text("Dimmy needs access to your microphone and to the active app so it can paste transcribed text.")
```

- [ ] **Step 7.3: Call refreshNow() on every request* action**

In `requestMic()`, after the `refresh()` inside the Task block... wait, `PermissionsManager.requestMicrophone()` already calls `refresh()` internally. Good. But the outer `micRequestInFlight = false` happens after, and the view's `perms` observation should update. Still, to be explicit for manual-dialog paths, add `perms.refreshNow()` at the end of each request method.

Replace `requestAccessibility()` (around lines 183–191) with:

```swift
    private func requestAccessibility() {
        if perms.accessibilityGranted { return }
        if accessibilityPromptShown {
            perms.openAccessibilitySettings()
        } else {
            perms.promptAccessibility()
            withAnimation { accessibilityPromptShown = true }
        }
        perms.refreshNow()
    }
```

Replace `requestInputMonitoring()` (around lines 193–201) with:

```swift
    private func requestInputMonitoring() {
        if perms.inputMonitoringGranted { return }
        if inputMonitoringPromptShown {
            perms.openInputMonitoringSettings()
        } else {
            perms.requestInputMonitoring()
            withAnimation { inputMonitoringPromptShown = true }
        }
        perms.refreshNow()
    }
```

In `requestMic()`, after the `micRequestInFlight = false` line (around line 180), add `perms.refreshNow()`:

```swift
            micRequestInFlight = false
            perms.refreshNow()
```

- [ ] **Step 7.4: Build**

```bash
cd platforms/macos && xcodebuild -project Dimmy.xcodeproj -scheme Dimmy -configuration Debug -destination 'platform=macOS,arch=arm64' build 2>&1 | tail -20
```

Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 7.5: Commit**

```bash
git add platforms/macos/Dimmy/Views/Onboarding/PermissionsStepView.swift
git commit -m "$(cat <<'EOF'
refactor(macos): hide Input Monitoring row unless Fn shortcut is picked

Input Monitoring is only required for the Fn-only shortcut path.
Hiding it otherwise reduces first-run friction from three scary
toggles to two. Also calls refreshNow() after every user click so
the UI picks up TCC state the moment the native dialog returns,
not on the next 5s poll tick.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 8: TryItStepView — non-blocking setup card

**Purpose:** Let the onboarding always reach a completable state. If cloud is selected without a key, or local without a model, the user sees an inline card with CTAs — but "Finish" always works.

**Files:**
- Modify: `platforms/macos/Dimmy/Views/Onboarding/TryItStepView.swift`

- [ ] **Step 8.1: Replace the file contents**

Overwrite `TryItStepView.swift` with:

```swift
import SwiftUI

struct TryItStepView: View {
    @ObservedObject var appState: AppState
    let onComplete: () -> Void

    @State private var demoText: String = ""
    @State private var hasTriedRecording = false
    @State private var showSuccess = false
    @State private var modelReady: Bool = false

    private var needsCloudKey: Bool {
        appState.sttMode == "cloud" && !appState.hasKey
    }
    private var needsLocalModel: Bool {
        appState.sttMode == "local" && !modelReady
    }
    private var needsSetup: Bool {
        needsCloudKey || needsLocalModel
    }

    var body: some View {
        VStack(spacing: 16) {
            Spacer(minLength: 4)

            if showSuccess {
                successView
            } else {
                tryView
            }

            Spacer(minLength: 4)
        }
        .padding(.horizontal, 32)
        .onAppear {
            modelReady = DimmyCore.shared.modelExists(appState.localModel)
        }
        .onChange(of: appState.recordingState) { _, newState in
            if case .completing = newState {
                demoText = appState.lastTranscript.isEmpty ? "No speech detected" : appState.lastTranscript
                hasTriedRecording = true
            }
            if case .idle = newState, hasTriedRecording {
                withAnimation(.spring(response: 0.4)) {
                    showSuccess = true
                }
            }
        }
    }

    private var tryView: some View {
        VStack(spacing: 16) {
            Text("Try it!")
                .font(.system(size: 26, weight: .bold))

            if needsSetup {
                setupCard
            } else {
                readyView
            }

            Button(action: {
                withAnimation(.spring(response: 0.4)) { showSuccess = true }
            }) {
                Text(needsSetup ? "Finish (I'll set up later)" : "Skip for now")
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)
            }
            .buttonStyle(.plain)
        }
    }

    private var readyView: some View {
        VStack(spacing: 14) {
            Text("Hold \(appState.shortcut.displayString) and say something")
                .font(.system(size: 14))
                .foregroundColor(.secondary)

            Text("The pill overlay will animate while you speak")
                .font(.system(size: 12))
                .foregroundColor(Color(nsColor: .tertiaryLabelColor))

            VStack(alignment: .leading, spacing: 6) {
                Text("Your dictation will appear here:")
                    .font(.system(size: 11))
                    .foregroundColor(Color(nsColor: .tertiaryLabelColor))

                ZStack(alignment: .topLeading) {
                    RoundedRectangle(cornerRadius: 8)
                        .fill(Color(nsColor: .textBackgroundColor))
                        .frame(height: 80)

                    if demoText.isEmpty {
                        Text("Waiting for your voice...")
                            .font(.system(size: 13))
                            .foregroundColor(Color(nsColor: .tertiaryLabelColor))
                            .padding(10)
                    } else {
                        Text(demoText)
                            .font(.system(size: 13))
                            .padding(10)
                    }
                }
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(Color.primary.opacity(0.1), lineWidth: 1)
                )
            }
        }
    }

    private var setupCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 8) {
                Image(systemName: "gearshape.fill")
                    .foregroundColor(.accentColor)
                Text("One more thing")
                    .font(.system(size: 14, weight: .semibold))
            }

            if needsCloudKey {
                Text("Dimmy is configured for cloud transcription. Add an API key in Settings to start dictating.")
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Button("Open Settings") {
                    AppDelegate.shared?.openSettings()
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.regular)
            } else if needsLocalModel {
                Text("Download the local Whisper model (78 MB) to start dictating — no internet needed afterwards.")
                    .font(.system(size: 12))
                    .foregroundColor(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if appState.isDownloadingModel {
                    VStack(spacing: 6) {
                        ProgressView(value: appState.modelDownloadProgress, total: 1.0)
                        Text("\(Int(appState.modelDownloadProgress * 100))%")
                            .font(.system(size: 11))
                            .foregroundColor(.secondary)
                    }
                } else {
                    Button("Download model") {
                        startDownload()
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.regular)
                }
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            RoundedRectangle(cornerRadius: 12)
                .fill(Color(nsColor: .controlBackgroundColor))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 12)
                .stroke(Color.accentColor.opacity(0.2), lineWidth: 1)
        )
    }

    private var successView: some View {
        VStack(spacing: 20) {
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 56))
                .foregroundColor(.green)

            Text("You're all set!")
                .font(.system(size: 24, weight: .bold))

            Text("Dimmy lives in your menu bar.\nHold \(appState.shortcut.displayString) anywhere to dictate.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .lineSpacing(4)

            Button(action: {
                appState.showPillIntro = true
                onComplete()
            }) {
                Text("Start Using Dimmy")
                    .font(.system(size: 14, weight: .semibold))
                    .frame(maxWidth: 200)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
        }
    }

    private func startDownload() {
        appState.isDownloadingModel = true
        appState.modelDownloadProgress = 0.0

        DispatchQueue.global(qos: .userInitiated).async {
            let success = DimmyCore.shared.downloadModel(appState.localModel)
            DispatchQueue.main.async {
                appState.isDownloadingModel = false
                if success {
                    modelReady = DimmyCore.shared.modelExists(appState.localModel)
                }
            }
        }
    }
}
```

- [ ] **Step 8.2: Build**

```bash
cd platforms/macos && xcodebuild -project Dimmy.xcodeproj -scheme Dimmy -configuration Debug -destination 'platform=macOS,arch=arm64' build 2>&1 | tail -20
```

Expected: `** BUILD SUCCEEDED **`. If `modelReady` errors out when referenced inside `startDownload()` (value type capture in a @State variable from a closure on `self`), add `@MainActor` isolation or rewrite the assignment as `Task { @MainActor in modelReady = … }`. The SwiftUI struct's `@State` is already main-actor-isolated when inside the view — the DispatchQueue.main.async context is fine.

- [ ] **Step 8.3: Commit**

```bash
git add platforms/macos/Dimmy/Views/Onboarding/TryItStepView.swift
git commit -m "$(cat <<'EOF'
feat(macos): make Try It step non-blocking with inline setup card

When cloud is selected without an API key, or local is selected
without a model, the step now shows a contextual setup card with a
CTA instead of silently failing. The Finish button is always
enabled so users always reach the menu bar.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 9: AppDelegate — reopen at Permissions step on perm loss

**Purpose:** If permissions were revoked after onboarding, drop the user into the Permissions step directly instead of restarting the whole flow.

**Files:**
- Modify: `platforms/macos/Dimmy/AppDelegate.swift`

- [ ] **Step 9.1: Replace the perm-loss branch**

In `applicationDidFinishLaunching`, locate the block that currently reads (around lines 51–57):

```swift
            if permissionsGranted() {
                initializeCoreAsync()
            } else {
                // Onboarding previously done but permissions were revoked → re-run permissions flow.
                hkLog("[AppDelegate] onboarding complete but permissions missing — reopening permissions onboarding")
                appState.isOnboardingComplete = false
                showOnboarding()
            }
```

Replace it with:

```swift
            if permissionsGranted() {
                initializeCoreAsync()
            } else {
                hkLog("[AppDelegate] onboarding complete but permissions missing — reopening Permissions step")
                showOnboarding(startStep: 1)
            }
```

- [ ] **Step 9.2: Update showOnboarding signature**

Replace the existing `private func showOnboarding()` (starts around line 127) with:

```swift
    private func showOnboarding(startStep: Int = 0) {
        let onboardingView = OnboardingContainerView(appState: appState, startStep: startStep)

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 440),
            styleMask: [.titled, .closable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.center()
        window.title = "Welcome to Dimmy"
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.contentView = NSHostingView(rootView: onboardingView)
        window.isReleasedWhenClosed = false
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)

        self.onboardingWindow = window
    }
```

- [ ] **Step 9.3: Build**

```bash
cd platforms/macos && xcodebuild -project Dimmy.xcodeproj -scheme Dimmy -configuration Debug -destination 'platform=macOS,arch=arm64' build 2>&1 | tail -20
```

Expected: `** BUILD SUCCEEDED **`.

- [ ] **Step 9.4: Commit**

```bash
git add platforms/macos/Dimmy/AppDelegate.swift
git commit -m "$(cat <<'EOF'
refactor(macos): reopen onboarding at Permissions step on perm loss

Previously, revoking Accessibility post-onboarding reset the whole
flow. Now the window reopens directly at step 1 (Permissions) —
Welcome and Shortcut are skipped because the user has already
configured them.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 10: DiagnosticsSettingsView + wire into Settings tabs

**Purpose:** One-shot visibility into every live state relevant to debugging: bundle path, TCC status, hotkey status, recording state, actions.

**Files:**
- Create: `platforms/macos/Dimmy/Views/Settings/DiagnosticsSettingsView.swift`
- Modify: `platforms/macos/Dimmy/Views/Settings/SettingsContainerView.swift`
- Modify: `platforms/macos/Dimmy.xcodeproj/project.pbxproj` (add file reference + build file entry)

- [ ] **Step 10.1: Create DiagnosticsSettingsView.swift**

Write a new file at `platforms/macos/Dimmy/Views/Settings/DiagnosticsSettingsView.swift`:

```swift
import AppKit
import SwiftUI

struct DiagnosticsSettingsView: View {
    @ObservedObject var appState: AppState
    @ObservedObject private var perms = PermissionsManager.shared

    var body: some View {
        Form {
            Section("Bundle") {
                row("Path", Bundle.main.bundlePath)
                row("Identifier", Bundle.main.bundleIdentifier ?? "—")
                row("Version", bundleVersionString)
            }

            Section("Permissions (TCC)") {
                tccRow("Microphone", granted: perms.microphoneGranted,
                       detail: "\(perms.microphone.rawValue)")
                tccRow("Accessibility", granted: perms.accessibilityGranted,
                       detail: perms.accessibilityGranted ? "trusted" : "not trusted")
                tccRow("Input Monitoring", granted: perms.inputMonitoringGranted,
                       detail: inputMonitoringDescription)
            }

            Section("Hotkey") {
                row("Status", hotkeyDescription)
                row("Shortcut", appState.shortcut.displayString)
                row("Mode", appState.preferredMode.rawValue)
            }

            Section("Core") {
                row("STT Mode", appState.sttMode)
                row("Local Model", appState.localModel)
                row("Has API key", appState.hasKey ? "yes" : "no")
                row("Recording State", recordingStateDescription)
                row("Last Error", appState.lastError ?? "—")
            }

            Section("Actions") {
                Button("Refresh permissions now") { perms.refreshNow() }
                Button("Open /tmp/dimmy-hotkey.log") { openLog() }
                Button("Reset onboarding") { resetOnboarding() }
            }
        }
        .formStyle(.grouped)
        .onAppear { perms.refreshNow() }
    }

    private func row(_ label: String, _ value: String) -> some View {
        HStack(alignment: .top) {
            Text(label)
                .font(.system(size: 12))
                .foregroundColor(.secondary)
                .frame(width: 140, alignment: .leading)
            Text(value)
                .font(.system(size: 12, design: .monospaced))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func tccRow(_ label: String, granted: Bool, detail: String) -> some View {
        HStack(alignment: .top) {
            Text(label)
                .font(.system(size: 12))
                .foregroundColor(.secondary)
                .frame(width: 140, alignment: .leading)
            Image(systemName: granted ? "checkmark.circle.fill" : "xmark.circle.fill")
                .foregroundColor(granted ? .green : .orange)
            Text(detail)
                .font(.system(size: 12, design: .monospaced))
                .foregroundColor(granted ? .primary : .secondary)
        }
    }

    private var bundleVersionString: String {
        let short = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?"
        let build = Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "?"
        return "\(short) (build \(build))"
    }

    private var hotkeyDescription: String {
        switch appState.hotkeyStatus {
        case .uninstalled: return "uninstalled"
        case .installed: return "installed (CGEventTap active)"
        case .accessibilityMissing: return "accessibility missing"
        case .tapFailed(let reason): return "tap failed: \(reason)"
        }
    }

    private var recordingStateDescription: String {
        switch appState.recordingState {
        case .idle: return "idle"
        case .recording(let mode): return "recording (\(mode.rawValue))"
        case .transcribing: return "transcribing"
        case .processing: return "processing"
        case .completing: return "completing"
        }
    }

    private var inputMonitoringDescription: String {
        switch perms.inputMonitoring {
        case kIOHIDAccessTypeGranted: return "granted"
        case kIOHIDAccessTypeDenied: return "denied"
        case kIOHIDAccessTypeUnknown: return "unknown (not yet requested)"
        default: return "other(\(perms.inputMonitoring.rawValue))"
        }
    }

    private func openLog() {
        let url = URL(fileURLWithPath: "/tmp/dimmy-hotkey.log")
        NSWorkspace.shared.open(url)
    }

    private func resetOnboarding() {
        appState.isOnboardingComplete = false
        NSApp.keyWindow?.close()
        AppDelegate.shared?.reopenOnboarding()
    }
}
```

- [ ] **Step 10.2: Expose `reopenOnboarding()` on AppDelegate**

In `platforms/macos/Dimmy/AppDelegate.swift`, add this method near `openSettings()`:

```swift
    func reopenOnboarding() {
        showOnboarding(startStep: 0)
    }
```

- [ ] **Step 10.3: Add `.diagnostics` tab to SettingsContainerView**

In `platforms/macos/Dimmy/Views/Settings/SettingsContainerView.swift`, add to the `SettingsTab` enum (line 3), after `.debug`:

```swift
    case diagnostics = "Diagnostics"
```

Add to the `icon` switch:

```swift
        case .diagnostics: return "stethoscope"
```

In `visibleTabs`:

```swift
            case .diagnostics:
                return appState.showAdvanced
```

In `detailView`:

```swift
        case .diagnostics:
            DiagnosticsSettingsView(appState: appState)
```

- [ ] **Step 10.4: Register the new file in Xcode project**

The Xcode project file (`platforms/macos/Dimmy.xcodeproj/project.pbxproj`) needs three pieces for the new Swift file:

1. A `PBXFileReference` entry.
2. A `PBXBuildFile` entry referencing it.
3. Membership in the `Sources` build phase.
4. Membership in the `Views/Settings` group.

The lowest-risk way to make this edit is to mirror the block that already exists for `PermissionsSettingsView.swift`. Open the file and find every line that contains `PermissionsSettingsView`; for each line, add an analogous line immediately below it with `DiagnosticsSettingsView` substituted and fresh unique 24-character hex IDs (invent any unused ones — Xcode accepts them as long as they're consistent within the file).

Concretely, run:

```bash
grep -n PermissionsSettingsView platforms/macos/Dimmy.xcodeproj/project.pbxproj
```

You will see four lines matching: a `PBXBuildFile` line, a `PBXFileReference` line, a `PBXGroup` children line, and a `PBXSourcesBuildPhase` files line. For each of those four lines, duplicate it directly below, changing the filename to `DiagnosticsSettingsView.swift` and the two hex IDs on that line to freshly generated 24-hex-character strings. (macOS `uuidgen | tr -d - | head -c 24` gives one.) Save the file.

Verify:

```bash
grep -c DiagnosticsSettingsView platforms/macos/Dimmy.xcodeproj/project.pbxproj
```

Expected: `4`.

- [ ] **Step 10.5: Build**

```bash
cd platforms/macos && xcodebuild -project Dimmy.xcodeproj -scheme Dimmy -configuration Debug -destination 'platform=macOS,arch=arm64' build 2>&1 | tail -30
```

Expected: `** BUILD SUCCEEDED **`. If the build complains about `DiagnosticsSettingsView` not being a target member, revisit Step 10.4 — one of the four insertions is missing or has a mismatched ID.

- [ ] **Step 10.6: Commit**

```bash
git add platforms/macos/Dimmy/Views/Settings/DiagnosticsSettingsView.swift platforms/macos/Dimmy/Views/Settings/SettingsContainerView.swift platforms/macos/Dimmy/AppDelegate.swift platforms/macos/Dimmy.xcodeproj/project.pbxproj
git commit -m "$(cat <<'EOF'
feat(macos): add Diagnostics pane in advanced Settings

Surfaces bundle path, TCC state per permission, hotkey install
status, recording state, STT config, and quick actions (refresh
permissions, open hotkey log, reset onboarding). Gated on the
existing Advanced toggle so it doesn't clutter the default
Settings UI. Makes subsequent bug reports 10x more useful.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 11: Dev install script + doc

**Purpose:** Standardise the dev loop so TCC records a stable bundle path and permissions survive across rebuilds.

**Files:**
- Create: `scripts/macos/install-to-applications.sh`
- Create: `docs/dev/macos-development.md`

- [ ] **Step 11.1: Create the install script**

Write `scripts/macos/install-to-applications.sh`:

```bash
#!/usr/bin/env bash
#
# Build Dimmy (Debug) from Xcode and install into /Applications so that
# TCC (microphone / accessibility / input monitoring) records a stable
# bundle path across rebuilds. Recommended dev loop for macOS.
#
# Usage: scripts/macos/install-to-applications.sh [--release]

set -euo pipefail

CONFIG="Debug"
if [[ "${1:-}" == "--release" ]]; then
    CONFIG="Release"
fi

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT/platforms/macos"

echo "[install] building Dimmy ($CONFIG)…"
xcodebuild \
    -project Dimmy.xcodeproj \
    -scheme Dimmy \
    -configuration "$CONFIG" \
    -destination 'platform=macOS,arch=arm64' \
    build \
    | tail -20

BUILT_DIR=$(xcodebuild \
    -project Dimmy.xcodeproj \
    -scheme Dimmy \
    -configuration "$CONFIG" \
    -showBuildSettings 2>/dev/null \
  | awk -F' = ' '/^ *BUILT_PRODUCTS_DIR/ {print $2}' | head -1)

SRC="$BUILT_DIR/Dimmy.app"
DST="/Applications/Dimmy.app"

if [[ ! -d "$SRC" ]]; then
    echo "[install] ERROR: $SRC not found" >&2
    exit 1
fi

echo "[install] replacing $DST"
rm -rf "$DST"
cp -R "$SRC" "$DST"

echo "[install] verifying code signature"
codesign --verify --deep --strict "$DST"

echo "[install] launching"
open "$DST"

echo "[install] done."
```

Make it executable:

```bash
chmod +x scripts/macos/install-to-applications.sh
```

- [ ] **Step 11.2: Create the dev-loop doc**

Write `docs/dev/macos-development.md`:

```markdown
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
```

- [ ] **Step 11.3: Commit**

```bash
git add scripts/macos/install-to-applications.sh docs/dev/macos-development.md
git commit -m "$(cat <<'EOF'
chore(macos): add install-to-applications script + dev-loop doc

Standardizes the dev loop on /Applications/Dimmy.app to avoid TCC
path-mismatch confusion during iterative builds. The script builds,
replaces the /Applications bundle, verifies codesign, and launches.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Final verification

- [ ] **Full build, Debug:**

```bash
cd platforms/macos && xcodebuild -project Dimmy.xcodeproj -scheme Dimmy -configuration Debug -destination 'platform=macOS,arch=arm64' build 2>&1 | tail -10
```

Expected: `** BUILD SUCCEEDED **`.

- [ ] **Install + runtime SelfTests:**

```bash
scripts/macos/install-to-applications.sh
sleep 5
pgrep -x Dimmy >/dev/null && echo "PASS: app running, SelfTests passed" || echo "FAIL: app crashed"
```

- [ ] **Manual QA checklist (documented in spec):**

Open the Diagnostics pane and verify every field populates correctly. Then run the 9-step manual QA sequence from the spec (Section "Testing") end-to-end.

- [ ] **Push branch for review (do NOT merge yet):**

```bash
git push -u origin feat/macos-onboarding-redesign
```

---

## Rollback

Every task is a single commit. If a task breaks the build or regresses behaviour, revert with:

```bash
git revert HEAD
```

The WIP baseline commit (`chore(macos): snapshot WIP onboarding/permissions/hotkey rewrite`) is preserved on the branch and can be reverted too if the whole redesign needs to be dropped.
