import Foundation

final class EventLogger {
    static let shared = EventLogger()

    let logURL: URL
    private let lock = NSLock()

    init(baseDirectory: URL? = nil) {
        let environmentDirectory = ProcessInfo.processInfo.environment["VISUAL_FILES_LOG_DIR"]
            .map { URL(fileURLWithPath: $0, isDirectory: true) }
        let directory = baseDirectory ?? environmentDirectory ?? FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first!.appendingPathComponent("VisualFilesAppKit", isDirectory: true)
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        logURL = directory.appendingPathComponent("events.ndjson")
        if !FileManager.default.fileExists(atPath: logURL.path) {
            FileManager.default.createFile(atPath: logURL.path, contents: nil)
        }
    }

    func record(_ event: String, fields: [String: Any] = [:]) {
        var payload = fields
        payload["event"] = event
        payload["monotonic_ns"] = UInt64(ProcessInfo.processInfo.systemUptime * 1_000_000_000)
        payload["timestamp"] = ISO8601DateFormatter().string(from: Date())
        payload["pid"] = ProcessInfo.processInfo.processIdentifier
        guard JSONSerialization.isValidJSONObject(payload),
              let data = try? JSONSerialization.data(withJSONObject: payload, options: [.sortedKeys]),
              let newline = "\n".data(using: .utf8) else { return }

        lock.lock()
        defer { lock.unlock() }
        do {
            let handle = try FileHandle(forWritingTo: logURL)
            try handle.seekToEnd()
            try handle.write(contentsOf: data)
            try handle.write(contentsOf: newline)
            try handle.synchronize()
            try handle.close()
        } catch {
            fputs("event log error: \(error)\n", stderr)
        }
    }
}
