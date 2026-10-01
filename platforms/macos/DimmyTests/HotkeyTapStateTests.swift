import AppKit
import XCTest
@testable import Dimmy

// MARK: - HotkeyTapStateTests
//
// The keyboard tap decides on its own thread now, so these pin that the
// decision is the one the main thread used to make:
//
//   1. Ordinary typing is never consumed and never wakes the main thread.
//   2. Each shortcut shape (modifier-only, dictation with a key, command,
//      meeting) consumes exactly its own events and emits its edges in order.
//   3. Every edge carries the time of the key event, so push-to-talk's
//      minimum hold is measured on the keyboard, not on a busy main thread.
//   4. Deciding from another thread while the main thread rebinds is safe.

final class HotkeyTapStateTests: XCTestCase {

    private let t0 = Date(timeIntervalSince1970: 1_000)
    private let kVK_X: UInt16 = 7
    private let kVK_M: UInt16 = 46
    private let kVK_A: UInt16 = 0

    private func combo(control: Bool = false, option: Bool = false, shift: Bool = false,
                       key: UInt16?) -> HotkeyCombo {
        HotkeyCombo(control: control, option: option, command: false, shift: shift,
                    keyCode: key, keyChar: key == nil ? "" : "K")
    }

    private func bound() -> HotkeyTapState {
        let s = HotkeyTapState()
        s.setDictationShortcut(.fnOnly)
        s.setCommandCombo(combo(control: true, shift: true, key: kVK_X))
        s.setMeetingCombo(combo(control: true, option: true, key: kVK_M))
        return s
    }

    func testOrdinaryTypingIsNeverConsumedAndTriggersNothing() {
        let s = bound()
        let none = HotkeyTapState.Decision()
        XCTAssertEqual(s.keyDown(keyCode: kVK_A, flags: [], at: t0), none)
        XCTAssertEqual(s.keyUp(keyCode: kVK_A, at: t0), none)
        // Shift for a capital letter is a flagsChanged of its own.
        XCTAssertEqual(s.flagsChanged([.shift], at: t0), none)
        XCTAssertEqual(s.keyDown(keyCode: kVK_A, flags: [.shift], at: t0), none)
        XCTAssertEqual(s.flagsChanged([], at: t0), none)
    }

    func testModifierOnlyDictationPressAndReleaseAreConsumedWithTheirTimes() {
        let s = bound()
        let t1 = t0.addingTimeInterval(0.4)
        XCTAssertEqual(s.flagsChanged([.function], at: t0),
                       .init(consume: true, actions: [.dictationPressed(t0)]))
        XCTAssertEqual(s.flagsChanged([], at: t1),
                       .init(consume: true, actions: [.dictationReleased(t1)]))
        XCTAssertEqual(s.flagsChanged([.shift], at: t1), HotkeyTapState.Decision())
    }

    func testDictationWithAKeyGoesThroughItsComboMachine() {
        let s = HotkeyTapState()
        var shortcut = ModifierShortcut.controlOption
        shortcut.keyCode = 2
        shortcut.keyChar = "D"
        s.setDictationShortcut(shortcut)
        // The modifiers alone must not start it.
        XCTAssertEqual(s.flagsChanged([.control, .option], at: t0), HotkeyTapState.Decision())
        XCTAssertEqual(s.keyDown(keyCode: 2, flags: [.control, .option], at: t0),
                       .init(consume: true, actions: [.dictationPressed(t0)]))
        XCTAssertEqual(s.keyUp(keyCode: 2, at: t0),
                       .init(consume: true, actions: [.dictationReleased(t0)]))
    }

    func testCommandChordConsumesItsKeyBothWays() {
        let s = bound()
        XCTAssertEqual(s.flagsChanged([.control, .shift], at: t0), HotkeyTapState.Decision())
        XCTAssertEqual(s.keyDown(keyCode: kVK_X, flags: [.control, .shift], at: t0),
                       .init(consume: true, actions: [.commandPressed(t0)]))
        // Auto-repeat while held: consumed by nobody, fires nothing.
        XCTAssertEqual(s.keyDown(keyCode: kVK_X, flags: [.control, .shift], at: t0),
                       HotkeyTapState.Decision())
        XCTAssertEqual(s.keyUp(keyCode: kVK_X, at: t0),
                       .init(consume: true, actions: [.commandReleased(t0)]))
    }

    func testDroppingAModifierMidChordReleasesIt() {
        let s = bound()
        _ = s.keyDown(keyCode: kVK_X, flags: [.control, .shift], at: t0)
        XCTAssertEqual(s.flagsChanged([.control], at: t0),
                       .init(consume: true, actions: [.commandReleased(t0)]))
    }

    func testMeetingIsToggleOnlyButItsReleaseIsStillConsumed() {
        let s = bound()
        XCTAssertEqual(s.keyDown(keyCode: kVK_M, flags: [.control, .option], at: t0),
                       .init(consume: true, actions: [.meetingPressed]))
        XCTAssertEqual(s.keyUp(keyCode: kVK_M, at: t0), .init(consume: true, actions: []))
    }

    func testUnboundStateConsumesNothing() {
        let s = HotkeyTapState()
        XCTAssertEqual(s.flagsChanged([.function], at: t0), HotkeyTapState.Decision())
        XCTAssertEqual(s.keyDown(keyCode: kVK_X, flags: [.control, .shift], at: t0),
                       HotkeyTapState.Decision())
    }

    func testRebindingWhileTheTapDecidesIsSafe() {
        let s = bound()
        let done = expectation(description: "tap thread finished")
        DispatchQueue.global(qos: .userInteractive).async {
            for i in 0..<20_000 {
                _ = s.keyDown(keyCode: UInt16(i % 50), flags: [.control, .shift])
                _ = s.keyUp(keyCode: UInt16(i % 50))
                _ = s.flagsChanged(i.isMultiple(of: 2) ? [.control] : [])
            }
            done.fulfill()
        }
        for i in 0..<2_000 {
            s.setCommandCombo(combo(control: true, shift: true, key: UInt16(i % 50)))
            s.setDictationShortcut(i.isMultiple(of: 2) ? .fnOnly : .controlOption)
        }
        wait(for: [done], timeout: 30)
    }
}
