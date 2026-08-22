import Carbon
import Foundation

private func hotKeyEventHandler(
    _ nextHandler: EventHandlerCallRef?,
    _ event: EventRef?,
    _ userData: UnsafeMutableRawPointer?
) -> OSStatus {
    guard let event, let userData else { return OSStatus(eventNotHandledErr) }
    var hotKeyID = EventHotKeyID()
    let status = GetEventParameter(
        event,
        EventParamName(kEventParamDirectObject),
        EventParamType(typeEventHotKeyID),
        nil,
        MemoryLayout<EventHotKeyID>.size,
        nil,
        &hotKeyID
    )
    guard status == noErr, hotKeyID.id == 1 else { return OSStatus(eventNotHandledErr) }
    let owner = Unmanaged<GlobalHotKey>.fromOpaque(userData).takeUnretainedValue()
    owner.receive()
    return noErr
}

final class GlobalHotKey {
    struct Configuration {
        let keyCode: UInt32
        let modifiers: UInt32

        static func fromEnvironment(_ environment: [String: String] = ProcessInfo.processInfo.environment) -> Configuration {
            let keyCode = UInt32(environment["VISUAL_FILES_SHORTCUT_KEYCODE"] ?? "") ?? UInt32(kVK_ANSI_F)
            let modifierNames = environment["VISUAL_FILES_SHORTCUT_MODIFIERS"] ?? "command,shift"
            let modifiers = modifierNames.split(separator: ",").reduce(UInt32(0)) { result, rawName in
                switch rawName.trimmingCharacters(in: .whitespaces).lowercased() {
                case "command": return result | UInt32(cmdKey)
                case "shift": return result | UInt32(shiftKey)
                case "option": return result | UInt32(optionKey)
                case "control": return result | UInt32(controlKey)
                default: return result
                }
            }
            return Configuration(keyCode: keyCode, modifiers: modifiers)
        }
    }

    private var hotKeyRef: EventHotKeyRef?
    private var eventHandlerRef: EventHandlerRef?
    private let handler: () -> Void

    init(configuration: Configuration = .fromEnvironment(), handler: @escaping () -> Void) throws {
        self.handler = handler
        var eventType = EventTypeSpec(
            eventClass: OSType(kEventClassKeyboard),
            eventKind: UInt32(kEventHotKeyPressed)
        )
        let userData = Unmanaged.passUnretained(self).toOpaque()
        let installStatus = InstallEventHandler(
            GetApplicationEventTarget(),
            hotKeyEventHandler,
            1,
            &eventType,
            userData,
            &eventHandlerRef
        )
        guard installStatus == noErr else {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(installStatus))
        }
        let identifier = EventHotKeyID(signature: OSType(0x56464E54), id: 1)
        let registerStatus = RegisterEventHotKey(
            configuration.keyCode,
            configuration.modifiers,
            identifier,
            GetApplicationEventTarget(),
            0,
            &hotKeyRef
        )
        guard registerStatus == noErr else {
            if let eventHandlerRef { RemoveEventHandler(eventHandlerRef) }
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(registerStatus))
        }
    }

    func receive() {
        DispatchQueue.main.async { [handler] in handler() }
    }

    deinit {
        if let hotKeyRef { UnregisterEventHotKey(hotKeyRef) }
        if let eventHandlerRef { RemoveEventHandler(eventHandlerRef) }
    }
}
