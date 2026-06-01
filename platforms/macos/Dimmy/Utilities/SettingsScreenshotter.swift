import AppKit
import SwiftUI

// MARK: - SettingsScreenshotter
//
// Renders every Settings tab to a PNG by driving the Settings window
// programmatically from inside the running app. Triggered by the env
// var `DIMMY_SCREENSHOT_ALL=1` at launch.
//
// Why in-process: an SSH shell can't drive `screencapture` against the
// user's WindowServer when the session is reached via RDP or Screen
// Sharing without a registered ConsoleUser. The Dimmy process itself,
// however, IS running inside that GUI session, so an in-process
// `NSView.cacheDisplay(in:to:)` draws the SwiftUI view tree into a
// bitmap without needing WindowServer round-trip access. PNGs land
// under `out/mac-settings-screenshots/` next to the project root.
//
// The shooter rotates Appearance (light + dark) so the same set of
// PNGs is produced for both themes, suffixed `-light` / `-dark`.
@MainActor
enum SettingsScreenshotter {
    private static let outDir: URL = {
        // Walk up from the executable to find the repo root, fall back
        // to ~/dimmy-screenshots if we can't (e.g. installed builds).
        let bundleURL = Bundle.main.bundleURL
        var dir = bundleURL
        while dir.path != "/" {
            let candidate = dir.appendingPathComponent("scripts/dev")
            if FileManager.default.fileExists(atPath: candidate.path) {
                return dir.appendingPathComponent("out/mac-settings-screenshots")
            }
            dir = dir.deletingLastPathComponent()
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("dimmy-screenshots")
    }()

    /// Public entry point. Returns immediately when the env var is unset.
    static func runIfRequested() {
        let v = ProcessInfo.processInfo.environment["DIMMY_SCREENSHOT_ALL"] ?? "<nil>"
        NSLog("[Screenshotter] runIfRequested entered, DIMMY_SCREENSHOT_ALL=\(v)")
        guard v == "1" else {
            return
        }
        NSLog("[Screenshotter] DIMMY_SCREENSHOT_ALL=1, scheduling capture run")
        // Give the app a beat to finish core init + first-paint, then
        // kick off the capture loop on the main actor.
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) {
            Task { @MainActor in
                await runCaptureLoop()
            }
        }
    }

    private static func runCaptureLoop() async {
        do {
            try FileManager.default.createDirectory(
                at: outDir, withIntermediateDirectories: true
            )
        } catch {
            NSLog("[Screenshotter] could not create output dir: \(error)")
            return
        }
        NSLog("[Screenshotter] output dir = \(outDir.path)")

        // Force-enable Advanced so every gated tab is reachable.
        AppState.shared.showAdvanced = true

        // NSApplicationDelegateAdaptor in SwiftUI wraps our AppDelegate,
        // so NSApp.delegate is a SwiftUI proxy, not AppDelegate directly.
        // AppDelegate.shared is published during applicationDidFinishLaunching
        // (line 64) before this hook runs, so it is always set by now.
        guard let appDelegate = AppDelegate.shared else {
            NSLog("[Screenshotter] no AppDelegate.shared, aborting")
            return
        }
        appDelegate.openSettings()
        try? await Task.sleep(nanoseconds: 600_000_000)

        guard let window = NSApp.windows.first(where: { $0.title == "Dimmy Settings" })
        else {
            NSLog("[Screenshotter] could not locate Settings window")
            return
        }
        window.setContentSize(NSSize(width: 1100, height: 820))
        window.makeKeyAndOrderFront(nil)

        let tabs = MacSettingsTab.allCases

        let modes: [(String, NSAppearance?)] = [
            ("light", NSAppearance(named: NSAppearance.Name.aqua)),
            ("dark",  NSAppearance(named: NSAppearance.Name.darkAqua)),
        ]
        for (mode, appearance) in modes {
            NSApp.appearance = appearance
            window.appearance = appearance
            try? await Task.sleep(nanoseconds: 400_000_000)

            for tab in tabs {
                NotificationCenter.default.post(
                    name: .dimmyNavigateSettingsTab,
                    object: nil,
                    userInfo: ["tab": tab.rawValue]
                )
                try? await Task.sleep(nanoseconds: 650_000_000)

                guard let contentView = window.contentView else { continue }
                let bounds = contentView.bounds
                guard bounds.width > 0, bounds.height > 0,
                      let rep = contentView.bitmapImageRepForCachingDisplay(in: bounds) else {
                    NSLog("[Screenshotter] skipped \(mode)/\(tab.rawValue), no bitmap rep")
                    continue
                }
                contentView.cacheDisplay(in: bounds, to: rep)
                guard let pngData = rep.representation(
                    using: NSBitmapImageRep.FileType.png,
                    properties: [:]
                ) else {
                    NSLog("[Screenshotter] skipped \(mode)/\(tab.rawValue), png encode failed")
                    continue
                }
                let safe = tab.rawValue.replacingOccurrences(of: " ", with: "_")
                let outURL = outDir.appendingPathComponent("\(safe)-\(mode).png")
                do {
                    try pngData.write(to: outURL, options: Data.WritingOptions.atomic)
                    NSLog("[Screenshotter] wrote \(outURL.path)")
                } catch {
                    NSLog("[Screenshotter] write failed for \(outURL.path), \(error)")
                }
            }
        }

        // Reset appearance to auto so the user's normal launch doesn't
        // stay forced into one mode.
        NSApp.appearance = nil
        NSLog("[Screenshotter] done, terminating in 0.5 s")
        try? await Task.sleep(nanoseconds: 500_000_000)
        NSApp.terminate(nil)
    }
}

extension Notification.Name {
    static let dimmyNavigateSettingsTab = Notification.Name("dimmyNavigateSettingsTab")
}
