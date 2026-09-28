import Foundation

// `GET /api/tasks/{task}/trace` (`vibe trace --json`,
// `crates/vibe-pipeline/src/trace.rs`) and the complete output of a call.

/// How a call was matched with its result.
public enum PairedBy: OpenEnum {
    /// By call id.
    case id
    /// By order, per role, tool and subtask (logs recorded before 0.5).
    case order
    /// A call that never returned, or a result without its call.
    case unmatched
    case unknown(String)

    public init(rawValue: String) {
        switch rawValue {
        case "id": self = .id
        case "order": self = .order
        case "unmatched": self = .unmatched
        default: self = .unknown(rawValue)
        }
    }

    public var rawValue: String {
        switch self {
        case .id: "id"
        case .order: "order"
        case .unmatched: "unmatched"
        case .unknown(let value): value
        }
    }
}

/// A tool call paired with its result (`Call` on the server).
public struct TraceCall: Codable, Hashable, Sendable {
    /// The id of calls logged before 0.5.
    public static let nilCall = "000000000000"

    public var run: String
    /// 12 hex digits; `000000000000` in logs recorded before 0.5.
    public var call: String
    public var role: String
    public var subtask: String?
    public var tool: String
    /// Complete arguments (`null` for a result without its call).
    public var input: JSONValue
    public var calledAt: Date
    public var returnedAt: Date?
    public var durationMs: UInt64?
    public var isError: Bool?
    public var exitCode: Int64?
    public var timedOut: Bool
    /// First characters of the output.
    public var preview: String
    /// Length of the complete output, in characters (0 when unknown).
    public var outputChars: UInt64
    /// Where the complete output is traced, if it is.
    public var outputFile: String?
    public var paired: PairedBy

    enum CodingKeys: String, CodingKey {
        case run, call, role, subtask, tool, input, preview, paired
        case calledAt = "called_at"
        case returnedAt = "returned_at"
        case durationMs = "duration_ms"
        case isError = "is_error"
        case exitCode = "exit_code"
        case timedOut = "timed_out"
        case outputChars = "output_chars"
        case outputFile = "output_file"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        run = try c.decode(String.self, forKey: .run)
        call = try c.decode(String.self, forKey: .call, default: Self.nilCall)
        role = try c.decode(String.self, forKey: .role)
        subtask = try c.decodeIfPresent(String.self, forKey: .subtask)
        tool = try c.decode(String.self, forKey: .tool)
        input = try c.decode(JSONValue.self, forKey: .input, default: .null)
        calledAt = try c.decode(Date.self, forKey: .calledAt)
        returnedAt = try c.decodeIfPresent(Date.self, forKey: .returnedAt)
        durationMs = try c.decodeIfPresent(UInt64.self, forKey: .durationMs)
        isError = try c.decodeIfPresent(Bool.self, forKey: .isError)
        exitCode = try c.decodeIfPresent(Int64.self, forKey: .exitCode)
        timedOut = try c.decode(Bool.self, forKey: .timedOut, default: false)
        preview = try c.decode(String.self, forKey: .preview, default: "")
        outputChars = try c.decode(UInt64.self, forKey: .outputChars, default: 0)
        outputFile = try c.decodeIfPresent(String.self, forKey: .outputFile)
        paired = try c.decode(PairedBy.self, forKey: .paired, default: .id)
    }

    /// Whether `GET …/trace/{call}/output` can serve it: a real call id and
    /// a traced output.
    public var hasOutput: Bool { call != Self.nilCall && outputFile != nil }
}

/// The tool calls of one run.
public struct RunTrace: Codable, Hashable, Sendable, Identifiable {
    public var run: String
    /// In the order they were made.
    public var calls: [TraceCall]
    /// Paths written without error, relative to the workspace root.
    public var filesWritten: [String]

    public var id: String { run }

    enum CodingKeys: String, CodingKey {
        case run, calls
        case filesWritten = "files_written"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        run = try c.decode(String.self, forKey: .run)
        calls = try c.decode([TraceCall].self, forKey: .calls, default: [])
        filesWritten = try c.decode([String].self, forKey: .filesWritten, default: [])
    }

    /// Calls that failed.
    public var errorCount: Int { calls.filter { $0.isError == true }.count }
}

/// `GET /api/tasks/{task}/trace/{call}/output`.
public struct CallOutput: Hashable, Sendable {
    public var text: String
    /// The server cut it at 8 MiB (`X-Vibe-Truncated`).
    public var truncated: Bool

    public init(text: String, truncated: Bool) {
        self.text = text
        self.truncated = truncated
    }
}
