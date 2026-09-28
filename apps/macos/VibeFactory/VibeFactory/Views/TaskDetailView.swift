import SwiftUI
import VibeAPI

/// Header, budget gauges, approval banner and the tabs of the selected task.
struct TaskDetailView: View {
    enum Tab: Hashable { case overview, activity, trace, changes }

    let model: TaskDetailViewModel
    let project: ProjectViewModel
    @Environment(AppSettings.self) private var settings
    @State private var tab: Tab = .overview

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let detail = model.detail {
                header(detail)
                    .padding([.horizontal, .top], 18)
                    .padding(.bottom, 10)
                if let gate = model.pendingGate {
                    ApprovalBanner(gate: gate.display, acting: model.acting,
                                   approve: { project.decision = .approve },
                                   reject: { project.decision = .reject })
                        .padding(.horizontal, 18)
                        .padding(.bottom, 10)
                }
                Picker("", selection: $tab) {
                    Text(settings.t("tab_overview")).tag(Tab.overview)
                    Text(settings.t("tab_activity")).tag(Tab.activity)
                    Text(settings.t("tab_trace")).tag(Tab.trace)
                    Text(settings.t("tab_changes")).tag(Tab.changes)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .padding(.horizontal, 18)
                .padding(.bottom, 8)
                Divider()
                switch tab {
                case .overview: OverviewTab(detail: detail)
                case .activity: ActivityTab(model: model)
                case .trace: TraceTab(model: model)
                case .changes: ChangesTab(model: model)
                }
            } else {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .alert(settings.t("error_title"), isPresented: Binding(
            get: { model.errorMessage != nil },
            set: { if !$0 { model.errorMessage = nil } }
        )) {
            Button(settings.t("ok")) { model.errorMessage = nil }
        } message: {
            Text(model.errorMessage ?? "")
        }
    }

    private func header(_ detail: TaskDetail) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(detail.row.label)
                    .font(.system(.title2, design: .monospaced))
                    .foregroundStyle(.secondary)
                Text(detail.task.title).font(.title2.bold()).textSelection(.enabled)
                StatusBadge(status: detail.task.status)
                if detail.running { ProgressView().controlSize(.small) }
            }
            if !detail.task.description.isEmpty {
                Text(detail.task.description)
                    .foregroundStyle(.secondary)
                    .lineLimit(4)
                    .textSelection(.enabled)
            }
            HStack(spacing: 14) {
                if let run = detail.run {
                    Label("\(run.status.rawValue) · \(run.currentPhase.display)", systemImage: "gearshape.2")
                    if let error = run.lastError, run.pendingApproval == nil {
                        Label(error, systemImage: "exclamationmark.triangle")
                            .foregroundStyle(.red)
                            .lineLimit(1)
                            .help(error)
                    }
                } else {
                    Text(settings.t("not_run"))
                }
                if let branch = detail.task.branch {
                    Label(branch, systemImage: "arrow.triangle.branch").lineLimit(1)
                }
            }
            .font(.callout)
            .foregroundStyle(.secondary)
            if let budget = model.budget {
                BudgetGauges(budget: budget)
            }
        }
    }
}

struct StatusBadge: View {
    let status: TaskStatus
    @Environment(AppSettings.self) private var settings

    var body: some View {
        Text(settings.t(BoardColumn(status).titleKey))
            .font(.caption.weight(.semibold))
            .padding(.horizontal, 8)
            .padding(.vertical, 2)
            .background(color.opacity(0.18), in: Capsule())
            .foregroundStyle(color)
    }

    private var color: Color {
        switch status {
        case .done, .ready: .green
        case .failed: .red
        case .review: .orange
        case .building, .planning: .blue
        default: .secondary
        }
    }
}

/// Tokens and active time against the limits of `budget_updated`.
struct BudgetGauges: View {
    let budget: TaskDetailViewModel.Budget
    @Environment(AppSettings.self) private var settings

    var body: some View {
        HStack(spacing: 24) {
            gauge(title: settings.t("budget_tokens"), value: Double(budget.tokens),
                  limit: budget.tokenLimit.map(Double.init),
                  text: Format.tokens(budget.tokens) + (budget.tokenLimit.map { " / " + Format.tokens($0) } ?? ""))
            gauge(title: settings.t("budget_time"), value: Double(budget.activeMs),
                  limit: budget.durationLimitMs.map(Double.init),
                  text: Format.duration(ms: budget.activeMs)
                    + (budget.durationLimitMs.map { " / " + Format.duration(ms: $0) } ?? ""))
        }
    }

    @ViewBuilder
    private func gauge(title: String, value: Double, limit: Double?, text: String) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text("\(title): \(text)").font(.caption).foregroundStyle(.secondary)
            if let limit, limit > 0 {
                ProgressView(value: min(value, limit), total: limit)
                    .tint(value / limit > 0.9 ? .red : value / limit > 0.7 ? .orange : .accentColor)
                    .frame(width: 180)
            }
        }
    }
}

struct ApprovalBanner: View {
    let gate: String
    let acting: Bool
    let approve: () -> Void
    let reject: () -> Void
    @Environment(AppSettings.self) private var settings

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: "hand.raised.fill").foregroundStyle(.orange).font(.title3)
            VStack(alignment: .leading, spacing: 2) {
                Text(settings.t("approval_waiting", gate)).font(.headline)
                Text(settings.t("approval_help")).font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            Button(settings.t("action_reject"), action: reject).disabled(acting)
            Button(settings.t("action_approve"), action: approve)
                .buttonStyle(.borderedProminent)
                .disabled(acting)
        }
        .padding(12)
        .background(Color.orange.opacity(0.12), in: RoundedRectangle(cornerRadius: 10))
    }
}

/// Spec, plan with subtasks and statuses, QA reports, validations.
struct OverviewTab: View {
    let detail: TaskDetail
    @Environment(AppSettings.self) private var settings

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 22) {
                section(settings.t("tab_spec")) {
                    if let spec = detail.spec {
                        Text(spec.summary).textSelection(.enabled)
                        ForEach(spec.requirements) { requirement in
                            VStack(alignment: .leading, spacing: 2) {
                                Text("**\(requirement.id)** \(requirement.description)")
                                ForEach(requirement.acceptance, id: \.self) { item in
                                    Text("✓ \(item)").font(.callout).foregroundStyle(.secondary)
                                }
                            }
                        }
                    } else {
                        muted(settings.t("no_spec"))
                    }
                }
                section(settings.t("tab_plan")) {
                    if let plan = detail.plan {
                        if !plan.approach.isEmpty { Text(plan.approach).textSelection(.enabled) }
                        ForEach(Array(plan.phases.enumerated()), id: \.offset) { _, phase in
                            Text(phase.name + (phase.parallel ? " (\(settings.t("parallel")))" : ""))
                                .font(.headline)
                            ForEach(phase.subtasks) { subtask in
                                HStack(alignment: .firstTextBaseline) {
                                    Image(systemName: icon(subtask.status.rawValue))
                                        .foregroundStyle(color(subtask.status.rawValue))
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(subtask.title)
                                        if !subtask.description.isEmpty {
                                            Text(subtask.description).font(.callout).foregroundStyle(.secondary)
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        muted(settings.t("no_plan"))
                    }
                }
                section(settings.t("tab_qa")) {
                    if detail.qaReports.isEmpty {
                        muted(settings.t("no_qa"))
                    }
                    ForEach(detail.qaReports.reversed(), id: \.round) { report in
                        Text("\(settings.t("qa_round")) \(report.round): \(report.verdict.display)").font(.headline)
                        if !report.summary.isEmpty { Text(report.summary).textSelection(.enabled) }
                        ForEach(Array(report.issues.enumerated()), id: \.offset) { _, issue in
                            VStack(alignment: .leading, spacing: 2) {
                                Text("[\(issue.severity.display)] **\(issue.title)**")
                                Text(issue.detail).font(.callout).foregroundStyle(.secondary)
                                if let file = issue.file {
                                    Text(file + (issue.line.map { ":\($0)" } ?? ""))
                                        .font(.system(.caption, design: .monospaced))
                                }
                            }
                        }
                    }
                }
                if let validations = detail.run?.validations, !validations.isEmpty {
                    section(settings.t("validations")) {
                        ForEach(Array(validations.enumerated()), id: \.offset) { _, validation in
                            Label(validation.command, systemImage: validation.passed ? "checkmark.circle" : "xmark.circle")
                                .foregroundStyle(validation.passed ? .green : .red)
                                .font(.system(.callout, design: .monospaced))
                        }
                    }
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(18)
        }
    }

    private func section<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title).font(.title3.bold())
            content()
        }
    }

    private func muted(_ text: String) -> some View {
        Text(text).foregroundStyle(.secondary)
    }

    private func icon(_ status: String) -> String {
        switch status {
        case "done": "checkmark.circle.fill"
        case "in_progress": "circle.dotted.circle"
        case "failed": "xmark.circle.fill"
        case "skipped": "minus.circle"
        default: "circle"
        }
    }

    private func color(_ status: String) -> Color {
        switch status {
        case "done": .green
        case "in_progress": .blue
        case "failed": .red
        default: .secondary
        }
    }
}

/// Events of the current run, described, with the streamed text of the current step.
struct ActivityTab: View {
    let model: TaskDetailViewModel
    @Environment(AppSettings.self) private var settings

    var body: some View {
        let lines = model.lines
        VStack(spacing: 0) {
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        if lines.isEmpty {
                            Text(settings.t("no_activity")).foregroundStyle(.secondary)
                        }
                        ForEach(lines) { line in
                            HStack(alignment: .firstTextBaseline, spacing: 8) {
                                Text(line.at, format: .dateTime.hour().minute().second())
                                    .foregroundStyle(.tertiary)
                                Text(line.text)
                                    .foregroundStyle(line.tone.color)
                                    .textSelection(.enabled)
                            }
                            .id(line.id)
                        }
                    }
                    .font(.system(.callout, design: .monospaced))
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(14)
                }
                .onChange(of: lines.last?.id) { _, id in
                    if let id { proxy.scrollTo(id, anchor: .bottom) }
                }
            }
            LivePanel(model: model)
        }
    }
}

/// The text streamed by the current step. Only this view reads `liveText`,
/// so an `agent_delta` redraws it and not the feed above.
private struct LivePanel: View {
    let model: TaskDetailViewModel

    var body: some View {
        if !model.liveText.isEmpty {
            Divider()
            ScrollView {
                Text("… " + model.liveText)
                    .font(.system(.callout, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(10)
            }
            .defaultScrollAnchor(.bottom)
            .frame(maxHeight: 160)
            .background(Color.accentColor.opacity(0.06))
        }
    }
}

/// `/changes`: the diff summary of the task workspace.
struct ChangesTab: View {
    let model: TaskDetailViewModel
    @Environment(AppSettings.self) private var settings

    var body: some View {
        ScrollView([.vertical, .horizontal]) {
            Group {
                if let changes = model.changes {
                    Text(changes.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                         ? settings.t("no_changes") : changes)
                } else {
                    Text(settings.t("loading"))
                }
            }
            .font(.system(.callout, design: .monospaced))
            .textSelection(.enabled)
            .frame(maxWidth: .infinity, alignment: .topLeading)
            .padding(14)
        }
        .overlay(alignment: .topTrailing) {
            Button { Task { await model.loadChanges() } } label: {
                Label(settings.t("refresh"), systemImage: "arrow.clockwise")
            }
            .disabled(model.loadingChanges)
            .padding(10)
        }
        .task { await model.loadChanges() }
    }
}
