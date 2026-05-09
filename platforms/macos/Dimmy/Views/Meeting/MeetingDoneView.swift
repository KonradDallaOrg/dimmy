import SwiftUI
import AppKit
import AVFoundation

// MARK: - MeetingDoneView
//
// Done state — shows a finished meeting (just-stopped OR a sidebar
// selection of a past meeting). Mirror of Win DonePanel: header with
// title + meta + toolbar (regen transcript, regen recap, copy, open
// folder), audio playback card, then a stack of recap section cards
// (TLDR, Decisions, Topics, Actions, Open Questions, Risks, Next
// Steps), and a raw-transcript expander at the bottom.
//
// Markdown rendering uses Apple's native `AttributedString.markdown`
// initialiser — no extra dependency, and it's the same vocabulary
// users see across macOS apps.

struct MeetingDoneView: View {
    @ObservedObject var vm: MeetingViewModel
    @State private var rawExpanded: Bool = false
    @State private var copiedFlash: Bool = false

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                header
                if let url = vm.doneAudioURL {
                    audioCard(url: url)
                }
                if let tldr = vm.doneSections["TLDR"], !tldr.isEmpty {
                    tldrCard(tldr)
                }
                cardIfPresent(key: "CONTEXT", title: "Context", systemImage: "info.circle", tint: .secondary)
                cardIfPresent(key: "HIGHLIGHTS", title: "Highlights", systemImage: "sparkles", tint: .yellow)
                cardIfPresent(key: "NARRATIVE", title: "Narrative", systemImage: "text.alignleft", tint: .secondary)
                cardIfPresent(key: "KEY_DECISIONS", title: "Key decisions", systemImage: "checkmark.seal", tint: .green)
                cardIfPresent(key: "TOPICS", title: "Topics discussed", systemImage: "list.bullet.rectangle", tint: .blue)
                cardIfPresent(key: "ACTIONS", title: "Action items", systemImage: "checklist", tint: .orange)
                cardIfPresent(key: "OPEN_QUESTIONS", title: "Open questions", systemImage: "questionmark.circle", tint: .purple)
                cardIfPresent(key: "RISKS", title: "Risks & blockers", systemImage: "exclamationmark.triangle", tint: .red)
                cardIfPresent(key: "NEXT_STEPS", title: "Next steps", systemImage: "arrow.right.circle", tint: .accentColor)
                cardIfPresent(key: "FOLLOWUPS", title: "Follow-ups", systemImage: "envelope.open", tint: .secondary)
                rawTranscriptExpander
            }
            .padding(.vertical, 8)
        }
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(vm.doneTitle)
                    .font(.system(size: 18, weight: .semibold))
                    .lineLimit(1)
                    .truncationMode(.tail)
                Text(vm.doneMeta)
                    .font(.system(size: 11))
                    .foregroundStyle(Color.macTextSecondary)
            }
            Spacer()
            HStack(spacing: 6) {
                toolbarButton(systemImage: "waveform.badge.magnifyingglass",
                              help: "(Re)generate transcript from audio") {
                    vm.regenerateTranscript()
                }
                toolbarButton(systemImage: "arrow.triangle.2.circlepath",
                              help: "(Re)generate recap from transcript") {
                    vm.regenerateRecap()
                }
                toolbarButton(systemImage: copiedFlash ? "checkmark" : "doc.on.doc",
                              help: "Copy recap to clipboard") {
                    copyRecap()
                }
                toolbarButton(systemImage: "folder", help: "Open meeting folder") {
                    let dir = vm.selectedDir ?? activeDirFromAudio
                    if let dir, !dir.isEmpty {
                        NSWorkspace.shared.activateFileViewerSelecting(
                            [URL(fileURLWithPath: dir)]
                        )
                    }
                }
            }
        }
        .padding(14)
        .background(cardBackground)
    }

    private var activeDirFromAudio: String? {
        vm.doneAudioURL?.deletingLastPathComponent().path
    }

    private func toolbarButton(systemImage: String, help: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.system(size: 13))
                .frame(width: 26, height: 26)
        }
        .buttonStyle(.bordered)
        .controlSize(.small)
        .help(help)
    }

    private func copyRecap() {
        let md = MeetingPostProcessService.buildMarkdownFromSections(vm.doneSections)
        guard !md.isEmpty else { return }
        let pb = NSPasteboard.general
        pb.clearContents()
        pb.setString(md, forType: .string)
        copiedFlash = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
            copiedFlash = false
        }
    }

    // MARK: Audio card

    /// Fixed-height audio playback card. We use AVAudioPlayer behind an
    /// NSViewRepresentable rather than `AVKit.VideoPlayer`, because the
    /// latter cannot determine an intrinsic size for audio-only assets
    /// (no video track) and SwiftUI ends up in a layout loop that
    /// crashes with `swift::fatalError` from the layout subsystem.
    /// See `~/Library/Application Support/dimmy/dimmy.log` for the
    /// SIGABRT we hit on first repro.
    private func audioCard(url: URL) -> some View {
        AudioPlaybackBar(url: url)
            .frame(height: 60)
            .padding(.horizontal, 8)
            .padding(.vertical, 8)
            .background(cardBackground)
    }

    // MARK: Cards

    private func tldrCard(_ tldr: String) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("TL;DR")
                .font(.system(size: 11, weight: .semibold))
                .tracking(0.8)
                .foregroundStyle(Color.accentColor)
            renderMarkdown(tldr)
                .font(.system(size: 14))
                .textSelection(.enabled)
        }
        .padding(14)
        .background(
            RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                .fill(Color.accentColor.opacity(0.08))
        )
        .overlay(
            RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                .stroke(Color.accentColor, lineWidth: 1.5)
        )
    }

    @ViewBuilder
    private func cardIfPresent(key: String, title: String, systemImage: String, tint: Color) -> some View {
        if let body = vm.doneSections[key],
           !body.isEmpty,
           !(body == "—" || body == "-") {
            VStack(alignment: .leading, spacing: 8) {
                HStack(spacing: 8) {
                    Image(systemName: systemImage)
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(tint)
                    Text(title)
                        .font(.system(size: 14, weight: .semibold))
                }
                renderMarkdown(body)
                    .font(.system(size: 13))
                    .textSelection(.enabled)
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(cardBackground)
        }
    }

    private var rawTranscriptExpander: some View {
        DisclosureGroup(isExpanded: $rawExpanded) {
            Text(vm.doneRawTranscript.isEmpty
                 ? "(no transcript)"
                 : vm.doneRawTranscript)
                .font(.system(size: 12, design: .monospaced))
                .foregroundStyle(vm.doneRawTranscript.isEmpty
                                  ? Color.macTextSecondary
                                  : Color.primary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .textSelection(.enabled)
                .padding(.top, 8)
        } label: {
            Text("Raw transcript")
                .font(.system(size: 13, weight: .semibold))
        }
        .padding(14)
        .background(cardBackground)
    }

    private var cardBackground: some View {
        RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
            .fill(Color(nsColor: .windowBackgroundColor).opacity(0.6))
            .overlay(
                RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                    .stroke(Color.macStrokeHairline, lineWidth: 0.5)
            )
    }

    // MARK: - Markdown rendering helper

    /// Render a multi-line markdown body. Apple's
    /// `AttributedString.markdown` only handles inline syntax (bold,
    /// italic, code, links) — block-level constructs (bullets, numbered
    /// lists, sub-headings, block quotes) come out as literal text.
    /// We split on lines, classify each, and render each block with the
    /// right SwiftUI shape. Inline syntax inside the block content
    /// still goes through AttributedString so `**bold**`, `*italic*`,
    /// `` `code` `` keep working.
    @ViewBuilder
    private func renderMarkdown(_ body: String) -> some View {
        let lines = body
            .split(separator: "\n", omittingEmptySubsequences: false)
            .map(String.init)
        VStack(alignment: .leading, spacing: 4) {
            ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                renderLine(line)
            }
        }
    }

    @ViewBuilder
    private func renderLine(_ raw: String) -> some View {
        let trimmed = raw.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty {
            // Vertical breathing room between blocks.
            Color.clear.frame(height: 4)
        } else if trimmed.hasPrefix("### ") {
            Text(inlineMarkdown(String(trimmed.dropFirst(4))))
                .font(.system(size: 13, weight: .semibold))
                .padding(.top, 6)
                .frame(maxWidth: .infinity, alignment: .leading)
        } else if trimmed.hasPrefix("## ") {
            Text(inlineMarkdown(String(trimmed.dropFirst(3))))
                .font(.system(size: 14, weight: .semibold))
                .padding(.top, 6)
                .frame(maxWidth: .infinity, alignment: .leading)
        } else if trimmed.hasPrefix("# ") {
            Text(inlineMarkdown(String(trimmed.dropFirst(2))))
                .font(.system(size: 15, weight: .semibold))
                .padding(.top, 6)
                .frame(maxWidth: .infinity, alignment: .leading)
        } else if trimmed.hasPrefix("> ") {
            HStack(alignment: .top, spacing: 8) {
                Rectangle()
                    .fill(Color.accentColor.opacity(0.55))
                    .frame(width: 2)
                Text(inlineMarkdown(String(trimmed.dropFirst(2))))
                    .italic()
                    .foregroundStyle(Color.macTextSecondary)
            }
            .padding(.vertical, 2)
        } else if let bullet = bulletBody(trimmed) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text("•")
                    .foregroundStyle(Color.macTextSecondary)
                    .frame(width: 12, alignment: .leading)
                Text(inlineMarkdown(bullet))
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        } else if let numbered = numberedBody(trimmed) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text("\(numbered.n).")
                    .foregroundStyle(Color.macTextSecondary)
                    .monospacedDigit()
                    .frame(width: 18, alignment: .trailing)
                Text(inlineMarkdown(numbered.body))
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        } else {
            Text(inlineMarkdown(trimmed))
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// Run an inline-only markdown pass over `s`. Falls back to the
    /// raw string on parse error.
    private func inlineMarkdown(_ s: String) -> AttributedString {
        if let attr = try? AttributedString(
            markdown: s,
            options: AttributedString.MarkdownParsingOptions(
                interpretedSyntax: .inlineOnlyPreservingWhitespace
            )
        ) {
            return attr
        }
        return AttributedString(s)
    }

    /// `- item` or `* item` → "item". Returns nil for non-bullets.
    private func bulletBody(_ s: String) -> String? {
        if s.hasPrefix("- ") { return String(s.dropFirst(2)) }
        if s.hasPrefix("* ") { return String(s.dropFirst(2)) }
        return nil
    }

    /// `1. item` / `12. item` → (n, "item"). Returns nil otherwise.
    /// Caps at three digits — meeting recaps don't have 1000-item lists,
    /// so anything longer is almost certainly a sentence starting with
    /// digits, not a numbered marker.
    private func numberedBody(_ s: String) -> (n: Int, body: String)? {
        var i = s.startIndex
        var digits = ""
        while i < s.endIndex, let d = s[i].asciiValue, d >= 0x30, d <= 0x39 {
            digits.append(s[i])
            i = s.index(after: i)
            if digits.count > 3 { return nil }
        }
        guard !digits.isEmpty,
              i < s.endIndex, s[i] == ".",
              s.index(after: i) < s.endIndex, s[s.index(after: i)] == " ",
              let n = Int(digits)
        else { return nil }
        let body = String(s[s.index(i, offsetBy: 2)...])
        return (n, body)
    }
}
