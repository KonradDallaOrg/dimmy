import SwiftUI

struct OnboardingContainerView: View {
    static let totalSteps = 4

    @ObservedObject var appState: AppState
    @ObservedObject private var perms = PermissionsManager.shared
    @State private var currentStep: Int

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
                    TryItStepView(appState: appState) {
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
        if currentStep == 2 {
            // Entering TryIt — trigger the pill intro animation.
            appState.showPillIntro = true
        }
        withAnimation { currentStep += 1 }
    }
}
