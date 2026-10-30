import AppKit
import CoreGraphics

@MainActor
final class TextInjector {
    static let shared = TextInjector()
    private init() {}

    func injectText(_ text: String) {
        let pasteboard = NSPasteboard.general
        let previousContents = pasteboard.string(forType: .string)

        pasteboard.clearContents()
        pasteboard.setString(text, forType: .string)

        simulatePaste()

        // Restore previous clipboard after a short delay
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) {
            if let previous = previousContents {
                pasteboard.clearContents()
                pasteboard.setString(previous, forType: .string)
            }
        }
    }

    private var lastTypedPid: pid_t?

    /// Live-dictation segment: optional backspaces (a correction), then the
    /// delta typed at the cursor. Same contract as Windows OnStreamingSegmentFinalized.
    func typeStreamed(delta: String, backspace: Int) {
        let pid = NSWorkspace.shared.frontmostApplication?.processIdentifier
        // Backspacing after the user switched apps would eat THEIR text in the
        // new app, not our correction.
        if backspace > 0, let pid, pid == lastTypedPid {
            let source = CGEventSource(stateID: .hidSystemState)
            for _ in 0..<min(backspace, 200) {
                CGEvent(keyboardEventSource: source, virtualKey: 0x33, keyDown: true)?.post(tap: .cghidEventTap)
                CGEvent(keyboardEventSource: source, virtualKey: 0x33, keyDown: false)?.post(tap: .cghidEventTap)
            }
        }
        if !delta.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            let trimmed = delta.replacingOccurrences(of: "\\s+$", with: "", options: .regularExpression)
            typeUnicode(trimmed + " ")
        }
        lastTypedPid = pid
    }

    private func typeUnicode(_ text: String) {
        let source = CGEventSource(stateID: .hidSystemState)
        let units = Array(text.utf16)
        var i = 0
        while i < units.count {
            var end = min(i + 20, units.count)
            // Never split a surrogate pair across events.
            if end < units.count, UTF16.isLeadSurrogate(units[end - 1]) { end -= 1 }
            var chunk = Array(units[i..<end])
            for down in [true, false] {
                let ev = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: down)
                ev?.keyboardSetUnicodeString(stringLength: chunk.count, unicodeString: &chunk)
                ev?.post(tap: .cghidEventTap)
            }
            i = end
        }
    }

    private func simulatePaste() {
        let source = CGEventSource(stateID: .hidSystemState)

        let keyDown = CGEvent(keyboardEventSource: source, virtualKey: 0x09, keyDown: true) // 'v' key
        keyDown?.flags = .maskCommand
        keyDown?.post(tap: .cghidEventTap)

        let keyUp = CGEvent(keyboardEventSource: source, virtualKey: 0x09, keyDown: false)
        keyUp?.flags = .maskCommand
        keyUp?.post(tap: .cghidEventTap)
    }
}
