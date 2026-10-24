import Foundation

/// The onboarding screens, in order.
enum Step: String, CaseIterable, Codable {
    /// `access` is Grant access: Contacts & Messages and Google.
    case welcome, login, privacy, rules, access, start, done

    /// The screens a person may skip.
    var skippable: Bool { [.rules, .access].contains(self) }
}

/// How a person left a screen.
enum Passed: String, Codable {
    case continued, skipped
}

/// Where the app opens.
enum Destination: Equatable {
    case onboarding(Step)
    case main
}

/// What the app remembers about onboarding, in its own preferences (never
/// latch's state): which screens the person passed, and how, and whether
/// they finished. Real setup state always comes from latch.
struct Progress: Equatable {
    var passed: [Step: Passed] = [:]
    var finished = false
    /// "Set up again…": screens show what's set up now rather than first-run
    /// defaults.
    var rerun = false

    /// Where a launch lands: not logged in → Welcome until it was passed,
    /// then Log in, finished or not (a logout means setting up again);
    /// logged in and finished → the main window; logged in → the first
    /// screen not passed yet (Done once all are).
    func destination(loggedIn: Bool) -> Destination {
        if !loggedIn { return .onboarding(passed[.welcome] == nil ? .welcome : .login) }
        if finished { return .main }
        // Set up again walks Welcome and Log in too; a first run that is
        // logged in already is past them.
        let steps: [Step] = rerun ? [.welcome, .login, .privacy, .rules, .access, .start] : [.privacy, .rules, .access, .start]
        return .onboarding(steps.first { passed[$0] == nil } ?? .done)
    }

    /// "Set up again…": every screen is walked again, from Welcome, each
    /// showing what's set up now; a relaunch midway resumes there.
    func settingUpAgain() -> Progress {
        Progress(passed: [:], finished: false, rerun: true)
    }
}

/// Where the app keeps onboarding progress: its own preferences, one entry
/// per state folder, so each latch it runs for has its own.
struct ProgressStore {
    let stateDir: String
    private let read: () -> [String: [String: Any]]
    private let write: ([String: [String: Any]]) -> Void
    private static let key = "Onboarding"

    init(defaults: UserDefaults, stateDir: String) {
        self.stateDir = stateDir
        read = { defaults.dictionary(forKey: Self.key) as? [String: [String: Any]] ?? [:] }
        write = { defaults.set($0, forKey: Self.key) }
    }

    /// Kept in memory only, for tests: nothing reaches the preferences.
    final class Memory {
        var all: [String: [String: Any]] = [:]
    }

    init(memory: Memory, stateDir: String) {
        self.stateDir = stateDir
        read = { memory.all }
        write = { memory.all = $0 }
    }

    func load() -> Progress {
        let saved = read()[stateDir] ?? [:]
        var passed: [Step: Passed] = [:]
        for (step, how) in saved["passed"] as? [String: String] ?? [:] {
            if let step = Step(rawValue: step), let how = Passed(rawValue: how) { passed[step] = how }
        }
        return Progress(passed: passed, finished: saved["finished"] as? Bool ?? false, rerun: saved["rerun"] as? Bool ?? false)
    }

    func save(_ progress: Progress) {
        var all = read()
        all[stateDir] = [
            "passed": Dictionary(uniqueKeysWithValues: progress.passed.map { ($0.key.rawValue, $0.value.rawValue) }),
            "finished": progress.finished,
            "rerun": progress.rerun,
        ]
        write(all)
    }
}
