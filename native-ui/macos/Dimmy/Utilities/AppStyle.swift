import SwiftUI

/// Global UI scale factor — change this single value to make everything bigger/smaller
enum AppStyle {
    static let scaleFactor: CGFloat = 1.15

    // Typography
    static let titleSize: CGFloat = 28 * scaleFactor
    static let headlineSize: CGFloat = 18 * scaleFactor
    static let bodySize: CGFloat = 14 * scaleFactor
    static let captionSize: CGFloat = 12 * scaleFactor
    static let smallSize: CGFloat = 11 * scaleFactor

    // Spacing
    static let paddingSmall: CGFloat = 8 * scaleFactor
    static let paddingMedium: CGFloat = 14 * scaleFactor
    static let paddingLarge: CGFloat = 20 * scaleFactor

    // Corner radius
    static let cornerRadius: CGFloat = 12 * scaleFactor
    static let cornerRadiusSmall: CGFloat = 8 * scaleFactor
}

/// Apply to any view to bump up its control size globally
struct BoldUIModifier: ViewModifier {
    func body(content: Content) -> some View {
        content
            .controlSize(.large)
    }
}

extension View {
    func boldUI() -> some View {
        modifier(BoldUIModifier())
    }
}
