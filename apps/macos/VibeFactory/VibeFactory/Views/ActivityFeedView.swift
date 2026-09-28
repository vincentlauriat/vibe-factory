import SwiftUI
import VibeAPI

/// The Activity entry of the sidebar: every task's events as they happen,
/// from the project stream, with the web UI's type groups, a task filter
/// and a pause. Clicking a line shows its task in the detail column.
struct ActivityFeedView: View {
    @Bindable var model: ProjectViewModel
    @Environment(AppSettings.self) private var settings

    var body: some View {
        let activity = model.activity
        let feed = model.session.feed
        let entries = activity.visible(feed)
        VStack(spacing: 0) {
            ActivityFilterBar(activity: activity, feed: feed, rows: model.session.rows)
            Divider()
            if entries.isEmpty {
                ContentUnavailableView(settings.t("activity_title"), systemImage: "waveform.path.ecg",
                                       description: Text(settings.t(model.session.streamUp
                                                                     ? "activity_empty" : "activity_down")))
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 1) {
                            ForEach(entries) { entry in
                                FeedRow(entry: entry, selected: entry.task == model.selectedTaskId) {
                                    model.selectedTaskId = entry.task
                                }
                                .id(entry.id)
                            }
                        }
                        .padding(.vertical, 6)
                    }
                    .onChange(of: entries.last?.id) { _, id in
                        if let id, !activity.paused { proxy.scrollTo(id, anchor: .bottom) }
                    }
                    .onAppear {
                        if let id = entries.last?.id { proxy.scrollTo(id, anchor: .bottom) }
                    }
                }
            }
            if let error = model.session.streamError {
                Divider()
                Label(error, systemImage: "exclamationmark.triangle")
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .lineLimit(2)
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
        }
        .navigationTitle(settings.t("sidebar_activity"))
    }
}

/// Group chips, the task picker and the pause button.
private struct ActivityFilterBar: View {
    @Bindable var activity: ActivityViewModel
    let feed: ProjectFeed
    let rows: [TaskRow]
    @Environment(AppSettings.self) private var settings

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 4) {
                    ForEach(EventGroup.allCases, id: \.self) { group in
                        Toggle(settings.t("group_" + group.rawValue), isOn: Binding(
                            get: { activity.filter.groups.contains(group) },
                            set: { _ in activity.toggle(group) }))
                            .toggleStyle(.button)
                            .controlSize(.small)
                            .help(group.types.joined(separator: ", "))
                    }
                }
            }
            HStack(spacing: 8) {
                Picker(settings.t("activity_task"), selection: $activity.filter.task) {
                    Text(settings.t("activity_all_tasks")).tag(String?.none)
                    ForEach(rows.sorted { ($0.number ?? .max) < ($1.number ?? .max) }) { row in
                        Text("\(row.label) \(row.task.title)").lineLimit(1).tag(Optional(row.id))
                    }
                }
                .labelsHidden()
                .controlSize(.small)
                Spacer(minLength: 0)
                let held = activity.held(feed)
                if held > 0 {
                    Text(settings.t("activity_held", "\(held)"))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Button { activity.togglePause(feed) } label: {
                    Label(settings.t(activity.paused ? "activity_resume" : "activity_pause"),
                          systemImage: activity.paused ? "play.fill" : "pause.fill")
                }
                .controlSize(.small)
                .help(settings.t("activity_pause_help"))
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 8)
    }
}

/// One line of the project feed: time, task number, description.
private struct FeedRow: View {
    let entry: ProjectFeed.Entry
    let selected: Bool
    let select: () -> Void

    var body: some View {
        Button(action: select) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(entry.line.at, format: .dateTime.hour().minute().second())
                    .foregroundStyle(.tertiary)
                Text("#\(entry.number)")
                    .foregroundStyle(.secondary)
                    .frame(minWidth: 24, alignment: .trailing)
                Text(entry.line.text)
                    .foregroundStyle(entry.line.tone.color)
                    .lineLimit(3)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .font(.system(.caption, design: .monospaced))
            .padding(.horizontal, 10)
            .padding(.vertical, 2)
            .contentShape(Rectangle())
            .background(selected ? Color.accentColor.opacity(0.12) : Color.clear)
        }
        .buttonStyle(.plain)
    }
}

extension ActivityLine.Tone {
    var color: Color {
        switch self {
        case .normal: .primary
        case .dim: .secondary
        case .good: .green
        case .warn: .orange
        case .bad: .red
        }
    }
}
