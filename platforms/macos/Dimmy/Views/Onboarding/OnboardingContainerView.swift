import SwiftUI

struct OnboardingContainerView: View {
    static let totalSteps = 5

    /// Step index → canonical name shared with Win + Rust telemetry
    /// allowlist. Keep aligned with `core/src/ffi.rs::
    /// dimmy_telemetry_track_typed`'s onboarding allowlist.
    static func stepName(_ index: Int) -> String {
        switch index {
        case 0: return "welcome"
        case 1: return "permissions"
        case 2: return "shortcut"
        case 3: return "model_download"
        case 4: return "try_it"
        default: return "welcome"
        }
    }

    @ObservedObject var appState: AppState
    @ObservedObject private var perms = PermissionsManager.shared
    @State private var currentStep: Int
    @State private var onboardingStartedAt: Date = Date()
    @State private var onboardingStartEmitted: Bool = false

    init(appState: AppState, startStep: Int = 0) {
        self.appState = appState
        let clamped = max(0, min(startStep, Self.totalSteps - 1))
        self._currentStep = State(initialValue: clamped)
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                ForEach(0..<Self.totalSteps, id: \.self) { index in
                    Circle()
                        .fill(index <= currentStep ? Color.accentColor : Color.secondary.opacity(0.3))
                        .frame(width: 8, height: 8)
                }
            }
            .padding(.top, 20)

            Group {
                switch currentStep {
                case 0:
                    WelcomeStepView()
                case 1:
                    PermissionsStepView(appState: appState)
                case 2:
                    ShortcutStepView(appState: appState)
                case 3:
                    ModelDownloadStepView(appState: appState)
                case 4:
                    TryItStepView(appState: appState) {
                        emitOnboardingCompleted()
                        appState.isOnboardingComplete = true
                    }
                default:
                    EmptyView()
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .transition(.asymmetric(
                insertion: .move(edge: .trailing).combined(with: .opacity),
                removal: .move(edge: .leading).combined(with: .opacity)
            ))

            footer
        }
        .frame(width: 520, height: 460)
        .boldUI()
        .onAppear {
            // Funnel anchor. Single emission per container lifecycle —
            // `onAppear` fires on every view re-display (window
            // re-shown after dismissal), but the wizard is destroyed +
            // recreated on each open so per-attempt firing is correct.
            // Guards against SwiftUI's transient re-onAppear during
            // animations.
            if !onboardingStartEmitted {
                onboardingStartEmitted = true
                onboardingStartedAt = Date()
                DimmyCore.shared.trackEvent("onboarding.started")
            }
            // Persist the step index so AppDelegate's
            // windowWillClose can read it for the `onboarding.
            // abandoned` event's `last_step` payload. UserDefaults
            // is cheap, syncs on every step transition below.
            UserDefaults.standard.set(currentStep, forKey: "onboarding.lastSeenStep")
        }
        .onChange(of: currentStep) { _, new in
            UserDefaults.standard.set(new, forKey: "onboarding.lastSeenStep")
        }
    }

    /// Back is always visible (disabled on step 0). Next disappears on the final step —
    /// TryIt has its own "Start Using Dimmy" / "Skip for now" buttons.
    private var footer: some View {
        HStack {
            Button(action: goBack) {
                Label("Back", systemImage: "chevron.left")
                    .labelStyle(.titleAndIcon)
            }
            .buttonStyle(.bordered)
            .controlSize(.regular)
            .disabled(currentStep == 0)

            Spacer()

            // TryIt (last step) drives its own primary action ("Start Using
            // Dimmy" + Skip); all earlier steps use the container's Next.
            if currentStep < Self.totalSteps - 1 {
                Button(action: goNext) {
                    HStack(spacing: 4) {
                        Text(primaryLabel)
                        Image(systemName: "chevron.right")
                    }
                }
                .buttonStyle(.borderedProminent)
                .controlSize(.regular)
                .keyboardShortcut(.return, modifiers: [])
            }
        }
        .padding(.horizontal, 28)
        .padding(.vertical, 14)
    }

    private var primaryLabel: String {
        switch currentStep {
        case 1:
            return perms.allRequiredGranted ? "Next" : "Continue anyway"
        default:
            return "Next"
        }
    }

    private func goBack() {
        guard currentStep > 0 else { return }
        withAnimation { currentStep -= 1 }
    }

    private func goNext() {
        if currentStep == Self.totalSteps - 1 { return }
        let leaving = Self.stepName(currentStep)
        if currentStep == 3 {
            // Entering TryIt — trigger the pill intro animation.
            appState.showPillIntro = true
        }
        withAnimation { currentStep += 1 }
        DimmyCore.shared.trackEvent("onboarding.step_completed", ["step": leaving])
    }

    /// Called by `TryItStepView` when the user hits "Start Using
    /// Dimmy". Sets `isOnboardingComplete` AND emits the funnel
    /// terminal `onboarding.completed` event with the chosen STT
    /// path + duration. Distinguished from the abandonment terminal
    /// (window dismissed before TryIt's CTA): AppDelegate's window-
    /// close observer emits `.abandoned` when `isOnboardingComplete`
    /// is still false at close time.
    func emitOnboardingCompleted() {
        let path: String
        if appState.sttMode == "cloud" {
            path = "cloud"
        } else if appState.sttMode == "local" {
            path = "local"
        } else {
            path = "unknown"
        }
        let duration = UInt64(max(0, Date().timeIntervalSince(onboardingStartedAt)))
        DimmyCore.shared.trackEvent("onboarding.completed", [
            "path": path,
            "duration_secs": duration,
        ])
    }
}
