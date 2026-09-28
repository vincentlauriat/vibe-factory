import AppKit
import SwiftUI
import VibeAPI

/// Root of a project window: the session's connection state, then the project.
struct ProjectWindow: View {
    let ref: ProjectRef?
    @Environment(AppSettings.self) private var settings
    @Environment(SessionRegistry.self) private var registry
    @Environment(\.openWindow) private var openWindow
    @State private var model: ProjectViewModel?

    var body: some View {
        Group {
            if ref != nil {
                if let model {
                    ConnectionGate(model: model)
                } else {
                    ProgressView()
                }
            } else {
                // A restored window without its project.
                ContentUnavailableView {
                    Label(settings.t("no_project"), systemImage: "folder.badge.questionmark")
                } actions: {
                    Button(settings.t("menu_welcome")) { openWindow(id: WindowID.welcome) }
                }
            }
        }
        .navigationTitle(ref?.displayName ?? "Vibe Factory")
        .navigationSubtitle(ref?.detail ?? "")
        .background(WindowCloseObserver {
            // Only a real close stops the server: `onDisappear` also fires
            // when the window is minimised, hidden or its tab switched.
            model?.detail?.stop()
            if let ref { registry.close(ref) }
        })
        .onAppear {
            guard let ref else { return }
            let session = registry.open(ref, settings: settings)
            if model?.session !== session {
                model = ProjectViewModel(session: session, translate: { [settings] in settings.t($0) })
            }
            model?.showRequestedTask()
        }
    }
}

/// Calls `onClose` when the hosting `NSWindow` posts `willCloseNotification`.
private struct WindowCloseObserver: NSViewRepresentable {
    let onClose: () -> Void

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async { context.coordinator.observe(view.window) }
        return view
    }

    func updateNSView(_ view: NSView, context: Context) {
        context.coordinator.onClose = onClose
        if context.coordinator.window == nil {
            DispatchQueue.main.async { context.coordinator.observe(view.window) }
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator(onClose: onClose) }

    final class Coordinator {
        var onClose: () -> Void
        private(set) weak var window: NSWindow?
        private var token: NSObjectProtocol?

        init(onClose: @escaping () -> Void) { self.onClose = onClose }

        func observe(_ window: NSWindow?) {
            guard let window, window !== self.window else { return }
            if let token { NotificationCenter.default.removeObserver(token) }
            self.window = window
            token = NotificationCenter.default.addObserver(
                forName: NSWindow.willCloseNotification, object: window, queue: .main
            ) { [weak self] _ in self?.onClose() }
        }

        deinit {
            if let token { NotificationCenter.default.removeObserver(token) }
        }
    }
}

/// Shows the project once connected, a spinner while `vibe serve` starts,
/// and the error with Retry when it could not.
private struct ConnectionGate: View {
    let model: ProjectViewModel
    @Environment(AppSettings.self) private var settings

    var body: some View {
        switch model.session.state {
        case .connected:
            ProjectView(model: model)
        case .idle, .starting:
            VStack(spacing: 12) {
                ProgressView()
                Text(settings.t("starting_server")).foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .failed(let message):
            ContentUnavailableView {
                Label(settings.t("server_failed"), systemImage: "exclamationmark.triangle")
            } description: {
                ScrollView {
                    Text(message)
                        .font(.system(.caption, design: .monospaced))
                        .textSelection(.enabled)
                        .frame(maxWidth: 520, alignment: .leading)
                }
                .frame(maxHeight: 220)
            } actions: {
                Button(settings.t("retry")) { Task { await model.session.retry() } }
                SettingsLink { Text(settings.t("open_settings")) }
            }
        }
    }
}
