import SwiftUI
import VibeAPI

/// The Activity entry of the sidebar. Until the server has a global stream
/// (`/api/stream`, Step 4) it shows the live activity of the selected task,
/// with the running tasks and pending approvals to pick from.
struct ActivityFeedView: View {
    @Bindable var model: ProjectViewModel
    @Environment(AppSettings.self) private var settings

    var body: some View {
        VStack(spacing: 0) {
            let live = liveRows
            if !live.isEmpty {
                List(live, selection: $model.selectedTaskId) { row in
                    TaskRowView(row: row).tag(row.id)
                }
                .frame(height: min(CGFloat(live.count) * 52 + 12, 220))
                Divider()
            }
            if let detail = model.detail {
                ActivityTab(model: detail)
            } else {
                ContentUnavailableView(settings.t("activity_title"), systemImage: "waveform.path.ecg",
                                       description: Text(settings.t("activity_soon")))
            }
        }
        .navigationTitle(settings.t("sidebar_activity"))
    }

    private var liveRows: [TaskRow] {
        model.session.rows.filter { $0.running || $0.run?.pendingApproval != nil }
    }
}
