import Foundation
import VibeAPI

/// One line of the Activity tab: an event described like the web UI's
/// `describe` (crates/vibe-cli/src/server/web/index.html).
struct ActivityLine: Identifiable, Hashable {
    enum Tone: Hashable { case normal, dim, good, warn, bad }

    let id: Int
    let at: Date
    let tone: Tone
    let text: String

    /// `nil` for events the feed does not show (deltas, empty texts, and
    /// unless `verbose`, budget updates, successful tool returns and info
    /// logs). The project-wide feed is verbose: its type filter decides.
    static func describe(_ envelope: Envelope, id: Int, detail: TaskDetail?, verbose: Bool = false,
                         t: (String) -> String) -> ActivityLine? {
        func line(_ tone: Tone, _ text: String) -> ActivityLine {
            ActivityLine(id: id, at: envelope.at, tone: tone, text: text)
        }
        func subtask(_ id: String) -> String { detail?.subtaskTitle(id) ?? String(id.prefix(8)) }

        switch envelope.event {
        case .runStarted:
            return line(.normal, "▶ " + t("ev_run_started"))
        case .phaseStarted(_, let phase):
            return line(.normal, "● " + phase.display)
        case .phaseFinished(_, let phase, let success, let summary):
            return line(success ? .good : .warn, "  \(phase.display) — \(summary)")
        case .agentStarted(_, let role, _, let model):
            return line(.dim, "  \(role) " + t("ev_started") + (model.isEmpty ? "" : " (\(model))"))
        case .agentText(_, let role, let text):
            let flat = text.split(whereSeparator: \.isWhitespace).joined(separator: " ")
            return flat.isEmpty ? nil : line(.normal, "  \(role): \(Format.short(flat, 240))")
        case .toolCalled(let call):
            return line(.dim, "    → \(call.tool) \(Format.short(call.input.compactText, 160))")
        case .toolReturned(let ret):
            guard ret.isError else {
                return verbose ? line(.dim, "    ← \(ret.tool) \(Format.duration(ms: ret.durationMs))") : nil
            }
            let code = ret.exitCode.map { " [\($0)]" } ?? ""
            return line(.warn, "    ← \(ret.tool)\(code) " + t("ev_failed") + ": \(Format.short(ret.preview, 160))")
        case .agentFinished(_, let role, _, let usage, _):
            return line(.dim, "  \(role) " + t("ev_finished") + " (\(Format.tokens(usage.total)) tokens)")
        case .subtaskUpdated(_, let id, let status):
            return line(.normal, "  ▸ \(subtask(id)): \(status.display)")
        case .subtaskIntegrated(_, let id, _, let conflicts):
            return conflicts.isEmpty ? nil
                : line(.warn, "  ▸ \(subtask(id)): " + t("ev_conflicts") + " \(conflicts.joined(separator: ", "))")
        case .committed(_, _, let commit, let message, let files):
            return line(.dim, "  ⎇ \(commit.prefix(8)) \(message) (\(files.count))")
        case .merged(_, let commit, let branch, let base):
            return line(.good, "  ⎇ " + t("ev_merged") + " \(branch) → \(base) (\(commit.prefix(8)))")
        case .validationFinished(_, let command, _, let passed, _):
            return line(passed ? .good : .bad, (passed ? "  ✓ " : "  ✗ ") + command)
        case .budgetUpdated(_, let tokens, let tokenLimit, let activeMs, let durationLimitMs):
            guard verbose else { return nil }
            let tokenText = Format.tokens(tokens) + (tokenLimit.map { " / " + Format.tokens($0) } ?? "")
            let timeText = Format.duration(ms: activeMs) + (durationLimitMs.map { " / " + Format.duration(ms: $0) } ?? "")
            return line(.dim, "  " + t("budget_tokens") + " \(tokenText) · " + t("budget_time") + " \(timeText)")
        case .artefactWritten(_, let artefact):
            let name: String
            switch artefact {
            case .spec: name = t("tab_spec")
            case .plan: name = t("tab_plan")
            case .qaReport(let round): name = t("qa_round") + " \(round)"
            case .unknown(let kind): name = kind
            }
            return line(.dim, "  " + t("ev_wrote") + " " + name)
        case .approvalRequested(_, let gate):
            return line(.warn, "⏸ " + t("ev_approval_needed") + " " + gate.display)
        case .approvalResolved(_, let gate, let approved, let comment):
            let verdict = t(approved ? "ev_approved" : "ev_rejected")
            return line(.good, "  \(gate.display) \(verdict)" + (comment.isEmpty ? "" : ": \(comment)"))
        case .retrying(_, let what, let attempt, _):
            return line(.warn, "  ↻ " + t("ev_retrying") + " \(what) (\(attempt))")
        case .paused(_, let reason):
            return line(.warn, "⏸ " + reason)
        case .runFinished(let totals):
            let tokens = totals.usage.total > 0 ? " · \(Format.tokens(totals.usage.total)) tokens" : ""
            let time = totals.activeMs > 0 ? " · \(Format.duration(ms: totals.activeMs))" : ""
            return line(totals.success ? .good : .warn,
                        "■ " + t("ev_run_finished") + ": \(totals.status.rawValue)\(tokens)\(time)")
        case .log(_, let level, let message):
            guard level != "info" else { return verbose ? line(.dim, "  " + message) : nil }
            return line(level == "error" ? .bad : .warn, "  ! " + message)
        case .unknown(let type, _):
            return verbose ? line(.dim, "  " + type) : nil
        case .agentDelta:
            return nil
        }
    }
}
