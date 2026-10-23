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

    /// Where a launch lands: finished → the main window; not logged in →
    /// Welcome until it was passed, then Log in; logged in → the first
    /// screen after Log in not passed yet (Done once all are).
    func destination(loggedIn: Bool) -> Destination {
        if finished { return .main }
        if !loggedIn { return .onboarding(passed[.welcome] == nil ? .welcome : .login) }
        let next = [Step.privacy, .rules, .google, .access, .start].first { passed[$0] == nil }
        return .onboarding(next ?? .done)
    }

    /// "Set up again…": the first screen skipped, or else the first after
    /// Log in; every screen after it shows what's set up now.
    func setUpAgain() -> Step {
        Step.allCases.first { passed[$0] == .skipped } ?? .privacy
    }
}

extension Progress {
    private static let passedKey = "OnboardingPassed"
    private static let finishedKey = "OnboardingFinished"

    static func load(_ defaults: UserDefaults = .standard) -> Progress {
        let raw = defaults.dictionary(forKey: passedKey) as? [String: String] ?? [:]
        var passed: [Step: Passed] = [:]
        for (step, how) in raw {
            if let step = Step(rawValue: step), let how = Passed(rawValue: how) { passed[step] = how }
        }
        return Progress(passed: passed, finished: defaults.bool(forKey: finishedKey))
    }

    func save(_ defaults: UserDefaults = .standard) {
        defaults.set(Dictionary(uniqueKeysWithValues: passed.map { ($0.key.rawValue, $0.value.rawValue) }), forKey: Self.passedKey)
        defaults.set(finished, forKey: Self.finishedKey)
    }
}
