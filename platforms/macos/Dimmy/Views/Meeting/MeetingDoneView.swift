import SwiftUI
import AppKit
import AVFoundation

// MARK: - MeetingDoneView
//
// Done state, shows a finished meeting (just-stopped OR a sidebar
// selection of a past meeting). Mirror of Win MeetingWindow.xaml
// DonePanel: a title+meta header card, an audio playback card with a
// click-to-seek waveform (dual-band mirrored stereo when per-track
// WAVs are present), then a 3-tab strip, "Recap | Transcript | Notes"
//, with the action toolbar (regen transcript, regen recap, copy,
// send-to-notion, open-folder) pinned to its right.
//
//   • Recap     : TL;DR accent card + structural section cards
//                 (Context, Highlights, Narrative, Key decisions,
//                 Topics, Actions, Open Questions, Risks, Next steps,
//                 Follow-ups).
//   • Transcript: full `transcripts.txt` in monospaced selectable text.
//   • Notes     : local TextEditor persisted to `<dir>/notes.md` on
//                 focus-loss / tab-switch / view-disappear.
//
// Markdown rendering uses Apple's native `AttributedString.markdown`
// initialiser, no extra dependency, and it's the same vocabulary
// users see across macOS apps.

struct MeetingDoneView: View {
    @ObservedObject var vm: MeetingViewModel
    @Environment(\.colorScheme) private var colorScheme
    @State private var copiedFlash: Bool = false
    @State private var claudeMcpInstalled: Bool = false
    @State private var claudeIconPath: String? = nil
    @State private var editingTitle: Bool = false
    @State private var titleDraft: String = ""
    @FocusState private var notesFocused: Bool
    @FocusState private var titleFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            header
            if let url = vm.doneAudioURL {
                audioCard(url: url)
            }
            tabStripWithToolbar
            tabContent
        }
        .padding(.vertical, 8)
        .padding(.horizontal, 4)
        .onAppear {
            // Surface the Claude Desktop deeplink button only when the
            // MCP extension is installed. Status query is a single FFI
            // call (cheap, no event-callback wiring needed).
            claudeMcpInstalled = DimmyCore.shared.claudeDesktopStatus().extensionInstalled
            if claudeMcpInstalled {
                Task { claudeIconPath = await ClaudeIconExtractor.tryExtract() }
            }
        }
        .onDisappear { vm.saveNotes() }
    }

    // MARK: Header (title + meta only, toolbar moved next to tab strip)

    private var header: some View {
        VStack(alignment: .leading, spacing: 2) {
            // Click-to-edit title, mirror of Win's
            // DoneTitle_Tapped + DoneTitleEdit (Enter commits, Esc
            // cancels, focus-out commits). Persisted to meta.json
            // via MeetingViewModel.renameSelectedMeeting.
            if editingTitle {
                TextField("Meeting title", text: $titleDraft, onCommit: commitTitleEdit)
                    .textFieldStyle(.plain)
                    .font(.system(size: 18, weight: .semibold))
                    .focused($titleFocused)
                    .onAppear { titleFocused = true }
                    .onChange(of: titleFocused) { _, focused in
                        if !focused { commitTitleEdit() }
                    }
                    .onExitCommand { editingTitle = false }
            } else {
                Text(vm.doneTitle)
                    .font(.system(size: 18, weight: .semibold))
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .contentShape(Rectangle())
                    .onTapGesture {
                        titleDraft = vm.doneTitle
                        editingTitle = true
                    }
                    .help("Click to rename the meeting")
            }
            Text(vm.doneMeta)
                .font(.system(size: 11))
                .foregroundStyle(Color.macTextSecondary)
            // Meeting-type chip (Notion-style): the auto-detected (or
            // chosen) type. Hidden when unresolved (auto/unknown).
            if let typeLabel = MeetingPostProcessService.friendlyTypeLabel(vm.doneSections["__TYPE__"]) {
                Text(typeLabel)
                    .font(.system(size: 10, weight: .semibold))
                    .padding(.horizontal, 8)
                    .padding(.vertical, 2)
                    .background(Capsule().fill(Color.macTextSecondary.opacity(0.12)))
                    .foregroundStyle(Color.macTextSecondary)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(14)
        .background(cardBackground)
    }

    private func commitTitleEdit() {
        guard editingTitle else { return }
        editingTitle = false
        vm.renameSelectedMeeting(to: titleDraft)
    }

    private var activeDirFromAudio: String? {
        vm.doneAudioURL?.deletingLastPathComponent().path
    }

    // MARK: Tab strip + action toolbar
    //
    // Mirror of Win MeetingWindow.xaml:409-475, tab labels on the
    // left with a 2pt accent underline on the selected entry, action
    // icons on the right always visible regardless of which tab is
    // active. Selected tab = SemiBold + full opacity + accent
    // underline; the others go to opacity 0.6 + Regular weight.

    private var tabStripWithToolbar: some View {
        HStack(alignment: .bottom, spacing: 18) {
            HStack(spacing: 22) {
                ForEach(MeetingViewModel.DoneTab.allCases) { tab in
                    tabLabel(tab)
                }
            }
            Spacer()
            HStack(spacing: 6) {
                // Meeting-type override: default "Auto-detect"; pick a type to
                // bias the recap's emphasis, then hit the regenerate-recap
                // button. The detected / chosen type shows as the chip above.
                Picker("", selection: $vm.selectedMeetingType) {
                    ForEach(MeetingPostProcessService.meetingTypes, id: \.key) { t in
                        Text(t.label).tag(t.key)
                    }
                }
                .labelsHidden()
                .frame(maxWidth: 170)
                .help("Meeting type — pick one, then regenerate the recap")
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
                // Notion brand mark is rendered as black-on-transparent;
                // invert in dark mode so it stays legible on the dark
                // toolbar instead of disappearing into the background.
                ToolbarIconButton(help: "Send recap to Notion") {
                    Task { await sendToNotion() }
                } label: {
                    notionToolbarIcon
                }
                if claudeMcpInstalled {
                    recapWithClaudeButton
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
        .padding(.horizontal, 4)
    }

    private func tabLabel(_ tab: MeetingViewModel.DoneTab) -> some View {
        let selected = vm.doneSelectedTab == tab
        return Button {
            // Save any in-flight notes edit before switching away, the
            // TextEditor's focus loss already triggers a save, but a
            // tab click before focus change wouldn't.
            if vm.doneSelectedTab == .notes && tab != .notes { vm.saveNotes() }
            vm.doneSelectedTab = tab
        } label: {
            VStack(spacing: 4) {
                Text(tab.title)
                    .font(.system(size: 14, weight: selected ? .semibold : .regular))
                    .foregroundStyle(selected ? Color.primary : Color.primary.opacity(0.6))
                Rectangle()
                    .fill(selected ? Color.accentColor : Color.clear)
                    .frame(height: 2)
            }
            .frame(minWidth: 56)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }

    // MARK: Per-tab content

    @ViewBuilder
    private var tabContent: some View {
        switch vm.doneSelectedTab {
        case .recap:
            recapContent
        case .transcript:
            transcriptContent
        case .notes:
            notesContent
        }
    }

    private var recapContent: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                if let tldr = vm.doneSections["TLDR"], !tldr.isEmpty {
                    tldrCard(tldr)
                }
                // Recovery CTA: when the configured recap backend failed
                // but Claude Desktop + the dimmy MCP extension are
                // connected, offer to generate the recap through Claude
                // Desktop (the user's subscription, no API key or CLI).
                // Surfaces the existing deeplink exactly when it's the
                // working path. Mirror of Win RefreshClaudeFallbackCta.
                if vm.recapFailed && claudeMcpInstalled {
                    recapWithClaudeFallbackCta
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
                if vm.doneSections.isEmpty {
                    Text("No recap yet, click the (Re)generate recap button to run the LLM, or wait if it's still processing.")
                        .font(.system(size: 13))
                        .foregroundStyle(Color.macTextSecondary)
                        .padding(14)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .background(cardBackground)
                }
            }
        }
    }

    private var transcriptContent: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 0) {
                Text(vm.doneRawTranscript.isEmpty
                     ? "No transcript on disk yet. Re-run transcription with the (Re)generate transcript button."
                     : vm.doneRawTranscript)
                    .font(.system(size: 12, design: .monospaced))
                    .foregroundStyle(vm.doneRawTranscript.isEmpty
                                      ? Color.macTextSecondary
                                      : Color.primary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .textSelection(.enabled)
            }
            .padding(14)
            .background(cardBackground)
        }
    }

    private var notesContent: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Local notes, saved to notes.md in the meeting folder. Markdown supported.")
                .font(.system(size: 11))
                .foregroundStyle(Color.macTextSecondary)
                .padding(.horizontal, 4)
            ZStack(alignment: .topLeading) {
                if vm.doneNotes.isEmpty {
                    Text("Write notes about this meeting...")
                        .font(.system(size: 13))
                        .foregroundStyle(Color.macTextSecondary.opacity(0.7))
                        .padding(.horizontal, 18)
                        .padding(.vertical, 16)
                        .allowsHitTesting(false)
                }
                TextEditor(text: $vm.doneNotes)
                    .font(.system(size: 13))
                    .scrollContentBackground(.hidden)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 12)
                    .focused($notesFocused)
                    .onChange(of: notesFocused) { _, isFocused in
                        if !isFocused { vm.saveNotes() }
                    }
            }
            .background(cardBackground)
        }
    }

    @ViewBuilder
    private var notionToolbarIcon: some View {
        let img = Image("notion").resizable().scaledToFit().frame(width: 16, height: 16)
        if colorScheme == .dark {
            img.colorInvert()
        } else {
            img
        }
    }

    private func toolbarButton(systemImage: String, help: String, action: @escaping () -> Void) -> some View {
        ToolbarIconButton(help: help, action: action) {
            Image(systemName: systemImage)
                .font(.system(size: 16, weight: .medium))
                .symbolRenderingMode(.hierarchical)
        }
    }

    /// "Recap with Claude Desktop", opens `claude://claude.ai/new?q=...`
    /// with the structured-recap prompt. Mirror of the Win
    /// `RecapWithClaude_Click` handler in MeetingWindow.xaml.cs. Visible
    /// only when the MCP extension is installed. Falls back to copying
    /// the prompt to the pasteboard if the deeplink fails (e.g. Claude
    /// Desktop's URI handler not yet registered after a fresh install).
    ///
    /// Icon: bundled `ClaudeMark.imageset`, the canonical orange
    /// Anthropic burst (#D97757), pulled from the official Anthropic
    /// brand mark. At 16-px toolbar size the bare burst reads cleaner
    /// than the full Mac AppIcon (which is the burst on a rounded
    /// squircle background). No SF-Symbol fallback, the brand mark
    /// is committed to the app bundle, always available.
    private var recapWithClaudeButton: some View {
        ToolbarIconButton(help: "Recap with Claude Desktop (uses MCP)") {
            recapWithClaude()
        } label: {
            Image("ClaudeMark")
                .resizable()
                .scaledToFit()
                .frame(width: 16, height: 16)
        }
    }

    /// Recovery CTA shown in the recap tab when the configured backend
    /// failed but Claude Desktop + the MCP bridge are connected. Reuses
    /// the same `recapWithClaude()` deeplink as the toolbar button, just
    /// surfaced prominently where the user is staring at the failure.
    private var recapWithClaudeFallbackCta: some View {
        Button {
            recapWithClaude()
        } label: {
            HStack(spacing: 8) {
                Image("ClaudeMark")
                    .resizable()
                    .scaledToFit()
                    .frame(width: 16, height: 16)
                Text("Generate recap with Claude Desktop")
            }
        }
        .buttonStyle(.borderedProminent)
        .help("Generate this meeting's recap through Claude Desktop (uses MCP)")
    }

    private func recapWithClaude() {
        let dir = vm.selectedDir ?? activeDirFromAudio ?? ""
        let meetingId = (dir as NSString).lastPathComponent
        guard !meetingId.isEmpty else { return }

        // Tight prompt, Claude consults `dimmy_get_recap_template` for
        // the structure, so we don't have to inline the entire format
        // here. Keep VERBATIM in sync with the Win counterpart in
        // `MeetingWindow.xaml.cs::RecapWithClaudeDesktop_Click`.
        let prompt =
            "Recap Dimmy meeting `\(meetingId)`.\n\n" +
            "1. Call `dimmy_get_recap_template` to fetch Dimmy's house format.\n" +
            "2. Call `dimmy_get_meeting` with id `\(meetingId)` to read the transcript.\n" +
            "3. Produce a recap that follows the template's rules exactly (first line is a Markdown H1 title in the transcript's language).\n" +
            "4. Call `dimmy_save_recap` with id `\(meetingId)` and your recap markdown to persist it back into Dimmy.\n" +
            "5. Confirm to me once saved."

        var comps = URLComponents()
        comps.scheme = "claude"
        comps.host = "claude.ai"
        comps.path = "/new"
        comps.queryItems = [URLQueryItem(name: "q", value: prompt)]
        guard let url = comps.url else { return }
        if !NSWorkspace.shared.open(url) {
            // Fallback: copy the prompt so the user can paste it
            // manually into a fresh Claude chat. The URI handler may
            // not be registered yet on a brand-new install.
            let pb = NSPasteboard.general
            pb.clearContents()
            pb.setString(prompt, forType: .string)
        }
    }

    private func toolbarButton(assetImage: String, help: String, action: @escaping () -> Void) -> some View {
        ToolbarIconButton(help: help, action: action) {
            Image(assetImage)
                .resizable()
                .scaledToFit()
                .frame(width: 16, height: 16)
        }
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

    /// Send the current meeting's recap.md to Notion. Surfaces a
    /// success / failure alert + offers an "Open in Notion" button on
    /// success. Disabled flow until token + target are configured
    /// (Settings → Integrations).
    @MainActor
    private func sendToNotion() async {
        guard let dir = vm.selectedDir ?? activeDirFromAudio, !dir.isEmpty else { return }
        if !DimmyCore.shared.notionHasToken {
            await showNotionAlert(
                title: "Notion not connected",
                message: "Connect to Notion in Settings → Integrations first. Paste your integration token, pick where recaps should land, then come back here.",
                isError: true,
                pageURL: nil
            )
            return
        }
        let json = await Task.detached { DimmyCore.shared.notionSendRecap(meetingDir: dir) }.value
        guard let json = json,
              let data = json.data(using: .utf8),
              let dict = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            await showNotionAlert(
                title: "Couldn't send to Notion",
                message: "Invalid response from the integration.",
                isError: true,
                pageURL: nil
            )
            return
        }
        let ok = dict["ok"] as? Bool ?? false
        if ok {
            let pageURL = dict["page_url"] as? String
            await showNotionAlert(
                title: "Sent to Notion ✓",
                message: "Your recap is live in Notion.",
                isError: false,
                pageURL: pageURL
            )
        } else {
            await showNotionAlert(
                title: "Couldn't send to Notion",
                message: (dict["error"] as? String) ?? "Unknown error",
                isError: true,
                pageURL: nil
            )
        }
    }

    @MainActor
    private func showNotionAlert(title: String, message: String, isError: Bool, pageURL: String?) async {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.alertStyle = isError ? .warning : .informational
        if !isError, let urlStr = pageURL, !urlStr.isEmpty {
            alert.addButton(withTitle: "Open in Notion")
            alert.addButton(withTitle: "Close")
        } else {
            alert.addButton(withTitle: "Close")
        }
        let response = alert.runModal()
        if response == .alertFirstButtonReturn,
           !isError,
           let urlStr = pageURL,
           let url = URL(string: urlStr) {
            NSWorkspace.shared.open(url)
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
        AudioPlaybackBar(
            url: url,
            micURL: vm.doneAudioMicURL,
            systemURL: vm.doneAudioSystemURL
        )
            .frame(height: 64)
            .padding(.horizontal, 16)
            .padding(.vertical, 14)
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
           !(body == ", " || body == "-") {
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

    private var cardBackground: some View {
        RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
            .fill(Color(nsColor: .controlBackgroundColor))
            .overlay(
                RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                    .stroke(Color.primary.opacity(0.09), lineWidth: 1)
            )
            .shadow(color: Color.black.opacity(0.06), radius: 6, x: 0, y: 1)
    }

    // MARK: - Markdown rendering helper

    /// Render a multi-line markdown body. Apple's
    /// `AttributedString.markdown` only handles inline syntax (bold,
    /// italic, code, links), block-level constructs (bullets, numbered
    /// lists, sub-headings, block quotes) come out as literal text.
    /// We split on lines, classify each, and render each block with the
    /// right SwiftUI shape. Inline syntax inside the block content
    /// still goes through AttributedString so `**bold**`, `*italic*`,
    /// `` `code` `` keep working.
    @ViewBuilder
    private func renderMarkdown(_ body: String) -> some View {
        let blocks = MarkdownBlockParser.parse(body)
        VStack(alignment: .leading, spacing: 4) {
            ForEach(Array(blocks.enumerated()), id: \.offset) { _, block in
                renderBlock(block)
            }
        }
    }

    @ViewBuilder
    private func renderBlock(_ block: MarkdownBlock) -> some View {
        switch block {
        case .blank:
            Color.clear.frame(height: 4)
        case .heading(let level, let text):
            // Levels 1-4. Level 4 collapses into the ### size, past the
            // recap structural headings (level 2) the LLM rarely needs
            // a fifth tier.
            let size: CGFloat = {
                switch level {
                case 1: return 15
                case 2: return 14
                case 3: return 13
                default: return 12
                }
            }()
            Text(inlineMarkdown(text))
                .font(.system(size: size, weight: .semibold))
                .padding(.top, 6)
                .frame(maxWidth: .infinity, alignment: .leading)
        case .quote(let text):
            HStack(alignment: .top, spacing: 8) {
                Rectangle()
                    .fill(Color.accentColor.opacity(0.55))
                    .frame(width: 2)
                Text(inlineMarkdown(text))
                    .italic()
                    .foregroundStyle(Color.macTextSecondary)
            }
            .padding(.vertical, 2)
        case .bullet(let text):
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text("•")
                    .foregroundStyle(Color.macTextSecondary)
                    .frame(width: 12, alignment: .leading)
                Text(inlineMarkdown(text))
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        case .numbered(let n, let text):
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text("\(n).")
                    .foregroundStyle(Color.macTextSecondary)
                    .monospacedDigit()
                    .frame(width: 18, alignment: .trailing)
                Text(inlineMarkdown(text))
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        case .codeBlock(let lines):
            // Recap LLM occasionally drops a fenced code snippet (e.g.
            // CLI commands in next-steps). Render in monospaced font on
            // a tinted slab so it doesn't collide with body copy.
            VStack(alignment: .leading, spacing: 0) {
                ForEach(Array(lines.enumerated()), id: \.offset) { _, line in
                    Text(line)
                        .font(.system(size: 12, design: .monospaced))
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            .padding(8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(Color(nsColor: .quaternaryLabelColor).opacity(0.35))
            )
        case .paragraph(let text):
            Text(inlineMarkdown(text))
                .frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    /// Run a full markdown pass over `s` so inline `[text](url)` and
    /// `**bold**` / `*italic*` / `` `code` `` all render. Falls back to
    /// the raw string on parse error.
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

}

// MARK: - MarkdownBlock + parser
//
// Pulled out of the view so it can be unit-tested without dragging
// SwiftUI in. Handles block-level constructs the recap LLM actually
// emits: ATX headings (`# `..`#### `), block quotes, dash/star bullets,
// `N. ` numbered lists, fenced code blocks, blank-line spacers, and a
// "paragraph" fallback for everything else. Tables and HTML are NOT
// supported, the recap prompt doesn't ask the LLM to emit them, but
// if it does the line lands in `paragraph` and renders as inline text.

enum MarkdownBlock: Equatable {
    case blank
    case heading(level: Int, text: String)
    case quote(text: String)
    case bullet(text: String)
    case numbered(n: Int, text: String)
    case codeBlock(lines: [String])
    case paragraph(text: String)
}

enum MarkdownBlockParser {
    static func parse(_ body: String) -> [MarkdownBlock] {
        let raw = body.split(separator: "\n", omittingEmptySubsequences: false)
        var blocks: [MarkdownBlock] = []
        var i = 0
        while i < raw.count {
            let line = String(raw[i])
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.isEmpty {
                blocks.append(.blank)
                i += 1
                continue
            }
            // Fenced code block. We accept ``` and ~~~. The fence may
            // include a language tag (```swift) which we discard.
            if trimmed.hasPrefix("```") || trimmed.hasPrefix("~~~") {
                let fence = String(trimmed.prefix(3))
                var collected: [String] = []
                i += 1
                while i < raw.count {
                    let inner = String(raw[i])
                    if inner.trimmingCharacters(in: .whitespaces).hasPrefix(fence) {
                        i += 1
                        break
                    }
                    collected.append(inner)
                    i += 1
                }
                blocks.append(.codeBlock(lines: collected))
                continue
            }
            if let level = atxHeadingLevel(trimmed) {
                let text = String(trimmed.dropFirst(level + 1))
                blocks.append(.heading(level: level, text: text))
            } else if trimmed.hasPrefix("> ") {
                blocks.append(.quote(text: String(trimmed.dropFirst(2))))
            } else if let bullet = bulletBody(trimmed) {
                blocks.append(.bullet(text: bullet))
            } else if let numbered = numberedBody(trimmed) {
                blocks.append(.numbered(n: numbered.n, text: numbered.body))
            } else {
                blocks.append(.paragraph(text: trimmed))
            }
            i += 1
        }
        return blocks
    }

    /// Returns the heading level (1-4) if `s` starts with `#`..`####`
    /// followed by a space. Returns nil for `#####`+ or non-headings.
    private static func atxHeadingLevel(_ s: String) -> Int? {
        for level in (1...4).reversed() {
            let prefix = String(repeating: "#", count: level) + " "
            if s.hasPrefix(prefix) { return level }
        }
        return nil
    }

    /// `- item` or `* item` → "item". Returns nil for non-bullets.
    private static func bulletBody(_ s: String) -> String? {
        if s.hasPrefix("- ") { return String(s.dropFirst(2)) }
        if s.hasPrefix("* ") { return String(s.dropFirst(2)) }
        return nil
    }

    /// `1. item` / `12. item` → (n, "item"). Caps at three digits , 
    /// meeting recaps don't have 1000-item lists, so anything longer is
    /// almost certainly a sentence starting with digits, not a marker.
    private static func numberedBody(_ s: String) -> (n: Int, body: String)? {
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

// MARK: - ToolbarIconButton
//
// Borderless icon button with a soft hover background, the macOS
// Mail / Messages toolbar style. Default state is flat (icon only) so
// the header stays clean; on hover a subtle rounded fill appears,
// giving the click affordance without the heavy default Bordered pill.
// Used for the Done view header toolbar (regen, copy, send-to-notion,
// open-folder).
private struct ToolbarIconButton<Label: View>: View {
    let help: String
    let action: () -> Void
    @ViewBuilder let label: () -> Label

    @State private var hovering: Bool = false

    var body: some View {
        Button(action: action) {
            label()
                .frame(width: 30, height: 28)
                .background(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(Color.primary.opacity(hovering ? 0.14 : 0))
                )
                .contentShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
        }
        .buttonStyle(.borderless)
        .onHover { hovering = $0 }
        .help(help)
    }
}
