// SPDX-License-Identifier: AGPL-3.0-or-later
import AppKit
import SwiftUI
import UniformTypeIdentifiers
import LucidCore

@main
struct LucidApp: App {
    @NSApplicationDelegateAdaptor(ApplicationDelegate.self) private var delegate

    var body: some Scene {
        let controller = delegate.controller
        WindowGroup("LUCID", id: "main") {
            ContentView(controller: controller)
                .frame(minWidth: 860, minHeight: 560)
                .onOpenURL { controller.receiveInvitation($0) }
        }
        .defaultSize(width: 1040, height: 690)
        .commands {
            CommandGroup(after: .newItem) {
                Button("Join a Network…") { controller.showJoinNetwork = true }
                    .keyboardShortcut("j", modifiers: [.command, .shift])
                Button("Import Network Invitation…") { controller.importInvitation() }
            }
            CommandGroup(after: .pasteboard) {
                Button("Copy LUCID API Address") { controller.copy(controller.localAddress) }
                    .keyboardShortcut("c", modifiers: [.command, .shift])
            }
        }
        Settings { SettingsView(controller: controller).frame(width: 490) }
        MenuBarExtra("LUCID", systemImage: "network") {
            MenuContent(controller: controller)
        }
    }
}

@MainActor
final class ApplicationDelegate: NSObject, NSApplicationDelegate {
    let controller = DaemonController()

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        controller.launchIfNeeded()
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationDidBecomeActive(_ notification: Notification) {
        controller.setAppActive(true)
    }

    func applicationDidResignActive(_ notification: Notification) {
        controller.setAppActive(false)
    }

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        guard controller.hasOwnedProcess else { return .terminateNow }
        Task {
            await controller.disconnect()
            sender.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }
}

private struct ContentView: View {
    @ObservedObject var controller: DaemonController
    @State private var search = ""
    @State private var joining = false

    var matchingModels: [CatalogModel] {
        controller.models.filter { search.isEmpty || $0.name.localizedCaseInsensitiveContains(search) }
    }

    var body: some View {
        NavigationSplitView {
            List(selection: $controller.selectedModelID) {
                Section("Available models") {
                    ForEach(matchingModels) { model in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(model.name).font(.body.weight(.medium)).lineLimit(2)
                            if !model.metadata.isEmpty {
                                Text(model.metadata.joined(separator: " · "))
                                    .font(.caption).foregroundStyle(.secondary).lineLimit(1)
                            }
                            if !model.canRequest {
                                Label("Network access required", systemImage: "lock")
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        }
                        .padding(.vertical, 5)
                        .tag(model.id)
                    }
                }
            }
            .searchable(text: $search, prompt: "Find a model")
            .navigationSplitViewColumnWidth(min: 250, ideal: 310, max: 400)
            .overlay {
                if controller.models.isEmpty {
                    VStack(spacing: 12) {
                        Image(systemName: controller.isRunning ? "sparkle.magnifyingglass" : "network")
                            .font(.system(size: 30)).foregroundStyle(.secondary)
                        Text(controller.isRunning ? "No models available yet" : "Connect to discover models")
                            .font(.headline).multilineTextAlignment(.center)
                        Text(controller.isRunning ? "Join a network or refresh after a contributor becomes available." : "LUCID finds models from the networks you can access.")
                            .font(.callout).foregroundStyle(.secondary).multilineTextAlignment(.center)
                    }.padding(28).allowsHitTesting(false)
                }
            }
        } detail: {
            ScrollView {
                VStack(alignment: .leading, spacing: 26) {
                    connectionSummary
                    if let message = controller.errorMessage {
                        Label(message, systemImage: "exclamationmark.circle")
                            .font(.callout).foregroundStyle(.orange)
                            .padding(14).frame(maxWidth: .infinity, alignment: .leading)
                            .background(.orange.opacity(0.08), in: RoundedRectangle(cornerRadius: 12))
                    }
                    if let model = controller.selectedModel {
                        modelDetail(model)
                    } else {
                        VStack(alignment: .leading, spacing: 10) {
                            Text("Your models. Your chat app.").font(.largeTitle.bold())
                            Text("Connect to a LUCID network, choose a model, and use it in Open WebUI or another Ollama-compatible app.")
                                .font(.title3).foregroundStyle(.secondary)
                        }.padding(.vertical, 16)
                    }
                    apiConnectionCard
                    Text("Remote contributors can read prompts they process. Use a network you trust for sensitive work.")
                        .font(.caption).foregroundStyle(.secondary)
                }.padding(32).frame(maxWidth: 760, alignment: .leading)
            }.frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .toolbar {
            ToolbarItemGroup {
                Button { Task { await controller.refresh() } } label: {
                    if controller.isRefreshing { ProgressView().controlSize(.small) }
                    else { Label("Refresh models", systemImage: "arrow.clockwise") }
                }.disabled(!controller.isRunning || controller.isRefreshing)
                    .keyboardShortcut("r", modifiers: .command)
                Button {
                    if controller.isRunning || controller.isStarting { Task { await controller.disconnect() } }
                    else { controller.connect() }
                } label: {
                    Label(controller.isRunning || controller.isStarting ? "Disconnect" : "Connect",
                          systemImage: controller.isRunning || controller.isStarting ? "stop.circle" : "network")
                }.disabled(controller.isStopping)
            }
        }
        .sheet(isPresented: $controller.showJoinNetwork, onDismiss: { controller.invitation = "" }) {
            VStack(alignment: .leading, spacing: 18) {
                Text("Join a network").font(.title.bold())
                Text("Use a network invitation to request access. Your private identity stays on this Mac.")
                    .foregroundStyle(.secondary)
                SecureField("Paste network invitation", text: $controller.invitation)
                    .textFieldStyle(.roundedBorder)
                    .onSubmit { submitInvitation() }
                if let error = controller.errorMessage {
                    Text(error).font(.callout).foregroundStyle(.orange)
                }
                HStack {
                    Button("Import invitation…") { controller.importInvitation() }
                    Spacer()
                    Button("Cancel") { controller.showJoinNetwork = false }
                    Button("Join") { submitInvitation() }
                        .buttonStyle(.borderedProminent)
                        .disabled(joining || !controller.isRunning || controller.invitation.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
                if !controller.isRunning {
                    HStack {
                        Text("Connect LUCID before joining a network.").font(.caption).foregroundStyle(.secondary)
                        Spacer()
                        Button("Connect LUCID") { controller.connect() }.disabled(controller.isStarting)
                    }
                }
            }.padding(28).frame(width: 500)
        }
    }

    private func submitInvitation() {
        guard !joining, controller.isRunning else { return }
        joining = true
        Task { await controller.joinNetwork(); joining = false }
    }

    private var connectionSummary: some View {
        HStack(alignment: .top) {
            VStack(alignment: .leading, spacing: 7) {
                HStack(spacing: 8) {
                    Circle().fill(controller.nodeStatus?.state == "connected" ? Color.green : Color.secondary)
                        .frame(width: 8, height: 8)
                    Text(controller.statusTitle).font(.headline)
                    if controller.isStarting { ProgressView().controlSize(.small) }
                }
                Text(controller.enrollmentTitle).font(.callout).foregroundStyle(.secondary)
            }
            Spacer()
            Button("Join a network…") { controller.showJoinNetwork = true }
                .disabled(!controller.isRunning)
        }
    }

    private func modelDetail(_ model: CatalogModel) -> some View {
        VStack(alignment: .leading, spacing: 18) {
            Image(systemName: "sparkles").font(.system(size: 33)).foregroundStyle(.tint)
            Text(model.name).font(.system(size: 28, weight: .semibold)).textSelection(.enabled)
            if !model.metadata.isEmpty {
                HStack {
                    ForEach(model.metadata, id: \.self) { value in
                        Text(value).font(.caption.weight(.medium)).padding(.horizontal, 10).padding(.vertical, 6)
                            .background(.quaternary, in: Capsule())
                    }
                }
            }
            if let size = model.size, size > 0 {
                LabeledContent("Model size", value: ByteCountFormatter.string(fromByteCount: Int64(clamping: size), countStyle: .file))
                    .font(.callout).foregroundStyle(.secondary)
            }
            Text("LUCID chooses an eligible contributor for each request and checks the returned receipt.")
                .font(.body).foregroundStyle(.secondary)
            HStack {
                if model.canRequest {
                    Button { controller.openChat(model: model) } label: { Label("Chat with this model", systemImage: "arrow.up.right") }
                        .buttonStyle(.borderedProminent).controlSize(.large)
                } else {
                    Button { controller.showJoinNetwork = true } label: { Label("Join to use this model", systemImage: "lock.open") }
                        .buttonStyle(.borderedProminent).controlSize(.large)
                }
                Button("Copy model ID") { controller.copy(model.id) }.controlSize(.large)
            }.padding(.top, 4)
        }.frame(maxWidth: .infinity, alignment: .leading)
    }

    private var apiConnectionCard: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Connect your chat app").font(.headline)
            Text("Add LUCID as an Ollama connection in your chat app. Both apps use the same model list.")
                .font(.callout).foregroundStyle(.secondary)
            HStack {
                Text(controller.localAddress).font(.system(.callout, design: .monospaced)).textSelection(.enabled)
                Spacer()
                Button { controller.copy(controller.localAddress) } label: { Label("Copy", systemImage: "doc.on.doc") }
            }
        }.padding(20).background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 14))
    }
}

private struct SettingsView: View {
    @ObservedObject var controller: DaemonController

    var body: some View {
        Form {
            Section("Connection") {
                Toggle("Connect when LUCID opens", isOn: $controller.automaticallyConnect)
                TextField("Local API port", value: $controller.apiPort, format: .number.grouping(.never))
                    .disabled(controller.isRunning || controller.isStarting)
                Text("The API listens on this Mac's loopback address. Disconnect before changing the port.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Section("Chat app") {
                TextField("Open WebUI URL", text: $controller.chatAppURL)
                Text("Selecting a model opens this address with Open WebUI's model-selection parameter. No prompt is sent in the URL.")
                    .font(.caption).foregroundStyle(.secondary)
                Button("Open chat app") { controller.openChat() }
            }
            Section("Diagnostics") {
                Button("Show local diagnostic log…") { controller.revealDiagnostics() }
                Text("Diagnostic files stay on this Mac and may contain internal network addresses. Network invitations and control tokens are never displayed or copied by LUCID.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }.formStyle(.grouped).padding()
    }
}

private struct MenuContent: View {
    @ObservedObject var controller: DaemonController
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Text(controller.statusTitle)
        Button("Open LUCID") { openWindow(id: "main"); NSApp.activate(ignoringOtherApps: true) }
        Divider()
        Button(controller.isRunning || controller.isStarting ? "Disconnect" : "Connect") {
            if controller.isRunning || controller.isStarting { Task { await controller.disconnect() } }
            else { controller.connect() }
        }.disabled(controller.isStopping)
        Button("Copy API address") { controller.copy(controller.localAddress) }
        Button("Open chat app") { controller.openChat(model: controller.selectedModel) }
        Divider()
        Button("Quit LUCID") { NSApp.terminate(nil) }.keyboardShortcut("q")
    }
}
