// swift-tools-version: 5.9
// SPDX-License-Identifier: AGPL-3.0-or-later
import PackageDescription

let package = Package(
    name: "Lucid",
    platforms: [.macOS(.v14)],
    products: [.executable(name: "Lucid", targets: ["LucidApp"])],
    targets: [
        .target(name: "LucidCore"),
        .executableTarget(name: "LucidApp", dependencies: ["LucidCore"]),
        .testTarget(name: "LucidCoreTests", dependencies: ["LucidCore"])
    ]
)
