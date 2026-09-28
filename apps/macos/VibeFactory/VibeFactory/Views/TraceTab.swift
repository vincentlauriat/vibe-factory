import SwiftUI
import VibeAPI

/// Every tool call of a run: role, subtask, tool, duration, exit code and
/// badges; a call expands to its arguments, its preview and, on demand, its
/// complete output. A picker chooses the run; the footer counts the calls
/// and errors and lists the files written.
struct TraceTab: View {
    let model: TaskDetailViewModel
    @Environment(AppSettings.self) private var settings

    var body: some View {
        let trace = model.trace
        VStack(spacing: 0) {
            HStack(spacing: 8) {
                if trace.runs.count > 1 {
                    Picker(settings.t("trace_run"), selection: Binding(
                        get: { trace.selectedRun ?? "" },
                        set: { trace.select(run: $0) })) {
                        ForEach(trace.runs) { run in
                            Text(settings.t("trace_run_choice", String(run.id.prefix(8)), "\(run.calls)")).tag(run.id)
                        }
                    }
                    .controlSize(.small)
                    .frame(maxWidth: 280)
                } else if let run = trace.selectedRun {
                    Text("\(settings.t("trace_run")) \(run.prefix(8))")
                        .font(.system(.callout, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                Spacer()
                if trace.loading { ProgressView().controlSize(.small) }
                Button { trace.reload() } label: {
                    Label(settings.t("refresh"), systemImage: "arrow.clockwise")
                }
                .controlSize(.small)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 8)
            Divider()
            if let run = trace.trace, !run.calls.isEmpty {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 0) {
                        ForEach(Array(run.calls.enumerated()), id: \.offset) { index, call in
                            CallView(index: index, call: call, trace: trace,
                                     subtask: call.subtask.map { model.detail?.subtaskTitle($0) ?? String($0.prefix(8)) })
                            Divider()
                        }
                    }
                }
                footer(run)
            } else if trace.notRun || trace.trace != nil {
                ContentUnavailableView(settings.t("trace_empty"), systemImage: "list.bullet.rectangle",
                                       description: Text(settings.t(trace.notRun ? "trace_not_run" : "trace_no_calls")))
            } else {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .onAppear { trace.appear() }
        .onDisappear { trace.stop() }
        .alert(settings.t("error_title"), isPresented: Binding(
            get: { trace.errorMessage != nil },
            set: { if !$0 { trace.errorMessage = nil } }
        )) {
            Button(settings.t("ok")) { trace.errorMessage = nil }
        } message: {
            Text(trace.errorMessage ?? "")
        }
    }

    private func footer(_ run: RunTrace) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Divider()
            Text(settings.t("trace_counts", "\(run.calls.count)", "\(run.errorCount)"))
            if !run.filesWritten.isEmpty {
                Text(settings.t("trace_files_written") + " " + run.filesWritten.joined(separator: ", "))
                    .lineLimit(3)
                    .textSelection(.enabled)
            }
        }
        .font(.caption)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 14)
        .padding(.bottom, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// One call: its line, and when expanded its arguments, preview and output.
private struct CallView: View {
    let index: Int
    let call: TraceCall
    let trace: TraceViewModel
    let subtask: String?
    @Environment(AppSettings.self) private var settings

    var body: some View {
        let expanded = trace.expanded.contains(index)
        VStack(alignment: .leading, spacing: 8) {
            Button { trace.toggle(index) } label: { header(expanded) }
                .buttonStyle(.plain)
            if expanded {
                block(settings.t("trace_arguments"), Format.short(call.input.prettyText, 20_000))
                if !call.preview.isEmpty { block(settings.t("trace_preview"), call.preview) }
                output
            }
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
    }

    private func header(_ expanded: Bool) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: expanded ? "chevron.down" : "chevron.right")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .frame(width: 10)
            Text("#\(index + 1)").foregroundStyle(.secondary)
            Text(call.role)
            if let subtask { Text("· \(subtask)").foregroundStyle(.secondary).lineLimit(1) }
            Text("· \(call.tool)").bold()
            Spacer(minLength: 6)
            if let ms = call.durationMs { Text(ms < 1_000 ? "\(ms) ms" : Format.duration(ms: ms)).foregroundStyle(.secondary) }
            if let code = call.exitCode { Text("exit \(code)").foregroundStyle(code == 0 ? Color.secondary : Color.orange) }
            if call.timedOut { badge(settings.t("trace_timed_out"), .orange) }
            if call.isError == true { badge(settings.t("trace_error"), .red) }
            switch call.paired {
            case .order: badge(settings.t("trace_paired_order"), .secondary)
            case .unmatched: badge(settings.t("trace_unmatched"), .orange)
            default: EmptyView()
            }
        }
        .font(.system(.callout, design: .monospaced))
        .contentShape(Rectangle())
    }

    @ViewBuilder
    private var output: some View {
        switch trace.outputs[index] {
        case nil:
            if call.hasOutput {
                Button(settings.t("trace_load_output")) { trace.loadOutput(index) }
                    .controlSize(.small)
                    .help(call.outputChars > 0 ? settings.t("trace_output_size", "\(call.outputChars)") : "")
            } else {
                note(settings.t("trace_not_traced"))
            }
        case .loading:
            ProgressView().controlSize(.small)
        case .missing:
            note(settings.t("trace_output_missing"))
        case .failed(let message):
            note(message)
        case .loaded(let text, let truncated, let clipped):
            VStack(alignment: .leading, spacing: 4) {
                ScrollView([.vertical, .horizontal]) {
                    Text(text)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .topLeading)
                        .padding(8)
                }
                .frame(maxHeight: 320)
                .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 6))
                if truncated { note(settings.t("trace_output_truncated")) }
                if clipped { note(settings.t("trace_output_clipped", "\(TraceViewModel.outputChars)")) }
            }
        }
    }

    private func block(_ title: String, _ text: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).font(.caption.weight(.semibold)).foregroundStyle(.secondary)
            Text(text)
                .font(.system(.caption, design: .monospaced))
                .textSelection(.enabled)
                .lineLimit(40)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(8)
                .background(Color.secondary.opacity(0.08), in: RoundedRectangle(cornerRadius: 6))
        }
    }

    private func note(_ text: String) -> some View {
        Text(text).font(.caption).foregroundStyle(.secondary)
    }

    private func badge(_ text: String, _ color: Color) -> some View {
        Text(text)
            .font(.caption2.weight(.semibold))
            .padding(.horizontal, 5)
            .padding(.vertical, 1)
            .background(color.opacity(0.18), in: Capsule())
            .foregroundStyle(color)
    }
}
