import Foundation

/// A moment of a run that deserves a user notification.
public enum EventNotice: Hashable, Sendable {
    /// The run waits on a gate.
    case approvalRequested(gate: Name)
    /// The run stopped for a human, without a gate.
    case paused(reason: String)
    /// The run ended without stopping for a human first.
    case finished(status: TaskStatus, success: Bool)
}

/// Turns live events into notices, one per stop of a run: a gate logs
/// `approval_requested`, `paused`, then `run_finished`, and only the first
/// of them is worth a notification.
public struct EventNoticeTracker: Sendable {
    /// Runs that stopped for a human since they last started.
    private var stopped: Set<String> = []

    public init() {}

    public mutating func notice(for event: Event) -> EventNotice? {
        switch event {
        case .runStarted(let run, _):
            stopped.remove(run)
            return nil
        case .approvalRequested(let run, let gate):
            stopped.insert(run)
            return .approvalRequested(gate: gate)
        case .paused(let run, let reason):
            return stopped.insert(run).inserted ? .paused(reason: reason) : nil
        case .runFinished(let totals):
            return stopped.remove(totals.run) == nil
                ? .finished(status: totals.status, success: totals.success) : nil
        default:
            return nil
        }
    }
}
