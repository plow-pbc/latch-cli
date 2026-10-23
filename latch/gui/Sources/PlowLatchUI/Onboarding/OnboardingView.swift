import SwiftUI

/// The onboarding window: one fixed shell, a screen per step, and a footer
/// with Back or Skip, progress dots and the step's primary action.
struct OnboardingView: View {
    @Environment(AppModel.self) private var app
    /// A still of the flow, for snapshots: its Welcome at a fixed moment.
    var entranceTime: Double?

    var body: some View {
        ZStack {
            Brand.content.ignoresSafeArea()
            if let flow = app.onboarding {
                VStack(spacing: 0) {
                    screen(flow)
                        .id(flow.step)
                        .transition(.opacity.combined(with: .offset(y: 6)))
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .latchAnimation(Brand.stepChange, value: flow.step)
                    if flow.step != .welcome {
                        OnboardingFooter(flow: flow)
                    }
                }
            }
        }
        .overlay { Grain() }
        .frame(width: Brand.windowSize.width, height: Brand.windowSize.height)
        .foregroundStyle(Brand.text)
        .tint(Brand.volt)
        .task { await app.resumeOnboarding() }
        .onDisappear { app.onboarding?.stopWork() }
    }

    @ViewBuilder private func screen(_ flow: Onboarding) -> some View {
        switch flow.step {
        case .welcome: WelcomeView(flow: flow, time: entranceTime)
        case .login: LoginView(flow: flow)
        case .privacy: PrivacyView()
        case .rules: RulesView(flow: flow)
        case .google: GoogleView(flow: flow)
        case .access: AccessView(flow: flow)
        case .start: StartView(flow: flow)
        case .done: DoneView(flow: flow)
        }
    }
}

/// A screen's title and lead, left-aligned as the old app's.
struct ScreenHeader: View {
    var title: String
    var lead: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(title)
                .font(Brand.headline)
                .accessibilityAddTraits(.isHeader)
                .fixedSize(horizontal: false, vertical: true)
            if let lead {
                Text(lead)
                    .font(Brand.lead)
                    .foregroundStyle(Brand.muted)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// A grouped block of rows on a screen.
struct Panel<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        VStack(spacing: 0) { content }
            .background(Brand.surface, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(Brand.line))
    }
}

/// A row in a panel: an icon, a title, and a state on the right.
struct PanelRow: View {
    var symbol: String
    var title: String
    var state: String
    var stateColor: Color = Brand.muted

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: symbol).frame(width: 20).foregroundStyle(Brand.muted).accessibilityHidden(true)
            Text(title)
            Spacer()
            Text(state).font(.callout).foregroundStyle(stateColor)
        }
        .padding(.horizontal, 14)
        .frame(minHeight: 44)
        .accessibilityElement(children: .combine)
    }
}

/// Back or Skip for now, the progress dots, and the step's primary action.
struct OnboardingFooter: View {
    static let height: CGFloat = 72
    var flow: Onboarding

    var body: some View {
        HStack(spacing: 12) {
            leading.frame(minWidth: 110, alignment: .leading)
            Spacer()
            dots
            Spacer()
            primary.frame(minWidth: 110, alignment: .trailing)
        }
        .padding(.horizontal, 28)
        .frame(height: Self.height)
        .background(Brand.bar)
        .overlay(alignment: .top) { Rectangle().fill(Brand.line).frame(height: 1) }
    }

    @ViewBuilder private var leading: some View {
        HStack(spacing: 18) {
            // Texting an agent is Done's main button; opening Latch goes here.
            if flow.step == .done, flow.textAgent != nil {
                Button("Open Latch") { flow.finish() }
            }
            if [.login, .privacy, .rules, .google, .access, .start].contains(flow.step) {
                Button("Back") { flow.back() }
                    .buttonStyle(.plain)
                    .foregroundStyle(Brand.muted)
            }
            // Once the step is set up, Continue is the way on.
            if flow.step.skippable, !flow.stepDone, !flow.primarySkips {
                Button("Skip for now") { flow.skip() }
                    .buttonStyle(.plain)
                    .foregroundStyle(Brand.muted)
            }
        }
        .disabled(flow.busy)
    }

    /// One dot for each screen from Log in to Start Latch.
    private var dots: some View {
        let steps: [Step] = [.login, .privacy, .rules, .google, .access, .start]
        let current = steps.firstIndex(of: flow.step) ?? steps.count
        return HStack(spacing: 8) {
            ForEach(Array(steps.enumerated()), id: \.offset) { index, _ in
                Circle()
                    .fill(index == current ? Brand.volt : index < current ? Brand.sage : Brand.line)
                    .frame(width: 6, height: 6)
            }
        }
        .opacity(flow.step == .done ? 0 : 1)
        .accessibilityElement()
        .accessibilityLabel("Step \(min(current + 1, steps.count)) of \(steps.count)")
    }

    @ViewBuilder private var primary: some View {
        if flow.step != .welcome {
            primaryButton(flow.primaryTitle) { flow.primary() }.disabled(!flow.primaryEnabled)
        }
    }

    private func primaryButton(_ title: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 8) {
                Text(title)
                Image(systemName: "arrow.right").accessibilityHidden(true)
            }
        }
        .buttonStyle(VoltButtonStyle())
        .keyboardShortcut(.defaultAction)
    }
}

/// The old app's film grain over the whole dark window: fine noise, screened
/// on at 5%.
private struct Grain: View {
    @Environment(\.colorScheme) private var colorScheme

    /// 180 × 180 pixels of grey noise, made once.
    private static let tile: CGImage? = {
        let side = 180
        var generator = SystemRandomNumberGenerator()
        let pixels = (0..<side * side).map { _ in UInt8.random(in: 0...255, using: &generator) }
        guard let provider = CGDataProvider(data: Data(pixels) as CFData) else { return nil }
        return CGImage(width: side, height: side, bitsPerComponent: 8, bitsPerPixel: 8, bytesPerRow: side,
                       space: CGColorSpaceCreateDeviceGray(), bitmapInfo: CGBitmapInfo(rawValue: 0),
                       provider: provider, decode: nil, shouldInterpolate: false, intent: .defaultIntent)
    }()

    var body: some View {
        if colorScheme == .dark, let tile = Self.tile {
            Image(decorative: tile, scale: 2)
                .resizable(resizingMode: .tile)
                .blendMode(.screen)
                .opacity(0.05)
                .allowsHitTesting(false)
                .accessibilityHidden(true)
        }
    }
}
