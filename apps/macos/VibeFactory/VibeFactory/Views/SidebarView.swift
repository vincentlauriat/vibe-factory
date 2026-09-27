import SwiftUI
import VibeAPI

/// Board columns with badge counts, then the project-wide views.
struct SidebarView: View {
    @Bindable var model: ProjectViewModel
    @Environment(AppSettings.self) private var settings

    var body: some View {
        List(selection: $model.sidebar) {
            Section(settings.t("sidebar_tasks")) {
                ForEach(columns, id: \.self) { column in
                    let rows = model.session.rows(in: column)
                    Label(settings.t(column.titleKey), systemImage: column.symbol)
                        .badge(rows.count)
                        .foregroundStyle(rows.contains { $0.run?.pendingApproval != nil } ? Color.orange : Color.primary)
                        .tag(SidebarItem.status(column))
                }
            }
            Section(settings.t("sidebar_project")) {
                Label(settings.t("sidebar_activity"), systemImage: "waveform.path.ecg")
                    .badge(model.session.runningRows.count)
                    .tag(SidebarItem.activity)
                Label(settings.t("sidebar_history"), systemImage: "clock.arrow.circlepath")
                    .tag(SidebarItem.history)
                Label(settings.t("sidebar_evaluations"), systemImage: "chart.bar.xaxis")
                    .tag(SidebarItem.evaluations)
            }
        }
        .listStyle(.sidebar)
        .safeAreaInset(edge: .bottom) { footer }
    }

    /// Statuses the app does not know appear only when a task has one.
    private var columns: [BoardColumn] {
        BoardColumn.allCases.filter { $0 != .other || !model.session.rows(in: .other).isEmpty }
    }

    private var footer: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(model.session.boardError == nil ? Color.green : Color.orange)
                .frame(width: 7, height: 7)
            Text(model.session.boardError ?? "vibe \(model.session.serverVersion ?? "")")
                .lineLimit(1)
                .truncationMode(.tail)
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .help(model.session.boardError ?? settings.t("connected"))
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Tasks of one board column.
struct TaskListView: View {
    @Bindable var model: ProjectViewModel
    let column: BoardColumn
    @Environment(AppSettings.self) private var settings

    var body: some View {
        let rows = model.session.rows(in: column)
        Group {
            if rows.isEmpty {
                ContentUnavailableView(settings.t("no_tasks"), systemImage: column.symbol,
                                       description: Text(settings.t("no_tasks_desc")))
            } else {
                List(rows, selection: $model.selectedTaskId) { row in
                    TaskRowView(row: row).tag(row.id)
                }
            }
        }
        .navigationTitle(settings.t(column.titleKey))
    }
}

struct TaskRowView: View {
    let row: TaskRow
    @Environment(AppSettings.self) private var settings

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(row.label)
                .font(.system(.callout, design: .monospaced))
                .foregroundStyle(.secondary)
            VStack(alignment: .leading, spacing: 3) {
                Text(row.task.title).lineLimit(2)
                if let run = row.run {
                    HStack(spacing: 6) {
                        if row.running { ProgressView().controlSize(.mini) }
                        Text("\(run.status.rawValue) · \(run.currentPhase.display)")
                        if let gate = run.pendingApproval {
                            Label(settings.t("awaiting", gate.display), systemImage: "hand.raised.fill")
                                .foregroundStyle(.orange)
                        }
                    }
                    .font(.caption)
                    .foregroundStyle(.secondary)
                }
            }
        }
        .padding(.vertical, 2)
    }
}

/// Placeholder until the server serves `/api/history` (Step 4).
struct HistoryPlaceholderView: View {
    @Environment(AppSettings.self) private var settings

    var body: some View {
        ContentUnavailableView(settings.t("history_title"), systemImage: "clock.arrow.circlepath",
                               description: Text(settings.t("history_soon")))
    }
}
