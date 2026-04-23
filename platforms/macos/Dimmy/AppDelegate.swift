import AppKit
import AVFoundation
import ApplicationServices
import Combine
import SwiftUI

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, NSWindowDelegate {
    static var shared: AppDelegate?

    private var statusBarController: StatusBarController?
    private var pillWindowController: PillWindowController?
    private var onboardingWindow: NSWindow?
    private var settingsWindow: NSWindow?
    private let appState = AppState.shared
    private var cancellables = Set<AnyCancellable>()
    private var coreInitialized = false

    func applicationDidFinishLaunching(_ notification: Notification) {
        hkLog("[AppDelegate] applicationDidFinishLaunching ENTER")
        AppDelegate.shared = self

        SelfTests.runAll()

        // UI-only setup. No audio, no keychain, no permission-triggering calls yet.
        statusBarController = StatusBarController(appState: appState)
        pillWindowController = PillWindowController(appState: appState)

        // Watch for onboarding completion — then initialize the core once permissions are granted.
        appState.$isOnboardingComplete
            .dropFirst()
            .filter { $0 == true }
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in
                guard let self = self else { return }
                self.onboardingWindow?.close()
                self.onboardingWindow = nil
                self.pillWindowController?.show()
                self.applyActivationPolicy()
            }
            .store(in: &cancellables)

        // React to the user toggling "Show app in Dock" in Settings.
        appState.$showInDock
            .dropFirst()
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in self?.applyActivationPolicy() }
            .store(in: &cancellables)

        // Initialize the Rust core only once microphone is granted — initializing earlier probes
        // audio devices and triggers the macOS mic prompt outside of onboarding. Tying it to the
        // permission flip means the prompt only happens when the user clicks Grant in the
        // Permissions step, and returning users (mic already authorized) init immediately.
        PermissionsManager.shared.$microphone
            .filter { $0 == .authorized }
            .first()
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in self?.initializeCoreAsync() }
            .store(in: &cancellables)

        hkLog("[AppDelegate] isOnboardingComplete=\(appState.isOnboardingComplete) perms=\(permissionsGranted())")
        if appState.isOnboardingComplete {
            pillWindowController?.show()
            if !permissionsGranted() {
                hkLog("[AppDelegate] onboarding complete but permissions missing — reopening Permissions step")
                showOnboarding(startStep: 1)
            }
        } else {
            hkLog("[AppDelegate] showing onboarding")
            showOnboarding()
        }
        applyActivationPolicy()
    }

    /// Reset TCC entries for any permission that the running process doesn't see as granted.
    /// Skips perms that are currently granted (stable signature — don't disrupt). Non-granted
    /// perms get wiped so any stale entries from old builds can't confuse macOS into showing
    /// Dimmy as enabled while the new signature has no matching record.
    private func resetStalePermissions() {
        let perms = PermissionsManager.shared
        perms.refresh()
        var toReset: [String] = []
        if !perms.microphoneGranted { toReset.append("Microphone") }
        if !perms.accessibilityGranted { toReset.append("Accessibility") }
        if !perms.inputMonitoringGranted { toReset.append("ListenEvent") }
        guard !toReset.isEmpty else { return }
        let servicesList = toReset.joined(separator: ", ")
        hkLog("[AppDelegate] resetting stale TCC entries: \(servicesList)")
        perms.resetTccEntries(services: toReset)
    }

    /// Single source of truth for whether Dimmy appears in the Dock / Cmd+Tab.
    /// Onboarding always forces `.regular` so users who click away to System Settings
    /// can click the Dock icon to return. Otherwise it follows the user preference.
    private func applyActivationPolicy() {
        let onboardingVisible = onboardingWindow?.isVisible == true
        let shouldBeRegular = onboardingVisible || appState.showInDock
        NSApp.setActivationPolicy(shouldBeRegular ? .regular : .accessory)
    }

    private func permissionsGranted() -> Bool {
        // Refresh first so we don't trust stale @Published state.
        PermissionsManager.shared.refresh()
        return PermissionsManager.shared.microphoneGranted
            && PermissionsManager.shared.accessibilityGranted
    }

    /// Runs Rust core initialization on a background queue so the main thread stays responsive
    /// while audio devices are probed / keychain is unlocked. Once init completes, we load config
    /// and start the global hotkey manager on the main thread.
    private func initializeCoreAsync() {
        guard !coreInitialized else { return }
        coreInitialized = true
        hkLog("[AppDelegate] initializeCoreAsync — dispatching to background")
        DispatchQueue.global(qos: .userInitiated).async {
            DimmyCore.shared.initialize()
            let cfg = DimmyCore.shared.getConfig()
            DispatchQueue.main.async { [weak self] in
                guard let self = self else { return }
                if let cfg = cfg {
                    self.appState.loadFromRustConfig(cfg)
                }
                hkLog("[AppDelegate] core ready — starting HotkeyManager")
                HotkeyManager.shared.start(appState: self.appState)
            }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        HotkeyManager.shared.stop()
        DimmyCore.shared.shutdown()
    }

    /// Bring the onboarding window back to the front whenever Dimmy is re-activated
    /// (Cmd+Tab, Cmd+Shift+Tab, Dock icon click). Without this, switching to Dimmy
    /// activates the process but leaves the window hidden behind other apps.
    func applicationDidBecomeActive(_ notification: Notification) {
        onboardingWindow?.makeKeyAndOrderFront(nil)
    }

    /// Fires when the user clicks the Dock icon while the app is running but has no
    /// visible windows (e.g., closed the onboarding red X). Re-open onboarding if it's
    /// still pending so users don't get stuck with a silent Dock icon.
    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows: Bool) -> Bool {
        if !hasVisibleWindows && !appState.isOnboardingComplete {
            showOnboarding()
        }
        return true
    }

    func openSettings() {
        // Refresh config from Rust before showing settings
        if let config = DimmyCore.shared.getConfig() {
            appState.loadFromRustConfig(config)
        }

        if let settingsWindow, settingsWindow.isVisible {
            settingsWindow.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }

        let settingsView = SettingsContainerView(appState: appState)

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 620, height: 500),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.center()
        window.title = "Dimmy Settings"
        window.contentView = NSHostingView(rootView: settingsView)
        window.isReleasedWhenClosed = false
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)

        self.settingsWindow = window
    }

    func reopenOnboarding() {
        showOnboarding(startStep: 0)
    }

    private func showOnboarding(startStep: Int = 0) {
        if let existing = onboardingWindow, existing.isVisible {
            existing.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }

        // Clear stale TCC entries for any permission the current binary doesn't see as granted.
        // Rebuilds produce a new ad-hoc signature, so prior grants bound to the old signature
        // remain listed in System Settings but are invisible to the running process. Resetting
        // up-front means the user always grants fresh matching entries in one clean pass.
        // No-op for pristine installs (TCC has nothing to reset).
        resetStalePermissions()

        let onboardingView = OnboardingContainerView(appState: appState, startStep: startStep)

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 440),
            styleMask: [.titled, .closable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.center()
        window.title = "Welcome to Dimmy"
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.delegate = self
        window.contentView = NSHostingView(rootView: onboardingView)
        window.isReleasedWhenClosed = false

        self.onboardingWindow = window
        applyActivationPolicy()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    // MARK: - NSWindowDelegate

    func windowWillClose(_ notification: Notification) {
        guard let window = notification.object as? NSWindow, window === onboardingWindow else { return }
        onboardingWindow = nil
        applyActivationPolicy()
    }
}
