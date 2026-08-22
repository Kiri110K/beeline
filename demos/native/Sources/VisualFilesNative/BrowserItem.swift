import Foundation

struct BrowserItem: Equatable {
    let url: URL?
    let name: String
    let kind: String
    let modified: Date?
    let size: Int64?
    let isDirectory: Bool

    static func syntheticItems(count: Int = 10_000) -> [BrowserItem] {
        (0..<count).map { index in
            BrowserItem(
                url: nil,
                name: String(format: "Benchmark Item %05d.txt", index),
                kind: "Plain Text",
                modified: Date(timeIntervalSince1970: 1_700_000_000 + Double(index)),
                size: Int64(512 + (index * 131) % 1_000_000),
                isDirectory: false
            )
        }
    }
}

enum BrowserTarget: Equatable {
    case recents
    case directory(URL)
    case benchmark

    var displayName: String {
        switch self {
        case .recents: return "Recents"
        case .directory(let url): return url.lastPathComponent.isEmpty ? url.path : url.lastPathComponent
        case .benchmark: return "Benchmark 10k"
        }
    }

    var pathText: String {
        switch self {
        case .recents: return "Recents"
        case .directory(let url): return url.path
        case .benchmark: return "benchmark:10k"
        }
    }
}

final class BrowserTab {
    let id = UUID()
    var target: BrowserTarget = .recents
    var history: [BrowserTarget] = []
    var items: [BrowserItem] = []
    var selectedRow = -1
    var loading = false
    var error: String?
}

enum PathResolver {
    static func resolve(_ input: String, home: String = FileManager.default.homeDirectoryForCurrentUser.path) -> URL? {
        let trimmed = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        let expanded: String
        if trimmed == "~" {
            expanded = home
        } else if trimmed.hasPrefix("~/") {
            expanded = home + String(trimmed.dropFirst())
        } else {
            expanded = trimmed
        }
        guard expanded.hasPrefix("/") else { return nil }
        return URL(fileURLWithPath: expanded).standardizedFileURL
    }
}
