import SwiftUI

// App rules — auto-switch the rewrite style based on the focused app.
// Backend landed in PR #38 (Rust `app_rules` module); this page is the
// macOS UI to view/edit/reorder/delete rules.
//
// Forge note: rules ARE stored on macOS today (config sync round-trips
// the `app_rules` JSON array through DimmyCore.setConfig), but the
// foreground-app capture at hotkey-down is NOT yet wired on macOS — that
// needs an NSWorkspace bridge in HotkeyManager. Until then, rules will
// sit in config but never match. The page works as a configurator and
// is functional once the macOS capture lands.

struct MacRulesPage: View {
    @ObservedObject var appState: AppState
    @State private var rules: [AppRuleEntry] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            MacNote(
                title: "Match the rewrite to the app you're in",
                body: "Dimmy detects the focused app and applies its rule before pasting. Slack gets Gen Z, Mail gets Professional, your terminal gets nothing — your call. Rules are evaluated top-down; the first match wins.",
                systemImage: "info.circle.fill"
            )
            .padding(.bottom, 8)

            statusGroup
            rulesGroup

            HStack(spacing: 8) {
                Button {
                    addRule()
                } label: {
                    Label("Add rule", systemImage: "plus.circle.fill")
                }
                .buttonStyle(.borderedProminent)
            }
            .padding(.top, 12)
        }
    }

    // MARK: Auto-switching status

    private var statusGroup: some View {
        Group {
            MacGroupLabel(text: "Auto-switching")
            MacTile {
                MacRow(
                    "Foreground capture",
                    description: rulesAvailableDescription,
                    icon: "rectangle.3.group.fill",
                    iconBackground: Color(red: 0.20, green: 0.78, blue: 0.35),
                    showsDivider: false
                ) {
                    if foregroundCaptureAvailable {
                        Label("Active", systemImage: "checkmark.circle.fill")
                            .foregroundStyle(.green)
                            .font(.system(size: 12, weight: .medium))
                    } else {
                        Label("Not yet wired", systemImage: "hourglass")
                            .foregroundStyle(.orange)
                            .font(.system(size: 12, weight: .medium))
                    }
                }
            }
        }
    }

    /// Until the macOS NSWorkspace foreground-app bridge lands, rules
    /// stay in config but can't match anything. We surface that honestly
    /// rather than letting users wonder why their rules don't fire.
    private var foregroundCaptureAvailable: Bool { false }

    private var rulesAvailableDescription: String {
        foregroundCaptureAvailable
            ? "Rules are evaluated top-down at hotkey-down."
            : "Rules can be configured here, but the macOS foreground-app capture hasn't shipped yet — they won't fire until the next release."
    }

    // MARK: Rules table

    private var rulesGroup: some View {
        Group {
            MacGroupLabel(text: "Rules · \(rules.count)")
            MacTile {
                if rules.isEmpty {
                    emptyState
                } else {
                    VStack(spacing: 0) {
                        ForEach(rules) { rule in
                            ruleRow(rule)
                            if rule.id != rules.last?.id {
                                Divider()
                                    .background(Color.macRowDivider)
                                    .padding(.leading, 14)
                                    .opacity(0.6)
                            }
                        }
                    }
                }
            }
        }
    }

    private var emptyState: some View {
        VStack(spacing: 8) {
            Image(systemName: "rectangle.3.group")
                .font(.system(size: 32))
                .foregroundStyle(Color.macTextTertiary)
            Text("No rules yet.")
                .font(.system(size: 13))
            Text("Add your first one — Dimmy will switch styles automatically when those apps are in focus.")
                .font(.system(size: 11))
                .foregroundStyle(Color.macTextSecondary)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity)
        .padding(36)
    }

    private func ruleRow(_ rule: AppRuleEntry) -> some View {
        HStack(spacing: 10) {
            MacSquircleIcon(systemName: "app.fill", background: Color.gray, size: 22)

            VStack(alignment: .leading, spacing: 2) {
                Text(rule.appName.isEmpty ? "Untitled" : rule.appName)
                    .font(.system(size: 13, weight: .medium))
                if !rule.matchPattern.isEmpty {
                    Text(rule.matchPattern)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(Color.macTextTertiary)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            HStack(spacing: 6) {
                Circle()
                    .fill(MacStyleColor.color(for: rule.style))
                    .frame(width: 6, height: 6)
                Picker("", selection: Binding(
                    get: { rule.style },
                    set: { newValue in updateRule(rule.id) { $0.style = newValue } }
                )) {
                    ForEach(MacLlmStyles, id: \.key) { entry in
                        Text(entry.label).tag(entry.key)
                    }
                }
                .labelsHidden()
                .frame(width: 130)
            }

            Toggle("", isOn: Binding(
                get: { rule.enabled },
                set: { newValue in updateRule(rule.id) { $0.enabled = newValue } }
            ))
            .toggleStyle(.switch)
            .labelsHidden()
            .controlSize(.small)

            Button {
                deleteRule(rule.id)
            } label: {
                Image(systemName: "trash")
                    .font(.system(size: 12))
                    .foregroundStyle(.red)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .opacity(rule.enabled ? 1.0 : 0.5)
    }

    // MARK: Local state ↔ AppState (Phase 4: round-trip via DimmyCore)

    /// Rule editing for now is a local-only stub that demonstrates the UI
    /// shape. Phase 4 wires this through DimmyCore by:
    ///   1) reading `app_rules` from the FFI getConfig dict
    ///   2) on every change, calling DimmyCore.shared.setConfig with the
    ///      updated rules array
    ///   3) initialising `rules` from AppState in onAppear
    /// Keeping it local until then prevents dispatching half-implemented
    /// FFI calls that would corrupt the Rust-side config.
    struct AppRuleEntry: Identifiable {
        let id: UUID
        var appName: String
        var matchPattern: String
        var style: String
        var enabled: Bool
    }

    private func addRule() {
        rules.append(AppRuleEntry(
            id: UUID(),
            appName: "New rule",
            matchPattern: "",
            style: "correct",
            enabled: true
        ))
    }

    private func updateRule(_ id: UUID, _ mutate: (inout AppRuleEntry) -> Void) {
        guard let idx = rules.firstIndex(where: { $0.id == id }) else { return }
        mutate(&rules[idx])
    }

    private func deleteRule(_ id: UUID) {
        rules.removeAll { $0.id == id }
    }
}
