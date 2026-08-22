import AppKit
import Darwin

final class AppDelegate: NSObject, NSApplicationDelegate {
    private let logger = EventLogger.shared
    private var mainWindowController: MainWindowController?
    private var hotKey: GlobalHotKey?
    private var localKeyMonitor: Any?

    override init() {
        super.init()
        logger.record("process_start")
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.accessory)
        let controller = MainWindowController(logger: logger)
        mainWindowController = controller
        do {
            hotKey = try GlobalHotKey { [weak self] in self?.shortcutReceived() }
            logger.record("backend_ready", fields: ["shortcut_registered": true])
        } catch {
            logger.record("backend_ready", fields: [
                "shortcut_registered": false,
                "error": error.localizedDescription,
            ])
            fputs("Could not register global shortcut: \(error)\n", stderr)
        }
        installLocalKeyMonitor()
        logger.record("frontend_ready", fields: ["window_hidden": true])
        if ProcessInfo.processInfo.environment["VISUAL_FILES_START_VISIBLE"] == "1" {
            controller.showAndFocus(origin: "test_hook")
        }
        if CommandLine.arguments.contains("--launch-smoke") {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
                Darwin.exit(0)
            }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        if let localKeyMonitor { NSEvent.removeMonitor(localKeyMonitor) }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { false }

    private func shortcutReceived() {
        logger.record("shortcut_received")
        guard let controller, let window = controller.window else { return }
        if window.isVisible {
            controller.hideWindow()
        } else {
            controller.showAndFocus()
        }
    }

    private var controller: MainWindowController? { mainWindowController }

    private func installLocalKeyMonitor() {
        localKeyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self, let controller = self.controller, controller.window?.isVisible == true else { return event }
            let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            let key = event.charactersIgnoringModifiers?.lowercased()
            if event.keyCode == 53 {
                if controller.isQuickLookVisible { controller.closeQuickLook() }
                else { controller.hideWindow() }
                return nil
            }
            guard flags.contains(.command) else { return event }
            switch key {
            case "t": controller.newTab(); return nil
            case "w": controller.closeActiveTab(); return nil
            case "r": controller.refresh(); return nil
            case "b": controller.showBenchmark(); return nil
            default: return event
            }
        }
    }
}
