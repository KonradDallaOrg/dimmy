import SwiftUI

struct WelcomeStepView: View {
    var body: some View {
        VStack(spacing: 20) {
            Spacer()

            // The real brand squircle (theme-aware: white in light, gradient
            // in dark) rather than a generic SF symbol — onboarding is the
            // user's first impression.
            Image("DimmyAppIcon")
                .resizable()
                .interpolation(.high)
                .aspectRatio(contentMode: .fit)
                .frame(width: 96, height: 96)

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
