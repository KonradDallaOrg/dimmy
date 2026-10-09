import SwiftUI

struct OnboardingContainerView: View {
    static let totalSteps = 6

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
        case 5: return "stay_updated"
        default: return "welcome"
        }
    }

    @ObservedObject var appState: AppState
    @ObservedObject private var perms = PermissionsManager.shared
    @State private var currentStep: Int
    @State private var onboardingStartedAt: Date = Date()
    @State private var onboardingStartEmitted: Bool = false
    /// Wizard picked on the TryIt success page, opened once onboarding ends.
    @State private var wizardAfterFinish: WizardWindowController.Kind? = nil

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
                    TryItStepView(appState: appState) { wizard in
                        wizardAfterFinish = wizard
                        continueToUpdates()
                    }
                case 5:
                    StayUpdatedStepView(onFinish: finish)
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

    /// Back is always visible (disabled on step 0). Next disappears on the last two
    /// steps: TryIt and StayUpdated each have their own buttons.
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

            // TryIt and StayUpdated drive their own primary actions; all
            // earlier steps use the container's Next.
            if currentStep < Self.totalSteps - 2 {
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
        if currentStep >= Self.totalSteps - 2 { return }
        let leaving = Self.stepName(currentStep)
        if currentStep == 3 {
            // Entering TryIt — trigger the pill intro animation.
            appState.showPillIntro = true
        }
        withAnimation { currentStep += 1 }
        DimmyCore.shared.trackEvent("onboarding.step_completed", ["step": leaving])
    }

    /// Every way out of TryIt goes through the last step, except for someone
    /// who already has a license or a running trial: asking them for an
    /// email again would be asking for nothing.
    private func continueToUpdates() {
        let kind = DimmyCore.shared.licenseStatus().kind
        if kind == "Active" || kind == "TrialActive" {
            finish()
            return
        }
        withAnimation { currentStep = Self.totalSteps - 1 }
        DimmyCore.shared.trackEvent("onboarding.step_completed", ["step": Self.stepName(4)])
    }

    private func finish() {
        let wizard = wizardAfterFinish
        wizardAfterFinish = nil
        appState.showPillIntro = true
        emitOnboardingCompleted()
        appState.isOnboardingComplete = true
        if let wizard {
            WizardWindowController.shared.show(wizard, appState: appState)
        }
    }

    /// Called from `finish`, the single way onboarding ends
    /// successfully. Sets `isOnboardingComplete` AND emits the funnel
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

/// Whether what the user typed can be an email address. A shape check that
/// keeps a typo from reaching the server; the magic link is what proves the
/// address exists. Mirror of Win `EmailInput.LooksValid`.
enum EmailInput {
    static func looksValid(_ text: String) -> Bool {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.count <= 254 else { return false }
        return trimmed.range(
            of: #"^[^@\s]+@[^@\s.]+(\.[^@\s.]+)+$"#,
            options: .regularExpression) != nil
    }
}

/// Last onboarding step: thank you, and the one optional ask. Mirror of
/// Step4Panel in the Windows OnboardingWindow.
struct StayUpdatedStepView: View {
    let onFinish: () -> Void

    @State private var email: String = ""
    @State private var busy = false
    @State private var sent = false
    @State private var errorText: String? = nil

    private var canSend: Bool { !busy && EmailInput.looksValid(email) }

    var body: some View {
        VStack(spacing: 14) {
            Spacer(minLength: 4)

            Text("Thank you for installing Dimmy")
                .font(.system(size: 24, weight: .bold))
                .multilineTextAlignment(.center)

            if sent {
                sentView
            } else {
                askView
            }

            Spacer(minLength: 4)
        }
        .padding(.horizontal, 40)
    }

    private var askView: some View {
        VStack(spacing: 12) {
            Text("One last thing, and it is optional. Leave your email to start the free 14-day trial: Dimmy then updates itself, new engines included.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: 8) {
                TextField("you@example.com", text: $email)
                    .textFieldStyle(.roundedBorder)
                    .frame(width: 230)
                    .onSubmit { if canSend { send() } }
                Button(busy ? "Sending..." : "Start free trial") { send() }
                    .buttonStyle(.borderedProminent)
                    .disabled(!canSend)
            }

            if let errorText {
                Text(errorText)
                    .font(.system(size: 11))
                    .foregroundColor(.red)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Text("Already on a paid or company plan? Use that email and the link we send activates it here.")
                .font(.system(size: 11))
                .foregroundColor(Color(nsColor: .tertiaryLabelColor))
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)

            Text("Without an email Dimmy stays free, on this version: it will not update itself. A newer one is always a download away on dimmy.app.")
                .font(.system(size: 11))
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
                .padding(.top, 6)

            Button("Skip, stay on this version") { onFinish() }
                .buttonStyle(.bordered)
                .controlSize(.regular)
        }
    }

    private var sentView: some View {
        VStack(spacing: 14) {
            Image(systemName: "envelope.fill")
                .font(.system(size: 36))
                .foregroundColor(.accentColor)

            Text("Check your inbox. The link in the email activates Dimmy on this Mac; until you open it nothing changes.")
                .font(.system(size: 13))
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)

            Button(action: onFinish) {
                Text("Start Using Dimmy")
                    .font(.system(size: 14, weight: .semibold))
                    .frame(maxWidth: 200)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
        }
    }

    private func send() {
        guard canSend else { return }
        busy = true
        errorText = nil
        let address = email.trimmingCharacters(in: .whitespacesAndNewlines)
        Task {
            let result = await DimmyCore.shared.licenseRequestTrial(email: address)
            await MainActor.run {
                busy = false
                if result.ok {
                    sent = true
                } else {
                    errorText = "We could not send the email. Check your connection and try again, or skip: you can do this later in Settings, License."
                }
            }
        }
    }
}
