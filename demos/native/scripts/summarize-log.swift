import Foundation

let defaultURL = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first!
    .appendingPathComponent("VisualFilesAppKit/events.ndjson")
let logURL = CommandLine.arguments.dropFirst().first.map { URL(fileURLWithPath: $0) } ?? defaultURL

guard let text = try? String(contentsOf: logURL, encoding: .utf8) else {
    fputs("No log at \(logURL.path)\n", stderr)
    exit(1)
}

let events: [[String: Any]] = text.split(separator: "\n").compactMap { line in
    guard let data = String(line).data(using: .utf8) else { return nil }
    return try? JSONSerialization.jsonObject(with: data) as? [String: Any]
}

func percentile(_ values: [Double], _ fraction: Double) -> Double? {
    guard !values.isEmpty else { return nil }
    let sorted = values.sorted()
    let index = max(0, min(sorted.count - 1, Int(ceil(Double(sorted.count) * fraction)) - 1))
    return sorted[index]
}

func report(_ name: String, values: [Double]) {
    guard let median = percentile(values, 0.5), let p95 = percentile(values, 0.95), let slowest = values.max() else {
        print("\(name): no samples")
        return
    }
    print(String(format: "%@: n=%d median=%.3f ms p95=%.3f ms slowest=%.3f ms", name, values.count, median, p95, slowest))
}

var processStarts: [Int: Double] = [:]
var shortcutStarts: [Int: Double] = [:]
var coldReady: [Double] = []
var warmShow: [Double] = []
var directoryLoads: [Double] = []
var recentsLoads: [Double] = []
var benchmarkRenders = 0

for event in events {
    guard let name = event["event"] as? String,
          let pid = (event["pid"] as? NSNumber)?.intValue,
          let ns = (event["monotonic_ns"] as? NSNumber)?.doubleValue else { continue }
    switch name {
    case "process_start": processStarts[pid] = ns
    case "frontend_ready":
        if let start = processStarts[pid] { coldReady.append((ns - start) / 1_000_000) }
    case "shortcut_received": shortcutStarts[pid] = ns
    case "window_visible":
        if let start = shortcutStarts.removeValue(forKey: pid) { warmShow.append((ns - start) / 1_000_000) }
    case "window_hidden": shortcutStarts.removeValue(forKey: pid)
    case "directory_loaded":
        if let value = (event["duration_ms"] as? NSNumber)?.doubleValue { directoryLoads.append(value) }
    case "recents_loaded":
        if let value = (event["duration_ms"] as? NSNumber)?.doubleValue { recentsLoads.append(value) }
    case "benchmark_10k_rendered": benchmarkRenders += 1
    default: break
    }
}

print("Log: \(logURL.path)")
report("cold hidden-ready", values: coldReady)
report("warm shortcut-visible", values: warmShow)
report("directory load", values: directoryLoads)
report("Recents load", values: recentsLoads)
print("Benchmark 10k render events: \(benchmarkRenders)")
