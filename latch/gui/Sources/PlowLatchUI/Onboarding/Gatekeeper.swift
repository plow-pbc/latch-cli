import SwiftUI

/// Set your rules, as the old app's Gatekeeper screen: the rules box, two
/// defaults, and five requests crossing a glowing boundary to the Mac.
/// An allowed one passes the beam; a refused one stops at it.
struct RulesView: View {
    @Bindable var flow: Onboarding
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(spacing: 0) {
            VStack(spacing: 10) {
                Text("Meet the Gatekeeper")
                    .font(.system(size: 34, weight: .semibold))
                    .accessibilityAddTraits(.isHeader)
                Text("Your agents can look things up and create or edit ordinary files on their own, and they see what they find. Anything riskier, like sending, buying, deleting or giving a program internet access, needs your OK in your rules or your Plow chat.")
                    .font(.system(size: 15))
                    .lineSpacing(4)
                    .foregroundStyle(Brand.muted)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Text("What may your agents do on this Mac?")
                .font(.system(size: 14, weight: .medium))
                .padding(.top, 24)
                .padding(.bottom, 10)
            field
            HStack(alignment: .firstTextBaseline, spacing: 9) {
                Text("Start with:").foregroundStyle(Brand.muted)
                ForEach(Onboarding.decks) { deck in
                    Button(deck.name) { flow.choose(deck) }
                        .buttonStyle(.plain)
                        .foregroundStyle(flow.deck.name == deck.name && flow.draftRules == deck.rules ? Brand.volt : Brand.sage)
                        .overlay(alignment: .bottom) { Rectangle().fill(Brand.sage.opacity(0.3)).frame(height: 1).offset(y: 2) }
                }
            }
            .font(.system(size: 12.5))
            .padding(.top, 10)
            if let error = flow.rulesError {
                ErrorText(error: error, host: ErrorCopy.thisMac).foregroundStyle(Brand.clay).font(.callout).padding(.top, 8)
            }
            Deck(flow: flow, reduceMotion: reduceMotion)
                .padding(.top, 14)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 56)
        .padding(.top, 46)
        .task { if !flow.app.still { await flow.loadRules() } }
        .onChange(of: flow.draftRules) { if !flow.app.still { flow.draftChanged() } }
    }

    private var field: some View {
        TextEditor(text: $flow.draftRules)
            .font(.system(size: 13.5))
            .lineSpacing(3)
            .scrollContentBackground(.hidden)
            .padding(.horizontal, 8)
            .padding(.vertical, 8)
            .frame(height: 76)
            .writingToolsBehavior(.disabled)
            .background(Brand.surface, in: RoundedRectangle(cornerRadius: 11, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 11, style: .continuous).strokeBorder(Brand.fieldBorder))
            .accessibilityLabel("Your rules for agents on this Mac")
    }
}

/// The five lanes, the beam and the Mac. Geometry is the old app's: a lane
/// 46 pt tall with a 36 pt pill, the Mac 118 pt wide at the right, the beam
/// 168 pt from the right edge.
private struct Deck: View {
    var flow: Onboarding
    var reduceMotion: Bool
    static let macWidth: CGFloat = 118
    static let beamX: CGFloat = 168

    var body: some View {
        GeometryReader { proxy in
            let width = proxy.size.width
            ZStack(alignment: .topLeading) {
                MacDrawing(lit: flow.macLit)
                    .frame(width: Self.macWidth, height: Self.macWidth * 86 / 120)
                    .position(x: width - Self.macWidth / 2, y: lanesHeight / 2)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(flow.deck.examples) { example in
                        Lane(example: example, preview: flow.previews[example.id] ?? .notChecked,
                             open: flow.openExamples.contains(example.id), width: width, reduceMotion: reduceMotion) {
                            if flow.openExamples.contains(example.id) {
                                flow.openExamples.remove(example.id)
                            } else {
                                flow.openExamples.insert(example.id)
                            }
                        }
                    }
                }
                Beam(scanning: flow.reviewing && !reduceMotion)
                    .frame(width: 2, height: lanesHeight + 24)
                    .offset(x: width - Self.beamX, y: -12)
                    .accessibilityHidden(true)
            }
        }
        .frame(height: lanesHeight + extraHeight)
    }

    private var lanesHeight: CGFloat { CGFloat(flow.deck.examples.count) * 46 }
    private var extraHeight: CGFloat { CGFloat(flow.openExamples.count) * 102 }
}

private struct Lane: View {
    var example: Onboarding.Example
    var preview: Onboarding.Preview
    var open: Bool
    var width: CGFloat
    var reduceMotion: Bool
    var toggle: () -> Void
    @State private var bump: CGFloat = 0
    @State private var flare = 0.0

    private enum Look { case idle, checking, allowed, refused, unavailable }

    private var look: Look {
        switch preview {
        case .notChecked: .idle
        case .checking: .checking
        case .failed, .noAnswer: .unavailable
        case let .verdict(answer): answer.verdict == "allow" ? .allowed : .refused
        }
    }

    private var pillWidth: CGFloat {
        switch look {
        case .allowed: width - Deck.macWidth - 8
        case .refused: width - Deck.beamX - 4
        default: width - Deck.beamX - 24
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ZStack(alignment: .leading) {
                Button(action: toggle) { pill }
                    .buttonStyle(.plain)
                    .frame(width: pillWidth, height: 36, alignment: .leading)
                    .offset(x: bump)
                    .accessibilityLabel("\(example.title): \(verdictWord)")
                    .accessibilityHint(open ? "Hides the request and decision." : "Shows the request and decision.")
                Ellipse()
                    .fill(RadialGradient(colors: [(look == .allowed ? Brand.voltFill : Color(light: 0xA14A2B, dark: 0xD89478)).opacity(0.8), .clear],
                                         center: .center, startRadius: 0, endRadius: 15))
                    .frame(width: 30, height: 56)
                    .offset(x: width - Deck.beamX - 15)
                    .opacity(flare)
                    .allowsHitTesting(false)
                    .accessibilityHidden(true)
            }
            .frame(height: 46)
            if open { detail.transition(.opacity) }
        }
        .frame(width: width, alignment: .leading)
        .animation(reduceMotion ? nil : .timingCurve(0.2, 0.9, 0.2, 1, duration: 0.6), value: pillWidth)
        .animation(reduceMotion ? nil : .easeOut(duration: 0.3), value: open)
        .onChange(of: look) { _, now in arrived(now) }
    }

    private var pill: some View {
        HStack(spacing: 10) {
            Image(systemName: example.symbol)
                .frame(width: 16, height: 16)
                .foregroundStyle(tint ?? Brand.muted)
            Text(example.title).lineLimit(1).truncationMode(.tail)
            Spacer(minLength: 6)
            end
        }
        .font(.system(size: 13.5))
        .foregroundStyle(look == .refused || look == .unavailable ? Brand.muted : Brand.text)
        .padding(.horizontal, 14)
        .frame(height: 36)
        .background(fill, in: RoundedRectangle(cornerRadius: 11, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 11, style: .continuous)
            .strokeBorder(border, style: StrokeStyle(lineWidth: 1, dash: look == .checking ? [4, 3] : [])))
        .contentShape(Rectangle())
    }

    @ViewBuilder private var end: some View {
        switch look {
        case .allowed: Text("✓").font(.system(size: 13.5, weight: .bold)).foregroundStyle(Brand.volt).transition(.scale(scale: 0.8).combined(with: .opacity))
        case .refused: Text("✕").font(.system(size: 13.5, weight: .bold)).foregroundStyle(Color(light: 0xA14A2B, dark: 0xD89478)).transition(.scale(scale: 0.8).combined(with: .opacity))
        case .checking: Shimmer(text: "Checking…", moving: !reduceMotion)
        default: EmptyView()
        }
    }

    private var tint: Color? {
        switch look {
        case .allowed: Brand.volt
        case .refused: Color(light: 0xA14A2B, dark: 0xD89478)
        default: nil
        }
    }

    private var fill: Color {
        switch look {
        case .allowed: Color(light: 0xEEF3DC, dark: 0x181A14)
        case .refused: Color(light: 0xA14A2B, dark: 0xC07355).opacity(0.07)
        default: Color(light: 0xFBFAF6, dark: 0x161614)
        }
    }

    private var border: Color {
        switch look {
        case .allowed: Brand.volt.opacity(0.3)
        case .refused: Color(light: 0xA14A2B, dark: 0xD89478).opacity(0.45)
        default: Brand.fieldBorder
        }
    }

    private var verdictWord: String {
        switch preview {
        case .notChecked: "Not checked"
        case .checking: "Checking…"
        case .failed: "Couldn't check"
        case .noAnswer: "Didn't answer"
        case let .verdict(answer):
            switch answer.verdict {
            case "allow": "Allowed"
            case "owner_said_no": "You said no"
            case "host_refused": "Refused by this Mac"
            default: "Needs your OK"
            }
        }
    }

    /// Why it couldn't be checked, or why this Mac refuses it, in the
    /// app's words: one sentence, with no step to take from here.
    private var reason: String? {
        switch preview {
        case let .failed(error): ErrorCopy.said(error, host: ErrorCopy.thisMac, place: .tryRequest(connector: example.request.words?.first))
        case let .verdict(answer):
            answer.refused.flatMap {
                ErrorCopy.sentence(.failed($0), host: ErrorCopy.thisMac, place: .tryRequest(connector: answer.connector))
            }.map(ErrorCopy.first)
        case .noAnswer: "The gatekeeper took longer than 15 seconds."
        default: nil
        }
    }

    /// latch's words for a failed check or a refusal, for Details.
    private var latchSaid: LatchError? {
        switch preview {
        case let .failed(error): error
        case let .verdict(answer): answer.refused.map(LatchError.failed)
        default: nil
        }
    }

    private var detail: some View {
        VStack(alignment: .leading, spacing: 5) {
            Text(example.detail)
                .font(.system(size: 12))
                .foregroundStyle(Brand.text)
            (Text("Decision: ").fontWeight(.semibold).foregroundStyle(Brand.text)
                + Text(verdictWord + (reason.map { ". \($0)" } ?? "")))
                .font(.system(size: 12))
                .foregroundStyle(Brand.muted)
                .fixedSize(horizontal: false, vertical: true)
            if let latchSaid {
                ErrorDetails(error: latchSaid).foregroundStyle(Brand.muted)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .frame(width: width - Deck.beamX - 24, alignment: .leading)
        .background(Color(light: 0xFBFAF6, dark: 0x161614), in: RoundedRectangle(cornerRadius: 11, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 11, style: .continuous).strokeBorder(Brand.fieldBorder))
        .padding(.bottom, 8)
    }

    /// A verdict arrived: the flare at the beam, and a refused pill's bump.
    private func arrived(_ look: Look) {
        guard !reduceMotion, look == .allowed || look == .refused else { return }
        flare = 0
        withAnimation(.easeOut(duration: 0.22)) { flare = 0.65 }
        withAnimation(.easeOut(duration: look == .allowed ? 0.68 : 0.78).delay(0.22)) { flare = 0 }
        if look == .refused {
            withAnimation(.easeOut(duration: 0.22).delay(0.45)) { bump = -4 }
            withAnimation(.easeOut(duration: 0.23).delay(0.67)) { bump = 0 }
        }
    }
}

/// Checking…, with a highlight sweeping through it while latch judges.
private struct Shimmer: View {
    var text: String
    var moving: Bool

    var body: some View {
        let label = Text(text).font(.system(size: 12, weight: .medium))
        if moving {
            TimelineView(.animation) { context in
                let t = context.date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: 1.4) / 1.4
                label.foregroundStyle(LinearGradient(stops: [.init(color: Brand.muted, location: t - 0.15), .init(color: Brand.text, location: t),
                                                             .init(color: Brand.muted, location: t + 0.15)],
                                                     startPoint: .leading, endPoint: .trailing))
            }
        } else {
            label.foregroundStyle(Brand.muted)
        }
    }
}

/// The boundary: a glowing volt line, with a bright band that scans down it
/// while latch is judging.
private struct Beam: View {
    var scanning: Bool

    var body: some View {
        GeometryReader { proxy in
            ZStack(alignment: .top) {
                RoundedRectangle(cornerRadius: 1)
                    .fill(LinearGradient(stops: [.init(color: .clear, location: 0), .init(color: Brand.voltFill.opacity(0.85), location: 0.1),
                                                 .init(color: Brand.voltFill.opacity(0.85), location: 0.9), .init(color: .clear, location: 1)],
                                         startPoint: .top, endPoint: .bottom))
                    .shadow(color: Brand.voltFill.opacity(0.35), radius: 6)
                    .shadow(color: Brand.voltFill.opacity(0.1), radius: 18)
                if scanning {
                    TimelineView(.animation) { context in
                        let t = context.date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: 1.1) / 1.1
                        LinearGradient(colors: [.clear, Color(red: 1, green: 1, blue: 0.94).opacity(0.95), .clear], startPoint: .top, endPoint: .bottom)
                            .frame(width: 8, height: 70)
                            .offset(y: -70 + (proxy.size.height + 70) * t)
                    }
                    .frame(width: 8)
                }
            }
        }
    }
}

/// The old app's little Mac (a 120 × 86 drawing), whose glow lights up when
/// a request gets through.
private struct MacDrawing: View {
    var lit: Bool

    var body: some View {
        Canvas { context, size in
            let s = size.width / 120
            func p(_ x: Double, _ y: Double) -> CGPoint { CGPoint(x: x * s, y: y * s) }
            let stroke = Color(light: 0x161614, dark: 0xF0F0E8, opacity: 0.34)
            let detail = Color(light: 0x161614, dark: 0xF0F0E8, opacity: 0.42)
            // The glow under it and the shadow.
            context.opacity = lit ? 0.85 : 0.12
            context.fill(Path(ellipseIn: CGRect(x: 6 * s, y: 71 * s, width: 108 * s, height: 12 * s)),
                         with: .radialGradient(Gradient(colors: [Brand.voltFill.opacity(0.55), .clear]), center: p(60, 77), startRadius: 0, endRadius: 54 * s))
            context.opacity = 1
            context.fill(Path(ellipseIn: CGRect(x: 10 * s, y: 72.9 * s, width: 100 * s, height: 5.2 * s)), with: .color(.black.opacity(0.55)))
            // The front.
            var front = Path()
            front.move(to: p(8.5, 36)); front.addLine(to: p(111.5, 36)); front.addLine(to: p(111.5, 65))
            front.addQuadCurve(to: p(102, 74), control: p(111.5, 74)); front.addLine(to: p(18, 74))
            front.addQuadCurve(to: p(8.5, 65), control: p(8.5, 74)); front.closeSubpath()
            context.fill(front, with: .linearGradient(Gradient(colors: [Color(light: 0xE6E5DE, dark: 0x191A16), Color(light: 0xDAD9D2, dark: 0x121310)]),
                                                      startPoint: p(60, 36), endPoint: p(60, 74)))
            context.stroke(front, with: .color(stroke), style: StrokeStyle(lineWidth: 1.3 * s, lineJoin: .round))
            // The top.
            var top = Path()
            top.move(to: p(17, 23)); top.addQuadCurve(to: p(26, 18), control: p(18, 18)); top.addLine(to: p(94, 18))
            top.addQuadCurve(to: p(103, 23), control: p(102, 18)); top.addLine(to: p(111, 32))
            top.addQuadCurve(to: p(107, 36.5), control: p(113, 36.5)); top.addLine(to: p(13, 36.5))
            top.addQuadCurve(to: p(9, 32), control: p(7, 36.5)); top.closeSubpath()
            context.fill(top, with: .linearGradient(Gradient(colors: [Color(light: 0xEFEEE7, dark: 0x1B1C18), Color(light: 0xE6E5DE, dark: 0x222420)]),
                                                    startPoint: p(60, 18), endPoint: p(60, 36.5)))
            context.stroke(top, with: .color(stroke), style: StrokeStyle(lineWidth: 1.3 * s, lineJoin: .round))
            var seam = Path(); seam.move(to: p(13, 36)); seam.addLine(to: p(107, 36))
            context.stroke(seam, with: .color(Color(light: 0x161614, dark: 0xF0F0E8, opacity: 0.12)), lineWidth: s)
            // Ports, a button, and the light.
            for x in [23.0, 31.0] {
                context.stroke(Path(roundedRect: CGRect(x: x * s, y: 53 * s, width: 3.6 * s, height: 10 * s), cornerRadius: 1.8 * s),
                               with: .color(detail), lineWidth: 1.1 * s)
            }
            context.stroke(Path(ellipseIn: CGRect(x: 93.7 * s, y: 55.7 * s, width: 4.6 * s, height: 4.6 * s)), with: .color(detail), lineWidth: 1.1 * s)
            context.opacity = lit ? 0.85 : 0.12
            context.fill(Path(ellipseIn: CGRect(x: 81 * s, y: 53 * s, width: 10 * s, height: 10 * s)),
                         with: .radialGradient(Gradient(stops: [.init(color: Color(red: 0.92, green: 1, blue: 0.69), location: 0),
                                                                .init(color: Brand.voltFill.opacity(0.8), location: 0.35),
                                                                .init(color: Brand.voltFill.opacity(0), location: 1)]),
                                               center: p(86, 58), startRadius: 0, endRadius: 5 * s))
            context.opacity = 1
            context.fill(Path(ellipseIn: CGRect(x: 85.1 * s, y: 57.1 * s, width: 1.8 * s, height: 1.8 * s)), with: .color(Color(light: 0x161614, dark: 0xF0F0E8, opacity: 0.5)))
        }
        .animation(.easeOut(duration: lit ? 0.08 : 0.6), value: lit)
    }
}
