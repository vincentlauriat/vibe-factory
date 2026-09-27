import Foundation

/// An event in its envelope, as logged in `events.jsonl` and sent by the API.
public struct Envelope: Codable, Hashable, Sendable {
    /// Event format version (1 for logs written before 0.3).
    public var schema: UInt32
    /// Position in its run, from 1; absent for ephemeral events.
    public var seq: UInt64?
    public var at: Date
    public var event: Event

    public init(schema: UInt32 = 2, seq: UInt64?, at: Date, event: Event) {
        self.schema = schema
        self.seq = seq
        self.at = at
        self.event = event
    }

    enum CodingKeys: String, CodingKey { case schema, seq, at, event }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        schema = try c.decode(UInt32.self, forKey: .schema, default: 1)
        seq = try c.decodeIfPresent(UInt64.self, forKey: .seq)
        at = try c.decode(Date.self, forKey: .at)
        event = try c.decode(Event.self, forKey: .event)
    }
}

/// The kind of an artefact written by a run.
public enum Artefact: Hashable, Sendable {
    case spec
    case plan
    case qaReport(round: UInt32)
    case unknown(String)
}

/// Text arriving while a step streams.
public struct StreamDelta: Codable, Hashable, Sendable {
    /// `text` or `thinking`.
    public var kind: String
    public var text: String
}

/// Everything the engine publishes, tagged by `type` (see `events.md`).
///
/// Field names are those of the documentation. Fields added in 0.5 decode
/// with the documented defaults so that older logs load. A type this app does
/// not know, or a known type whose fields do not decode, becomes `.unknown`
/// with its payload: new event types never break a client.
public enum Event: Hashable, Sendable {
    case runStarted(run: String, task: String)
    case phaseStarted(run: String, phase: Name)
    case phaseFinished(run: String, phase: Name, success: Bool, summary: String)
    case agentStarted(run: String, role: String, subtask: String?, model: String)
    case agentDelta(run: String, role: String, subtask: String?, delta: StreamDelta)
    case agentText(run: String, role: String, text: String)
    case toolCalled(ToolCall)
    case toolReturned(ToolReturn)
    case agentFinished(run: String, role: String, steps: UInt32, usage: Usage, stop: String)
    case subtaskUpdated(run: String, subtask: String, status: Name)
    case subtaskIntegrated(run: String, subtask: String, commit: String?, conflicts: [String])
    case committed(run: String, subtask: String?, commit: String, message: String, files: [String])
    case merged(run: String, commit: String, branch: String, base: String)
    case validationFinished(run: String, command: String, integration: Bool, passed: Bool, exitCode: Int64?)
    case budgetUpdated(run: String, tokens: UInt64, tokenLimit: UInt64?, activeMs: UInt64, durationLimitMs: UInt64?)
    case artefactWritten(run: String, artefact: Artefact)
    case approvalRequested(run: String, gate: Name)
    case approvalResolved(run: String, gate: Name, approved: Bool, comment: String)
    case retrying(run: String, what: String, attempt: UInt32, delayMs: UInt64)
    case paused(run: String, reason: String)
    case runFinished(RunTotals)
    case log(run: String?, level: String, message: String)
    case unknown(type: String, payload: JSONValue)

    /// `tool_called`.
    public struct ToolCall: Hashable, Sendable {
        public var run: String
        public var role: String
        public var tool: String
        /// Complete arguments.
        public var input: JSONValue
        /// Pairs the call with its return; `000000000000` before 0.5.
        public var call: String
        public var subtask: String?
    }

    /// `tool_returned`.
    public struct ToolReturn: Hashable, Sendable {
        public var run: String
        public var role: String
        public var tool: String
        public var isError: Bool
        public var durationMs: UInt64
        /// First 200 characters of the output.
        public var preview: String
        public var call: String
        public var subtask: String?
        public var exitCode: Int64?
        public var timedOut: Bool
        public var outputChars: UInt64
        /// Complete output, relative to the project root.
        public var outputFile: String?
    }

    /// `run_finished`: totals of the whole run, resumes included.
    public struct RunTotals: Hashable, Sendable {
        public var run: String
        public var success: Bool
        public var status: TaskStatus
        public var usage: Usage
        public var activeMs: UInt64
        /// `1970-01-01T00:00:00Z` for logs written before 0.5.
        public var startedAt: Date
    }

    /// The `type` tag.
    public var typeName: String {
        switch self {
        case .runStarted: "run_started"
        case .phaseStarted: "phase_started"
        case .phaseFinished: "phase_finished"
        case .agentStarted: "agent_started"
        case .agentDelta: "agent_delta"
        case .agentText: "agent_text"
        case .toolCalled: "tool_called"
        case .toolReturned: "tool_returned"
        case .agentFinished: "agent_finished"
        case .subtaskUpdated: "subtask_updated"
        case .subtaskIntegrated: "subtask_integrated"
        case .committed: "committed"
        case .merged: "merged"
        case .validationFinished: "validation_finished"
        case .budgetUpdated: "budget_updated"
        case .artefactWritten: "artefact_written"
        case .approvalRequested: "approval_requested"
        case .approvalResolved: "approval_resolved"
        case .retrying: "retrying"
        case .paused: "paused"
        case .runFinished: "run_finished"
        case .log: "log"
        case .unknown(let type, _): type
        }
    }

    /// The run the event belongs to.
    public var runId: String? {
        switch self {
        case .runStarted(let run, _), .phaseStarted(let run, _), .phaseFinished(let run, _, _, _),
             .agentStarted(let run, _, _, _), .agentDelta(let run, _, _, _), .agentText(let run, _, _),
             .agentFinished(let run, _, _, _, _), .subtaskUpdated(let run, _, _),
             .subtaskIntegrated(let run, _, _, _), .committed(let run, _, _, _, _),
             .merged(let run, _, _, _), .validationFinished(let run, _, _, _, _),
             .budgetUpdated(let run, _, _, _, _), .artefactWritten(let run, _),
             .approvalRequested(let run, _), .approvalResolved(let run, _, _, _),
             .retrying(let run, _, _, _), .paused(let run, _):
            return run
        case .toolCalled(let call): return call.run
        case .toolReturned(let ret): return ret.run
        case .runFinished(let totals): return totals.run
        case .log(let run, _, _): return run
        case .unknown(_, let payload): return payload["run"]?.stringValue
        }
    }

    /// Only sent live, never logged.
    public var isEphemeral: Bool {
        if case .agentDelta = self { return true }
        return false
    }
}

extension Event: Codable {
    private struct Key: CodingKey {
        var stringValue: String
        var intValue: Int? { nil }
        init(_ string: String) { stringValue = string }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
    }

    public init(from decoder: Decoder) throws {
        let payload = try JSONValue(from: decoder)
        let type = payload["type"]?.stringValue ?? ""
        do {
            self = try Self.decodeKnown(type: type, from: decoder)
        } catch {
            self = .unknown(type: type, payload: payload)
        }
    }

    private static func decodeKnown(type: String, from decoder: Decoder) throws -> Event {
        let c = try decoder.container(keyedBy: Key.self)
        func req<T: Decodable>(_ key: String, _: T.Type = T.self) throws -> T {
            try c.decode(T.self, forKey: Key(key))
        }
        func opt<T: Decodable>(_ key: String, _: T.Type = T.self) throws -> T? {
            try c.decodeIfPresent(T.self, forKey: Key(key))
        }
        let run: () throws -> String = { try req("run") }
        switch type {
        case "run_started":
            return .runStarted(run: try run(), task: try req("task"))
        case "phase_started":
            return .phaseStarted(run: try run(), phase: try req("phase"))
        case "phase_finished":
            return .phaseFinished(run: try run(), phase: try req("phase"),
                                  success: try req("success"), summary: try opt("summary") ?? "")
        case "agent_started":
            return .agentStarted(run: try run(), role: try req("role"), subtask: try opt("subtask"),
                                 model: try opt("model") ?? "")
        case "agent_delta":
            return .agentDelta(run: try run(), role: try req("role"), subtask: try opt("subtask"),
                               delta: try req("delta"))
        case "agent_text":
            return .agentText(run: try run(), role: try req("role"), text: try req("text"))
        case "tool_called":
            return .toolCalled(ToolCall(
                run: try run(), role: try req("role"), tool: try req("tool"),
                input: try opt("input") ?? .null, call: try opt("call") ?? "000000000000",
                subtask: try opt("subtask")))
        case "tool_returned":
            return .toolReturned(ToolReturn(
                run: try run(), role: try req("role"), tool: try req("tool"),
                isError: try req("is_error"), durationMs: try req("duration_ms"),
                preview: try opt("preview") ?? "", call: try opt("call") ?? "000000000000",
                subtask: try opt("subtask"), exitCode: try opt("exit_code"),
                timedOut: try opt("timed_out") ?? false, outputChars: try opt("output_chars") ?? 0,
                outputFile: try opt("output_file")))
        case "agent_finished":
            return .agentFinished(run: try run(), role: try req("role"), steps: try req("steps"),
                                  usage: try opt("usage") ?? .zero, stop: try opt("stop") ?? "")
        case "subtask_updated":
            return .subtaskUpdated(run: try run(), subtask: try req("subtask"), status: try req("status"))
        case "subtask_integrated":
            return .subtaskIntegrated(run: try run(), subtask: try req("subtask"),
                                      commit: try opt("commit"), conflicts: try opt("conflicts") ?? [])
        case "committed":
            return .committed(run: try run(), subtask: try opt("subtask"), commit: try req("commit"),
                              message: try opt("message") ?? "", files: try opt("files") ?? [])
        case "merged":
            return .merged(run: try run(), commit: try req("commit"),
                           branch: try opt("branch") ?? "", base: try opt("base") ?? "")
        case "validation_finished":
            return .validationFinished(run: try run(), command: try req("command"),
                                       integration: try opt("integration") ?? false,
                                       passed: try req("passed"), exitCode: try opt("exit_code"))
        case "budget_updated":
            return .budgetUpdated(run: try run(), tokens: try req("tokens"),
                                  tokenLimit: try opt("token_limit"), activeMs: try opt("active_ms") ?? 0,
                                  durationLimitMs: try opt("duration_limit_ms"))
        case "artefact_written":
            let artefact: JSONValue = try req("artefact")
            let kind = artefact["kind"]?.stringValue ?? ""
            let value: Artefact
            switch kind {
            case "spec": value = .spec
            case "plan": value = .plan
            case "qa_report": value = .qaReport(round: UInt32(artefact["round"]?.doubleValue ?? 0))
            default: value = .unknown(kind)
            }
            return .artefactWritten(run: try run(), artefact: value)
        case "approval_requested":
            return .approvalRequested(run: try run(), gate: try req("gate"))
        case "approval_resolved":
            return .approvalResolved(run: try run(), gate: try req("gate"), approved: try req("approved"),
                                     comment: try opt("comment") ?? "")
        case "retrying":
            return .retrying(run: try run(), what: try req("what"), attempt: try req("attempt"),
                             delayMs: try opt("delay_ms") ?? 0)
        case "paused":
            return .paused(run: try run(), reason: try opt("reason") ?? "")
        case "run_finished":
            return .runFinished(RunTotals(
                run: try run(), success: try req("success"), status: try req("status"),
                usage: try opt("usage") ?? .zero, activeMs: try opt("active_ms") ?? 0,
                startedAt: try opt("started_at") ?? Date(timeIntervalSince1970: 0)))
        case "log":
            return .log(run: try opt("run"), level: try opt("level") ?? "info", message: try req("message"))
        default:
            throw DecodingError.dataCorrupted(
                .init(codingPath: decoder.codingPath, debugDescription: "unknown event type \(type)"))
        }
    }

    public func encode(to encoder: Encoder) throws {
        if case .unknown(_, let payload) = self {
            try payload.encode(to: encoder)
            return
        }
        var c = encoder.container(keyedBy: Key.self)
        func put<T: Encodable>(_ key: String, _ value: T) throws { try c.encode(value, forKey: Key(key)) }
        try put("type", typeName)
        switch self {
        case .runStarted(let run, let task):
            try put("run", run); try put("task", task)
        case .phaseStarted(let run, let phase):
            try put("run", run); try put("phase", phase)
        case .phaseFinished(let run, let phase, let success, let summary):
            try put("run", run); try put("phase", phase); try put("success", success); try put("summary", summary)
        case .agentStarted(let run, let role, let subtask, let model):
            try put("run", run); try put("role", role); try put("subtask", subtask); try put("model", model)
        case .agentDelta(let run, let role, let subtask, let delta):
            try put("run", run); try put("role", role); try put("subtask", subtask); try put("delta", delta)
        case .agentText(let run, let role, let text):
            try put("run", run); try put("role", role); try put("text", text)
        case .toolCalled(let x):
            try put("run", x.run); try put("role", x.role); try put("tool", x.tool)
            try put("input", x.input); try put("call", x.call); try put("subtask", x.subtask)
        case .toolReturned(let x):
            try put("run", x.run); try put("role", x.role); try put("tool", x.tool)
            try put("is_error", x.isError); try put("duration_ms", x.durationMs); try put("preview", x.preview)
            try put("call", x.call); try put("subtask", x.subtask); try put("exit_code", x.exitCode)
            try put("timed_out", x.timedOut); try put("output_chars", x.outputChars)
            try put("output_file", x.outputFile)
        case .agentFinished(let run, let role, let steps, let usage, let stop):
            try put("run", run); try put("role", role); try put("steps", steps)
            try put("usage", usage); try put("stop", stop)
        case .subtaskUpdated(let run, let subtask, let status):
            try put("run", run); try put("subtask", subtask); try put("status", status)
        case .subtaskIntegrated(let run, let subtask, let commit, let conflicts):
            try put("run", run); try put("subtask", subtask); try put("commit", commit)
            try put("conflicts", conflicts)
        case .committed(let run, let subtask, let commit, let message, let files):
            try put("run", run); try put("subtask", subtask); try put("commit", commit)
            try put("message", message); try put("files", files)
        case .merged(let run, let commit, let branch, let base):
            try put("run", run); try put("commit", commit); try put("branch", branch); try put("base", base)
        case .validationFinished(let run, let command, let integration, let passed, let exitCode):
            try put("run", run); try put("command", command); try put("integration", integration)
            try put("passed", passed); try put("exit_code", exitCode)
        case .budgetUpdated(let run, let tokens, let tokenLimit, let activeMs, let durationLimitMs):
            try put("run", run); try put("tokens", tokens); try put("token_limit", tokenLimit)
            try put("active_ms", activeMs); try put("duration_limit_ms", durationLimitMs)
        case .artefactWritten(let run, let artefact):
            try put("run", run)
            let value: JSONValue
            switch artefact {
            case .spec: value = .object(["kind": .string("spec")])
            case .plan: value = .object(["kind": .string("plan")])
            case .qaReport(let round): value = .object(["kind": .string("qa_report"), "round": .number(Double(round))])
            case .unknown(let kind): value = .object(["kind": .string(kind)])
            }
            try put("artefact", value)
        case .approvalRequested(let run, let gate):
            try put("run", run); try put("gate", gate)
        case .approvalResolved(let run, let gate, let approved, let comment):
            try put("run", run); try put("gate", gate); try put("approved", approved); try put("comment", comment)
        case .retrying(let run, let what, let attempt, let delayMs):
            try put("run", run); try put("what", what); try put("attempt", attempt); try put("delay_ms", delayMs)
        case .paused(let run, let reason):
            try put("run", run); try put("reason", reason)
        case .runFinished(let x):
            try put("run", x.run); try put("success", x.success); try put("status", x.status)
            try put("usage", x.usage); try put("active_ms", x.activeMs); try put("started_at", x.startedAt)
        case .log(let run, let level, let message):
            try put("run", run); try put("level", level); try put("message", message)
        case .unknown:
            break
        }
    }
}
