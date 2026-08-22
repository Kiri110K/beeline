import AppKit

final class BrowserTableView: NSTableView {
    var onPrimaryAction: (() -> Void)?
    var onBack: (() -> Void)?
    var onQuickLook: (() -> Void)?

    override func keyDown(with event: NSEvent) {
        let key = event.charactersIgnoringModifiers?.lowercased()
        let modifiers = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        if modifiers.contains(.control), key == "j" {
            selectAndScroll(row: min(numberOfRows - 1, max(0, selectedRow + 1)))
        } else if modifiers.contains(.control), key == "k" {
            selectAndScroll(row: max(0, selectedRow - 1))
        } else {
            switch event.keyCode {
            case 124, 36, 76: onPrimaryAction?()
            case 123: onBack?()
            case 49: onQuickLook?()
            default: super.keyDown(with: event)
            }
        }
    }

    override func menu(for event: NSEvent) -> NSMenu? {
        let row = self.row(at: convert(event.locationInWindow, from: nil))
        if row >= 0 {
            selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        }
        return super.menu(for: event)
    }

    private func selectAndScroll(row: Int) {
        guard row >= 0, row < numberOfRows else { return }
        selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        scrollRowToVisible(row)
    }
}
