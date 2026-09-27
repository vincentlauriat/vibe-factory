import Foundation
import Observation
import VibeAPI

/// The connection of one project window: the `vibe serve` child it started
/// (or the remote server it was given), the client, and the board, polled
/// every 2 s like the web UI until the server has a global stream.
@Observable
@MainActor
final class ProjectSession: Identifiable {
    enum State: Equatable {
        case idle
        case starting
        case connected
        case failed(String)
    }

    static let pollInterval: UInt64 = 2_000_000_000

    let ref: ProjectRef
    private(set) var state: State = .idle
    private(set) var client: VibeClient?
    private(set) var rows: [TaskRow] = []
    private(set) var serverVersion: String?
    /// Last polling error, cleared by the next success.
    private(set) var boardError: String?
    /// A task to show, asked by a notification click; the window consumes it.
    var requestedTask: String?

    @ObservationIgnored private let settings: AppSettings
    @ObservationIgnored private var server: ServerProcess?
    @ObservationIgnored private var poller: Task<Void, Never>?
    /// Bumped by every `stop()`: a `start()` that finds it changed after
    /// connecting was cancelled meanwhile and drops what it started.
    @ObservationIgnored private var generation = 0
    @ObservationIgnored private var boardLoaded = false

    nonisolated var id: ProjectRef { ref }

    init(ref: ProjectRef, settings: AppSettings) {
        self.ref = ref
        self.settings = settings
    }

    // MARK: Board summaries (menu bar, sidebar badges)

    var runningRows: [TaskRow] { rows.filter(\.running) }
    var approvalRows: [TaskRow] { rows.filter { $0.run?.pendingApproval != nil } }

    func rows(in column: BoardColumn) -> [TaskRow] {
        rows.filter { BoardColumn($0.task.status) == column }
            .sorted { ($0.number ?? .max, $0.task.createdAt) < ($1.number ?? .max, $1.task.createdAt) }
    }

    func row(_ id: String) -> TaskRow? { rows.first { $0.id == id } }

    // MARK: Lifecycle

    /// Start the server (folder) or check the remote one, then poll the board.
    func start() async {
        if state == .starting || state == .connected { return }
        state = .starting
        let attempt = generation
        do {
            let (client, server) = try await connect()
            guard attempt == generation else {
                // stop() ran while connecting: this server is nobody's.
                await server?.stop(grace: 5)
                return
            }
            self.server = server
            self.client = client
            serverVersion = try? await client.health().version
            guard attempt == generation else { return }
            state = .connected
            boardLoaded = false
            Notifications.requestAuthorization()
            startPolling()
        } catch {
            guard attempt == generation else { return }
            state = .failed(error.localizedDescription)
        }
    }

    /// Stop polling and the server this session started (SIGINT, then SIGTERM
    /// after the server's grace period).
    func stop() async {
        generation += 1
        poller?.cancel()
        poller = nil
        client = nil
        state = .idle
        if let server {
            self.server = nil
            await server.stop()
        }
    }

    /// Application termination: SIGINT to the child without waiting; the
    /// registry waits once for every child.
    func interruptForTermination() -> ServerProcess? {
        generation += 1
        poller?.cancel()
        server?.interrupt()
        return server
    }

    func retry() async {
        await stop()
        await start()
    }

    private func connect() async throws -> (VibeClient, ServerProcess?) {
        switch ref {
        case .folder(let path):
            guard let executable = ServerProcess.resolveExecutable(setting: settings.vibePath) else {
                throw ServerProcessError.executableNotFound
            }
            let evals = settings.evalsPath.isEmpty ? nil : URL(fileURLWithPath: settings.evalsPath)
            let server = ServerProcess(project: URL(fileURLWithPath: path), executable: executable, evals: evals,
                                       extraEnvironment: settings.childEnvironment())
            let attempt = generation
            server.onExit = { [weak self] status, output in
                Task { @MainActor in self?.serverExited(status: status, output: output, attempt: attempt) }
            }
            return (VibeClient(endpoint: try await server.start()), server)
        case .remote(let url):
            guard let baseURL = URL(string: url) else { throw VibeError.transport("invalid URL \(url)") }
            let token = Keychain.get(account: ProjectRef.tokenAccount(for: url)) ?? ""
            let client = VibeClient(endpoint: ServerEndpoint(baseURL: baseURL, token: token))
            _ = try await client.tasks() // checks the token, not only the address
            return (client, nil)
        }
    }

    /// The child ended on its own after it was ready.
    private func serverExited(status: Int32, output: String, attempt: Int) {
        guard attempt == generation else { return }
        poller?.cancel()
        poller = nil
        client = nil
        server = nil
        state = .failed(settings.t("server_exited", "\(status)") + (output.isEmpty ? "" : "\n\n" + output))
    }

    // MARK: Board

    private func startPolling() {
        poller?.cancel()
        poller = Task { [weak self] in
            while !Task.isCancelled {
                guard let session = self else { return }
                await session.refresh()
                try? await Task.sleep(nanoseconds: Self.pollInterval)
            }
        }
    }

    func refresh() async {
        guard let client else { return }
        do {
            let next = try await client.tasks()
            let previous = rows
            rows = next
            boardError = nil
            if boardLoaded { notify(BoardTransition.between(previous, next)) }
            boardLoaded = true
        } catch is CancellationError {
        } catch {
            boardError = error.localizedDescription
        }
    }

    /// Post a notification per transition: a gate starts waiting, a run
    /// pauses, a run ends.
    private func notify(_ transitions: [BoardTransition]) {
        guard settings.notificationsEnabled else { return }
        for transition in transitions {
            let row = transition.row
            let title = "\(ref.displayName) — \(row.label) \(row.task.title)"
            let body: String
            let kind: String
            switch transition {
            case .approvalRequested(_, let gate):
                body = settings.t("notify_approval", gate.display)
                kind = "approval"
            case .paused:
                body = settings.t("notify_paused")
                kind = "paused"
            case .finished(let row):
                body = settings.t("notify_finished", row.task.status.rawValue)
                kind = "finished"
            }
            Notifications.post(title: title, body: body, identifier: "\(kind)-\(row.id)", project: ref, task: row.id)
        }
    }
}

/// Every open project session, for the menu bar item, notification clicks
/// and app termination.
@Observable
@MainActor
final class SessionRegistry {
    static let shared = SessionRegistry()

    private(set) var sessions: [ProjectRef: ProjectSession] = [:]
    /// A project window to open, asked from outside a view (notification
    /// click); a view that has `openWindow` consumes it.
    var windowRequest: ProjectRef?

    /// The session of a window, created and started on first use.
    func open(_ ref: ProjectRef, settings: AppSettings) -> ProjectSession {
        if let session = sessions[ref] { return session }
        let session = ProjectSession(ref: ref, settings: settings)
        sessions[ref] = session
        Task { await session.start() }
        return session
    }

    /// The window closed: stop its server.
    func close(_ ref: ProjectRef) {
        guard let session = sessions.removeValue(forKey: ref) else { return }
        Task { await session.stop() }
    }

    /// SIGINT to every child at once, then one wait of at most `seconds` for
    /// all of them; they finish their shutdown on their own after that.
    func stopAllForTermination(upTo seconds: TimeInterval = 2) {
        let servers = sessions.values.compactMap { $0.interruptForTermination() }
        sessions.removeAll()
        let deadline = Date().addingTimeInterval(seconds)
        while servers.contains(where: \.isRunning), Date() < deadline {
            Thread.sleep(forTimeInterval: 0.05)
        }
    }

    /// A notification was clicked: show the task in its project's window.
    func show(task: String, in ref: ProjectRef) {
        sessions[ref]?.requestedTask = task
        windowRequest = ref
    }

    var runningCount: Int { sessions.values.reduce(0) { $0 + $1.runningRows.count } }
    var approvalCount: Int { sessions.values.reduce(0) { $0 + $1.approvalRows.count } }
    var sortedSessions: [ProjectSession] {
        sessions.values.sorted { $0.ref.displayName.localizedStandardCompare($1.ref.displayName) == .orderedAscending }
    }
}
