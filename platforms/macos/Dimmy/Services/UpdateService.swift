import AppKit
import Foundation
import Sparkle

/// Auto-update orchestrator for macOS builds. Mirror of Win's
/// `UpdateService.cs` (Velopack-based) but built on top of Sparkle 2,
/// the Mac-native equivalent. Same UX contract:
///
///   1. Background check ~5 s after launch (don't block startup).
///   2. If an update exists → silent download in background.
///   3. Surface readiness (`isUpdateReady`) so Settings → About and
///      the menubar can react.
///   4. At quit time, Sparkle shows its own install-and-relaunch
///      prompt (the user can defer). No admin prompt — the per-user
///      install path under `/Applications` or `~/Applications` is
///      already user-writable.
///
/// Channel selection lives in `UserDefaults` key `dimmy.update_channel`:
///   - `stable`     → only releases without a Sparkle `<sparkle:channel>`
///                    tag (or with `channel == ""`).
///   - `prerelease` → also offers releases tagged `prerelease`.
///
/// Failure modes are silent by design — no network, GitHub down, or a
/// malformed appcast must NOT block the user. All errors land in the
/// console under `[Update]`.
@MainActor
final class UpdateService: NSObject, ObservableObject {

    static let shared = UpdateService()

    private var controller: SPUStandardUpdaterController?

    /// True once Sparkle has found a valid update for the running
    /// version. Drives the About-page banner + menubar dot.
    @Published private(set) var isUpdateReady: Bool = false

    /// Last status string for the About page.
    @Published private(set) var statusText: String = "Idle"

    /// Channel — `"stable"` or `"prerelease"`. Persisted in UserDefaults
    /// so the next launch picks up the same preference.
    @Published var channel: String = UserDefaults.standard.string(forKey: "dimmy.update_channel") ?? "stable" {
        didSet {
            UserDefaults.standard.set(channel, forKey: "dimmy.update_channel")
            // Re-evaluate the appcast with the new channel filter —
            // without this, the next scheduled check would still match
            // the previous filter.
            controller?.updater.resetUpdateCycle()
        }
    }

    private override init() {
        super.init()
    }

    /// Stand up Sparkle. Called from `AppDelegate.applicationDidFinishLaunching`.
    /// Idempotent — second call is a no-op so re-init from a test
    /// harness or hot-reload doesn't double-tap the network.
    func start() {
        if controller != nil { return }

        NSLog("[Update] start() — bringing up Sparkle SPUStandardUpdaterController (channel=\(channel))")
        controller = SPUStandardUpdaterController(
            startingUpdater: true,
            updaterDelegate: self,
            userDriverDelegate: nil
        )
        configureUpdater(controller?.updater)
        NSLog("[Update] start() — controller created, updater configured. First background check in 5 s.")

        // Delay the very first check ~5 s so we don't compete with
        // pill animation / hotkey setup / Metal shader compilation on
        // first launch. Mirrors Win's `FirstCheckDelay`.
        Task {
            try? await Task.sleep(nanoseconds: 5_000_000_000)
            await MainActor.run { self.checkInBackground() }
        }
    }

    private func configureUpdater(_ updater: SPUUpdater?) {
        guard let updater = updater else { return }
        updater.automaticallyChecksForUpdates = true
        updater.automaticallyDownloadsUpdates = true
        // 6 h re-check while running, mirror of Win's `ReCheckInterval`.
        updater.updateCheckInterval = 21600
    }

    /// Hero "Check for updates" button — forces a check now, ignoring
    /// the scheduled interval.
    func checkForUpdatesNow() {
        statusText = "Checking…"
        controller?.checkForUpdates(nil)
    }

    /// Background check used by the scheduled timer + first-launch
    /// delayed kick. Silent — the user doesn't see anything unless an
    /// update is found.
    private func checkInBackground() {
        controller?.updater.checkForUpdatesInBackground()
    }
}

// MARK: - SPUUpdaterDelegate

extension UpdateService: SPUUpdaterDelegate {

    /// Filter appcast items by the user's channel preference.
    nonisolated func allowedChannels(for updater: SPUUpdater) -> Set<String> {
        let stored = UserDefaults.standard.string(forKey: "dimmy.update_channel") ?? "stable"
        switch stored {
        case "prerelease":
            return ["prerelease"]
        default:
            return []
        }
    }

    nonisolated func updater(_ updater: SPUUpdater, didFindValidUpdate item: SUAppcastItem) {
        let version = item.displayVersionString
        let raw = item.versionString
        Task { @MainActor in
            self.isUpdateReady = true
            self.statusText = "Update \(version) available — apply at quit"
            NSLog("[Update] found valid update: \(raw)")
        }
    }

    nonisolated func updaterDidNotFindUpdate(_ updater: SPUUpdater) {
        Task { @MainActor in
            self.isUpdateReady = false
            self.statusText = "Up to date"
            NSLog("[Update] no update available")
        }
    }

    nonisolated func updater(_ updater: SPUUpdater, didAbortWithError error: any Error) {
        let msg = error.localizedDescription
        Task { @MainActor in
            self.statusText = "Check failed — will retry later"
            NSLog("[Update] aborted: \(msg)")
        }
    }
}
