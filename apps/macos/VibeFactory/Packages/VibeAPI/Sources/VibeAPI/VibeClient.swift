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
        do {
            return try VibeJSON.decoder().decode(T.self, from: data)
        } catch {
            throw VibeError.transport("unexpected answer from \(request.url?.path ?? ""): \(error)")
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
