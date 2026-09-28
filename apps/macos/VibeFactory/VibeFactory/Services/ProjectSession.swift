import Foundation
import Observation
import VibeAPI

/// The connection of one project window: the `vibe serve` child it started
/// (or the remote server it was given), the client, the board, and the one
/// project-wide event stream (`/api/stream`) that feeds the Activity feed,
/// the selected task, board refreshes, the History and notifications.
///
/// The board is reloaded when an event changes it, and polled every 10 s
/// since creating a task is not an event (every 2 s while the stream is
/// down, like the web UI).
@Observable
@MainActor
final class ProjectSession: Identifiable {
    enum State: Equatable {
        case idle
        case starting
        case connected
        case failed(String)
    }

    static let pollInterval: UInt64 = 10_000_000_000
    static let downPollInterval: UInt64 = 2_000_000_000
    /// Logged events loaded into the feed when the stream starts.
    static let seedLimit = 500
    /// Event types after which the board is reloaded.
    static let boardTypes: Set<String> = [
        "run_started", "phase_started", "phase_finished", "subtask_updated", "approval_requested",
        "approval_resolved", "paused", "merged", "run_finished",
    ]

    let ref: ProjectRef
    private(set) var state: State = .idle
    private(set) var client: VibeClient?
    private(set) var rows: [TaskRow] = []
    private(set) var serverVersion: String?
    /// Last polling error, cleared by the next success.
    private(set) var boardError: String?
    /// Whether the project-wide stream runs (it reconnects on its own after
    /// network errors; it stops on answers a retry cannot fix).
    private(set) var streamUp = false
    /// Why the stream is not running, if it stopped.
    private(set) var streamError: String?
    /// Every task's events, described, the newest last.
    let feed: ProjectFeed
    /// Bumped by each `run_finished`: the History reloads on it.
    private(set) var finishedRuns = 0
    /// A task to show, asked by a notification click; the window consumes it.
    var requestedTask: String?
    /// The variables given to `vibe serve` changed while a run was going: the
    /// server restarts with them once no task runs.
    private(set) var restartPending = false

    @ObservationIgnored private let settings: AppSettings
    @ObservationIgnored private var server: ServerProcess?
    @ObservationIgnored private var poller: Task<Void, Never>?
    @ObservationIgnored private var streamTask: Task<Void, Never>?
    @ObservationIgnored private var boardReload: Task<Void, Never>?
    @ObservationIgnored private var listeners: [UUID: (GlobalEvent) -> Void] = [:]
    @ObservationIgnored private var notices = EventNoticeTracker()
    /// Bumped by every `stop()`: a `start()` that finds it changed after
    /// connecting was cancelled meanwhile and drops what it started.
    @ObservationIgnored private var generation = 0

    nonisolated var id: ProjectRef { ref }

    init(ref: ProjectRef, settings: AppSettings) {
        self.ref = ref
        self.settings = settings
        feed = ProjectFeed(translate: { settings.t($0) })
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

    /// Start the server (folder) or check the remote one, then open the
    /// event stream and poll the board.
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
            Notifications.requestAuthorization()
            await refresh()
            startStream(client)
            startPolling()
        } catch {
            guard attempt == generation else { return }
            state = .failed(error.localizedDescription)
        }
    }

    /// Stop the stream, the polling and the server this session started
    /// (SIGINT, then SIGTERM after the server's grace period).
    func stop() async {
        generation += 1
        cancelTasks()
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
        cancelTasks()
        server?.interrupt()
        return server
    }

    func retry() async {
        await stop()
        await start()
    }

    /// The variables given to `vibe serve` changed (Settings): a server started
    /// by this session only reads them at launch, so restart it, now if no task
    /// runs, else after the last run ends. A remote server is not ours.
    func environmentChanged() {
        guard case .folder = ref, state != .idle else { return }
        if runningRows.isEmpty {
            Task { await retry() }
        } else {
            restartPending = true
        }
    }

    private func cancelTasks() {
        poller?.cancel()
        poller = nil
        streamTask?.cancel()
        streamTask = nil
        boardReload?.cancel()
        boardReload = nil
        streamUp = false
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
        cancelTasks()
        client = nil
        server = nil
        state = .failed(settings.t("server_exited", "\(status)") + (output.isEmpty ? "" : "\n\n" + output))
    }

    // MARK: Events

    /// Be told of every event of the stream (the selected task's detail).
    func subscribe(_ listener: @escaping (GlobalEvent) -> Void) -> UUID {
        let id = UUID()
        listeners[id] = listener
        return id
    }

    func unsubscribe(_ id: UUID) {
        listeners[id] = nil
    }

    /// Load the last logged events into the feed, then stream from the
    /// cursor of that answer. With no event logged yet there is no cursor:
    /// the stream starts from the time taken before the call, so what is
    /// logged in between is not lost.
    private func startStream(_ client: VibeClient) {
        streamTask?.cancel()
        streamTask = Task { [weak self] in
            let since = VibeJSON.formatDate(Date())
            let page: EventsPage
            do {
                page = try await client.events(limit: Self.seedLimit)
            } catch is CancellationError {
                return
            } catch {
                self?.streamStopped(error)
                return
            }
            guard let session = self else { return }
            session.feed.seed(page.events)
            session.streamUp = true
            session.streamError = nil
            let stream = client.globalStream(after: page.cursor, since: page.cursor == nil ? since : nil)
            do {
                for try await event in stream.events() {
                    self?.receive(event)
                }
            } catch is CancellationError {
                return
            } catch {
                self?.streamStopped(error)
                return
            }
            self?.streamUp = false
        }
    }

    private func streamStopped(_ error: Error) {
        streamUp = false
        streamError = error.localizedDescription
    }

    private func receive(_ event: GlobalEvent) {
        let tagged = event.tagged
        feed.append(tagged)
        for listener in listeners.values { listener(event) }
        let type = tagged.event.typeName
        if Self.boardTypes.contains(type) { scheduleBoardReload() }
        if case .runFinished = tagged.event { finishedRuns += 1 }
        if let notice = notices.notice(for: tagged.event) { notify(notice, about: tagged) }
    }

    // MARK: Board

    private func startPolling() {
        poller?.cancel()
        poller = Task { [weak self] in
            while !Task.isCancelled {
                guard let interval = self.map({ $0.streamUp ? Self.pollInterval : Self.downPollInterval }) else { return }
                try? await Task.sleep(nanoseconds: interval)
                guard let session = self, !Task.isCancelled else { return }
                await session.refresh()
            }
        }
    }

    /// Coalesce the reloads a burst of events asks for.
    private func scheduleBoardReload() {
        boardReload?.cancel()
        boardReload = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 300_000_000)
            guard !Task.isCancelled else { return }
            await self?.refresh()
        }
    }

    func refresh() async {
        guard let client else { return }
        do {
            rows = try await client.tasks()
            boardError = nil
            if restartPending, runningRows.isEmpty {
                restartPending = false
                Task { await retry() }
            }
        } catch is CancellationError {
        } catch {
            boardError = error.localizedDescription
        }
    }

    /// Post a notification: a gate starts waiting, a run pauses, a run ends.
    private func notify(_ notice: EventNotice, about tagged: TaggedEnvelope) {
        guard settings.notificationsEnabled else { return }
        let name = row(tagged.task).map { "\($0.label) \($0.task.title)" } ?? tagged.label
        let title = "\(ref.displayName) — \(name)"
        let body: String
        let kind: String
        switch notice {
        case .approvalRequested(let gate):
            body = settings.t("notify_approval", gate.display)
            kind = "approval"
        case .paused(let reason):
            body = settings.t("notify_paused") + (reason.isEmpty ? "" : ": \(reason)")
            kind = "paused"
        case .finished(let status, _):
            body = settings.t("notify_finished", status.rawValue)
            kind = "finished"
        }
        Notifications.post(title: title, body: body, identifier: "\(kind)-\(tagged.task)", project: ref,
                           task: tagged.task)
    }
}

/// The project-wide Activity feed: every task's events, described, at most
/// `limit` lines. Streamed text (`agent_delta`) is not kept: it goes to the
/// selected task only.
@Observable
@MainActor
final class ProjectFeed {
    /// What the feed keeps of an event.
    struct Entry: Identifiable, Hashable, FeedEntry {
        let line: ActivityLine
        let task: String
        let number: UInt32
        let type: String
        var id: Int { line.id }
    }

    static let limit = 3_000

    private(set) var entries: [Entry] = []
    @ObservationIgnored private var nextId = 0
    @ObservationIgnored private let translate: (String) -> String

    init(translate: @escaping (String) -> String) {
        self.translate = translate
    }

    /// The events loaded when the stream starts replace the feed.
    func seed(_ events: [TaggedEnvelope]) {
        entries = events.suffix(Self.limit).compactMap(entry)
    }

    func append(_ event: TaggedEnvelope) {
        guard let entry = entry(event) else { return }
        entries.append(entry)
        if entries.count > Self.limit { entries.removeFirst(entries.count - Self.limit) }
    }

    private func entry(_ event: TaggedEnvelope) -> Entry? {
        if event.event.isEphemeral { return nil }
        let id = nextId
        nextId += 1
        guard let line = ActivityLine.describe(event.envelope, id: id, detail: nil, verbose: true, t: translate)
        else { return nil }
        return Entry(line: line, task: event.task, number: event.number, type: event.event.typeName)
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

    /// The variables given to `vibe serve` changed: every session restarts its server.
    func environmentChanged() {
        for session in sessions.values { session.environmentChanged() }
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
