// SPDX-License-Identifier: AGPL-3.0-or-later
import Darwin
import Foundation
import XCTest
@testable import LucidCore

final class LocalHTTPContractTests: XCTestCase {
    func testEnrollmentSendsPrivateBearerAndAcceptsPending202() async throws {
        let server = try OneShotHTTPServer(status: 202, headers: [:], body: "{\"state\":\"pending\",\"message\":\"Joining\"}")
        XCTAssertThrowsError(try LocalEndpoint(port: server.port).requireAvailablePort())
        let token = try privateToken()
        defer { try? FileManager.default.removeItem(at: token.deletingLastPathComponent()) }
        let client = LocalAPI(endpoint: try LocalEndpoint(port: server.port), tokenFile: token)
        let result = try await client.enroll(invitation: "lucid://join/test-only-fixture")
        XCTAssertEqual(result.state, "pending")
        let request = server.receivedRequest.lowercased()
        XCTAssertTrue(request.hasPrefix("post /api/lucid/enroll http/1.1"))
        XCTAssertTrue(request.contains("authorization: bearer " + String(repeating: "a", count: 64)))
        XCTAssertTrue(request.contains("content-type: application/json"))
        XCTAssertTrue(request.contains("test-only-fixture"))
    }

    func testLocalControlRefusesRedirectBeforeSendingTokenToAnotherEndpoint() async throws {
        let server = try OneShotHTTPServer(status: 302, headers: ["Location": "http://127.0.0.1:9/another-endpoint"], body: "")
        let token = try privateToken()
        defer { try? FileManager.default.removeItem(at: token.deletingLastPathComponent()) }
        let client = LocalAPI(endpoint: try LocalEndpoint(port: server.port), tokenFile: token)
        do {
            _ = try await client.status()
            XCTFail("A local control redirect must be rejected.")
        } catch let error as LocalAPIError {
            XCTAssertEqual(error, .httpStatus(302))
        }
    }

    func testCatalogRefusesOversizedDeclaredResponse() async throws {
        let server = try OneShotHTTPServer(status: 200, headers: ["Content-Length": "2097152"], body: "{}")
        let token = try privateToken()
        defer { try? FileManager.default.removeItem(at: token.deletingLastPathComponent()) }
        let client = LocalAPI(endpoint: try LocalEndpoint(port: server.port), tokenFile: token)
        do {
            _ = try await client.catalog()
            XCTFail("Oversized API responses must be rejected.")
        } catch let error as LocalAPIError {
            XCTAssertEqual(error, .responseTooLarge)
        }
        XCTAssertFalse(server.receivedRequest.lowercased().contains("authorization:"))
    }

    private func privateToken() throws -> URL {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        let token = directory.appendingPathComponent("control.token")
        try String(repeating: "a", count: 64).write(to: token, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: token.path)
        return token
    }
}

/// A bounded, one-request HTTP fixture. It binds only loopback, closes all
/// descriptors, and exists exclusively in the test target.
private final class OneShotHTTPServer {
    let port: Int
    private let descriptor: Int32
    private let lock = NSLock()
    private var request = ""

    var receivedRequest: String {
        lock.lock(); defer { lock.unlock() }
        return request
    }

    init(status: Int, headers: [String: String], body: String) throws {
        let listener = Darwin.socket(AF_INET, SOCK_STREAM, 0)
        descriptor = listener
        guard listener >= 0 else { throw POSIXError(.EIO) }
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_addr = in_addr(s_addr: inet_addr("127.0.0.1"))
        let bound = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(listener, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard bound == 0, Darwin.listen(listener, 1) == 0 else {
            Darwin.close(listener); throw POSIXError(.EADDRINUSE)
        }
        var length = socklen_t(MemoryLayout<sockaddr_in>.size)
        let measured = withUnsafeMutablePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.getsockname(listener, $0, &length)
            }
        }
        guard measured == 0 else { Darwin.close(listener); throw POSIXError(.EIO) }
        port = Int(UInt16(bigEndian: address.sin_port))
        var values = headers
        values["Content-Type"] = "application/json"
        values["Connection"] = "close"
        if values["Content-Length"] == nil { values["Content-Length"] = String(body.utf8.count) }
        let headerLines = values.sorted { $0.key < $1.key }.map { "\($0.key): \($0.value)" }.joined(separator: "\r\n")
        let response = Data("HTTP/1.1 \(status) Test\r\n\(headerLines)\r\n\r\n\(body)".utf8)
        DispatchQueue.global(qos: .userInitiated).async { [self] in serve(response) }
    }

    deinit { Darwin.close(descriptor) }

    private func serve(_ response: Data) {
        var ready = pollfd(fd: descriptor, events: Int16(POLLIN), revents: 0)
        guard Darwin.poll(&ready, 1, 5000) > 0 else { return }
        let client = Darwin.accept(descriptor, nil, nil)
        guard client >= 0 else { return }
        defer { Darwin.close(client) }
        var timeout = timeval(tv_sec: 5, tv_usec: 0)
        _ = setsockopt(client, SOL_SOCKET, SO_RCVTIMEO, &timeout, socklen_t(MemoryLayout<timeval>.size))
        var noSignal: Int32 = 1
        _ = setsockopt(client, SOL_SOCKET, SO_NOSIGPIPE, &noSignal, socklen_t(MemoryLayout<Int32>.size))
        var bytes = Data()
        var buffer = [UInt8](repeating: 0, count: 4096)
        while bytes.count < 16 * 1024 {
            let count = Darwin.recv(client, &buffer, buffer.count, 0)
            if count <= 0 { return }
            bytes.append(contentsOf: buffer.prefix(count))
            if let boundary = bytes.range(of: Data("\r\n\r\n".utf8)),
               let text = String(data: bytes[..<boundary.lowerBound], encoding: .utf8) {
                let contentLength = text.components(separatedBy: "\r\n").first { $0.lowercased().hasPrefix("content-length:") }
                    .flatMap { Int($0.split(separator: ":", maxSplits: 1).last?.trimmingCharacters(in: .whitespacesAndNewlines) ?? "") } ?? 0
                if bytes.count >= boundary.upperBound + contentLength { break }
            }
        }
        lock.lock(); request = String(data: bytes, encoding: .utf8) ?? ""; lock.unlock()
        response.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return }
            var sent = 0
            while sent < raw.count {
                let count = Darwin.send(client, base.advanced(by: sent), raw.count - sent, 0)
                if count <= 0 { return }
                sent += count
            }
        }
    }
}
