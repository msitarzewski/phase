// SPDX-License-Identifier: AGPL-3.0-or-later
import Foundation
import Darwin

public enum LocalAPIError: Error, Equatable, LocalizedError {
    case invalidPort
    case portInUse
    case invalidChatURL
    case invalidModel
    case invalidInvitation
    case insecureControlToken
    case responseTooLarge
    case invalidPrivateProfile
    case httpStatus(Int)

    public var errorDescription: String? {
        switch self {
        case .invalidPort: return "Choose a local API port between 1024 and 65535."
        case .portInUse: return "The local API port is already in use. Disconnect that service or choose another port in Settings."
        case .invalidChatURL: return "Use an HTTPS chat app URL, or HTTP on localhost."
        case .invalidModel: return "The selected model identifier is invalid."
        case .invalidInvitation: return "Choose a non-empty network invitation smaller than 8 KiB."
        case .insecureControlToken: return "LUCID's private control token could not be read securely."
        case .responseTooLarge: return "The local API response exceeded the supported size."
        case .invalidPrivateProfile: return "The private LUCID network profile is invalid. Check its relay list, state path, or chat app URL."
        case .httpStatus(let code): return "The local API returned HTTP \(code)."
        }
    }
}

public struct PrivateNetworkProfile: Sendable {
    public let relayPeers: [String]?
    public let stateDirectory: URL?
    public let chatAppURL: String?

    public init(environment: [String: String]) throws {
        if let raw = environment["LUCID_RELAY_PEERS"] {
            guard raw.utf8.count <= 8 * 1024, let data = raw.data(using: .utf8),
                  let peers = try? JSONDecoder().decode([String].self, from: data), peers.count <= 4
            else { throw LocalAPIError.invalidPrivateProfile }
            for peer in peers {
                guard peer.utf8.count <= 512, peer.hasPrefix("/"), !peer.contains("/p2p-circuit"),
                      peer.utf8.allSatisfy({ (33...126).contains($0) }),
                      peer.range(of: #"/p2p/[1-9A-HJ-NP-Za-km-z]{20,128}$"#, options: .regularExpression) != nil
                else { throw LocalAPIError.invalidPrivateProfile }
            }
            relayPeers = peers
        } else { relayPeers = nil }
        if let path = environment["LUCID_APP_STATE_DIR"] {
            guard path.hasPrefix("/"), path != "/", path.utf8.count <= 4096, !path.contains("\0")
            else { throw LocalAPIError.invalidPrivateProfile }
            stateDirectory = URL(fileURLWithPath: path, isDirectory: true).standardizedFileURL
        } else { stateDirectory = nil }
        if let value = environment["LUCID_CHAT_URL"] {
            guard (try? ChatAppLink.make(base: value)) != nil else { throw LocalAPIError.invalidPrivateProfile }
            chatAppURL = value
        } else { chatAppURL = nil }
    }

    public var relayArguments: [String] {
        guard let relayPeers else { return [] }
        return ["--no-default-bootstrap", "--no-bootstrap-relays"] + relayPeers.flatMap { ["--relay-peer", $0] }
    }
}

public struct LocalEndpoint: Equatable, Sendable {
    public let port: Int
    public let url: URL

    public init(port: Int) throws {
        guard (1024...65535).contains(port), let url = URL(string: "http://127.0.0.1:\(port)")
        else { throw LocalAPIError.invalidPort }
        self.port = port; self.url = url
    }

    public func url(path: String) -> URL {
        url.appendingPathComponent(path)
    }

    public func requireAvailablePort() throws {
        let descriptor = Darwin.socket(AF_INET, SOCK_STREAM, 0)
        guard descriptor >= 0 else { throw LocalAPIError.portInUse }
        defer { Darwin.close(descriptor) }
        var address = sockaddr_in()
        address.sin_len = UInt8(MemoryLayout<sockaddr_in>.size)
        address.sin_family = sa_family_t(AF_INET)
        address.sin_port = UInt16(port).bigEndian
        address.sin_addr = in_addr(s_addr: inet_addr("127.0.0.1"))
        let result = withUnsafePointer(to: &address) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.bind(descriptor, $0, socklen_t(MemoryLayout<sockaddr_in>.size))
            }
        }
        guard result == 0 else { throw LocalAPIError.portInUse }
    }
}

public struct Catalog: Decodable, Sendable {
    public let models: [CatalogModel]
}

public struct CatalogModel: Decodable, Identifiable, Hashable, Sendable {
    public let name: String
    public let model: String?
    public let digest: String?
    public let size: UInt64?
    public let details: ModelDetails?
    public let access: String?
    public var id: String { model ?? name }
    public var canRequest: Bool { access == nil || access == "authorized" }

    enum CodingKeys: String, CodingKey {
        case name, model, digest, size, details
        case access = "x_lucid_access"
    }

    public init(name: String, model: String? = nil, digest: String? = nil,
                size: UInt64? = nil, details: ModelDetails? = nil, access: String? = nil) {
        self.name = name; self.model = model; self.digest = digest
        self.size = size; self.details = details; self.access = access
    }

    public var metadata: [String] {
        [details?.family, details?.parameterSize, details?.quantizationLevel]
            .compactMap { $0 }.filter { !$0.isEmpty && $0.lowercased() != "unknown" }
    }
}

public struct ModelDetails: Decodable, Hashable, Sendable {
    public let family: String?
    public let parameterSize: String?
    public let quantizationLevel: String?

    enum CodingKeys: String, CodingKey {
        case family
        case parameterSize = "parameter_size"
        case quantizationLevel = "quantization_level"
    }
}

public struct VersionResponse: Decodable, Sendable { public let version: String }

public struct EnrollmentStatus: Decodable, Sendable {
    public let state: String
    public let message: String?

    public var title: String {
        switch state {
        case "authorized": return "Network access granted"
        case "pending": return "Joining network"
        case "failed": return "Network access needs attention"
        default: return "Join a network"
        }
    }
}

public struct NodeStatus: Decodable, Sendable {
    public let state: String
    public let modelCount: Int
    public let enrollment: EnrollmentStatus

    enum CodingKeys: String, CodingKey {
        case state, enrollment
        case modelCount = "model_count"
    }
}

public enum Invitation {
    public static let maximumBytes = 8 * 1024

    public static func validate(_ input: String) throws -> String {
        let value = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty, value.utf8.count <= maximumBytes,
              !value.contains("\0") else { throw LocalAPIError.invalidInvitation }
        return value
    }

    public static func read(from url: URL) throws -> String {
        guard url.isFileURL else { throw LocalAPIError.invalidInvitation }
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        let data = try handle.read(upToCount: maximumBytes + 1) ?? Data()
        guard data.count <= maximumBytes, let value = String(data: data, encoding: .utf8)
        else { throw LocalAPIError.invalidInvitation }
        return try validate(value)
    }
}

public enum ChatAppLink {
    public static func make(base input: String, model: String? = nil) throws -> URL {
        guard var components = URLComponents(string: input),
              let scheme = components.scheme?.lowercased(),
              let host = components.host?.lowercased(), !host.isEmpty,
              components.user == nil, components.password == nil,
              scheme == "https" || (scheme == "http" && ["localhost", "127.0.0.1", "::1", "[::1]"].contains(host))
        else { throw LocalAPIError.invalidChatURL }
        if let model {
            guard !model.isEmpty, model.utf8.count <= 256,
                  model.unicodeScalars.allSatisfy({ !CharacterSet.controlCharacters.contains($0) })
            else { throw LocalAPIError.invalidModel }
            var items = components.queryItems ?? []
            items.removeAll { $0.name == "model" || $0.name == "models" }
            // Open WebUI's supported deep link selects the model without submitting a prompt.
            items.append(URLQueryItem(name: "model", value: model))
            components.queryItems = items
        }
        guard let url = components.url else { throw LocalAPIError.invalidChatURL }
        return url
    }
}

public enum ControlToken {
    public static func read(from url: URL) throws -> String {
        let values = try FileManager.default.attributesOfItem(atPath: url.path)
        guard values[.type] as? FileAttributeType == .typeRegular,
              let permissions = values[.posixPermissions] as? NSNumber,
              permissions.intValue & 0o077 == 0,
              let owner = values[.ownerAccountID] as? NSNumber,
              owner.uint32Value == geteuid() else { throw LocalAPIError.insecureControlToken }
        let data = try Data(contentsOf: url, options: [.mappedIfSafe])
        guard data.count <= 128, let raw = String(data: data, encoding: .utf8)
        else { throw LocalAPIError.insecureControlToken }
        let token = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard token.utf8.count == 64, token.utf8.allSatisfy({
            (48...57).contains($0) || (65...70).contains($0) || (97...102).contains($0)
        }) else { throw LocalAPIError.insecureControlToken }
        return token
    }
}

private final class NoRedirects: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        // Never forward a local control token to another endpoint.
        completionHandler(nil)
    }
}

public final class LocalAPI {
    public let endpoint: LocalEndpoint
    private let session: URLSession
    private let tokenFile: URL
    private let maximumResponseBytes = 1024 * 1024

    public init(endpoint: LocalEndpoint, tokenFile: URL) {
        self.endpoint = endpoint; self.tokenFile = tokenFile
        let configuration = URLSessionConfiguration.ephemeral
        configuration.timeoutIntervalForRequest = 12
        configuration.timeoutIntervalForResource = 25
        configuration.connectionProxyDictionary = [:]
        configuration.urlCache = nil
        session = URLSession(configuration: configuration, delegate: NoRedirects(), delegateQueue: nil)
    }

    deinit { session.invalidateAndCancel() }

    public func version() async throws -> VersionResponse {
        try await fetch("api/version", authenticated: false)
    }

    public func catalog() async throws -> [CatalogModel] {
        let value: Catalog = try await fetch("api/tags", authenticated: false)
        guard value.models.count <= 1024 else { throw LocalAPIError.responseTooLarge }
        return value.models.sorted { $0.name.localizedStandardCompare($1.name) == .orderedAscending }
    }

    public func status() async throws -> NodeStatus {
        try await fetch("api/lucid/status", authenticated: true)
    }

    public func enroll(invitation: String) async throws -> EnrollmentStatus {
        let value = try Invitation.validate(invitation)
        return try await fetch("api/lucid/enroll", authenticated: true,
                               body: JSONSerialization.data(withJSONObject: ["invitation": value]),
                               successCodes: [200, 202])
    }

    private func fetch<T: Decodable>(_ path: String, authenticated: Bool, body: Data? = nil,
                                     successCodes: Set<Int> = [200]) async throws -> T {
        var request = URLRequest(url: endpoint.url(path: path))
        request.httpMethod = body == nil ? "GET" : "POST"
        request.httpBody = body
        if body != nil { request.setValue("application/json", forHTTPHeaderField: "Content-Type") }
        if authenticated {
            request.setValue("Bearer \(try ControlToken.read(from: tokenFile))", forHTTPHeaderField: "Authorization")
        }
        let (stream, response) = try await session.bytes(for: request)
        guard let response = response as? HTTPURLResponse else { throw LocalAPIError.httpStatus(0) }
        guard successCodes.contains(response.statusCode) else { throw LocalAPIError.httpStatus(response.statusCode) }
        if response.expectedContentLength > maximumResponseBytes { throw LocalAPIError.responseTooLarge }
        var data = Data()
        for try await byte in stream {
            guard data.count < maximumResponseBytes else { throw LocalAPIError.responseTooLarge }
            data.append(byte)
        }
        return try JSONDecoder().decode(T.self, from: data)
    }
}
