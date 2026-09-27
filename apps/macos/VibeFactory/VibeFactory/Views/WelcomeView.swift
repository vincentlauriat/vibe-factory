import SwiftUI
import UniformTypeIdentifiers

/// Recent projects, Open…, a drop zone for folders, and "Connect to a running server".
struct WelcomeView: View {
    @Environment(AppSettings.self) private var settings
    @Environment(RecentProjects.self) private var recents
    @Environment(\.openWindow) private var openWindow
    @Environment(\.dismissWindow) private var dismissWindow
    @State private var vm = WelcomeViewModel()
    @State private var dropTargeted = false
    @State private var showingRemote = false

    var body: some View {
        HStack(spacing: 0) {
            intro
                .frame(width: 280)
                .padding(28)
            Divider()
            recentList
                .frame(width: 340)
        }
        .frame(height: 420)
        .onDrop(of: [.fileURL], isTargeted: $dropTargeted, perform: drop)
        .overlay {
            if dropTargeted {
                RoundedRectangle(cornerRadius: 12)
                    .stroke(Color.accentColor, style: StrokeStyle(lineWidth: 3, dash: [8]))
                    .padding(6)
            }
        }
        .sheet(isPresented: $showingRemote) { remoteSheet }
        .alert(settings.t("error_title"), isPresented: errorShown) {
            Button(settings.t("ok")) { vm.errorMessage = nil }
        } message: {
            Text(vm.errorMessage ?? "")
        }
    }

    private var errorShown: Binding<Bool> {
        Binding(get: { vm.errorMessage != nil && !showingRemote }, set: { if !$0 { vm.errorMessage = nil } })
    }

    private var intro: some View {
        VStack(alignment: .leading, spacing: 14) {
            Image(nsImage: NSApplication.shared.applicationIconImage)
                .resizable()
                .frame(width: 72, height: 72)
            Text("Vibe Factory").font(.largeTitle.bold())
            Text(settings.t("welcome_subtitle"))
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Spacer()
            Button {
                if let ref = vm.chooseFolder(prompt: settings.t("open")) { open(ref) }
            } label: {
                Label(settings.t("welcome_open"), systemImage: "folder")
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .controlSize(.large)
            .keyboardShortcut(.defaultAction)
            Button {
                showingRemote = true
            } label: {
                Label(settings.t("welcome_connect"), systemImage: "network")
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .controlSize(.large)
            Text(settings.t("welcome_drop_hint"))
                .font(.caption)
                .foregroundStyle(.tertiary)
        }
    }

    private var recentList: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text(settings.t("welcome_recent"))
                .font(.headline)
                .padding([.horizontal, .top], 16)
                .padding(.bottom, 8)
            if recents.items.isEmpty {
                ContentUnavailableView(settings.t("welcome_no_recent"), systemImage: "clock",
                                       description: Text(settings.t("welcome_no_recent_desc")))
            } else {
                List {
                    ForEach(recents.items) { ref in
                        recentRow(ref)
                    }
                }
                .listStyle(.sidebar)
            }
        }
    }

    private func recentRow(_ ref: ProjectRef) -> some View {
        let available = recents.isAvailable(ref)
        return Button {
            open(ref)
        } label: {
            HStack {
                Image(systemName: {
                    if case .remote = ref { return "network" }
                    return "folder"
                }())
                .foregroundStyle(Color.accentColor)
                VStack(alignment: .leading) {
                    Text(ref.displayName).font(.body.weight(.medium))
                    Text(ref.detail).font(.caption).foregroundStyle(.secondary).lineLimit(1).truncationMode(.middle)
                }
                Spacer()
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!available)
        .opacity(available ? 1 : 0.5)
        .help(ref.detail)
        .contextMenu {
            Button(settings.t("welcome_forget")) { recents.remove(ref) }
        }
    }

    private var remoteSheet: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(settings.t("welcome_connect")).font(.headline)
            Text(settings.t("remote_help"))
                .font(.callout)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Form {
                TextField(settings.t("remote_url"), text: $vm.remoteURL)
                SecureField(settings.t("remote_token"), text: $vm.remoteToken)
            }
            if let error = vm.errorMessage {
                Text(error).font(.caption).foregroundStyle(.red)
            }
            HStack {
                Spacer()
                Button(settings.t("cancel")) {
                    vm.errorMessage = nil
                    showingRemote = false
                }
                .keyboardShortcut(.cancelAction)
                Button(settings.t("connect")) {
                    Task {
                        if let ref = await vm.connect(invalidURL: settings.t("invalid_url")) {
                            showingRemote = false
                            open(ref)
                        }
                    }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(vm.connecting)
            }
        }
        .padding(20)
        .frame(width: 440)
    }

    private func open(_ ref: ProjectRef) {
        recents.add(ref)
        openWindow(value: ref)
        dismissWindow(id: WindowID.welcome)
    }

    private func drop(_ providers: [NSItemProvider]) -> Bool {
        guard let provider = providers.first(where: { $0.canLoadObject(ofClass: URL.self) }) else { return false }
        _ = provider.loadObject(ofClass: URL.self) { url, _ in
            guard let url, let ref = WelcomeViewModel.folder(from: url) else { return }
            Task { @MainActor in open(ref) }
        }
        return true
    }
}
