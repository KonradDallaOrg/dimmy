import SwiftUI

/// Paints the core's QR module grid (row-major, "1" = dark) with a
/// 4-module quiet zone, filled as one path so modules show no seams.
private struct TelegramQrView: View {
    let size: Int
    let modules: String

    var body: some View {
        Canvas { ctx, area in
            ctx.fill(Path(CGRect(origin: .zero, size: area)), with: .color(.white))
            guard size > 0, modules.utf8.count == size * size else { return }
            let unit = min(area.width, area.height) / CGFloat(size + 8)
            var path = Path()
            for (i, ch) in modules.utf8.enumerated() where ch == UInt8(ascii: "1") {
                path.addRect(CGRect(x: CGFloat(i % size + 4) * unit,
                                    y: CGFloat(i / size + 4) * unit,
                                    width: unit, height: unit))
            }
            ctx.fill(path, with: .color(.black))
        }
        .clipShape(RoundedRectangle(cornerRadius: 8))
    }
}

/// Telegram login sheet: QR code, or phone -> code, then optional 2FA password.
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
                    case "wait_qr":
                        qrStep
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
            Button("Cancel") {
                // A QR login left running would keep refreshing its code.
                if appState.telegramPhase.hasPrefix("wait_") {
                    DimmyCore.shared.telegramCancelLogin()
                }
                onClose()
            }
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

            HStack(spacing: 10) {
                Button("Log in with QR code") { startQrLogin() }
                    .buttonStyle(.borderedProminent)
                    .disabled(busy)
                if busy { ProgressView().controlSize(.small) }
                Spacer()
            }

            Text("Or use your phone number, with country code. The code arrives in the Telegram app, not by SMS.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            TextField("+39 333 1234567", text: $phone)
                .textFieldStyle(.roundedBorder)
                .disableAutocorrection(true)

            HStack(spacing: 10) {
                Button("Send code") { sendCode() }
                    .buttonStyle(.bordered)
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
            Text("Look in the Telegram app on your phone: the code arrives as a message from Telegram, usually not by SMS.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            TextField("12345", text: $code)
                .textFieldStyle(.roundedBorder)

            HStack(spacing: 10) {
                Button("Confirm") { submitCode() }
                    .buttonStyle(.borderedProminent)
                    .disabled(busy || code.trimmingCharacters(in: .whitespaces).isEmpty)
                if busy { ProgressView().controlSize(.small) }
                Spacer()
                // Back to the number, kept filled in, to ask again or switch
                // to the QR code.
                Button("Didn't get it? Go back") { cancelLogin() }
                    .buttonStyle(.link)
            }
        }
    }

    private var qrStep: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Scan with Telegram")
                .font(.system(size: 16, weight: .semibold))
            Text("On your phone, open Telegram, go to Settings, Devices, Link Desktop Device, and scan this code.")
                .font(.system(size: 12))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            TelegramQrView(size: appState.telegramQrSize, modules: appState.telegramQrModules)
                .frame(width: 220, height: 220)

            Button("Use phone number instead") { cancelLogin() }
                .buttonStyle(.link)
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

    private func startQrLogin() {
        appState.telegramError = nil
        busy = true
        let rc = DimmyCore.shared.telegramStartQrLogin()
        if rc != 0 {
            busy = false
            appState.telegramError = rc == -100
                ? "This build has no Telegram support."
                : "Could not start login. Try again."
        }
    }

    /// Back to the phone step from the code or QR step. The core drops the
    /// login in progress, so a QR code stops refreshing behind the sheet.
    private func cancelLogin() {
        code = ""
        busy = false
        appState.telegramError = nil
        DimmyCore.shared.telegramCancelLogin()
        appState.telegramPhase = "logged_out"
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
