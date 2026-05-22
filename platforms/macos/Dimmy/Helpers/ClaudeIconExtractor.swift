import AppKit
import Foundation

/// Extracts the installed Claude Desktop app icon for use in Dimmy's
/// UI cards. Mac mirror of `Helpers/ClaudeIconExtractor.cs` on Windows.
///
/// On Mac the canonical icon lives at
/// `/Applications/Claude.app/Contents/Resources/AppIcon.icns`. We
/// convert it to PNG via `sips` (always present on macOS — part of
/// the base system) and cache the result under
/// `<configDir>/cache/claude-desktop-icon.png`. The cache is
/// invalidated when the `.icns` mtime advances (= Claude Desktop was
/// updated by Sparkle).
///
/// Returns nil on any failure; the caller falls back to its built-in
/// SF Symbol so the card still renders, just "off-brand".
enum ClaudeIconExtractor {
    /// Async extract — runs the sips shellout off the main thread.
    /// Cheap to call repeatedly: the cache check fast-paths in the
    /// hot path (no Process spawn) when the icns mtime hasn't moved.
    static func tryExtract() async -> String? {
        await Task.detached(priority: .userInitiated) {
            tryExtractSync()
        }.value
    }

    private static func tryExtractSync() -> String? {
        // Walk a small list of common install paths. Anthropic ships
        // Claude.app under /Applications; some users keep it under
        // ~/Applications. Anything else is custom and we'd need a
        // `mdfind` to resolve — skipped for now since the wizard
        // already gates on a successful install detection upstream.
        let candidates = [
            "/Applications/Claude.app",
            NSHomeDirectory() + "/Applications/Claude.app",
        ]
        guard let appBundle = candidates.first(where: { FileManager.default.fileExists(atPath: $0) }) else {
            return nil
        }
        let icns = appBundle + "/Contents/Resources/AppIcon.icns"
        guard FileManager.default.fileExists(atPath: icns) else { return nil }

        guard let cacheDir = DimmyCore.shared.configDirURL?.appendingPathComponent("cache", isDirectory: true) else {
            return nil
        }
        try? FileManager.default.createDirectory(at: cacheDir, withIntermediateDirectories: true)
        let cached = cacheDir.appendingPathComponent("claude-desktop-icon.png")

        // Cache invalidation: reuse the cached PNG if its mtime is
        // newer than the source icns (= Claude wasn't updated since
        // we last extracted).
        if let cachedAttrs = try? FileManager.default.attributesOfItem(atPath: cached.path),
           let cachedDate = cachedAttrs[.modificationDate] as? Date,
           let icnsAttrs = try? FileManager.default.attributesOfItem(atPath: icns),
           let icnsDate = icnsAttrs[.modificationDate] as? Date,
           cachedDate >= icnsDate {
            return cached.path
        }

        // Use `sips` rather than NSImage(contentsOf:).tiffRepresentation
        // because NSImage's icns reader picks the smallest variant by
        // default (the 16×16 icon, blurry at 40 px UI render). `sips
        // --resampleHeight 256` ensures we get a crisp 256-px square.
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/sips")
        proc.arguments = [
            "-s", "format", "png",
            "--resampleHeight", "256",
            icns,
            "--out", cached.path,
        ]
        proc.standardOutput = Pipe()
        proc.standardError = Pipe()
        do {
            try proc.run()
            proc.waitUntilExit()
            return proc.terminationStatus == 0 ? cached.path : nil
        } catch {
            return nil
        }
    }
}
