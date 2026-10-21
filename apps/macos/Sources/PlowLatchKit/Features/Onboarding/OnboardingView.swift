import AppKit
import SwiftUI

/// The onboarding window: one fixed dark shell, a screen per step, and a
/// footer with Back, progress dots and the volt primary button.
public struct OnboardingView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.theme) private var theme
    @State private var onboarding: Onboarding?
    /// For snapshots: show this step and sign-in state instead of running the flow.
    private let preset: Onboarding?
    private let entranceTime: Double?

    public init() {
        preset = nil
        entranceTime = nil
    }

    /// A still of the flow, for snapshots and previews.
    public init(still onboarding: Onboarding, entranceTime: Double? = nil) {
        preset = onboarding
        self.entranceTime = entranceTime
    }

    public var body: some View {
        let brand = theme.brand
        ZStack {
            background
            if let flow = preset ?? onboarding {
                VStack(spacing: 0) {
                    ZStack {
                        screen(flow)
                            .id(flow.step == .linked ? Onboarding.Step.signIn : flow.step)
                            .transition(.opacity.combined(with: .offset(y: 6)))
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .animation(brand.stepChange, value: flow.step)
                    if flow.step != .welcome {
                        OnboardingFooter(onboarding: flow)
                    }
                }
            }
        }
        .frame(width: brand.windowSize.width, height: brand.windowSize.height)
        .foregroundStyle(brand.text)
        .environment(\.colorScheme, .dark)
        .task {
            guard preset == nil, onboarding == nil else { return }
            let flow = Onboarding(app: app) { NSWorkspace.shared.open($0) }
            flow.resumeIfLoggedIn()
            onboarding = flow
        }
        .onChange(of: app.status) { onboarding?.resumeIfLoggedIn() }
        .onDisappear { onboarding?.cancel() }
    }

    @ViewBuilder private func screen(_ flow: Onboarding) -> some View {
        switch flow.step {
        case .welcome: WelcomeView(onboarding: flow, time: entranceTime)
        case .signIn, .linked: SignInView(onboarding: flow)
        case .keepRunning: KeepRunningView(onboarding: flow)
        case .done: DoneView(onboarding: flow)
        }
    }

    /// The warm near-black window with two faint corner tints.
    private var background: some View {
        let brand = theme.brand
        return brand.content
            .overlay(alignment: .topTrailing) {
                Ellipse().fill(brand.sage.opacity(0.10)).frame(width: 620, height: 240).blur(radius: 70).offset(x: 220, y: -120)
            }
            .overlay(alignment: .bottomLeading) {
                Ellipse().fill(brand.iris.opacity(0.07)).frame(width: 500, height: 240).blur(radius: 70).offset(x: -200, y: 140)
            }
            .clipped()
            .ignoresSafeArea()
    }
}

/// Welcome: the Plow mark's entrance, the tagline and Get started.
struct WelcomeView: View {
    var onboarding: Onboarding
    /// A fixed moment of the entrance, for snapshots; nil plays it.
    var time: Double?
    @Environment(\.theme) private var theme
    @ReducedMotion private var reduceMotion
    @State private var start = Date.now
    @State private var settled = false

    var body: some View {
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

    private func content(_ frame: EntranceFrame, breathing: Bool) -> some View {
        let brand = theme.brand
        return VStack(spacing: 0) {
            EntranceMark(frame: frame, breathing: breathing)
                .padding(.top, 64)
            Spacer(minLength: 0)
            VStack(spacing: 2) {
                Text("Keep your passwords.")
                Text("Lose the busywork.").foregroundStyle(brand.volt)
            }
            .font(brand.tagline)
            .multilineTextAlignment(.center)
            .opacity(frame.tagline)
            .offset(y: frame.taglineOffset)
            Spacer(minLength: 0)
            HStack {
                Spacer()
                Button("Get started") { onboarding.next() }
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

/// Sign in and Linked: the QR code is the hero; on success it morphs into a check.
struct SignInView: View {
    var onboarding: Onboarding
    @Environment(\.theme) private var theme
    @ReducedMotion private var reduceMotion

    var body: some View {
        let brand = theme.brand
        VStack(spacing: 22) {
            Text(title).font(brand.headline)
                .padding(.top, 56)
            hero
            caption
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 56)
        .onChange(of: verified) { _, verified in
            guard verified else { return }
            Task {
                try? await Task.sleep(for: reduceMotion ? .zero : .milliseconds(900))
                withAnimation(brand.stepChange) { onboarding.showLinked() }
            }
        }
    }

    private var title: String {
        verified ? "This Mac is linked." : "Sign in with your phone"
    }

    private var verified: Bool {
        if case .verified = onboarding.signIn { true } else { false }
    }

    private var code: LoginCode? {
        switch onboarding.signIn {
        case let .waiting(code), let .expired(code): code
        default: nil
        }
    }

    /// The QR card, which shrinks and rounds into the check when linked.
    private var hero: some View {
        let brand = theme.brand
        let side: CGFloat = verified ? 120 : 248
        return ZStack {
            RoundedRectangle(cornerRadius: verified ? side / 2 : 22, style: .continuous)
                .fill(verified ? brand.volt : .white)
            if let code, !verified {
                QRCodeView(payload: code.smsto)
                    .padding(18)
                    .opacity(isExpired ? 0.25 : 1)
                    .transition(.opacity.combined(with: .scale(scale: 0.7)))
            } else if verified {
                Image(systemName: "checkmark")
                    .font(.system(size: 52, weight: .bold))
                    .foregroundStyle(brand.content)
                    .transition(.symbolEffect(.drawOn))
            } else if case .failed = onboarding.signIn {
                Image(systemName: "exclamationmark")
                    .font(.system(size: 44, weight: .bold))
                    .foregroundStyle(brand.clay)
            } else {
                Spinner(color: brand.content, size: 22)
            }
        }
        .frame(width: side, height: side)
        .shadow(color: brand.volt.opacity(verified ? 0.35 : 0.12), radius: 24)
        .animation(reduceMotion ? nil : .spring(duration: 0.55, bounce: 0.25), value: verified)
        .contentShape(Rectangle())
        .onTapGesture { onboarding.openMessages() }
        .onHover { inside in
            if inside, code != nil { NSCursor.pointingHand.push() } else { NSCursor.pop() }
        }
        .help(code == nil ? "" : "Open in Messages")
        .accessibilityElement()
        .accessibilityLabel(verified ? "Linked" : "QR code to text the sign-in code")
        .accessibilityAddTraits(code == nil ? [] : .isButton)
        .accessibilityAction { onboarding.openMessages() }
    }

    private var isExpired: Bool {
        if case .expired = onboarding.signIn { true } else { false }
    }

    @ViewBuilder private var caption: some View {
        let brand = theme.brand
        switch onboarding.signIn {
        case .starting:
            Text("Getting a code…").foregroundStyle(brand.muted)
        case let .waiting(code):
            VStack(spacing: 14) {
                textLine(code)
                HStack(spacing: 8) {
                    Spinner(color: brand.amber)
                    Text("Waiting for your text").foregroundStyle(brand.muted)
                }
            }
        case let .expired(code):
            VStack(spacing: 14) {
                textLine(code).opacity(0.4)
                HStack(spacing: 10) {
                    Text("This code expired.").foregroundStyle(brand.muted)
                    Button("Get a new code") { onboarding.startLogin() }.buttonStyle(.plain).foregroundStyle(brand.volt)
                }
            }
        case let .failed(message):
            HStack(spacing: 10) {
                Text(message).foregroundStyle(brand.clay)
                Button("Try again") { onboarding.startLogin() }.buttonStyle(.plain).foregroundStyle(brand.volt)
            }
            .multilineTextAlignment(.center)
        case let .verified(peer):
            Text("Signed in as \(peer.name).").foregroundStyle(brand.muted)
        }
    }

    /// "Text ABCDE to +1 555-555-1234", the code in volt mono.
    private func textLine(_ code: LoginCode) -> some View {
        let brand = theme.brand
        return Text("Text \(Text(code.code).font(brand.code).foregroundStyle(brand.volt)) to \(code.sendToDisplay)")
            .font(brand.lead)
            .textSelection(.enabled)
    }
}

/// Keep Latch running: pick folders, then start sharing or not yet.
struct KeepRunningView: View {
    @Bindable var onboarding: Onboarding
    @Environment(\.theme) private var theme
    @State private var choosing = false

    var body: some View {
        let brand = theme.brand
        VStack(alignment: .leading, spacing: 18) {
            Text("Keep Latch running").font(brand.headline)
            Text("Choose the folders your devices and agents may use. Sharing runs in the background, even with this window closed.")
                .font(brand.lead)
                .foregroundStyle(brand.muted)
                .fixedSize(horizontal: false, vertical: true)
            VStack(spacing: 0) {
                ForEach(onboarding.folders, id: \.self) { folder in
                    HStack {
                        Image(systemName: "folder.fill").foregroundStyle(brand.volt)
                        Text(folder.path).font(brand.code.weight(.regular)).lineLimit(1).truncationMode(.middle)
                        Spacer()
                        Button { onboarding.removeFolder(folder) } label: { Image(systemName: "minus.circle") }
                            .buttonStyle(.plain)
                            .foregroundStyle(brand.muted)
                            .help("Remove")
                    }
                    .padding(.horizontal, 14)
                    .frame(height: 40)
                    Divider().opacity(0.4)
                }
                Button { choosing = true } label: {
                    Label(onboarding.folders.isEmpty ? "Choose a folder…" : "Add another folder…", systemImage: "plus")
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .buttonStyle(.plain)
                .foregroundStyle(brand.volt)
                .padding(.horizontal, 14)
                .frame(height: 40)
            }
            .background(brand.text.opacity(0.045), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            if let error = onboarding.keepRunningError {
                Text(error).foregroundStyle(brand.clay)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 56)
        .padding(.top, 56)
        .fileImporter(isPresented: $choosing, allowedContentTypes: [.folder], allowsMultipleSelection: true) { result in
            if case let .success(urls) = result { onboarding.addFolders(urls) }
        }
    }
}

/// Done: one button into the main window.
struct DoneView: View {
    var onboarding: Onboarding
    @Environment(\.theme) private var theme
    @State private var shown = false

    var body: some View {
        let brand = theme.brand
        VStack(spacing: 18) {
            Spacer()
            Image(systemName: "checkmark.circle.fill")
                .font(.system(size: 64))
                .foregroundStyle(brand.content, brand.volt)
                .scaleEffect(shown ? 1 : 0.72)
                .opacity(shown ? 1 : 0)
            Text("You're all set").font(brand.headline)
            Text(onboarding.sharing
                 ? "This Mac is sharing. Plow Latch keeps it running in the background."
                 : "Your folders are saved. Start sharing from Plow Latch whenever you're ready.")
                .font(brand.lead)
                .foregroundStyle(brand.muted)
                .multilineTextAlignment(.center)
            Spacer()
        }
        .padding(.horizontal, 64)
        .latchAnimation(.timingCurve(0.2, 0.8, 0.2, 1.1, duration: 0.45).delay(0.15), value: shown)
        .onAppear { shown = true }
    }
}

/// Back, the progress dots, and the step's actions.
struct OnboardingFooter: View {
    static let height: CGFloat = 72
    var onboarding: Onboarding
    @Environment(\.theme) private var theme

    var body: some View {
        let brand = theme.brand
        HStack(spacing: 12) {
            if canGoBack {
                Button("Back") { onboarding.back() }
                    .buttonStyle(.plain)
                    .foregroundStyle(brand.muted)
            }
            Spacer()
            dots
            Spacer()
            actions
        }
        .padding(.horizontal, 28)
        .frame(height: Self.height)
        .overlay(alignment: .top) { Divider().opacity(0.5) }
    }

    /// Not once the login has gone through: it can't be undone from here.
    private var canGoBack: Bool {
        switch onboarding.step {
        case .signIn: if case .verified = onboarding.signIn { false } else { true }
        case .keepRunning: true
        default: false
        }
    }

    /// One dot each for Sign in, Linked and Keep Latch running.
    private var dots: some View {
        let brand = theme.brand
        let steps: [Onboarding.Step] = [.signIn, .linked, .keepRunning]
        return HStack(spacing: 8) {
            ForEach(steps, id: \.self) { step in
                let current = step == onboarding.step
                let past = step.rawValue < onboarding.step.rawValue
                Circle()
                    .fill(current ? brand.volt : past ? brand.sage : brand.text.opacity(0.18))
                    .frame(width: 7, height: 7)
                    .scaleEffect(current ? 1.15 : 1)
                    .shadow(color: current ? brand.volt.opacity(0.6) : .clear, radius: 4)
            }
        }
        .latchAnimation(.easeOut(duration: 0.2), value: onboarding.step)
        .opacity(onboarding.step == .done ? 0 : 1)
        .accessibilityHidden(true)
    }

    @ViewBuilder private var actions: some View {
        switch onboarding.step {
        case .welcome, .signIn:
            EmptyView()
        case .linked:
            Button("Continue") { onboarding.next() }
                .buttonStyle(VoltButtonStyle())
                .keyboardShortcut(.defaultAction)
        case .keepRunning:
            Button("Not now") { Task { await onboarding.keepRunning(startSharing: false) } }
                .buttonStyle(.plain)
                .foregroundStyle(theme.brand.muted)
                .disabled(onboarding.folders.isEmpty || onboarding.saving)
            Button("Start Sharing") { Task { await onboarding.keepRunning(startSharing: true) } }
                .buttonStyle(VoltButtonStyle())
                .keyboardShortcut(.defaultAction)
                .disabled(onboarding.folders.isEmpty || onboarding.saving)
        case .done:
            Button("Open Plow Latch") { WindowRouter.shared.finishOnboarding() }
                .buttonStyle(VoltButtonStyle())
                .keyboardShortcut(.defaultAction)
        }
    }
}
