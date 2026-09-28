import Foundation

// Mirrors of the server's JSON. Field names follow `vibe-core` and
// `vibe-pipeline` exactly (see `docs/src/reference/events.md` and
// `crates/vibe-cli/src/server/api.rs`). Enums keep unknown values instead of
// failing, so a newer server never breaks an older app.

/// A string enum that keeps values it does not know.
public protocol OpenEnum: RawRepresentable, Codable, Hashable, Sendable where RawValue == String {
    init(rawValue: String)
}

extension OpenEnum {
    public init(from decoder: Decoder) throws {
        self.init(rawValue: try decoder.singleValueContainer().decode(String.self))
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }
}

/// Lifecycle state of a task on the board.
public enum TaskStatus: OpenEnum, CaseIterable {
    case backlog, planning, building, review, ready, done, failed, cancelled
    case unknown(String)

    public static let allCases: [TaskStatus] = [
        .backlog, .planning, .building, .review, .ready, .done, .failed, .cancelled,
    ]

    public init(rawValue: String) {
        self = Self.allCases.first { $0.rawValue == rawValue } ?? .unknown(rawValue)
    }

    public var rawValue: String {
        switch self {
        case .backlog: "backlog"
        case .planning: "planning"
        case .building: "building"
        case .review: "review"
        case .ready: "ready"
        case .done: "done"
        case .failed: "failed"
        case .cancelled: "cancelled"
        case .unknown(let value): value
        }
    }

    public var isTerminal: Bool {
        switch self {
        case .done, .failed, .cancelled: true
        default: false
        }
    }
}

/// Lifecycle of a run (`run.json`).
public enum RunStatus: OpenEnum {
    case running, paused, finished, failed, cancelled
    case unknown(String)

    public init(rawValue: String) {
        switch rawValue {
        case "running": self = .running
        case "paused": self = .paused
        case "finished": self = .finished
        case "failed": self = .failed
        case "cancelled": self = .cancelled
        default: self = .unknown(rawValue)
        }
    }

    public var rawValue: String {
        switch self {
        case .running: "running"
        case .paused: "paused"
        case .finished: "finished"
        case .failed: "failed"
        case .cancelled: "cancelled"
        case .unknown(let value): value
        }
    }
}

/// A snake case name the app only displays: phases, gates, subtask
/// statuses, verdicts, severities, roles.
public struct Name: OpenEnum, CustomStringConvertible, ExpressibleByStringLiteral {
    public let rawValue: String
    public init(rawValue: String) { self.rawValue = rawValue }
    public init(stringLiteral value: String) { self.rawValue = value }
    public var description: String { rawValue }
    /// `in_progress` → `in progress`.
    public var display: String { rawValue.replacingOccurrences(of: "_", with: " ") }
}

/// Token counts.
public struct Usage: Codable, Hashable, Sendable {
    public var inputTokens: UInt64
    public var outputTokens: UInt64
    public var cacheReadTokens: UInt64
    public var cacheWriteTokens: UInt64

    public init(inputTokens: UInt64 = 0, outputTokens: UInt64 = 0,
                cacheReadTokens: UInt64 = 0, cacheWriteTokens: UInt64 = 0) {
        self.inputTokens = inputTokens
        self.outputTokens = outputTokens
        self.cacheReadTokens = cacheReadTokens
        self.cacheWriteTokens = cacheWriteTokens
    }

    public static let zero = Usage()

    /// Input plus output tokens, the figure budgets count.
    public var total: UInt64 { inputTokens + outputTokens }

    enum CodingKeys: String, CodingKey {
        case inputTokens = "input_tokens"
        case outputTokens = "output_tokens"
        case cacheReadTokens = "cache_read_tokens"
        case cacheWriteTokens = "cache_write_tokens"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        inputTokens = try c.decode(UInt64.self, forKey: .inputTokens, default: 0)
        outputTokens = try c.decode(UInt64.self, forKey: .outputTokens, default: 0)
        cacheReadTokens = try c.decode(UInt64.self, forKey: .cacheReadTokens, default: 0)
        cacheWriteTokens = try c.decode(UInt64.self, forKey: .cacheWriteTokens, default: 0)
    }
}

/// A unit of work (`task.json`).
public struct VibeTask: Codable, Hashable, Sendable, Identifiable {
    public var id: String
    public var title: String
    public var description: String
    public var status: TaskStatus
    public var complexity: Name?
    public var labels: [String]
    public var source: JSONValue?
    public var createdAt: Date
    public var updatedAt: Date
    public var branch: String?

    enum CodingKeys: String, CodingKey {
        case id, title, description, status, complexity, labels, source, branch
        case createdAt = "created_at"
        case updatedAt = "updated_at"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decode(String.self, forKey: .title)
        description = try c.decode(String.self, forKey: .description, default: "")
        status = try c.decode(TaskStatus.self, forKey: .status)
        complexity = try c.decodeIfPresent(Name.self, forKey: .complexity)
        labels = try c.decode([String].self, forKey: .labels, default: [])
        source = try c.decodeIfPresent(JSONValue.self, forKey: .source)
        createdAt = try c.decode(Date.self, forKey: .createdAt)
        updatedAt = try c.decode(Date.self, forKey: .updatedAt)
        branch = try c.decodeIfPresent(String.self, forKey: .branch)
    }
}

/// The run summary of a board row.
public struct RunSummary: Codable, Hashable, Sendable {
    public var status: RunStatus
    public var currentPhase: Name
    public var pendingApproval: Name?

    enum CodingKeys: String, CodingKey {
        case status
        case currentPhase = "current_phase"
        case pendingApproval = "pending_approval"
    }
}

/// One row of `GET /api/tasks`.
public struct TaskRow: Codable, Hashable, Sendable, Identifiable {
    public var task: VibeTask
    /// Position of the task on the board (`#3`); absent for old stores.
    public var number: Int?
    /// A run is active, in this server or in another process.
    public var running: Bool
    public var run: RunSummary?

    public var id: String { task.id }

    /// `#3`, or the first characters of the id.
    public var label: String {
        number.map { "#\($0)" } ?? String(task.id.prefix(8))
    }
}

/// A validation command run by the pipeline.
public struct ValidationResult: Codable, Hashable, Sendable {
    public var integration: Bool
    public var command: String
    public var finishedAt: Date
    public var passed: Bool
    public var output: String

    enum CodingKeys: String, CodingKey {
        case integration, command, passed, output
        case finishedAt = "finished_at"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        integration = try c.decode(Bool.self, forKey: .integration, default: false)
        command = try c.decode(String.self, forKey: .command)
        finishedAt = try c.decode(Date.self, forKey: .finishedAt)
        passed = try c.decode(Bool.self, forKey: .passed)
        output = try c.decode(String.self, forKey: .output, default: "")
    }
}

/// A human decision on an approval gate.
public struct ApprovalRecord: Codable, Hashable, Sendable {
    public var gate: Name
    public var approved: Bool
    public var comment: String
    public var at: Date

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        gate = try c.decode(Name.self, forKey: .gate)
        approved = try c.decode(Bool.self, forKey: .approved)
        comment = try c.decode(String.self, forKey: .comment, default: "")
        at = try c.decode(Date.self, forKey: .at)
    }

    enum CodingKeys: String, CodingKey { case gate, approved, comment, at }
}

/// State of the last run of a task (`run.json`).
public struct RunState: Codable, Hashable, Sendable {
    public var runId: String
    public var taskId: String
    public var currentPhase: Name
    public var completedPhases: [Name]
    public var qaRound: UInt32
    public var startedAt: Date
    public var updatedAt: Date
    public var status: RunStatus
    public var validations: [ValidationResult]
    public var usage: Usage
    public var activeMs: UInt64
    public var pendingApproval: Name?
    public var approvals: [ApprovalRecord]
    public var lastError: String?

    enum CodingKeys: String, CodingKey {
        case status, validations, usage, approvals
        case runId = "run_id"
        case taskId = "task_id"
        case currentPhase = "current_phase"
        case completedPhases = "completed_phases"
        case qaRound = "qa_round"
        case startedAt = "started_at"
        case updatedAt = "updated_at"
        case activeMs = "active_ms"
        case pendingApproval = "pending_approval"
        case lastError = "last_error"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        runId = try c.decode(String.self, forKey: .runId)
        taskId = try c.decode(String.self, forKey: .taskId)
        currentPhase = try c.decode(Name.self, forKey: .currentPhase)
        completedPhases = try c.decode([Name].self, forKey: .completedPhases, default: [])
        qaRound = try c.decode(UInt32.self, forKey: .qaRound, default: 0)
        startedAt = try c.decode(Date.self, forKey: .startedAt)
        updatedAt = try c.decode(Date.self, forKey: .updatedAt)
        status = try c.decode(RunStatus.self, forKey: .status)
        validations = try c.decode([ValidationResult].self, forKey: .validations, default: [])
        usage = try c.decode(Usage.self, forKey: .usage, default: .zero)
        activeMs = try c.decode(UInt64.self, forKey: .activeMs, default: 0)
        pendingApproval = try c.decodeIfPresent(Name.self, forKey: .pendingApproval)
        approvals = try c.decode([ApprovalRecord].self, forKey: .approvals, default: [])
        lastError = try c.decodeIfPresent(String.self, forKey: .lastError)
    }
}

/// A requirement of the specification.
public struct Requirement: Codable, Hashable, Sendable, Identifiable {
    public var id: String
    public var description: String
    public var kind: Name
    public var priority: Int
    public var acceptance: [String]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        description = try c.decode(String.self, forKey: .description)
        kind = try c.decode(Name.self, forKey: .kind, default: "functional")
        priority = try c.decode(Int.self, forKey: .priority, default: 1)
        acceptance = try c.decode([String].self, forKey: .acceptance, default: [])
    }

    enum CodingKeys: String, CodingKey { case id, description, kind, priority, acceptance }
}

/// What must be built and how we know it is done.
public struct Spec: Codable, Hashable, Sendable {
    public var summary: String
    public var requirements: [Requirement]
    public var body: String

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        summary = try c.decode(String.self, forKey: .summary, default: "")
        requirements = try c.decode([Requirement].self, forKey: .requirements, default: [])
        body = try c.decode(String.self, forKey: .body, default: "")
    }

    enum CodingKeys: String, CodingKey { case summary, requirements, body }
}

/// A subtask of the plan.
public struct Subtask: Codable, Hashable, Sendable, Identifiable {
    public var id: String
    public var title: String
    public var description: String
    public var files: [String]
    public var status: Name
    public var attempts: UInt32

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decode(String.self, forKey: .title)
        description = try c.decode(String.self, forKey: .description, default: "")
        files = try c.decode([String].self, forKey: .files, default: [])
        status = try c.decode(Name.self, forKey: .status, default: "pending")
        attempts = try c.decode(UInt32.self, forKey: .attempts, default: 0)
    }

    enum CodingKeys: String, CodingKey { case id, title, description, files, status, attempts }
}

/// A group of subtasks of the plan.
public struct PlanPhase: Codable, Hashable, Sendable {
    public var name: String
    public var parallel: Bool
    public var subtasks: [Subtask]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        parallel = try c.decode(Bool.self, forKey: .parallel, default: false)
        subtasks = try c.decode([Subtask].self, forKey: .subtasks, default: [])
    }

    enum CodingKeys: String, CodingKey { case name, parallel, subtasks }
}

/// The ordered plan of subtasks.
public struct Plan: Codable, Hashable, Sendable {
    public var approach: String
    public var phases: [PlanPhase]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        approach = try c.decode(String.self, forKey: .approach, default: "")
        phases = try c.decode([PlanPhase].self, forKey: .phases, default: [])
    }

    enum CodingKeys: String, CodingKey { case approach, phases }

    public var subtasks: [Subtask] { phases.flatMap(\.subtasks) }
}

/// An issue found by QA.
public struct QaIssue: Codable, Hashable, Sendable {
    public var severity: Name
    public var title: String
    public var detail: String
    public var file: String?
    public var line: UInt32?
    public var suggestedFix: String?

    enum CodingKeys: String, CodingKey {
        case severity, title, detail, file, line
        case suggestedFix = "suggested_fix"
    }
}

/// A QA review.
public struct QaReport: Codable, Hashable, Sendable {
    public var round: UInt32
    public var verdict: Name
    public var summary: String
    public var issues: [QaIssue]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        round = try c.decode(UInt32.self, forKey: .round)
        verdict = try c.decode(Name.self, forKey: .verdict)
        summary = try c.decode(String.self, forKey: .summary, default: "")
        issues = try c.decode([QaIssue].self, forKey: .issues, default: [])
    }

    enum CodingKeys: String, CodingKey { case round, verdict, summary, issues }
}

/// `GET /api/tasks/{task}`: the row, with the full run state and the artefacts.
public struct TaskDetail: Codable, Hashable, Sendable, Identifiable {
    public var task: VibeTask
    public var number: Int?
    public var running: Bool
    public var run: RunState?
    public var spec: Spec?
    public var plan: Plan?
    public var qaReports: [QaReport]

    public var id: String { task.id }

    public var row: TaskRow {
        TaskRow(task: task, number: number, running: running, run: run.map {
            RunSummary(status: $0.status, currentPhase: $0.currentPhase, pendingApproval: $0.pendingApproval)
        })
    }

    enum CodingKeys: String, CodingKey {
        case task, number, running, run, spec, plan
        case qaReports = "qa_reports"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        task = try c.decode(VibeTask.self, forKey: .task)
        number = try c.decodeIfPresent(Int.self, forKey: .number)
        running = try c.decode(Bool.self, forKey: .running, default: false)
        run = try c.decodeIfPresent(RunState.self, forKey: .run)
        spec = try c.decodeIfPresent(Spec.self, forKey: .spec)
        plan = try c.decodeIfPresent(Plan.self, forKey: .plan)
        qaReports = try c.decode([QaReport].self, forKey: .qaReports, default: [])
    }

    /// Title of a subtask of the plan, or the start of its id.
    public func subtaskTitle(_ id: String) -> String {
        plan?.subtasks.first { $0.id == id }?.title ?? String(id.prefix(8))
    }
}

/// `GET /api/health`.
public struct Health: Codable, Hashable, Sendable {
    public var name: String
    public var version: String
}

/// Answer of `POST /run`.
public struct RunAccepted: Codable, Hashable, Sendable {
    public var taskId: String
    public var resumed: Bool

    enum CodingKeys: String, CodingKey {
        case resumed
        case taskId = "task_id"
    }
}

/// One line of an evaluation summary (`evals/summarize.py`).
public struct EvalRow: Codable, Hashable, Sendable {
    public var provider: String?
    public var model: String?
    public var `case`: String?
    public var runs: Double?
    public var successes: Double?
    public var successRate: Double?
    public var meanSeconds: Double?
    public var medianSeconds: Double?
    public var meanTokens: Double?

    enum CodingKeys: String, CodingKey {
        case provider, model, `case`, runs, successes
        case successRate = "success_rate"
        case meanSeconds = "mean_seconds"
        case medianSeconds = "median_seconds"
        case meanTokens = "mean_tokens"
    }
}

/// One `summary.json` found under `--evals`.
public struct EvalSuite: Codable, Hashable, Sendable, Identifiable {
    public var name: String
    public var modified: Date?
    /// The summary as written; `rows` is its typed table.
    public var summary: JSONValue

    public var id: String { name }

    /// Rows of the summary that decode; the others are skipped.
    public var rows: [EvalRow] {
        guard let items = summary["rows"]?.arrayValue else { return [] }
        let decoder = VibeJSON.decoder()
        return items.compactMap { item in
            guard let data = try? JSONEncoder().encode(item) else { return nil }
            return try? decoder.decode(EvalRow.self, from: data)
        }
    }
}

/// `GET /api/evals`.
public struct EvalsResponse: Codable, Hashable, Sendable {
    /// False when the server was started without `--evals`.
    public var enabled: Bool
    public var suites: [EvalSuite]
}
