import SwiftUI

struct AboutSettingsView: View {
    var body: some View {
        VStack(spacing: 16) {
            Spacer()

            Image(systemName: "waveform.circle.fill")
                .font(.system(size: 48))
                .foregroundStyle(.tint)

            Text("Dimmy")
                .font(.system(size: 20, weight: .bold, design: .rounded))

            Text("Version 0.1.0 (Prototype)")
                .font(.system(size: 12))
                .foregroundColor(.secondary)

            Text("Voice dictation that stays out of your way")
                .font(.system(size: 12))
                .foregroundColor(.secondary)

            Divider()
                .frame(width: 200)

            Text("Made with irony")
                .font(.system(size: 11))
                .foregroundColor(Color(nsColor: .tertiaryLabelColor))

            Spacer()
        }
        .frame(width: 400, height: 250)
    }
}
