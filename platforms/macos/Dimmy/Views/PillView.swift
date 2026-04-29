import SwiftUI

// MARK: - Border style helpers

enum BorderStyle {
    case rainbow, blue, green, purple, orange, none

    static func from(_ string: String) -> BorderStyle {
        switch string.lowercased() {
        case "rainbow": return .rainbow
        case "blue", "blue pulse": return .blue
        case "green": return .green
        case "purple": return .purple
        case "orange": return .orange
        case "none": return .none
        default: return .rainbow
        }
    }

    func borderStroke(phase: Double) -> some ShapeStyle {
        // This method is not used directly — see borderView helper below
        AnyShapeStyle(Color.clear)
    }

    var solidColor: Color {
        switch self {
        case .rainbow: return .white // not used for rainbow
        case .blue: return .blue
        case .green: return .green
        case .purple: return .purple
        case .orange: return .orange
        case .none: return .clear
        }
    }

    var glowColor: Color {
        switch self {
        case .rainbow: return .white // will use phase-based glow
        case .blue: return .blue
        case .green: return .green
        case .purple: return .purple
        case .orange: return .orange
        case .none: return .clear
        }
    }
}

// MARK: - Translate-to language list (kept in sync with MacOutputPage / Windows)

/// (config key, two-letter pill label, full display name).
/// Empty key = no translation, shown as `—` to give the user a hit target
/// to scroll back to off without making the indicator disappear.
let PillTranslateLanguages: [(key: String, short: String, display: String)] = [
    ("",   "—",  "No translation"),
    ("it", "IT", "Italiano"),
    ("en", "EN", "English"),
    ("es", "ES", "Español"),
    ("fr", "FR", "Français"),
    ("de", "DE", "Deutsch"),
    ("pt", "PT", "Português"),
]

// MARK: - Scroll wheel detector
//
// SwiftUI on macOS doesn't expose mouse-wheel events at the View level,
// so we drop in a tiny NSView that forwards scrollWheel events upward.
// Keep it transparent and overlay it on the target hit area.

private struct ScrollWheelCatcher: NSViewRepresentable {
    let onScroll: (CGFloat) -> Void

    func makeNSView(context: Context) -> ScrollableNSView {
        let v = ScrollableNSView()
        v.onScroll = onScroll
        return v
    }

    func updateNSView(_ view: ScrollableNSView, context: Context) {
        view.onScroll = onScroll
    }

    final class ScrollableNSView: NSView {
        var onScroll: ((CGFloat) -> Void)?

        override func scrollWheel(with event: NSEvent) {
            // Use precise (trackpad) delta when available, fall back to deltaY.
            let dy = event.hasPreciseScrollingDeltas
                ? event.scrollingDeltaY
                : event.deltaY
            if abs(dy) > 0.1 {
                onScroll?(dy)
            } else {
                super.scrollWheel(with: event)
            }
        }

        override func hitTest(_ point: NSPoint) -> NSView? {
            // Only intercept scroll — let clicks through to the SwiftUI layer.
            self
        }

        override func mouseDown(with event: NSEvent) { nextResponder?.mouseDown(with: event) }
        override func rightMouseDown(with event: NSEvent) { nextResponder?.rightMouseDown(with: event) }
    }
}

struct PillView: View {
    @ObservedObject var appState: AppState
    @State private var isHovering = false
    @State private var borderPhase: Double = 0
    @State private var showCheckmark = false
    @State private var introPhase: Double = 0

    /// Tooltip text shown briefly after a scroll-cycle on the language /
    /// style indicators. Cleared by `tooltipHideTask` after ~1.5s.
    @State private var scrollTooltip: String? = nil
    @State private var tooltipHideTask: Task<Void, Never>? = nil
    /// Throttle scroll-cycle events so a single trackpad swipe doesn't
    /// blow through 5 languages in one gesture (matches Windows 250ms).
    @State private var lastScrollAt: Date = .distantPast

    private let pillHeight: CGFloat = 36

    private var activeBorderStyle: BorderStyle {
        BorderStyle.from(appState.borderStyle)
    }

    private var activeWaveformStyle: WaveformStyle {
        WaveformStyle.from(appState.waveformStyle)
    }

    /// Where to anchor the (size-to-content) pill inside the larger panel.
    /// The panel is fixed-size so the glow/shadow has room; the pill itself
    /// is only as wide as its content. If we let it center inside the panel
    /// it visually drifts away from the chosen screen corner. Aligning the
    /// pill body to the matching corner pulls it flush against the edge.
    private var pillAlignment: Alignment {
        switch appState.overlayPosition {
        case "Top Left":      return .topLeading
        case "Top Center":    return .top
        case "Top Right":     return .topTrailing
        case "Bottom Left":   return .bottomLeading
        case "Bottom Center": return .bottom
        case "Bottom Right":  return .bottomTrailing
        default:              return .bottomTrailing
        }
    }

    var body: some View {
        ZStack(alignment: pillAlignment) {
            // Transparent spacer so the ZStack expands to fill the panel —
            // otherwise it sizes to its content and the alignment is moot.
            Color.clear
            switch appState.recordingState {
            case .idle:
                idleView
            case .recording(let mode):
                recordingView(mode: mode)
            case .transcribing:
                transcribingView
            case .processing:
                processingView
            case .completing:
                completionView
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        // Floating tooltip shown briefly after scroll-cycle on the language
        // or style indicator. Renders below the pill (most users keep the
        // pill near the top of the screen) so it doesn't get clipped by the
        // panel's glow padding budget. The fixedSize lets the bubble grow
        // wider than the pill itself when the language name is long.
        .overlay(alignment: .bottom) {
            if let text = scrollTooltip {
                Text(text)
                    .font(.system(size: 11, weight: .medium))
                    .foregroundColor(.white)
                    .lineLimit(1)
                    .fixedSize()
                    .padding(.horizontal, 10)
                    .padding(.vertical, 5)
                    .background(
                        Capsule().fill(Color.black.opacity(0.82))
                    )
                    .offset(y: 32)
                    .transition(.opacity.combined(with: .move(edge: .top)))
                    .allowsHitTesting(false)
            }
        }
        // Context menu is handled by PillHostingView (NSMenu) — not SwiftUI .contextMenu
        // which doesn't work on borderless NSPanel
        .onChange(of: appState.showPillIntro) { _, show in
            if show {
                introPhase = 0
                withAnimation(.linear(duration: 1.5).repeatForever(autoreverses: false)) {
                    introPhase = 360
                }
                DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) {
                    withAnimation(.easeOut(duration: 0.5)) {
                        appState.showPillIntro = false
                        introPhase = 0
                    }
                }
            }
        }
    }

    // MARK: - Idle State

    private var idleView: some View {
        HStack(spacing: 8) {
            styleDot
            languageLabel
            // Waveform icon hidden in idle — only meaningful while we're
            // actually capturing audio. Recording state has the live bars.
            if isHovering {
                Text(appState.shortcut.displayString)
                    .font(.system(size: 11, weight: .medium, design: .monospaced))
                    .foregroundColor(.secondary)
            }
        }
        .frame(height: pillHeight)
        .padding(.horizontal, isHovering ? 16 : 14)
        // Warning badge sits on the pill itself (not on the larger panel),
        // so it follows the pill wherever the position grid moves it.
        // Borderless: just the orange triangle, no black backdrop.
        .overlay(alignment: .topTrailing) {
            if appState.hotkeyStatus != .installed {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 11, weight: .bold))
                    .foregroundColor(.orange)
                    .offset(x: 2, y: -2)
                    .help(Self.warningText(for: appState.hotkeyStatus))
            }
        }
        .background(
            Capsule()
                .fill(.ultraThinMaterial)
                .opacity(isHovering ? 0.95 : 0.5)
        )
        .overlay(
            Capsule()
                .stroke(
                    appState.showPillIntro ? rainbowGradient(phase: introPhase) : rainbowGradient(phase: 0),
                    lineWidth: appState.showPillIntro ? 2 : 0.5
                )
                .opacity(appState.showPillIntro ? 1.0 : 0.12)
        )
        .shadow(color: appState.showPillIntro ? phaseGlowColor(phase: introPhase, offset: 0.0) : .clear, radius: 12)
        .shadow(color: appState.showPillIntro ? phaseGlowColor(phase: introPhase, offset: 0.3) : .clear, radius: 8)
        .shadow(color: appState.showPillIntro ? phaseGlowColor(phase: introPhase, offset: 0.6) : .clear, radius: 4)
        .onHover { hovering in
            withAnimation(.easeInOut(duration: 0.2)) {
                isHovering = hovering
            }
        }
    }

    // MARK: - Idle indicators (style dot + language label) — both scroll-cyclable

    /// Always-visible style indicator. When the rewrite is off it stays as
    /// a hollow ring so the user has a hit target to scroll back into a
    /// non-off style. Scroll cycles `llm_style`.
    private var styleDot: some View {
        let active = appState.llmEnabled && appState.llmStyleEnum != .off
        let color = active ? appState.llmStyleEnum.color : Color.secondary.opacity(0.5)
        return Circle()
            .fill(active ? color : Color.clear)
            .overlay(
                Circle().stroke(color, lineWidth: active ? 0 : 1.2)
            )
            .frame(width: 8, height: 8)
            .overlay(
                ScrollWheelCatcher { delta in cycleStyle(delta: delta) }
                    .frame(width: 22, height: 22)
            )
            .help(currentStyleDisplayName)
    }

    /// Always-visible "translate to" indicator. Empty translation shows
    /// `—` (matches Windows) so there's always a hit target to scroll
    /// from when re-enabling translation. Borderless — sits flush in the
    /// pill body.
    private var languageLabel: some View {
        Text(currentLanguageShort)
            .font(.system(size: 11, weight: .semibold, design: .rounded))
            .foregroundColor(.primary)
            .frame(minWidth: 18)
            .overlay(
                ScrollWheelCatcher { delta in cycleLanguage(delta: delta) }
            )
            .help("Translate to: \(currentLanguageDisplayName) — scroll to change")
    }

    private var currentLanguageShort: String {
        let key = appState.llmTranslateTo
        return PillTranslateLanguages.first(where: { $0.key == key })?.short ?? "—"
    }

    private var currentLanguageDisplayName: String {
        let key = appState.llmTranslateTo
        return PillTranslateLanguages.first(where: { $0.key == key })?.display ?? "No translation"
    }

    private var currentStyleDisplayName: String {
        MacLlmStyles.first(where: { $0.key == appState.llmStyle })?.label ?? appState.llmStyle.capitalized
    }

    // MARK: - Scroll cycling (debounced 250ms, persists via FFI)

    private func cycleLanguage(delta: CGFloat) {
        guard throttle() else { return }
        let keys = PillTranslateLanguages.map { $0.key }
        let current = keys.firstIndex(of: appState.llmTranslateTo) ?? 0
        let step = delta > 0 ? -1 : 1
        let next = (current + step + keys.count) % keys.count
        appState.llmTranslateTo = keys[next]
        DimmyCore.shared.setConfig(["llm_translate_to": keys[next]])
        showTooltip("Translate to: \(currentLanguageDisplayName)")
    }

    private func cycleStyle(delta: CGFloat) {
        guard throttle() else { return }
        let keys = MacLlmStyles.map { $0.key }
        let current = keys.firstIndex(of: appState.llmStyle) ?? 0
        let step = delta > 0 ? -1 : 1
        let next = (current + step + keys.count) % keys.count
        let key = keys[next]
        appState.llmStyle = key
        appState.llmEnabled = key != "off"
        DimmyCore.shared.setConfig([
            "llm_style": key,
            "llm_enabled": key != "off",
        ])
        showTooltip("Style: \(currentStyleDisplayName)")
    }

    private func throttle() -> Bool {
        let now = Date()
        if now.timeIntervalSince(lastScrollAt) < 0.25 { return false }
        lastScrollAt = now
        return true
    }

    private func showTooltip(_ text: String) {
        tooltipHideTask?.cancel()
        withAnimation(.easeOut(duration: 0.18)) { scrollTooltip = text }
        tooltipHideTask = Task { @MainActor in
            try? await Task.sleep(nanoseconds: 1_500_000_000)
            if !Task.isCancelled {
                withAnimation(.easeIn(duration: 0.25)) { scrollTooltip = nil }
            }
        }
    }

    // MARK: - Recording State (uses borderStyle from settings)

    private func recordingView(mode: RecordingMode) -> some View {
        HStack(spacing: 12) {
            WaveformView(levels: appState.waveformLevels, style: activeWaveformStyle)

            if mode == .toggle {
                Button(action: {
                    HotkeyManager.shared.stopToggleRecording()
                }) {
                    RoundedRectangle(cornerRadius: 2.5)
                        .fill(Color.white.opacity(0.9))
                        .frame(width: 12, height: 12)
                }
                .buttonStyle(.plain)
                .contentShape(Rectangle())
            }
        }
        .frame(height: pillHeight)
        .padding(.horizontal, 18)
        .background(
            Capsule()
                .fill(.ultraThickMaterial)
        )
        .overlay(recordingBorderOverlay)
        .shadow(color: recordingGlowColor(offset: 0.0), radius: 12)
        .shadow(color: recordingGlowColor(offset: 0.3), radius: 8)
        .shadow(color: recordingGlowColor(offset: 0.6), radius: 4)
        .onAppear {
            borderPhase = 0
            withAnimation(.linear(duration: 2.5).repeatForever(autoreverses: false)) {
                borderPhase = 360
            }
        }
        .onDisappear {
            borderPhase = 0
        }
    }

    /// Border overlay that respects the selected border style
    @ViewBuilder
    private var recordingBorderOverlay: some View {
        let style = activeBorderStyle
        switch style {
        case .rainbow:
            Capsule()
                .stroke(rainbowGradient(phase: borderPhase), lineWidth: 2)
        case .none:
            Capsule()
                .stroke(Color.clear, lineWidth: 0)
        default:
            // Solid color with pulsing opacity
            Capsule()
                .stroke(style.solidColor.opacity(0.5 + 0.5 * sin(borderPhase * .pi / 180.0)), lineWidth: 2)
        }
    }

    /// Glow color for recording, respecting border style
    private func recordingGlowColor(offset: Double) -> Color {
        let style = activeBorderStyle
        switch style {
        case .rainbow:
            return phaseGlowColor(phase: borderPhase, offset: offset)
        case .none:
            return .clear
        default:
            let pulse = 0.2 + 0.2 * sin(borderPhase * .pi / 180.0 + offset * .pi * 2)
            return style.glowColor.opacity(pulse)
        }
    }

    // MARK: - Transcribing State

    private var transcribingView: some View {
        HStack(spacing: 8) {
            ProgressView()
                .controlSize(.small)
                .scaleEffect(0.7)
            Text("Transcribing...")
                .font(.system(size: 11, weight: .medium))
                .foregroundColor(.secondary)
                .lineLimit(1)
                .fixedSize()
        }
        .frame(height: pillHeight)
        .padding(.horizontal, 14)
        .background(
            Capsule()
                .fill(.ultraThickMaterial)
        )
        .overlay(
            Capsule()
                .stroke(Color.blue.opacity(0.4), lineWidth: 1.5)
        )
        .shadow(color: .blue.opacity(0.3), radius: 8)
    }

    // MARK: - LLM Processing State

    private var processingView: some View {
        HStack(spacing: 8) {
            ProgressView()
                .controlSize(.small)
                .scaleEffect(0.7)
            Text("Processing...")
                .font(.system(size: 11, weight: .medium))
                .foregroundColor(.secondary)
                .lineLimit(1)
                .fixedSize()
        }
        .frame(height: pillHeight)
        .padding(.horizontal, 14)
        .background(
            Capsule()
                .fill(.ultraThickMaterial)
        )
        .overlay(
            Capsule()
                .stroke(Color.purple.opacity(0.4), lineWidth: 1.5)
        )
        .shadow(color: .purple.opacity(0.3), radius: 8)
    }

    // MARK: - Completion State

    private var completionView: some View {
        Image(systemName: "checkmark")
            .font(.system(size: 16, weight: .bold))
            .foregroundColor(.green)
            .scaleEffect(showCheckmark ? 1.0 : 0.3)
            .opacity(showCheckmark ? 1.0 : 0.0)
            .frame(height: pillHeight)
            .padding(.horizontal, 16)
            .background(
                Capsule()
                    .fill(.ultraThickMaterial)
            )
            .overlay(
                Capsule()
                    .stroke(Color.green.opacity(0.4), lineWidth: 1.5)
            )
            .shadow(color: .green.opacity(0.4), radius: 10)
            .onAppear {
                withAnimation(.spring(response: 0.25, dampingFraction: 0.6)) {
                    showCheckmark = true
                }
            }
            .onDisappear {
                showCheckmark = false
            }
    }

    // MARK: - Rainbow gradient helpers

    private func rainbowGradient(phase: Double) -> AngularGradient {
        AngularGradient(
            gradient: Gradient(colors: rainbowColors),
            center: .center,
            angle: .degrees(phase)
        )
    }

    /// Phase-based rotating glow (used for rainbow style)
    private func phaseGlowColor(phase: Double, offset: Double) -> Color {
        let hue = ((phase / 360.0) + offset).truncatingRemainder(dividingBy: 1.0)
        return Color(hue: hue, saturation: 0.6, brightness: 1.0).opacity(0.4)
    }

    private static func warningText(for status: HotkeyStatus) -> String {
        switch status {
        case .installed, .uninstalled: return ""
        case .accessibilityMissing: return "Shortcut disabled: grant Accessibility in System Settings"
        case .tapFailed(let reason): return "Shortcut disabled: \(reason)"
        }
    }

    private var rainbowColors: [Color] {
        [
            Color(hue: 0.0, saturation: 0.7, brightness: 1.0),
            Color(hue: 0.08, saturation: 0.8, brightness: 1.0),
            Color(hue: 0.15, saturation: 0.7, brightness: 1.0),
            Color(hue: 0.35, saturation: 0.7, brightness: 0.95),
            Color(hue: 0.52, saturation: 0.6, brightness: 1.0),
            Color(hue: 0.62, saturation: 0.7, brightness: 1.0),
            Color(hue: 0.75, saturation: 0.6, brightness: 1.0),
            Color(hue: 0.85, saturation: 0.6, brightness: 1.0),
            Color(hue: 0.95, saturation: 0.7, brightness: 1.0),
        ]
    }
}

extension RecordingState {
    var animationId: Int {
        switch self {
        case .idle: return 0
        case .recording(.pushToTalk): return 1
        case .recording(.toggle): return 2
        case .transcribing: return 3
        case .processing: return 4
        case .completing: return 5
        }
    }
}
