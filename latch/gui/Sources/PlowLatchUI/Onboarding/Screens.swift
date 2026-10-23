import SwiftUI

/// Welcome: the Plow mark's entrance, the tagline and Get started.
struct WelcomeView: View {
    var flow: Onboarding
    /// A fixed moment of the entrance, for snapshots; nil plays it.
    var time: Double?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var start = Date.now
    @State private var settled = false

    var body: some View {
        Group {
            if let time {
                content(EntranceFrame.at(time), breathing: false)
            } else if reduceMotion {
                content(.rest, breathing: false)
            } else if settled {
                content(.rest, breathing: true)
            } else {
                TimelineView(.animation) { context in
                    content(.at(context.date.timeIntervalSince(start)), breathing: false)
                }
                .task {
                    start = .now
                    try? await Task.sleep(for: .seconds(EntranceFrame.settlesAt))
                    settled = true
                }
            }
        }
    }

    private func content(_ frame: EntranceFrame, breathing: Bool) -> some View {
        VStack(spacing: 0) {
            EntranceMark(frame: frame, breathing: breathing)
                .padding(.top, 70)
            Spacer(minLength: 0)
            VStack(spacing: 2) {
                Text("Put your agent to work.")
                Text("Keep control.").foregroundStyle(Brand.volt)
            }
            .font(.system(.largeTitle, weight: .semibold))
            .multilineTextAlignment(.center)
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
            .opacity(frame.tagline)
            .offset(y: frame.taglineOffset)
            Text("Your agent can use this Mac. Nothing runs unless you asked for it.")
                .font(Brand.lead)
                .foregroundStyle(Brand.muted)
                .multilineTextAlignment(.center)
                .padding(.horizontal, 80)
                .padding(.top, 14)
                .opacity(frame.tagline)
            Spacer(minLength: 0)
            HStack {
                Spacer()
                Button {
                    flow.primary()
                } label: {
                    HStack(spacing: 8) { Text(flow.primaryTitle); Image(systemName: "arrow.right").accessibilityHidden(true) }
                }
                .buttonStyle(VoltButtonStyle())
                .keyboardShortcut(.defaultAction)
                .opacity(frame.button)
                .offset(y: frame.buttonOffset)
            }
            .padding(.horizontal, 28)
            .frame(height: OnboardingFooter.height)
        }
    }
}

/// Log in to Plow: the QR code is the hero; once logged in it becomes a check.
struct LoginView: View {
    var flow: Onboarding
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(spacing: 20) {
            Text("Log in to Plow")
                .font(Brand.headline)
                .accessibilityAddTraits(.isHeader)
                .padding(.top, 48)
            Text(verified ? "This Mac is logged in." : "Scan with your iPhone, or text the code below.")
                .font(Brand.lead)
                .foregroundStyle(Brand.muted)
            hero
            caption
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 56)
        .task { if !flow.app.still { flow.startLogin() } }
    }

    private var verified: Bool {
        if case .verified = flow.login { true } else { false }
    }

    private var code: LoginCode? {
        switch flow.login {
        case let .waiting(code), let .expired(code): code
        default: nil
        }
    }

    private var expired: Bool {
        if case .expired = flow.login { true } else { false }
    }

    /// The white QR card, which shrinks and rounds into the check.
    private var hero: some View {
        let side: CGFloat = verified ? 120 : 220
        return ZStack {
            RoundedRectangle(cornerRadius: verified ? side / 2 : 20, style: .continuous)
                .fill(verified ? Brand.voltFill : .white)
            if let code, !verified {
                QRCodeView(payload: code.smsto)
                    .padding(16)
                    .opacity(expired ? 0.2 : 1)
                    .transition(.opacity.combined(with: .scale(scale: 0.7)))
            } else if verified {
                Image(systemName: "checkmark")
                    .font(.system(size: 50, weight: .bold))
                    .foregroundStyle(Brand.onVolt)
                    .transition(.symbolEffect(.drawOn))
            } else if case .failed = flow.login {
                Image(systemName: "exclamationmark")
                    .font(.system(size: 44, weight: .bold))
                    .foregroundStyle(Brand.clay)
            } else {
                ProgressView().controlSize(.large).tint(.black)
            }
        }
        .frame(width: side, height: side)
        .shadow(color: Brand.voltFill.opacity(verified ? 0.35 : 0.1), radius: 24)
        .animation(reduceMotion ? nil : .spring(duration: 0.55, bounce: 0.25), value: verified)
        .accessibilityElement()
        .accessibilityLabel(verified ? "Logged in" : code == nil ? "Getting a code" : "QR code that texts the login code")
    }

    @ViewBuilder private var caption: some View {
        switch flow.login {
        case .starting:
            Text("Getting a code…").foregroundStyle(Brand.muted)
        case let .waiting(code), let .expired(code):
            VStack(spacing: 12) {
                VStack(spacing: 0) {
                    codeRow("To", code.sendToDisplay)
                    Rectangle().fill(Brand.line).frame(height: 1)
                    codeRow("Text", code.code, mono: true)
                }
                .background(Brand.surface, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                .opacity(expired ? 0.4 : 1)
                if expired {
                    HStack(spacing: 10) {
                        Text("This code expired.").foregroundStyle(Brand.muted)
                        Button("Get a new code") { flow.startLogin() }.buttonStyle(.link).foregroundStyle(Brand.volt)
                    }
                } else {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.mini)
                        Text("Waiting for your text").foregroundStyle(Brand.muted)
                        if let expires = code.expiresAt {
                            Text(timerInterval: Date.now...max(expires, .now), countsDown: true)
                                .monospacedDigit()
                                .foregroundStyle(Brand.muted)
                        }
                    }
                    Button("Open in Messages") { flow.openMessages() }.buttonStyle(.link).foregroundStyle(Brand.volt)
                }
                Text("Keep this code private. The phone that texts it gets access to this Mac.")
                    .font(.callout)
                    .foregroundStyle(Brand.muted)
                    .multilineTextAlignment(.center)
            }
            .frame(maxWidth: 360)
        case let .failed(message):
            VStack(spacing: 8) {
                Text(message).foregroundStyle(Brand.clay).multilineTextAlignment(.center)
                Button("Try again") { flow.startLogin() }.buttonStyle(.link).foregroundStyle(Brand.volt)
            }
        case let .verified(name):
            Text("Logged in as \(name).").foregroundStyle(Brand.muted)
        }
    }

    private func codeRow(_ label: String, _ value: String, mono: Bool = false) -> some View {
        HStack {
            Text(label).font(.callout).foregroundStyle(Brand.muted)
            Spacer()
            Text(value)
                .font(mono ? Brand.code : .title3)
                .foregroundStyle(mono ? Brand.volt : Brand.text)
                .textSelection(.enabled)
        }
        .padding(.horizontal, 14)
        .frame(height: 42)
        .accessibilityElement(children: .combine)
    }
}

/// Privacy: what the person controls, and what Plow sees.
struct PrivacyView: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            Label("Logged in to Plow.", systemImage: "checkmark")
                .font(.callout)
                .foregroundStyle(Brand.volt)
            ScreenHeader(title: "You decide what your agents do on this Mac.",
                         lead: "Their requests are checked against what you asked for, and every one is on the record. The connection is encrypted end to end.")
            VStack(alignment: .leading, spacing: 0) {
                statement("checkmark", "Checked before it runs.",
                          "Commands, file changes, Google calls and browser plans are checked against what you asked for. If the check can't run, nothing runs.")
                Rectangle().fill(Brand.line).frame(height: 1).padding(.leading, 44)
                statement("list.bullet", "On the record.",
                          "Every request, allowed or refused, goes in a sealed log on this Mac.")
                Rectangle().fill(Brand.line).frame(height: 1).padding(.leading, 44)
                statement("lock", "Encrypted end to end.",
                          "Plow checks what each request asks for, and never sees your passwords, your secrets or the results.")
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 48)
        .padding(.top, 48)
    }

    private func statement(_ symbol: String, _ title: String, _ detail: String) -> some View {
        HStack(alignment: .top, spacing: 16) {
            Image(systemName: symbol)
                .frame(width: 28, height: 28)
                .background(Brand.surface, in: RoundedRectangle(cornerRadius: 7, style: .continuous))
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.headline)
                Text(detail).foregroundStyle(Brand.muted).fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.vertical, 14)
        .accessibilityElement(children: .combine)
    }
}

/// Connect Google: Gmail and Calendar, through the account connected in Plow.
struct GoogleView: View {
    var flow: Onboarding

    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            PlowMark().fill(Brand.volt, style: FillStyle(eoFill: true)).frame(width: 56, height: 56).accessibilityHidden(true)
            ScreenHeader(title: "Connect Google", lead: "Let your agent work with Gmail and Calendar. Connect Google in Plow, then come back here.")
            Panel {
                PanelRow(symbol: "g.circle", title: "Google account", state: state, stateColor: color)
            }
            if case let .failed(message) = flow.google {
                Text(message).foregroundStyle(Brand.clay).font(.callout)
            }
            if flow.google == .done, !flow.googleAccounts.isEmpty {
                Text(flow.googleAccounts.joined(separator: ", ")).foregroundStyle(Brand.muted)
            }
            Text("The gatekeeper checks each Google request against the task you approved.")
                .foregroundStyle(Brand.muted)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 56)
        .padding(.top, 52)
        // Coming back from Plow: check again, once.
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            if case .needed = flow.google { Task { await flow.checkGoogle() } }
        }
    }

    private var state: String {
        switch flow.google {
        case .unknown: "Not checked"
        case .checking: "Checking…"
        case .done: "Connected"
        case .needed: "Not connected in Plow"
        case .failed: "Couldn't check"
        }
    }

    private var color: Color {
        switch flow.google {
        case .done: Brand.volt
        case .needed, .failed: Brand.amber
        default: Brand.muted
        }
    }
}

/// Contacts & Messages: Full Disk Access for Plow Latch, then both connectors.
struct AccessView: View {
    var flow: Onboarding

    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            ScreenHeader(title: "Contacts & Messages",
                         lead: "Let your agent search your messages and contacts on this Mac. Latch needs Full Disk Access to read them.")
            Panel {
                HStack(spacing: 12) {
                    Image(systemName: "lock.shield").frame(width: 20).foregroundStyle(Brand.muted).accessibilityHidden(true)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Turn on \(Text("Plow Latch").bold()) in Full Disk Access")
                        Text("System Settings ▸ Privacy & Security ▸ Full Disk Access").font(.callout).foregroundStyle(Brand.muted)
                    }
                    Spacer()
                }
                .padding(14)
                .accessibilityElement(children: .combine)
            }
            Panel {
                PanelRow(symbol: "message", title: "Messages", state: state(flow.messages), stateColor: color(flow.messages))
                Rectangle().fill(Brand.line).frame(height: 1)
                PanelRow(symbol: "person.crop.circle", title: "Contacts", state: state(flow.contacts), stateColor: color(flow.contacts))
            }
            ForEach([flow.messages, flow.contacts].compactMap { if case let .failed(m) = $0 { m } else { nil } }, id: \.self) {
                Text($0).foregroundStyle(Brand.clay).font(.callout)
            }
            Text(flow.waitingForAccess
                 ? "Latch checks every few seconds and carries on once access is on."
                 : "Open System Settings, then drag Plow Latch from the Finder window into the list.")
                .foregroundStyle(Brand.muted)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 56)
        .padding(.top, 52)
        .onDisappear { flow.stopWork() }
    }

    private func state(_ check: Onboarding.Check) -> String {
        switch check {
        case .unknown: "Not checked"
        case .checking: "Checking…"
        case .done: "Allowed"
        case .needed: "Waiting for access"
        case .failed: "Couldn't turn on"
        }
    }

    private func color(_ check: Onboarding.Check) -> Color {
        switch check {
        case .done: Brand.volt
        case .needed: Brand.amber
        case .failed: Brand.clay
        default: Brand.muted
        }
    }
}

/// Start Latch: run the host in the background, and at login.
struct StartView: View {
    @Bindable var flow: Onboarding
    @Environment(AppModel.self) private var app

    var body: some View {
        VStack(alignment: .leading, spacing: 22) {
            PlowMark().fill(Brand.volt, style: FillStyle(eoFill: true)).frame(width: 56, height: 56).accessibilityHidden(true)
            ScreenHeader(title: "Keep Latch running",
                         lead: "Keep your Mac ready for your agents. Latch runs in the background when you close this window.")
            Panel {
                toggle("Run in the background", "arrow.clockwise", $flow.runInBackground)
                Rectangle().fill(Brand.line).frame(height: 1)
                toggle("Start when you log in", "clock", $flow.startAtLogin)
            }
            if let blocked = app.status?.serve.blocked {
                Text(blocked.message).foregroundStyle(Brand.clay).font(.callout)
            } else if app.status?.serve.state == "serving" {
                Text("Latch is already running on this Mac.").foregroundStyle(Brand.muted)
            }
            if let error = flow.startError {
                Text(error).foregroundStyle(Brand.clay).font(.callout)
            }
            Text("Your Mac stays reachable while it's awake and Latch is running.").foregroundStyle(Brand.muted)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 56)
        .padding(.top, 52)
        .task { await app.refreshStatus() }
    }

    private func toggle(_ title: String, _ symbol: String, _ on: Binding<Bool>) -> some View {
        HStack(spacing: 12) {
            Image(systemName: symbol).frame(width: 20).foregroundStyle(Brand.muted).accessibilityHidden(true)
            Text(title)
            Spacer()
            Toggle(title, isOn: on).toggleStyle(.switch).labelsHidden()
        }
        .padding(.horizontal, 14)
        .frame(minHeight: 44)
    }
}

/// Done: what's set up, and what was skipped.
struct DoneView: View {
    var flow: Onboarding
    @Environment(AppModel.self) private var app

    var body: some View {
        VStack(spacing: 20) {
            Image(systemName: "checkmark")
                .font(.system(size: 34, weight: .semibold))
                .foregroundStyle(Brand.volt)
                .frame(width: 76, height: 76)
                .background(Brand.surface, in: Circle())
                .accessibilityHidden(true)
                .padding(.top, 56)
            Text("You're all set").font(Brand.headline).accessibilityAddTraits(.isHeader)
            Panel {
                ForEach(Array(Summary.rows(flow: flow, status: app.status).enumerated()), id: \.offset) { index, row in
                    if index > 0 { Rectangle().fill(Brand.line).frame(height: 1) }
                    PanelRow(symbol: row.symbol, title: row.title, state: row.state, stateColor: row.set ? Brand.volt : Brand.muted)
                }
            }
            Text("Your agents can now use this Mac. The gatekeeper reviews their requests.")
                .foregroundStyle(Brand.muted)
                .multilineTextAlignment(.center)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 56)
        .task { await app.refreshStatus() }
    }
}

/// Done's and the main window's list of what onboarding set up.
enum Summary {
    struct Row {
        var symbol: String
        var title: String
        var state: String
        var set: Bool
    }

    static func rows(flow: Onboarding?, status: LocalStatus?, progress: Progress? = nil) -> [Row] {
        let passed = progress?.passed ?? flow?.progress.passed ?? [:]
        func step(_ step: Step, done: Bool, _ title: String, _ symbol: String, on: String) -> Row {
            if done { return Row(symbol: symbol, title: title, state: on, set: true) }
            return Row(symbol: symbol, title: title, state: "Not set up", set: false)
        }
        let loggedIn = status?.peer != nil
        let serving = status?.serve.state == "serving"
        return [
            Row(symbol: "checkmark", title: "Logged in to Plow", state: loggedIn ? "Done" : "Not logged in", set: loggedIn),
            step(.rules, done: passed[.rules] == .continued && flow?.savedRules?.isEmpty != true, "Your rules", "text.alignleft", on: "Set"),
            step(.google, done: passed[.google] == .continued, "Google", "g.circle", on: "Connected"),
            step(.access, done: passed[.access] == .continued, "Contacts & Messages", "message", on: "Allowed"),
            Row(symbol: "arrow.clockwise", title: "Latch", state: serving ? "Running" : "Not running", set: serving),
        ]
    }
}
