import Foundation

/// A change between two polls of the board that deserves a notification.
public enum BoardTransition: Hashable, Sendable {
    /// A run started waiting on a gate.
    case approvalRequested(TaskRow, gate: Name)
    /// A run became paused (without a pending gate).
    case paused(TaskRow)
    /// A task stopped running; `row.task.status` is where it ended.
    case finished(TaskRow)

    public var row: TaskRow {
        switch self {
        case .approvalRequested(let row, _), .paused(let row), .finished(let row): row
        }
    }

    /// At most one transition per task, in the order of `next`. Tasks absent
    /// from `previous` (new, or first load of the board) produce none.
    public static func between(_ previous: [TaskRow], _ next: [TaskRow]) -> [BoardTransition] {
        let before = Dictionary(previous.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        return next.compactMap { row in
            guard let old = before[row.id] else { return nil }
            if let gate = row.run?.pendingApproval, old.run?.pendingApproval == nil {
                return .approvalRequested(row, gate: gate)
            }
            if row.run?.status == .paused, old.run?.status != .paused, row.run?.pendingApproval == nil {
                return .paused(row)
            }
            if old.running, !row.running {
                return .finished(row)
            }
            return nil
        }
    }
}
