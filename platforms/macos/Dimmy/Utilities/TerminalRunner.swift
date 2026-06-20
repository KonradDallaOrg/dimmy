import AppKit
import Foundation

/// Run a shell command in a NEW Terminal window without requiring the
/// macOS Automation (AppleScript-controls-Terminal) permission.
///
/// Why not `tell application "Terminal" / do script`:
/// `NSAppleScript` controlling Terminal is gated by TCC Automation. On
/// first attempt macOS shows the "Dimmy.app wants to control Terminal"
/// prompt; if it never surfaces (stale TCC db, prior denial) the script
/// fails silently — `do script` returns without executing and the user
/// sees Terminal come forward to its existing window with no command
/// run. We hit this in both wizards (Codex + Claude CLI).
///
/// Why `.command` files:
/// Terminal.app registers as the LaunchServices handler for the
/// `com.apple.terminal.shell-script` UTI (`.command`). Opening one via
/// `NSWorkspace` always spawns a NEW window and auto-runs the file's
/// contents. No Automation permission, no AppleScript. The only
/// implicit grant needed is "Dimmy can open files it just wrote", which
/// is part of the default sandbox.
enum TerminalRunner {
    /// Write `command` to a sticky helper file under
    /// `~/Library/Application Support/dimmy/installers/install-<slug>.command`,
    /// chmod +x it, and hand it to Terminal via LaunchServices.
    ///
    /// `slug` should be a short identifier (e.g. `"codex-brew"`,
    /// `"claude-cli-curl"`) so repeated installs overwrite the same
    /// file instead of accumulating one per click.
    static func run(_ command: String, slug: String) {
        assert(!command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
               "TerminalRunner.run requires a non-empty command")
        assert(!slug.isEmpty, "TerminalRunner.run requires a slug")

        let dir = installersDir()
        do {
            try FileManager.default.createDirectory(
                at: dir, withIntermediateDirectories: true, attributes: nil)
        } catch {
            NSLog("[TerminalRunner] mkdir failed: \(error)")
            return
        }

        let safeSlug = slug
            .components(separatedBy: CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-_"))
                .inverted)
            .joined(separator: "-")
        let file = dir.appendingPathComponent("install-\(safeSlug).command")

        let script = """
        #!/bin/zsh
        set -e
        \(command)
        echo
        echo "[Dimmy] Install command finished. You can close this window."
        """

        do {
            try script.write(to: file, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes(
                [.posixPermissions: 0o755], ofItemAtPath: file.path)
        } catch {
            NSLog("[TerminalRunner] write failed: \(error)")
            return
        }

        let ok = NSWorkspace.shared.open(file)
        if !ok {
            NSLog("[TerminalRunner] NSWorkspace.open returned false for \(file.path)")
        }
    }

    private static func installersDir() -> URL {
        let appSupport = FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask
        ).first ?? URL(fileURLWithPath: NSHomeDirectory())
            .appendingPathComponent("Library/Application Support")
        return appSupport
            .appendingPathComponent("dimmy")
            .appendingPathComponent("installers")
    }
}
