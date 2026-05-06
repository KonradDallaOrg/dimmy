import SwiftUI
import UniformTypeIdentifiers

// App rules — auto-switch the rewrite style based on the focused app.
//
// The Rust core (core/src/app_rules.rs) resolves rules at LLM-enhance
// time using the bundle id captured at hotkey-down. This page lets the
// user view, add, edit, reorder, and delete those rules. State is
// round-tripped through `DimmyCore.setConfig` — no local mutation goes
// unsaved.

struct MacRulesPage: View {
    @ObservedObject var appState: AppState

    /// Suppresses the on-change persistence while the page first
    /// hydrates `rules` from `appState.appRules`. Without this guard,
    /// the initial `onAppear` assignment would round-trip through
    /// setConfig and bump every other UI's saved-pulse animation.
    @State private var hydrating = false

    /// UUID of the rule the user is currently dragging. Read by the
    /// drop delegate to reorder, then cleared on drop or drag-end.
    @State private var draggedRuleId: UUID?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            MacNote(
                title: "Match the rewrite to the app you're in",
                message: "Dimmy detects the focused app and applies its rule before pasting. Slack gets Imbruttito, Outlook gets Professional, Xcode gets nothing — your call. Rules are evaluated top-down; the first match wins.",
                systemImage: "info.circle.fill"
            )
            .padding(.bottom, 8)

            statusGroup
                .padding(.bottom, 8)

            rulesGroup

            footerActions
                .padding(.top, 12)
        }
        .onAppear { reloadFromAppState() }
        .onChange(of: appState.appRules) { _, _ in
            // Another component (e.g. licensing flow) might have
            // refreshed the config — keep our local mirror in sync.
            // Skipping the persist on this path is automatic because we
            // copy without mutating.
            reloadFromAppState()
        }
    }

    private func reloadFromAppState() {
        hydrating = true
        defer { DispatchQueue.main.async { hydrating = false } }
        // copy by value so subsequent edits stay local until persisted
        // back through setConfig.
        // (AppRule is a struct, the array elements are independent.)
    }

    // MARK: - Auto-switching status

    private var statusGroup: some View {
        VStack(alignment: .leading, spacing: 0) {
            MacGroupLabel(text: "Auto-switching")
            MacTile {
                MacRow(
                    "Foreground capture",
                    description: "Active — rules are evaluated top-down at hotkey-down.",
                    icon: "rectangle.3.group.fill",
                    iconBackground: Color(red: 0.20, green: 0.78, blue: 0.35),
                    showsDivider: false
                ) {
                    Label("Active", systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                        .font(.system(size: 12, weight: .medium))
                }
            }
        }
    }

    // MARK: - Rules table

    private var rulesGroup: some View {
        VStack(alignment: .leading, spacing: 0) {
            MacGroupLabel(text: "Rules · \(appState.appRules.count)")
            MacTile {
                if appState.appRules.isEmpty {
                    emptyState
                } else {
                    VStack(spacing: 0) {
                        ForEach(Array(appState.appRules.enumerated()), id: \.element.id) { idx, rule in
                            ruleRow(at: idx, rule: rule)
                            if idx < appState.appRules.count - 1 {
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
            Text("Click \"Load defaults\" to ship with sensible Slack / Teams / Outlook / VS Code rules — or build your own from scratch.")
                .font(.system(size: 11))
                .foregroundStyle(Color.macTextSecondary)
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity)
        .padding(36)
    }

    // MARK: - Row

    @ViewBuilder
    private func ruleRow(at index: Int, rule: AppRule) -> some View {
        HStack(spacing: 10) {
            // Drag handle — three-line glyph the user grabs to reorder.
            // The whole row is a drag source, but the handle gives the
            // affordance + keeps clicks on textfields / pickers from
            // accidentally starting a drag (we only attach .onDrag to
            // the handle's hit-rect via the surrounding HStack below).
            Image(systemName: "line.3.horizontal")
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(Color.macTextTertiary)
                .frame(width: 18, height: 26)
                .contentShape(Rectangle())
                .onDrag {
                    draggedRuleId = rule.id
                    return NSItemProvider(object: rule.id.uuidString as NSString)
                }
                .help("Drag to reorder")

            AppRuleIcon(rule: rule, size: 26)

            VStack(alignment: .leading, spacing: 2) {
                TextField("App label", text: Binding(
                    get: { appState.appRules[safe: index]?.label ?? "" },
                    set: { newValue in updateRule(at: index) { $0.label = newValue } }
                ))
                .textFieldStyle(.plain)
                .font(.system(size: 13, weight: .medium))

                TextField("Bundle ID", text: Binding(
                    get: { appState.appRules[safe: index]?.matchPattern ?? "" },
                    set: { newValue in updateRule(at: index) { $0.matchPattern = newValue } }
                ))
                .textFieldStyle(.plain)
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(Color.macTextTertiary)
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            HStack(spacing: 6) {
                Circle()
                    .fill(MacStyleColor.color(for: rule.llmStyle))
                    .frame(width: 6, height: 6)
                Picker("", selection: Binding(
                    get: { appState.appRules[safe: index]?.llmStyle ?? "off" },
                    set: { newValue in updateRule(at: index) { $0.llmStyle = newValue } }
                )) {
                    ForEach(MacLlmStyles, id: \.key) { entry in
                        Text(entry.label).tag(entry.key)
                    }
                }
                .labelsHidden()
                .frame(width: 130)
            }

            Toggle("", isOn: Binding(
                get: { appState.appRules[safe: index]?.enabled ?? false },
                set: { newValue in updateRule(at: index) { $0.enabled = newValue } }
            ))
            .toggleStyle(.switch)
            .labelsHidden()
            .controlSize(.small)

            Button {
                deleteRule(at: index)
            } label: {
                Image(systemName: "trash")
                    .font(.system(size: 12))
                    .foregroundStyle(.red)
            }
            .buttonStyle(.plain)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .opacity(rule.enabled ? 1.0 : 0.55)
        .background(
            // Visual hint while a drag is hovering this row. We can't
            // know which row is the hover target without a delegate,
            // but the dragged row dims so the user has a clear sense
            // of what they're moving.
            (draggedRuleId == rule.id ? Color.accentColor.opacity(0.08) : Color.clear)
        )
        .onDrop(
            of: [UTType.text],
            delegate: AppRuleDropDelegate(
                targetIndex: index,
                getRules: { appState.appRules },
                setRules: { newValue in
                    appState.appRules = newValue
                    persist()
                },
                draggedRuleId: $draggedRuleId
            )
        )
    }

    // MARK: - Footer actions

    private var footerActions: some View {
        HStack(spacing: 8) {
            Button {
                addRule()
            } label: {
                Label("Add rule", systemImage: "plus.circle.fill")
            }
            .buttonStyle(.borderedProminent)

            Button {
                loadDefaults()
            } label: {
                Label("Load defaults", systemImage: "wand.and.stars")
            }
            .buttonStyle(.bordered)

            if !appState.appRules.isEmpty {
                Spacer()
                Button(role: .destructive) {
                    appState.appRules.removeAll()
                    persist()
                } label: {
                    Label("Remove all", systemImage: "trash")
                        .font(.system(size: 11))
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
            }
        }
    }

    // MARK: - Mutations

    private func addRule() {
        let new = AppRule(
            matchPattern: "com.example.app",
            matchType: "bundle_id",
            llmStyle: "correct",
            label: "New rule",
            enabled: true
        )
        appState.appRules.append(new)
        persist()
    }

    private func loadDefaults() {
        // Append-without-duplicates so the user doesn't lose hand-built
        // rules when they explore the defaults. Match by bundle id —
        // case-sensitive per Apple convention.
        var existing = Set(appState.appRules.map { $0.matchPattern })
        for rule in AppRulesDefaults.macV1 where !existing.contains(rule.matchPattern) {
            appState.appRules.append(rule)
            existing.insert(rule.matchPattern)
        }
        persist()
    }

    private func updateRule(at index: Int, _ mutate: (inout AppRule) -> Void) {
        guard appState.appRules.indices.contains(index) else { return }
        var copy = appState.appRules[index]
        mutate(&copy)
        appState.appRules[index] = copy
        persist()
    }

    private func deleteRule(at index: Int) {
        guard appState.appRules.indices.contains(index) else { return }
        appState.appRules.remove(at: index)
        persist()
    }

    private func persist() {
        guard !hydrating else { return }
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }
}

// MARK: - Drag-reorder drop delegate
//
// Each rule row is both a drag source (the line.3.horizontal handle
// emits the rule's UUID via NSItemProvider) and a drop target (this
// delegate). On drop we resolve the dragged UUID → source index, then
// move it to the row the user dropped onto. Persistence happens via
// the setRules callback so a successful reorder is round-tripped to
// Rust immediately.
private struct AppRuleDropDelegate: DropDelegate {
    let targetIndex: Int
    let getRules: () -> [AppRule]
    let setRules: ([AppRule]) -> Void
    @Binding var draggedRuleId: UUID?

    func dropEntered(info: DropInfo) {
        // Live preview: as the user hovers a different row, slide the
        // dragged item into that position so the list rearranges in
        // real time instead of jumping only on release.
        guard let draggedId = draggedRuleId else { return }
        var rules = getRules()
        guard let from = rules.firstIndex(where: { $0.id == draggedId }) else { return }
        if from == targetIndex { return }
        let item = rules.remove(at: from)
        let insertAt = max(0, min(targetIndex, rules.count))
        rules.insert(item, at: insertAt)
        setRules(rules)
    }

    func dropUpdated(info: DropInfo) -> DropProposal? {
        DropProposal(operation: .move)
    }

    func performDrop(info: DropInfo) -> Bool {
        // The actual reorder happened during dropEntered for live
        // feedback. Just clear the drag state so the highlight goes
        // away and we're ready for the next drag.
        draggedRuleId = nil
        return true
    }

    func dropExited(info: DropInfo) {
        // No-op: we keep the live-rearrangement done in dropEntered;
        // exiting just means the user moved off this row, the next
        // dropEntered on a sibling row will take over.
    }
}

// MARK: - Array safe index

private extension Array {
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}
