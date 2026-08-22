// swift-tools-version: 6.0

import PackageDescription

let package = Package(
    name: "VisualFilesNative",
    platforms: [.macOS(.v14)],
    products: [
        .executable(name: "VisualFilesNative", targets: ["VisualFilesNative"]),
    ],
    targets: [
        .executableTarget(
            name: "VisualFilesNative",
            linkerSettings: [
                .linkedFramework("AppKit"),
                .linkedFramework("Carbon"),
                .linkedFramework("Quartz"),
            ]
        ),
    ],
    swiftLanguageModes: [.v5]
)
