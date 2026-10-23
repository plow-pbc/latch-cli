import Testing
@testable import PlowLatchUI

/// The Google section onboarding and Connections share: a row per account
/// `setup --status` lists, and the one sign-in it offers.
@MainActor
struct GoogleAccountsTests {
    let three = Fixtures.threeAccounts
    let timeout = LatchError.failed(LatchFailure(code: "sign_in_timeout", message: "No new Google account was observed before the wait ended."))

    private func app(_ client: SignInClient) -> AppModel {
        let app = AppModel(client: client, store: ProgressStore(memory: ProgressStore.Memory(), stateDir: client.stateDir), still: true)
        app.open = { _ in }
        return app
    }

    /// Both places read the same accounts off `setup --status`.
    private func listed(_ google: String) async -> (onboarding: GoogleAccounts, connections: Connections?) {
        let client = SignInClient()
        client.google = google
        let app = app(client)
        let flow = app.stage(.google)
        await flow.checkGoogle()
        await app.refreshConnections()
        return (flow.googleAccounts, app.connections)
    }

    @Test func threeAccountsGiveThreeRowsInBothPlaces() async {
        let (onboarding, connections) = await listed(Fixtures.googleConnected(three))
        #expect(onboarding.accounts == three)
        #expect(connections == .google(onboarding))
        #expect(!three.contains { onboarding.needsSignIn($0) || onboarding.signsInAgain($0) })
        #expect(onboarding.state == "Connected")
    }

    @Test func theButtonConnectsTheFirstAccountThenAddsAnother() {
        #expect(GoogleAccounts(accounts: []).buttonTitle == "Connect Google…")
        #expect(!GoogleAccounts(accounts: []).addsAccount)
        for accounts in [[three[0]], three] {
            #expect(GoogleAccounts(accounts: accounts).buttonTitle == "Add Another Account…")
            #expect(GoogleAccounts(accounts: accounts).addsAccount)
        }
    }

    /// With accounts, onboarding's sign-in is `--add-account`, and a wait
    /// that saw no new one says an account already there isn't added again,
    /// leaving Google on with the accounts it had.
    @Test func addingPassesAddAccountAndATimeoutSaysItIsntAddedAgain() async {
        let client = SignInClient()
        client.google = Fixtures.googleConnected(three)
        let flow = app(client).stage(.google)
        await flow.checkGoogle()
        #expect(flow.google == .connected)
        flow.connectGoogle()
        await waitUntil { client.events != nil }
        #expect(client.signIns == [true])
        client.events?.finish(throwing: timeout)
        await waitUntil { flow.google != .waiting }
        #expect(flow.google == .connected)
        #expect(flow.googleAccounts.accounts == three)
        #expect(flow.googleWaitEnded == "No new Google account showed up in time. An account that's already connected isn't added again.")
    }

    /// The account Google wants signed in again is marked, with no button:
    /// latch doesn't wait for it while others work, so it says to reconnect
    /// it in Plow. The others work, so the section still adds an account.
    @Test func someNeedingSignInAreMarked() async {
        let (onboarding, connections) = await listed(Fixtures.googleConnected(three, needsSignIn: [three[2]]))
        #expect(connections == .google(onboarding))
        #expect(onboarding.needsSignIn == [three[2]])
        #expect(onboarding.needsSignIn(three[2]) && !onboarding.needsSignIn(three[0]))
        #expect(!three.contains { onboarding.signsInAgain($0) })
        #expect(onboarding.reconnectsInPlow(three[2]) && !onboarding.reconnectsInPlow(three[0]))
        #expect(onboarding.state == "Connected")
        #expect(onboarding.buttonTitle == "Add Another Account…" && onboarding.addsAccount)
    }

    /// None works: every row offers Sign In Again…, the section's own button
    /// goes, and signing in is the plain wait (no `--add-account`), in
    /// onboarding and in Connections.
    @Test func allNeedingSignInSignInAgainWithoutAddAccount() async throws {
        let google = Fixtures.googleConnected(three, needsSignIn: three)
        let (onboarding, connections) = await listed(google)
        #expect(connections == .google(onboarding))
        #expect(three.allSatisfy { onboarding.signsInAgain($0) && !onboarding.reconnectsInPlow($0) })
        #expect(onboarding.buttonTitle == nil && !onboarding.addsAccount)
        #expect(onboarding.state == "Needs sign-in")

        let client = SignInClient()
        client.google = google
        client.enableRefusal = LatchFailure(code: "google_needs_sign_in", message: "Google needs the owner to sign in again.")
        let app = app(client)
        let flow = app.stage(.google)
        await flow.checkGoogle()
        #expect(flow.google == .notConnected)
        flow.connectGoogle()
        await waitUntil { client.signIns.count == 1 }
        flow.stopWork()
        await app.refreshConnections()
        let accounts = try #require({ if case let .google(accounts)? = app.connections { accounts } else { nil } }())
        app.connectGoogle(adding: accounts.addsAccount)
        await waitUntil { client.signIns.count == 2 }
        #expect(client.signIns == [false, false])
    }

    /// latch's enable lists the accounts that work and names the rest; the
    /// section lists them all.
    @Test func anEnableNamingSomeToSignInAgainListsThemAll() {
        let accounts = GoogleAccounts(GoogleEnabled(accounts: [three[0]], needsSignIn: [three[1]]))
        #expect(accounts.accounts == [three[0], three[1]])
        #expect(accounts.needsSignIn(three[1]))
    }
}
