import Foundation
import Observation
import VibeAPI

/// The selected task: its detail, its live events (one `EventStream`), the
/// budget gauges, the changes, and the actions of the toolbar and banner.
@Observable
@MainActor
final class TaskDetailViewModel {
    /// Token and time use against the limits, from the last `budget_updated`.
    struct Budget: Equatable {
        var tokens: UInt64
        var tokenLimit: UInt64?
        var activeMs: UInt64
        var durationLimitMs: UInt64?
    }

    /// Most events kept in memory; older ones are dropped from the feed.
    static let eventLimit = 3_000
    /// Most streamed characters kept for the current step.
    static let liveLimit = 6_000

    let taskId: String
    private(set) var detail: TaskDetail? {
        didSet { if detail?.plan != oldValue?.plan { redescribe() } }
    }
    /// Logged events of the current run (deltas excluded).
    @ObservationIgnored private(set) var envelopes: [Envelope] = []
    /// The Activity feed, described as events arrive (not in `body`): each
    /// line keeps the id of its envelope, a counter that never goes back.
    private(set) var lines: [ActivityLine] = []
    /// Text streamed by the current agent step (`agent_delta`).
    private(set) var liveText = ""
    private(set) var budgetFromEvents: Budget?
    private(set) var changes: String?
    private(set) var loadingChanges = false
    private(set) var acting = false
    var errorMessage: String?

    @ObservationIgnored private let client: VibeClient
    @ObservationIgnored private let translate: (String) -> String
    /// Id of each entry of `envelopes`, same order.
    @ObservationIgnored private var envelopeIds: [Int] = []
    @ObservationIgnored private var nextId = 0
    @ObservationIgnored private var streamTask: Task<Void, Never>?
    @ObservationIgnored private var reloadTask: Task<Void, Never>?

    init(taskId: String, client: VibeClient, translate: @escaping (String) -> String) {
        self.taskId = taskId
        self.client = client
        self.translate = translate
    }

    var budget: Budget? {
        if let budgetFromEvents { return budgetFromEvents }
        guard let run = detail?.run else { return nil }
        return Budget(tokens: run.usage.total, tokenLimit: nil, activeMs: run.activeMs, durationLimitMs: nil)
    }

    var pendingGate: Name? { detail?.run?.pendingApproval }
    var isRunning: Bool { detail?.running ?? false }
    var canRun: Bool { detail != nil && !isRunning && !acting }
    var canResume: Bool {
        guard let run = detail?.run, !isRunning, !acting else { return false }
        return run.status != .finished
    }
    var canCancel: Bool { isRunning && !acting }

    // MARK: Lifecycle

    func start() {
        Task { await reload() }
        streamTask?.cancel()
        let stream = client.stream(taskId)
        streamTask = Task { [weak self] in
            do {
                for try await envelope in stream.envelopes() {
                    self?.receive(envelope)
                }
            } catch is CancellationError {
            } catch {
                self?.errorMessage = error.localizedDescription
            }
        }
    }

    func stop() {
        streamTask?.cancel()
        streamTask = nil
        reloadTask?.cancel()
    }

    func reload() async {
        do {
            detail = try await client.task(taskId)
        } catch is CancellationError {
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    /// The board changed this task (status, run, running): reload the detail.
    func boardRowChanged(_ row: TaskRow) {
        guard let detail, row != detail.row || row.task.updatedAt != detail.task.updatedAt else { return }
        scheduleReload()
    }

    func loadChanges() async {
        loadingChanges = true
        defer { loadingChanges = false }
        do {
            changes = try await client.changes(taskId)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    // MARK: Actions

    func run(resume: Bool) async { await act { try await $0.run(self.taskId, resume: resume) } }
    func cancel() async { await act { try await $0.cancel(self.taskId) } }
    func approve(comment: String) async { await act { try await $0.approve(self.taskId, comment: comment) } }
    func reject(reason: String) async { await act { try await $0.reject(self.taskId, reason: reason) } }

    private func act(_ body: (VibeClient) async throws -> Void) async {
        acting = true
        defer { acting = false }
        do {
            try await body(client)
            await reload()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    // MARK: Events

    /// Describe every kept envelope again (subtask titles come with the plan).
    private func redescribe() {
        lines = zip(envelopeIds, envelopes).compactMap { id, envelope in
            ActivityLine.describe(envelope, id: id, detail: detail, t: translate)
        }
    }

    private func receive(_ envelope: Envelope) {
        switch envelope.event {
        case .agentDelta(_, _, _, let delta):
            guard delta.kind == "text" else { return }
            liveText += delta.text
            if liveText.count > Self.liveLimit { liveText = String(liveText.suffix(Self.liveLimit)) }
            return
        case .runStarted(let run, _):
            if envelopes.last?.event.runId != run {
                envelopes.removeAll()
                envelopeIds.removeAll()
                lines.removeAll()
                budgetFromEvents = nil
            }
            liveText = ""
        case .agentText, .agentFinished, .toolCalled:
            liveText = ""
        case .budgetUpdated(_, let tokens, let tokenLimit, let activeMs, let durationLimitMs):
            budgetFromEvents = Budget(tokens: tokens, tokenLimit: tokenLimit, activeMs: activeMs,
                                      durationLimitMs: durationLimitMs)
        case .artefactWritten, .subtaskUpdated, .phaseFinished, .runFinished, .approvalRequested,
             .approvalResolved, .paused, .merged:
            scheduleReload()
        default:
            break
        }
        let id = nextId
        nextId += 1
        envelopes.append(envelope)
        envelopeIds.append(id)
        if let line = ActivityLine.describe(envelope, id: id, detail: detail, t: translate) {
            lines.append(line)
        }
        if envelopes.count > Self.eventLimit {
            let excess = envelopes.count - Self.eventLimit
            envelopes.removeFirst(excess)
            envelopeIds.removeFirst(excess)
            if let first = envelopeIds.first { lines.removeAll { $0.id < first } }
        }
    }

    /// Coalesce the reloads a burst of events asks for.
    private func scheduleReload() {
        reloadTask?.cancel()
        reloadTask = Task { [weak self] in
            try? await Task.sleep(nanoseconds: 300_000_000)
            guard !Task.isCancelled else { return }
            await self?.reload()
        }
    }
}
