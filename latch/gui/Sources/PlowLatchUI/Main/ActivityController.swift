import AppKit
import Quartz
import SwiftUI

/// Activity: every reachable host's records in an AppKit table, newest first
/// unless sorted otherwise, under the SwiftUI heading and scope bar and
/// over the footer, with the reasons there are none (offline, reading, no
/// match) in their place. The table is NSTableView's own: its sorting, type
/// to select, column moving and resizing, kept across launches with the
/// sort; a record selected opens in the window's inspector (a double-click
/// opens the inspector too), the space bar shows it in Quick Look, and
/// records arriving keep the rows in view where they are.
final class ActivityController: NSViewController, NSTableViewDataSource, NSTableViewDelegate, NSMenuDelegate,
    NSMenuItemValidation, QLPreviewPanelDataSource, QLPreviewPanelDelegate {
    let app: AppModel
    let table = ActivityTable()
    let shown = ActivityShown()
    /// The rows listed, in the table's order.
    private(set) var rows: [ActivityRow] = []
    /// Records whose cited message is being read for its row.
    private var citing: Set<String> = []
    /// Selecting rows to match the model, not a person's choice.
    private var selecting = false
    private var scheduled = false
    private let autosaveName: String?
    private let previews: QuickLookFiles
    /// The file Quick Look shows, while it shows one.
    private(set) var preview: URL?
    /// Quick Look was opened here, on the selected request: only then is
    /// the panel this table's. One the inspector opens on a screenshot is
    /// the inspector's, though the table has the keyboard.
    private var quickLooking = false

    private lazy var heading = Self.hosting(ActivityHeading(shown: shown), app, sizing: [.intrinsicContentSize])
    private lazy var states = Self.hosting(ActivityStates(shown: shown), app)
    private lazy var footer = Self.hosting(ActivityFooter(shown: shown), app)

    /// `autosaveName` keeps the columns and sort across launches (nil: a
    /// still's or a test's, which keeps nothing).
    init(app: AppModel, autosaveName: String?, previews: QuickLookFiles = .shared) {
        self.app = app
        self.autosaveName = autosaveName
        self.previews = previews
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { nil }

    private static func hosting(_ view: some View, _ app: AppModel, sizing: NSHostingSizingOptions = []) -> NSView {
        let hosting = NSHostingView(rootView: view.environment(app))
        // No sizes of their own beyond what's asked: a hosting view's
        // default constraints once shrank the window to an empty state's.
        hosting.sizingOptions = sizing
        return hosting
    }

    enum Column: String, CaseIterable {
        case time, host, status, request, intent, caller

        var title: String { self == .intent ? "Cited Message" : rawValue.capitalized }
        var widths: (min: CGFloat, ideal: CGFloat, max: CGFloat) {
            switch self {
            case .time: (95, 100, 130)
            case .host: (70, 100, 200)
            case .status: (140, 180, 400)
            case .request: (160, 260, 10_000)
            case .intent: (90, 150, 400)
            case .caller: (110, 140, 240)
            }
        }
    }

    override func loadView() {
        for column in Column.allCases {
            let item = NSTableColumn(identifier: .init(column.rawValue))
            item.title = column.title
            (item.minWidth, item.width, item.maxWidth) = column.widths
            // A change in the table's width is shared out by all but Time, so the columns fit the pane (down to
            // their minimums) before the table scrolls sideways.
            item.resizingMask = column == .time ? .userResizingMask : [.autoresizingMask, .userResizingMask]
            item.sortDescriptorPrototype = NSSortDescriptor(key: column.rawValue, ascending: column != .time)
            table.addTableColumn(item)
        }
        table.style = .inset
        table.usesAlternatingRowBackgroundColors = true
        table.allowsColumnReordering = true
        table.allowsMultipleSelection = false
        table.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.doubleAction = #selector(inspect(_:))
        table.space = { [weak self] in self?.toggleQuickLook() }
        let menu = NSMenu()
        menu.delegate = self
        table.menu = menu
        // Columns' order and widths, and the sort, as AppKit kept them.
        if let autosaveName {
            table.autosaveName = autosaveName
            table.autosaveTableColumns = true
        }
        if table.sortDescriptors.isEmpty { table.sortDescriptors = [Self.newestFirst] }

        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        heading.setContentCompressionResistancePriority(.init(1), for: .horizontal)
        heading.setContentHuggingPriority(.init(1), for: .horizontal)
        let container = NSView()
        for view in [heading, scroll, states, footer] {
            view.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(view)
            NSLayoutConstraint.activate([
                view.leadingAnchor.constraint(equalTo: container.leadingAnchor),
                view.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            ])
        }
        NSLayoutConstraint.activate([
            // Under the content bar, not behind it.
            heading.topAnchor.constraint(equalTo: container.safeAreaLayoutGuide.topAnchor),
            scroll.topAnchor.constraint(equalTo: heading.bottomAnchor),
            scroll.bottomAnchor.constraint(equalTo: footer.topAnchor),
            states.topAnchor.constraint(equalTo: heading.bottomAnchor),
            states.bottomAnchor.constraint(equalTo: footer.topAnchor),
            footer.bottomAnchor.constraint(equalTo: container.bottomAnchor),
            footer.heightAnchor.constraint(equalToConstant: 30),
        ])
        view = container
        follow()
    }

    /// The screen went: Quick Look's file with it.
    override func viewDidDisappear() {
        super.viewDidDisappear()
        endQuickLook()
    }

    static let newestFirst = NSSortDescriptor(key: Column.time.rawValue, ascending: false)

    // MARK: Following the model

    /// The host, its records, the filters and the selection, reloaded once
    /// however many changes come before the next pass of the run loop (a
    /// feed changes at most every ~100 ms).
    private func follow() {
        withObservationTracking { refresh() } onChange: { [weak self] in
            Task { @MainActor in
                guard let self, !self.scheduled else { return }
                self.scheduled = true
                DispatchQueue.main.async {
                    self.scheduled = false
                    self.follow()
                }
            }
        }
    }

    private func refresh() {
        let showing = app.route == .account(.activity)
        let hosts = showing ? app.reachableHosts : []
        let feeds = hosts.map { (host: $0, feed: app.feed(for: $0)) }
        let new = showing ? sorted(app.activityRows) : []
        let old = rows
        rows = new

        shown.set(\.hosts, hosts)
        shown.set(\.rows, new.count)
        shown.set(\.records, feeds.reduce(0) { $0 + $1.feed.records.count })
        shown.set(\.caughtUp, feeds.allSatisfy(\.feed.caughtUp))
        // From when every host's records go back.
        shown.set(\.since, feeds.compactMap { $0.feed.hasOlder ? $0.feed.records.first?.time : nil }.max())
        let failed = feeds.lazy.compactMap { host, feed in feed.error.map { ActivityShown.Failure(host: host, error: $0) } }.first
        if shown.error?.host != failed?.host || shown.error?.error.code != failed?.error.code { shown.error = failed }
        shown.set(\.agentName, app.activityAgent.flatMap { target in
            feeds.first { $0.host.name == target.host }?.feed.records.last { $0.agent == target.agent && $0.callerName != nil }?.callerName
        })
        if states.isHidden != !new.isEmpty { states.isHidden = !new.isEmpty }
        fitStatuses()

        let selected = app.selectedRecord
        // Records arrived on top of those shown: those shown stay where they
        // are on screen, unless the top is in view.
        let added = new.count - old.count
        // Only rows above every row shown, in their order: any other change reloads.
        if added > 0, !old.isEmpty, new.dropFirst(added).map(\.id) == old.map(\.id) {
            let clip = table.enclosingScrollView?.contentView
            let atTop = (clip?.bounds.minY ?? 0) <= table.rect(ofRow: 0).minY + 1
            selecting = true
            table.insertRows(at: IndexSet(0..<added), withAnimation: [])
            selecting = false
            if !atTop, let clip {
                let shift = table.rect(ofRow: added).minY - table.rect(ofRow: 0).minY
                clip.scroll(to: CGPoint(x: clip.bounds.minX, y: clip.bounds.minY + shift))
                table.enclosingScrollView?.reflectScrolledClipView(clip)
            }
        } else if new.map(\.id) != old.map(\.id) {
            // A row index past the new rows loses AppKit's selection; the
            // record selected is still the person's, picked again below.
            selecting = true
            table.reloadData()
            selecting = false
            // Another order, filter or host: the selected record in view.
            if let selected, let row = new.firstIndex(where: { $0.selection == selected }) {
                select(row)
                table.scrollRowToVisible(row)
            }
        }
        if let selected, let row = new.firstIndex(where: { $0.selection == selected }) {
            if table.selectedRow != row { select(row) }
        } else if table.selectedRow >= 0 {
            select(nil)
        }
        if preview != nil { previewSelected() }
    }

    private func select(_ row: Int?) {
        selecting = true
        if let row { table.selectRowIndexes([row], byExtendingSelection: false) } else { table.deselectAll(nil) }
        selecting = false
    }

    /// The table's order of the rows (which come newest first).
    private func sorted(_ rows: [ActivityRow]) -> [ActivityRow] {
        guard let sort = table.sortDescriptors.first, let key = sort.key.flatMap(Column.init(rawValue:)) else { return rows }
        if key == .time { return sort.ascending ? rows.reversed() : rows }
        // Ties keep the newest first.
        let keyed = rows.map { (row: $0, key: text(of: $0, key)) }
        return keyed.enumerated().sorted { a, b in
            let order = a.element.key.localizedStandardCompare(b.element.key)
            if order == .orderedSame { return a.offset < b.offset }
            return sort.ascending ? order == .orderedAscending : order == .orderedDescending
        }.map(\.element.row)
    }

    private func text(of row: ActivityRow, _ column: Column) -> String {
        let record = row.record
        return switch column {
        case .time: record.time.formatted(.dateTime.hour().minute().second())
        case .host: row.host.name
        case .status: record.status.title
        case .request: record.request ?? "—"
        // The message's own words once its kept call is read; a citation
        // that can't be read is left blank, never its id.
        case .intent: app.keptCalls[record.id]?.cited ?? ""
        case .caller: app.caller(of: record, on: row.host).who
        }
    }

    /// The status column as wide as its widest status: a status is never
    /// cut short.
    private func fitStatuses() {
        guard let column = table.tableColumn(withIdentifier: .init(Column.status.rawValue)) else { return }
        let font = NSFont.systemFont(ofSize: NSFont.systemFontSize)
        let widest = Set(rows.map(\.record.status.title)).map { ($0 as NSString).size(withAttributes: [.font: font]).width }.max() ?? 0
        // The dot, the gaps and the cell's insets.
        let fit = max(Column.status.widths.min, ceil(widest) + 30)
        if column.minWidth != fit {
            column.minWidth = fit
            if column.width < fit { column.width = fit }
        }
    }

    /// Reads the message a row cites, once, and shows it in its row.
    private func cite(_ row: ActivityRow) {
        let record = row.record
        guard record.gate?.keptRequest != nil, app.keptCalls[record.id] == nil, !app.still, citing.insert(record.id).inserted else { return }
        Task { [weak self] in
            guard let self, await app.readKeptCall(record, on: row.host) != nil,
                  let index = rows.firstIndex(where: { $0.id == row.id }),
                  let column = table.tableColumns.firstIndex(where: { $0.identifier.rawValue == Column.intent.rawValue }) else { return }
            table.reloadData(forRowIndexes: [index], columnIndexes: [column])
        }
    }

    // MARK: The table

    func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    func tableView(_ tableView: NSTableView, sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
        refresh()
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !selecting else { return }
        let row = table.selectedRow
        app.selectedRecord = row >= 0 ? rows[row].selection : nil
    }

    /// Type to select: the column sorted by (Request when sorted by time).
    func tableView(_ tableView: NSTableView, typeSelectStringFor tableColumn: NSTableColumn?, row: Int) -> String? {
        let sorted = tableView.sortDescriptors.first?.key.flatMap(Column.init(rawValue:)) ?? .time
        let column = sorted == .time ? .request : sorted
        guard tableColumn?.identifier.rawValue == column.rawValue else { return nil }
        return text(of: rows[row], column)
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard let id = tableColumn?.identifier, let column = Column(rawValue: id.rawValue) else { return nil }
        let cell = tableView.makeView(withIdentifier: id, owner: self) as? NSTableCellView ?? Self.cell(column)
        let record = rows[row].record
        switch column {
        case .time, .host:
            cell.textField?.stringValue = text(of: rows[row], column)
        case .status:
            cell.imageView?.contentTintColor = Self.color(record.status)
            cell.textField?.stringValue = record.status.title
            cell.toolTip = record.status.title
        case .request:
            cell.textField?.stringValue = text(of: rows[row], .request)
            cell.toolTip = record.command ?? "A connection"
        case .intent:
            cite(rows[row])
            let cited = text(of: rows[row], .intent)
            cell.textField?.stringValue = cited
            cell.toolTip = cited.isEmpty ? nil : "Your message it cited: \(cited)"
        case .caller:
            let caller = app.caller(of: record, on: rows[row].host)
            // One line, cut short at its end like the others.
            let style = NSMutableParagraphStyle()
            style.lineBreakMode = .byTruncatingTail
            let line = NSMutableAttributedString(string: caller.who + " ", attributes: [.foregroundColor: NSColor.labelColor, .paragraphStyle: style])
            line.append(NSAttributedString(string: "· \(caller.kind)", attributes: [.foregroundColor: NSColor.secondaryLabelColor, .paragraphStyle: style]))
            cell.textField?.attributedStringValue = line
        }
        return cell
    }

    private static func cell(_ column: Column) -> NSTableCellView {
        let cell = NSTableCellView()
        cell.identifier = .init(column.rawValue)
        let text = NSTextField(labelWithString: "")
        text.translatesAutoresizingMaskIntoConstraints = false
        text.lineBreakMode = column == .request ? .byTruncatingMiddle : .byTruncatingTail
        text.maximumNumberOfLines = 1
        text.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        if column == .request { text.font = .monospacedSystemFont(ofSize: NSFont.systemFontSize, weight: .regular) }
        if column == .time { text.textColor = .secondaryLabelColor }
        cell.addSubview(text)
        cell.textField = text
        var leading = cell.leadingAnchor
        if column == .status {
            // The status's dot before its words, which carry it all. Out of
            // VoiceOver: an image view's accessibility element is its cell.
            let dot = NSImageView(image: NSImage(systemSymbolName: "circle.fill", accessibilityDescription: nil)!
                .withSymbolConfiguration(.init(pointSize: 7, weight: .regular))!)
            dot.translatesAutoresizingMaskIntoConstraints = false
            dot.cell?.setAccessibilityElement(false)
            // Its own size, however wide the column: the words follow it.
            dot.setContentHuggingPriority(.required, for: .horizontal)
            cell.addSubview(dot)
            cell.imageView = dot
            NSLayoutConstraint.activate([
                dot.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 2),
                dot.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
            ])
            leading = dot.trailingAnchor
        }
        NSLayoutConstraint.activate([
            text.leadingAnchor.constraint(equalTo: leading, constant: column == .status ? 6 : 2),
            text.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -2),
            text.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }

    private static func color(_ status: AuditRecord.Status) -> NSColor {
        switch status {
        case .connected: .secondaryLabelColor
        case .allowed: .systemGreen
        case .gatekeeper("not_covered"): .systemOrange
        case .gatekeeper, .refused: .systemRed
        case .stopped: .systemYellow
        }
    }

    /// A double-click inspects the row: the inspector opens if it's closed.
    @objc private func inspect(_ sender: Any?) {
        guard table.clickedRow >= 0, !app.inspecting.contains(.account(.activity)) else { return }
        NSApp.sendAction(#selector(NSSplitViewController.toggleInspector(_:)), to: nil, from: self)
    }

    // MARK: Copy and the row's menu

    private var selectedRecord: ActivityRow? { table.selectedRow >= 0 ? rows[table.selectedRow] : nil }

    /// Edit › Copy (⌘C): the selected request.
    @objc func copy(_ sender: Any?) {
        guard let record = selectedRecord?.record else { return }
        app.copy(record.command ?? record.status.title)
    }

    func validateMenuItem(_ item: NSMenuItem) -> Bool {
        switch item.action {
        case #selector(copy(_:)): selectedRecord != nil
        default: true
        }
    }

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        guard table.clickedRow >= 0 else { return }
        let row = rows[table.clickedRow], record = row.record
        func add(_ title: String, _ action: Selector) {
            let item = NSMenuItem(title: title, action: action, keyEquivalent: "")
            item.target = self
            item.representedObject = row.id
            menu.addItem(item)
        }
        add("Quick Look", #selector(quickLookClicked(_:)))
        menu.addItem(.separator())
        add("Copy Request", #selector(copyClicked(_:)))
        if let agent = record.agent, app.activityAgent(on: row.host) != agent {
            menu.addItem(.separator())
            add("Show Only \(record.callerName ?? "This Agent")\u{2019}s Requests", #selector(filterClicked(_:)))
        }
    }

    private func clicked(_ sender: NSMenuItem) -> ActivityRow? {
        rows.first { $0.id == sender.representedObject as? String }
    }

    @objc private func copyClicked(_ sender: NSMenuItem) {
        if let record = clicked(sender)?.record { app.copy(record.command ?? record.status.title) }
    }

    @objc private func filterClicked(_ sender: NSMenuItem) {
        if let row = clicked(sender), let agent = row.record.agent { app.activityAgent = AgentTarget(host: row.host.name, agent: agent) }
    }

    @objc private func quickLookClicked(_ sender: NSMenuItem) {
        guard let row = clicked(sender), let index = rows.firstIndex(of: row) else { return }
        select(index)
        app.selectedRecord = row.selection
        if preview == nil { toggleQuickLook() }
    }

    // MARK: Quick Look

    /// The space bar: Quick Look on the selected request, or closed.
    func toggleQuickLook() {
        if QLPreviewPanel.sharedPreviewPanelExists(), QLPreviewPanel.shared().isVisible {
            QLPreviewPanel.shared().orderOut(nil)
        } else if selectedRecord != nil {
            quickLooking = true
            QLPreviewPanel.shared().makeKeyAndOrderFront(nil)
        }
    }

    /// The selected request's file for Quick Look, the one before it gone.
    func previewSelected() {
        let old = preview
        preview = selectedRecord.flatMap { row in previews.write(previewText(row), name: "Request \(row.record.id.suffix(12)).txt") }
        if let old, old != preview { previews.remove(old) }
        if QLPreviewPanel.sharedPreviewPanelExists(), QLPreviewPanel.shared().isVisible { QLPreviewPanel.shared().reloadData() }
    }

    /// Quick Look closed, or the screen went: its file goes.
    func endQuickLook() {
        if let preview { previews.remove(preview) }
        preview = nil
    }

    private func previewText(_ row: ActivityRow) -> String {
        let record = row.record, host = row.host
        let caller = app.caller(of: record, on: host)
        return """
        \(record.command ?? "A connection")

        \(record.status.title)
        Caller: \(caller.who) · \(caller.kind)
        Host: \(host.name)
        Time: \(record.time.formatted(date: .abbreviated, time: .standard))
        Record: \(record.id)
        """
    }

    // Quick Look asks these on the main thread, as AppKit does.
    nonisolated override func acceptsPreviewPanelControl(_ panel: QLPreviewPanel!) -> Bool {
        MainActor.assumeIsolated { quickLooking }
    }

    nonisolated override func beginPreviewPanelControl(_ panel: QLPreviewPanel!) {
        MainActor.assumeIsolated {
            panel.dataSource = self
            panel.delegate = self
            previewSelected()
        }
    }

    nonisolated override func endPreviewPanelControl(_ panel: QLPreviewPanel!) {
        MainActor.assumeIsolated {
            panel.dataSource = nil
            panel.delegate = nil
            quickLooking = false
            endQuickLook()
        }
    }

    func numberOfPreviewItems(in panel: QLPreviewPanel!) -> Int { preview == nil ? 0 : 1 }

    func previewPanel(_ panel: QLPreviewPanel!, previewItemAt index: Int) -> (any QLPreviewItem)! { preview as NSURL? }

    /// The panel's keys go to the table: the arrows move the selection, and
    /// the preview with it, and the space bar closes it, as in Finder.
    func previewPanel(_ panel: QLPreviewPanel!, handle event: NSEvent!) -> Bool {
        guard event.type == .keyDown else { return false }
        table.keyDown(with: event)
        return true
    }
}

/// The space bar is Quick Look's; every other key is the table's.
final class ActivityTable: NSTableView {
    var space: () -> Void = {}

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 49, event.modifierFlags.intersection(.deviceIndependentFlagsMask).isEmpty { space() } else { super.keyDown(with: event) }
    }
}
