import Foundation
import VibeAPI

/// What a project window shows: a folder the app serves itself, or a server
/// started elsewhere (`vibe serve` in a terminal). The value restores the
/// window; a remote server's token stays in the Keychain, keyed by its URL.
enum ProjectRef: Codable, Hashable, Identifiable {
    case folder(path: String)
    case remote(url: String)

    var id: String {
        switch self {
        case .folder(let path): "folder:" + path
        case .remote(let url): "remote:" + url
        }
    }

    /// Folder name, or `host:port`.
    var displayName: String {
        switch self {
        case .folder(let path):
            return URL(fileURLWithPath: path).lastPathComponent
        case .remote(let url):
            guard let components = URLComponents(string: url), let host = components.host else { return url }
            return components.port.map { "\(host):\($0)" } ?? host
        }
    }

    /// Full path or URL, for tooltips and the recent list.
    var detail: String {
        switch self {
        case .folder(let path): (path as NSString).abbreviatingWithTildeInPath
        case .remote(let url): url
        }
    }

    /// Keychain account of a remote server's token.
    static func tokenAccount(for url: String) -> String { "server-token:" + url }
}

/// Sidebar entries of a project window.
enum SidebarItem: Hashable {
    case status(BoardColumn)
    case activity
    case history
    case evaluations
}

/// A column of the board: one task status, or every status the app does not know.
enum BoardColumn: Hashable, CaseIterable {
    case backlog, planning, building, review, ready, done, failed, cancelled, other

    init(_ status: TaskStatus) {
        switch status {
        case .backlog: self = .backlog
        case .planning: self = .planning
        case .building: self = .building
        case .review: self = .review
        case .ready: self = .ready
        case .done: self = .done
        case .failed: self = .failed
        case .cancelled: self = .cancelled
        case .unknown: self = .other
        }
    }

    var titleKey: String {
        switch self {
        case .backlog: "status_backlog"
        case .planning: "status_planning"
        case .building: "status_building"
        case .review: "status_review"
        case .ready: "status_ready"
        case .done: "status_done"
        case .failed: "status_failed"
        case .cancelled: "status_cancelled"
        case .other: "status_other"
        }
    }

    var symbol: String {
        switch self {
        case .backlog: "tray"
        case .planning: "list.bullet.clipboard"
        case .building: "hammer"
        case .review: "checklist"
        case .ready: "checkmark.seal"
        case .done: "checkmark.circle"
        case .failed: "xmark.octagon"
        case .cancelled: "slash.circle"
        case .other: "questionmark.circle"
        }
    }
}
