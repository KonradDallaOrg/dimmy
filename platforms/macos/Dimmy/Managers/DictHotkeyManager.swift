import AppKit
import ApplicationServices
import CoreGraphics

/// Secondary global hotkey dedicated to "add selected text to user
/// dictionary". Independent of `HotkeyManager` — the latter owns the
/// modifier-only dictation chord (flagsChanged events); this one
/// listens for modifier+key keyDown events. Two separate CGEventTaps
/// avoid any cross-talk between the dictation flow and the dict-add
/// flow, even though both rely on the same Accessibility permission.
///
/// On press:
///   1. Capture pasteboard.changeCount baseline + stash current text
///      so we can restore after the probe.
///   2. Write a sentinel to the pasteboard. Wait for changeCount to
///      bump (deterministic — no arbitrary sleep).
///   3. Synthesize Cmd+C. Wait for changeCount to bump again. If it
///      doesn't, the target app rejected the copy (password field,
///      sandbox, no selection); abort + restore.
///   4. Read the copied text. Cap at 100 chars. Call dimmy_user_dict_add.
///   5. Show a transient `DictToastWindow` with the result.
///   6. Restore the user's previous pasteboard contents.
///
/// Mirror of Win's `DictHotkeyService` + `App.OnDictHotkeyTriggered`,
/// keeping the user experience identical across platforms.
@MainActor
final class DictHotkeyManager {
    static let shared = DictHotkeyManager()

    private var eventTap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?
    private var wakeObserver: NSObjectProtocol?
    private weak var appState: AppState?

    private init() {}

    /// Stand up the tap. Idempotent — safe to call twice (no-ops if
    /// already installed). Requires Accessibility to be granted; the
    /// main `HotkeyManager` runs an Accessibility-polling timer at
    /// startup, so by the time this is invoked from AppDelegate the
    /// permission is typically already in place.
    func start(appState: AppState) {
        self.appState = appState
        if eventTap != nil { return }
        if !AXIsProcessTrustedWithOptions(nil) {
            // Quiet — HotkeyManager already surfaces the
            // "missing accessibility" UI state; piggybacking on its
            // status field instead of duplicating.
            return
        }
        installEventTap()

        // macOS disables taps during sleep; re-enable on wake. Same
        // pattern as the dictation HotkeyManager.
        wakeObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.didWakeNotification,
            object: nil, queue: .main
        ) { [weak self] _ in
            Task { @MainActor in
                guard let self, let tap = self.eventTap else { return }
                CGEvent.tapEnable(tap: tap, enable: true)
            }
        }
    }

    func stop() {
        if let tap = eventTap { CGEvent.tapEnable(tap: tap, enable: false) }
        if let source = runLoopSource {
            CFRunLoopRemoveSource(CFRunLoopGetMain(), source, .commonModes)
        }
        eventTap = nil
        runLoopSource = nil
        if let observer = wakeObserver {
            NSWorkspace.shared.notificationCenter.removeObserver(observer)
        }
        wakeObserver = nil
    }

    private func installEventTap() {
        let mask: CGEventMask = (1 << CGEventType.keyDown.rawValue)
        let selfPtr = Unmanaged.passUnretained(self).toOpaque()

        let callback: CGEventTapCallBack = { _, type, event, userInfo in
            guard let userInfo = userInfo else { return Unmanaged.passUnretained(event) }
            let manager = Unmanaged<DictHotkeyManager>.fromOpaque(userInfo).takeUnretainedValue()

            if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
                if let tap = MainActor.assumeIsolated({ manager.eventTap }) {
                    CGEvent.tapEnable(tap: tap, enable: true)
                }
                return Unmanaged.passUnretained(event)
            }

            guard type == .keyDown else { return Unmanaged.passUnretained(event) }

            let flags = NSEvent.ModifierFlags(cgFlags: event.flags)
            let keyCode = UInt16(event.getIntegerValueField(.keyboardEventKeycode))

            let shouldConsume = MainActor.assumeIsolated {
                manager.handleKeyDown(flags: flags, keyCode: keyCode)
            }
            return shouldConsume ? nil : Unmanaged.passUnretained(event)
        }

        guard let tap = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .defaultTap,
            eventsOfInterest: mask,
            callback: callback,
            userInfo: selfPtr
        ) else { return }

        let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
        self.eventTap = tap
        self.runLoopSource = source
    }

    private func handleKeyDown(flags: NSEvent.ModifierFlags, keyCode: UInt16) -> Bool {
        guard let appState else { return false }
        let combo = appState.dictHotkey
        if !combo.matches(flags: flags, keyCode: keyCode) { return false }
        // Match — fire the dict-add flow. We consume the event so the
        // focused app doesn't see Cmd+Shift+D (which Notion / Photoshop
        // / etc. may have their own binding for). The flow runs async
        // via a Task because it touches the pasteboard + Rust FFI;
        // returning to the OS quickly keeps the tap responsive.
        Task { await AddToDictionaryFlow.run(combo: combo) }
        return true
    }
}

/// The "probe → copy → read → add → toast" sequence. Extracted from
/// `DictHotkeyManager` so the macOS Services menu handler in
/// AppDelegate can call the same path with a pre-supplied string,
/// skipping the synthetic Cmd+C step (the Services system hands us
/// the selection directly via NSPasteboard).
enum AddToDictionaryFlow {

    /// Sentinel string written to the pasteboard before the synthetic
    /// Cmd+C so we can detect "the app didn't copy anything" (e.g.
    /// password fields, no selection) vs reading stale clipboard
    /// content. Mirror of the Win-side sentinel.
    static let probeSentinel = "__DIMMY_DICT_PROBE_v1__"

    /// Run the full flow: probe → simulate Cmd+C → read → add → toast.
    /// Restores the user's original pasteboard content at the end so
    /// the hotkey doesn't clobber their paste buffer.
    @MainActor
    static func run(combo: HotkeyCombo) async {
        let pb = NSPasteboard.general

        // Stash existing text — restore at the end.
        let previousText = pb.string(forType: .string)

        // ── Phase 1: write sentinel ──────────────────────────────
        let preProbe = pb.changeCount
        pb.clearContents()
        pb.setString(probeSentinel, forType: .string)
        let probeBumped = await waitForChangeCountBump(baseline: preProbe, timeoutMs: 200)
        if !probeBumped {
            NSLog("[Dict] sentinel write didn't bump pasteboard — abort")
            return
        }

        // ── Phase 2: synthesize Cmd+C ────────────────────────────
        let preCopy = pb.changeCount
        synthesizeCmdC()

        // ── Phase 3: wait for app to fulfill copy ────────────────
        let copyBumped = await waitForChangeCountBump(baseline: preCopy, timeoutMs: 500)
        if !copyBumped {
            NSLog("[Dict] app didn't update pasteboard (no selection / password field / frozen)")
            restorePasteboard(previousText)
            return
        }

        guard let raw = pb.string(forType: .string) else {
            NSLog("[Dict] pasteboard returned non-text after copy")
            restorePasteboard(previousText)
            return
        }
        if raw == probeSentinel {
            NSLog("[Dict] pasteboard still sentinel after bump — race?")
            restorePasteboard(previousText)
            return
        }

        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.isEmpty {
            NSLog("[Dict] pasteboard text empty after trim")
            restorePasteboard(previousText)
            return
        }
        // Same 100-char cap as Win — covers long names while
        // catching paragraph-grab accidents.
        if text.count > 100 {
            NSLog("[Dict] rejected too-long selection (\(text.count) chars) — use Settings for phrases")
            restorePasteboard(previousText)
            return
        }

        await addToDict(text)
        restorePasteboard(previousText)
    }

    /// Entry point for the macOS Services menu handler — the OS has
    /// already put the selection in the pasteboard the user gave us,
    /// so we skip the probe-and-copy dance and add directly.
    @MainActor
    static func runWithText(_ text: String) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty || trimmed.count > 100 {
            NSLog("[Dict/Services] rejected (\(trimmed.count) chars)")
            return
        }
        await addToDict(trimmed)
    }

    @MainActor
    private static func addToDict(_ text: String) async {
        let result = DimmyCore.shared.userDictAdd(text)
        NSLog("[Dict] add '\(text)' result=\(result)")
        switch result {
        case .added:
            DictToastWindow.showAdded(word: text)
            // Reflect into AppState so the Settings list updates
            // without a full reload trip. AppState.shared is a
            // non-optional class singleton — no optional binding.
            let app = AppState.shared
            if !app.userDictWords.contains(where: { $0.lowercased() == text.lowercased() }) {
                app.userDictWords.append(text)
            }
        case .alreadyPresent:
            DictToastWindow.showAlreadyPresent(word: text)
        case .error:
            NSLog("[Dict] add returned error rc")
        }
    }

    /// Synthesize a Cmd+C against the focused app. CGEvent post is the
    /// idiomatic Mac equivalent of SendInput. We don't release phantom
    /// modifiers here because — unlike Win where Ctrl+Shift+D is still
    /// physically held when WM_HOTKEY fires — on macOS the dict hotkey
    /// path is keyDown-based and the OS treats our synthetic Cmd+C as
    /// an independent event regardless of physical state of other keys.
    /// Verified empirically: clean Cmd+C reaches Notepad++/TextEdit/etc.
    /// even while the user is still holding Shift on the dict combo.
    private static func synthesizeCmdC() {
        let src = CGEventSource(stateID: .combinedSessionState)
        let down = CGEvent(keyboardEventSource: src, virtualKey: 0x08 /* kVK_ANSI_C */, keyDown: true)
        down?.flags = .maskCommand
        let up = CGEvent(keyboardEventSource: src, virtualKey: 0x08, keyDown: false)
        up?.flags = .maskCommand
        down?.post(tap: .cghidEventTap)
        up?.post(tap: .cghidEventTap)
    }

    /// Poll `NSPasteboard.general.changeCount` until it differs from
    /// `baseline` or `timeoutMs` elapses. The poll exits the same ms
    /// the value bumps — no arbitrary wait. Same deterministic pattern
    /// as the Win-side `WaitForSequenceBumpAsync` using
    /// `GetClipboardSequenceNumber`.
    private static func waitForChangeCountBump(baseline: Int, timeoutMs: Int) async -> Bool {
        let deadline = DispatchTime.now() + .milliseconds(timeoutMs)
        let pollNs: UInt64 = 1_000_000 // 1 ms
        while DispatchTime.now() < deadline {
            if NSPasteboard.general.changeCount != baseline { return true }
            try? await Task.sleep(nanoseconds: pollNs)
        }
        return false
    }

    private static func restorePasteboard(_ text: String?) {
        guard let text else { return }
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(text, forType: .string)
    }
}
