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
/// Auto-update is license-gated exactly like Windows: without the
/// `auto_update` scope Sparkle's scheduled checks stay OFF and the
/// About-page "Check for updates" button falls back to opening the
/// download page. Source / dev builds report every scope as present so
/// this is a no-op locally. The gate is (re-)evaluated lazily rather
/// than once at `start()` because the Rust core may still be
/// initialising when the app delegate calls us.
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

    /// Last status string for the About page. "Idle" read as "this pane
    /// is inert" — say what is actually true instead, since the first
    /// scheduled check lands 5 s after launch and overwrites it.
    @Published private(set) var statusText: String = "Not checked yet"

    /// True while a user-initiated check is in flight — the About page
    /// disables the button and shows a spinner so a press is visibly
    /// doing something even when the answer turns out to be "no update".
    @Published private(set) var isChecking: Bool = false

    /// False when the `auto_update` scope is absent. The About page
    /// hides the channel picker on false, mirroring Windows, where
    /// `UpdateChannelCard` collapses for users without the scope.
    @Published private(set) var isLicensed: Bool = true

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

    private var licenseObserver: NSObjectProtocol?

    private override init() {
        super.init()
        // Redeeming a license mid-session must flip updates on without
        // a relaunch. Mirror of Win's `LicenseService.LicenseChanged`
        // hook in `UpdateService`'s constructor.
        licenseObserver = NotificationCenter.default.addObserver(
            forName: .dimmyLicenseChanged, object: nil, queue: .main
        ) { [weak self] _ in
            Task { @MainActor in
                guard let self else { return }
                self.applyLicenseGate(kickCheck: true)
            }
        }
    }

    /// Read the `auto_update` scope and switch Sparkle's scheduled
    /// checks accordingly. Called after `start()`, on every license
    /// change, and before each explicit check.
    ///
    /// Returns the scope state so callers can branch on it directly.
    /// While the Rust core is still initialising the FFI would be
    /// unsafe to call, so we report "not licensed yet" and leave the
    /// scheduler off — the 5 s post-launch kick re-evaluates.
    @discardableResult
    private func applyLicenseGate(kickCheck: Bool = false) -> Bool {
        guard DimmyCore.shared.isInitialized else {
            controller?.updater.automaticallyChecksForUpdates = false
            return false
        }
        let licensed = DimmyCore.shared.licenseHasScope(.autoUpdate)
        let wasLicensed = isLicensed
        isLicensed = licensed
        controller?.updater.automaticallyChecksForUpdates = licensed
        if !licensed {
            isUpdateReady = false
            statusText = "In-app updates need an active plan"
            NSLog("[Update] auto_update scope absent — scheduled checks disabled")
        } else if kickCheck && !wasLicensed {
            NSLog("[Update] auto_update scope gained — re-checking now")
            checkInBackground()
        }
        return licensed
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
            await MainActor.run {
                // Evaluate the license gate here, not in start(): the
                // Rust core finishes initialising asynchronously and may
                // not be up yet when the app delegate calls us.
                guard self.applyLicenseGate() else { return }
                self.checkInBackground()
            }
        }
    }

    private func configureUpdater(_ updater: SPUUpdater?) {
        guard let updater = updater else { return }
        // Left OFF until applyLicenseGate() says otherwise — Sparkle
        // runs its own scheduler, so enabling this before we know the
        // license state would poll on behalf of an unlicensed user.
        updater.automaticallyChecksForUpdates = false
        updater.automaticallyDownloadsUpdates = true
        // 6 h re-check while running, mirror of Win's `ReCheckInterval`.
        updater.updateCheckInterval = 21600
    }

    /// Hero "Check for updates" button — forces a check now, ignoring
    /// the scheduled interval.
    ///
    /// Two things the previous version got wrong, both reported as
    /// "the button does nothing": Dimmy is an `LSUIElement` agent, so
    /// Sparkle's modal opened without the app ever coming forward and
    /// could sit behind other windows; and an unlicensed user got the
    /// same silent no-op as a licensed one who is already current.
    func checkForUpdatesNow() {
        // applyLicenseGate() reports "not licensed" while the core is
        // still coming up, which is the right default for the silent
        // scheduler but the wrong answer for a button press: it would
        // send a paying user to the download page. Say so instead.
        guard DimmyCore.shared.isInitialized else {
            statusText = "Still starting up. Try again in a moment."
            return
        }
        guard applyLicenseGate() else {
            statusText = "In-app updates need an active plan"
            if let url = URL(string: "https://dimmy.app/download") {
                NSWorkspace.shared.open(url)
            }
            return
        }
        isChecking = true
        statusText = "Checking for updates..."
        // Bring Dimmy forward so Sparkle's modal lands on top. Without
        // this an agent app shows the sheet behind whatever the user
        // was looking at.
        NSApp.activate(ignoringOtherApps: true)
        controller?.checkForUpdates(nil)
    }

    /// Background check used by the scheduled timer + first-launch
    /// delayed kick. Silent — the user doesn't see anything unless an
    /// update is found.
    private func checkInBackground() {
        controller?.updater.checkForUpdatesInBackground()
    }

    /// Human-readable channel name for the status line, so "you're on
    /// the latest" says WHICH latest — a prerelease user seeing the
    /// stable number would otherwise read it as a stuck check.
    var channelLabel: String {
        channel == "prerelease" ? "stable + prerelease" : "stable"
    }

    /// The version to SHOW the user, without a `v` prefix.
    ///
    /// `CFBundleShortVersionString` is the marketing number, taken from
    /// `core/Cargo.toml`, and it is IDENTICAL across `0.6.74-rc.1`,
    /// `0.6.74-rc.2` and the `0.6.74` stable that follows. On its own it
    /// cannot tell a tester which build they are running — reported from
    /// the field as "the About page doesn't show the installed version".
    /// `CFBundleVersion` carries the tag (`0.6.74-rc.2`) precisely because
    /// Sparkle needs a value that is unique per build; we simply never
    /// showed it.
    ///
    /// Prefer the build version, but only when it EXTENDS the marketing
    /// one — that is the contract `release.yml` establishes
    /// (`CURRENT_PROJECT_VERSION` = tag minus `v`, `MARKETING_VERSION` =
    /// Cargo.toml). A local Xcode build inherits the .pbxproj defaults
    /// (`1`, or a stale `0.6.65`), which do not extend it: fall back
    /// rather than present a stale number as fact.
    nonisolated static func displayVersion(short: String?, build: String?) -> String {
        let marketing = (short ?? "").trimmingCharacters(in: .whitespaces)
        let tagged = (build ?? "").trimmingCharacters(in: .whitespaces)
        guard !marketing.isEmpty else { return tagged.isEmpty ? "0.0.0" : tagged }
        guard !tagged.isEmpty, tagged.hasPrefix(marketing) else { return marketing }
        return tagged
    }

    /// Running app version as shown in the About hero, e.g. `0.6.74-rc.2`.
    nonisolated static var runningVersionPlain: String {
        displayVersion(
            short: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString")
                as? String,
            build: Bundle.main.object(forInfoDictionaryKey: "CFBundleVersion") as? String)
    }

    /// Same, `v`-prefixed, for the update status line.
    nonisolated static var runningVersion: String { "v\(runningVersionPlain)" }
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
            self.isChecking = false
            self.isUpdateReady = true
            self.statusText = "Update \(version) available — apply at quit"
            NSLog("[Update] found valid update: \(raw)")
        }
    }

    nonisolated func updaterDidNotFindUpdate(_ updater: SPUUpdater) {
        Task { @MainActor in
            self.isChecking = false
            self.isUpdateReady = false
            self.statusText = "You're on the latest \(self.channelLabel) version (\(Self.runningVersion))"
            NSLog("[Update] no update available")
        }
    }

    nonisolated func updater(_ updater: SPUUpdater, didAbortWithError error: any Error) {
        let nsErr = error as NSError
        let msg = error.localizedDescription
        // Sparkle calls didAbortWithError for several benign outcomes
        // that have ALREADY been surfaced to the user via the modal
        // (or via the explicit "no update" delegate). Treating these
        // as "Check failed" produces a confusing UX: the modal says
        // "Up to date" and the line right under it says "Check
        // failed". The SUErrorDomain codes worth filtering:
        //
        //   SUNoUpdateError              = 1001  (no update available)
        //   SUInstallationCanceledError  = 4005  (user dismissed install prompt)
        //   SUInstallationCancelledError = 4005  (alt spelling in some versions)
        //   SUInstallationAuthorizeLaterError = 4014 (user picked "Later")
        //
        // For these we leave the status text alone — either the
        // happy-path delegate ran first (`updaterDidNotFindUpdate` →
        // "Up to date") or the user actively dismissed the prompt.
        let benignCodes: Set<Int> = [1001, 4005, 4014]
        if nsErr.domain == "SUSparkleErrorDomain" && benignCodes.contains(nsErr.code) {
            NSLog("[Update] benign abort (\(nsErr.code)): \(msg)")
            // Still clear the spinner: the happy-path delegate has
            // already written the status line, but nothing else would
            // take the button out of its "Checking..." state.
            Task { @MainActor in self.isChecking = false }
            return
        }
        Task { @MainActor in
            self.isChecking = false
            self.statusText = "Couldn't reach the update server. Try again in a moment."
            NSLog("[Update] aborted (\(nsErr.domain) \(nsErr.code)): \(msg)")
        }
    }
}
