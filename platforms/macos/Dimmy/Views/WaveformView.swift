import SwiftUI

// MARK: - Waveform styles

enum WaveformStyle: String {
    case bars = "Bars"
    case barsCenter = "Bars Center"
    case barsRound = "Bars Round"
    case line = "Line"
    case dots = "Dots"

    static func from(_ string: String) -> WaveformStyle {
        WaveformStyle(rawValue: string) ?? .bars
    }
}

struct WaveformView: View {
    let levels: [CGFloat]
    var style: WaveformStyle = .bars
    let barCount: Int
    /// Round recording pill: three bars (tall centre, low sides) or three
    /// dots, close together and centred on both axes, or a short line.
    var compact: Bool = false

    init(levels: [CGFloat], style: WaveformStyle = .bars, barCount: Int = 7, compact: Bool = false) {
        self.levels = levels
        self.style = style
        self.barCount = barCount
        self.compact = compact
    }

    // Ink follows the appearance: white on the dark pill, near-black on the
    // light one (mirror of Win WaveformControl.SetInk).
    private var ink: Color { Color.primary.opacity(compact ? 1.0 : 0.9) }

    var body: some View {
        Group {
            switch style {
            case .bars:
                barsView(centered: compact, round: false)
            case .barsCenter:
                barsView(centered: true, round: false)
            case .barsRound:
                barsView(centered: compact, round: true)
            case .line:
                lineView
            case .dots:
                dotsView
            }
        }
        .frame(height: 18)
        .animation(.easeOut(duration: 0.2), value: levels)
    }

    /// Bars / dots on screen, with the weight of each. Compact keeps the
    /// three central ones; the sides follow the voice at a quarter.
    private var shown: [(index: Int, weight: CGFloat)] {
        guard compact else { return (0..<barCount).map { ($0, 1.0) } }
        let mid = barCount / 2
        return [(mid - 1, 0.25), (mid, 1.0), (mid + 1, 0.25)]
    }

    private func level(_ index: Int) -> CGFloat {
        index >= 0 && index < levels.count ? levels[index] : 0.2
    }

    // MARK: - Bars

    private func barsView(centered: Bool, round: Bool) -> some View {
        HStack(alignment: centered ? .center : .bottom, spacing: compact ? 2 : 3) {
            ForEach(shown, id: \.index) { item in
                RoundedRectangle(cornerRadius: round ? 2.5 : 1.5)
                    .fill(ink)
                    .frame(width: round ? 4 : 3, height: barHeight(level(item.index) * item.weight))
            }
        }
    }

    // MARK: - Line

    private var lineView: some View {
        GeometryReader { geo in
            Path { path in
                let width = geo.size.width
                let height = geo.size.height
                let step = width / CGFloat(max(barCount - 1, 1))

                for i in 0..<barCount {
                    let x = step * CGFloat(i)
                    let y = height * (1.0 - level(i))
                    if i == 0 {
                        path.move(to: CGPoint(x: x, y: y))
                    } else {
                        path.addLine(to: CGPoint(x: x, y: y))
                    }
                }
            }
            .stroke(ink, style: StrokeStyle(lineWidth: compact ? 2.5 : 2, lineCap: .round, lineJoin: .round))
        }
        .frame(width: compact ? 20 : nil)
    }

    // MARK: - Dots

    private var dotsView: some View {
        HStack(alignment: .center, spacing: compact ? 2 : 5) {
            ForEach(shown, id: \.index) { item in
                let lv = level(item.index) * (compact && item.weight < 1 ? 0.4 : 1)
                let size = 3.0 + lv * 7.0 // 3–10pt
                Circle()
                    .fill(Color.primary.opacity(compact ? 1.0 : 0.7 + lv * 0.3))
                    .frame(width: size, height: size)
            }
        }
    }

    private func barHeight(_ level: CGFloat) -> CGFloat {
        let minHeight: CGFloat = 3
        let maxHeight: CGFloat = 16
        return minHeight + (maxHeight - minHeight) * level
    }
}

struct WaveformIconView: View {
    var body: some View {
        HStack(spacing: 3) {
            ForEach(0..<3, id: \.self) { index in
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(Color.secondary)
                    .frame(width: 3, height: [8, 14, 8][index])
            }
        }
    }
}
