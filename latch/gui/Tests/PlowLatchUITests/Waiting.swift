import Synchronization
import Testing

/// Waits for `done`, looking again after a brief yield, and records an
/// issue if it still doesn't hold by `deadline`: a stuck operation fails,
/// and a quick one costs only its own time.
@discardableResult
func waitUntil(within deadline: Duration = .seconds(10), sourceLocation: SourceLocation = #_sourceLocation,
               _ done: () -> Bool) async -> Bool {
    let end = ContinuousClock.now + deadline
    while !done() {
        guard ContinuousClock.now < end else {
            Issue.record("still waiting after \(deadline)", sourceLocation: sourceLocation)
            return false
        }
        try? await Task.sleep(for: .milliseconds(2))
    }
    return true
}

/// A fake's reply held until the test lets it go, so the test, not a
/// clock, decides what answers when.
final class Gate: Sendable {
    private struct State {
        var open = false
        var waiting: [CheckedContinuation<Void, Never>] = []
    }
    private let state = Mutex(State())

    func wait() async {
        await withCheckedContinuation { continuation in
            let go = state.withLock { state in
                if !state.open { state.waiting.append(continuation) }
                return state.open
            }
            if go { continuation.resume() }
        }
    }

    func open() {
        let waiting = state.withLock { state in
            state.open = true
            defer { state.waiting = [] }
            return state.waiting
        }
        for waiter in waiting { waiter.resume() }
    }
}
