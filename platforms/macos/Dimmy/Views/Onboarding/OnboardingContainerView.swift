import SwiftUI

struct OnboardingContainerView: View {
    @ObservedObject var appState: AppState
    @State private var currentStep = 0

    var body: some View {
        VStack(spacing: 0) {
            // Progress dots
            HStack(spacing: 8) {
                ForEach(0..<5, id: \.self) { index in
                    Circle()
                        .fill(index <= currentStep ? Color.accentColor : Color.secondary.opacity(0.3))
                        .frame(width: 8, height: 8)
                }
            }
            .padding(.top, 20)

            // Step content
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
                    ModelDownloadStepView(appState: appState) {
                        withAnimation { currentStep = 3 }
                    }
                case 3:
                    ShortcutStepView(appState: appState) {
                        // Show pill with glow before Try It step
                        appState.showPillIntro = true
                        withAnimation { currentStep = 4 }
                    }
                case 4:
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
