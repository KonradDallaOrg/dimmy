import SwiftUI

struct WelcomeStepView: View {
    /// The first-run gate: the container's Next stays disabled until ticked.
    @Binding var termsAccepted: Bool

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

            VStack(spacing: 4) {
                Toggle(isOn: $termsAccepted) {
                    Text("I accept the Terms of Service and End-User Licence Agreement")
                        .font(.system(size: 12))
                }
                .toggleStyle(.checkbox)

                Link("Read the terms", destination: TermsPrompt.url)
                    .font(.system(size: 12))
            }
            .padding(.top, 8)

            Spacer()
        }
        .padding(.horizontal, 40)
    }
}

/// Existing installs: asked once per launch until the current Terms version
/// is accepted. Never blocks anything. "Later" leaves the answer unrecorded,
/// so it comes back next launch.
enum TermsPrompt {
    static let url = URL(string: "https://dimmy.app/terms/")!

    /// `cfg` is the config snapshot the caller already read at launch.
    static func isAccepted(in cfg: [String: Any]?) -> Bool {
        let current = DimmyCore.shared.termsVersion
        return current.isEmpty || (cfg?["terms_accepted_version"] as? String) == current
    }

    @MainActor
    static func present() {
        let alert = NSAlert()
        alert.messageText = "Dimmy terms"
        alert.informativeText =
            "Please read and accept the Terms of Service and End-User Licence Agreement."
        let accept = alert.addButton(withTitle: "Accept")
        alert.addButton(withTitle: "Later")
        accept.isEnabled = false

        let handler = TermsAlertHandler(accept: accept)
        let check = NSButton(
            checkboxWithTitle: "I accept the Terms of Service and End-User Licence Agreement",
            target: handler, action: #selector(TermsAlertHandler.toggled(_:)))
        let link = NSButton(
            title: "Read the terms", target: handler,
            action: #selector(TermsAlertHandler.openTerms(_:)))
        link.isBordered = false
        link.contentTintColor = .linkColor

        let stack = NSStackView(views: [check, link])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 6
        stack.setFrameSize(stack.fittingSize)
        alert.accessoryView = stack

        if alert.runModal() == .alertFirstButtonReturn {
            DimmyCore.shared.acceptTerms()
        }
        withExtendedLifetime(handler) {}
    }
}

private final class TermsAlertHandler: NSObject {
    private let accept: NSButton

    init(accept: NSButton) {
        self.accept = accept
    }

    @objc func toggled(_ sender: NSButton) {
        accept.isEnabled = sender.state == .on
    }

    @objc func openTerms(_ sender: Any?) {
        NSWorkspace.shared.open(TermsPrompt.url)
    }
}
