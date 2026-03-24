import SwiftUI

struct WelcomeStepView: View {
    let onContinue: () -> Void

    var body: some View {
        VStack(spacing: 20) {
            Spacer()

            Image(systemName: "waveform.circle.fill")
                .font(.system(size: 64))
                .foregroundStyle(.tint)

            Text("Dimmy")
                .font(.system(size: 32, weight: .bold, design: .rounded))

            Text("Voice dictation that stays out of your way")
                .font(.system(size: 15))
                .foregroundColor(.secondary)

            Text("Hold a shortcut, speak, release.\nYour words appear wherever you're typing.")
                .font(.system(size: 13))
                .foregroundColor(.secondary)
                .multilineTextAlignment(.center)
                .lineSpacing(4)

            Spacer()

            Button(action: onContinue) {
                Text("Get Started")
                    .font(.system(size: 14, weight: .semibold))
                    .frame(maxWidth: 200)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)

            Spacer().frame(height: 20)
        }
        .padding(.horizontal, 40)
    }
}
