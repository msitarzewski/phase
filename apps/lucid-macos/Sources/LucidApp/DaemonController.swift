// SPDX-License-Identifier: AGPL-3.0-or-later
import AppKit
import CryptoKit
import Foundation
import LucidCore
import UniformTypeIdentifiers

@MainActor
final class DaemonController: ObservableObject {
    @Published private(set) var isRunning = false
    @Published private(set) var isStarting = false
    @Published private(set) var isStopping = false
    @Published private(set) var isRefreshing = false
    @Published private(set) var models: [CatalogModel] = []
    @Published private(set) var nodeStatus: NodeStatus?
    @Published private(set) var enrollment: EnrollmentStatus?
    @Published var errorMessage: String?
    @Published var selectedModelID: String?
    @Published var showJoinNetwork = false
    @Published var invitation = ""
    @Published var apiPort: Int {
        didSet { if privateProfile?.stateDirectory == nil { UserDefaults.standard.set(apiPort, forKey: "apiPort") } }
    }
    @Published var chatAppURL: String {
        didSet { if privateProfile?.stateDirectory == nil { UserDefaults.standard.set(chatAppURL, forKey: "chatAppURL") } }
    }
    @Published var automaticallyConnect: Bool {
        didSet { if privateProfile?.stateDirectory == nil { UserDefaults.standard.set(automaticallyConnect, forKey: "automaticallyConnect") } }
    }

    private var process: Process?
    private var api: LocalAPI?
    private var monitor: Task<Void, Never>?
    private var readiness: Task<Void, Never>?
    private var shutdown: Task<Void, Never>?
    private var stopRequested = false
    private var hasLaunched = false
    private var isAppActive = true
    private var terminationWaiters: [CheckedContinuation<Void, Never>] = []
    private var controlTokenFile: URL?
    private var privateProfile: PrivateNetworkProfile?
    private var profileError: String?

    init() {
        let profile: PrivateNetworkProfile?
        do { profile = try PrivateNetworkProfile(environment: ProcessInfo.processInfo.environment) }
        catch { profile = nil; profileError = LocalAPIError.invalidPrivateProfile.errorDescription }
        privateProfile = profile
        let preferences = UserDefaults.standard
        let isolated = profile?.stateDirectory != nil
        let savedPort = isolated ? 0 : preferences.integer(forKey: "apiPort")
        apiPort = (1024...65535).contains(savedPort) ? savedPort : 11435
        chatAppURL = profile?.chatAppURL ?? (isolated ? nil : preferences.string(forKey: "chatAppURL")) ?? "http://localhost:3000"
        automaticallyConnect = isolated ? true : preferences.object(forKey: "automaticallyConnect") as? Bool ?? true
    }

    var selectedModel: CatalogModel? { models.first { $0.id == selectedModelID } }
    var localAddress: String { "http://127.0.0.1:\(apiPort)" }
    var hasOwnedProcess: Bool { process?.isRunning == true }
    var statusTitle: String {
        if isStopping { return "Disconnecting" }
        if isStarting { return "Starting LUCID" }
        guard isRunning else { return "Disconnected" }
        return nodeStatus?.state == "connected" ? "Connected" : "Waiting for network"
    }
    var enrollmentTitle: String { enrollment?.title ?? nodeStatus?.enrollment.title ?? "Join a network" }

    private var supportDirectory: URL {
        privateProfile?.stateDirectory ?? FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Application Support/LUCID", isDirectory: true)
    }

    func launchIfNeeded() {
        guard !hasLaunched else { return }
        hasLaunched = true
        if automaticallyConnect { connect() }
    }

    func connect() {
        guard !hasOwnedProcess, !isStarting, !isStopping else { return }
        if let profileError { errorMessage = profileError; return }
        guard geteuid() != 0 else {
            errorMessage = "Run LUCID as your normal macOS user. Administrator privileges aren't required."
            return
        }
        do {
            let endpoint = try LocalEndpoint(port: apiPort)
            try endpoint.requireAvailablePort()
            let state = supportDirectory
            try preparePrivateDirectory(state)
            let binary = try bundledDaemon()
            let tokenFile = state.appendingPathComponent("control-\(UUID().uuidString).token")
            controlTokenFile = tokenFile
            let client = LocalAPI(endpoint: endpoint, tokenFile: tokenFile)
            let child = Process()
            child.executableURL = binary
            child.arguments = [
                "--mode", "consume-only", "--outbound-only",
                "--identity-path", state.appendingPathComponent("identity.key").path,
                "--policy-config", state.appendingPathComponent("policy.toml").path,
                "--artifact-dir", state.appendingPathComponent("artifacts", isDirectory: true).path,
                "--control-token-file", tokenFile.path
            ] + (privateProfile?.relayArguments ?? [])
            // Do not pass the launching shell's API keys, proxy settings or credentials to the daemon.
            child.environment = [
                "LUCIDD_HOST": "127.0.0.1", "LUCIDD_PORT": String(apiPort),
                "RUST_LOG": "warn", "NO_COLOR": "1",
                "HOME": FileManager.default.homeDirectoryForCurrentUser.path,
                "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "TMPDIR": NSTemporaryDirectory()
            ]
            child.currentDirectoryURL = state
            child.standardInput = FileHandle.nullDevice
            let log = try openLog(in: state)
            child.standardOutput = log; child.standardError = log
            child.terminationHandler = { [weak self] ended in
                Task { @MainActor in self?.didTerminate(ended) }
            }
            stopRequested = false; errorMessage = nil; isStarting = true
            process = child; api = client
            try child.run()
            try? log.close()
            readiness = Task { [weak self, weak child] in
                let deadline = Date().addingTimeInterval(25)
                while !Task.isCancelled, Date() < deadline, child?.isRunning == true {
                    do {
                        _ = try await client.version()
                        guard let self, self.process === child, child?.isRunning == true else { return }
                        self.isRunning = true; self.isStarting = false
                        await self.refresh()
                        self.beginMonitoring()
                        return
                    } catch { /* Startup can include DNS discovery; retry while our own child is alive. */ }
                    try? await Task.sleep(nanoseconds: 250_000_000)
                }
                guard let self, !Task.isCancelled, self.process === child else { return }
                self.errorMessage = "LUCID couldn't start its local API. Check that the port is free, then reconnect."
                await self.disconnect()
            }
        } catch {
            isStarting = false; process = nil; api = nil
            removeSessionToken()
            errorMessage = (error as? LocalAPIError)?.errorDescription
                ?? "LUCID couldn't start. Rebuild or reinstall the app with its bundled daemon."
        }
    }

    func disconnect() async {
        if let pending = shutdown { await pending.value; return }
        isStopping = true; stopRequested = true
        readiness?.cancel(); readiness = nil
        monitor?.cancel(); monitor = nil
        let child = process
        if child?.isRunning == true { child?.terminate() }
        let pending = Task { @MainActor [weak self, child] in
            if let child, child.isRunning {
                await self?.waitForExitOrKill(child, timeoutNanoseconds: 3_000_000_000)
            }
            guard let self else { return }
            self.process = nil; self.api = nil; self.isRunning = false; self.isStarting = false
            self.removeSessionToken()
            self.models = []; self.nodeStatus = nil; self.enrollment = nil; self.selectedModelID = nil
            self.shutdown = nil; self.isStopping = false
        }
        shutdown = pending
        await pending.value
    }

    private func waitForExitOrKill(_ child: Process, timeoutNanoseconds: UInt64) async {
        guard child.isRunning else { return }
        let escalation = Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: timeoutNanoseconds)
            guard !Task.isCancelled else { return }
            // Only the Process instance launched by this app is eligible for escalation.
            if child.isRunning { Darwin.kill(child.processIdentifier, SIGKILL) }
            self?.resumeTerminationWaiters()
        }
        await withCheckedContinuation { continuation in
            if !child.isRunning {
                continuation.resume()
            } else {
                terminationWaiters.append(continuation)
            }
        }
        escalation.cancel()
    }

    private func resumeTerminationWaiters() {
        let waiters = terminationWaiters
        terminationWaiters.removeAll()
        for waiter in waiters { waiter.resume() }
    }

    func setAppActive(_ active: Bool) {
        guard isAppActive != active else { return }
        isAppActive = active
        guard isRunning else { return }
        beginMonitoring()
        if active {
            Task { [weak self] in await self?.refresh() }
        }
    }

    func refresh() async {
        guard isRunning, !isRefreshing, let api else { return }
        isRefreshing = true
        defer { isRefreshing = false }
        do {
            async let statusRequest = api.status()
            async let catalogRequest = api.catalog()
            let (status, found) = try await (statusRequest, catalogRequest)
            guard isRunning else { return }
            nodeStatus = status; enrollment = status.enrollment; models = found
            if !found.contains(where: { $0.id == selectedModelID }) { selectedModelID = found.first?.id }
            errorMessage = nil
        } catch is CancellationError {
            return
        } catch {
            guard isRunning else { return }
            // Clear expired availability rather than leaving a stale model looking usable.
            models = []; selectedModelID = nil
            errorMessage = "The model catalog couldn't be refreshed. Check network access or join a network, then retry."
        }
    }

    func joinNetwork() async {
        guard isRunning, let api else { return }
        do {
            let value = try Invitation.validate(invitation)
            invitation = "" // Invitation material is never persisted or logged by the app.
            enrollment = try await api.enroll(invitation: value)
            showJoinNetwork = false
            errorMessage = nil
            await refresh()
        } catch {
            invitation = ""
            errorMessage = "Network access couldn't be requested. Check the invitation and your connection, then try again."
        }
    }

    func importInvitation() {
        let panel = NSOpenPanel()
        panel.title = "Join a LUCID network"
        panel.message = "Choose the network invitation you received."
        panel.allowsMultipleSelection = false; panel.canChooseDirectories = false
        panel.allowedContentTypes = [.json, .init(filenameExtension: "lucidinvite") ?? .data]
        guard panel.runModal() == .OK, let url = panel.url else { return }
        receiveInvitation(url)
    }

    func receiveInvitation(_ url: URL) {
        do {
            let access = url.startAccessingSecurityScopedResource()
            defer { if access { url.stopAccessingSecurityScopedResource() } }
            invitation = try url.isFileURL ? Invitation.read(from: url) : Invitation.validate(url.absoluteString)
            showJoinNetwork = true
        } catch {
            errorMessage = "This network invitation couldn't be read. Choose a valid .lucidinvite or JSON invitation."
        }
    }

    func openChat(model: CatalogModel? = nil) {
        if let model, !model.canRequest { showJoinNetwork = true; return }
        do {
            let url = try ChatAppLink.make(base: chatAppURL, model: model?.id)
            if !NSWorkspace.shared.open(url) { errorMessage = "Your chat app couldn't be opened. Check its URL in Settings." }
        } catch { errorMessage = (error as? LocalAPIError)?.errorDescription }
    }

    func copy(_ value: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(value, forType: .string)
    }

    func revealDiagnostics() {
        NSWorkspace.shared.activateFileViewerSelecting([supportDirectory.appendingPathComponent("lucidd.log")])
    }

    private func beginMonitoring() {
        monitor?.cancel()
        monitor = Task { [weak self] in
            while !Task.isCancelled {
                let intervalNs: UInt64 = (self?.isAppActive ?? true) ? 15_000_000_000 : 60_000_000_000
                try? await Task.sleep(nanoseconds: intervalNs)
                guard !Task.isCancelled, let self, self.isRunning else { return }
                await self.refresh()
            }
        }
    }

    private func didTerminate(_ child: Process) {
        resumeTerminationWaiters()
        guard process === child else { return }
        monitor?.cancel(); monitor = nil
        process = nil; api = nil; isRunning = false; isStarting = false
        removeSessionToken()
        models = []; nodeStatus = nil
        if !stopRequested {
            errorMessage = "LUCID stopped unexpectedly (exit \(child.terminationStatus)). Check that its local port is free, then reconnect."
        }
    }

    private func removeSessionToken() {
        if let file = controlTokenFile { try? FileManager.default.removeItem(at: file) }
        controlTokenFile = nil
    }

    private func preparePrivateDirectory(_ url: URL) throws {
        let files = FileManager.default
        if files.fileExists(atPath: url.path) {
            let values = try files.attributesOfItem(atPath: url.path)
            guard values[.type] as? FileAttributeType == .typeDirectory,
                  (values[.ownerAccountID] as? NSNumber)?.uint32Value == geteuid()
            else { throw LocalAPIError.insecureControlToken }
        } else {
            try files.createDirectory(at: url, withIntermediateDirectories: true, attributes: [.posixPermissions: 0o700])
        }
        try files.setAttributes([.posixPermissions: 0o700], ofItemAtPath: url.path)
    }

    private func openLog(in directory: URL) throws -> FileHandle {
        let url = directory.appendingPathComponent("lucidd.log")
        let files = FileManager.default
        if files.fileExists(atPath: url.path) {
            let attributes = try files.attributesOfItem(atPath: url.path)
            guard attributes[.type] as? FileAttributeType == .typeRegular else { throw LocalAPIError.insecureControlToken }
            if (attributes[.size] as? NSNumber)?.intValue ?? 0 > 1024 * 1024 {
                let previous = directory.appendingPathComponent("lucidd.previous.log")
                if files.fileExists(atPath: previous.path) { try files.removeItem(at: previous) }
                try files.moveItem(at: url, to: previous)
            }
        }
        if !files.fileExists(atPath: url.path) {
            guard files.createFile(atPath: url.path, contents: nil, attributes: [.posixPermissions: 0o600])
            else { throw CocoaError(.fileWriteUnknown) }
        }
        try files.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
        let handle = try FileHandle(forWritingTo: url)
        try handle.seekToEnd()
        return handle
    }

    private func bundledDaemon() throws -> URL {
        let binary = Bundle.main.bundleURL.appendingPathComponent("Contents/Helpers/lucidd")
        let files = FileManager.default
        let values = try files.attributesOfItem(atPath: binary.path)
        // LaunchServices can retain the previous bundle's metadata after a local rebuild.
        let infoURL = Bundle.main.bundleURL.appendingPathComponent("Contents/Info.plist")
        let info = try PropertyListSerialization.propertyList(from: Data(contentsOf: infoURL), format: nil) as? [String: Any]
        guard values[.type] as? FileAttributeType == .typeRegular,
              files.isExecutableFile(atPath: binary.path),
              let expected = info?["LucidDaemonSHA256"] as? String
        else { throw CocoaError(.fileReadNoSuchFile) }
        let digest = SHA256.hash(data: try Data(contentsOf: binary, options: .mappedIfSafe))
            .map { String(format: "%02x", $0) }.joined()
        guard digest == expected else { throw CocoaError(.fileReadCorruptFile) }
        return binary
    }
}
