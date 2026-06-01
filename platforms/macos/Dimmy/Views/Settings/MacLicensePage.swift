import SwiftUI
import AppKit

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

    @State private var manageBusy: Bool = false
    @State private var manageError: String? = nil

    // Production licensing endpoint. Custom domain attached to the
    // dimmy-licensing Worker via wrangler.toml [[routes]]. Single
    // source of truth shared with the C# Win counterpart in
    // SettingsWindow.xaml.cs::License_ManageSubscription_Click.
    private let licensingServerURL = "https://license.dimmy.app"

    @State private var buyBusy: Bool = false
    @State private var buyStatus: String? = nil
    @State private var buyIsError: Bool = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            statusHero
            devicesGroup
            buyGroup
            capabilityGroup
            activationGroup
            fallbackGroup
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
                            .foregroundStyle(.primary)
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
                    .fill(statusTint.opacity(0.05))
            )
            .overlay(
                RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                    .stroke(Color.primary.opacity(MacTheme.hairlineOpacity), lineWidth: 0.8)
            )
            .padding(.bottom, 10)

            HStack(spacing: 8) {
                if isManageableTier {
                    Button(manageBusy ? "Opening…" : "Manage subscription") {
                        Task { await openBillingPortal() }
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(manageBusy)
                }
                Button("Refresh now") {
                    Task {
                        _ = await DimmyCore.shared.licenseRefresh()
                        refreshStatus()
                    }
                }
                Spacer()
                Button("Sign out (this device)") {
                    let alert = NSAlert()
                    alert.messageText = "Sign out from this device?"
                    alert.informativeText =
                        "Your subscription on Stripe will stay active and will keep " +
                        "billing on its renewal date. To cancel billing, use 'Manage subscription' instead.\n\n" +
                        "If you sign out, your activation token on this device is removed. " +
                        "You can sign in again from the same email — we'll resend the magic link."
                    alert.alertStyle = .warning
                    alert.addButton(withTitle: "Sign out")
                    alert.addButton(withTitle: "Cancel")
                    if alert.runModal() == .alertFirstButtonReturn {
                        DimmyCore.shared.licenseClear()
                        refreshStatus()
                    }
                }
                .tint(.red)
            }
            .padding(.bottom, 4)
            if let err = manageError {
                Text(err)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .padding(.bottom, 8)
            }
            Spacer().frame(height: 8)
        }
    }

    private var tierBadge: some View {
        // Win11 Fluent badge style: subtle tinted fill (~14%), matching
        // 35% tinted stroke, tint-colored text. Same recipe as
        // SettingsWindow.xaml.cs::ApplyLicenseHero so both platforms
        // read at identical visual weight — no saturated solid pills.
        Text(tierBadgeText)
            .font(.system(size: 11, weight: .semibold, design: .rounded))
            .tracking(0.6)
            .foregroundStyle(statusTint)
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(
                RoundedRectangle(cornerRadius: 4, style: .continuous)
                    .fill(statusTint.opacity(0.14))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 4, style: .continuous)
                    .stroke(statusTint.opacity(0.35), lineWidth: 0.8)
            )
    }

    private var tierBadgeText: String {
        switch status.kind {
        case "Unrestricted":  return "DEV"
        case "TrialActive", "TrialExpired": return "TRIAL"
        case "Active":
            switch status.tier {
            case "monthly":  return "PRO • MONTHLY"
            case "annual":   return "PRO • ANNUAL"
            case "lifetime": return "PRO • LIFETIME"
            default:         return "PRO"
            }
        case "Expired":   return "EXPIRED"
        case "Suspended": return "SUSPENDED"
        case "Invalid":   return "INVALID"
        default:          return "INACTIVE"
        }
    }

    private var statusTint: Color {
        // Color is reserved for *positive* state — TrialActive (orange,
        // "act soon") and Active (green, "you're paid"). Everything else
        // (NotFound, TrialExpired, Expired, Invalid, Suspended) shows as
        // neutral gray: a license problem isn't an error to alarm about,
        // it's just a state. Red was reading as "something is broken" —
        // exactly the wrong vibe for a user who simply hasn't activated.
        switch status.kind {
        case "TrialActive":   return Color(red: 1.00, green: 0.62, blue: 0.04)   // orange
        case "Active":        return Color(red: 0.20, green: 0.74, blue: 0.40)   // green
        case "Unrestricted":  return Color(red: 0.55, green: 0.40, blue: 0.95)   // purple (dev)
        default:              return Color.secondary                             // gray (everything else)
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
            case "monthly":  return "Pro license — Monthly"
            case "annual":   return "Pro license — Annual"
            case "lifetime": return "Pro license — Lifetime (3y)"
            default:         return "Pro license"
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
            if let ca = status.cancelsAt {
                let fmt = DateFormatter()
                fmt.dateStyle = .medium
                fmt.timeStyle = .none
                let date = Date(timeIntervalSince1970: TimeInterval(ca))
                return "Subscription scheduled to cancel on \(fmt.string(from: date)). You keep cloud features until then."
            }
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

    // MARK: Buy / Upgrade — three-tier picker shown for NotFound,
    // TrialActive, TrialExpired, Expired. Hidden when Active (the user
    // already has a license — they go to "Manage subscription") or in
    // dev/Suspended/Invalid states.

    @ViewBuilder
    private var buyGroup: some View {
        if buyVisible {
            MacGroupLabel(text: buyHeadline)
            MacTile {
                MacRow(
                    buyDetail,
                    description: "Stripe handles payment.",
                    hint: "Stripe processes payment + tax. The moment payment clears we email you a magic link that activates this Mac in one click — no manual code copy.",
                    showsDivider: false
                ) {
                    HStack(spacing: 8) {
                        if showMonthlyButton {
                            Button { Task { await buy(tier: "monthly") } } label: {
                                VStack(spacing: 1) {
                                    Text(monthlyLabel).bold()
                                    Text("recurring").font(.system(size: 9))
                                        .foregroundStyle(Color.macTextSecondary)
                                }.frame(minWidth: 64)
                            }
                            .disabled(buyBusy)
                        }
                        if showAnnualButton {
                            Button { Task { await buy(tier: "annual") } } label: {
                                VStack(spacing: 1) {
                                    Text(annualLabel).bold()
                                    Text("best value").font(.system(size: 9))
                                        .foregroundStyle(.white.opacity(0.85))
                                }.frame(minWidth: 64)
                            }
                            .buttonStyle(.borderedProminent)
                            .keyboardShortcut(.defaultAction)
                            .disabled(buyBusy)
                        }
                        if showLifetimeButton {
                            Button { Task { await buy(tier: "lifetime") } } label: {
                                VStack(spacing: 1) {
                                    Text(lifetimeLabel).bold()
                                    Text("one-time").font(.system(size: 9))
                                        .foregroundStyle(Color.macTextSecondary)
                                }.frame(minWidth: 64)
                            }
                            .disabled(buyBusy)
                        }
                    }
                }
                if showPortalHint {
                    MacRow("Need to downgrade, cancel, or update payment? Use the “Manage subscription” button above — it opens the secure Stripe billing portal.",
                           description: "",
                           showsDivider: false) {
                        EmptyView()
                    }
                }
                if let msg = buyStatus {
                    MacRow(msg, description: "", showsDivider: false) {
                        Image(systemName: buyIsError ? "exclamationmark.octagon.fill" : "checkmark.circle.fill")
                            .foregroundStyle(buyIsError ? .red : .green)
                    }
                }
            }
        }
    }

    /// Tier-aware visibility — see Win counterpart in
    /// SettingsWindow.xaml.cs::ApplyBuyCardForStatus for the matrix.
    /// Active{lifetime} hides everything (lifetime is the ceiling).
    /// Active{monthly} hides Monthly. Active{annual} hides Monthly + Annual.
    /// Trial / NotFound / TrialExpired / Expired / Suspended → all three.
    private var buyVisible: Bool {
        switch status.kind {
        case "NotFound", "TrialActive", "TrialExpired", "Expired", "Suspended":
            return true
        case "Active":
            switch status.tier?.lowercased() {
            case "monthly", "annual": return true
            default: return false // lifetime + unknown
            }
        default:
            return false
        }
    }

    private var showMonthlyButton: Bool {
        // Hidden when already on Monthly (no-op) and on Lifetime
        // (Lifetime → Monthly downgrade is not a sensible flow). Visible
        // on Active{annual} as 'Switch to Monthly' for in-app downgrade
        // (matches Win behaviour); also visible on all non-Active states
        // (NotFound, Trial, Expired, Suspended) as a fresh purchase.
        if status.kind == "Active" {
            let t = status.tier?.lowercased()
            return !(t == "monthly" || t == "lifetime")
        }
        return true
    }

    private var showAnnualButton: Bool {
        // Hidden only when already on Annual.
        !(status.kind == "Active" && (status.tier?.lowercased() == "annual"))
    }

    private var showLifetimeButton: Bool {
        // Hidden only when already on Lifetime (which buyVisible already
        // catches by hiding the whole group, but defensive).
        !(status.kind == "Active" && (status.tier?.lowercased() == "lifetime"))
    }

    private var showPortalHint: Bool {
        // Visible only on Active monthly/annual where downgrade/cancel
        // path lives in the Stripe Customer Portal.
        status.kind == "Active"
            && (status.tier?.lowercased() == "monthly"
                || status.tier?.lowercased() == "annual")
    }

    private var monthlyLabel: String {
        status.kind == "Active" && status.tier?.lowercased() == "annual"
            ? "Switch to Monthly" : "Monthly"
    }
    private var annualLabel: String {
        status.kind == "Active" && status.tier?.lowercased() == "monthly"
            ? "Switch to Annual" : "Annual"
    }
    private var lifetimeLabel: String {
        status.kind == "Active"
            && (status.tier?.lowercased() == "monthly" || status.tier?.lowercased() == "annual")
            ? "Upgrade to Lifetime" : "Lifetime"
    }

    private var buyHeadline: String {
        switch status.kind {
        case "NotFound":     return "Buy a license"
        case "TrialActive":  return "Upgrade to Pro"
        case "TrialExpired": return "Trial ended — buy to continue"
        case "Expired":      return "Renew your license"
        case "Suspended":    return "Resume your license"
        case "Active":
            switch status.tier?.lowercased() {
            case "monthly": return "Upgrade your plan"
            case "annual":  return "Change your plan"
            default:        return "Buy a license"
            }
        default:             return "Buy a license"
        }
    }

    private var buyDetail: String {
        switch status.kind {
        case "TrialActive":
            return "Skip the trial and unlock cloud features without interruption."
        case "TrialExpired", "Expired":
            return "Cloud features are paused. Pick a plan to re-activate."
        case "Suspended":
            return "Pick a plan to restore cloud features."
        case "Active":
            switch status.tier?.lowercased() {
            case "monthly":
                return "Switch to Annual for the best value, or Lifetime to skip renewals entirely."
            case "annual":
                return "Drop to Monthly (you'll get a credit on the next invoice) or jump to Lifetime for one final payment."
            default:
                return "Pick a plan and we'll email you a magic link to activate."
            }
        default:
            return "Pick a plan and we'll email you a magic link to activate."
        }
    }

    private func buy(tier: String) async {
        buyBusy = true
        defer { buyBusy = false }
        buyIsError = false

        // monthly⇄annual on an Active sub is a Stripe subscription update
        // (proration, no second charge for the period), NOT a new checkout.
        // Sending users through Checkout for a tier-switch was billing them
        // again while leaving the old sub running — see PR description.
        let currentTier = status.tier?.lowercased()
        let isPlanChange = status.kind == "Active"
            && (currentTier == "monthly" || currentTier == "annual")
            && (tier == "monthly" || tier == "annual")

        if isPlanChange {
            // Confirm dialog so the user knows the click mutates an
            // existing sub (proration on next invoice, no second card
            // prompt) rather than opening a fresh Checkout. Without
            // it the click 'flagga istantaneamente' the new tier and
            // the silent UX feels off — same dialog also on Win.
            let confirmed = await MainActor.run { () -> Bool in
                let alert = NSAlert()
                alert.messageText = "Switch plan to \(tier.capitalized)?"
                alert.informativeText =
                    "You're already subscribed (current: \(currentTier?.capitalized ?? "")).\n\n" +
                    "Switching to \(tier.capitalized) mutates your existing subscription:\n\n" +
                    "• No new payment now — Stripe reuses your saved card.\n" +
                    "• Stripe issues a prorated invoice on the next billing date " +
                    "(credit for unused days of the old plan, debit for the new one).\n" +
                    "• No magic-link email — your license stays active, just the tier changes."
                alert.alertStyle = .informational
                alert.addButton(withTitle: "Switch to \(tier.capitalized)")
                alert.addButton(withTitle: "Cancel")
                return alert.runModal() == .alertFirstButtonReturn
            }
            guard confirmed else {
                await MainActor.run {
                    buyIsError = false
                    buyStatus = "Plan change cancelled."
                }
                return
            }
            buyStatus = "Switching plan to \(tier) (proration applies)…"
            let pc = await DimmyCore.shared.licensePlanChange(newTier: tier)
            guard pc.ok else {
                buyIsError = true
                buyStatus = pc.error ?? "Plan change failed."
                return
            }
            // Webhook customer.subscription.updated lands ~instantly; give
            // the server a beat, then refresh the local token so the UI
            // reflects the new tier.
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            _ = await DimmyCore.shared.licenseRefresh()
            await MainActor.run {
                refreshStatus()
                buyIsError = false
                buyStatus = "Plan switched to \(tier). Stripe will prorate the difference on the next invoice."
            }
            return
        }

        // Pre-checkout email gate: ask the user for their email before
        // minting the Stripe Checkout URL. The server uses the email
        // to look up an existing license and 409 BEFORE Stripe charges.
        // Pre-fill with the previously-entered email (persisted in
        // AppState; survives Sign out — UX convenience, no auth weight).
        let prefilled = appState.buyerEmail ?? ""
        let promptedEmail: String? = await MainActor.run { () -> String? in
            let alert = NSAlert()
            alert.messageText = "Continue to \(tier.capitalized) checkout"
            alert.informativeText =
                "Used to look up an existing license + match the Stripe customer. " +
                "If you've bought before with this email, we'll resend the magic link " +
                "instead of charging you again."
            alert.alertStyle = .informational
            alert.addButton(withTitle: "Continue")
            alert.addButton(withTitle: "Cancel")
            let input = NSTextField(frame: NSRect(x: 0, y: 0, width: 280, height: 24))
            input.placeholderString = "you@example.com"
            input.stringValue = prefilled
            alert.accessoryView = input
            // Force initial focus on the text field.
            alert.window.initialFirstResponder = input
            let res = alert.runModal()
            if res != .alertFirstButtonReturn { return nil }
            let trimmed = input.stringValue.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            return trimmed.contains("@") && trimmed.contains(".") && trimmed.count < 254 ? trimmed : nil
        }
        guard let email = promptedEmail else {
            await MainActor.run {
                buyIsError = false
                buyStatus = "Purchase cancelled."
            }
            return
        }
        // Persist for next time.
        await MainActor.run { appState.buyerEmail = email }

        buyStatus = "Opening Stripe checkout for \(tier)…"
        let r = await DimmyCore.shared.licenseCheckoutUrl(tier: tier, email: email)
        if !r.ok {
            // 409 path → license already exists. Offer "send magic link"
            // fallback (re-issue activation email instead of charging).
            if r.statusCode == 409, let curTier = r.currentTier {
                let confirmed: Bool = await MainActor.run {
                    let alert = NSAlert()
                    alert.messageText = "You already have a \(curTier) license"
                    alert.informativeText =
                        "The email \(email) is already linked to an active \(curTier) license. " +
                        "We can resend the activation magic link for that license — " +
                        "no new payment, no second sub."
                    alert.alertStyle = .informational
                    alert.addButton(withTitle: "Send magic link")
                    alert.addButton(withTitle: "Cancel")
                    return alert.runModal() == .alertFirstButtonReturn
                }
                if confirmed {
                    let t = await DimmyCore.shared.licenseRequestTrial(email: email)
                    await MainActor.run {
                        buyIsError = !t.ok
                        buyStatus = t.ok
                            ? "Magic link sent to \(email). Check your inbox."
                            : (t.error ?? "Could not resend magic link.")
                    }
                } else {
                    await MainActor.run {
                        buyIsError = false
                        buyStatus = "Cancelled."
                    }
                }
                return
            }
            buyIsError = true
            buyStatus = r.error ?? "Could not start checkout."
            return
        }
        guard let urlStr = r.url, let url = URL(string: urlStr) else {
            buyIsError = true
            buyStatus = "Stripe returned an empty URL."
            return
        }
        NSWorkspace.shared.open(url)
        buyIsError = false
        buyStatus = "Checkout opened. After payment, check your email for the magic link."
    }

    // MARK: Activate

    private var activationGroup: some View {
        Group {
            MacGroupLabel(text: "Activate Dimmy")
            MacTile {
                MacRow(
                    "Email",
                    description: "We'll send a magic link.",
                    hint: "Clicking the magic link opens Dimmy and activates the license on this Mac. Using the same email on another device joins it to the same license (subject to the device cap)."
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
                        description: "32-char code or full magic-link URL.",
                        hint: "Use this when the magic link from your email doesn't open Dimmy automatically — happens on machines where dimmy:// isn't registered as a URL handler. Paste either the bare code or the whole URL; we extract the code."
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

    // MARK: Actions

    private func refreshStatus() {
        status = DimmyCore.shared.licenseStatus()
    }

    // MARK: Manage subscription (Stripe Customer Portal)

    /// Show the "Manage subscription" button only for paid tiers that
    /// have a Stripe billing relationship attached. Trials don't, and
    /// Unrestricted (source build) doesn't. Lifetime DOES — even if it
    /// can't be cancelled, the portal lets the user view the invoice
    /// and update payment method for future purchases.
    private var isManageableTier: Bool {
        guard status.kind == "Active" else { return false }
        switch status.tier {
        case "monthly", "annual", "lifetime": return true
        default: return false
        }
    }

    /// Open Stripe Customer Portal via the licensing FFI. Goes through
    /// the same server URL the rest of the FFI uses (default
    /// localhost:8787 in dev, license.dimmy.app in prod) — no hardcoded
    /// URL, no manual file I/O for the token. The portal session is
    /// single-tab + ~5 min lifetime; we never store the URL.
    private func openBillingPortal() async {
        await MainActor.run {
            manageBusy = true
            manageError = nil
        }
        defer {
            Task { @MainActor in manageBusy = false }
        }
        let r = await DimmyCore.shared.licenseBillingPortalUrl()
        guard r.ok, let urlStr = r.url, let url = URL(string: urlStr) else {
            await MainActor.run {
                manageError = r.error ?? "Cannot open portal."
            }
            return
        }
        NSWorkspace.shared.open(url)
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
        // The dimmy:// URL handler in AppDelegate may have consumed this
        // same code first (when the user opened the magic link in a browser
        // AND pasted the code here). The server then 409s the second redeem
        // with "code already consumed". If the local file is now active,
        // treat it as success — the device is licensed regardless of which
        // call won the race.
        let postStatus = DimmyCore.shared.licenseStatus()
        let alreadyActive = postStatus.kind == "TrialActive" || postStatus.kind == "Active"
        await MainActor.run {
            if r.ok || alreadyActive {
                pasteIsError = false
                pasteStatus = r.ok
                    ? "Activated. Welcome to Dimmy."
                    : "Already activated on this device."
                status = postStatus
            } else {
                pasteIsError = true
                pasteStatus = r.error ?? "Activation failed."
            }
        }
        if r.ok || alreadyActive { await refreshDevices() }
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
