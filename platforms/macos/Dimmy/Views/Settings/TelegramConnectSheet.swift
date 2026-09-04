import SwiftUI

/// Telegram login sheet: phone -> code -> optional 2FA password.
/// Mac mirror of the inline login panels in Win
/// `Views/SettingsWindow.Telegram.cs`. Unlike the linear Notion wizard,
/// this is an event-driven state machine: the panel shown is chosen by
/// `appState.telegramPhase`, which the Rust worker drives via the
/// `telegram_state` event. Submitting a step only queues a command; the
/// next phase (or `telegramError`) decides what renders next.
struct TelegramConnectSheet: View {
    @ObservedObject var appState: AppState
    let onClose: () -> Void

    @State private var phone: String = ""
    @State private var code: String = ""
    @State private var password: String = ""
    @State private var busy: Bool = false

    var body: some View {
        VStack(spacing: 16) {
            header
            Divider()

            ScrollView {
                Group {
                    switch appState.telegramPhase {
                    case "no_credentials":
                        noCredentials
                    case "wait_code":
                        codeStep
                    case "wait_password":
                        passwordStep
                    case "connected":
                        connectedStep
                    default:
                        phoneStep
                    }
                }
                .padding(.vertical, 4)
            }
            .frame(minHeight: 220)

            if let err = appState.telegramError, !err.isEmpty {
                Text(err)
                    .font(.system(size: 12))
                    .foregroundStyle(.red)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .lineLimit(2)
            }

            Divider()
            footer
        }
        .padding(20)
        .frame(width: 520)
        .onAppear {
            // Enabling persists the master switch AND starts the worker
            // (dimmy_set_config_json -> telegram::set_enabled), so the
            // login command has a live worker to talk to.
            if !appState.telegramEnabled {
                appState.telegramEnabled = true
                DimmyCore.shared.setConfig(appState.toRustConfig())
            }
            appState.telegramError = nil
            TelegramService.shared.refreshState(appState: appState)
        }
        .onChange(of: appState.telegramPhase) { _, newPhase in
            // Any phase advance means the last command was accepted.
            busy = false
            if newPhase == "connected" {
                // Login done — close; the Settings card shows the
                // connected state + account behind the sheet.
                onClose()
            }
        }
        .onChange(of: appState.telegramError) { _, err in
            if let err, !err.isEmpty { busy = false }
        }
    }

    // MARK: - Header / footer

    private var header: some View {
        HStack(spacing: 10) {
            Image(systemName: "paperplane.circle.fill")
                .font(.system(size: 20))
                .foregroundStyle(Color(red: 0.16, green: 0.63, blue: 0.86))
            Text("Connect Telegram")
                .font(.system(size: 16, weight: .semibold))
            Spacer()
        }
    }

    private var footer: some View {
        HStack {
            Button("Cancel") { onClose() }
                .keyboardShortcut(.cancelAction)
            Spacer()
        }
    }

    // MARK: - Steps

    private var phoneStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Log in with your Telegram account")
                .font(.system(size: 16, weight: .semibold))
            Text("Record on your phone, forward the audio to your own Saved Messages, and Dimmy transcribes + recaps it here. Dimmy signs in as you via Telegram's official protocol; nothing is posted, nothing else is read.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            TextField("+39 333 1234567", text: $phone)
                .textFieldStyle(.roundedBorder)
                .disableAutocorrection(true)

            HStack(spacing: 10) {
                Button("Send code") { sendCode() }
                    .buttonStyle(.borderedProminent)
                    .disabled(busy || phone.trimmingCharacters(in: .whitespaces).isEmpty)
                if busy { ProgressView().controlSize(.small) }
                Spacer()
            }
        }
    }

    private var codeStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Enter the code")
                .font(.system(size: 16, weight: .semibold))
            Text("Telegram sent a login code to your other devices (or by SMS). Enter it below.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)

            TextField("12345", text: $code)
                .textFieldStyle(.roundedBorder)

            HStack(spacing: 10) {
                Button("Confirm") { submitCode() }
                    .buttonStyle(.borderedProminent)
                    .disabled(busy || code.trimmingCharacters(in: .whitespaces).isEmpty)
                if busy { ProgressView().controlSize(.small) }
                Spacer()
            }
        }
    }

    private var passwordStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Two-step verification")
                .font(.system(size: 16, weight: .semibold))
            Text("Your account has a cloud password. Enter it to finish signing in.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)

            SecureField("Cloud password", text: $password)
                .textFieldStyle(.roundedBorder)

            HStack(spacing: 10) {
                Button("Confirm") { submitPassword() }
                    .buttonStyle(.borderedProminent)
                    .disabled(busy || password.isEmpty)
                if busy { ProgressView().controlSize(.small) }
                Spacer()
            }
        }
    }

    private var connectedStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(.green)
                Text(appState.telegramAccount.isEmpty
                     ? "Connected."
                     : "Connected as \(appState.telegramAccount).")
                    .font(.system(size: 14, weight: .semibold))
            }
            Text("Forward any audio to your Telegram Saved Messages and Dimmy will pick it up.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
        }
    }

    private var noCredentials: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Telegram isn't available in this build")
                .font(.system(size: 16, weight: .semibold))
            Text("This build was compiled without Telegram API credentials, so the inbox can't run. A future update will enable it.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
        }
    }

    // MARK: - Actions

    private func sendCode() {
        appState.telegramError = nil
        busy = true
        let rc = DimmyCore.shared.telegramStartLogin(
            phone: phone.trimmingCharacters(in: .whitespacesAndNewlines))
        if rc != 0 {
            busy = false
            appState.telegramError = rc == -100
                ? "This build has no Telegram support."
                : "Could not start login. Check the phone number and try again."
        }
    }

    private func submitCode() {
        appState.telegramError = nil
        busy = true
        let rc = DimmyCore.shared.telegramSubmitCode(
            code.trimmingCharacters(in: .whitespacesAndNewlines))
        if rc != 0 {
            busy = false
            appState.telegramError = "Could not submit the code."
        }
    }

    private func submitPassword() {
        appState.telegramError = nil
        busy = true
        let rc = DimmyCore.shared.telegramSubmitPassword(password)
        if rc != 0 {
            busy = false
            appState.telegramError = "Could not submit the password."
        }
    }
}
