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

    init(levels: [CGFloat], style: WaveformStyle = .bars, barCount: Int = 7) {
        self.levels = levels
        self.style = style
        self.barCount = barCount
    }

    var body: some View {
        Group {
            switch style {
            case .bars:
                barsView(centered: false, round: false)
            case .barsCenter:
                barsView(centered: true, round: false)
            case .barsRound:
                barsView(centered: false, round: true)
            case .line:
                lineView
            case .dots:
                dotsView
            }
        }
        .frame(height: 18)
        .animation(.easeOut(duration: 0.2), value: levels)
    }

    // MARK: - Bars

    private func barsView(centered: Bool, round: Bool) -> some View {
        HStack(alignment: centered ? .center : .bottom, spacing: 3) {
            ForEach(0..<barCount, id: \.self) { index in
                RoundedRectangle(cornerRadius: round ? 2.5 : 1.5)
                    .fill(Color.white.opacity(0.9))
                    .frame(width: round ? 4 : 3, height: barHeight(for: index))
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
                    let level = i < levels.count ? levels[i] : 0.2
                    let x = step * CGFloat(i)
                    let y = height * (1.0 - level)
                    if i == 0 {
                        path.move(to: CGPoint(x: x, y: y))
                    } else {
                        path.addLine(to: CGPoint(x: x, y: y))
                    }
                }
            }
            .stroke(Color.white.opacity(0.9), style: StrokeStyle(lineWidth: 2, lineCap: .round, lineJoin: .round))
        }
    }

    // MARK: - Dots

    private var dotsView: some View {
        HStack(alignment: .center, spacing: 5) {
            ForEach(0..<barCount, id: \.self) { index in
                let level = index < levels.count ? levels[index] : 0.2
                let size = 3.0 + level * 7.0 // 3–10pt
                Circle()
                    .fill(Color.white.opacity(0.7 + level * 0.3))
                    .frame(width: size, height: size)
            }
        }
    }

    private func barHeight(for index: Int) -> CGFloat {
        let level = index < levels.count ? levels[index] : 0.2
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
