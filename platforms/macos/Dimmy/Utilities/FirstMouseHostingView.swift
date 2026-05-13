import AppKit
import SwiftUI

/// NSHostingView subclass that opts every embedded control into
/// "first-mouse" delivery.
///
/// Why: on macOS the first click on a window that is not yet key is, by
/// default, consumed by the window-activation machinery — controls
/// inside the window never see it. A user clicking the "Advanced" toggle
/// in a backgrounded Settings window observes a no-op on the first
/// click and a normal toggle on the second one, which reads as a broken
/// switch.
///
/// Overriding `acceptsFirstMouse(for:)` to return `true` makes the
/// underlying NSView (and every subview, including SwiftUI controls
/// hosted inside it) consume the first click as a real event. This is
/// the documented Cocoa pattern for "make this control behave like the
/// menu bar — a single click always works."
final class FirstMouseHostingView<Content: View>: NSHostingView<Content> {
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool {
        true
    }
}
