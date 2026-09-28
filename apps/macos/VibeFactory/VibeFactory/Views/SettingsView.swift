import AppKit
import SwiftUI
import VibeAPI

struct SettingsView: View {
    @Environment(AppSettings.self) private var settings
    @State private var newName = ""
    @State private var newValue = ""

    var body: some View {
        @Bindable var settings = settings

        Form {
            Section(settings.t("settings_vibe")) {
                HStack {
                    TextField(settings.t("settings_vibe_path"), text: $settings.vibePath,
                              prompt: Text(resolvedPath ?? "~/.cargo/bin/vibe"))
                    Button(settings.t("choose")) {
                        if let path = choose(directory: false) { settings.vibePath = path }
                    }
                }
                Text(resolvedPath.map { settings.t("settings_vibe_found", $0) } ?? settings.t("settings_vibe_missing"))
                    .font(.caption)
                    .foregroundStyle(resolvedPath == nil ? .red : .secondary)
                HStack {
                    TextField(settings.t("settings_evals"), text: $settings.evalsPath, prompt: Text("evals/results"))
                    Button(settings.t("choose")) {
                        if let path = choose(directory: true) { settings.evalsPath = path }
                    }
                }
                Text(settings.t("settings_restart_hint")).font(.caption).foregroundStyle(.secondary)
            }

            Section {
                ForEach(settings.environmentNames, id: \.self) { name in
                    HStack {
                        Text(name).font(.system(.body, design: .monospaced))
                        Spacer()
                        Text("••••••").foregroundStyle(.secondary)
                        Button(role: .destructive) { settings.setEnvironment(name, value: "") } label: {
                            Image(systemName: "minus.circle")
                        }
                        .buttonStyle(.borderless)
                        .help(settings.t("remove"))
                    }
                }
                HStack {
                    TextField(settings.t("env_name"), text: $newName, prompt: Text("ANTHROPIC_API_KEY"))
                        .font(.system(.body, design: .monospaced))
                    SecureField(settings.t("env_value"), text: $newValue)
                    Button(settings.t("add")) {
                        settings.setEnvironment(newName, value: newValue)
                        newName = ""
                        newValue = ""
                    }
                    .disabled(newName.trimmingCharacters(in: .whitespaces).isEmpty || newValue.isEmpty)
                }
            } header: {
                Text(settings.t("settings_env"))
            } footer: {
                Text(settings.t("settings_env_help")).font(.caption).foregroundStyle(.secondary)
            }

            Section(settings.t("settings_appearance")) {
                Picker(settings.t("settings_appearance"), selection: $settings.appearanceRaw) {
                    ForEach(AppearanceMode.allCases) { mode in
                        Text(settings.t(mode.titleKey)).tag(mode.rawValue)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
            }

            Section(settings.t("settings_language")) {
                Picker(settings.t("settings_language"), selection: $settings.languageRaw) {
                    ForEach(AppLanguage.allCases) { lang in
                        Text(lang == .system ? settings.t("language_system") : lang.nativeName)
                            .tag(lang.rawValue)
                    }
                }
                .labelsHidden()
            }

            Section(settings.t("settings_notifications")) {
                Toggle(settings.t("settings_notifications_toggle"), isOn: $settings.notificationsEnabled)
            }
        }
        .formStyle(.grouped)
        .frame(width: 560, height: 600)
    }

    private var resolvedPath: String? {
        ServerProcess.resolveExecutable(setting: settings.vibePath).map { ($0.path as NSString).abbreviatingWithTildeInPath }
    }

    private func choose(directory: Bool) -> String? {
        let panel = NSOpenPanel()
        panel.canChooseFiles = !directory
        panel.canChooseDirectories = directory
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK else { return nil }
        return panel.url?.path
    }
}
