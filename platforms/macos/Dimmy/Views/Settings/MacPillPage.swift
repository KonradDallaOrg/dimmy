import SwiftUI

// Pill overlay, live preview + 3×3 position picker + appearance.
// Live preview is a static SwiftUI mock (NOT the real PillView so we don't
// risk firing a real recording from inside Settings) that reflects the
// user's current border-style + waveform-style choices in real time.

struct MacPillPage: View {
    @ObservedObject var appState: AppState
    @State private var previewState: String = "idle"

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            // Live-preview pill mock removed per user request 2026-06-01:
            // it ate vertical space on every Settings open while only
            // really serving the *appearance* knobs at the bottom of
            // the page. Border + Waveform pickers already preview
            // themselves on the *real* pill once the user picks an
            // option. Keep the page focused on actual toggles.
            positionGroup
            visibilityGroup
            appearanceGroup
        }
    }

    // MARK: Visibility

    private var visibilityGroup: some View {
        Group {
            MacGroupLabel(text: "Visibility")
            MacTile {
                MacRow(
                    "Show in Dock",
                    hint: "When off, Dimmy disappears from the Dock and Cmd-Tab. The global hotkey still works.",
                    hintURL: URL(string: "https://dimmy.app/help/tray-menu")
                ) {
                    Toggle("", isOn: $appState.showInDock)
                        .toggleStyle(.switch)
                        .labelsHidden()
                }
                MacRow(
                    "Show in menu bar",
                    hint: "When off, the menu-bar icon at the top-right disappears. At least one of Dock or menu bar must stay on.",
                    hintURL: URL(string: "https://dimmy.app/help/tray-menu"),
                    showsDivider: false
                ) {
                    Toggle("", isOn: $appState.showInMenuBar)
                        .toggleStyle(.switch)
                        .labelsHidden()
                }
            }
        }
    }

    // MARK: Live preview

    private var livePreviewGroup: some View {
        Group {
            MacGroupLabel(text: "Live preview")
            previewStage
                .padding(.bottom, 8)
            stateChips
                .padding(.bottom, 8)
        }
    }

    private var previewStage: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(
                    LinearGradient(
                        colors: [Color(red: 0.06, green: 0.07, blue: 0.10),
                                 Color(red: 0.12, green: 0.13, blue: 0.18)],
                        startPoint: .topLeading,
                        endPoint: .bottomTrailing
                    )
                )
            previewPill
            VStack {
                Spacer()
                HStack {
                    Spacer()
                    Text("Preview · \(previewState)")
                        .font(.system(size: 11, weight: .medium))
                        .tracking(0.55)
                        .foregroundStyle(Color.white.opacity(0.4))
                        .padding(.trailing, 12)
                        .padding(.bottom, 8)
                }
            }
        }
        .frame(height: 140)
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .stroke(Color.macStrokeHairline, lineWidth: 0.5)
        )
    }

    private var previewPill: some View {
        Capsule()
            .fill(Color(red: 0.10, green: 0.10, blue: 0.10))
            .frame(width: 200, height: 56)
            .overlay(previewPillContent)
            .overlay(
                Capsule().stroke(borderBrush, lineWidth: 2.5)
            )
    }

    @ViewBuilder
    private var previewPillContent: some View {
        switch previewState {
        case "recording", "transcribing":
            waveformContent
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .padding(.horizontal, 18)
        case "done":
            Image(systemName: "checkmark")
                .font(.system(size: 18, weight: .bold))
                .foregroundStyle(.green)
        case "error":
            Image(systemName: "exclamationmark.triangle.fill")
                .font(.system(size: 18))
                .foregroundStyle(.red)
        default: // idle, processing
            Image(systemName: "mic.fill")
                .font(.system(size: 16))
                .foregroundStyle(Color(white: 0.6))
        }
    }

    @ViewBuilder
    private var waveformContent: some View {
        let style = appState.waveformStyle.lowercased()
        switch style {
        case "line":
            // Sinuous line, Path with smooth bezier-ish polyline.
            Canvas { context, size in
                var path = Path()
                let amplitude: CGFloat = 8
                let waves = 4
                let step = size.width / CGFloat(waves * 2)
                path.move(to: CGPoint(x: 0, y: size.height / 2))
                for i in 0..<(waves * 2) {
                    let x = CGFloat(i + 1) * step
                    let y = size.height / 2 + (i % 2 == 0 ? -amplitude : amplitude)
                    path.addQuadCurve(
                        to: CGPoint(x: x, y: y),
                        control: CGPoint(x: x - step / 2, y: size.height / 2)
                    )
                }
                context.stroke(path, with: .color(Color(white: 0.7)), lineWidth: 1.5)
            }
        case "dots":
            HStack(spacing: 4) {
                ForEach([4, 6, 8, 5, 7, 4, 6], id: \.self) { size in
                    Circle()
                        .fill(Color(white: 0.7))
                        .frame(width: CGFloat(size), height: CGFloat(size))
                }
            }
        default:
            // Bars / Bars Center
            let centered = style == "bars center" || style == "bars centered"
            HStack(alignment: centered ? .center : .bottom, spacing: 3) {
                ForEach([10, 18, 24, 14, 22, 12, 20], id: \.self) { h in
                    RoundedRectangle(cornerRadius: 1.5, style: .continuous)
                        .fill(Color(white: 0.7))
                        .frame(width: 3, height: CGFloat(h))
                }
            }
        }
    }

    private var borderBrush: AnyShapeStyle {
        switch (previewState, appState.borderStyle.lowercased()) {
        case ("recording", _), ("idle", _) where appState.borderStyle.lowercased() != "none":
            return rainbowOrSolid(appState.borderStyle)
        case ("transcribing", _):
            return AnyShapeStyle(Color(red: 0.38, green: 0.65, blue: 0.98))
        case ("done", _):
            return AnyShapeStyle(Color.green)
        case ("error", _):
            return AnyShapeStyle(Color.red)
        default:
            return AnyShapeStyle(Color.gray.opacity(0.4))
        }
    }

    private func rainbowOrSolid(_ style: String) -> AnyShapeStyle {
        switch style.lowercased() {
        case "rainbow":
            return AnyShapeStyle(
                LinearGradient(
                    colors: [
                        Color(red: 0.43, green: 0.91, blue: 0.72),
                        Color(red: 0.38, green: 0.65, blue: 0.98),
                        Color(red: 0.96, green: 0.45, blue: 0.71),
                        Color(red: 0.98, green: 0.75, blue: 0.14),
                    ],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )
            )
        case "blue":   return AnyShapeStyle(Color(red: 0.38, green: 0.65, blue: 0.98))
        case "green":  return AnyShapeStyle(Color(red: 0.29, green: 0.87, blue: 0.50))
        case "purple": return AnyShapeStyle(Color(red: 0.66, green: 0.47, blue: 0.96))
        case "orange": return AnyShapeStyle(Color(red: 0.98, green: 0.75, blue: 0.14))
        case "none":   return AnyShapeStyle(Color.gray.opacity(0.4))
        default:       return AnyShapeStyle(Color.gray.opacity(0.4))
        }
    }

    private var stateChips: some View {
        HStack(spacing: 6) {
            ForEach(["idle", "recording", "transcribing", "processing", "done", "error"], id: \.self) { state in
                MacChip(
                    label: state,
                    color: chipColor(for: state),
                    selected: previewState == state
                ) {
                    previewState = state
                }
            }
        }
    }

    private func chipColor(for state: String) -> Color {
        switch state {
        case "recording":    return Color.red
        case "transcribing": return Color.blue
        case "processing":   return Color.purple
        case "done":         return Color.green
        case "error":        return Color.orange
        default:             return Color.gray
        }
    }

    // MARK: Position

    private var positionGroup: some View {
        Group {
            MacGroupLabel(text: "Position")
            MacTile {
                MacRow(
                    "Default position",
                    hint: "Where the pill appears when Dimmy launches or after Reset position. You can always drag it to a custom spot.",
                    hintURL: URL(string: "https://dimmy.app/help/pill-position")
                ) {
                    MacPositionPicker(
                        selection: Binding(
                            get: { appState.overlayPosition },
                            set: { newValue in
                                appState.overlayPosition = newValue
                                persistConfig()
                            }
                        ),
                        allowedPositions: [
                            "Top Left", "Top Center", "Top Right",
                            "Bottom Left", "Bottom Center", "Bottom Right",
                        ]
                    )
                }
                MacRow(
                    "Reset position",
                    description: "Snap back to default.",
                    showsDivider: false
                ) {
                    Button {
                        appState.overlayPosition = "Bottom Right"
                        appState.pillPosition = nil
                        persistConfig()
                    } label: {
                        Label("Reset", systemImage: "arrow.counterclockwise")
                    }
                    .controlSize(.small)
                }
            }
        }
    }

    // MARK: Appearance

    private var appearanceGroup: some View {
        Group {
            MacGroupLabel(text: "Appearance")
            MacTile {
                MacRow(
                    "Border style",
                    hint: "Color of the pill outline when idle. Recording, transcribing, done and error states use fixed status colors.",
                    hintURL: URL(string: "https://dimmy.app/help/pill-overview")
                ) {
                    Picker("", selection: Binding(
                        get: { appState.borderStyle },
                        set: { newValue in
                            appState.borderStyle = newValue
                            persistConfig()
                        }
                    )) {
                        Text("Rainbow").tag("Rainbow")
                        Text("Blue").tag("Blue")
                        Text("Green").tag("Green")
                        Text("Purple").tag("Purple")
                        Text("Orange").tag("Orange")
                        Text("None").tag("None")
                    }
                    .labelsHidden()
                    .frame(width: 160)
                }

                MacRow(
                    "Waveform style",
                    hint: "How sound is visualised inside the pill while recording. Bars are densest, Dots are subtlest.",
                    hintURL: URL(string: "https://dimmy.app/help/pill-states"),
                    showsDivider: false
                ) {
                    Picker("", selection: Binding(
                        get: { appState.waveformStyle },
                        set: { newValue in
                            appState.waveformStyle = newValue
                            persistConfig()
                        }
                    )) {
                        Text("Bars").tag("Bars")
                        Text("Bars Center").tag("Bars Center")
                        Text("Bars Round").tag("Bars Round")
                        Text("Line").tag("Line")
                        Text("Dots").tag("Dots")
                    }
                    .labelsHidden()
                    .frame(width: 160)
                }
            }
        }
    }

    private func persistConfig() {
        DimmyCore.shared.setConfig(appState.toRustConfig())
    }
}
