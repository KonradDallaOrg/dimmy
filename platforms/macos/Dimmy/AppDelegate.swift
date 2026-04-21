import AppKit
import AVFoundation
import ApplicationServices
import Combine
import SwiftUI

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
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

        #if DEBUG
        appState.isOnboardingComplete = false
        #endif

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
                self.initializeCoreAsync()
            }
            .store(in: &cancellables)

        hkLog("[AppDelegate] isOnboardingComplete=\(appState.isOnboardingComplete) perms=\(permissionsGranted())")
        if appState.isOnboardingComplete {
            pillWindowController?.show()
            if permissionsGranted() {
                initializeCoreAsync()
            } else {
                hkLog("[AppDelegate] onboarding complete but permissions missing — reopening Permissions step")
                showOnboarding(startStep: 1)
            }
        } else {
            hkLog("[AppDelegate] showing onboarding")
            showOnboarding()
        }
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
        window.contentView = NSHostingView(rootView: onboardingView)
        window.isReleasedWhenClosed = false
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)

        self.onboardingWindow = window
    }
}
