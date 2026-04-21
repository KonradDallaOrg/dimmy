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

struct PillView: View {
    @ObservedObject var appState: AppState
    @State private var isHovering = false
    @State private var borderPhase: Double = 0
    @State private var showCheckmark = false
    @State private var introPhase: Double = 0

    private let pillHeight: CGFloat = 36

    private var activeBorderStyle: BorderStyle {
        BorderStyle.from(appState.borderStyle)
    }

    private var activeWaveformStyle: WaveformStyle {
        WaveformStyle.from(appState.waveformStyle)
    }

    var body: some View {
        ZStack {
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
        .overlay(alignment: .topTrailing) {
            if appState.hotkeyStatus != .installed {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.system(size: 11, weight: .bold))
                    .foregroundColor(.orange)
                    .padding(6)
                    .background(Circle().fill(Color.black.opacity(0.6)))
                    .offset(x: 4, y: -4)
                    .help(Self.warningText(for: appState.hotkeyStatus))
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
        HStack(spacing: 10) {
            // LLM style dot indicator
            if appState.llmEnabled, appState.llmStyleEnum != .off {
                Circle()
                    .fill(appState.llmStyleEnum.color)
                    .frame(width: 6, height: 6)
            }
            Image(systemName: "waveform")
                .font(.system(size: 14, weight: .semibold))
                .foregroundColor(.secondary)
            if isHovering {
                Text(appState.selectedLanguage.isEmpty || appState.selectedLanguage == "Auto Detect" ? "Auto" : appState.selectedLanguage)
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundColor(.primary)
                Text(appState.shortcut.displayString)
                    .font(.system(size: 11, weight: .medium, design: .monospaced))
                    .foregroundColor(.secondary)
            }
        }
        .frame(height: pillHeight)
        .padding(.horizontal, isHovering ? 16 : 14)
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
