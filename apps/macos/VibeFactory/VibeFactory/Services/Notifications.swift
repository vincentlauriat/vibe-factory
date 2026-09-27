import Foundation
import UserNotifications

/// User notifications for the moments a human is needed or a run ends.
/// Each carries its project and task: a click opens the project window on
/// that task.
@MainActor
enum Notifications {
    private static var authorized: Bool?
    private static let delegate = Delegate()

    /// Install the delegate (banners while the app is active, clicks); call at launch.
    static func install() {
        UNUserNotificationCenter.current().delegate = delegate
    }

    /// Ask once; later calls reuse the answer.
    static func requestAuthorization() {
        guard authorized == nil else { return }
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { granted, _ in
            Task { @MainActor in authorized = granted }
        }
    }

    static func post(title: String, body: String, identifier: String, project: ProjectRef, task: String) {
        guard authorized != false else { return }
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        if let data = try? JSONEncoder().encode(project) {
            content.userInfo = ["project": data, "task": task]
        }
        let request = UNNotificationRequest(identifier: identifier, content: content, trigger: nil)
        UNUserNotificationCenter.current().add(request)
    }

    private final class Delegate: NSObject, UNUserNotificationCenterDelegate {
        /// Show banners even when Vibe Factory is the active app.
        func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification,
                                    withCompletionHandler completionHandler:
                                    @escaping (UNNotificationPresentationOptions) -> Void) {
            completionHandler([.banner, .sound])
        }

        func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                                    withCompletionHandler completionHandler: @escaping () -> Void) {
            let info = response.notification.request.content.userInfo
            if let data = info["project"] as? Data, let task = info["task"] as? String,
               let project = try? JSONDecoder().decode(ProjectRef.self, from: data) {
                Task { @MainActor in SessionRegistry.shared.show(task: task, in: project) }
            }
            completionHandler()
        }
    }
}
