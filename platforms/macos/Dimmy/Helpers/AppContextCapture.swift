import AppKit

// MARK: - AppContextCapture
//
// Mirror of `platforms/windows/Dimmy.Windows/Helpers/AppContextCapture.cs`.
// On macOS the "foreground app" is identified by its bundle id, surfaced
// by NSWorkspace.frontmostApplication. The captured value is fed to the
// Rust core via dimmy_set_app_context BEFORE recording starts so the
// LLM-enhance step can resolve user-defined app rules.
//
// Best-effort: a missing or sandboxed-no-bundle-id foreground returns an
// empty string — Rust treats that as "no rule matches" and the user's
// default style stays in effect. Same shape as the Win-side fallback.
//
// Note about timing: NSWorkspace.frontmostApplication is updated by AppKit
// when another process becomes active (kAEActiveAppChanged AppleEvent
// path). At hotkey-down on Dimmy we are NOT the frontmost app — the user
// pressed the hotkey while focused on Slack/Outlook/etc — so this returns
// the correct bundle id of THAT app, not Dimmy. Verified on Tahoe with a
// CGEventTap-driven hotkey path identical to ours.

enum AppContextCapture {
    /// The bundle ID of the foreground app at the moment of the call,
    /// or "" when none can be identified (kiosk shells, fast user
    /// switching mid-frame, etc).
    static func foregroundBundleId() -> String {
        guard let id = NSWorkspace.shared.frontmostApplication?.bundleIdentifier else {
            return ""
        }
        return id
    }

    /// Resolve a bundle id back to a human-friendly app name (the value
    /// of CFBundleDisplayName / CFBundleName from the app's Info.plist).
    /// Used by the History detail view to render "Slack" instead of
    /// "com.tinyspeck.slackmacgap". Empty string when the app is no
    /// longer running and not in the LaunchServices DB. Cached for the
    /// session so we don't hit the disk on every history row.
    static func appName(for bundleId: String) -> String {
        if bundleId.isEmpty { return "" }
        if let cached = nameCache.value(for: bundleId) { return cached }

        // Fast path: the app is currently running.
        if let running = NSRunningApplication.runningApplications(withBundleIdentifier: bundleId).first,
           let name = running.localizedName, !name.isEmpty {
            nameCache.set(name, for: bundleId)
            return name
        }

        // Slow path: walk LaunchServices for the on-disk app bundle.
        if let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleId) {
            let bundle = Bundle(url: url)
            let name = (bundle?.object(forInfoDictionaryKey: "CFBundleDisplayName") as? String)
                ?? (bundle?.object(forInfoDictionaryKey: "CFBundleName") as? String)
                ?? url.deletingPathExtension().lastPathComponent
            if !name.isEmpty {
                nameCache.set(name, for: bundleId)
                return name
            }
        }

        return ""
    }

    /// Cached app name lookups. Cheap because the same bundle ids are
    /// queried over and over (history detail, app rules list, meeting
    /// transcript). Not thread-safe — all callers run on the main actor.
    private static let nameCache = StringCache()

    private final class StringCache {
        private var entries: [String: String] = [:]
        func value(for key: String) -> String? { entries[key] }
        func set(_ value: String, for key: String) { entries[key] = value }
    }

    // MARK: - App icon resolution

    /// Real Mac app icon for a bundle id (transparent background, the
    /// original CFBundleIconFile from the .app's Resources/). Returns nil
    /// when the app isn't installed in LaunchServices' DB.
    /// Cached per-session: NSWorkspace.icon(forFile:) hits the disk + the
    /// IconServices DB, expensive enough to memoise across the rules list
    /// + history detail render passes.
    static func appIcon(for bundleId: String) -> NSImage? {
        if bundleId.isEmpty { return nil }
        if let cached = iconCache.value(for: bundleId) { return cached }

        // Fast path: running app's NSImage already in memory.
        if let running = NSRunningApplication.runningApplications(withBundleIdentifier: bundleId).first,
           let icon = running.icon {
            iconCache.set(icon, for: bundleId)
            return icon
        }

        // Slow path: ask LaunchServices for the on-disk .app, then read
        // its icon. NSWorkspace.icon(forFile:) understands .app bundles
        // and returns the proper hi-res template-aware NSImage.
        guard let url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleId) else {
            return nil
        }
        let icon = NSWorkspace.shared.icon(forFile: url.path)
        iconCache.set(icon, for: bundleId)
        return icon
    }

    private static let iconCache = ImageCache()

    private final class ImageCache {
        private var entries: [String: NSImage] = [:]
        func value(for key: String) -> NSImage? { entries[key] }
        func set(_ value: NSImage, for key: String) { entries[key] = value }
    }
}
