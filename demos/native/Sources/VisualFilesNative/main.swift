import AppKit
import Foundation

if CommandLine.arguments.contains("--smoke") {
    let home = "/Users/smoke"
    guard PathResolver.resolve("~/Downloads", home: home)?.path == "/Users/smoke/Downloads" else {
        fputs("path expansion failed\n", stderr)
        exit(1)
    }
    let items = BrowserItem.syntheticItems()
    guard items.count == 10_000,
          items.first?.name == "Benchmark Item 00000.txt",
          items.last?.name == "Benchmark Item 09999.txt" else {
        fputs("benchmark fixture failed\n", stderr)
        exit(1)
    }
    print("smoke checks passed")
    exit(0)
}

let application = NSApplication.shared
let delegate = AppDelegate()
application.delegate = delegate
application.run()
