import Foundation

/// A failed API call.
public enum VibeError: Error, Equatable, Sendable, LocalizedError {
    /// 401: missing or wrong token.
    case unauthorized
    /// 404: no task matches the reference (or unknown route).
    case notFound(String)
    /// Any other non-2xx answer; `message` is the server's `error` field or the body.
    case server(status: Int, message: String)
    /// The request did not reach the server or the answer did not decode.
    case transport(String)

    public var errorDescription: String? {
        switch self {
        case .unauthorized: "The server refused the token."
        case .notFound(let message): message
        case .server(let status, let message): "HTTP \(status): \(message)"
        case .transport(let message): message
        }
    }
}

/// Where a server is and how to authenticate to it.
public struct ServerEndpoint: Hashable, Sendable {
    /// `http://127.0.0.1:7777/` (with or without the trailing slash).
    public var baseURL: URL
    public var token: String

    public init(baseURL: URL, token: String) {
        self.baseURL = baseURL
        self.token = token
    }

    /// `baseURL` + `api/<path>`, with a query.
    public func url(_ path: String, query: [URLQueryItem] = []) -> URL {
        var components = URLComponents(url: baseURL, resolvingAgainstBaseURL: false)!
        let prefix = components.path.hasSuffix("/") ? components.path : components.path + "/"
        components.path = prefix + "api/" + path
        components.queryItems = query.isEmpty ? nil : query
        // A literal `+` (an RFC 3339 offset) would reach the server as a space.
        components.percentEncodedQuery = components.percentEncodedQuery?.replacingOccurrences(of: "+", with: "%2B")
        return components.url!
    }

    /// A request carrying `Authorization: Bearer <token>`.
    public func request(_ path: String, query: [URLQueryItem] = [], method: String = "GET") -> URLRequest {
        var request = URLRequest(url: url(path, query: query))
        request.httpMethod = method
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        return request
    }
}

/// Client of the `vibe serve` HTTP API (`docs/src/user/cli.md`).
public final class VibeClient: Sendable {
    public let endpoint: ServerEndpoint
    private let session: URLSession

    public init(endpoint: ServerEndpoint, session: URLSession = .shared) {
        self.endpoint = endpoint
        self.session = session
    }

    // MARK: Routes

    /// `GET /api/health`.
    public func health() async throws -> Health {
        try await get("health")
    }

    /// `GET /api/tasks`: every task with its run summary.
    public func tasks() async throws -> [TaskRow] {
        try await get("tasks")
    }

    /// `POST /api/tasks`.
    public func createTask(title: String, description: String = "") async throws -> TaskRow {
        try await send("tasks", body: ["title": title, "description": description])
    }

    /// `GET /api/tasks/{ref}`: the row, full run state, spec, plan, QA reports.
    public func task(_ ref: String) async throws -> TaskDetail {
        try await get("tasks/\(ref)")
    }

    /// `POST /api/tasks/{ref}/run`: start, or resume the last run.
    @discardableResult
    public func run(_ ref: String, resume: Bool = false) async throws -> RunAccepted {
        try await send("tasks/\(ref)/run", body: ["resume": resume])
    }

    /// `POST /api/tasks/{ref}/cancel`.
    public func cancel(_ ref: String) async throws {
        let _: JSONValue = try await send("tasks/\(ref)/cancel", body: [String: String]())
    }

    /// `POST /api/tasks/{ref}/approve`.
    @discardableResult
    public func approve(_ ref: String, comment: String = "") async throws -> ApprovalRecord {
        try await send("tasks/\(ref)/approve", body: ["comment": comment])
    }

    /// `POST /api/tasks/{ref}/reject`; the reason is handed to the agents.
    @discardableResult
    public func reject(_ ref: String, reason: String) async throws -> ApprovalRecord {
        try await send("tasks/\(ref)/reject", body: ["reason": reason])
    }

    /// `GET /api/tasks/{ref}/changes`: the diff summary of the workspace.
    public func changes(_ ref: String) async throws -> String {
        let body: [String: String] = try await get("tasks/\(ref)/changes")
        return body["changes"] ?? ""
    }

    /// `GET /api/tasks/{ref}/events`: logged events of the last run after
    /// `after`, or of every run with `all`.
    public func events(_ ref: String, after: UInt64 = 0, all: Bool = false) async throws -> [Envelope] {
        var query: [URLQueryItem] = []
        if after > 0 { query.append(URLQueryItem(name: "after", value: String(after))) }
        if all { query.append(URLQueryItem(name: "all", value: "true")) }
        return try await get("tasks/\(ref)/events", query: query)
    }

    /// `GET /api/evals`.
    public func evals() async throws -> EvalsResponse {
        try await get("evals")
    }

    /// `GET /api/events`: logged events of every task, oldest first, the
    /// last `limit` (server default 1000) after the filters, with the cursor
    /// to go on from and the number of logs that could not be read.
    public func events(after: EventCursor? = nil, since: String? = nil, types: [String] = [],
                       tasks: [String] = [], limit: Int? = nil) async throws -> EventsPage {
        let query = GlobalStream.Query(after: after, since: since, types: types, tasks: tasks)
        let (data, response) = try await fetch(endpoint.request("events", query: query.items(limit: limit)))
        let events: [TaggedEnvelope] = try decode(data, from: response)
        return EventsPage(
            events: events,
            cursor: response.value(forHTTPHeaderField: "X-Vibe-Cursor").flatMap(EventCursor.init),
            readErrors: response.value(forHTTPHeaderField: "X-Vibe-Read-Errors").flatMap { Int($0) } ?? 0)
    }

    /// Every task's events as they happen (`GET /api/stream`), after `after`
    /// (else `since`, else from now). The token goes in the header.
    public func globalStream(after: EventCursor? = nil, since: String? = nil, types: [String] = [],
                             tasks: [String] = [], backoff: Backoff = Backoff()) -> GlobalStream {
        GlobalStream(endpoint: endpoint, query: GlobalStream.Query(after: after, since: since, types: types, tasks: tasks),
                     session: session, backoff: backoff)
    }

    /// `GET /api/history`: finished tasks, most recent first; `all` adds the
    /// failed and cancelled ones.
    public func history(all: Bool = false) async throws -> [TaskHistory] {
        try await get("history", query: all ? [URLQueryItem(name: "all", value: "true")] : [])
    }

    /// `GET /api/history/{ref}`: the history of any task.
    public func history(task ref: String) async throws -> TaskHistory {
        try await get("history/\(ref)")
    }

    /// `GET /api/tasks/{ref}/trace`: the calls of the last run, of the run
    /// whose id starts with `run`, or of every run with `all` (the server
    /// answers one object, or a list with `all`; this is always a list).
    /// `.notFound` when the task has not been run or no run matches.
    public func trace(task ref: String, run: String? = nil, all: Bool = false) async throws -> [RunTrace] {
        var query: [URLQueryItem] = []
        if let run { query.append(URLQueryItem(name: "run", value: run)) }
        if all { query.append(URLQueryItem(name: "all", value: "true")) }
        if all { return try await get("tasks/\(ref)/trace", query: query) }
        let trace: RunTrace = try await get("tasks/\(ref)/trace", query: query)
        return [trace]
    }

    /// `GET /api/tasks/{ref}/trace/{call}/output`: the complete output of a
    /// call; `nil` when it was not traced, is gone, or the call is unknown.
    public func traceOutput(task ref: String, call: String) async throws -> CallOutput? {
        var request = endpoint.request("tasks/\(ref)/trace/\(call)/output")
        request.setValue("text/plain", forHTTPHeaderField: "Accept")
        do {
            let (data, response) = try await fetch(request)
            return CallOutput(text: String(decoding: data, as: UTF8.self),
                              truncated: response.value(forHTTPHeaderField: "X-Vibe-Truncated") == "true")
        } catch VibeError.notFound {
            return nil
        }
    }

    /// Live events of a task (`GET /api/tasks/{ref}/stream`).
    public func stream(_ ref: String, after: UInt64 = 0) -> EventStream {
        EventStream(endpoint: endpoint, path: "tasks/\(ref)/stream", after: after, session: session) {
            [self] in try await self.task(ref).run?.runId
        }
    }

    // MARK: Plumbing

    private func get<T: Decodable>(_ path: String, query: [URLQueryItem] = []) async throws -> T {
        try await perform(endpoint.request(path, query: query))
    }

    private func send<T: Decodable, B: Encodable>(_ path: String, body: B) async throws -> T {
        var request = endpoint.request(path, method: "POST")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try VibeJSON.encoder().encode(body)
        return try await perform(request)
    }

    private func perform<T: Decodable>(_ request: URLRequest) async throws -> T {
        let (data, response) = try await fetch(request)
        return try decode(data, from: response)
    }

    /// The body and headers of a 2xx answer; other answers become `VibeError`.
    private func fetch(_ request: URLRequest) async throws -> (Data, HTTPURLResponse) {
        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: request)
        } catch is CancellationError {
            throw CancellationError()
        } catch let error as URLError where error.code == .cancelled {
            // A cancelled task surfaces as URLError: report it as a cancellation.
            throw CancellationError()
        } catch {
            throw VibeError.transport(error.localizedDescription)
        }
        try Self.check(response, body: data)
        guard let http = response as? HTTPURLResponse else { throw VibeError.transport("not an HTTP answer") }
        return (data, http)
    }

    private func decode<T: Decodable>(_ data: Data, from response: HTTPURLResponse) throws -> T {
        do {
            return try VibeJSON.decoder().decode(T.self, from: data)
        } catch {
            throw VibeError.transport("unexpected answer from \(response.url?.path ?? ""): \(error)")
        }
    }

    /// Map a non-2xx answer to a `VibeError`.
    static func check(_ response: URLResponse, body: Data) throws {
        guard let http = response as? HTTPURLResponse else {
            throw VibeError.transport("not an HTTP answer")
        }
        guard !(200..<300).contains(http.statusCode) else { return }
        let message = (try? JSONDecoder().decode([String: String].self, from: body))?["error"]
            ?? String(decoding: body, as: UTF8.self)
        switch http.statusCode {
        case 401: throw VibeError.unauthorized
        case 404: throw VibeError.notFound(message)
        default: throw VibeError.server(status: http.statusCode, message: message)
        }
    }
}
