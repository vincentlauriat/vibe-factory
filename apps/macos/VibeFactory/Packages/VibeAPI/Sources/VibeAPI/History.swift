import Foundation

// `GET /api/history` and `GET /api/history/{task}`: the `TaskHistory`
// documents of `vibe history --json` (`crates/vibe-pipeline/src/history.rs`).

/// A phase of a run.
public struct PhaseSummary: Codable, Hashable, Sendable {
    public var phase: Name
    public var startedAt: Date
    public var finishedAt: Date?
    public var success: Bool?
    public var summary: String

    enum CodingKeys: String, CodingKey {
        case phase, success, summary
        case startedAt = "started_at"
        case finishedAt = "finished_at"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        phase = try c.decode(Name.self, forKey: .phase)
        startedAt = try c.decode(Date.self, forKey: .startedAt)
        finishedAt = try c.decodeIfPresent(Date.self, forKey: .finishedAt)
        success = try c.decodeIfPresent(Bool.self, forKey: .success)
        summary = try c.decode(String.self, forKey: .summary, default: "")
    }
}

/// A commit in the task workspace.
public struct CommitRecord: Codable, Hashable, Sendable {
    public var commit: String
    /// First line of the message (empty for integrations logged before 0.5).
    public var message: String
    /// Files changed (empty for integrations logged before 0.5).
    public var files: [String]
    public var subtask: String?
    public var at: Date

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        commit = try c.decode(String.self, forKey: .commit)
        message = try c.decode(String.self, forKey: .message, default: "")
        files = try c.decode([String].self, forKey: .files, default: [])
        subtask = try c.decodeIfPresent(String.self, forKey: .subtask)
        at = try c.decode(Date.self, forKey: .at)
    }

    enum CodingKeys: String, CodingKey { case commit, message, files, subtask, at }
}

/// The merge of a run into the base branch.
public struct MergeRecord: Codable, Hashable, Sendable {
    public var commit: String
    public var branch: String
    public var base: String
    public var at: Date
}

/// A required validation command and its result.
public struct ValidationRecord: Codable, Hashable, Sendable {
    public var command: String
    /// True when it checked the combined integration candidate.
    public var integration: Bool
    public var passed: Bool
    public var exitCode: Int64?
    public var at: Date

    enum CodingKeys: String, CodingKey {
        case command, integration, passed, at
        case exitCode = "exit_code"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        command = try c.decode(String.self, forKey: .command)
        integration = try c.decode(Bool.self, forKey: .integration, default: false)
        passed = try c.decode(Bool.self, forKey: .passed)
        exitCode = try c.decodeIfPresent(Int64.self, forKey: .exitCode)
        at = try c.decode(Date.self, forKey: .at)
    }
}

/// Whether a run is going, ended, or was cut by the death of its process.
public enum RunSummaryState: OpenEnum {
    case running, finished, interrupted
    case unknown(String)

    public init(rawValue: String) {
        switch rawValue {
        case "running": self = .running
        case "finished": self = .finished
        case "interrupted": self = .interrupted
        default: self = .unknown(rawValue)
        }
    }

    public var rawValue: String {
        switch self {
        case .running: "running"
        case .finished: "finished"
        case .interrupted: "interrupted"
        case .unknown(let value): value
        }
    }
}

/// One run of a task's history (`RunSummary` on the server; the board row's
/// `RunSummary` is another shape).
public struct HistoryRun: Codable, Hashable, Sendable, Identifiable {
    public var run: String
    public var startedAt: Date
    /// When it last finished; `nil` while it runs again, or when interrupted.
    public var finishedAt: Date?
    public var state: RunSummaryState
    /// Task status it finished with.
    public var status: TaskStatus?
    public var success: Bool?
    public var resumes: UInt32
    public var usage: Usage
    public var activeMs: UInt64
    /// False for a run logged before 0.5 whose totals are unknown: show
    /// them as unknown, not as zeros.
    public var totalsKnown: Bool
    public var phases: [PhaseSummary]
    public var commits: [CommitRecord]
    public var merged: MergeRecord?
    public var validations: [ValidationRecord]
    public var approvals: [ApprovalRecord]
    public var pendingApproval: Name?
    public var lastError: String?
    public var lastEventAt: Date

    public var id: String { run }

    enum CodingKeys: String, CodingKey {
        case run, state, status, success, resumes, usage, phases, commits, merged, validations, approvals
        case startedAt = "started_at"
        case finishedAt = "finished_at"
        case activeMs = "active_ms"
        case totalsKnown = "totals_known"
        case pendingApproval = "pending_approval"
        case lastError = "last_error"
        case lastEventAt = "last_event_at"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        run = try c.decode(String.self, forKey: .run)
        startedAt = try c.decode(Date.self, forKey: .startedAt)
        finishedAt = try c.decodeIfPresent(Date.self, forKey: .finishedAt)
        state = try c.decode(RunSummaryState.self, forKey: .state)
        status = try c.decodeIfPresent(TaskStatus.self, forKey: .status)
        success = try c.decodeIfPresent(Bool.self, forKey: .success)
        resumes = try c.decode(UInt32.self, forKey: .resumes, default: 0)
        usage = try c.decode(Usage.self, forKey: .usage, default: .zero)
        activeMs = try c.decode(UInt64.self, forKey: .activeMs, default: 0)
        totalsKnown = try c.decode(Bool.self, forKey: .totalsKnown, default: true)
        phases = try c.decode([PhaseSummary].self, forKey: .phases, default: [])
        commits = try c.decode([CommitRecord].self, forKey: .commits, default: [])
        merged = try c.decodeIfPresent(MergeRecord.self, forKey: .merged)
        validations = try c.decode([ValidationRecord].self, forKey: .validations, default: [])
        approvals = try c.decode([ApprovalRecord].self, forKey: .approvals, default: [])
        pendingApproval = try c.decodeIfPresent(Name.self, forKey: .pendingApproval)
        lastError = try c.decodeIfPresent(String.self, forKey: .lastError)
        lastEventAt = try c.decode(Date.self, forKey: .lastEventAt)
    }
}

/// What happened to a changed file.
public enum FileStatus: OpenEnum {
    case added, modified, deleted, renamed, copied
    /// The server's own `unknown`: files from commit events or the trace.
    case unknown
    /// A value this app does not know.
    case other(String)

    public init(rawValue: String) {
        switch rawValue {
        case "added": self = .added
        case "modified": self = .modified
        case "deleted": self = .deleted
        case "renamed": self = .renamed
        case "copied": self = .copied
        case "unknown": self = .unknown
        default: self = .other(rawValue)
        }
    }

    public var rawValue: String {
        switch self {
        case .added: "added"
        case .modified: "modified"
        case .deleted: "deleted"
        case .renamed: "renamed"
        case .copied: "copied"
        case .unknown: "unknown"
        case .other(let value): value
        }
    }

    /// `A`, `M`, `D`, `R`, `C`, `?`, as `vibe history`.
    public var letter: String {
        switch self {
        case .added: "A"
        case .modified: "M"
        case .deleted: "D"
        case .renamed: "R"
        case .copied: "C"
        case .unknown, .other: "?"
        }
    }
}

/// A file a task changed.
public struct FileChange: Codable, Hashable, Sendable {
    public var path: String
    public var status: FileStatus
    /// Former path of a renamed or copied file.
    public var oldPath: String?

    enum CodingKeys: String, CodingKey {
        case path, status
        case oldPath = "old_path"
    }
}

/// Where the list of changed files comes from (tagged by `kind`).
public enum ChangedFilesSource: Codable, Hashable, Sendable {
    /// `git diff <base>...<branch>`.
    case branch(branch: String, base: String)
    /// The recorded merge.
    case mergeCommit(commit: String, fastForward: Bool)
    /// The files of the `committed` events (approximate).
    case commits
    /// The paths written by `write_file` and `edit_file` (approximate).
    case trace
    case none
    case unknown(String)

    enum CodingKeys: String, CodingKey {
        case kind, branch, base, commit
        case fastForward = "fast_forward"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try c.decode(String.self, forKey: .kind)
        switch kind {
        case "branch":
            self = .branch(branch: try c.decode(String.self, forKey: .branch),
                           base: try c.decode(String.self, forKey: .base))
        case "merge_commit":
            self = .mergeCommit(commit: try c.decode(String.self, forKey: .commit),
                                fastForward: try c.decode(Bool.self, forKey: .fastForward, default: false))
        case "commits": self = .commits
        case "trace": self = .trace
        case "none": self = .none
        default: self = .unknown(kind)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .branch(let branch, let base):
            try c.encode("branch", forKey: .kind)
            try c.encode(branch, forKey: .branch)
            try c.encode(base, forKey: .base)
        case .mergeCommit(let commit, let fastForward):
            try c.encode("merge_commit", forKey: .kind)
            try c.encode(commit, forKey: .commit)
            try c.encode(fastForward, forKey: .fastForward)
        case .commits: try c.encode("commits", forKey: .kind)
        case .trace: try c.encode("trace", forKey: .kind)
        case .none: try c.encode("none", forKey: .kind)
        case .unknown(let kind): try c.encode(kind, forKey: .kind)
        }
    }
}

/// The files a task changed.
public struct ChangedFiles: Codable, Hashable, Sendable {
    public var source: ChangedFilesSource
    /// Whether the list may be incomplete or too long.
    public var approximate: Bool
    /// Sorted by path.
    public var files: [FileChange]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        source = try c.decode(ChangedFilesSource.self, forKey: .source)
        approximate = try c.decode(Bool.self, forKey: .approximate, default: false)
        files = try c.decode([FileChange].self, forKey: .files, default: [])
    }

    enum CodingKeys: String, CodingKey { case source, approximate, files }
}

/// Totals over the runs of a task.
public struct HistoryTotals: Codable, Hashable, Sendable {
    /// Tokens of the runs whose totals are known.
    public var usage: Usage
    public var activeMs: UInt64
    public var runs: UInt32
    public var commits: UInt32
    /// Whether every run's totals are known; otherwise the totals are a lower bound.
    public var complete: Bool

    enum CodingKeys: String, CodingKey {
        case usage, runs, commits, complete
        case activeMs = "active_ms"
    }
}

/// The last QA review.
public struct QaSummary: Codable, Hashable, Sendable {
    public var round: UInt32
    public var verdict: Name
    public var summary: String
    /// Number of issues found.
    public var issues: Int
}

/// Money cost of a task, with a `[pricing]` table.
public struct Cost: Codable, Hashable, Sendable {
    public var amount: Double
    /// `USD`.
    public var currency: String
    /// False when the amount is a lower bound (tokens used outside a
    /// finished agent session are not priced).
    public var complete: Bool
}

/// What a task did: `vibe history --json`.
public struct TaskHistory: Codable, Hashable, Sendable, Identifiable {
    public var task: VibeTask
    public var number: UInt32
    public var runs: [HistoryRun]
    public var totals: HistoryTotals
    public var changedFiles: ChangedFiles
    public var lastQa: QaSummary?
    /// Whether every required validation of the last run that ran any passed.
    public var validationsPassed: Bool?
    /// `nil` without a price for every model used.
    public var cost: Cost?
    public var lastActivity: Date
    /// Problems met while reading git.
    public var errors: [String]

    public var id: String { task.id }

    enum CodingKeys: String, CodingKey {
        case task, number, runs, totals, cost, errors
        case changedFiles = "changed_files"
        case lastQa = "last_qa"
        case validationsPassed = "validations_passed"
        case lastActivity = "last_activity"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        task = try c.decode(VibeTask.self, forKey: .task)
        number = try c.decode(UInt32.self, forKey: .number)
        runs = try c.decode([HistoryRun].self, forKey: .runs, default: [])
        totals = try c.decode(HistoryTotals.self, forKey: .totals)
        changedFiles = try c.decode(ChangedFiles.self, forKey: .changedFiles)
        lastQa = try c.decodeIfPresent(QaSummary.self, forKey: .lastQa)
        validationsPassed = try c.decodeIfPresent(Bool.self, forKey: .validationsPassed)
        cost = try c.decodeIfPresent(Cost.self, forKey: .cost)
        lastActivity = try c.decode(Date.self, forKey: .lastActivity)
        errors = try c.decode([String].self, forKey: .errors, default: [])
    }

    /// When the task finished: the end of its last finished run, else its
    /// last activity.
    public var finishedAt: Date {
        runs.reversed().lazy.compactMap(\.finishedAt).first ?? lastActivity
    }
}

/// The cells of a History row, written as `vibe history` writes them:
/// `+` marks a lower bound, `~` an approximate file list, `-` no cost.
public struct HistoryRowText: Hashable, Sendable, Identifiable {
    /// Task id.
    public var id: String
    public var number: String
    public var title: String
    public var status: TaskStatus
    public var runs: String
    public var commits: String
    public var files: String
    public var tokens: String
    public var active: String
    public var cost: String
    public var finished: Date

    public init(_ h: TaskHistory) {
        id = h.task.id
        number = "\(h.number)"
        title = h.task.title
        status = h.task.status
        runs = "\(h.totals.runs)"
        commits = "\(h.totals.commits)"
        files = "\(h.changedFiles.files.count)" + (h.changedFiles.approximate ? "~" : "")
        tokens = Format.lowerBound(Format.tokens(h.totals.usage.total), complete: h.totals.complete)
        active = Format.lowerBound(Format.duration(ms: h.totals.activeMs), complete: h.totals.complete)
        cost = h.cost.map(Self.cost) ?? "-"
        finished = h.finishedAt
    }

    /// `1.84 USD`, `+` when some tokens were not priced.
    public static func cost(_ cost: Cost) -> String {
        Format.lowerBound(String(format: "%.2f %@", locale: Locale(identifier: "en_US_POSIX"),
                                 cost.amount, cost.currency),
                          complete: cost.complete)
    }
}
