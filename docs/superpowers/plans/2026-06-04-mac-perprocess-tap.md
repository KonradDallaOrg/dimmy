# Mac per-process AudioProcessTap (Tahoe regression workaround) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the global `monoGlobalTapButExcludeProcesses` AudioProcessTap with a per-process `monoMixdownOfProcesses` tap that enumerates audio-active PIDs at meeting start and rebuilds the tap when the active set changes. Restores `audio_system.ogg` content on macOS 26.x Tahoe where the global tap silently delivers zero-amplitude buffers.

**Architecture:** The Core Audio HAL exposes a per-process tap initializer that delivers real audio on Tahoe (empirically confirmed 2026-06-04: probe on `afplay` PID returned `peak=0.0900`, while the global variant on the same machine + same audio returned `peak=0.0000` across 6 aggregate-device configurations). Replace the single self-excluding global tap with a tap initialized from a curated list of PIDs that are *currently producing audio output*. Enumerate via `kAudioHardwarePropertyProcessObjectList`, filter to processes whose `kAudioProcessPropertyIsRunning == 1` and `kAudioProcessPropertyPID != self.pid`. Re-enumerate on a 3 s tick; rebuild the tap when the active-PID set changes (cheap — full teardown + recreate runs in <50 ms; no audible glitch because Dimmy's own mic chain isn't disturbed). Keep the existing `TapAutoStartKey=false` + explicit `AudioDeviceStart` workaround for the Tahoe HAL register regression.

**Tech Stack:** Swift (CoreAudio HAL APIs), no new Rust FFI required (existing `dimmy_push_loopback_audio` is reused unchanged), no ABI golden fixture updates.

**Branch:** `fix/mac-perprocess-tap` (off `origin/staging` — must be created AFTER PR #105 merges, since PR #105 lands the `peak_abs` diagnostic this plan depends on for verification).

**Pre-flight reading:**
- `platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift` (lines 27–192 = current global tap; lines 460–600 = probes including the per-process probe that proved the fix viable)
- `platforms/macos/Dimmy/Services/SystemAudioCaptureService.swift` (lines 94–146 = tap-vs-SCKit dispatch; the SilentEnv cache hint stays as belt-and-braces for the next regression class)
- `core/src/audio.rs:743-847` = `[Audio/loopback] 5s tick` is the verification surface — `peak_abs` must rise above ~0.01 across all routes for the fix to ship

**Out of scope:**
- UI app-picker for explicit per-app capture (deferred — auto-enumeration covers 90% of cases)
- Multi-tap aggregation (one tap with multiple PIDs already handles the common "Chrome + Slack + Zoom all open" case)
- Linux / Windows changes (cross-platform unaffected — Windows uses cpal loopback, Linux uses PipeWire/PulseAudio paths)
- Fixing the SCStream-rate-race bug from `fix/mac-system-audio-rate` Task 1 (separate follow-up: SCStream is no longer the primary code path once this lands)

---

### Task 1: Audio-active process enumeration helper

**Files:**
- Modify: `platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift` (add new static method + private helpers, right after the existing `processObject(for:)` helper around line 263)

- [ ] **Step 1: Read the existing `processObject(for:)` helper for reference**

Read `platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift:261-275`. The new helper uses the same property-data idioms.

- [ ] **Step 2: Add the enumeration helper**

Insert immediately after the existing `processObject(for:)` helper:

```swift
    /// Enumerate every audio process object the HAL knows about. Returns
    /// AudioObjectIDs (not PIDs) usable directly in
    /// `CATapDescription(monoMixdownOfProcesses:)`.
    ///
    /// Each entry is a process that has at least one IO context registered
    /// with coreaudiod (currently or in the recent past). To narrow further
    /// to "actively producing output", check `kAudioProcessPropertyIsRunning`
    /// per object — see `audioActiveProcessObjects(excludingSelf:)`.
    fileprivate static func allAudioProcessObjects() -> [AudioObjectID] {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioHardwarePropertyProcessObjectList,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var size: UInt32 = 0
        var err = AudioObjectGetPropertyDataSize(
            AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size)
        guard err == noErr, size > 0 else { return [] }
        let count = Int(size) / MemoryLayout<AudioObjectID>.size
        var ids = [AudioObjectID](repeating: 0, count: count)
        err = AudioObjectGetPropertyData(
            AudioObjectID(kAudioObjectSystemObject), &address, 0, nil, &size, &ids)
        guard err == noErr else { return [] }
        return ids
    }

    /// Read the `pid_t` for an audio process object.
    fileprivate static func pid(forAudioObject obj: AudioObjectID) -> pid_t? {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioProcessPropertyPID,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var pid: pid_t = -1
        var size = UInt32(MemoryLayout<pid_t>.size)
        let err = AudioObjectGetPropertyData(obj, &address, 0, nil, &size, &pid)
        return (err == noErr && pid > 0) ? pid : nil
    }

    /// True when the process is currently producing audio (has an active
    /// IO context). False otherwise — including processes that have an
    /// audio object but aren't actively playing. Used to prune the tap
    /// list to the apps we actually want to capture.
    fileprivate static func isRunning(_ obj: AudioObjectID) -> Bool {
        var address = AudioObjectPropertyAddress(
            mSelector: kAudioProcessPropertyIsRunning,
            mScope: kAudioObjectPropertyScopeGlobal,
            mElement: kAudioObjectPropertyElementMain)
        var value: UInt32 = 0
        var size = UInt32(MemoryLayout<UInt32>.size)
        let err = AudioObjectGetPropertyData(obj, &address, 0, nil, &size, &value)
        return err == noErr && value != 0
    }

    /// Build the tap input list: every audio-active process EXCEPT Dimmy
    /// itself. Returned as AudioObjectIDs ready for
    /// `CATapDescription(monoMixdownOfProcesses:)`.
    ///
    /// Empty result means no app is currently producing audio. The caller
    /// should treat this as "no-op for now" (don't create a tap) and re-poll
    /// later — the periodic re-enumerate tick in `start()` will pick up the
    /// first audio-producing app within ~3 s.
    fileprivate static func audioActiveProcessObjects(excludingSelf selfPid: pid_t) -> [AudioObjectID] {
        let all = allAudioProcessObjects()
        return all.filter { obj in
            guard let p = pid(forAudioObject: obj), p != selfPid else { return false }
            return isRunning(obj)
        }
    }
```

- [ ] **Step 3: Write a smoke test that calls the enumerator at app launch**

The HAL helpers can't be unit-tested without coreaudiod running, so add a one-shot env-gated probe instead:

In `AppDelegate.swift`, near the existing `DIMMY_TAP_PROBE` block, add:

```swift
        if #available(macOS 14.4, *),
           ProcessInfo.processInfo.environment["DIMMY_TAP_PROBE_ENUM"] == "1" {
            DispatchQueue.global(qos: .userInteractive).async {
                SystemAudioProcessTap.runEnumerationProbe()
            }
        }
```

And in `SystemAudioProcessTap.swift`, add:

```swift
    /// Diagnostic (`DIMMY_TAP_PROBE_ENUM=1`): log the current audio-active
    /// process set. Sanity-checks the enumeration helper without
    /// instantiating a tap.
    static func runEnumerationProbe() {
        let selfPid = ProcessInfo.processInfo.processIdentifier
        let objs = audioActiveProcessObjects(excludingSelf: selfPid)
        NSLog("[EnumProbe] selfPid=%d active_audio_processes=%d", selfPid, objs.count)
        for obj in objs {
            let p = pid(forAudioObject: obj) ?? -1
            NSLog("[EnumProbe]   audioObjectID=%u pid=%d", obj, p)
        }
    }
```

- [ ] **Step 4: Build + run the probe locally**

```bash
cd /Users/francesco.fiorentino/personal/dimmy/core && ~/.cargo/bin/cargo build --release --lib --target aarch64-apple-darwin --features local-stt,local-llm
cd /Users/francesco.fiorentino/personal/dimmy && xcodebuild -project platforms/macos/Dimmy.xcodeproj -scheme Dimmy -configuration Debug -derivedDataPath platforms/macos/build/DerivedData -destination 'platform=macOS' build > /tmp/xcodebuild.log 2>&1
```

Play any audio (Spotify / YouTube / `afplay /System/Library/Sounds/Submarine.aiff`) then:

```bash
DIMMY_TAP_PROBE_ENUM=1 platforms/macos/build/DerivedData/Build/Products/Debug/Dimmy.app/Contents/MacOS/Dimmy 2>&1 | grep EnumProbe
```

Expected: at least one `audioObjectID=N pid=N` line for the audio-playing app.

- [ ] **Step 5: Commit**

```bash
git add platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift platforms/macos/Dimmy/AppDelegate.swift
git commit -m "feat(mac/tap): enumerate audio-active process objects

New helpers in SystemAudioProcessTap:
- allAudioProcessObjects() — list every HAL audio process
- pid(forAudioObject:) — translate object → pid
- isRunning() — true when process has an active IO context
- audioActiveProcessObjects(excludingSelf:) — composed: returns the
  AudioObjectIDs ready to pass into CATapDescription(monoMixdownOfProcesses:)

Env-gated probe DIMMY_TAP_PROBE_ENUM=1 logs the current set. Wiring
into the production tap happens in the next commit."
```

---

### Task 2: Switch `start()` from global to per-process tap

**Files:**
- Modify: `platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift:55-185` (`start()` body)

- [ ] **Step 1: Read the current `start()` for the exact line range**

Read `platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift:55-185`. The tap construction at lines 58-80 is the only block that needs to change. Aggregate device + IO proc setup (lines 96-184) stays.

- [ ] **Step 2: Replace the global-tap construction with per-process**

Replace lines 58-80 (the block starting `// Exclude Dimmy's own output from the global tap` and ending at the `tapID = newTap` assignment) with:

```swift
        // Per-process tap (Tahoe workaround): enumerate every audio-active
        // process except Dimmy itself, then build a mono mixdown of their
        // outputs. The historic global `monoGlobalTapButExcludeProcesses`
        // variant silently delivers zero-amplitude buffers on macOS 26.x
        // — verified by 6-config aggregate probe 2026-06-04: every variant
        // returned peak=0.0000. The per-process variant on the same
        // machine returned peak=0.0900 (real audio).
        let selfPid = ProcessInfo.processInfo.processIdentifier
        let activeObjects = Self.audioActiveProcessObjects(excludingSelf: selfPid)
        guard !activeObjects.isEmpty else {
            // No app is currently producing audio. Don't create a dead tap;
            // SystemAudioCaptureService will re-call start() when the
            // periodic re-enum tick (Task 3) detects new activity, OR the
            // user can pause + resume the meeting to retrigger.
            NSLog("[SystemAudio/tap] no audio-active processes at start() — deferring tap creation")
            return false
        }
        NSLog("[SystemAudio/tap] tapping %d audio-active process(es)", activeObjects.count)

        let description = CATapDescription(monoMixdownOfProcesses: activeObjects)
        description.uuid = UUID()
        description.name = "Dimmy System Audio"
        description.muteBehavior = .unmuted
        description.isPrivate = true
        description.isExclusive = false

        var newTap = AudioObjectID(kAudioObjectUnknown)
        var err = AudioHardwareCreateProcessTap(description, &newTap)
        guard err == noErr, newTap != AudioObjectID(kAudioObjectUnknown) else {
            NSLog("[SystemAudio/tap] AudioHardwareCreateProcessTap failed: %d", err)
            return false
        }
        tapID = newTap
        currentTapPidSet = Set(activeObjects.compactMap { Self.pid(forAudioObject: $0) })
```

- [ ] **Step 3: Add the `currentTapPidSet` stored property**

In the property declarations near the top of the class (~line 34-46), add:

```swift
    /// PID set the current tap was built from. Used by the re-enumerate
    /// tick (Task 3) to detect "the active-audio process set changed,
    /// rebuild the tap" without paying for a teardown when nothing changed.
    private var currentTapPidSet: Set<pid_t> = []
```

- [ ] **Step 4: Clear `currentTapPidSet` in `teardown()`**

In the existing `teardown()` (around line 243), add at the start of the function body:

```swift
        currentTapPidSet = []
```

- [ ] **Step 5: Build + smoke-test locally**

```bash
cd /Users/francesco.fiorentino/personal/dimmy/core && ~/.cargo/bin/cargo build --release --lib --target aarch64-apple-darwin --features local-stt,local-llm
cd /Users/francesco.fiorentino/personal/dimmy && xcodebuild -project platforms/macos/Dimmy.xcodeproj -scheme Dimmy -configuration Debug -derivedDataPath platforms/macos/build/DerivedData -destination 'platform=macOS' build > /tmp/xcodebuild.log 2>&1
```

Then start a meeting in the launched Debug.app with audio playing in another app (Spotify / YouTube / `afplay`):

1. Launch Dimmy Debug.app
2. Start playing audio in another app
3. Open Dimmy meeting window, click Record
4. Wait 15 s
5. Click Stop
6. `tail -50 ~/Library/Application\ Support/dimmy/dimmy.log | grep "Audio/loopback"`

Expected: `peak_abs` field shows non-zero values (≥ 0.01) on at least one tick.

- [ ] **Step 6: Commit**

```bash
git add platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift
git commit -m "fix(mac/tap): switch global tap to per-process mixdown

CATapDescription(monoGlobalTapButExcludeProcesses:) returns zero-
amplitude buffers on macOS 26.x Tahoe. Replaced with
CATapDescription(monoMixdownOfProcesses:) built from the live
audio-active process list. Per-process variant verified working on
the same machine + same audio that the global variant could not
capture (probe 2026-06-04: peak=0.0900 vs peak=0.0000).

start() now defers tap creation when no app is producing audio; the
re-enumerate tick (next commit) picks up activity within ~3 s.
currentTapPidSet records what the current tap was built from so the
re-enum tick can detect membership changes cheaply."
```

---

### Task 3: Periodic re-enumerate + tap rebuild

**Files:**
- Modify: `platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift` (add timer + rebuild logic)

- [ ] **Step 1: Add the re-enumerate timer + handler**

Add as a new method in the class body, near `start()` / `stop()`:

```swift
    /// Re-enumerate audio-active processes every 3 s; rebuild the tap when
    /// the active set changes. Cheap: same-PID-set tick is two property
    /// reads + a Set equality. Different-set tick is a full teardown +
    /// re-start (~50 ms; no audible artifact because the mic chain is on
    /// a separate cpal stream).
    ///
    /// Trade-off: a brand-new audio-producing app takes up to 3 s to be
    /// added to the capture set. We accept that latency rather than
    /// poll faster — the IO-proc-fire pattern means audio doesn't
    /// disappear, it just isn't recorded for those 3 s. For meeting use
    /// (Zoom / Meet / Teams launched once at meeting start) this is a
    /// one-off cost paid before the user joins.
    private var rescanTimer: DispatchSourceTimer?

    private func startRescanTimer() {
        rescanTimer?.cancel()
        let timer = DispatchSource.makeTimerSource(queue: ioQueue)
        timer.schedule(deadline: .now() + 3.0, repeating: 3.0)
        timer.setEventHandler { [weak self] in self?.rescanAndRebuildIfNeeded() }
        timer.resume()
        rescanTimer = timer
    }

    private func stopRescanTimer() {
        rescanTimer?.cancel()
        rescanTimer = nil
    }

    private func rescanAndRebuildIfNeeded() {
        guard running else { return }
        let selfPid = ProcessInfo.processInfo.processIdentifier
        let activeObjects = Self.audioActiveProcessObjects(excludingSelf: selfPid)
        let newPidSet = Set(activeObjects.compactMap { Self.pid(forAudioObject: $0) })
        guard newPidSet != currentTapPidSet else { return }
        NSLog("[SystemAudio/tap] audio-active PID set changed (was %d, now %d) — rebuilding tap",
              currentTapPidSet.count, newPidSet.count)
        let savedHandler = onSamples
        teardown()
        running = false
        // start() reads onSamples; restore it before re-starting.
        onSamples = savedHandler
        _ = start()
    }
```

- [ ] **Step 2: Call `startRescanTimer()` at the end of `start()`**

Find the last line before `return true` in `start()` (currently `NSLog("[SystemAudio/tap] started rate=%d ch=%d", rate, channels)` at line 183) and add IMMEDIATELY after it:

```swift
        startRescanTimer()
```

- [ ] **Step 3: Call `stopRescanTimer()` at the start of `stop()` + `teardown()`**

In `stop()` (line 187), at the start of the function body, add:

```swift
        stopRescanTimer()
```

In `teardown()` (line 243), at the start of the function body, add (BEFORE the `currentTapPidSet = []` line added in Task 2):

```swift
        stopRescanTimer()
```

- [ ] **Step 4: Smoke-test**

```bash
cd /Users/francesco.fiorentino/personal/dimmy/core && ~/.cargo/bin/cargo build --release --lib --target aarch64-apple-darwin --features local-stt,local-llm
cd /Users/francesco.fiorentino/personal/dimmy && xcodebuild -project platforms/macos/Dimmy.xcodeproj -scheme Dimmy -configuration Debug -derivedDataPath platforms/macos/build/DerivedData -destination 'platform=macOS' build > /tmp/xcodebuild.log 2>&1
```

Run scenario:
1. Launch Dimmy.app
2. Start a meeting with NO audio playing in other apps (tests "defer until audio" path)
3. Within 15 s, open Spotify + start audio playback (tests "re-enum detects new app, rebuilds tap")
4. Wait 20 s with audio playing
5. Stop meeting
6. `grep -E "Audio/loopback|tap.*rebuilding|tapping" ~/Library/Application\ Support/dimmy/dimmy.log | tail -20`

Expected log shape:
```
[SystemAudio/tap] no audio-active processes at start() — deferring tap creation
... (mic still records, peak_abs lines from cpal primary stream continue)
[SystemAudio/tap] audio-active PID set changed (was 0, now 1) — rebuilding tap
[SystemAudio/tap] tapping 1 audio-active process(es)
[SystemAudio/tap] started rate=48000 ch=1
[Audio/loopback] 5s tick: ... peak_abs=0.0NNN ...  # NON-ZERO
```

- [ ] **Step 5: Commit**

```bash
git add platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift
git commit -m "feat(mac/tap): re-enumerate audio-active processes every 3 s

The set of audio-producing apps changes during a meeting (user opens
Slack mid-recording, Zoom call ends and Spotify resumes, etc.).
DispatchSourceTimer on the ioQueue tracks the active-PID set and
rebuilds the tap when membership changes — a few-ms cost per change.
Same-set ticks are cheap (Set equality after one HAL list read).

Resolves the deferred-tap edge case from the previous commit:
meeting started before any audio plays gets a working tap as soon
as the first audio source appears."
```

---

### Task 4: Update SystemAudioCaptureService dispatch for empty-tap case

**Files:**
- Modify: `platforms/macos/Dimmy/Services/SystemAudioCaptureService.swift:94-146` (the `start()` body)

Background: the tap can now legitimately return `false` from `start()` when no audio is playing yet. The current code path treats `tap.start() == false` as a fall-through to SCKit. With the rescan timer, we want to KEEP the tap object alive and let it self-recover when audio appears — falling through to SCKit would defeat the new design.

- [ ] **Step 1: Read existing dispatch**

Read `platforms/macos/Dimmy/Services/SystemAudioCaptureService.swift:94-146`.

- [ ] **Step 2: Distinguish "tap creation failed" vs "tap deferred (no audio yet)"**

Add a new return type to the tap's `start()` so the service can tell the two apart. In `SystemAudioProcessTap.swift`, replace `func start() -> Bool` with:

```swift
    /// Outcome of a `start()` attempt. `.live` means the tap is recording.
    /// `.deferred` means no audio source was active at start; the rescan
    /// timer will pick up the first source within ~3 s and self-promote
    /// to `.live` (the caller should keep the instance alive). `.failed`
    /// means a HAL error — caller should fall back to SCStream.
    enum StartOutcome { case live, deferred, failed }

    func start() -> StartOutcome {
```

Then change the existing `return false` lines:
- After `[SystemAudio/tap] AudioHardwareCreateProcessTap failed` → `return .failed`
- After every other HAL-failure log + teardown → `return .failed`
- The new "no audio-active" path → `return .deferred` (but ALSO call `startRescanTimer()` before returning so the rescan tick can still fire and recover)

And the success path → `return .live`.

Then update the rescan handler to handle the deferred case:

```swift
    private func rescanAndRebuildIfNeeded() {
        let selfPid = ProcessInfo.processInfo.processIdentifier
        let activeObjects = Self.audioActiveProcessObjects(excludingSelf: selfPid)
        let newPidSet = Set(activeObjects.compactMap { Self.pid(forAudioObject: $0) })

        if running {
            guard newPidSet != currentTapPidSet else { return }
            NSLog("[SystemAudio/tap] PID set changed (%d → %d) — rebuilding",
                  currentTapPidSet.count, newPidSet.count)
            let savedHandler = onSamples
            teardown()
            running = false
            onSamples = savedHandler
            _ = start()
        } else {
            // Deferred state: not yet running, no tap created.
            guard !activeObjects.isEmpty else { return }
            NSLog("[SystemAudio/tap] audio now active — promoting deferred tap to live")
            _ = start()
        }
    }
```

Note: when running=false, the rescan timer was kicked off from the deferred `start()`. Make sure the timer keeps firing while in deferred state (don't `stopRescanTimer()` in `teardown()` if the next state will be `.deferred` — easier: just unconditionally start the timer when entering deferred OR live, stop it only on user-initiated `stop()`).

- [ ] **Step 3: Update `SystemAudioCaptureService.start()` to handle the three outcomes**

In `SystemAudioCaptureService.swift`, replace the tap-start block (currently at lines 118-139, the `if tap.start() { ... } else { ... }` branch) with:

```swift
            switch tap.start() {
            case .live:
                processTap = tap
                isRunning = true
                UserDefaults.standard.removeObject(
                    forKey: Self.tapSilentEnvDefaultsKey)
                NSLog("[SystemAudio] capture via Core Audio process tap")
                return true
            case .deferred:
                processTap = tap
                isRunning = true
                NSLog("[SystemAudio] tap deferred (no audio source yet) — rescan will promote when audio appears")
                return true
            case .failed:
                UserDefaults.standard.set(
                    currentEnv, forKey: Self.tapSilentEnvDefaultsKey)
                NSLog("[SystemAudio] tap creation failed — caching env=%@ → ScreenCaptureKit fallback",
                    currentEnv)
            }
```

Drop the existing `hasReceivedAudio` 800 ms wait — the rescan timer subsumes its purpose.

- [ ] **Step 4: Build + test the deferred path**

```bash
cd /Users/francesco.fiorentino/personal/dimmy/core && ~/.cargo/bin/cargo build --release --lib --target aarch64-apple-darwin --features local-stt,local-llm
cd /Users/francesco.fiorentino/personal/dimmy && xcodebuild -project platforms/macos/Dimmy.xcodeproj -scheme Dimmy -configuration Debug -derivedDataPath platforms/macos/build/DerivedData -destination 'platform=macOS' build > /tmp/xcodebuild.log 2>&1
```

Test: launch app in silence, start meeting, then start audio after 5 s. Look for `tap deferred` → `audio now active — promoting deferred tap to live` → `peak_abs > 0`.

- [ ] **Step 5: Commit**

```bash
git add platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift platforms/macos/Dimmy/Services/SystemAudioCaptureService.swift
git commit -m "fix(mac/tap): handle deferred-tap state (no audio source yet)

SystemAudioProcessTap.start() now returns StartOutcome (.live /
.deferred / .failed). Deferred keeps the tap instance + rescan timer
alive so it self-promotes to live the first time an app starts
producing audio (~3 s latency). Avoids the false-positive SCKit
fallback when the user starts a meeting before opening their
videoconf app.

Drops the historical 800 ms 'hasReceivedAudio' wait — superseded by
the rescan tick. Cache write to DimmySystemAudioTapSilentEnv now
fires only on HAL-level failure, not on deferred state."
```

---

### Task 5: Pre-flight + push

- [ ] **Step 1: Full pre-push checklist**

From `core/`:

```bash
~/.cargo/bin/cargo fmt --check && \
~/.cargo/bin/cargo clippy --features local-stt,local-llm -- -D warnings && \
~/.cargo/bin/cargo test --lib --features local-stt,local-llm
```

Note: existing `ffi::tests::process_with_llm_graceful_no_key` fails on staging baseline (OS-keyring leaks real Groq key into test process). Unrelated. Ignore.

- [ ] **Step 2: Mac preflight (full xcodebuild + SelfTests launch gate)**

```bash
cd /Users/francesco.fiorentino/personal/dimmy && PATH="$HOME/.cargo/bin:$PATH" ./scripts/dev/preflight-mac.sh
```

Expected: `✓ All 6 pre-flight steps passed. Safe to push.`

- [ ] **Step 3: Manual end-to-end verification on signed staging build**

The Tahoe regression may behave differently on ad-hoc-signed Debug builds vs Developer-ID-signed staging builds. Confirm fix works on the latter before declaring done:

1. Push branch to `origin`
2. Cut a `v0.6.NN-staging.M` tag → `staging-tester.yml` builds + signs DMG
3. Install side-by-side, repro original symptom (start meeting, play audio), confirm `peak_abs > 0.01` in `~/Library/Application Support/dimmy-staging/dimmy.log`

```bash
git push -u origin fix/mac-perprocess-tap
# After GitHub PR review + merge to staging:
git tag v0.6.NN-staging.M <merge-sha>
git push origin v0.6.NN-staging.M
```

- [ ] **Step 4: Update CLAUDE.md known-bugs section**

Append to `docs/dev/known-bugs.md` (or create the entry if missing):

```markdown
### MACOS-004 — global AudioProcessTap zero-amplitude on Tahoe (FIXED 2026-06-NN)

**Symptom:** `audio_system.ogg` shipped as Vorbis silence (~5-7 KB) on every Mac meeting on macOS 26.x. `[Audio/loopback] 5s tick` showed pushes flowing but `peak_abs=0.0000`.

**Root cause:** `CATapDescription(monoGlobalTapButExcludeProcesses:)` returns zero-amplitude buffers on macOS 26.2. Verified across 6 aggregate-device configurations. `CATapDescription(monoMixdownOfProcesses:)` on the same machine returns real audio.

**Fix:** PR #NNN — enumerate audio-active process objects every 3 s, build the tap from that list. Falls back to deferred state when no app is producing audio at meeting start.

**Re-applying the diagnostic:** `DIMMY_TAP_PROBE_ENUM=1`, `DIMMY_TAP_PROBE_PID=<pid>`, `DIMMY_TAP_PROBE_MULTI=1` — see `SystemAudioProcessTap.swift`.
```

---

### Task 6: Follow-ups (deferred, document only)

These are NOT in scope for this plan but should be tracked once the fix ships:

- [ ] **App-picker UI**: user explicitly picks which app to capture (e.g. "tap only Zoom, ignore Spotify"). Future feature, low priority — auto-enumerate handles 90% of cases. Would need a new `meeting_audio_app_allowlist` config field.

- [ ] **Multi-process meeting recap accuracy**: when both Zoom AND Slack-huddle are open, the tap captures both. Recap may need a way to attribute audio to the actual meeting app. Probably not worth fixing until users report it.

- [ ] **Re-enable SCKit fallback gating**: with the new per-process tap, the SCKit fallback only fires on HAL-level failure. The historic `hasReceivedAudio` 800 ms wait is gone. If a new Tahoe sub-regression turns up, the rescan timer might cover it but the SilentEnv cache won't fire. Consider adding a "X failed rescan ticks → give up + fall through to SCKit" counter once we have field data.

- [ ] **SCStream-rate race**: separate bug from `fix/mac-system-audio-rate` Task 1 (the device-native rate FFI). Plan exists in `docs/superpowers/plans/2026-06-04-mac-system-audio-rate.md` — but since this branch removes SCStream as the primary path, Fix #1 from that plan becomes lower priority. Decide later whether to ship the race fix as belt-and-braces.

---

## Verification matrix

| Scenario | Expected `peak_abs` in `[Audio/loopback] 5s tick` |
|---|---|
| Meeting started, no audio playing | 0.0 (deferred state) |
| Audio playing in Spotify when meeting starts (Spotify is default output device's client) | > 0.01 |
| Meeting starts, then Spotify launched + plays audio 5 s in | > 0.01 within ~8 s of meeting start (rescan tick + first audio buffers) |
| Spotify quit mid-meeting, no other audio source | back to 0.0 after rescan tick + 2 s (next 5 s tick reads the empty buffer state) |
| Spotify quit + YouTube started in browser 10 s later | > 0.01 within ~15 s (rescan picks up Chrome PID, tap rebuilds) |
| User in Zoom call (peer voice = audio source) | > 0.01 throughout |
| User in BT-HFP call on Jabra (peer voice on HFP private bus, system output empty) | TBD — may still be 0.0 if HFP doesn't surface to the audio process list. If so, this is a separate fix path (per-app capture of the call client app, e.g. Slack / FaceTime / Teams). |

## Key files

| File | Why |
|---|---|
| `platforms/macos/Dimmy/Services/SystemAudioProcessTap.swift` | The whole tap implementation lives here. Tasks 1–4 all touch it. |
| `platforms/macos/Dimmy/Services/SystemAudioCaptureService.swift:94-146` | Tap-vs-SCKit dispatch. Task 4 updates the start() switch on StartOutcome. |
| `platforms/macos/Dimmy/AppDelegate.swift:68-80` | Env-gated probe wiring. Task 1 adds `DIMMY_TAP_PROBE_ENUM`. |
| `core/src/audio.rs:743-847` | `[Audio/loopback] 5s tick` log line — the verification surface. Untouched by this plan; consumes the per-process tap output transparently via existing `dimmy_push_loopback_audio` FFI. |
| `docs/dev/known-bugs.md` | Task 5 step 4 — document the fix + diagnostic env vars. |

## Empirical evidence backing this plan

- `2026-06-04 12:14:23` production meeting (built-in mic + built-in speakers, user confirmed audio playing during call) — `peak_abs=0.0000` across 5 ticks. Rules out HFP-route hypothesis.
- `2026-06-04 12:20:27` `DIMMY_TAP_PROBE=1` probe (empty exclude list, otherwise identical to production tap) — `samples=33600 peak=0.0000`. Rules out self-exclude hypothesis.
- `2026-06-04 12:21:13–12:21:35` `DIMMY_TAP_PROBE_MULTI=1` 6-config probe — all 6 variants `peak=0.0000`. Confirms Tahoe regression hits global tap regardless of aggregate config.
- `2026-06-04 12:31:42` `DIMMY_TAP_PROBE_PID=60371` (afplay playing 30 s tone) — `samples=80000 peak=0.0900 rate=48000`. **This is the empirical proof that per-process tap works.**

Branch + commits at `fix/mac-system-audio-rate` head:
- `17a5a30` device-native rate atomic
- `f2839e4` `dimmy_get_active_mic_device_rate` FFI
- `1bd3cdd` Swift Plan-A wiring
- `d1401c0` `peak_abs` diagnostic (the surface that made this plan possible)

PR #105 (against `staging`) carries those four. This plan branches from `staging` AFTER #105 merges so the diagnostic is available for verification.
