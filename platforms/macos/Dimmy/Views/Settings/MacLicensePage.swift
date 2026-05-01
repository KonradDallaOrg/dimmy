import SwiftUI

// License — Tahoe Settings page mirroring Windows v3 Settings → License.
//
// Layout: status hero → activation form → capability grid → devices list →
// fallback paste-code (collapsed) → server URL override (collapsed).
// Wires all eight licensing FFI calls through `DimmyCore`. The auto-open
// flow on dimmy:// is handled by AppDelegate; this page just binds to the
// `dimmyLicenseChanged` notification to refresh after redeem completes.

struct MacLicensePage: View {
    @ObservedObject var appState: AppState

    @State private var status: DimmyCore.LicenseStatus =
        DimmyCore.shared.licenseStatus()
    @State private var devices: [DimmyCore.LicenseDeviceInfo] = []
    @State private var maxDevices: Int = 5
    @State private var devicesError: String? = nil

    @State private var trialEmail: String = ""
    @State private var trialStatus: String? = nil
    @State private var trialIsError: Bool = false
    @State private var trialBusy: Bool = false

    @State private var pasteCode: String = ""
    @State private var pasteLabel: String = ""
    @State private var pasteStatus: String? = nil
    @State private var pasteIsError: Bool = false

    @State private var serverUrl: String = "http://127.0.0.1:8787"

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            statusHero
            activationGroup
            capabilityGroup
            devicesGroup
            fallbackGroup
            advancedGroup
        }
        .onAppear {
            refreshStatus()
            Task { await refreshDevices() }
        }
        .onReceive(NotificationCenter.default.publisher(for: .dimmyLicenseChanged)) { _ in
            refreshStatus()
            Task { await refreshDevices() }
        }
    }

    // MARK: Status hero

    private var statusHero: some View {
        Group {
            HStack(alignment: .center, spacing: 14) {
                tierBadge
                VStack(alignment: .leading, spacing: 2) {
                    Text(statusHeadline)
                        .font(.system(size: 18, weight: .semibold))
                        .foregroundStyle(.primary)
                    Text(statusDetail)
                        .font(.system(size: 12))
                        .foregroundStyle(Color.macTextSecondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 12)
                if let trailing = statusTrailing {
                    VStack(alignment: .trailing, spacing: 2) {
                        Text(trailing.value)
                            .font(.system(size: 22, weight: .semibold, design: .rounded))
                            .foregroundStyle(statusTint)
                            .monospacedDigit()
                        Text(trailing.label)
                            .font(.system(size: 11, weight: .medium))
                            .foregroundStyle(Color.macTextSecondary)
                            .textCase(.uppercase)
                            .tracking(0.5)
                    }
                }
            }
            .padding(EdgeInsets(top: 18, leading: 18, bottom: 18, trailing: 18))
            .background(
                RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                    .fill(statusTint.opacity(0.08))
            )
            .overlay(
                RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                    .stroke(statusTint.opacity(0.35), lineWidth: 0.8)
            )
            .padding(.bottom, 10)

            HStack(spacing: 8) {
                Button("Refresh now") {
                    Task {
                        _ = await DimmyCore.shared.licenseRefresh()
                        refreshStatus()
                    }
                }
                Button("Sign out / clear") {
                    DimmyCore.shared.licenseClear()
                    refreshStatus()
                }
                Spacer()
            }
            .padding(.bottom, 16)
        }
    }

    private var tierBadge: some View {
        Text(tierBadgeText)
            .font(.system(size: 11, weight: .bold, design: .rounded))
            .tracking(0.8)
            .foregroundStyle(.white)
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .background(
                Capsule(style: .continuous).fill(statusTint)
            )
            .overlay(
                Capsule(style: .continuous)
                    .stroke(Color.white.opacity(0.15), lineWidth: 0.5)
            )
            .shadow(color: statusTint.opacity(0.35), radius: 3, x: 0, y: 1)
    }

    private var tierBadgeText: String {
        switch status.kind {
        case "Unrestricted":  return "DEV"
        case "TrialActive", "TrialExpired": return "TRIAL"
        case "Active":
            switch status.tier {
            case "annual": return "PRO • ANNUAL"
            case "3year":  return "PRO • 3-YEAR"
            default:       return "PRO"
            }
        case "Expired":   return "EXPIRED"
        case "Suspended": return "SUSPENDED"
        case "Invalid":   return "INVALID"
        default:          return "INACTIVE"
        }
    }

    private var statusTint: Color {
        switch status.kind {
        case "TrialActive":   return Color(red: 1.00, green: 0.62, blue: 0.04)   // orange
        case "Active":        return Color(red: 0.20, green: 0.74, blue: 0.40)   // green
        case "Unrestricted":  return Color(red: 0.55, green: 0.40, blue: 0.95)   // purple (dev)
        case "TrialExpired", "Expired", "Invalid":
            return Color(red: 0.92, green: 0.30, blue: 0.30)                     // red
        case "Suspended":     return Color(red: 0.95, green: 0.65, blue: 0.10)   // amber
        default:              return Color.secondary                             // gray (NotFound)
        }
    }

    private var statusTrailing: (value: String, label: String)? {
        switch status.kind {
        case "TrialActive":
            let d = status.daysRemaining ?? 0
            return ("\(d)", d == 1 ? "day left" : "days left")
        case "Active":
            let d = status.daysRemaining ?? 0
            return ("\(d)", d == 1 ? "day left" : "days left")
        case "Suspended":
            let d = status.daysOffline ?? 0
            return ("\(d)", d == 1 ? "day offline" : "days offline")
        default:
            return nil
        }
    }

    private var statusHeadline: String {
        switch status.kind {
        case "Unrestricted":  return "Source build — licensing disabled"
        case "NotFound":      return "No license on this device"
        case "TrialActive":   return "Trial active"
        case "TrialExpired":  return "Trial ended"
        case "Active":
            switch status.tier {
            case "annual": return "Pro license — Annual"
            case "3year":  return "Pro license — 3-year"
            default:       return "Pro license"
            }
        case "Expired":   return "License expired"
        case "Suspended": return "License suspended"
        case "Invalid":   return "License file invalid"
        default:          return status.kind
        }
    }

    private var statusDetail: String {
        switch status.kind {
        case "Unrestricted":
            return "This binary was built without a licensing public key. All features are unlocked."
        case "NotFound":
            return "Activate Dimmy with your email, or paste an activation code from email."
        case "TrialActive":
            return "Your free 14-day trial is running. Cloud STT/LLM and auto-update are enabled."
        case "TrialExpired":
            return "Your trial has ended. Cloud features are paused. Purchase a license to continue."
        case "Active":
            return "Thanks for supporting Dimmy. All cloud features are enabled."
        case "Expired":
            return "Renew to re-enable cloud features."
        case "Suspended":
            return "Reconnect this device online to refresh your license."
        case "Invalid":
            return status.error ?? "Re-activate this device."
        default:
            return status.error ?? ""
        }
    }

    // MARK: Activate

    private var activationGroup: some View {
        Group {
            MacGroupLabel(text: "Activate Dimmy")
            MacTile {
                MacRow(
                    "Email",
                    description: "We'll send a magic link. Clicking it opens Dimmy and activates the license. Same email on another device joins your existing license."
                ) {
                    HStack(spacing: 8) {
                        TextField("you@example.com", text: $trialEmail)
                            .textFieldStyle(.roundedBorder)
                            .frame(width: 220)
                        Button(trialBusy ? "Sending…" : "Activate") {
                            Task { await sendMagicLink() }
                        }
                        .keyboardShortcut(.defaultAction)
                        .disabled(trialBusy || !looksLikeEmail(trialEmail))
                    }
                }
                if let msg = trialStatus {
                    MacRow(msg, description: "", showsDivider: false) {
                        Image(systemName: trialIsError ? "exclamationmark.octagon.fill" : "checkmark.circle.fill")
                            .foregroundStyle(trialIsError ? .red : .green)
                    }
                }
            }
        }
    }

    // MARK: Capabilities

    private var capabilityGroup: some View {
        Group {
            MacGroupLabel(text: "Included with your license")
            MacTile {
                ForEach(Array(DimmyCore.LicenseScope.allCases.enumerated()), id: \.element) { idx, scope in
                    let granted = status.scopes.contains(scope.rawValue)
                    let isLast = idx == DimmyCore.LicenseScope.allCases.count - 1
                    MacRow(scope.display, description: scope.subtitle, showsDivider: !isLast) {
                        HStack(spacing: 6) {
                            Image(systemName: granted ? "checkmark.circle.fill" : "xmark.circle")
                                .foregroundStyle(granted ? .green : .secondary)
                            Text(granted ? "Included" : "Not included")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            }
        }
    }

    // MARK: Devices

    private var devicesGroup: some View {
        Group {
            HStack {
                MacGroupLabel(text: "Devices")
                Spacer()
                if let err = devicesError {
                    Text(err).font(.caption).foregroundStyle(.red)
                } else if maxDevices > 0 {
                    let active = devices.filter { $0.status == "active" }.count
                    Text("\(active) active / \(maxDevices) max").font(.caption).foregroundStyle(.secondary)
                }
                Button("Reload") { Task { await refreshDevices() } }
                    .buttonStyle(.borderless)
            }
            .padding(.bottom, 4)
            if devices.isEmpty {
                MacTile {
                    MacRow("No devices", description: "Activate your license to see your devices here.", showsDivider: false) { EmptyView() }
                }
            } else {
                MacTile {
                    ForEach(Array(devices.enumerated()), id: \.element.id) { idx, d in
                        let isLast = idx == devices.count - 1
                        let title = d.label.isEmpty ? "(unnamed device)" : d.label + (d.isSelf ? " · this device" : "")
                        let subtitle = d.status == "active"
                            ? "Last seen: \(formatDate(d.lastSeen))"
                            : "Status: \(d.status)"
                        MacRow(title, description: subtitle, showsDivider: !isLast) {
                            Button(d.isSelf ? "Sign out" : "Sign out") {
                                Task { await deactivate(d) }
                            }
                            .disabled(d.status != "active")
                        }
                    }
                }
            }
        }
    }

    // MARK: Fallback paste

    private var fallbackGroup: some View {
        Group {
            DisclosureGroup("Activation didn't open Dimmy? Paste the code") {
                MacTile {
                    MacRow(
                        "Activation code",
                        description: "Paste the bare 32-char code or the full magic-link URL from your email."
                    ) {
                        TextField("32-char code or magic-link URL", text: $pasteCode)
                            .textFieldStyle(.roundedBorder)
                            .frame(width: 320)
                    }
                    MacRow("Device label (optional)", description: "") {
                        TextField("e.g. Konrad's MacBook", text: $pasteLabel)
                            .textFieldStyle(.roundedBorder)
                            .frame(width: 320)
                    }
                    MacRow("", description: "", showsDivider: false) {
                        Button("Activate from code") {
                            Task { await activateFromCode() }
                        }
                        .disabled(pasteCode.trimmingCharacters(in: .whitespaces).isEmpty)
                    }
                    if let msg = pasteStatus {
                        MacRow(msg, description: "", showsDivider: false) {
                            Image(systemName: pasteIsError ? "exclamationmark.octagon.fill" : "checkmark.circle.fill")
                                .foregroundStyle(pasteIsError ? .red : .green)
                        }
                    }
                }
            }
            .padding(.vertical, 4)
        }
    }

    // MARK: Advanced (server URL)

    private var advancedGroup: some View {
        DisclosureGroup("Advanced — server URL") {
            MacTile {
                MacRow("Licensing server",
                       description: "Override the licensing endpoint. Default points at the local Node mock.",
                       showsDivider: false) {
                    HStack(spacing: 8) {
                        TextField("http://127.0.0.1:8787", text: $serverUrl)
                            .textFieldStyle(.roundedBorder)
                            .frame(width: 280)
                        Button("Apply") {
                            DimmyCore.shared.licenseSetServerUrl(serverUrl)
                        }
                    }
                }
            }
        }
        .padding(.vertical, 4)
    }

    // MARK: Actions

    private func refreshStatus() {
        status = DimmyCore.shared.licenseStatus()
    }

    private func refreshDevices() async {
        let res = await DimmyCore.shared.licenseDevicesList()
        await MainActor.run {
            if !res.ok {
                devicesError = res.error
                devices = []
                maxDevices = 0
            } else {
                devicesError = nil
                devices = res.devices
                maxDevices = res.maxDevices
            }
        }
    }

    private func sendMagicLink() async {
        let email = trialEmail.trimmingCharacters(in: .whitespacesAndNewlines)
        guard looksLikeEmail(email) else {
            trialIsError = true
            trialStatus = "Enter a valid email address."
            return
        }
        trialBusy = true
        defer { trialBusy = false }
        trialStatus = "Requesting magic link…"
        trialIsError = false
        let r = await DimmyCore.shared.licenseRequestTrial(email: email)
        if !r.ok {
            trialIsError = true
            trialStatus = r.error ?? "Request failed."
            return
        }
        guard let link = r.magicLink else {
            trialStatus = "Magic link sent. Check your inbox."
            return
        }
        if link.hasPrefix("dimmy://") {
            trialStatus = "Activating via magic link…"
            if let url = URL(string: link) { NSWorkspace.shared.open(url) }
            // Poll status briefly — URL scheme dispatch redeems async.
            for _ in 0..<20 {
                try? await Task.sleep(nanoseconds: 400_000_000)
                let s = DimmyCore.shared.licenseStatus()
                if s.kind == "TrialActive" || s.kind == "Active" {
                    refreshStatus()
                    await refreshDevices()
                    trialIsError = false
                    trialStatus = "Activated. Welcome to Dimmy."
                    return
                }
            }
            trialIsError = true
            trialStatus = "Auto-activation didn't complete. Use the fallback below: \(r.code ?? extractCode(link) ?? "")"
        } else {
            trialIsError = false
            trialStatus = "Magic link sent to \(email). Click it from this device to activate."
        }
    }

    private func activateFromCode() async {
        let raw = pasteCode.trimmingCharacters(in: .whitespacesAndNewlines)
        let code = extractCode(raw) ?? raw
        let label = pasteLabel.trimmingCharacters(in: .whitespacesAndNewlines)
        let r = await DimmyCore.shared.licenseRedeem(
            code: code,
            deviceLabel: label.isEmpty ? Host.current().localizedName ?? "Mac" : label)
        await MainActor.run {
            if r.ok {
                pasteIsError = false
                pasteStatus = "Activated. Welcome to Dimmy."
                refreshStatus()
            } else {
                pasteIsError = true
                pasteStatus = r.error ?? "Activation failed."
            }
        }
        if r.ok { await refreshDevices() }
    }

    private func deactivate(_ d: DimmyCore.LicenseDeviceInfo) async {
        let r = await DimmyCore.shared.licenseDeactivateDevice(deviceId: d.isSelf ? nil : d.deviceId)
        if r.ok {
            refreshStatus()
            await refreshDevices()
        } else {
            await MainActor.run { devicesError = r.error }
        }
    }

    // MARK: Helpers

    private func looksLikeEmail(_ s: String) -> Bool {
        let trimmed = s.trimmingCharacters(in: .whitespacesAndNewlines)
        return !trimmed.isEmpty && trimmed.contains("@") && trimmed.contains(".")
    }

    private func extractCode(_ s: String) -> String? {
        guard let range = s.range(of: "code=", options: .caseInsensitive) else { return nil }
        let rest = s[range.upperBound...]
        if let amp = rest.firstIndex(of: "&") {
            return String(rest[..<amp])
        }
        return String(rest)
    }

    private func formatDate(_ epoch: Int64) -> String {
        guard epoch > 0 else { return "—" }
        let d = Date(timeIntervalSince1970: TimeInterval(epoch))
        let fmt = DateFormatter()
        fmt.dateStyle = .short
        fmt.timeStyle = .short
        return fmt.string(from: d)
    }
}
