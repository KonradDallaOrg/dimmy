import AppKit
import SwiftUI

// MARK: - Tahoe design tokens
//
// Mirrors the `mac-styles.css` token set from the design handoff bundle.
// Centralising values here keeps every page consistent and avoids the
// drift you get when each view re-implements its own padding / radius /
// stroke colour. macOS dark/light is derived from `NSAppearance` rather
// than a custom toggle, the OS already picks the right side of the
// `Color` shape resolver.

enum MacTheme {
    // Tile / row geometry
    static let tileCornerRadius: CGFloat = 14
    static let heroCornerRadius: CGFloat = 16
    static let rowMinHeight: CGFloat = 40
    static let rowVerticalPadding: CGFloat = 10
    static let rowHorizontalPadding: CGFloat = 14
    static let rowSpacing: CGFloat = 12

    // Group label
    static let groupLabelTopPadding: CGFloat = 18
    static let groupLabelBottomPadding: CGFloat = 6
    static let groupFooterTopPadding: CGFloat = 6

    // Sidebar
    static let sidebarWidth: CGFloat = 240
    static let navItemHeight: CGFloat = 36
    static let navIconSize: CGFloat = 28
    static let navIconRadius: CGFloat = 8

    // Page chrome
    static let pageHorizontalPadding: CGFloat = 24
    static let pageTopPadding: CGFloat = 4
    static let pageBottomPadding: CGFloat = 32
    static let pageHeadBottomPadding: CGFloat = 18

    // Controls
    static let controlHeight: CGFloat = 24
    static let controlCornerRadius: CGFloat = 8
    static let switchWidth: CGFloat = 38
    static let switchHeight: CGFloat = 22
    static let chipHeight: CGFloat = 24

    // Shadows / strokes
    static let hairlineOpacity: Double = 0.08
    static let strongStrokeOpacity: Double = 0.22
}

extension Color {
    static let macStrokeHairline = Color.primary.opacity(0.06)
    static let macRowDivider = Color.primary.opacity(MacTheme.hairlineOpacity)
    static let macControlStroke = Color.primary.opacity(0.12)
    static let macControlStrokeStrong = Color.primary.opacity(MacTheme.strongStrokeOpacity)
    static let macTextSecondary = Color.primary.opacity(0.56)
    static let macTextTertiary = Color.primary.opacity(0.4)
}

// MARK: - GroupLabel + GroupFooter
//
// "TELEMETRY" / "RESOURCES" uppercase headers that sit above each Tile,
// plus the optional small explanatory paragraph below.

struct MacGroupLabel: View {
    let text: String
    var body: some View {
        Text(text.uppercased())
            .font(.system(size: 11, weight: .semibold))
            .tracking(0.5)
            .foregroundStyle(Color.macTextTertiary)
            .padding(.top, MacTheme.groupLabelTopPadding)
            .padding(.bottom, MacTheme.groupLabelBottomPadding)
            .padding(.horizontal, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

struct MacGroupFooter: View {
    let text: String
    var body: some View {
        Text(text)
            .font(.system(size: 11))
            .foregroundStyle(Color.macTextSecondary)
            .lineSpacing(2)
            .padding(.top, MacTheme.groupFooterTopPadding)
            .padding(.horizontal, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

// MARK: - Tile (grouped settings card)
//
// Wraps a stack of `MacRow`s with the Tahoe rounded corner + hairline
// stroke + subtle shadow look. Children render their own internal
// dividers via `MacRow.showsDivider` so the padding/inset stays
// consistent with macOS Settings.app.

struct MacTile<Content: View>: View {
    @ViewBuilder var content: () -> Content
    var body: some View {
        VStack(spacing: 0) { content() }
            .background(
                RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                    .fill(Color(nsColor: .windowBackgroundColor).opacity(0.6))
            )
            .overlay(
                RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                    .stroke(Color.macStrokeHairline, lineWidth: 0.5)
            )
            .shadow(color: .black.opacity(0.04), radius: 0.5, x: 0, y: 0.5)
    }
}

// MARK: - Row
//
// One settings line: optional 28×28 squircle icon, label + description,
// trailing control. Internal divider sits 14px from leading edge to match
// Apple's NSGroupedListView behaviour.

struct MacRow<Trailing: View>: View {
    let label: String
    var description: String? = nil
    var hint: String? = nil
    var hintURL: URL? = nil
    var hintURLLabel: String? = nil
    var icon: String? = nil
    var iconBackground: Color? = nil
    var showsDivider: Bool = true
    @ViewBuilder var trailing: () -> Trailing

    init(
        _ label: String,
        description: String? = nil,
        hint: String? = nil,
        hintURL: URL? = nil,
        hintURLLabel: String? = nil,
        icon: String? = nil,
        iconBackground: Color? = nil,
        showsDivider: Bool = true,
        @ViewBuilder trailing: @escaping () -> Trailing
    ) {
        self.label = label
        self.description = description
        self.hint = hint
        self.hintURL = hintURL
        self.hintURLLabel = hintURLLabel
        self.icon = icon
        self.iconBackground = iconBackground
        self.showsDivider = showsDivider
        self.trailing = trailing
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: MacTheme.rowSpacing) {
                if let icon, let bg = iconBackground {
                    MacSquircleIcon(systemName: icon, background: bg)
                }
                VStack(alignment: .leading, spacing: 2) {
                    HStack(spacing: 4) {
                        Text(label).font(.system(size: 13))
                        if let hint {
                            MacInfoButton(text: hint, url: hintURL, urlLabel: hintURLLabel)
                        }
                    }
                    if let description {
                        Text(description)
                            .font(.system(size: 11))
                            .foregroundStyle(Color.macTextSecondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer(minLength: 8)
                trailing()
            }
            .padding(.horizontal, MacTheme.rowHorizontalPadding)
            .padding(.vertical, MacTheme.rowVerticalPadding)
            .frame(minHeight: MacTheme.rowMinHeight)

            if showsDivider {
                Divider()
                    .background(Color.macRowDivider)
                    .padding(.leading, MacTheme.rowHorizontalPadding)
                    .opacity(0.6)
            }
        }
    }
}

// Convenience init when trailing is omitted
extension MacRow where Trailing == EmptyView {
    init(
        _ label: String,
        description: String? = nil,
        hint: String? = nil,
        hintURL: URL? = nil,
        hintURLLabel: String? = nil,
        icon: String? = nil,
        iconBackground: Color? = nil,
        showsDivider: Bool = true
    ) {
        self.init(label,
                  description: description,
                  hint: hint,
                  hintURL: hintURL,
                  hintURLLabel: hintURLLabel,
                  icon: icon,
                  iconBackground: iconBackground,
                  showsDivider: showsDivider) {
            EmptyView()
        }
    }
}

// MARK: - Info button (inline (i) → popover)
//
// Mirrors the Windows SettingCard.Hint pattern (PR #95). A subtle 12pt
// `info.circle` button next to a row label. Click opens an NSPopover
// with the verbose copy (max width about 320) and an optional "Learn more"
// link. Hover surfaces the same copy via SwiftUI's `.help()` so
// keyboard-only / hover-only users get the explanation too.

struct MacInfoButton: View {
    let text: String
    var url: URL? = nil
    var urlLabel: String? = nil

    @State private var isOpen = false

    var body: some View {
        Button {
            isOpen.toggle()
        } label: {
            Image(systemName: "info.circle")
                .font(.system(size: 12, weight: .regular))
                .foregroundStyle(Color.macTextSecondary)
                .contentShape(Rectangle())
                .frame(width: 16, height: 16)
        }
        .buttonStyle(.plain)
        .help(text)
        .accessibilityLabel("Show details")
        .popover(isPresented: $isOpen, arrowEdge: .top) {
            VStack(alignment: .leading, spacing: 8) {
                Text(text)
                    .font(.system(size: 12))
                    .foregroundStyle(Color.primary)
                    .fixedSize(horizontal: false, vertical: true)
                if let url {
                    Link(urlLabel ?? "Open full guide \u{203A}", destination: url)
                        .font(.system(size: 12))
                }
            }
            .padding(EdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 14))
            .frame(width: 320, alignment: .topLeading)
            .fixedSize(horizontal: false, vertical: true)
        }
    }
}

// MARK: - Squircle icon
//
// 28×28 rounded square with a coloured background and a centred SF Symbol
// glyph in white, matches the sidebar nav icons and per-row leading icons.

struct MacSquircleIcon: View {
    let systemName: String
    let background: Color
    var size: CGFloat = MacTheme.navIconSize

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: MacTheme.navIconRadius, style: .continuous)
                .fill(
                    LinearGradient(
                        colors: [background.opacity(1.0), background.opacity(0.85)],
                        startPoint: .top,
                        endPoint: .bottom
                    )
                )
                .overlay(
                    RoundedRectangle(cornerRadius: MacTheme.navIconRadius, style: .continuous)
                        .strokeBorder(Color.white.opacity(0.35), lineWidth: 0.5)
                        .blendMode(.plusLighter)
                )
                .shadow(color: .black.opacity(0.2), radius: 1, x: 0, y: 1)
            Image(systemName: systemName)
                .font(.system(size: size * 0.5, weight: .semibold))
                .foregroundStyle(.white)
        }
        .frame(width: size, height: size)
    }
}

// MARK: - Hero card
//
// Used on Home and About, large rounded tile with text on the left and
// an arbitrary trailing visual (pill stage / app icon) on the right.

struct MacHero<Trailing: View>: View {
    let title: String
    let subtitle: String
    var actions: AnyView? = nil
    @ViewBuilder var trailing: () -> Trailing

    var body: some View {
        HStack(alignment: .center, spacing: 24) {
            VStack(alignment: .leading, spacing: 4) {
                Text(title)
                    .font(.system(size: 24, weight: .semibold, design: .default))
                    .tracking(-0.24)
                Text(subtitle)
                    .font(.system(size: 13))
                    .foregroundStyle(Color.macTextSecondary)
                    .lineSpacing(2)
                    .fixedSize(horizontal: false, vertical: true)
                if let actions {
                    actions.padding(.top, 10)
                }
            }
            Spacer(minLength: 0)
            trailing()
        }
        .padding(EdgeInsets(top: 22, leading: 24, bottom: 22, trailing: 24))
        .background(
            RoundedRectangle(cornerRadius: MacTheme.heroCornerRadius, style: .continuous)
                .fill(Color(nsColor: .windowBackgroundColor).opacity(0.6))
        )
        .overlay(
            RoundedRectangle(cornerRadius: MacTheme.heroCornerRadius, style: .continuous)
                .stroke(Color.macStrokeHairline, lineWidth: 0.5)
        )
        .shadow(color: .black.opacity(0.04), radius: 0.5, x: 0, y: 0.5)
    }
}

// MARK: - Stat tile
//
// Three-up grid on Home: words / speaking-time / time-saved.

struct MacStatTile: View {
    let value: String
    let label: String

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(value)
                .font(.system(size: 22, weight: .semibold))
                .tracking(-0.22)
            Text(label.uppercased())
                .font(.system(size: 11, weight: .medium))
                .tracking(0.55)
                .foregroundStyle(Color.macTextSecondary)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(EdgeInsets(top: 14, leading: 16, bottom: 14, trailing: 16))
        .background(
            RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                .fill(Color(nsColor: .windowBackgroundColor).opacity(0.6))
        )
        .overlay(
            RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                .stroke(Color.macStrokeHairline, lineWidth: 0.5)
        )
    }
}

// MARK: - Chip (style picker)
//
// Pill button used by the Output → Style picker. Coloured swatch on the
// left, label on the right; selected state inverts to accent fill.

struct MacChip: View {
    let label: String
    let color: Color
    let selected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 6) {
                Circle()
                    .fill(selected ? Color.white : color)
                    .frame(width: 6, height: 6)
                Text(label)
                    .font(.system(size: 12))
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 5)
            .foregroundStyle(selected ? Color.white : Color.primary)
            .background(
                Capsule()
                    .fill(selected ? Color.accentColor : Color(nsColor: .controlBackgroundColor))
            )
            .overlay(
                Capsule()
                    .stroke(
                        selected ? Color.accentColor : Color.macControlStroke,
                        lineWidth: 0.5
                    )
            )
        }
        .buttonStyle(.plain)
    }
}

// MARK: - Position picker (3×3 grid)
//
// Wallpaper-position picker style, eight valid corners + middle-center,
// or a subset depending on what the platform supports.

struct MacPositionPicker: View {
    @Binding var selection: String
    var allowedPositions: Set<String> = [
        "Top Left", "Top Center", "Top Right",
        "Bottom Left", "Bottom Center", "Bottom Right",
    ]

    private let layout: [[String]] = [
        ["Top Left", "Top Center", "Top Right"],
        ["Middle Left", "Middle Center", "Middle Right"],
        ["Bottom Left", "Bottom Center", "Bottom Right"],
    ]

    var body: some View {
        VStack(spacing: 4) {
            ForEach(layout, id: \.self) { row in
                HStack(spacing: 4) {
                    ForEach(row, id: \.self) { id in
                        cell(for: id)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func cell(for id: String) -> some View {
        let allowed = allowedPositions.contains(id)
        let selected = selection == id
        Button {
            if allowed { selection = id }
        } label: {
            ZStack {
                // Base surface: solid system background that adapts
                // to dark + a primary-opacity tint on top so the
                // cell stays visibly distinct from the parent tile
                // (the tile already uses windowBackgroundColor at
                // 0.6, so a plain controlBackgroundColor used to
                // disappear into it on dark mode).
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(selected ? AnyShapeStyle(Color.accentColor)
                                   : AnyShapeStyle(Color(nsColor: .controlBackgroundColor)))
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(Color.primary.opacity(selected ? 0 : 0.10))
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .stroke(
                        selected ? Color.accentColor : Color.macControlStrokeStrong,
                        lineWidth: selected ? 1.2 : 0.8
                    )
            }
            .opacity(allowed ? 1.0 : 0.20)
            .frame(width: 24, height: 24)
        }
        .buttonStyle(.plain)
        .disabled(!allowed)
    }
}

// MARK: - Keycap
//
// `⌃` `⌥` etc rendered as a small Tahoe-shaped key for the Shortcut page
// and the Home hero "Hold ⌃+⌥ anywhere" hint.

struct MacKeycap: View {
    let glyph: String

    var body: some View {
        Text(glyph)
            .font(.system(size: 12, weight: .medium))
            .frame(minWidth: 26, minHeight: 26)
            .padding(.horizontal, 7)
            .background(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(Color(nsColor: .controlBackgroundColor))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .stroke(Color.macControlStrokeStrong, lineWidth: 0.5)
            )
    }
}

// MARK: - Inline status pill
//
// Small "● Listening, groq whisper" pill rendered in the toolbar to show
// current STT provider. Reused for "Live detection" on App Rules page.

struct MacStatusPill: View {
    let text: String
    var dotColor: Color = .green

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(dotColor)
                .frame(width: 6, height: 6)
                .shadow(color: dotColor.opacity(0.6), radius: 3, x: 0, y: 0)
            Text(text)
                .font(.system(size: 12))
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 4)
        .foregroundStyle(Color.macTextSecondary)
        .background(
            Capsule()
                .fill(.ultraThinMaterial)
        )
        .overlay(
            Capsule()
                .stroke(Color.primary.opacity(0.2), lineWidth: 0.5)
        )
    }
}

// MARK: - Note (info banner)
//
// Used on Privacy and App Rules pages, accent-circled icon + "**Title**\nbody".

struct MacNote: View {
    let title: String
    let message: String
    var systemImage: String = "info.circle.fill"

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Circle()
                .fill(Color.accentColor)
                .frame(width: 22, height: 22)
                .overlay(
                    Image(systemName: systemImage)
                        .font(.system(size: 11, weight: .bold))
                        .foregroundStyle(.white)
                )
            VStack(alignment: .leading, spacing: 2) {
                Text(title)
                    .font(.system(size: 12, weight: .semibold))
                Text(message)
                    .font(.system(size: 12))
                    .foregroundStyle(Color.macTextSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 0)
        }
        .padding(EdgeInsets(top: 12, leading: 14, bottom: 12, trailing: 14))
        .background(
            RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                .fill(Color(nsColor: .windowBackgroundColor).opacity(0.6))
        )
        .overlay(
            RoundedRectangle(cornerRadius: MacTheme.tileCornerRadius, style: .continuous)
                .stroke(Color.macStrokeHairline, lineWidth: 0.5)
        )
    }
}

// MARK: - Saved-pulse badge
//
// Brief "✓ Saved" badge that fades in/out next to the toolbar after any
// settings change. Kept as a local component because the timing is the
// same across every page that mutates AppState.

struct MacSavedPulse: View {
    var body: some View {
        HStack(spacing: 4) {
            ZStack {
                Circle().fill(Color.green).frame(width: 12, height: 12)
                Image(systemName: "checkmark")
                    .font(.system(size: 8, weight: .bold))
                    .foregroundStyle(.white)
            }
            Text("Saved")
                .font(.system(size: 11))
                .foregroundStyle(Color.macTextSecondary)
        }
    }
}

// MARK: - LLM Style → colour mapping
//
// Mirrors the Windows StyleToColorBrushConverter so the dot colour next
// to each style chip matches across platforms. Kept inline rather than
// extending `LlmStyle` so the design tokens sit alongside the other Mac
// atoms, `LlmStyle.color` already exists for the pill but uses slightly
// different hues; this enum keeps the design-bundle palette specifically.

enum MacStyleColor {
    static func color(for style: String) -> Color {
        switch style.lowercased() {
        case "off":            return Color(red: 0.62, green: 0.62, blue: 0.62)
        case "correct":        return Color(red: 0.26, green: 0.65, blue: 0.96)
        case "summarize":      return Color(red: 0.15, green: 0.78, blue: 0.85)
        case "elaborate":      return Color(red: 0.40, green: 0.73, blue: 0.42)
        case "comprehensible": return Color(red: 0.67, green: 0.28, blue: 0.74)
        case "professional":   return Color(red: 0.12, green: 0.53, blue: 0.90)
        case "prompt":         return Color(red: 0.49, green: 0.34, blue: 0.76)
        case "genz":           return Color(red: 0.92, green: 0.25, blue: 0.48)
        case "boomer":         return Color(red: 0.55, green: 0.43, blue: 0.39)
        case "emoji":          return Color(red: 1.00, green: 0.70, blue: 0.00)
        case "acronyms":       return Color(red: 1.00, green: 0.44, blue: 0.26)
        case "imbruttito":     return Color(red: 0.85, green: 0.26, blue: 0.08)
        case "custom":         return Color(red: 0.36, green: 0.42, blue: 0.75)
        default:               return Color(red: 0.62, green: 0.62, blue: 0.62)
        }
    }
}

// MARK: - LLM Style display + ordering
//
// The chip list uses this sorted list so every page renders styles in
// the same order as the design bundle. Maps style key → display label.

let MacLlmStyles: [(key: String, label: String)] = [
    ("off", "Off"),
    ("correct", "Correct"),
    ("summarize", "Summarize"),
    ("elaborate", "Elaborate"),
    ("comprehensible", "Comprehensible"),
    ("professional", "Professional"),
    ("prompt", "Prompt"),
    ("genz", "Gen Z"),
    ("boomer", "Boomer"),
    ("emoji", "Emoji"),
    ("acronyms", "Acronyms"),
    ("imbruttito", "Imbruttito"),
    ("custom", "Custom"),
]
