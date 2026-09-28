import SwiftUI

/// New task: a title and an optional description.
struct NewTaskSheet: View {
    let create: (_ title: String, _ description: String) -> Void
    @Environment(AppSettings.self) private var settings
    @Environment(\.dismiss) private var dismiss
    @State private var title = ""
    @State private var description = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(settings.t("action_new_task")).font(.headline)
            TextField(settings.t("task_title"), text: $title)
                .textFieldStyle(.roundedBorder)
            Text(settings.t("task_description")).font(.caption).foregroundStyle(.secondary)
            TextEditor(text: $description)
                .font(.body)
                .frame(minHeight: 140)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color.secondary.opacity(0.3)))
            HStack {
                Spacer()
                Button(settings.t("cancel")) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(settings.t("create")) {
                    create(title.trimmingCharacters(in: .whitespaces), description)
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(title.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .padding(20)
        .frame(width: 480)
    }
}

/// Approve with an optional comment, or reject with a required reason (the
/// agents redo their work from it).
struct DecisionSheet: View {
    let decision: ProjectViewModel.Decision
    let gate: String
    let submit: (String) -> Void
    @Environment(AppSettings.self) private var settings
    @Environment(\.dismiss) private var dismiss
    @State private var text = ""

    private var isReject: Bool { decision == .reject }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(settings.t(isReject ? "reject_title" : "approve_title", gate)).font(.headline)
            Text(settings.t(isReject ? "reject_help" : "approve_help"))
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            TextEditor(text: $text)
                .font(.body)
                .frame(minHeight: 110)
                .overlay(RoundedRectangle(cornerRadius: 6).stroke(Color.secondary.opacity(0.3)))
            HStack {
                Spacer()
                Button(settings.t("cancel")) { dismiss() }
                    .keyboardShortcut(.cancelAction)
                Button(settings.t(isReject ? "action_reject" : "action_approve")) {
                    submit(text.trimmingCharacters(in: .whitespacesAndNewlines))
                    dismiss()
                }
                .keyboardShortcut(.defaultAction)
                .disabled(isReject && text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
        .padding(20)
        .frame(width: 460)
    }
}
