import SwiftUI
import AppKit

// MARK: - MeetingSidebar
//
// Left rail of the meeting window. Shows a search box, a "New meeting"
// CTA, and a scrollable list of past meeting directories. Mirrors the
// Win sidebar (`MeetingWindow.xaml` Grid.Column="0"). Selection updates
// `vm.selectedDir`; the trash button calls `vm.deleteHistory(...)`
// after an NSAlert confirm.
//
// Visual treatment: NSVisualEffectView background (sidebar material)
// for the proper Mac translucent feel, with the same hairline divider
// stroke vocabulary used in MacAtoms.swift.

struct MeetingSidebar: View {
    @ObservedObject var vm: MeetingViewModel

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Meetings")
                .font(.system(size: 16, weight: .semibold))
                .padding(.horizontal, 16)
                .padding(.top, 14)

            Button(action: { vm.newMeeting() }) {
                HStack(spacing: 6) {
                    Image(systemName: "plus")
                        .font(.system(size: 11, weight: .semibold))
                    Text("New meeting")
                        .font(.system(size: 13, weight: .semibold))
                }
                .frame(maxWidth: .infinity)
                .padding(.vertical, 7)
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.regular)
            .padding(.horizontal, 14)

            searchField
                .padding(.horizontal, 14)

            Divider().opacity(0.4)

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 2) {
                    if vm.historyRows.isEmpty {
                        Text(vm.historySearch.isEmpty
                             ? "No meetings yet."
                             : "No matches.")
                            .font(.system(size: 12))
                            .foregroundStyle(Color.macTextSecondary)
                            .padding(.horizontal, 16)
                            .padding(.top, 12)
                    } else {
                        ForEach(vm.historyRows) { row in
                            MeetingSidebarRow(
                                row: row,
                                isSelected: vm.selectedDir == row.dir,
                                onSelect: { vm.selectHistory(row) },
                                onDelete: { vm.deleteHistory(row) }
                            )
                        }
                    }
                }
                .padding(.vertical, 6)
            }
        }
        .frame(width: 280)
        .background(VisualEffectBackground())
        .overlay(alignment: .trailing) {
            Rectangle()
                .fill(Color.macStrokeHairline)
                .frame(width: 0.5)
        }
    }

    private var searchField: some View {
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 11))
                .foregroundStyle(Color.macTextSecondary)
            TextField("Search past meetings...", text: Binding(
                get: { vm.historySearch },
                set: { newValue in
                    vm.historySearch = newValue
                    vm.loadHistory()
                }
            ))
            .textFieldStyle(.plain)
            .font(.system(size: 12))
            if !vm.historySearch.isEmpty {
                Button {
                    vm.historySearch = ""
                    vm.loadHistory()
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 11))
                        .foregroundStyle(Color.macTextSecondary)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(Color(nsColor: .controlBackgroundColor).opacity(0.7))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .stroke(Color.macControlStroke, lineWidth: 0.5)
        )
    }
}

// MARK: - Row

private struct MeetingSidebarRow: View {
    let row: MeetingHistoryRow
    let isSelected: Bool
    let onSelect: () -> Void
    let onDelete: () -> Void

    @State private var isHovering = false

    var body: some View {
        Button(action: onSelect) {
            HStack(alignment: .center, spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(row.title)
                        .font(.system(size: 13, weight: .semibold))
                        .lineLimit(1)
                        .truncationMode(.tail)
                    Text(row.subtitle)
                        .font(.system(size: 11))
                        .foregroundStyle(Color.macTextSecondary)
                        .lineLimit(1)
                }
                Spacer(minLength: 4)
                if !row.rightLabel.isEmpty {
                    Text(row.rightLabel)
                        .font(.system(size: 9, weight: .semibold))
                        .tracking(0.4)
                        .padding(.horizontal, 5)
                        .padding(.vertical, 2)
                        .foregroundStyle(Color.macTextSecondary)
                        .background(
                            RoundedRectangle(cornerRadius: 4, style: .continuous)
                                .fill(Color.macControlStroke)
                        )
                }
                if isHovering {
                    Button(action: onDelete) {
                        Image(systemName: "trash")
                            .font(.system(size: 11))
                            .foregroundStyle(Color.macTextSecondary)
                            .padding(4)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help("Delete meeting")
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            // Make the WHOLE row hit-test, not just the Text/badges.
            // Without an explicit contentShape, the transparent Spacer
            // and the no-fill `rowBackground` (when idle) leave dead
            // zones where clicks fall through. User report: "vorrei
            // che la selezione fosse per tutta la card del singolo
            // meeting, non solo dove c'è il testo".
            .contentShape(Rectangle())
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(rowBackground)
            )
            .padding(.horizontal, 8)
        }
        .buttonStyle(.plain)
        .onHover { isHovering = $0 }
    }

    private var rowBackground: Color {
        if isSelected { return Color.accentColor.opacity(0.18) }
        if isHovering { return Color.primary.opacity(0.06) }
        return .clear
    }
}

// MARK: - Translucent backdrop

/// NSVisualEffectView-backed SwiftUI helper that gives the sidebar the
/// proper macOS sidebar-material translucency. SwiftUI's
/// `.background(.ultraThinMaterial)` would also work but the
/// `.sidebar` material specifically inherits the right tint at every
/// appearance and accent setting.
struct VisualEffectBackground: NSViewRepresentable {
    var material: NSVisualEffectView.Material = .sidebar
    var blendingMode: NSVisualEffectView.BlendingMode = .behindWindow

    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = blendingMode
        view.state = .followsWindowActiveState
        return view
    }

    func updateNSView(_ nsView: NSVisualEffectView, context: Context) {
        nsView.material = material
        nsView.blendingMode = blendingMode
    }
}
