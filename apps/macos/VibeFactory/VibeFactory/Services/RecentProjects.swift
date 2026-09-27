import Foundation
import Observation

/// Projects opened recently, newest first, in `UserDefaults`. The app is not
/// sandboxed, so plain paths are enough (no security-scoped bookmarks).
@Observable
@MainActor
final class RecentProjects {
    private static let key = "recentProjects"
    private static let limit = 12

    private(set) var items: [ProjectRef] = []

    init() {
        if let data = UserDefaults.standard.data(forKey: Self.key),
           let items = try? JSONDecoder().decode([ProjectRef].self, from: data) {
            self.items = items
        }
    }

    func add(_ ref: ProjectRef) {
        items.removeAll { $0 == ref }
        items.insert(ref, at: 0)
        items = Array(items.prefix(Self.limit))
        save()
    }

    func remove(_ ref: ProjectRef) {
        items.removeAll { $0 == ref }
        if case .remote(let url) = ref {
            Keychain.set(nil, account: ProjectRef.tokenAccount(for: url))
        }
        save()
    }

    /// Whether a folder still exists (moved projects are shown dimmed).
    func isAvailable(_ ref: ProjectRef) -> Bool {
        guard case .folder(let path) = ref else { return true }
        var isDirectory: ObjCBool = false
        return FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory) && isDirectory.boolValue
    }

    private func save() {
        if let data = try? JSONEncoder().encode(items) {
            UserDefaults.standard.set(data, forKey: Self.key)
        }
    }
}
