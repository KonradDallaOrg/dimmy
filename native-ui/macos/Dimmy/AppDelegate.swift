import AppKit
import SwiftUI
import Combine

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    static var shared: AppDelegate?

    private var statusBarController: StatusBarController?
    private var pillWindowController: PillWindowController?
    private var onboardingWindow: NSWindow?
    private var settingsWindow: NSWindow?
    private let appState = AppState.shared
    private var cancellables = Set<AnyCancellable>()

    func applicationDidFinishLaunching(_ notification: Notification) {
        AppDelegate.shared = self

        // Run self-tests in debug — crashes on failure (NSP)
        SelfTests.runAll()

        // Initialize Rust core (loads config, starts audio thread, migrates keys)
        DimmyCore.shared.initialize()

        // Load Rust config into AppState
        if let config = DimmyCore.shared.getConfig() {
            appState.loadFromRustConfig(config)
        }

        #if DEBUG
        appState.isOnboardingComplete = false
        #endif

        statusBarController = StatusBarController(appState: appState)
        pillWindowController = PillWindowController(appState: appState)
        HotkeyManager.shared.start(appState: appState)

        // Watch for onboarding completion via Combine (reliable, not SwiftUI onChange)
        appState.$isOnboardingComplete
            .dropFirst() // skip initial value
            .filter { $0 == true }
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in
                self?.onboardingWindow?.close()
                self?.onboardingWindow = nil
                self?.pillWindowController?.show()
            }
            .store(in: &cancellables)

        if !appState.isOnboardingComplete {
            showOnboarding()
        } else {
            pillWindowController?.show()
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

    private func showOnboarding() {
        let onboardingView = OnboardingContainerView(appState: appState)

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
