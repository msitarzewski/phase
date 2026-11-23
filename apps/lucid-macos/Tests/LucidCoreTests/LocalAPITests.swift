// SPDX-License-Identifier: AGPL-3.0-or-later
import XCTest
@testable import LucidCore

final class LocalAPITests: XCTestCase {
    func testEndpointAlwaysUsesLoopbackAndRejectsPrivilegedOrInvalidPorts() throws {
        XCTAssertEqual(try LocalEndpoint(port: 11435).url.absoluteString, "http://127.0.0.1:11435")
        XCTAssertEqual(try LocalEndpoint(port: 11435).url(path: "api/tags").absoluteString, "http://127.0.0.1:11435/api/tags")
        for invalid in [-1, 0, 80, 1023, 65536] {
            XCTAssertThrowsError(try LocalEndpoint(port: invalid))
        }
    }

    func testPrivateNetworkProfileIsBoundedAndNeverEnablesHostListeners() throws {
        let peer = "/ip4/127.0.0.1/tcp/14001/p2p/12D3KooWJwbyF4d4so1sqd1qsSF24dTDtc3DduLYX7v9VruBfpH7"
        let encoded = String(data: try JSONEncoder().encode([peer]), encoding: .utf8) ?? ""
        let profile = try PrivateNetworkProfile(environment: ["LUCID_RELAY_PEERS": encoded, "LUCID_APP_STATE_DIR": "/private/tmp/lucid-fixture", "LUCID_CHAT_URL": "http://127.0.0.1:13000"])
        XCTAssertEqual(profile.relayArguments, ["--no-default-bootstrap", "--no-bootstrap-relays", "--relay-peer", peer])
        XCTAssertEqual(profile.stateDirectory?.path, "/private/tmp/lucid-fixture")
        XCTAssertEqual(profile.chatAppURL, "http://127.0.0.1:13000")
        XCTAssertEqual(try PrivateNetworkProfile(environment: [:]).relayArguments, [])
        XCTAssertEqual(try PrivateNetworkProfile(environment: ["LUCID_RELAY_PEERS": "[]"]).relayArguments, ["--no-default-bootstrap", "--no-bootstrap-relays"])
        for environment in [
            ["LUCID_RELAY_PEERS": "not-json"],
            ["LUCID_RELAY_PEERS": "[\"/ip4/127.0.0.1/tcp/1\"]"],
            ["LUCID_RELAY_PEERS": "[\"" + String(repeating: "x", count: 513) + "\"]"],
            ["LUCID_APP_STATE_DIR": "relative/path"],
            ["LUCID_APP_STATE_DIR": "/"],
            ["LUCID_CHAT_URL": "http://remote.example.com"]
        ] { XCTAssertThrowsError(try PrivateNetworkProfile(environment: environment)) }
        let tooMany = String(data: try JSONEncoder().encode(Array(repeating: peer, count: 5)), encoding: .utf8) ?? ""
        XCTAssertThrowsError(try PrivateNetworkProfile(environment: ["LUCID_RELAY_PEERS": tooMany]))
    }

    func testCatalogDecodesRemoteModelsWithoutInventingSizeOrHardwareMetadata() throws {
        let data = Data("""
        {"models":[{"name":"gemma-network","model":"gemma-network","digest":"sha256:abcd"}]}
        """.utf8)
        let catalog = try JSONDecoder().decode(Catalog.self, from: data)
        XCTAssertEqual(catalog.models.count, 1)
        XCTAssertNil(catalog.models[0].size)
        XCTAssertEqual(catalog.models[0].metadata, [])
        let empty = try JSONDecoder().decode(Catalog.self, from: Data("{\"models\":[]}".utf8))
        XCTAssertTrue(empty.models.isEmpty)
    }

    func testRemoteModelUsesExactDispatchIDAndHonorsAccessState() throws {
        let cid = String(repeating: "b", count: 64)
        let data = Data("{\"models\":[{\"name\":\"gemma\",\"model\":\"\(cid)\",\"x_lucid_access\":\"access_not_confirmed\"}]}".utf8)
        let model = try XCTUnwrap(JSONDecoder().decode(Catalog.self, from: data).models.first)
        XCTAssertEqual(model.name, "gemma")
        XCTAssertEqual(model.id, cid)
        XCTAssertFalse(model.canRequest)
        XCTAssertTrue(CatalogModel(name: "local").canRequest)
        XCTAssertTrue(CatalogModel(name: "remote", model: cid, access: "authorized").canRequest)
    }

    func testChatDeepLinkEncodesModelAndReplacesExistingSelectionWithoutSendingAPrompt() throws {
        let url = try ChatAppLink.make(base: "http://localhost:3000/?models=old&temporary-chat=true", model: "gemma&web-search=true")
        let parts = try XCTUnwrap(URLComponents(url: url, resolvingAgainstBaseURL: false))
        let items = try XCTUnwrap(parts.queryItems)
        XCTAssertEqual(items.filter { $0.name == "model" }.map(\.value), ["gemma&web-search=true"])
        XCTAssertFalse(items.contains { ["q", "web-search", "models"].contains($0.name) })
        XCTAssertEqual(items.first { $0.name == "temporary-chat" }?.value, "true")
    }

    func testChatAppRejectsCredentialsAndNonLocalCleartextURLs() throws {
        for invalid in ["javascript:alert(1)", "file:///etc/passwd", "http://example.com", "https://secret@example.com", "https://user:password@example.com"] {
            XCTAssertThrowsError(try ChatAppLink.make(base: invalid))
        }
        XCTAssertNoThrow(try ChatAppLink.make(base: "https://chat.example.com"))
        XCTAssertNoThrow(try ChatAppLink.make(base: "http://[::1]:3000"))
        XCTAssertThrowsError(try ChatAppLink.make(base: "http://localhost:3000", model: "model\nheader"))
    }

    func testInvitationsAreBoundedAndOpaque() throws {
        XCTAssertEqual(try Invitation.validate("  lucid://join/opaque-signed-payload\n"), "lucid://join/opaque-signed-payload")
        XCTAssertThrowsError(try Invitation.validate("\n  "))
        XCTAssertThrowsError(try Invitation.validate("hello\0world"))
        XCTAssertThrowsError(try Invitation.validate(String(repeating: "x", count: Invitation.maximumBytes + 1)))
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("network.lucidinvite")
        try Data(repeating: 65, count: Invitation.maximumBytes + 1).write(to: file)
        XCTAssertThrowsError(try Invitation.read(from: file))
    }

    func testControlTokenRequiresAnOwnedPrivateRegularFile() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let token = directory.appendingPathComponent("control.token")
        let value = String(repeating: "a", count: 64)
        try (value + "\n").write(to: token, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: token.path)
        XCTAssertEqual(try ControlToken.read(from: token), value)
        try FileManager.default.setAttributes([.posixPermissions: 0o644], ofItemAtPath: token.path)
        XCTAssertThrowsError(try ControlToken.read(from: token))
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: token.path)
        let link = directory.appendingPathComponent("link.token")
        try FileManager.default.createSymbolicLink(at: link, withDestinationURL: token)
        XCTAssertThrowsError(try ControlToken.read(from: link))
        try "invalid".write(to: token, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: token.path)
        XCTAssertThrowsError(try ControlToken.read(from: token))
    }
}
