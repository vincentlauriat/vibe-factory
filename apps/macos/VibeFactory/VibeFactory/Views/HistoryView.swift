import SwiftUI
import VibeAPI

/// What each finished task delivered: the columns of `vibe history`, with
/// failed and cancelled tasks on demand. Reloaded when a run finishes.
struct HistoryView: View {
    @Bindable var model: HistoryViewModel
    let session: ProjectSession
    @Environment(AppSettings.self) private var settings

    /// Changes of either reload the list.
    private struct ReloadKey: Equatable {
        var all: Bool
        var finishedRuns: Int
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Toggle(settings.t("history_show_all"), isOn: $model.showAll)
                    .toggleStyle(.checkbox)
                Spacer()
                if model.loading { ProgressView().controlSize(.small) }
                Button { Task { await model.load(session.client) } } label: {
                    Label(settings.t("refresh"), systemImage: "arrow.clockwise")
                }
                .controlSize(.small)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 8)
            Divider()
            if model.loaded, model.histories.isEmpty {
                ContentUnavailableView(settings.t("history_title"), systemImage: "clock.arrow.circlepath",
                                       description: Text(settings.t(model.showAll ? "history_empty_all" : "history_empty")))
            } else {
                table
                Text(settings.t("history_notes"))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .navigationTitle(settings.t("sidebar_history"))
        .task(id: ReloadKey(all: model.showAll, finishedRuns: session.finishedRuns)) {
            await model.load(session.client)
        }
        .onDisappear { model.stop() }
        .alert(settings.t("error_title"), isPresented: Binding(
            get: { model.errorMessage != nil },
            set: { if !$0 { model.errorMessage = nil } }
        )) {
            Button(settings.t("ok")) { model.errorMessage = nil }
        } message: {
            Text(model.errorMessage ?? "")
        }
    }

    private var table: some View {
        Table(model.rows, selection: $model.selectedId) {
            TableColumn("#") { Text($0.number).monospacedDigit().foregroundStyle(.secondary) }
                .width(min: 24, ideal: 30, max: 44)
            TableColumn(settings.t("history_col_title")) { Text($0.title).lineLimit(1).help($0.title) }
                .width(min: 120, ideal: 220)
            TableColumn(settings.t("history_col_status")) { StatusBadge(status: $0.status) }
                .width(min: 70, ideal: 86)
            TableColumn(settings.t("history_col_runs")) { Text($0.runs).monospacedDigit() }
                .width(min: 30, ideal: 40)
            TableColumn(settings.t("history_col_commits")) { Text($0.commits).monospacedDigit() }
                .width(min: 40, ideal: 56)
            TableColumn(settings.t("history_col_files")) { Text($0.files).monospacedDigit() }
                .width(min: 34, ideal: 44)
            TableColumn(settings.t("history_col_tokens")) { Text($0.tokens).monospacedDigit() }
                .width(min: 50, ideal: 64)
            TableColumn(settings.t("history_col_active")) { Text($0.active).monospacedDigit() }
                .width(min: 60, ideal: 84)
            TableColumn(settings.t("history_col_cost")) { Text($0.cost).monospacedDigit() }
                .width(min: 50, ideal: 76)
            TableColumn(settings.t("history_col_finished")) { row in
                Text(row.finished, format: .relative(presentation: .named))
                    .foregroundStyle(.secondary)
                    .help(row.finished.formatted(date: .abbreviated, time: .shortened))
            }
            .width(min: 70, ideal: 96)
        }
    }
}

/// The detail of a task's history: totals, runs, commits, changed files,
/// validations, last QA and problems.
struct HistoryDetailView: View {
    let history: TaskHistory?
    @Environment(AppSettings.self) private var settings

    var body: some View {
        if let h = history {
            ScrollView {
                VStack(alignment: .leading, spacing: 20) {
                    header(h)
                    runs(h)
                    commits(h)
                    files(h)
                    validations(h)
                    qa(h)
                    if !h.errors.isEmpty {
                        section(settings.t("history_errors")) {
                            ForEach(h.errors, id: \.self) { error in
                                Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.orange)
                            }
                        }
                    }
                }
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(18)
            }
        } else {
            ContentUnavailableView(settings.t("history_select"), systemImage: "clock.arrow.circlepath",
                                   description: Text(settings.t("history_select_desc")))
        }
    }

    private func header(_ h: TaskHistory) -> some View {
        let row = HistoryRowText(h)
        return VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text("#\(h.number)").font(.system(.title2, design: .monospaced)).foregroundStyle(.secondary)
                Text(h.task.title).font(.title2.bold())
                StatusBadge(status: h.task.status)
            }
            if let branch = h.task.branch {
                Label(branch, systemImage: "arrow.triangle.branch").foregroundStyle(.secondary)
            }
            let tokens = Format.lowerBound(
                "\(Format.tokens(h.totals.usage.inputTokens)) in / \(Format.tokens(h.totals.usage.outputTokens)) out",
                complete: h.totals.complete)
            Text("\(settings.t("history_col_tokens")): \(tokens) · \(settings.t("history_col_active")): \(row.active) · "
                 + "\(settings.t("history_col_cost")): \(row.cost)")
                .font(.callout)
                .foregroundStyle(.secondary)
        }
    }

    private func runs(_ h: TaskHistory) -> some View {
        section(settings.t("history_runs") + " (\(h.runs.count))") {
            ForEach(h.runs) { run in
                VStack(alignment: .leading, spacing: 3) {
                    HStack(spacing: 8) {
                        Text(run.run.prefix(8)).font(.system(.callout, design: .monospaced))
                        RunStateBadge(state: run.state)
                        if let status = run.status { Text("→ " + settings.t(BoardColumn(status).titleKey)) }
                        Text(run.startedAt, format: .dateTime.day().month().hour().minute())
                            .foregroundStyle(.secondary)
                        if run.resumes > 0 { Text(settings.t("history_resumes", "\(run.resumes)")).foregroundStyle(.secondary) }
                    }
                    Text(run.totalsKnown
                         ? "\(Format.duration(ms: run.activeMs)) · \(Format.tokens(run.usage.total)) tokens"
                         : settings.t("history_totals_unknown"))
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    if !run.phases.isEmpty {
                        Text(run.phases.map { "\($0.phase.display) \($0.success == true ? "✓" : $0.success == false ? "✗" : "…")" }
                            .joined(separator: "  "))
                            .font(.system(.caption, design: .monospaced))
                            .foregroundStyle(.secondary)
                    }
                    if let merged = run.merged {
                        Label("\(merged.branch) → \(merged.base) (\(merged.commit.prefix(8)))",
                              systemImage: "arrow.triangle.merge")
                            .font(.callout)
                            .foregroundStyle(.green)
                    }
                    if let gate = run.pendingApproval {
                        Label(settings.t("awaiting", gate.display), systemImage: "hand.raised.fill")
                            .font(.callout)
                            .foregroundStyle(.orange)
                    }
                    if let error = run.lastError {
                        Label(error, systemImage: "exclamationmark.triangle").font(.callout).foregroundStyle(.red)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func commits(_ h: TaskHistory) -> some View {
        let commits = h.runs.flatMap(\.commits)
        section(settings.t("history_commits") + " (\(commits.count))") {
            if commits.isEmpty { Text(settings.t("history_none")).foregroundStyle(.secondary) }
            ForEach(Array(commits.enumerated()), id: \.offset) { _, commit in
                DisclosureGroup {
                    ForEach(commit.files, id: \.self) { file in
                        Text(file).font(.system(.caption, design: .monospaced)).foregroundStyle(.secondary)
                    }
                } label: {
                    HStack(spacing: 8) {
                        Text(commit.commit.prefix(8)).font(.system(.callout, design: .monospaced))
                        Text(commit.message.isEmpty ? "—" : commit.message).lineLimit(1)
                        Text(settings.t("history_n_files", "\(commit.files.count)")).foregroundStyle(.secondary)
                        if let subtask = commit.subtask {
                            Text(subtask.prefix(8)).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
                .disabled(commit.files.isEmpty)
            }
        }
    }

    private func files(_ h: TaskHistory) -> some View {
        section(settings.t("history_files") + " (\(h.changedFiles.files.count))") {
            HStack(spacing: 8) {
                Text(source(h.changedFiles.source)).foregroundStyle(.secondary)
                if h.changedFiles.approximate {
                    Text(settings.t("history_approximate"))
                        .font(.caption.weight(.semibold))
                        .padding(.horizontal, 6)
                        .padding(.vertical, 1)
                        .background(Color.orange.opacity(0.18), in: Capsule())
                        .foregroundStyle(.orange)
                }
            }
            .font(.callout)
            ForEach(h.changedFiles.files, id: \.path) { file in
                HStack(spacing: 8) {
                    Text(file.status.letter).bold().foregroundStyle(color(file.status)).frame(width: 14)
                    Text(file.path)
                    if let old = file.oldPath { Text("← " + old).foregroundStyle(.secondary) }
                }
                .font(.system(.callout, design: .monospaced))
            }
        }
    }

    @ViewBuilder
    private func validations(_ h: TaskHistory) -> some View {
        // The validations of the last run that ran any.
        if let run = h.runs.last(where: { !$0.validations.isEmpty }) {
            section(settings.t("validations")) {
                ForEach(Array(run.validations.enumerated()), id: \.offset) { _, validation in
                    Label(validation.command + (validation.exitCode.map { "  [\($0)]" } ?? "")
                          + (validation.integration ? "  (" + settings.t("history_integration") + ")" : ""),
                          systemImage: validation.passed ? "checkmark.circle" : "xmark.circle")
                        .foregroundStyle(validation.passed ? .green : .red)
                        .font(.system(.callout, design: .monospaced))
                }
            }
        }
    }

    @ViewBuilder
    private func qa(_ h: TaskHistory) -> some View {
        if let qa = h.lastQa {
            section(settings.t("history_last_qa")) {
                Text("\(settings.t("qa_round")) \(qa.round): \(qa.verdict.display) · "
                     + settings.t("history_issues", "\(qa.issues)"))
                    .font(.headline)
                if !qa.summary.isEmpty { Text(qa.summary) }
            }
        }
    }

    private func section<Content: View>(_ title: String, @ViewBuilder content: () -> Content) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title).font(.title3.bold())
            content()
        }
    }

    private func source(_ source: ChangedFilesSource) -> String {
        switch source {
        case .branch(let branch, let base): settings.t("source_branch", branch, base)
        case .mergeCommit(let commit, let fastForward):
            settings.t("source_merge", String(commit.prefix(8))) + (fastForward ? " (fast-forward)" : "")
        case .commits: settings.t("source_commits")
        case .trace: settings.t("source_trace")
        case .none: settings.t("source_none")
        case .unknown(let kind): kind
        }
    }

    private func color(_ status: FileStatus) -> Color {
        switch status {
        case .added: .green
        case .deleted: .red
        case .modified, .renamed, .copied: .orange
        case .unknown, .other: .secondary
        }
    }
}

struct RunStateBadge: View {
    let state: RunSummaryState
    @Environment(AppSettings.self) private var settings

    var body: some View {
        Text(label)
            .font(.caption.weight(.semibold))
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(color.opacity(0.18), in: Capsule())
            .foregroundStyle(color)
    }

    private var label: String {
        switch state {
        case .running: settings.t("run_state_running")
        case .finished: settings.t("run_state_finished")
        case .interrupted: settings.t("run_state_interrupted")
        case .unknown(let value): value
        }
    }

    private var color: Color {
        switch state {
        case .running: .blue
        case .finished: .green
        case .interrupted: .red
        case .unknown: .secondary
        }
    }
}
