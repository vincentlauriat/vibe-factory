import Foundation

/// Text forms shared by the views, the same as the other interfaces
/// (`vibe history`, the TUI, the web UI).
public enum Format {
    public static func short(_ text: String, _ limit: Int) -> String {
        text.count <= limit ? text : String(text.prefix(limit)) + "…"
    }

    /// 950, 12.3k, 1.2M.
    public static func tokens(_ value: UInt64) -> String {
        switch value {
        case ..<1_000: return "\(value)"
        case ..<1_000_000: return String(format: "%.1fk", Double(value) / 1_000)
        default: return String(format: "%.1fM", Double(value) / 1_000_000)
        }
    }

    /// 42 s, 3 min 10 s, 1 h 05 min.
    public static func duration(ms: UInt64) -> String {
        let seconds = Int(ms / 1_000)
        if seconds < 60 { return "\(seconds) s" }
        if seconds < 3_600 { return "\(seconds / 60) min \(String(format: "%02d", seconds % 60)) s" }
        return "\(seconds / 3_600) h \(String(format: "%02d", seconds / 60 % 60)) min"
    }

    /// `value+` when `complete` is false: a lower bound.
    public static func lowerBound(_ value: String, complete: Bool) -> String {
        complete ? value : value + "+"
    }
}
