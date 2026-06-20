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
    private var captionWindowController: CaptionWindowController?
    private var onboardingWindow: NSWindow?
    private var settingsWindow: NSWindow?
    private let appState = AppState.shared
    private var cancellables = Set<AnyCancellable>()
    private var coreInitialized = false

    func applicationDidFinishLaunching(_ notification: Notification) {
        hkLog("[AppDelegate] applicationDidFinishLaunching ENTER")

        // Crash capture: route any uncaught NSException + fatal POSIX
        // signal (SIGSEGV / SIGABRT / SIGBUS) to the Rust core's
        // dimmy.log so the user can hand-deliver a useful "what
        // happened?" line instead of just "the app vanished". Sentry
        // catches Rust panics but NOT Swift / AppKit faults — which
        // is exactly the class of crash we've been hitting on pill
        // scroll. This stays on even in release builds; the secret
        // sauce is just routing the trace through our existing log.
        Self.installCrashHandlers()

        // Single-instance guard. macOS deduplicates by bundle path, not
        // bundle ID — so a Release in /Applications and a Debug build in
        // ~/Library/Developer/Xcode/DerivedData with the same
        // CFBundleIdentifier can both run at once. Two Dimmys means two
        // global hotkey monitors fighting over Cmd+Shift+Space, two pill
        // windows, two licensing FFI sessions writing the same
        // license.json. Detect any other process advertising our bundle
        // ID and bail out — let the existing instance keep running.
        //
        // Skipped under XCTest. The test runner launches Dimmy.app as a
        // host process; if a developer build is already running, the
        // mutex would terminate the test host before XCTest can attach
        // and the run reports "Early unexpected exit, operation never
        // finished bootstrapping". Tests own their own lifecycle, so
        // bypass this check when `XCTestConfigurationFilePath` is set
        // (the env var XCTest sets in the host's environment).
        let isUnderTest = ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
        if !isUnderTest {
            let myBundleID = Bundle.main.bundleIdentifier ?? ""
            let myPID = ProcessInfo.processInfo.processIdentifier
            let others = NSRunningApplication
                .runningApplications(withBundleIdentifier: myBundleID)
                .filter { $0.processIdentifier != myPID }
            if let existing = others.first {
                hkLog("[AppDelegate] another Dimmy already running (pid=\(existing.processIdentifier)) — activating it and quitting")
                existing.activate(options: [.activateIgnoringOtherApps])
                NSApp.terminate(nil)
                return
            }
        }

        AppDelegate.shared = self

        SelfTests.runAll()

        // Diagnostic only (DIMMY_TAP_PROBE=1): exercise the Core Audio
        // system-audio tap once at launch and log sample/peak counts. Off
        // by default — never touches a normal launch.
        if #available(macOS 14.4, *),
           ProcessInfo.processInfo.environment["DIMMY_TAP_PROBE"] == "1" {
            SystemAudioProcessTap.runDiagnosticProbe()
        }
        if #available(macOS 14.4, *),
           ProcessInfo.processInfo.environment["DIMMY_TAP_PROBE_MULTI"] == "1" {
            DispatchQueue.global(qos: .userInteractive).async {
                SystemAudioProcessTap.runMultiConfigProbe()
            }
        }
        if #available(macOS 14.4, *),
           let pidStr = ProcessInfo.processInfo.environment["DIMMY_TAP_PROBE_PID"],
           let pid = pid_t(pidStr) {
            DispatchQueue.global(qos: .userInteractive).async {
                SystemAudioProcessTap.runPerProcessProbe(pid: pid)
            }
        }
        if #available(macOS 14.4, *),
           ProcessInfo.processInfo.environment["DIMMY_TAP_PROBE_ENUM"] == "1" {
            DispatchQueue.global(qos: .userInteractive).async {
                SystemAudioProcessTap.runEnumerationProbe()
            }
        }

        // UI-only setup. No audio, no keychain, no permission-triggering calls yet.
        statusBarController = StatusBarController(appState: appState)
        pillWindowController = PillWindowController(appState: appState)
        captionWindowController = CaptionWindowController()
        wireLiveCaptions()

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

        // Initialize the Rust core unconditionally at launch. FFI init
        // sets up telemetry, sentry, AEC pipeline, history DB — none of
        // which open audio devices, so the mic permission prompt is NOT
        // triggered here (verified by the launch-time stderr log).
        // Initializing eagerly lets the Parakeet onboarding preload start
        // at the Welcome step instead of waiting for mic auth, so by the
        // time the user reaches the model-download step the 466 MB bundle
        // is already partly (or fully) on disk. Idempotent.
        initializeCoreAsync()

        hkLog("[AppDelegate] isOnboardingComplete=\(appState.isOnboardingComplete) perms=\(permissionsGranted())")
        if appState.isOnboardingComplete {
            pillWindowController?.show()
            // Reopen onboarding at the Permissions step when access is
            // missing — useful for Release users who revoked a permission
            // and need a reminder. Skipped in Debug because Xcode rebuilds
            // produce a fresh ad-hoc signature, invalidating prior TCC
            // grants and causing an infinite reopen loop on every launch.
            // The yellow badge on the menubar icon plus Settings →
            // Permissions still surface the missing perm in Debug.
            #if !DEBUG
            if !permissionsGranted() {
                hkLog("[AppDelegate] onboarding complete but permissions missing — reopening Permissions step")
                showOnboarding(startStep: 1)
            }
            #else
            if !permissionsGranted() {
                hkLog("[AppDelegate] perms missing in Debug — skipping auto-reopen (use Settings → Permissions)")
            }
            #endif
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

    // MARK: - Crash capture
    //
    // The handlers below MUST be top-level `@convention(c)` functions
    // (or method-less closures) because NSSetUncaughtExceptionHandler
    // and signal(2) take a C function pointer. Capturing `self` would
    // make Swift refuse to bridge the closure to a C pointer.
    // `crashAppendLine` is the freestanding helper; `installCrashHandlers`
    // wires up the static blocks.

    private static func installCrashHandlers() {
        NSSetUncaughtExceptionHandler(crashHandleNSException)
        for sig in [SIGSEGV, SIGABRT, SIGBUS, SIGILL, SIGFPE] {
            signal(sig, crashHandleSignal)
        }
        crashAppendLine("crash handlers installed (NSException + SIG{SEGV,ABRT,BUS,ILL,FPE})")
    }

    // MARK: - Live captions

    /// Subscribe AppState publishers to drive the floating subtitle
    /// window. Mirrors the Win OnSttChunkReceived flow:
    ///   * recording starts → if (live captions on AND chunked AND backend == parakeet)
    ///                        show the (empty) caption window
    ///   * stt_chunk arrives → push delta. is_final=true schedules a delayed hide
    ///   * recording cancels / is otherwise reset → hide immediately
    private func wireLiveCaptions() {
        // Per-chunk push, driven by liveCaptionTick so observers fire even
        // if two consecutive chunks happen to share the same delta string.
        appState.$liveCaptionTick
            .dropFirst()
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in
                guard let self else { return }
                guard self.appState.liveCaptionsEnabled else { return }
                self.captionWindowController?.push(delta: self.appState.liveCaptionDelta)
                if self.appState.liveCaptionIsFinal {
                    self.captionWindowController?.scheduleHide()
                } else {
                    self.captionWindowController?.show()
                }
            }
            .store(in: &cancellables)

        // Drive show/hide on recording lifecycle. Show only when the
        // chunked + parakeet preconditions are met; otherwise the
        // window stays hidden and the user gets only the final paste.
        appState.$recordingState
            .receive(on: DispatchQueue.main)
            .sink { [weak self] state in
                guard let self else { return }
                switch state {
                case .recording:
                    if self.shouldShowLiveCaptions() {
                        self.captionWindowController?.reset()
                        self.captionWindowController?.show()
                    }
                case .idle:
                    // Stop / cancel paths converge here. If the user
                    // cancelled (no final emitted) the schedule-hide
                    // never fires — hide eagerly.
                    if !self.appState.liveCaptionIsFinal {
                        self.captionWindowController?.hide()
                    }
                case .transcribing, .processing, .completing:
                    // Brief tail states between stop and final paste —
                    // leave the caption alone, the schedule-hide from
                    // the final stt_chunk will close it.
                    break
                }
            }
            .store(in: &cancellables)

        // Toggling the preference off mid-session should hide the
        // window immediately without waiting for a recording to end.
        appState.$liveCaptionsEnabled
            .dropFirst()
            .receive(on: DispatchQueue.main)
            .sink { [weak self] enabled in
                if !enabled { self?.captionWindowController?.hide() }
            }
            .store(in: &cancellables)

        // Re-fire the Parakeet warmup when the user flips the backend
        // from "whisper" → "parakeet" in Settings. Skips automatically
        // if already warm.
        appState.$localSttBackend
            .dropFirst()
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in self?.maybeWarmupParakeet() }
            .store(in: &cancellables)
    }

    /// Reactive bridge between `AppState.callDetectEnabled` and the
    /// 1 Hz CoreAudio poll in `CallDetectionManager`. Flipping the
    /// toggle in Settings cleanly winds down the cooldown timers
    /// (`signal(0)`) without restarting the timer.
    private func wireCallDetectToggle() {
        appState.$callDetectEnabled
            .dropFirst()
            .receive(on: DispatchQueue.main)
            .sink { enabled in
                CallDetectionManager.shared.setEnabled(enabled)
            }
            .store(in: &cancellables)
    }

    private func shouldShowLiveCaptions() -> Bool {
        appState.liveCaptionsEnabled
            && appState.chunkStreamingEnabled
            && appState.localSttBackend == "parakeet"
            && appState.sttMode == "local"
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

                // Honour DIMMY_SCREENSHOT_ALL=1, drives the in-process
                // Settings shooter once core init + main loop are up.
                // No-op when the env var is unset.
                SettingsScreenshotter.runIfRequested()

                // Secondary global hotkey for "add selected text to
                // dictionary" (Wispr Flow-style). Independent CGEventTap
                // for keyDown events; uses the same Accessibility
                // permission HotkeyManager already required. Default
                // combo Cmd+Shift+D, rebindable in Settings → Shortcut.
                DictHotkeyManager.shared.start(appState: self.appState)

                // Stand up the Sparkle-based auto-updater. Idempotent;
                // delays its first network check 5 s internally so we
                // don't compete with hotkey setup or Metal shader
                // compile on first launch. Mirror of Win's UpdateService
                // boot in `App.OnLaunched`.
                UpdateService.shared.start()

                // Register macOS Services provider so the OS-wide
                // "Add to Dimmy Dictionary" Services menu entry routes
                // back to AppDelegate.addToDimmyDictionary. Bypasses
                // the synthetic Cmd+C dance — the Services system
                // hands us the selection directly via NSPasteboard.
                NSApp.servicesProvider = self
                NSUpdateDynamicServices()

                // Async warmup — Parakeet only, and only if the bundle is
                // already on disk. Avoids the ~6 s cold path on the user's
                // first recording at the cost of one upfront background
                // inference (~6-10 s wall, no UI block). Triggered only
                // when Parakeet is the active backend; flipping to it
                // later in Settings re-fires below.
                self.maybeWarmupParakeet()

                // Boot the 1 Hz CoreAudio call-detect poll. Honours
                // AppState.callDetectEnabled (loaded from config above)
                // — the manager keeps ticking but emits 0/0 when the
                // toggle is off, so Rust cooldowns wind down cleanly.
                CallDetectionManager.shared.start(appState: self.appState)
                CallDetectionManager.shared.setEnabled(self.appState.callDetectEnabled)
                self.wireCallDetectToggle()

                // Preload Parakeet during onboarding so by the time the
                // user reaches the model-download step the bundle is
                // already on disk (or downloading). Mirrors the Win
                // "smart auto-pick" — eligibility: Apple Silicon
                // (always true for our arm64 build), >= 2 GB free disk,
                // bundle not yet present, onboarding still in flight.
                self.maybeStartParakeetOnboardingPreload()
            }
        }
    }

    /// Pre-download the Parakeet bundle in the background while the user
    /// is still clicking through Welcome / Permissions / Shortcut. By the
    /// time they hit the model-download step the bundle is already on
    /// disk — they tap Continue without waiting on a 466 MB download.
    /// Mirrors the Win onboarding behaviour. Skipped when the user has
    /// already finished onboarding (they've made their choice) or when
    /// the bundle is already present.
    private var didStartParakeetPreload = false
    private func maybeStartParakeetOnboardingPreload() {
        guard !didStartParakeetPreload else { return }
        guard !appState.isOnboardingComplete else { return }  // user past the step
        guard DimmyCore.shared.isInitialized else { return }
        guard !DimmyCore.shared.parakeetBundlePresent() else { return }

        // Disk gate — same threshold the model-download view uses.
        guard let support = try? FileManager.default.url(
            for: .applicationSupportDirectory,
            in: .userDomainMask,
            appropriateFor: nil,
            create: false
        ),
              let caps = try? support.resourceValues(forKeys: [.volumeAvailableCapacityForImportantUsageKey]),
              let bytes = caps.volumeAvailableCapacityForImportantUsage,
              bytes >= 2 * 1024 * 1024 * 1024
        else {
            hkLog("[AppDelegate] Parakeet preload skipped — disk gate (< 2 GB free)")
            return
        }

        didStartParakeetPreload = true
        hkLog("[AppDelegate] Parakeet onboarding preload starting (~466 MB)")
        appState.localSttBackend = "parakeet"
        appState.parakeetDownloadProgress = 0.0
        appState.isDownloadingParakeet = true
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            let ok = DimmyCore.shared.downloadParakeetBundle()
            DispatchQueue.main.async {
                guard let self else { return }
                self.appState.isDownloadingParakeet = false
                self.appState.parakeetBundlePresent = ok
                hkLog("[AppDelegate] Parakeet onboarding preload done (ok=\(ok))")
            }
        }
    }

    /// Spawn a one-shot Parakeet warmup if (a) the active backend is
    /// Parakeet, (b) the bundle is on disk, and (c) we haven't already
    /// warmed in this session. Idempotent across calls — the Rust side
    /// short-circuits if the model cache is already populated.
    private var didWarmupParakeet = false
    func maybeWarmupParakeet() {
        guard !didWarmupParakeet else { return }
        guard appState.localSttBackend == "parakeet" else { return }
        guard DimmyCore.shared.isInitialized else { return }
        guard DimmyCore.shared.parakeetBundlePresent() else { return }
        didWarmupParakeet = true
        hkLog("[AppDelegate] Parakeet warmup → background")
        DispatchQueue.global(qos: .utility).async {
            let t0 = Date()
            let ok = DimmyCore.shared.warmupParakeet()
            let ms = Int(Date().timeIntervalSince(t0) * 1000)
            hkLog("[AppDelegate] Parakeet warmup done in \(ms) ms (ok=\(ok))")
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

    /// Handle `dimmy://` URLs delivered by the OS (clicking the magic
    /// link in the activation email opens this scheme — see
    /// CFBundleURLTypes in Info.plist).
    ///
    /// On `dimmy://activate?code=…` we redeem the code via the licensing
    /// FFI in the background, then post `.dimmyLicenseChanged` so any
    /// open Settings → License view refreshes its status without polling.
    /// On success we also surface Settings to the foreground so the user
    /// gets a visible confirmation that activation completed.
    func application(_ application: NSApplication, open urls: [URL]) {
        for url in urls {
            guard url.scheme?.lowercased() == "dimmy" else { continue }
            hkLog("[AppDelegate] custom-URL received: \(url.absoluteString)")

            // Expected forms:
            //   dimmy://activate?code=ABC123…           (magic link)
            //   dimmy://activate?token=eyJhbGc…          (paste-token fallback — TBD)
            guard let comps = URLComponents(url: url, resolvingAgainstBaseURL: false),
                  comps.host?.lowercased() == "activate" else {
                hkLog("[AppDelegate] unrecognised URL host: \(url)")
                continue
            }
            let code = comps.queryItems?.first { $0.name == "code" }?.value
            let token = comps.queryItems?.first { $0.name == "token" }?.value
            if let code = code, !code.isEmpty {
                hkLog("[AppDelegate] activation code received (len=\(code.count))")
                let label = Host.current().localizedName ?? "Mac"
                Task.detached { [weak self] in
                    let result = await DimmyCore.shared.licenseRedeem(code: code, deviceLabel: label)
                    if result.ok {
                        hkLog("[AppDelegate] licensed activated via dimmy:// scheme")
                    } else {
                        hkLog("[AppDelegate] license activation failed: \(result.error ?? "unknown")")
                    }
                    await MainActor.run {
                        if result.ok {
                            // Post the change notification AFTER opening the
                            // License tab so the freshly-mounted page is
                            // already subscribed. Posting before the tab
                            // switch raced with view materialisation and
                            // left the License page showing stale state.
                            // Bring Settings to the front so the user sees
                            // the confirmation. NSApp.activate is reliable
                            // here because we're responding to a user-
                            // initiated open-URL event.
                            self?.openSettingsToLicense()
                        } else {
                            // On failure the License page may already be
                            // visible — let it refresh to surface the error.
                            NotificationCenter.default.post(name: .dimmyLicenseChanged, object: nil)
                        }
                    }
                }
            } else if token != nil {
                hkLog("[AppDelegate] activation token (paste fallback) received — not yet wired")
            } else {
                hkLog("[AppDelegate] activate URL missing both code and token")
            }
        }
    }

    /// Bring the Settings window to the front (creating it if needed) and
    /// scroll to the License page. Called from the dimmy:// dispatch so
    /// successful activation has a visible end-state. The container view
    /// observes `dimmyOpenLicenseTab` to handle the cross-component nav.
    private func openSettingsToLicense() {
        NSApp.activate(ignoringOtherApps: true)
        openSettings()
        // Two-stage signal: switch tab first so MacLicensePage is mounted,
        // THEN nudge it to refresh. .onAppear already calls refreshStatus
        // on first mount, but the explicit dimmyLicenseChanged also covers
        // the path where the page was already mounted (Settings open on
        // License tab before the URL fired) and SwiftUI is short-circuiting
        // the re-mount.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.15) {
            NotificationCenter.default.post(name: .dimmyOpenLicenseTab, object: nil)
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.05) {
                NotificationCenter.default.post(name: .dimmyLicenseChanged, object: nil)
            }
        }
    }

    /// Right-click menu on the Dock icon. Mirrors the Translate-to and Style
    /// submenus from the menu-bar NSMenu so users get the same quick switchers
    /// regardless of which entry point they prefer. Rebuilt by AppKit on every
    /// right-click, so checkmarks always reflect the current state.
    func applicationDockMenu(_ sender: NSApplication) -> NSMenu? {
        guard let sbc = statusBarController else { return nil }
        hkLog("[AppDelegate] applicationDockMenu rebuilt — pillVisible=\(appState.pillVisible)")
        let menu = NSMenu()
        // AppKit's auto-validation can silently disable items in the dock
        // context (the dock-menu responder chain doesn't include this
        // delegate the way the menubar one does). Disable auto-validation
        // and own enable/disable explicitly via isEnabled below.
        menu.autoenablesItems = false

        // Settings entry — same surface as the pill right-click
        // (PillWindowController:171) and the menubar app menu. Cmd+,
        // is the AppKit standard preferences shortcut and is shown
        // next to the title automatically. Without this, users who
        // hide the pill have no Dock affordance to reach Settings.
        let settingsItem = NSMenuItem(title: "Settings…",
                                      action: #selector(openSettingsFromDock),
                                      keyEquivalent: ",")
        settingsItem.target = self
        settingsItem.isEnabled = true
        menu.addItem(settingsItem)

        menu.addItem(NSMenuItem.separator())

        let translateLabel = appState.llmTranslateTo.isEmpty || appState.llmTranslateTo == "none"
            ? "(none)"
            : appState.llmTranslateTo
        let translateItem = NSMenuItem(title: "Translate to: \(translateLabel)",
                                       action: nil, keyEquivalent: "")
        translateItem.submenu = sbc.buildTranslateToSubmenu()
        menu.addItem(translateItem)

        let styleItem = NSMenuItem(title: "Style: \(appState.llmStyleEnum.displayName)",
                                   action: nil, keyEquivalent: "")
        styleItem.submenu = sbc.buildStyleSubmenu()
        menu.addItem(styleItem)

        menu.addItem(NSMenuItem.separator())

        // Toggle pill visibility (independent from system "Hide Dimmy",
        // which hides the whole app). Checkmark is rebuilt by AppKit on
        // every right-click, so it always reflects appState.pillVisible.
        // Dynamic title (no checkmark) — checkmarks on Dock items behave
        // oddly in macOS Tahoe (state=.off items silently swallow clicks
        // without invoking the action). Plain title-flipping avoids the
        // whole state machinery and reads better to the user anyway.
        // "Pill Overlay" disambiguates from the system "Hide Dimmy" item
        // that AppKit always appends below — that one is Cmd+H (hides
        // the whole app), this one only toggles the floating pill.
        let pillTitle = appState.pillVisible ? "Hide Pill Overlay" : "Show Pill Overlay"
        let pillItem = NSMenuItem(title: pillTitle,
                                  action: #selector(toggleDockPillVisibility),
                                  keyEquivalent: "")
        pillItem.target = self
        pillItem.isEnabled = true
        menu.addItem(pillItem)

        let meetingItem = NSMenuItem(title: "Open Meeting…",
                                     action: #selector(openMeetingFromDock),
                                     keyEquivalent: "")
        meetingItem.target = self
        meetingItem.isEnabled = true
        menu.addItem(meetingItem)

        // Command Mode toggle — mirror of pill / status-bar entries.
        // Title-flip (no checkmark) for the same Tahoe Dock-menu reason
        // documented above on the pill-visibility item: state=.off items
        // can silently swallow clicks in the Dock context.
        let commandTitle = appState.commandMode
            ? "✓ Command (edit selection)"
            : "Command (edit selection)"
        let commandItem = NSMenuItem(title: commandTitle,
                                     action: #selector(toggleDockCommandMode),
                                     keyEquivalent: "")
        commandItem.target = self
        commandItem.isEnabled = true
        menu.addItem(commandItem)

        return menu
    }

    @objc func openMeetingFromDock() {
        openMeetingWindow()
    }

    @objc func openSettingsFromDock() {
        openSettings()
    }

    @objc func toggleDockCommandMode() {
        hkLog("[AppDelegate] dock menu: toggle command mode (was=\(appState.commandMode))")
        appState.commandMode.toggle()
    }

    @objc func toggleDockPillVisibility() {
        hkLog("[AppDelegate] dock menu: toggle pill visibility (was=\(appState.pillVisible))")
        appState.pillVisible.toggle()
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
        // Only refresh config from Rust if the core has been initialized.
        // Without this guard, opening Settings before the Rust core comes up
        // (e.g. permissions still pending) panics inside dimmy_get_config_json.
        if DimmyCore.shared.isInitialized, let config = DimmyCore.shared.getConfig() {
            appState.loadFromRustConfig(config)
        }

        if let settingsWindow, settingsWindow.isVisible {
            settingsWindow.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }

        let useTahoe = appState.useTahoeSettings
        let rootView: AnyView = useTahoe
            ? AnyView(MacSettingsContainerView(appState: appState))
            : AnyView(SettingsContainerView(appState: appState))

        let initialSize: NSSize = useTahoe
            ? NSSize(width: 1000, height: 740)
            : NSSize(width: 620, height: 500)

        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: initialSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.center()
        window.title = "Dimmy Settings"
        // FirstMouseHostingView lets controls accept the click that
        // also activates the window — fixes the "Advanced toggle does
        // nothing on first click" pattern reported on macOS.
        window.contentView = FirstMouseHostingView(rootView: rootView)
        window.isReleasedWhenClosed = false
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)

        self.settingsWindow = window
    }

    func reopenOnboarding() {
        showOnboarding(startStep: 0)
    }

    /// Open the dedicated MeetingWindow (Phase 4 entry point). Called
    /// from the menubar menu, dock menu, and pill right-click. The
    /// controller is a singleton so reopening with a meeting in flight
    /// activates the existing window instead of forking state.
    func openMeetingWindow() {
        // Kick off core init if it hasn't run yet (mic-not-authorized path).
        // The window can open without the core; Start button handles the
        // not-initialised case with a clear status message.
        if !DimmyCore.shared.isInitialized {
            initializeCoreAsync()
        }
        MeetingWindowController.shared.show()
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
        // FirstMouseHostingView so the Next / preset buttons work on the
        // first click after the window appears (e.g. when launched from
        // the menubar with another app frontmost).
        window.contentView = FirstMouseHostingView(rootView: onboardingView)
        window.isReleasedWhenClosed = false

        self.onboardingWindow = window
        applyActivationPolicy()
        window.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }

    // MARK: - NSWindowDelegate

    func windowWillClose(_ notification: Notification) {
        guard let window = notification.object as? NSWindow, window === onboardingWindow else { return }
        // Abandonment funnel terminal: the window is closing AND
        // `isOnboardingComplete` is still false, which means the user
        // dismissed via the red close button without reaching
        // TryItStepView's "Start Using Dimmy" CTA. Skipped when the
        // user completed normally (TryItStepView already emitted
        // `.completed` and flipped the flag). `last_step` is the
        // step they were viewing when they bailed — drives the
        // drop-off-by-step funnel in PostHog.
        if !appState.isOnboardingComplete {
            // Best-effort: pull current step out of the hosted
            // SwiftUI view via UserDefaults so we don't have to
            // thread a binding through. Falls back to "welcome" if
            // not yet written (very early dismissal).
            let lastStep = OnboardingContainerView.stepName(
                UserDefaults.standard.integer(forKey: "onboarding.lastSeenStep")
            )
            DimmyCore.shared.trackEvent("onboarding.abandoned", [
                "last_step": lastStep,
            ])
        }
        onboardingWindow = nil
        applyActivationPolicy()
    }

    // MARK: - macOS Services provider
    //
    // Selector is wired in Info.plist via NSServices →
    // NSMessage = "addToDimmyDictionary". The OS pre-fills `pboard`
    // with the user's current selection — no synthetic Cmd+C needed,
    // so we bypass the probe/copy dance in DictHotkeyManager and call
    // straight into `AddToDictionaryFlow.runWithText`. The method
    // signature is fixed by Cocoa: any deviation (different param
    // names / types / `@objc` style) and the Services system
    // silently fails to bind the menu item.
    @objc func addToDimmyDictionary(
        _ pboard: NSPasteboard,
        userData: String,
        error: AutoreleasingUnsafeMutablePointer<NSString?>
    ) {
        guard let text = pboard.string(forType: .string), !text.isEmpty else {
            error.pointee = "No text selected" as NSString
            NSLog("[Dict/Services] called with empty pasteboard")
            return
        }
        NSLog("[Dict/Services] received \(text.count) chars from selection")
        Task { @MainActor in
            await AddToDictionaryFlow.runWithText(text)
        }
    }
}

// MARK: - Free-standing crash handlers
//
// These live at top level so they convert to C function pointers
// (NSSetUncaughtExceptionHandler / signal(2) both want plain
// `@convention(c)` callbacks — captured-context closures don't bridge).

private func crashLogPath() -> String {
    // Match the Rust core's `config_dir_name()` — `dimmy-staging` for
    // staging-flavor builds, `dimmy` otherwise. Hardcoding "dimmy"
    // here would mean staging crash output lands in the prod log
    // (cross-contaminating diagnostics) and prod testers can't find
    // their own crash trail. Same root-cause class as the Win
    // app_rules-wipe bug fixed 2026-05-12. The DimmyCore.shared
    // accessor reads `dimmy_build_flavor()` from FFI; on a pristine
    // launch where FFI hasn't been loaded yet, configDirURL is nil
    // and we fall back to /tmp.
    let dir = DimmyCore.shared.configDirURL
        ?? FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first?.appendingPathComponent(DimmyCore.shared.configDirName, isDirectory: true)
    return dir?.appendingPathComponent("dimmy.log").path
        ?? "/tmp/dimmy-crash.log"
}

private func crashAppendLine(_ line: String) {
    let path = crashLogPath()
    let stamped = "[\(ISO8601DateFormatter().string(from: Date()))] [crash] \(line)\n"
    if let data = stamped.data(using: .utf8) {
        if let handle = FileHandle(forWritingAtPath: path) {
            handle.seekToEndOfFile()
            handle.write(data)
            try? handle.close()
        } else {
            try? stamped.write(toFile: path, atomically: false, encoding: .utf8)
        }
    }
    FileHandle.standardError.write(stamped.data(using: .utf8) ?? Data())
}

private func crashHandleNSException(_ exc: NSException) {
    let name = exc.name.rawValue
    let reason = exc.reason ?? ""
    let symbols = exc.callStackSymbols.joined(separator: "\n  ")
    crashAppendLine("NSException name=\(name) reason=\"\(reason)\"\n  \(symbols)")
}

private func crashHandleSignal(_ sig: Int32) {
    let name: String
    switch sig {
    case SIGSEGV: name = "SIGSEGV"
    case SIGABRT: name = "SIGABRT"
    case SIGBUS:  name = "SIGBUS"
    case SIGILL:  name = "SIGILL"
    case SIGFPE:  name = "SIGFPE"
    default:      name = "signal=\(sig)"
    }
    let backtrace = Thread.callStackSymbols.joined(separator: "\n  ")
    crashAppendLine("\(name)\n  \(backtrace)")
    // Re-raise via default handler so the exit code is correct + Apple
    // CrashReporter still files an .ips at the system level.
    signal(sig, SIG_DFL)
    raise(sig)
}
