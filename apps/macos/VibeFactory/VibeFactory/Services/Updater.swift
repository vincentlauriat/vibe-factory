import Combine
import Observation
import Sparkle

/// Sparkle auto-update: checks the appcast (`SUFeedURL` in Info.plist) in the
/// background and backs the "Check for Updates…" menu item.
@MainActor
@Observable
final class Updater {
    static let shared = Updater()

    /// False while a check is already running.
    private(set) var canCheckForUpdates = false

    @ObservationIgnored private let controller = SPUStandardUpdaterController(
        startingUpdater: true, updaterDelegate: nil, userDriverDelegate: nil
    )
    @ObservationIgnored private var observation: AnyCancellable?

    private init() {
        observation = controller.updater.publisher(for: \.canCheckForUpdates)
            .receive(on: DispatchQueue.main)
            .sink { [weak self] value in self?.canCheckForUpdates = value }
    }

    func checkForUpdates() {
        controller.checkForUpdates(nil)
    }
}
