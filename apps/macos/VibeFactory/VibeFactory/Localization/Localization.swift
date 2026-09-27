import SwiftUI
import Observation

// Appearance and language follow Templates/AppKitTemplate: a string table
// per language (`Strings.swift`), English as the fallback, French when the
// system prefers it.

// MARK: - Appearance

enum AppearanceMode: String, CaseIterable, Identifiable {
    case system, light, dark
    var id: String { rawValue }
    var colorScheme: ColorScheme? {
        switch self {
        case .system: return nil
        case .light: return .light
        case .dark: return .dark
        }
    }
    var titleKey: String {
        switch self {
        case .system: return "appearance_system"
        case .light: return "appearance_light"
        case .dark: return "appearance_dark"
        }
    }
}

// MARK: - Language

enum AppLanguage: String, CaseIterable, Identifiable {
    case system, fr, en
    var id: String { rawValue }

    /// Name shown in the picker, in the language itself.
    var nativeName: String {
        switch self {
        case .system: return ""
        case .fr: return "Français"
        case .en: return "English"
        }
    }
}

// MARK: - Settings

@Observable
@MainActor
final class AppSettings {
    var appearanceRaw: String {
        didSet { UserDefaults.standard.set(appearanceRaw, forKey: "appearance") }
    }
    var languageRaw: String {
        didSet { UserDefaults.standard.set(languageRaw, forKey: "language") }
    }
    /// Path of the `vibe` executable; empty: `~/.cargo/bin/vibe`, then `PATH`.
    var vibePath: String {
        didSet { UserDefaults.standard.set(vibePath, forKey: "vibePath") }
    }
    /// Evaluation results directory passed as `vibe serve --evals`; empty: none.
    var evalsPath: String {
        didSet { UserDefaults.standard.set(evalsPath, forKey: "evalsPath") }
    }
    /// Names of the variables given to `vibe serve` (provider API keys: what
    /// `api_key_env` names); their values live in the Keychain.
    private(set) var environmentNames: [String] {
        didSet { UserDefaults.standard.set(environmentNames, forKey: "environmentNames") }
    }
    /// Post user notifications for approvals, pauses and finished runs.
    var notificationsEnabled: Bool {
        didSet { UserDefaults.standard.set(notificationsEnabled, forKey: "notifications") }
    }

    init() {
        let defaults = UserDefaults.standard
        appearanceRaw = defaults.string(forKey: "appearance") ?? AppearanceMode.system.rawValue
        languageRaw = defaults.string(forKey: "language") ?? AppLanguage.system.rawValue
        vibePath = defaults.string(forKey: "vibePath") ?? ""
        evalsPath = defaults.string(forKey: "evalsPath") ?? ""
        notificationsEnabled = defaults.object(forKey: "notifications") as? Bool ?? true
        environmentNames = defaults.stringArray(forKey: "environmentNames") ?? []
    }

    // MARK: Child environment

    private static func keychainAccount(_ name: String) -> String { "env:" + name }

    /// Set (or, with an empty value, remove) a variable for `vibe serve`.
    func setEnvironment(_ name: String, value: String) {
        let name = name.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty else { return }
        Keychain.set(value.isEmpty ? nil : value, account: Self.keychainAccount(name))
        if value.isEmpty {
            environmentNames.removeAll { $0 == name }
        } else if !environmentNames.contains(name) {
            environmentNames.append(name)
        }
    }

    /// The variables and their values, read from the Keychain.
    func childEnvironment() -> [String: String] {
        var environment: [String: String] = [:]
        for name in environmentNames {
            if let value = Keychain.get(account: Self.keychainAccount(name)) { environment[name] = value }
        }
        return environment
    }

    var appearance: AppearanceMode { AppearanceMode(rawValue: appearanceRaw) ?? .system }
    var language: AppLanguage { AppLanguage(rawValue: languageRaw) ?? .system }

    /// Effective language code (`.system` resolved from the OS preferences).
    var effectiveLang: String {
        if language == .system {
            let preferred = Locale.preferredLanguages.first ?? "en"
            return preferred.hasPrefix("fr") ? "fr" : "en"
        }
        return language.rawValue
    }

    var locale: Locale {
        Locale(identifier: effectiveLang == "fr" ? "fr_FR" : "en_US")
    }

    // MARK: Translation

    func t(_ key: String) -> String {
        Strings.table[effectiveLang]?[key] ?? Strings.table["en"]?[key] ?? key
    }

    /// `t(key)` with `%@` replaced by the arguments, in order.
    func t(_ key: String, _ arguments: CVarArg...) -> String {
        String(format: t(key), locale: locale, arguments: arguments)
    }
}
