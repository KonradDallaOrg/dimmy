import SwiftUI

struct OnboardingContainerView: View {
    static let totalSteps = 4

    @ObservedObject var appState: AppState
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
                    WelcomeStepView {
                        withAnimation { currentStep = 1 }
                    }
                case 1:
                    PermissionsStepView(appState: appState) {
                        withAnimation { currentStep = 2 }
                    }
                case 2:
                    ShortcutStepView(appState: appState) {
                        appState.showPillIntro = true
                        withAnimation { currentStep = 3 }
                    }
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
        }
        .frame(width: 520, height: 440)
        .boldUI()
    }
}
