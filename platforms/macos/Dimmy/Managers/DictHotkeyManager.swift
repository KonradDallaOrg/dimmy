import AppKit
import ApplicationServices
import Carbon.HIToolbox
import CoreGraphics

/// Secondary global hotkey dedicated to "add selected text to user
/// dictionary". Independent of `HotkeyManager` — the latter owns the
/// modifier-only dictation chord (flagsChanged events); this one
/// listens for modifier+key keyDown events.
///
/// Two-path trigger model, picked at `start()`:
///
///   1. **CGEventTap (preferred)** — needs Accessibility. Lets us
///      synthesize Cmd+C against the focused app so the user just
///      presses the combo on a selection. Full flow:
///        a. Capture pasteboard baseline + stash current text.
///        b. Write sentinel; wait for changeCount bump.
///        c. Synthesize Cmd+C; wait for changeCount bump.
///        d. Read the copied text. Cap at 100 chars. Call
///           dimmy_user_dict_add. Show toast. Restore pasteboard.
///
///   2. **Carbon RegisterEventHotKey (fallback)** — no Accessibility.
///      We cannot synthesize Cmd+C (post-event also gated by
///      Accessibility), so the user must press Cmd+C themselves
///      first, then trigger the dict combo. The handler reads
///      whatever is currently on the pasteboard and adds it. The
///      Services menu provides the same "user-copied" semantics for
///      the right-click path.
///
/// Mirror of Win's `DictHotkeyService` + `App.OnDictHotkeyTriggered`,
/// keeping the user experience identical across platforms — except
/// for the Carbon-fallback caveat, which mirrors the Windows policy
/// "if no accessibility, the user copies first".
@MainActor
final class DictHotkeyManager {
    static let shared = DictHotkeyManager()

    /// Path 1: CGEventTap (Accessibility-gated, lets us auto-Cmd+C)
    private var eventTap: CFMachPort?
    private var runLoopSource: CFRunLoopSource?
    private var wakeObserver: NSObjectProtocol?

    /// Path 2: Carbon RegisterEventHotKey (Accessibility-free fallback)
    private var carbonHotKeyRef: EventHotKeyRef?
    private var carbonEventHandler: EventHandlerRef?

    private weak var appState: AppState?

    private init() {}

    /// Stand up the trigger. Idempotent — second call no-ops.
    /// Picks the best path available given current permissions.
    func start(appState: AppState) {
        self.appState = appState
        if eventTap != nil || carbonHotKeyRef != nil { return }

        if AXIsProcessTrustedWithOptions(nil) {
            installEventTap()
            NSLog("[Dict] CGEventTap path active (Accessibility granted)")

            // macOS disables taps during sleep; re-enable on wake.
            // Same pattern as the dictation HotkeyManager. Only the
            // CGEventTap path needs this — Carbon hotkeys survive
            // sleep without intervention.
            wakeObserver = NSWorkspace.shared.notificationCenter.addObserver(
                forName: NSWorkspace.didWakeNotification,
                object: nil, queue: .main
            ) { [weak self] _ in
                Task { @MainActor in
                    guard let self, let tap = self.eventTap else { return }
                    CGEvent.tapEnable(tap: tap, enable: true)
                }
            }
        } else {
            installCarbonHotKey()
            NSLog("[Dict] Carbon RegisterEventHotKey path active (no Accessibility — user must copy before pressing combo)")
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

        if let ref = carbonHotKeyRef {
            UnregisterEventHotKey(ref)
            carbonHotKeyRef = nil
        }
        if let h = carbonEventHandler {
            RemoveEventHandler(h)
            carbonEventHandler = nil
        }
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

    // MARK: - Carbon RegisterEventHotKey fallback

    /// Bridge `HotkeyCombo` modifiers to Carbon bitmask. Carbon uses
    /// its own constants (`cmdKey`, `shiftKey`, …) distinct from
    /// NSEvent.ModifierFlags or CGEventFlags. Values are stable since
    /// classic Mac OS — they're not going anywhere.
    private func carbonModifiers(_ combo: HotkeyCombo) -> UInt32 {
        var m: UInt32 = 0
        if combo.command { m |= UInt32(cmdKey) }
        if combo.shift   { m |= UInt32(shiftKey) }
        if combo.option  { m |= UInt32(optionKey) }
        if combo.control { m |= UInt32(controlKey) }
        return m
    }

    private func installCarbonHotKey() {
        guard let appState else { return }
        let combo = appState.dictHotkey

        // Carbon hotkeys must include at least one modifier; bare keys
        // would collide with normal typing. The default is ⌘⇧D so the
        // common path is fine, but a user-customised combo with no
        // modifier set should be rejected up front so we don't end up
        // claiming every press of the letter key system-wide.
        let mods = carbonModifiers(combo)
        guard mods != 0 else {
            NSLog("[Dict/Carbon] refusing to register modifier-less hotkey — would intercept normal typing")
            return
        }
        // The dict hotkey grammar (Settings) requires a letter, but the
        // shared HotkeyCombo now also models modifier-only chords used by
        // the command hotkey. Defensive guard: never attempt to register
        // a key-less dict combo through Carbon.
        guard let keyCode = combo.keyCode else {
            NSLog("[Dict/Carbon] refusing to register key-less combo")
            return
        }

        // EventHotKeyID: signature is an arbitrary 4-char code unique
        // per app; id is per-hotkey within the app. We only have one
        // dict hotkey, so id=1.
        let signature: OSType = 0x44494D44 // 'DIMD'
        let hotKeyID = EventHotKeyID(signature: signature, id: 1)

        var ref: EventHotKeyRef?
        let regStatus = RegisterEventHotKey(
            UInt32(keyCode),
            mods,
            hotKeyID,
            GetApplicationEventTarget(),
            0,
            &ref
        )
        guard regStatus == noErr, let ref = ref else {
            NSLog("[Dict/Carbon] RegisterEventHotKey failed with status \(regStatus) — combo may collide with system shortcut")
            return
        }
        self.carbonHotKeyRef = ref

        // Install the event handler that fires on hotkey press.
        // Selector is `kEventHotKeyPressed` under `kEventClassKeyboard`.
        var eventType = EventTypeSpec(
            eventClass: OSType(kEventClassKeyboard),
            eventKind: UInt32(kEventHotKeyPressed)
        )
        let selfPtr = Unmanaged.passUnretained(self).toOpaque()

        let handler: EventHandlerUPP = { (_, _, userData) -> OSStatus in
            // No need to inspect the event — we only registered one
            // hotkey under this handler, so any fire IS our combo.
            guard let userData = userData else { return noErr }
            let manager = Unmanaged<DictHotkeyManager>.fromOpaque(userData).takeUnretainedValue()
            Task { @MainActor in await manager.handleCarbonHotKey() }
            return noErr
        }

        var handlerRef: EventHandlerRef?
        let installStatus = InstallEventHandler(
            GetApplicationEventTarget(),
            handler,
            1,
            &eventType,
            selfPtr,
            &handlerRef
        )
        if installStatus == noErr {
            self.carbonEventHandler = handlerRef
        } else {
            NSLog("[Dict/Carbon] InstallEventHandler failed with status \(installStatus)")
            UnregisterEventHotKey(ref)
            self.carbonHotKeyRef = nil
        }
    }

    /// Pasteboard changeCount at the time of the last Carbon-path
    /// trigger. Used to detect "user pressed the combo without doing a
    /// new Cmd+C in between" — a workflow mistake that would otherwise
    /// silently re-process whatever stale text is on the pasteboard.
    private var lastCarbonChangeCount: Int = -1

    /// Carbon path runtime handler. No Accessibility means no synthetic
    /// Cmd+C — we read whatever the user already has on the pasteboard.
    ///
    /// Workflow-mistake detection: if the pasteboard.changeCount is the
    /// same as on the previous trigger, the user pressed the combo
    /// without copying anything new. Show a workflow hint toast instead
    /// of silently re-adding the same text (which would either be a
    /// no-op via dedupe or, worse, add a different word than the one
    /// they THINK they selected).
    @MainActor
    private func handleCarbonHotKey() async {
        let pb = NSPasteboard.general
        let currentChangeCount = pb.changeCount

        guard let raw = pb.string(forType: .string) else {
            NSLog("[Dict/Carbon] pasteboard has no text — workflow hint")
            DictToastWindow.showWorkflowHint(hotkey: appState?.dictHotkey.displayString ?? "⌘⇧D")
            lastCarbonChangeCount = currentChangeCount
            return
        }
        let text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.isEmpty {
            NSLog("[Dict/Carbon] pasteboard text empty after trim — workflow hint")
            DictToastWindow.showWorkflowHint(hotkey: appState?.dictHotkey.displayString ?? "⌘⇧D")
            lastCarbonChangeCount = currentChangeCount
            return
        }
        if text.count > 100 {
            NSLog("[Dict/Carbon] pasteboard text \(text.count) chars — too long, use Settings for phrases")
            return
        }

        // The signature workflow mistake — pressing the combo without a
        // fresh Cmd+C. Surfaces inline at the moment of confusion so
        // the user learns the two-step pattern without reading docs.
        if currentChangeCount == lastCarbonChangeCount {
            NSLog("[Dict/Carbon] pasteboard changeCount unchanged since last press — user didn't re-copy, showing workflow hint")
            DictToastWindow.showWorkflowHint(hotkey: appState?.dictHotkey.displayString ?? "⌘⇧D")
            return
        }
        lastCarbonChangeCount = currentChangeCount

        // Reuse the Services-menu entry point: same "text in hand,
        // skip the probe/copy dance, persist + toast" pipeline.
        await AddToDictionaryFlow.runWithText(text)
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
            NSLog("[Dict] AppState.userDictWords now has \(app.userDictWords.count) entries — Settings list should reflect this within one runloop tick")
        case .alreadyPresent:
            DictToastWindow.showAlreadyPresent(word: text)
            NSLog("[Dict] AppState.userDictWords count unchanged (\(AppState.shared.userDictWords.count)) — word was already present in core")
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

/// Capture whatever text is currently selected in the foreground app,
/// without permanently clobbering the user's pasteboard. The macOS twin of
/// Win's `SelectionCaptureService` and the selection grab for Command Mode.
///
/// Reuses the exact probe → synthesize Cmd+C → wait-for-changeCount → read
/// → restore sequence proven by `AddToDictionaryFlow` above, with two
/// differences: it RETURNS the captured text (instead of adding it to the
/// dictionary), and has NO 100-char cap (Command Mode operates on
/// paragraphs).
///
/// Lives in this file rather than its own so it doesn't need a new
/// project.pbxproj entry — it sits next to `AddToDictionaryFlow`, the flow
/// it mirrors. Accessibility is already granted for the dictation
/// text-injection that ships today, so Command Mode adds no new prompt;
/// when it's somehow absent the synthetic Cmd+C is a no-op, changeCount
/// never bumps, and we return nil (caller falls back to plain dictation).
enum SelectionCaptureFlow {

    private static let probeSentinel = "__DIMMY_CMD_PROBE_v1__"

    /// Public entry point. Tries the Accessibility API first (instant,
    /// no clipboard pollution, no synthetic keystroke); falls back to a
    /// synthetic Cmd+C round-trip when the focused element doesn't
    /// expose `kAXSelectedTextAttribute` (e.g. Electron app, browser
    /// web content, app without AX). Same Accessibility permission
    /// the existing CGEventTap path already requires — no new TCC
    /// prompt.
    @MainActor
    static func capture() async -> String? {
        if let viaAX = captureViaAccessibility() {
            NSLog("[CmdMode] AX path captured \(viaAX.count) chars")
            return viaAX
        }
        return await captureViaSyntheticCopy()
    }

    /// Read the focused element's selected text directly via the AX
    /// tree. No clipboard, no keystroke, microseconds instead of 500
    /// ms. Returns nil for:
    ///   - AX call failure (no focused element, attribute unsupported)
    ///   - empty selection (caller wants nil for "no selection")
    ///   - password fields (AX deliberately refuses)
    /// Callers fall back to `captureViaSyntheticCopy` on nil.
    private static func captureViaAccessibility() -> String? {
        let systemWide = AXUIElementCreateSystemWide()
        var focused: CFTypeRef?
        let focusErr = AXUIElementCopyAttributeValue(
            systemWide,
            kAXFocusedUIElementAttribute as CFString,
            &focused
        )
        guard focusErr == .success, let raw = focused else { return nil }
        // The attribute contract guarantees AXUIElement here; defensive
        // CFTypeID check in case a buggy app returns something else
        // (would crash the force-cast otherwise).
        guard CFGetTypeID(raw) == AXUIElementGetTypeID() else { return nil }
        let element = raw as! AXUIElement

        var selected: CFTypeRef?
        let selErr = AXUIElementCopyAttributeValue(
            element,
            kAXSelectedTextAttribute as CFString,
            &selected
        )
        guard selErr == .success else { return nil }
        return normalizeCapturedText(selected as? String)
    }

    /// Normalize a raw captured string into the contract the caller
    /// expects: nil for "no usable selection" (nil input, empty string,
    /// or whitespace-only); the ORIGINAL string verbatim otherwise so
    /// the user's exact selection survives downstream. Pure, no side
    /// effects, no AX — directly unit-testable.
    static func normalizeCapturedText(_ raw: String?) -> String? {
        guard let raw else { return nil }
        if raw.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return nil
        }
        return raw
    }

    /// Legacy fallback: synthesize Cmd+C, wait for pasteboard.changeCount
    /// to bump, restore the previous clipboard. Same path that shipped
    /// before the AX-first refactor; used when AX returns nil.
    @MainActor
    private static func captureViaSyntheticCopy() async -> String? {
        let pb = NSPasteboard.general
        let previousText = pb.string(forType: .string)

        // Phase 1: write sentinel, wait for changeCount bump.
        let preProbe = pb.changeCount
        pb.clearContents()
        pb.setString(probeSentinel, forType: .string)
        if !(await waitForChangeCountBump(baseline: preProbe, timeoutMs: 200)) {
            NSLog("[CmdMode] sentinel write didn't bump pasteboard — abort")
            return nil
        }

        // Phase 2: synthesize Cmd+C.
        let preCopy = pb.changeCount
        synthesizeCmdC()

        // Phase 3: wait for the app to fulfill the copy.
        if !(await waitForChangeCountBump(baseline: preCopy, timeoutMs: 500)) {
            NSLog("[CmdMode] app didn't update pasteboard (no selection / password field / frozen)")
            restorePasteboard(previousText)
            return nil
        }

        guard let raw = pb.string(forType: .string) else {
            restorePasteboard(previousText)
            return nil
        }
        if raw == probeSentinel {
            restorePasteboard(previousText)
            return nil
        }
        restorePasteboard(previousText)
        return normalizeCapturedText(raw)
    }

    private static func synthesizeCmdC() {
        let src = CGEventSource(stateID: .combinedSessionState)
        let down = CGEvent(keyboardEventSource: src, virtualKey: 0x08 /* kVK_ANSI_C */, keyDown: true)
        down?.flags = .maskCommand
        let up = CGEvent(keyboardEventSource: src, virtualKey: 0x08, keyDown: false)
        up?.flags = .maskCommand
        down?.post(tap: .cghidEventTap)
        up?.post(tap: .cghidEventTap)
    }

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
