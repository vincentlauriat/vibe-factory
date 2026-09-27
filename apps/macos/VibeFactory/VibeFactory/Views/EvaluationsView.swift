import SwiftUI
import VibeAPI

/// Evaluation suites found under `vibe serve --evals`, newest first.
struct EvaluationsListView: View {
    @Bindable var model: EvaluationsViewModel
    let reload: () async -> Void
    @Environment(AppSettings.self) private var settings

    var body: some View {
        Group {
            if let response = model.response, !response.suites.isEmpty {
                List(response.suites, selection: $model.selectedSuite) { suite in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(suite.name.isEmpty ? "." : suite.name).lineLimit(1).truncationMode(.middle)
                        if let modified = suite.modified {
                            Text(modified, format: .dateTime.day().month().year().hour().minute())
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .tag(suite.id)
                }
            } else if model.loading {
                ProgressView()
            } else {
                ContentUnavailableView(settings.t("evals_none"), systemImage: "chart.bar.xaxis",
                                       description: Text(settings.t(model.response?.enabled == false
                                                                    ? "evals_disabled" : "evals_none_desc")))
            }
        }
        .navigationTitle(settings.t("sidebar_evaluations"))
        .toolbar {
            Button { Task { await reload() } } label: {
                Label(settings.t("refresh"), systemImage: "arrow.clockwise")
            }
        }
        .task { await reload() }
    }
}

/// The rows of one summary: success, time and tokens per provider, model and case.
struct EvaluationTableView: View {
    let suite: EvalSuite?
    let enabled: Bool
    @Environment(AppSettings.self) private var settings

    var body: some View {
        if let suite {
            Table(suite.rows.enumerated().map { IndexedRow(id: $0.offset, element: $0.element) }) {
                TableColumn(settings.t("evals_case")) { Text($0.element.case ?? "-").fontWeight($0.element.case == "ALL" ? .bold : .regular) }
                TableColumn(settings.t("evals_provider")) { Text($0.element.provider ?? "-") }
                TableColumn(settings.t("evals_model")) { Text($0.element.model ?? "-") }
                TableColumn(settings.t("evals_success")) { item in
                    Text(success(item.element))
                }
                TableColumn(settings.t("evals_mean_time")) { Text(number($0.element.meanSeconds, suffix: " s")) }
                TableColumn(settings.t("evals_median_time")) { Text(number($0.element.medianSeconds, suffix: " s")) }
                TableColumn(settings.t("evals_tokens")) { item in
                    Text(item.element.meanTokens.map { Format.tokens(UInt64(max(0, $0))) } ?? "-")
                }
            }
            .navigationTitle(suite.name)
        } else {
            ContentUnavailableView(settings.t(enabled ? "evals_select" : "evals_disabled_title"),
                                   systemImage: "tablecells")
        }
    }

    private func success(_ row: EvalRow) -> String {
        guard let rate = row.successRate else { return "-" }
        let counts = row.successes.flatMap { s in row.runs.map { " (\(Int(s))/\(Int($0)))" } } ?? ""
        return "\(Int((rate * 100).rounded())) %" + counts
    }

    private func number(_ value: Double?, suffix: String) -> String {
        value.map { String(format: "%.1f", $0) + suffix } ?? "-"
    }
}

/// A summary row with its position, for `Table` (rows have no identity of their own).
private struct IndexedRow: Identifiable {
    let id: Int
    let element: EvalRow
}
