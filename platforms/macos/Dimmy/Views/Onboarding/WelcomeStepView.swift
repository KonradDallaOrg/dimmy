import SwiftUI

struct WelcomeStepView: View {
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
        }
        .padding(.horizontal, 40)
    }
}
