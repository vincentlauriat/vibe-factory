import Foundation

// The project-wide events: `GET /api/events` and `GET /api/stream`
// (`crates/vibe-cli/src/server/api/read.rs`, `crates/vibe-pipeline/src/events_log.rs`).

/// Position of an event in the project-wide order: by time, then task
/// number, then sequence number. Written `<nanoseconds>-<number>-<seq>`
/// (`1790000000123456789-3-17`), the SSE `id` of the global stream and the
/// `X-Vibe-Cursor` of `/api/events`.
///
/// The time stays in nanoseconds: a `Date` (a `Double`) cannot hold them
/// exactly, and a cursor that does not format back identically would make a
/// resumed stream skip or repeat events. A cursor is only ever taken from
/// the server (an id or a header), never computed from `at`.
public struct EventCursor: Hashable, Comparable, Sendable, LosslessStringConvertible {
    /// Publication time, in nanoseconds since the epoch.
    public var nanos: Int64
    /// Task sequence number.
    public var number: UInt32
    /// Sequence number in the run (0 when the event has none).
    public var seq: UInt64

    public init(nanos: Int64, number: UInt32, seq: UInt64) {
        self.nanos = nanos
        self.number = number
        self.seq = seq
    }

    /// Parse the compact form, splitting from the right like the server
    /// (the time may be negative).
    public init?(_ description: String) {
        guard let seqDash = description.lastIndex(of: "-") else { return nil }
        let head = description[..<seqDash]
        guard let numberDash = head.lastIndex(of: "-"),
              let seq = UInt64(description[description.index(after: seqDash)...]),
              let number = UInt32(head[head.index(after: numberDash)...]),
              let nanos = Int64(head[..<numberDash])
        else { return nil }
        self.init(nanos: nanos, number: number, seq: seq)
    }

    public var description: String { "\(nanos)-\(number)-\(seq)" }

    /// Publication time, for display only.
    public var date: Date { Date(timeIntervalSince1970: Double(nanos) / 1_000_000_000) }

    public static func < (lhs: EventCursor, rhs: EventCursor) -> Bool {
        (lhs.nanos, lhs.number, lhs.seq) < (rhs.nanos, rhs.number, rhs.seq)
    }
}

/// An envelope with its task: `{"task", "number", "schema", "seq", "at", "event"}`.
public struct TaggedEnvelope: Codable, Hashable, Sendable {
    /// Task id.
    public var task: String
    /// Task sequence number (`3` in `003-add-login`).
    public var number: UInt32
    public var envelope: Envelope

    public init(task: String, number: UInt32, envelope: Envelope) {
        self.task = task
        self.number = number
        self.envelope = envelope
    }

    public var event: Event { envelope.event }
    /// `#3`.
    public var label: String { "#\(number)" }

    enum CodingKeys: String, CodingKey { case task, number }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        task = try c.decode(String.self, forKey: .task)
        number = try c.decode(UInt32.self, forKey: .number)
        envelope = try Envelope(from: decoder) // flattened
    }

    public func encode(to encoder: Encoder) throws {
        try envelope.encode(to: encoder)
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encode(task, forKey: .task)
        try c.encode(number, forKey: .number)
    }
}

/// `GET /api/events`: the events and the two headers.
public struct EventsPage: Hashable, Sendable {
    /// Oldest first.
    public var events: [TaggedEnvelope]
    /// `X-Vibe-Cursor`: the last event read before the filters, to go on
    /// from with `after`; `nil` when nothing was read.
    public var cursor: EventCursor?
    /// `X-Vibe-Read-Errors`: task logs that could not be read.
    public var readErrors: Int

    public init(events: [TaggedEnvelope], cursor: EventCursor?, readErrors: Int) {
        self.events = events
        self.cursor = cursor
        self.readErrors = readErrors
    }
}

/// One event of the global stream.
public struct GlobalEvent: Hashable, Sendable {
    /// Its SSE id; `nil` for streamed text (`agent_delta`), which is not
    /// logged and not replayed.
    public var cursor: EventCursor?
    public var tagged: TaggedEnvelope

    public init(cursor: EventCursor?, tagged: TaggedEnvelope) {
        self.cursor = cursor
        self.tagged = tagged
    }
}

/// Every task's events as they happen: `GET /api/stream`.
///
/// It starts after `after` (or after `since`, or from now without either),
/// and after a cut it resumes from the id of the last logged event it
/// received (`Last-Event-ID`, which the server prefers, and `after`). Ids are
/// not always increasing (another process's clock may be behind), so nothing
/// is dropped by comparing them: the server sends each event once per
/// connection.
public struct GlobalStream: Sendable {
    /// Starting point and filters.
    public struct Query: Hashable, Sendable {
        public var after: EventCursor?
        /// RFC 3339 (`2026-09-27T10:00:00Z`) or an age (`30m`), used when
        /// there is no `after`.
        public var since: String?
        /// Event types to keep (all when empty).
        public var types: [String]
        /// Task references to keep (all when empty).
        public var tasks: [String]

        public init(after: EventCursor? = nil, since: String? = nil, types: [String] = [], tasks: [String] = []) {
            self.after = after
            self.since = since
            self.types = types
            self.tasks = tasks
        }

        /// Query items of `/api/events` and `/api/stream`; `resumeFrom`
        /// replaces the starting point.
        func items(resumeFrom lastId: String = "", limit: Int? = nil) -> [URLQueryItem] {
            var items: [URLQueryItem] = []
            if !lastId.isEmpty {
                items.append(URLQueryItem(name: "after", value: lastId))
            } else if let after {
                items.append(URLQueryItem(name: "after", value: after.description))
            } else if let since {
                items.append(URLQueryItem(name: "since", value: since))
            }
            items += types.map { URLQueryItem(name: "type", value: $0) }
            items += tasks.map { URLQueryItem(name: "task", value: $0) }
            if let limit { items.append(URLQueryItem(name: "limit", value: String(limit))) }
            return items
        }
    }

    private let endpoint: ServerEndpoint
    private let query: Query
    private let session: URLSession
    private let backoff: Backoff

    public init(endpoint: ServerEndpoint, query: Query = Query(), session: URLSession = .shared,
                backoff: Backoff = Backoff()) {
        self.endpoint = endpoint
        self.query = query
        self.session = session
        self.backoff = backoff
    }

    /// Events until the consumer stops iterating (or on 401/404/other 4xx).
    public func events() -> AsyncThrowingStream<GlobalEvent, Error> {
        let endpoint = endpoint
        let query = query
        let connection = SSEConnection(session: session, backoff: backoff) { lastId in
            endpoint.request("stream", query: query.items(resumeFrom: lastId))
        }
        let messages = connection.messages()
        return AsyncThrowingStream { continuation in
            let task = Task {
                let decoder = VibeJSON.decoder()
                do {
                    for try await message in messages {
                        guard let tagged = try? decoder.decode(TaggedEnvelope.self, from: Data(message.data.utf8))
                        else { continue }
                        continuation.yield(GlobalEvent(cursor: message.id.flatMap(EventCursor.init), tagged: tagged))
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}

/// The groups of event types of the Activity filter, as in the web UI.
public enum EventGroup: String, CaseIterable, Hashable, Sendable {
    case agents, tools, phases, git, approvals, budget, logs

    public var types: [String] {
        switch self {
        case .agents: ["agent_started", "agent_text", "agent_delta", "agent_finished"]
        case .tools: ["tool_called", "tool_returned"]
        case .phases: ["run_started", "phase_started", "phase_finished", "subtask_updated", "validation_finished",
                       "artefact_written", "run_finished"]
        case .git: ["subtask_integrated", "committed", "merged"]
        case .approvals: ["approval_requested", "approval_resolved", "paused"]
        case .budget: ["budget_updated"]
        case .logs: ["retrying", "log"]
        }
    }

    /// The group of an event type; `nil` for a type this app does not know.
    public static func of(_ type: String) -> EventGroup? {
        allCases.first { $0.types.contains(type) }
    }
}

/// What the Activity feed shows: groups of event types and one task or all.
public struct ActivityFilter: Hashable, Sendable {
    public var groups: Set<EventGroup>
    /// Task id; `nil` for every task.
    public var task: String?

    public init(groups: Set<EventGroup> = Set(EventGroup.allCases), task: String? = nil) {
        self.groups = groups
        self.task = task
    }

    /// Types the app does not know go with the logs.
    public func keeps(_ event: TaggedEnvelope) -> Bool {
        keeps(task: event.task, type: event.event.typeName)
    }

    /// The same, from what a feed keeps of an event.
    public func keeps(task: String, type: String) -> Bool {
        if let selected = self.task, selected != task { return false }
        return groups.contains(EventGroup.of(type) ?? .logs)
    }
}
