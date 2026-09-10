import AppKit
import SwiftUI

/// A clickable card: icon above, title, one line of description.
///
/// Neither platform had one. `MacRow` is strictly horizontal (icon leading,
/// text beside it) and `MacStatTile` stacks a value over a label but takes no
/// icon, so a "pick this setup" card had nowhere to live. This is modelled on
/// `MacStatTile`'s shape with `MacSquircleIcon` on top, and mirrors
/// `WizardCard` on Windows so the two platforms show recognisably the same
/// screen rather than two different ideas.
struct MacWizardCard: View {
    let icon: String
    let iconBackground: Color
    let title: String
    let description: String
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            VStack(spacing: 10) {
                MacSquircleIcon(systemName: icon, background: iconBackground)
                Text(title)
                    .font(.system(size: 13, weight: .semibold))
                    .multilineTextAlignment(.center)
                Text(description)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .padding(.vertical, 16)
            .padding(.horizontal, 12)
            .frame(maxWidth: .infinity)
            .background(
                RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                    .fill(Color(nsColor: .windowBackgroundColor)
                        .opacity(hovering ? 0.95 : 0.6))
            )
            .overlay(
                RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                    .strokeBorder(Color.primary.opacity(hovering ? 0.18 : 0.08), lineWidth: 1)
            )
        }
        // A plain Button keeps keyboard focus, Return and the accessibility
        // label; only the chrome is replaced. A tap-gesture on a shape would
        // have dropped all three.
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .accessibilityLabel("\(title). \(description)")
    }
}
