import AppKit

final class KeyPanel: NSPanel {
    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { true }
}

final class MainWindowController: NSWindowController, NSWindowDelegate, NSTableViewDataSource, NSTableViewDelegate, NSTextFieldDelegate, NSMenuDelegate {
    private let logger: EventLogger
    private let provider = FileProvider()
    private let quickLook = QuickLookController()
    private var tabs = [BrowserTab()]
    private var activeTabIndex = 0
    private var tabButtons = NSStackView()
    private let pathField = NSTextField()
    private let tableView = BrowserTableView()
    private let statusLabel = NSTextField(labelWithString: "")
    private let dateFormatter: DateFormatter = {
        let formatter = DateFormatter()
        formatter.dateStyle = .medium
        formatter.timeStyle = .short
        return formatter
    }()
    private let byteFormatter: ByteCountFormatter = {
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        return formatter
    }()

    var activeTab: BrowserTab { tabs[activeTabIndex] }
    var isQuickLookVisible: Bool { quickLook.isVisible }

    init(logger: EventLogger) {
        self.logger = logger
        let panel = KeyPanel(
            contentRect: NSRect(x: 0, y: 0, width: 920, height: 640),
            styleMask: [.titled, .closable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        panel.title = "Visual Files AppKit Demo"
        panel.isFloatingPanel = true
        panel.hidesOnDeactivate = false
        panel.collectionBehavior = [.moveToActiveSpace, .fullScreenAuxiliary]
        panel.setFrameAutosaveName("VisualFilesAppKitMainWindow")
        super.init(window: panel)
        panel.delegate = self
        buildInterface(in: panel)
        rebuildTabs()
        renderActiveTab()
        loadRecents(into: activeTab)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    private func buildInterface(in panel: NSPanel) {
        let root = NSView()
        root.translatesAutoresizingMaskIntoConstraints = false
        panel.contentView = root

        tabButtons = NSStackView()
        tabButtons.orientation = .horizontal
        tabButtons.alignment = .centerY
        tabButtons.spacing = 6
        tabButtons.translatesAutoresizingMaskIntoConstraints = false

        let tabScroll = NSScrollView()
        tabScroll.drawsBackground = false
        tabScroll.hasHorizontalScroller = true
        tabScroll.hasVerticalScroller = false
        tabScroll.documentView = tabButtons
        tabScroll.translatesAutoresizingMaskIntoConstraints = false

        pathField.placeholderString = "Paste an absolute path, ~/path, or benchmark:10k"
        pathField.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        pathField.delegate = self
        pathField.target = self
        pathField.action = #selector(pathSubmitted)
        pathField.translatesAutoresizingMaskIntoConstraints = false

        tableView.headerView = NSTableHeaderView()
        tableView.usesAlternatingRowBackgroundColors = true
        tableView.allowsEmptySelection = true
        tableView.allowsMultipleSelection = false
        tableView.rowHeight = 24
        tableView.dataSource = self
        tableView.delegate = self
        tableView.target = self
        tableView.action = #selector(tableClicked)
        tableView.doubleAction = #selector(performPrimaryAction)
        tableView.onPrimaryAction = { [weak self] in self?.performPrimaryAction() }
        tableView.onBack = { [weak self] in self?.goBack() }
        tableView.onQuickLook = { [weak self] in self?.toggleQuickLook() }
        addColumn("name", title: "Name", width: 420)
        addColumn("kind", title: "Kind", width: 180)
        addColumn("modified", title: "Modified", width: 180)
        addColumn("size", title: "Size", width: 100)

        let contextMenu = NSMenu(title: "Item")
        contextMenu.delegate = self
        tableView.menu = contextMenu

        let tableScroll = NSScrollView()
        tableScroll.documentView = tableView
        tableScroll.hasVerticalScroller = true
        tableScroll.hasHorizontalScroller = true
        tableScroll.autohidesScrollers = true
        tableScroll.translatesAutoresizingMaskIntoConstraints = false

        statusLabel.font = .systemFont(ofSize: 11)
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.lineBreakMode = .byTruncatingMiddle
        statusLabel.translatesAutoresizingMaskIntoConstraints = false

        root.addSubview(tabScroll)
        root.addSubview(pathField)
        root.addSubview(tableScroll)
        root.addSubview(statusLabel)
        NSLayoutConstraint.activate([
            tabScroll.topAnchor.constraint(equalTo: root.topAnchor, constant: 8),
            tabScroll.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 10),
            tabScroll.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -10),
            tabScroll.heightAnchor.constraint(equalToConstant: 38),
            tabButtons.leadingAnchor.constraint(equalTo: tabScroll.contentView.leadingAnchor),
            tabButtons.centerYAnchor.constraint(equalTo: tabScroll.contentView.centerYAnchor),
            tabButtons.heightAnchor.constraint(equalToConstant: 30),

            pathField.topAnchor.constraint(equalTo: tabScroll.bottomAnchor, constant: 6),
            pathField.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 10),
            pathField.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -10),
            pathField.heightAnchor.constraint(equalToConstant: 28),

            tableScroll.topAnchor.constraint(equalTo: pathField.bottomAnchor, constant: 8),
            tableScroll.leadingAnchor.constraint(equalTo: root.leadingAnchor),
            tableScroll.trailingAnchor.constraint(equalTo: root.trailingAnchor),
            tableScroll.bottomAnchor.constraint(equalTo: statusLabel.topAnchor, constant: -6),

            statusLabel.leadingAnchor.constraint(equalTo: root.leadingAnchor, constant: 10),
            statusLabel.trailingAnchor.constraint(equalTo: root.trailingAnchor, constant: -10),
            statusLabel.bottomAnchor.constraint(equalTo: root.bottomAnchor, constant: -7),
            statusLabel.heightAnchor.constraint(equalToConstant: 16),
        ])
    }

    private func addColumn(_ identifier: String, title: String, width: CGFloat) {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(identifier))
        column.title = title
        column.width = width
        column.minWidth = identifier == "name" ? 180 : 80
        tableView.addTableColumn(column)
    }

    func showAndFocus(origin: String? = nil) {
        guard let window else { return }
        var showFields: [String: Any] = [:]
        if let origin { showFields["origin"] = origin }
        logger.record("show_requested", fields: showFields)
        NSApp.activate(ignoringOtherApps: true)
        window.center()
        window.makeKeyAndOrderFront(nil)
        window.orderFrontRegardless()
        window.makeFirstResponder(pathField)
        pathField.selectText(nil)
        DispatchQueue.main.async { [weak self, weak window] in
            guard let self, let window else { return }
            window.makeFirstResponder(self.pathField)
            self.pathField.selectText(nil)
            var fields: [String: Any] = [
                "path_input_focused": window.firstResponder === self.pathField.currentEditor(),
                "window_key": window.isKeyWindow,
            ]
            if let origin { fields["origin"] = origin }
            self.logger.record("window_visible", fields: fields)
        }
    }

    func hideWindow() {
        quickLook.close()
        window?.orderOut(nil)
        logger.record("window_hidden")
    }

    func toggleQuickLook() {
        guard let item = selectedItem, let url = item.url, !item.isDirectory else { return }
        logger.record("quick_look_requested", fields: ["path": url.path])
        quickLook.toggle(url: url)
    }

    func closeQuickLook() { quickLook.close() }

    func newTab() {
        tabs.append(BrowserTab())
        activeTabIndex = tabs.count - 1
        rebuildTabs()
        renderActiveTab()
        loadRecents(into: activeTab)
        focusPathInput()
    }

    func closeActiveTab() {
        if tabs.count == 1 {
            let tab = activeTab
            tab.history.removeAll()
            tab.target = .recents
            tab.selectedRow = -1
            loadRecents(into: tab)
        } else {
            tabs.remove(at: activeTabIndex)
            activeTabIndex = min(activeTabIndex, tabs.count - 1)
            rebuildTabs()
            renderActiveTab()
        }
    }

    func refresh() {
        switch activeTab.target {
        case .recents: loadRecents(into: activeTab)
        case .directory(let url): loadDirectory(url, into: activeTab, selectionPath: nil, pushHistory: false)
        case .benchmark: showBenchmark(in: activeTab)
        }
    }

    func showBenchmark() { showBenchmark(in: activeTab) }

    private func focusPathInput() {
        window?.makeFirstResponder(pathField)
        pathField.selectText(nil)
    }

    @objc private func pathSubmitted() {
        let value = pathField.stringValue
        logger.record("path_submitted", fields: ["input": value])
        if value.lowercased() == "benchmark:10k" {
            showBenchmark(in: activeTab)
            return
        }
        if value.lowercased() == "recents" {
            navigate(to: .recents)
            return
        }
        guard let url = PathResolver.resolve(value) else {
            activeTab.error = "Enter an absolute path, ~/path, Recents, or benchmark:10k"
            updateStatus()
            return
        }
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDirectory) else {
            activeTab.error = "Path does not exist"
            updateStatus()
            return
        }
        if isDirectory.boolValue {
            navigate(to: .directory(url))
        } else {
            loadDirectory(url.deletingLastPathComponent(), into: activeTab, selectionPath: url.path, pushHistory: true)
        }
    }

    private func navigate(to target: BrowserTarget) {
        let tab = activeTab
        if tab.target != target { tab.history.append(tab.target) }
        switch target {
        case .recents:
            tab.target = .recents
            loadRecents(into: tab)
        case .directory(let url):
            loadDirectory(url, into: tab, selectionPath: nil, pushHistory: false)
        case .benchmark:
            showBenchmark(in: tab)
        }
    }

    func goBack() {
        let tab = activeTab
        guard let previous = tab.history.popLast() else { return }
        switch previous {
        case .recents:
            tab.target = .recents
            loadRecents(into: tab)
        case .directory(let url):
            loadDirectory(url, into: tab, selectionPath: nil, pushHistory: false)
        case .benchmark:
            showBenchmark(in: tab)
        }
    }

    private func loadDirectory(_ url: URL, into tab: BrowserTab, selectionPath: String?, pushHistory: Bool) {
        if pushHistory, tab.target != .directory(url) { tab.history.append(tab.target) }
        tab.target = .directory(url)
        tab.loading = true
        tab.error = nil
        if tab === activeTab { renderActiveTab() }
        provider.loadDirectory(url) { [weak self, weak tab] result, duration in
            guard let self, let tab, tab.target == .directory(url) else { return }
            tab.loading = false
            switch result {
            case .success(let items):
                tab.items = items
                tab.selectedRow = selectionPath.flatMap { path in items.firstIndex { $0.url?.path == path } } ?? (items.isEmpty ? -1 : 0)
                self.logger.record("directory_loaded", fields: [
                    "duration_ms": duration, "item_count": items.count, "path": url.path,
                ])
            case .failure(let error):
                tab.items = []
                tab.selectedRow = -1
                tab.error = error.localizedDescription
                self.logger.record("directory_loaded", fields: [
                    "duration_ms": duration, "item_count": 0, "path": url.path,
                    "error": error.localizedDescription,
                ])
            }
            if tab === self.activeTab { self.rebuildTabs(); self.renderActiveTab() }
        }
    }

    private func loadRecents(into tab: BrowserTab) {
        tab.target = .recents
        tab.loading = true
        tab.error = nil
        if tab === activeTab { renderActiveTab() }
        provider.loadRecents { [weak self, weak tab] items, duration in
            guard let self, let tab, tab.target == .recents else { return }
            tab.items = items
            tab.loading = false
            tab.selectedRow = items.isEmpty ? -1 : 0
            self.logger.record("recents_loaded", fields: [
                "duration_ms": duration, "item_count": items.count,
            ])
            if tab === self.activeTab { self.rebuildTabs(); self.renderActiveTab() }
        }
    }

    private func showBenchmark(in tab: BrowserTab) {
        tab.target = .benchmark
        tab.loading = false
        tab.error = nil
        tab.items = BrowserItem.syntheticItems()
        tab.selectedRow = 0
        if tab === activeTab {
            rebuildTabs()
            renderActiveTab()
            DispatchQueue.main.async { [weak self] in
                self?.logger.record("benchmark_10k_rendered", fields: ["item_count": 10_000])
            }
        }
    }

    private func renderActiveTab() {
        pathField.stringValue = activeTab.target.pathText
        tableView.reloadData()
        if activeTab.selectedRow >= 0, activeTab.selectedRow < activeTab.items.count {
            tableView.selectRowIndexes(IndexSet(integer: activeTab.selectedRow), byExtendingSelection: false)
            tableView.scrollRowToVisible(activeTab.selectedRow)
        } else {
            tableView.deselectAll(nil)
        }
        updateStatus()
    }

    private func updateStatus() {
        if let error = activeTab.error {
            statusLabel.stringValue = "\(activeTab.target.pathText)  •  \(error)"
            statusLabel.textColor = .systemRed
        } else {
            let state = activeTab.loading ? "Loading…" : "\(activeTab.items.count) items"
            statusLabel.stringValue = "\(activeTab.target.pathText)  •  \(state)"
            statusLabel.textColor = .secondaryLabelColor
        }
    }

    private func rebuildTabs() {
        tabButtons.arrangedSubviews.forEach { view in
            tabButtons.removeArrangedSubview(view)
            view.removeFromSuperview()
        }
        for (index, tab) in tabs.enumerated() {
            let select = NSButton(title: tab.target.displayName, target: self, action: #selector(selectTab(_:)))
            select.tag = index
            select.bezelStyle = index == activeTabIndex ? .rounded : .recessed
            select.setContentHuggingPriority(.defaultHigh, for: .horizontal)
            let close = NSButton(title: "×", target: self, action: #selector(closeTabButton(_:)))
            close.tag = index
            close.bezelStyle = .inline
            close.toolTip = "Close Tab"
            let pair = NSStackView(views: [select, close])
            pair.orientation = .horizontal
            pair.spacing = 1
            tabButtons.addArrangedSubview(pair)
        }
        let add = NSButton(title: "+", target: self, action: #selector(addTabButton))
        add.bezelStyle = .texturedRounded
        add.toolTip = "New Tab"
        tabButtons.addArrangedSubview(add)
    }

    @objc private func selectTab(_ sender: NSButton) {
        guard tabs.indices.contains(sender.tag) else { return }
        activeTab.selectedRow = tableView.selectedRow
        activeTabIndex = sender.tag
        rebuildTabs()
        renderActiveTab()
    }

    @objc private func closeTabButton(_ sender: NSButton) {
        guard tabs.indices.contains(sender.tag) else { return }
        activeTabIndex = sender.tag
        closeActiveTab()
    }

    @objc private func addTabButton() { newTab() }

    func numberOfRows(in tableView: NSTableView) -> Int { activeTab.items.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard activeTab.items.indices.contains(row), let tableColumn else { return nil }
        let identifier = tableColumn.identifier
        let cell = tableView.makeView(withIdentifier: identifier, owner: self) as? NSTableCellView ?? makeCell(identifier: identifier)
        let item = activeTab.items[row]
        switch identifier.rawValue {
        case "name": cell.textField?.stringValue = item.name
        case "kind": cell.textField?.stringValue = item.kind
        case "modified": cell.textField?.stringValue = item.modified.map(dateFormatter.string) ?? ""
        case "size": cell.textField?.stringValue = item.size.map(byteFormatter.string) ?? ""
        default: cell.textField?.stringValue = ""
        }
        cell.toolTip = item.url?.path
        return cell
    }

    private func makeCell(identifier: NSUserInterfaceItemIdentifier) -> NSTableCellView {
        let cell = NSTableCellView()
        cell.identifier = identifier
        let label = NSTextField(labelWithString: "")
        label.lineBreakMode = .byTruncatingMiddle
        label.translatesAutoresizingMaskIntoConstraints = false
        cell.textField = label
        cell.addSubview(label)
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 5),
            label.trailingAnchor.constraint(equalTo: cell.trailingAnchor, constant: -5),
            label.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        activeTab.selectedRow = tableView.selectedRow
    }

    @objc private func tableClicked() {
        activeTab.selectedRow = tableView.clickedRow >= 0 ? tableView.clickedRow : tableView.selectedRow
    }

    private var selectedItem: BrowserItem? {
        let row = tableView.selectedRow
        guard activeTab.items.indices.contains(row) else { return nil }
        return activeTab.items[row]
    }

    @objc func performPrimaryAction() {
        guard let item = selectedItem, let url = item.url else { return }
        if item.isDirectory {
            navigate(to: .directory(url))
        } else {
            NSWorkspace.shared.open(url)
        }
    }

    @objc private func copySelectedPath() {
        guard let path = selectedItem?.url?.path else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(path, forType: .string)
    }

    @objc private func quickLookSelected() { toggleQuickLook() }

    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        let available = selectedItem?.url != nil
        let open = NSMenuItem(title: "Open", action: #selector(performPrimaryAction), keyEquivalent: "")
        open.target = self
        open.isEnabled = available
        menu.addItem(open)
        let preview = NSMenuItem(title: "Quick Look", action: #selector(quickLookSelected), keyEquivalent: "")
        preview.target = self
        preview.isEnabled = available && selectedItem?.isDirectory == false
        menu.addItem(preview)
        let copy = NSMenuItem(title: "Copy Path", action: #selector(copySelectedPath), keyEquivalent: "")
        copy.target = self
        copy.isEnabled = available
        menu.addItem(copy)
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        hideWindow()
        return false
    }
}
