import CoreServices
import Foundation

final class FileProvider {
    private final class RecentsRequest {
        let query = NSMetadataQuery()
        let startedAt = ProcessInfo.processInfo.systemUptime
        var observer: NSObjectProtocol?
    }
    private var recentsRequests: [UUID: RecentsRequest] = [:]

    deinit {
        for request in recentsRequests.values {
            if let observer = request.observer { NotificationCenter.default.removeObserver(observer) }
            request.query.stop()
        }
    }

    func loadDirectory(_ url: URL, completion: @escaping (Result<[BrowserItem], Error>, Double) -> Void) {
        let startedAt = ProcessInfo.processInfo.systemUptime
        DispatchQueue.global(qos: .userInitiated).async {
            do {
                let keys: [URLResourceKey] = [
                    .isDirectoryKey, .contentModificationDateKey, .fileSizeKey,
                    .localizedTypeDescriptionKey, .isHiddenKey,
                ]
                let urls = try FileManager.default.contentsOfDirectory(
                    at: url,
                    includingPropertiesForKeys: keys,
                    options: [.skipsHiddenFiles]
                )
                let items = try urls.map { child -> BrowserItem in
                    let values = try child.resourceValues(forKeys: Set(keys))
                    return BrowserItem(
                        url: child,
                        name: child.lastPathComponent,
                        kind: values.localizedTypeDescription ?? (values.isDirectory == true ? "Folder" : "File"),
                        modified: values.contentModificationDate,
                        size: values.isDirectory == true ? nil : values.fileSize.map(Int64.init),
                        isDirectory: values.isDirectory == true
                    )
                }.sorted {
                    if $0.isDirectory != $1.isDirectory { return $0.isDirectory }
                    return $0.name.localizedStandardCompare($1.name) == .orderedAscending
                }
                let duration = (ProcessInfo.processInfo.systemUptime - startedAt) * 1_000
                DispatchQueue.main.async { completion(.success(items), duration) }
            } catch {
                let duration = (ProcessInfo.processInfo.systemUptime - startedAt) * 1_000
                DispatchQueue.main.async { completion(.failure(error), duration) }
            }
        }
    }

    func loadRecents(limit: Int = 500, completion: @escaping ([BrowserItem], Double) -> Void) {
        let requestID = UUID()
        let request = RecentsRequest()
        let query = request.query
        recentsRequests[requestID] = request
        let cutoff = Date(timeIntervalSinceNow: -90 * 24 * 60 * 60)
        // NSMetadataQuery rejects `attribute != nil` because its Spotlight query
        // parser does not accept nil on the right-hand side. Missing dates do not
        // satisfy this comparison, so this keeps the intended recent-item filter.
        query.predicate = NSPredicate(format: "%K >= %@", kMDItemLastUsedDate as String, cutoff as NSDate)
        query.searchScopes = [NSMetadataQueryUserHomeScope]
        query.sortDescriptors = [NSSortDescriptor(key: kMDItemLastUsedDate as String, ascending: false)]

        request.observer = NotificationCenter.default.addObserver(
            forName: .NSMetadataQueryDidFinishGathering,
            object: query,
            queue: .main
        ) { [weak self, weak request] _ in
            guard let self, let request else { return }
            let query = request.query
            query.disableUpdates()
            let items = query.results.prefix(limit).compactMap { result -> BrowserItem? in
                guard let metadata = result as? NSMetadataItem,
                      let path = metadata.value(forAttribute: kMDItemPath as String) as? String else { return nil }
                let url = URL(fileURLWithPath: path)
                var isDirectory: ObjCBool = false
                guard FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory) else { return nil }
                let name = metadata.value(forAttribute: kMDItemFSName as String) as? String ?? url.lastPathComponent
                let kind = metadata.value(forAttribute: kMDItemKind as String) as? String ?? (isDirectory.boolValue ? "Folder" : "File")
                let modified = metadata.value(forAttribute: kMDItemFSContentChangeDate as String) as? Date
                let size = (metadata.value(forAttribute: kMDItemFSSize as String) as? NSNumber)?.int64Value
                return BrowserItem(url: url, name: name, kind: kind, modified: modified, size: size, isDirectory: isDirectory.boolValue)
            }
            let duration = (ProcessInfo.processInfo.systemUptime - request.startedAt) * 1_000
            query.stop()
            if let observer = request.observer { NotificationCenter.default.removeObserver(observer) }
            self.recentsRequests.removeValue(forKey: requestID)
            completion(items, duration)
        }
        if !query.start() {
            if let observer = request.observer { NotificationCenter.default.removeObserver(observer) }
            recentsRequests.removeValue(forKey: requestID)
            completion([], 0)
        }
    }
}
