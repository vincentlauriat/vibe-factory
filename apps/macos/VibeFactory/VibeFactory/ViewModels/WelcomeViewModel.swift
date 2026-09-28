import AppKit
import Foundation
import Observation
import VibeAPI

/// The welcome window: open a folder, reopen a recent project, or connect to
/// a server started elsewhere.
@Observable
@MainActor
final class WelcomeViewModel {
    var remoteURL = "http://127.0.0.1:7777/"
    var remoteToken = ""
    private(set) var connecting = false
    var errorMessage: String?

    /// `NSOpenPanel` for a folder.
    func chooseFolder(prompt: String) -> ProjectRef? {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        panel.prompt = prompt
        guard panel.runModal() == .OK, let url = panel.url else { return nil }
        return ProjectRef.folder(path: url.standardizedFileURL.path)
    }

    /// A dropped item, if it is a folder.
    nonisolated static func folder(from url: URL) -> ProjectRef? {
        var isDirectory: ObjCBool = false
        guard url.isFileURL, FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory),
              isDirectory.boolValue else { return nil }
        return .folder(path: url.standardizedFileURL.path)
    }

    /// Check the address and token, then keep the token in the Keychain.
    func connect(invalidURL: String) async -> ProjectRef? {
        let text = remoteURL.trimmingCharacters(in: .whitespaces)
        var token = remoteToken.trimmingCharacters(in: .whitespacesAndNewlines)
        // Accept the `open http://…/#token=…` line `vibe serve` prints.
        var address = text
        // …and the same line with its `open ` prefix.
        if address.hasPrefix("open ") { address = String(address.dropFirst(5)).trimmingCharacters(in: .whitespaces) }
        if let hash = address.range(of: "#token=") {
            if token.isEmpty { token = String(address[hash.upperBound...]) }
            address = String(address[..<hash.lowerBound])
        }
        guard let url = URL(string: address), url.scheme == "http" || url.scheme == "https", url.host != nil else {
            errorMessage = "\(invalidURL) \(address)"
            return nil
        }
        connecting = true
        defer { connecting = false }
        do {
            _ = try await VibeClient(endpoint: ServerEndpoint(baseURL: url, token: token)).tasks()
        } catch {
            errorMessage = error.localizedDescription
            return nil
        }
        let key = url.absoluteString
        Keychain.set(token, account: ProjectRef.tokenAccount(for: key))
        remoteToken = ""
        return .remote(url: key)
    }
}
