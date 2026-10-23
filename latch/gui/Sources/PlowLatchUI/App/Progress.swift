import Foundation

/// The onboarding screens, in order.
enum Step: String, CaseIterable, Codable {
    case welcome, login, privacy, rules, google, access, start, done

    /// The screens a person may skip.
    var skippable: Bool { [.rules, .google, .access].contains(self) }
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
    /// screen after Log in not passed yet (Done once all are).
    func destination(loggedIn: Bool) -> Destination {
        if !loggedIn { return .onboarding(passed[.welcome] == nil ? .welcome : .login) }
        if finished { return .main }
        let next = [Step.privacy, .rules, .google, .access, .start].first { passed[$0] == nil }
        return .onboarding(next ?? .done)
    }

    /// "Set up again…": every screen after Log in is walked again, each
    /// showing what's set up now; a relaunch midway resumes there.
    func settingUpAgain() -> Progress {
        Progress(passed: passed.filter { $0.key == .welcome || $0.key == .login }, finished: false, rerun: true)
    }
}

extension Progress {
    private static let passedKey = "OnboardingPassed"
    private static let finishedKey = "OnboardingFinished"
    private static let rerunKey = "OnboardingRerun"

    static func load(_ defaults: UserDefaults = .standard) -> Progress {
        let raw = defaults.dictionary(forKey: passedKey) as? [String: String] ?? [:]
        var passed: [Step: Passed] = [:]
        for (step, how) in raw {
            if let step = Step(rawValue: step), let how = Passed(rawValue: how) { passed[step] = how }
        }
        return Progress(passed: passed, finished: defaults.bool(forKey: finishedKey), rerun: defaults.bool(forKey: rerunKey))
    }

    func save(_ defaults: UserDefaults = .standard) {
        defaults.set(Dictionary(uniqueKeysWithValues: passed.map { ($0.key.rawValue, $0.value.rawValue) }), forKey: Self.passedKey)
        defaults.set(finished, forKey: Self.finishedKey)
        defaults.set(rerun, forKey: Self.rerunKey)
    }
}
