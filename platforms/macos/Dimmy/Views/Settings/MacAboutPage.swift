import AppKit
import SwiftUI

// About, hero with app icon + version + check-for-updates / release
// notes, then Updates settings (auto-update, channel) and Resources
// links. Footer credit "Made with [Anthropic mark] Claude Code" matches
// the Windows v3 footer.

struct MacAboutPage: View {
    @ObservedObject var appState: AppState
    @ObservedObject private var updates = UpdateService.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            heroCard
                .padding(.bottom, 8)

            // Updates group has Status (Simple) + Update channel.
            // Update channel is Advanced per Win XAML (visibility
            // bound to IsAdvanced) since it controls whether the
            // user sees prerelease builds. Status alone stays
            // visible in Simple.
            statusOnlyGroup
            // Channel picker is meaningless without the auto_update
            // scope — Windows collapses `UpdateChannelCard` for the
            // same reason.
            if appState.showAdvanced && updates.isLicensed {
                updateChannelGroup
            }

            setupGroup
            resourcesGroup
            legalGroup
            // "Made with Claude Code" credit removed per Win redesign
            // (commit 4080ff06).
        }
    }

    // MARK: Setup

    // Mirror of the Windows About setup cards. Reopening any of these is
    // idempotent: they re-detect an already-saved key, already-downloaded
    // models and an already-bound shortcut, so nothing is re-downloaded and
    // no key is asked for twice.
    //
    // The first card flips `isOnboardingComplete` to false so the finish step
    // can flip it back to true and trigger the window close, the same
    // handshake the Diagnostics "Reset onboarding" uses. The two wizards need
    // no such dance: they are standalone windows that close themselves.
    private var setupGroup: some View {
        Group {
            MacGroupLabel(text: "Setup")
            HStack(alignment: .top, spacing: 12) {
                MacWizardCard(
                    icon: "sparkles",
                    iconBackground: Color(red: 0.04, green: 0.52, blue: 1.00),
                    title: "Run setup",
                    description: "Model, key, shortcut."
                ) {
                    appState.resetOnboarding()
                    AppDelegate.shared?.reopenOnboarding()
                }
                MacWizardCard(
                    icon: "text.cursor",
                    iconBackground: Color(red: 0.55, green: 0.35, blue: 0.95),
                    title: "Command mode",
                    description: "Edit selected text by voice."
                ) {
                    WizardWindowController.shared.show(.command, appState: appState)
                }
                MacWizardCard(
                    icon: "waveform",
                    iconBackground: Color(red: 0.94, green: 0.36, blue: 0.24),
                    title: "Meeting mode",
                    description: "Record, then get a recap."
                ) {
                    WizardWindowController.shared.show(.meeting, appState: appState)
                }
            }
            // Same height for all three whatever the text length: each card
            // fills the row, and the row sizes to its tallest card.
            .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var statusOnlyGroup: some View {
        Group {
            MacGroupLabel(text: "Updates")
            MacTile {
                MacRow(
                    "Status",
                    description: updates.statusText,
                    showsDivider: false
                ) {
                    if updates.isChecking {
                        ProgressView()
                            .controlSize(.small)
                    } else if updates.canInstallNow {
                        Button("Install and relaunch") { updates.installNow() }
                            .buttonStyle(.borderedProminent)
                    } else if updates.isUpdateReady {
                        Circle()
                            .fill(Color.accentColor)
                            .frame(width: 8, height: 8)
                    } else {
                        EmptyView()
                    }
                }
            }
        }
    }

    private var updateChannelGroup: some View {
        Group {
            MacGroupLabel(text: "Channel")
            MacTile {
                MacRow(
                    "Update channel",
                    hint: "Stable only is the safer default. Prerelease also offers staging builds with new features earlier.",
                    hintURL: URL(string: "https://dimmy.app/help/about-and-updates"),
                    showsDivider: false
                ) {
                    Picker("", selection: $updates.channel) {
                        Text("Stable").tag("stable")
                        Text("Prerelease").tag("prerelease")
                    }
                    .labelsHidden()
                    .frame(width: 140)
                }
            }
        }
    }

    // MARK: Hero

    private var heroCard: some View {
        MacHero(
            title: "Dimmy \(versionString)",
            subtitle: "Voice dictation that stays out of your way.\nReleased \(buildDateString).",
            actions: AnyView(
                HStack(spacing: 8) {
                    Button {
                        updates.checkForUpdatesNow()
                    } label: {
                        Label(updates.isChecking ? "Checking..." : "Check for updates...",
                              systemImage: "arrow.down.circle.fill")
                    }
                    .buttonStyle(.borderedProminent)
                    .disabled(updates.isChecking)

                    Button {
                        if let url = URL(string: "https://github.com/KonradDallaOrg/dimmy/releases") {
                            NSWorkspace.shared.open(url)
                        }
                    } label: {
                        Label("View release notes", systemImage: "doc.text")
                    }
                }
            )
        ) {
            ZStack {
                // Theme-aware brand squircle: white squircle + gradient
                // glyph in light, gradient squircle + white glyph in dark
                // (DimmyAppIcon imageset has both appearances). The Dock's
                // AppIcon is the light variant; this in-app copy follows
                // the system appearance so it reads on either surface.
                Image("DimmyAppIcon")
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .frame(width: 96, height: 96)
                    .shadow(color: Color.accentColor.opacity(0.30),
                            radius: 12, x: 0, y: 6)
            }
            .frame(width: 200, height: 110)
        }
    }

    // MARK: Updates

    // Legacy combined group replaced by statusOnlyGroup +
    // updateChannelGroup (channel gated behind Advanced).

    // MARK: Resources

    private var resourcesGroup: some View {
        Group {
            MacGroupLabel(text: "Resources")
            MacTile {
                MacRow(
                    "Documentation",
                    description: "Guides, shortcuts, troubleshooting",
                    icon: "globe",
                    iconBackground: Color(red: 0.04, green: 0.52, blue: 1.00)
                ) {
                    Link(destination: URL(string: "https://dimmy.app/docs")!) {
                        Text("dimmy.app/docs ›")
                            .font(.system(size: 12))
                    }
                }
                MacRow(
                    "Source code of this version",
                    description: "Source, issues, releases",
                    icon: "chevron.left.forwardslash.chevron.right",
                    iconBackground: Color(red: 0.11, green: 0.11, blue: 0.12),
                    showsDivider: false
                ) {
                    Link(destination: URL(string: "https://github.com/KonradDallaOrg/dimmy/tree/v\(versionString)")!) {
                        Text("github.com ›")
                            .font(.system(size: 12))
                    }
                }
            }
        }
    }

    // MARK: Legal

    // AGPL-3.0 sections 0 and 5(d): the notice, the no-warranty statement and
    // the licence texts, reachable from the running app.
    private var legalGroup: some View {
        Group {
            MacGroupLabel(text: "Legal")
            Text("Dimmy v\(versionString) \u{00A9} Konrad Dalla. Free software under the GNU AGPL v3. NO WARRANTY.")
                .font(.system(size: 12))
                .foregroundStyle(Color.macTextSecondary)
                .padding(.bottom, 8)
            MacTile {
                MacRow(
                    "Licence (AGPL-3.0)",
                    description: "The full licence text",
                    icon: "doc.text.fill",
                    iconBackground: Color.gray
                ) {
                    Button("View") { LegalWindow.show(kind: 0, title: "Licence (AGPL-3.0)") }
                }
                MacRow(
                    "Open source notices",
                    description: "Licences of the components Dimmy includes",
                    icon: "doc.on.doc.fill",
                    iconBackground: Color.gray,
                    showsDivider: false
                ) {
                    Button("View") { LegalWindow.show(kind: 1, title: "Open source notices") }
                }
            }
        }
    }

    // MARK: Footer credit

    private var anthropicCredit: some View {
        HStack(spacing: 6) {
            Text("Made with")
                .font(.system(size: 12))
                .foregroundStyle(Color.macTextTertiary)
            // Same SVG path data as Windows About panel, bundled as a
            // vector asset so it stays crisp at any size.
            Image("ClaudeMark")
                .resizable()
                .aspectRatio(contentMode: .fit)
                .frame(width: 14, height: 14)
            Text("Claude Code")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Color.macTextSecondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 24)
        .padding(.bottom, 4)
    }

    // MARK: Helpers

    /// Shows the BUILD version (`0.6.74-rc.2`), not just the marketing
    /// number — rc.1 and rc.2 print the same `CFBundleShortVersionString`,
    /// so a tester could not tell which one they had installed.
    private var versionString: String {
        UpdateService.runningVersionPlain
    }

    private var buildDateString: String {
        if let infoPath = Bundle.main.path(forResource: "Info", ofType: "plist"),
           let attrs = try? FileManager.default.attributesOfItem(atPath: infoPath),
           let date = attrs[.creationDate] as? Date {
            let formatter = DateFormatter()
            formatter.dateFormat = "MMMM d, yyyy"
            return formatter.string(from: date)
        }
        return "this build"
    }
}

// MARK: - Legal text window

/// The core's legal blocks (`dimmy_legal_document`) as one attributed
/// document for an NSTextView: headings, paragraphs, tables as NSTextTable
/// (the columns line up and long cells wrap), licence texts reflowed in a
/// tinted block. Same column and reflow rules as Windows' LegalDocument.
/// An NSTextView, not a SwiftUI list: the notices are ~1 MB, and on Windows
/// both virtualizing lists failed on rows this uneven (a crash and a hang,
/// 2026-11-07). TextKit lays a document out as a document, and gives
/// selection and Cmd+F for free.
enum LegalDocument {
    private struct Block: Decodable {
        let t: String
        let level: Int?
        let text: String?
        let header: [String]?
        let rows: [[String]]?
    }

    /// Each column's share of a table: its longest cell, clamped to 6...48.
    static func proportions(header: [String], rows: [[String]]) -> [CGFloat] {
        header.indices.map { c in
            let longest = ([header[c]] + rows.compactMap { c < $0.count ? $0[c] : nil })
                .map(\.count).max() ?? 0
            return CGFloat(min(max(longest, 6), 48))
        }
    }

    private static let listMarker = try! NSRegularExpression(pattern: #"^(\(?[0-9a-zA-Z]{1,3}[.)]|[-*•+])\s"#)

    private static func trimmedEnd(_ s: Substring) -> String {
        var s = s
        while let last = s.last, last.isWhitespace { s.removeLast() }
        return String(s)
    }

    /// One paragraph as flowing text when it is prose; with its line breaks
    /// (common indent removed) when a line carries structure.
    static func reflow(_ lines: [String]) -> String {
        let indent: (String) -> Int = { $0.prefix(while: { $0 == " " || $0 == "\t" }).count }
        let cut = lines.map(indent).min() ?? 0
        let body = lines.map { trimmedEnd($0.dropFirst(cut)) }
        let hasText: (String) -> Bool = { $0.contains(where: { $0.isLetter || $0.isNumber }) }
        let structured = body.count > 1 && (!hasText(body[0]) || body.dropFirst().contains { l in
            guard let first = l.first else { return false }
            let range = NSRange(l.startIndex..., in: l)
            return first.isWhitespace || listMarker.firstMatch(in: l, range: range) != nil || !hasText(l)
        })
        return structured
            ? body.joined(separator: "\n")
            : body.map { $0.trimmingCharacters(in: .whitespaces) }.joined(separator: " ")
    }

    /// A licence text, hard-wrapped at ~80 columns, with each paragraph
    /// reflowed to the window width.
    static func licence(_ text: String) -> String {
        var paragraphs: [String] = []
        var para: [String] = []
        for line in text.replacingOccurrences(of: "\r\n", with: "\n").components(separatedBy: "\n") {
            if line.trimmingCharacters(in: .whitespaces).isEmpty {
                if !para.isEmpty { paragraphs.append(reflow(para)); para = [] }
            } else {
                para.append(line)
            }
        }
        if !para.isEmpty { paragraphs.append(reflow(para)) }
        return paragraphs.joined(separator: "\n\n")
    }

    private static func style(before: CGFloat = 0, after: CGFloat = 0, blocks: [NSTextBlock] = []) -> NSParagraphStyle {
        let p = NSMutableParagraphStyle()
        p.paragraphSpacingBefore = before
        p.paragraphSpacing = after
        p.textBlocks = blocks
        return p
    }

    static func attributed(_ json: String?) -> NSAttributedString {
        let out = NSMutableAttributedString()
        guard let data = json?.data(using: .utf8),
              let blocks = try? JSONDecoder().decode([Block].self, from: data), !blocks.isEmpty else {
            return NSAttributedString(string: "Text unavailable.", attributes: [.font: NSFont.systemFont(ofSize: 13), .foregroundColor: NSColor.labelColor])
        }
        func add(_ text: String, _ font: NSFont, _ paragraph: NSParagraphStyle, _ color: NSColor = .labelColor) {
            out.append(NSAttributedString(string: text + "\n", attributes: [.font: font, .foregroundColor: color, .paragraphStyle: paragraph]))
        }
        for b in blocks {
            switch b.t {
            case "heading":
                let level = b.level ?? 1
                let font: NSFont = level <= 1 ? .systemFont(ofSize: 22, weight: .bold)
                    : level == 2 ? .systemFont(ofSize: 17, weight: .semibold) : .systemFont(ofSize: 14, weight: .semibold)
                add(b.text ?? "", font, style(before: out.length == 0 ? 0 : 18, after: 6))
            case "paragraph":
                add(b.text ?? "", .systemFont(ofSize: 13), style(after: 8))
            case "code":
                let card = NSTextBlock()
                card.backgroundColor = .controlBackgroundColor
                card.setWidth(14, type: .absoluteValueType, for: .padding)
                card.setWidth(1, type: .absoluteValueType, for: .border)
                card.setBorderColor(.separatorColor)
                card.setWidth(4, type: .absoluteValueType, for: .margin, edge: .minY)
                card.setWidth(10, type: .absoluteValueType, for: .margin, edge: .maxY)
                add(licence(b.text ?? ""), .systemFont(ofSize: 12), style(blocks: [card]))
            case "table":
                let header = b.header ?? [], rows = b.rows ?? []
                guard !header.isEmpty else { continue }
                let cols = proportions(header: header, rows: rows)
                let total = cols.reduce(0, +)
                let table = NSTextTable()
                table.numberOfColumns = header.count
                table.collapsesBorders = true
                table.setContentWidth(100, type: .percentageValueType)
                for (r, cells) in ([header] + rows).enumerated() {
                    for c in header.indices {
                        let cell = NSTextTableBlock(table: table, startingRow: r, rowSpan: 1, startingColumn: c, columnSpan: 1)
                        cell.setContentWidth(100 * cols[c] / total, type: .percentageValueType)
                        cell.setWidth(r == 0 ? 7 : 4, type: .absoluteValueType, for: .padding)
                        cell.setWidth(1, type: .absoluteValueType, for: .border, edge: .maxY)
                        cell.setBorderColor(.separatorColor, for: .maxY)
                        let font: NSFont = r == 0 ? .systemFont(ofSize: 12, weight: .semibold) : .systemFont(ofSize: 12)
                        add(c < cells.count ? cells[c] : "", font, style(blocks: [cell]))
                    }
                }
                add("", .systemFont(ofSize: 6), style(after: 6))
            default:
                continue
            }
        }
        return out
    }
}

/// The licence or the notices in a window of their own: resizable, with a
/// scroll bar, searchable with Cmd+F.
@MainActor
enum LegalWindow {
    private static var windows: [Int32: NSWindow] = [:]

    /// kind 0 licence, 1 open source notices. Asking again brings it back.
    static func show(kind: Int32, title: String) {
        if let w = windows[kind] {
            w.makeKeyAndOrderFront(nil)
            NSApp.activate(ignoringOtherApps: true)
            return
        }
        let text = NSTextView(usingTextLayoutManager: false) // TextKit 1: NSTextTable
        text.isEditable = false
        text.isSelectable = true
        text.usesFindBar = true
        text.isIncrementalSearchingEnabled = true
        text.drawsBackground = true
        text.backgroundColor = .windowBackgroundColor
        text.textContainerInset = NSSize(width: 24, height: 20)
        text.isVerticallyResizable = true
        text.isHorizontallyResizable = false
        text.autoresizingMask = [.width]
        text.textContainer?.widthTracksTextView = true
        text.layoutManager?.allowsNonContiguousLayout = true
        text.textStorage?.setAttributedString(LegalDocument.attributed(DimmyCore.shared.legalDocument(kind)))

        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.autohidesScrollers = false
        scroll.documentView = text

        let w = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 860, height: 720),
                         styleMask: [.titled, .closable, .miniaturizable, .resizable],
                         backing: .buffered, defer: false)
        w.title = title
        w.isReleasedWhenClosed = false
        w.contentView = scroll
        w.contentMinSize = NSSize(width: 520, height: 400)
        text.frame = NSRect(origin: .zero, size: scroll.contentSize)
        w.center()
        windows[kind] = w
        w.makeKeyAndOrderFront(nil)
        NSApp.activate(ignoringOtherApps: true)
    }
}
