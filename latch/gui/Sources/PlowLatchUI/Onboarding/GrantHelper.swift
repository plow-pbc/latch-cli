import AppKit
import Observation
import SwiftUI

/// The Full Disk Access helper while it shows: told when access has landed,
/// or to go.
protocol GrantGuide: AnyObject {
    /// Access landed: the helper says so, then leaves on its own.
    func granted()
    func stop()
}

/// Where System Settings' window is, from the window server's list. Bounds,
/// owner and layer need no permission; window titles would need Screen
/// Recording, so the window is found by its owner's pid, never its name.
nonisolated struct SettingsTracker {
    enum Report: Equatable {
        case window(CGRect, front: Bool)
        /// System Settings closed, or never showed a window.
        case gone
    }

    static let interval: TimeInterval = 1.0 / 15
    /// A lookup misses while Settings opens or swaps panes, so one miss is
    /// not "closed"; a launch the deep link is still racing gets longer.
    static let missesWhileTracking = 12
    static let missesBeforeFirstWindow = 150

    private var last: Report?
    private var misses = 0

    /// One look at the window list: a report when Settings moved, came
    /// forward or went back, or closed; nil when nothing changed.
    mutating func observe(_ windows: [[String: Any]], pid: pid_t?) -> Report? {
        if let pid, let frame = Self.frame(of: pid, in: windows) {
            misses = 0
            let report = Report.window(frame, front: Self.isFrontmost(pid, in: windows))
            guard report != last else { return nil }
            last = report
            return report
        }
        misses += 1
        return misses >= (last == nil ? Self.missesBeforeFirstWindow : Self.missesWhileTracking) ? .gone : nil
    }

    /// The largest visible normal window `pid` owns: the main window, not
    /// a sheet or a toast.
    static func frame(of pid: pid_t, in windows: [[String: Any]]) -> CGRect? {
        windows
            .filter { $0[kCGWindowOwnerPID as String] as? pid_t == pid && isNormal($0) }
            .compactMap(bounds)
            .filter { $0.width > 320 && $0.height > 240 }
            .max { $0.width * $0.height < $1.width * $1.height }
    }

    /// Whether `pid` owns the frontmost normal window. The list runs front
    /// to back; the helper floats above normal windows, so never counts.
    static func isFrontmost(_ pid: pid_t, in windows: [[String: Any]]) -> Bool {
        for window in windows where isNormal(window) {
            guard let rect = bounds(window), rect.width >= 64, rect.height >= 64 else { continue }
            return window[kCGWindowOwnerPID as String] as? pid_t == pid
        }
        return false
    }

    private static func isNormal(_ window: [String: Any]) -> Bool {
        (window[kCGWindowLayer as String] as? Int ?? 0) == 0 && (window[kCGWindowAlpha as String] as? Double ?? 1) > 0
    }

    private static func bounds(_ window: [String: Any]) -> CGRect? {
        (window[kCGWindowBounds as String] as? NSDictionary).flatMap { CGRect(dictionaryRepresentation: $0) }
    }

    /// Settings' sidebar: the helper lines up with the content beside it.
    static let sidebarWidth: CGFloat = 230
    static let screenInset: CGFloat = 12
    static let minWidth: CGFloat = 240

    /// Where the helper sits for a Settings window: right under it, as wide
    /// as its content column, kept inside the screen's work area. Top-left
    /// coordinates, as the window list gives them.
    static func panelFrame(settings: CGRect, workArea: CGRect, height: CGFloat) -> CGRect {
        let width = min(max(minWidth, settings.width - sidebarWidth), workArea.width - screenInset * 2).rounded()
        func clamp(_ value: CGFloat, _ low: CGFloat, _ high: CGFloat) -> CGFloat { max(low, min(value, high)) }
        return CGRect(x: clamp(settings.minX + sidebarWidth, workArea.minX + screenInset, workArea.maxX - width - screenInset).rounded(),
                      y: clamp(settings.maxY, workArea.minY + screenInset, workArea.maxY - height - screenInset).rounded(),
                      width: width, height: height)
    }
}

/// The Full Disk Access helper: a small floating card under System
/// Settings' window, holding the app as a tile to drag straight into the
/// Full Disk Access list. It follows the window, shows only while Settings
/// is in front, and goes when access lands, Settings closes, or after
/// three minutes.
@Observable
final class GrantHelper: GrantGuide {
    /// System Settings' process, if it runs; a test build points this at a
    /// stand-in window instead.
    static var settingsApp: () -> pid_t? = {
        NSRunningApplication.runningApplications(withBundleIdentifier: "com.apple.systempreferences")
            .max { ($0.activationPolicy == .prohibited ? 0 : 1) < ($1.activationPolicy == .prohibited ? 0 : 1) }?
            .processIdentifier
    }
    static var windowList: () -> [[String: Any]] = {
        CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
    }

    static let timeout: Duration = .seconds(180)
    /// Long enough to read "granted" before the helper goes.
    static let linger: Duration = .milliseconds(2500)
    /// A cancelled drag's image slides home before the tile comes back.
    static let slideBack: Duration = .milliseconds(350)

    /// The app to drag in, as the Finder names it.
    let bundle: URL
    let name: String
    let icon: NSImage
    let label = "Full Disk Access"
    private(set) var isGranted = false
    private(set) var dragging = false

    @ObservationIgnored private var panel: NSPanel?
    @ObservationIgnored private var content: NSHostingController<GrantPanelView>?
    @ObservationIgnored private var poll: Timer?
    /// Keeps App Nap off while the helper floats: with this app's windows
    /// all behind Settings, a napping app would follow it seconds late.
    @ObservationIgnored private var activity: NSObjectProtocol?
    @ObservationIgnored private var tracker = SettingsTracker()
    @ObservationIgnored private var timers: [Task<Void, Never>] = []
    @ObservationIgnored private var holdRelease: Task<Void, Never>?
    /// Settings is in front; the helper shows only then.
    @ObservationIgnored private var settingsInFront = false
    /// A press or drag on the tile under way: hiding the panel now would
    /// cancel the drag.
    @ObservationIgnored private var holding = false
    @ObservationIgnored private(set) var settingsFrame: CGRect?

    init(bundle: URL, icon: NSImage? = nil) {
        self.bundle = bundle
        name = bundle.deletingPathExtension().lastPathComponent
        self.icon = icon ?? NSWorkspace.shared.icon(forFile: bundle.path)
    }

    /// For snapshots: as it shows when access landed, or mid-drag.
    func stage(granted: Bool, dragging: Bool) {
        isGranted = granted
        self.dragging = dragging
    }

    /// Floats the helper for `bundle`; it appears once Settings' window does.
    static func show(dragging bundle: URL) -> GrantHelper {
        let helper = GrantHelper(bundle: bundle)
        helper.open()
        return helper
    }

    private func open() {
        let content = NSHostingController(rootView: GrantPanelView(helper: self))
        content.sizingOptions = []
        let panel = GrantPanel(contentRect: NSRect(x: 0, y: 0, width: 420, height: 94),
                               styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: true)
        panel.title = "Grant \(label)"
        panel.contentViewController = content
        self.panel = panel
        self.content = content
        let poll = Timer(timeInterval: SettingsTracker.interval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.look() }
        }
        // Common modes: it keeps following while a drag runs its own loop.
        RunLoop.main.add(poll, forMode: .common)
        self.poll = poll
        activity = ProcessInfo.processInfo.beginActivity(options: .userInitiatedAllowingIdleSystemSleep,
                                                         reason: "Following System Settings for Full Disk Access")
        after(Self.timeout) { $0.stop() }
    }

    func granted() {
        guard panel != nil, !isGranted else { return }
        isGranted = true
        after(Self.linger) { $0.stop() }
    }

    func stop() {
        poll?.invalidate()
        poll = nil
        if let activity { ProcessInfo.processInfo.endActivity(activity) }
        activity = nil
        timers.forEach { $0.cancel() }
        timers = []
        holdRelease?.cancel()
        holdRelease = nil
        panel?.orderOut(nil)
        panel?.contentViewController = nil
        panel = nil
        content = nil
    }

    var isShowing: Bool { panel?.isVisible ?? false }
    /// The helper's frame on screen, top-left coordinates.
    var frame: CGRect? { panel.map { Self.flipped($0.frame) } }
    var contentView: NSView? { content?.view }

    private func after(_ delay: Duration, _ act: @escaping @MainActor (GrantHelper) -> Void) {
        timers.append(Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard !Task.isCancelled, let self else { return }
            act(self)
        })
    }

    private func look() {
        switch tracker.observe(Self.windowList(), pid: Self.settingsApp()) {
        case nil: break
        case .gone: stop()
        case let .window(frame, front):
            place(under: frame)
            settingsInFront = front
            showOrHide()
        }
    }

    private func place(under settings: CGRect) {
        guard let panel, let content else { return }
        settingsFrame = settings
        let screen = NSScreen.screens.max { a, b in
            Self.flipped(a.frame).intersection(settings).area < Self.flipped(b.frame).intersection(settings).area
        } ?? NSScreen.main
        guard let screen else { return }
        let workArea = Self.flipped(screen.visibleFrame)
        // The height the content needs at that width: a long name wraps.
        let width = SettingsTracker.panelFrame(settings: settings, workArea: workArea, height: 0).width
        let height = content.sizeThatFits(in: CGSize(width: width, height: 400)).height.rounded(.up)
        let frame = Self.flipped(SettingsTracker.panelFrame(settings: settings, workArea: workArea, height: height))
        if panel.frame != frame { panel.setFrame(frame, display: true) }
    }

    private func showOrHide() {
        guard let panel else { return }
        let show = settingsInFront || holding
        if show, !panel.isVisible { panel.orderFrontRegardless() }
        else if !show, panel.isVisible { panel.orderOut(nil) }
    }

    /// The tile is pressed (or let go): the helper stays on screen while a
    /// gesture is under way, for at most 30 s should the release never come.
    func hold(_ on: Bool) {
        holdRelease?.cancel()
        holding = on
        holdRelease = on ? Task { [weak self] in
            try? await Task.sleep(for: .seconds(30))
            guard !Task.isCancelled else { return }
            self?.hold(false)
        } : nil
        showOrHide()
    }

    /// The drag left the tile: the tile goes with it and the helper dims.
    func dragBegan() {
        dragging = true
        panel?.alphaValue = 0.72
    }

    func dragEnded() {
        Task { [weak self] in
            try? await Task.sleep(for: Self.slideBack)
            guard let self else { return }
            dragging = false
            panel?.alphaValue = 1
            hold(false)
        }
    }

    /// What the drag carries: the app itself, as the Finder would.
    static func dragItem(for bundle: URL) -> NSDraggingItem {
        NSDraggingItem(pasteboardWriter: bundle as NSURL)
    }

    /// Top-left screen coordinates (the window list's) to AppKit's
    /// bottom-left ones, and back: the same flip about the main screen.
    static func flipped(_ rect: CGRect) -> CGRect {
        let height = NSScreen.screens.first?.frame.height ?? 0
        return CGRect(x: rect.minX, y: height - rect.maxY, width: rect.width, height: rect.height)
    }
}

private extension CGRect {
    var area: CGFloat { isNull ? 0 : width * height }
}

/// A panel that never takes focus, so System Settings stays the app in
/// front while the tile is dragged.
private final class GrantPanel: NSPanel {
    override init(contentRect: NSRect, styleMask: NSWindow.StyleMask, backing: NSWindow.BackingStoreType, defer flag: Bool) {
        super.init(contentRect: contentRect, styleMask: styleMask, backing: backing, defer: flag)
        level = .floating
        collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .ignoresCycle]
        isOpaque = false
        backgroundColor = .clear
        hasShadow = true
        hidesOnDeactivate = false
        isReleasedWhenClosed = false
        becomesKeyOnlyIfNeeded = true
    }

    override var canBecomeKey: Bool { false }
    override var canBecomeMain: Bool { false }
}

/// The old app's helper colours, light and dark.
private enum HelperColor {
    static let panel = Color(light: 0xF6F6F1, dark: 0x141413, opacity: 0.94)
    static let panelBorder = Color(light: 0x121211, dark: 0xF4F3EC, opacity: 0.12)
    static let text = Color(light: 0x13130F, dark: 0xF4F3EC)
    static let muted = Color(light: 0x57574F, dark: 0xB1B0A7)
    static let accent = Color(light: 0x4C6606, dark: 0xD5EF8A)
    static let close = Color(light: 0x121211, dark: 0xF4F3EC, opacity: 0.1, darkOpacity: 0.12)
    static let tile = Color(light: 0xFFFFFF, dark: 0xF4F3EC, opacity: 0.7, darkOpacity: 0.06)
    static let tileBorder = Color(light: 0x121211, dark: 0xF4F3EC, opacity: 0.08, darkOpacity: 0.1)
    /// The tile as it rides a drag: its face over the panel, made opaque.
    static let tileLifted = Color(light: 0xFCFCFA, dark: 0x212120)
}

struct GrantPanelView: View {
    var helper: GrantHelper

    var body: some View {
        VStack(spacing: 6) {
            HStack(alignment: .top, spacing: 6) {
                // A new arrow once granted: a repeating animation can't be
                // stopped mid-swing, only replaced.
                WiggleArrow(still: helper.isGranted).id(helper.isGranted)
                header
                    .font(.system(size: 14))
                    .lineSpacing(3)
                    .foregroundStyle(HelperColor.text)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 0)
                Button { helper.stop() } label: {
                    Image(systemName: "xmark").font(.system(size: 8, weight: .semibold))
                        .frame(width: 20, height: 20)
                        .background(HelperColor.close, in: Circle())
                        .foregroundStyle(HelperColor.text)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Close")
            }
            DragTile(helper: helper)
                .frame(height: 46)
        }
        // The old card's padding, inside its 1-point border.
        .padding(EdgeInsets(top: 9, leading: 13, bottom: 13, trailing: 13))
        .background(HelperColor.panel, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 18, style: .continuous).strokeBorder(HelperColor.panelBorder))
    }

    private var header: Text {
        helper.isGranted
            ? Text("\(Text(helper.label).bold()) granted.")
            : Text("Drag \(Text(helper.name).bold()) to the list above to allow \(Text(helper.label).bold()).")
    }
}

/// The up arrow toward the list, wiggling until access lands.
private struct WiggleArrow: View {
    var still: Bool
    @State private var tilted = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let moving = tilted && !still && !reduceMotion
        Image(systemName: "arrowshape.up.fill")
            .font(.system(size: 13))
            .frame(width: 16, height: 16)
            .foregroundStyle(HelperColor.accent)
            .rotationEffect(.degrees(still || reduceMotion ? 0 : moving ? 12 : -12))
            .offset(y: still || reduceMotion ? 0 : moving ? -2 : 1)
            .animation(still || reduceMotion ? nil : .timingCurve(0.2, 0.7, 0.2, 1, duration: 0.22).repeatForever(autoreverses: true),
                       value: tilted)
            .padding(.top, 1)
            .onAppear { tilted = true }
            .accessibilityHidden(true)
    }
}

/// The app's icon and name, with a small "Drag" hint: what the person
/// picks up and drops on the Full Disk Access list.
struct TileFace: View {
    var helper: GrantHelper
    var lifted = false

    var body: some View {
        HStack(spacing: 8) {
            Image(nsImage: helper.icon).resizable().frame(width: 32, height: 32)
            Text(helper.name).font(.system(size: 16)).foregroundStyle(HelperColor.text).lineLimit(1)
            Spacer(minLength: 0)
            if !lifted {
                VStack(spacing: 1) {
                    Image(systemName: "hand.raised").font(.system(size: 13, weight: .light))
                    Text("Drag").font(.system(size: 8, weight: .light))
                }
                .foregroundStyle(HelperColor.muted)
                .padding(.trailing, 6)
                .accessibilityHidden(true)
            }
        }
        .padding(6)
        .frame(maxHeight: .infinity)
        .background(lifted ? HelperColor.tileLifted : HelperColor.tile, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 16, style: .continuous).strokeBorder(HelperColor.tileBorder))
        .opacity(helper.isGranted && !lifted ? 0.55 : 1)
    }
}

/// The tile as a drag source: AppKit's, so the drag carries the app's file
/// and its image is the tile lifted off the panel, grabbed where pressed.
private struct DragTile: NSViewRepresentable {
    var helper: GrantHelper
    @Environment(\.colorScheme) private var colorScheme

    func makeNSView(context: Context) -> DragSource {
        DragSource(helper: helper, face: NSHostingView(rootView: face))
    }

    func updateNSView(_ view: DragSource, context: Context) {
        view.face.rootView = face
        view.colorScheme = colorScheme
    }

    private var face: AnyView {
        AnyView(TileFace(helper: helper).opacity(helper.dragging ? 0 : 1).accessibilityLabel("Drag \(helper.name)"))
    }
}

private final class DragSource: NSView, NSDraggingSource {
    let helper: GrantHelper
    let face: NSHostingView<AnyView>
    var colorScheme: ColorScheme = .light
    private var pressedAt: NSPoint?

    init(helper: GrantHelper, face: NSHostingView<AnyView>) {
        self.helper = helper
        self.face = face
        super.init(frame: .zero)
        face.autoresizingMask = [.width, .height]
        addSubview(face)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    override func layout() {
        super.layout()
        face.frame = bounds
    }

    // The tile takes every click in it: the face is only its picture.
    override func hitTest(_ point: NSPoint) -> NSView? { frame.contains(point) ? self : nil }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
    override var mouseDownCanMoveWindow: Bool { false }
    override func resetCursorRects() { addCursorRect(bounds, cursor: .openHand) }

    override func mouseDown(with event: NSEvent) {
        pressedAt = convert(event.locationInWindow, from: nil)
        helper.hold(true)
    }

    override func mouseDragged(with event: NSEvent) {
        guard let start = pressedAt, !helper.isGranted else { return }
        let point = convert(event.locationInWindow, from: nil)
        guard hypot(point.x - start.x, point.y - start.y) > 3 else { return }
        pressedAt = nil
        let item = GrantHelper.dragItem(for: helper.bundle)
        item.setDraggingFrame(bounds, contents: liftedImage())
        helper.dragBegan()
        beginDraggingSession(with: [item], event: event, source: self)
    }

    override func mouseUp(with event: NSEvent) {
        guard pressedAt != nil else { return }
        pressedAt = nil
        helper.hold(false)
    }

    func draggingSession(_ session: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation {
        context == .outsideApplication ? [.copy, .link, .generic] : []
    }

    func draggingSession(_ session: NSDraggingSession, endedAt screenPoint: NSPoint, operation: NSDragOperation) {
        helper.dragEnded()
    }

    /// The tile as it looks on the panel, without the "Drag" hint.
    private func liftedImage() -> NSImage? {
        let renderer = ImageRenderer(content: TileFace(helper: helper, lifted: true)
            .frame(width: bounds.width, height: bounds.height)
            .environment(\.colorScheme, colorScheme))
        renderer.scale = window?.backingScaleFactor ?? 2
        return renderer.nsImage
    }
}
