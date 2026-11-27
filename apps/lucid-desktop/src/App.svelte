<script lang="ts">
  import { onMount, onDestroy } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { open as openDialog } from "@tauri-apps/plugin-dialog";
  import {
    lucidApi,
    type OverviewPayload,
    type ModelEntry,
    type InviteResponse,
    type ClusterStatusResponse,
    type PeerEntry,
  } from "./lib/api";
  import {
    connectionPathLabel,
    enrollmentLabel,
    formatBytes,
    shortPeerId,
    sourceLabel,
    statusPillText,
  } from "./lib/format";
  import ChatPanel from "./lib/ChatPanel.svelte";

  let overview = $state<OverviewPayload | null>(null);
  let loading = $state(true);
  let busy = $state(false);
  let runtimeBusy = $state(false);
  let errorBanner = $state<string | null>(null);
  let toast = $state<string | null>(null);

  let search = $state("");
  let filter = $state<"all" | "local" | "sharing">("all");
  let selectedId = $state<string | null>(null);
  let showDetails = $state(false);

  // Modals / Sheets
  let showJoinModal = $state(false);
  let joinInput = $state("");
  let showInviteModal = $state(false);
  let activeInvite = $state<InviteResponse | null>(null);
  let showPeersModal = $state(false);
  let showClusterModal = $state(false);
  let clusterResponse = $state<ClusterStatusResponse | null>(null);
  let clusterLoading = $state(false);
  let clusterError = $state<string | null>(null);
  let clusterRequest = 0;
  let clusterStatus = $derived(clusterResponse?.status ?? null);
  let clusterMemory = $derived(formatBytes(clusterStatus?.local_node?.available_memory_bytes ?? 0, true));
  let deviceEnrollment = $derived(enrollmentLabel(overview?.status.enrollment?.state));
  let showSettingsModal = $state(false);
  let showChatModal = $state(false);
  let chatInitialModel = $state<string | null>(null);
  let chatModels = $derived(
    (overview?.models ?? [])
      .filter((m) => m.can_request || m.providing || m.on_device)
      .map((m) => m.name),
  );

  // Permission-first modal before enabling providing or installing runtime
  let pendingProvideModel = $state<ModelEntry | null>(null);
  let onboardingSelected = $state<Record<string, boolean>>({});
  let chatUrlDraft = $state("http://localhost:3000");

  let pollTimer: ReturnType<typeof setInterval> | null = null;
  let unlistenDeepLink: (() => void) | null = null;

  function showToast(msg: string) {
    toast = msg;
    setTimeout(() => {
      if (toast === msg) toast = null;
    }, 3200);
  }

  async function refresh(silent = false) {
    try {
      const data = await lucidApi.getOverview();
      overview = data;
      chatUrlDraft = data.status.chat_app_url;
      if (!selectedId || !data.models.some((m) => m.id === selectedId)) {
        selectedId = data.models[0]?.id ?? null;
      }
      if (!silent) errorBanner = null;
    } catch (err) {
      if (!silent) {
        errorBanner = err instanceof Error ? err.message : String(err);
      }
    } finally {
      loading = false;
    }
  }

  onMount(async () => {
    await refresh();
    pollTimer = setInterval(() => {
      if (!document.hidden) void refresh(true);
    }, 10000);

    unlistenDeepLink = await listen<string>("lucid-invitation", (event) => {
      if (typeof event.payload === "string" && event.payload.trim().length > 0) {
        joinInput = event.payload.trim();
        showJoinModal = true;
      }
    });
  });

  onDestroy(() => {
    if (pollTimer) clearInterval(pollTimer);
    if (unlistenDeepLink) unlistenDeepLink();
  });

  let filteredModels = $derived.by(() => {
    const list = overview?.models ?? [];
    const q = search.trim().toLowerCase();
    return list.filter((m) => {
      if (filter === "local" && !m.on_device) return false;
      if (filter === "sharing" && !m.providing) return false;
      if (q && !m.name.toLowerCase().includes(q)) return false;
      return true;
    });
  });

  let selectedModel = $derived.by(() => {
    const list = overview?.models ?? [];
    return list.find((m) => m.id === selectedId) ?? null;
  });

  let showOnboarding = $derived.by(() => {
    if (!overview || !overview.probe) return false;
    return !overview.status.onboarding_completed;
  });

  async function copyText(text: string, label: string) {
    try {
      await navigator.clipboard.writeText(text);
      showToast(`Copied ${label}`);
    } catch {
      errorBanner = `Could not copy ${label} to clipboard.`;
    }
  }

  async function handleAddFolder() {
    try {
      const picked = await openDialog({
        directory: true,
        multiple: false,
        title: "Choose a folder containing .gguf models",
      });
      if (typeof picked !== "string" || !picked) return;
      busy = true;
      errorBanner = null;
      overview = await lucidApi.addModelFolder(picked);
      showToast("Model folder added");
    } catch (err) {
      errorBanner = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function handleRemoveFolder(path: string) {
    try {
      busy = true;
      overview = await lucidApi.removeModelFolder(path);
      showToast("Folder removed");
    } catch (err) {
      errorBanner = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  function requestToggleProvide(model: ModelEntry) {
    if (model.providing) {
      void applyProvide(model, false);
    } else {
      // Always ask explicit permission before sharing compute or installing runtime.
      pendingProvideModel = model;
    }
  }

  async function applyProvide(model: ModelEntry, enable: boolean) {
    try {
      busy = true;
      errorBanner = null;
      if (enable && overview?.probe && overview.probe.runtime.status !== "ready") {
        runtimeBusy = true;
        const action =
          overview.probe.runtime.status === "detected_unpinned"
            ? "pin_detected"
            : "download_pinned";
        await lucidApi.setupRuntime(action, true);
        runtimeBusy = false;
      }
      const res = await lucidApi.setProvidePermission(
        model.name,
        enable,
        enable,
        true,
        model.local_path
      );
      pendingProvideModel = null;
      if (res.restart_required) {
        overview = await lucidApi.restartDaemon();
        showToast(`Now providing ${model.name}`);
      } else {
        await refresh();
        showToast(enable ? `Now providing ${model.name}` : `Stopped providing ${model.name}`);
      }
    } catch (err) {
      errorBanner = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
      runtimeBusy = false;
    }
  }

  async function completeOnboarding(shareSelected: boolean) {
    try {
      busy = true;
      errorBanner = null;
      const chosen = Object.entries(onboardingSelected)
        .filter(([, v]) => v)
        .map(([k]) => k);

      if (shareSelected && chosen.length > 0) {
        if (overview?.probe && overview.probe.runtime.status !== "ready") {
          runtimeBusy = true;
          const action =
            overview.probe.runtime.status === "detected_unpinned"
              ? "pin_detected"
              : "download_pinned";
          await lucidApi.setupRuntime(action, true);
          runtimeBusy = false;
        }
        let needsRestart = false;
        for (const name of chosen) {
          const disc = overview?.probe?.discovered_models.find((m) => m.name === name);
          const res = await lucidApi.setProvidePermission(
            name,
            true,
            true,
            true,
            disc?.path ?? null
          );
          if (res.restart_required) needsRestart = true;
        }
        if (needsRestart) {
          overview = await lucidApi.restartDaemon();
        }
      } else {
        await lucidApi.updateSettings({ onboarding_completed: true });
      }
      await refresh();
      showToast(
        shareSelected && chosen.length > 0
          ? `Sharing ${chosen.length} model${chosen.length === 1 ? "" : "s"}`
          : "LUCID is ready in private consume-only mode"
      );
    } catch (err) {
      errorBanner = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
      runtimeBusy = false;
    }
  }

  async function handleCreateInvite() {
    try {
      busy = true;
      errorBanner = null;
      activeInvite = await lucidApi.createInvite(1800);
      showInviteModal = true;
    } catch (err) {
      errorBanner = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function handleJoinInvite() {
    if (!joinInput.trim()) return;
    try {
      busy = true;
      errorBanner = null;
      await lucidApi.joinInvite(joinInput.trim());
      joinInput = "";
      showJoinModal = false;
      showToast("Join request sent — waiting for approval");
      await refresh();
    } catch (err) {
      errorBanner = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function handleTogglePolicy(
    patch: {
      manual_pause?: boolean;
      auto_pause_on_battery?: boolean;
      thermal_limit_c?: number | null;
      chat_app_url?: string;
    },
    label: string
  ) {
    try {
      busy = true;
      await lucidApi.updateSettings(patch);
      await refresh();
      showToast(label);
    } catch (err) {
      errorBanner = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }

  async function handleOpenChat(model: ModelEntry) {
    try {
      await lucidApi.openChat(model.id);
    } catch (err) {
      errorBanner = err instanceof Error ? err.message : String(err);
    }
  }

  function handleOpenCluster() {
    showClusterModal = true;
    void loadClusterStatus();
  }

  function handleCloseCluster() {
    showClusterModal = false;
    clusterRequest += 1;
  }

  async function loadClusterStatus() {
    const request = ++clusterRequest;
    clusterResponse = null;
    clusterError = null;
    clusterLoading = true;
    try {
      const response = await lucidApi.getClusterStatus();
      if (request === clusterRequest) clusterResponse = response;
    } catch {
      if (request === clusterRequest) clusterError = "Cluster status could not be loaded.";
    } finally {
      if (request === clusterRequest) clusterLoading = false;
    }
  }

  async function handleToggleCluster() {
    const enabled = clusterStatus?.cluster_enabled;
    if (typeof enabled !== "boolean") return;
    try {
      busy = true;
      clusterError = null;
      const next = !enabled;
      await lucidApi.updateSettings({ cluster_enabled: next });
      if (showClusterModal) await loadClusterStatus();
      await refresh();
      showToast(next ? "Trusted-Circle cluster enabled" : "Trusted-Circle cluster disabled");
    } catch {
      if (showClusterModal) clusterError = "The cluster setting could not be changed.";
    } finally {
      busy = false;
    }
  }

  async function handlePullModel(model: ModelEntry) {
    try {
      busy = true;
      errorBanner = null;
      showToast(`Downloading verified weights for ${model.name}…`);
      await lucidApi.pullModel(model.name, model.cid);
      await refresh();
      showToast(`Downloaded & verified ${model.name}`);
    } catch (err) {
      errorBanner = err instanceof Error ? err.message : String(err);
    } finally {
      busy = false;
    }
  }
</script>

<div class="shell">
  <header class="topbar">
    <div class="brand">
      <span class="dot" class:green={overview?.status.state === "connected"} class:amber={Boolean(overview?.status.paused_reason)}></span>
      <span class="brand-name">LUCID</span>
      <span class="pill">
        {#if loading}
          Starting…
        {:else if overview}
          {statusPillText(
            overview.status.state,
            overview.status.peer_count,
            overview.status.paused_reason
          )}
        {:else}
          Offline
        {/if}
      </span>
    </div>

    <div class="actions">
      <button class="ghost" onclick={handleOpenCluster} disabled={!overview}>
        Cluster
      </button>
      <button class="ghost" onclick={() => (showPeersModal = true)} disabled={!overview}>
        Peers{#if overview?.peers} ({overview.peers.length}){/if}
      </button>
      <button onclick={() => (showJoinModal = true)} disabled={!overview}>Join Invite</button>
      <button class="primary" onclick={handleCreateInvite} disabled={busy || !overview}>
        Share Invite
      </button>
      <button class="ghost" onclick={() => (showSettingsModal = true)} aria-label="Settings">
        ⚙
      </button>
    </div>
  </header>

  {#if errorBanner}
    <div class="banner error">
      <span>{errorBanner}</span>
      <button class="ghost" onclick={() => (errorBanner = null)}>Dismiss</button>
    </div>
  {/if}

  {#if toast}
    <div class="toast">{toast}</div>
  {/if}

  <div class="workspace">
    <aside class="sidebar">
      <div class="sidebar-controls">
        <input type="search" placeholder="Find a model…" bind:value={search} />
        <div class="segmented">
          <button class:active={filter === "all"} onclick={() => (filter = "all")}>All</button>
          <button class:active={filter === "local"} onclick={() => (filter = "local")}>
            On Device
          </button>
          <button class:active={filter === "sharing"} onclick={() => (filter = "sharing")}>
            Sharing
          </button>
        </div>
      </div>

      <div class="model-list">
        {#if filteredModels.length === 0}
          <div class="empty-list">
            <p class="empty-title">No models here yet</p>
            <p class="empty-sub">
              Add a local model folder or join a network invite to see available models.
            </p>
          </div>
        {:else}
          {#each filteredModels as model (model.id)}
            <button
              class="model-row"
              class:selected={selectedId === model.id}
              onclick={() => {
                selectedId = model.id;
                showDetails = false;
              }}
            >
              <div class="row-top">
                <span class="row-name">{model.name}</span>
                {#if model.providing}
                  <span class="chip sharing">Providing</span>
                {:else if model.peer_count > 0}
                  <span class="chip">{model.peer_count} {model.peer_count === 1 ? "peer" : "peers"}</span>
                {/if}
              </div>
              <div class="row-meta">
                <span>{sourceLabel(model.source)}</span>
                {#if model.size_bytes > 0}
                  <span>· {formatBytes(model.size_bytes)}</span>
                {/if}
              </div>
            </button>
          {/each}
        {/if}
      </div>

      <div class="sidebar-footer">
        <button class="full-btn" onclick={handleAddFolder} disabled={busy}>
          + Add model folder
        </button>
      </div>
    </aside>

    <main class="detail">
      {#if selectedModel}
        <div class="detail-inner">
          <div class="detail-header">
            <div>
              <h1>{selectedModel.name}</h1>
              <p class="subtitle">
                {sourceLabel(selectedModel.source)}
                {#if selectedModel.size_bytes > 0}
                  · {formatBytes(selectedModel.size_bytes)}
                {/if}
                {#if selectedModel.quantization}
                  · {selectedModel.quantization}
                {/if}
                {#if selectedModel.peer_count > 0}
                  · {selectedModel.peer_count} {selectedModel.peer_count === 1 ? "peer" : "peers"} online
                {/if}
              </p>
            </div>
          </div>

          <section class="card">
            <div class="toggle-row">
              <div>
                <div class="row-label">Available for local apps</div>
                <div class="row-help">
                  Use this model in Open WebUI or any Ollama client on
                  <span class="mono">{overview?.status.api_address ?? "http://127.0.0.1:11435"}</span>
                </div>
              </div>
              <span class="badge" class:on={selectedModel.available_locally || selectedModel.can_request}>
                {selectedModel.available_locally || selectedModel.can_request ? "Ready" : "Join or enable"}
              </span>
            </div>

            <div class="divider"></div>

            <div class="toggle-row">
              <div>
                <div class="row-label">Help provide to my network</div>
                <div class="row-help">
                  {#if selectedModel.providing}
                    Sharing with your authorized network · auto-pauses on battery & high heat
                  {:else if !selectedModel.on_device}
                    Only models stored on this device can be provided right now
                  {:else if !selectedModel.fits_hardware}
                    Larger than recommended memory for this machine
                  {:else}
                    Off by default — LUCID always asks permission before sharing your hardware
                  {/if}
                </div>
              </div>
              <button
                class:primary={!selectedModel.providing}
                disabled={busy || !selectedModel.on_device}
                onclick={() => selectedModel && requestToggleProvide(selectedModel)}
              >
                {selectedModel.providing ? "Stop Providing" : "Provide…"}
              </button>
            </div>
          </section>

          <div class="cta-row">
            {#if selectedModel.can_request || selectedModel.providing}
              <button
                class="primary"
                onclick={() => {
                  chatInitialModel = selectedModel?.name ?? null;
                  showChatModal = true;
                }}
              >
                Chat
              </button>
              <button onclick={() => selectedModel && handleOpenChat(selectedModel)}>
                Open WebUI ↗
              </button>
            {:else if !selectedModel.on_device}
              <button class="primary" onclick={() => (showJoinModal = true)}>
                Join Network to Use
              </button>
            {/if}
            {#if !selectedModel.on_device && selectedModel.peer_count > 0}
              <button
                disabled={busy}
                onclick={() => selectedModel && handlePullModel(selectedModel)}
              >
                Download Locally (P2P)
              </button>
            {/if}
            <button
              onclick={() =>
                copyText(overview?.status.api_address ?? "http://127.0.0.1:11435", "API address")}
            >
              Copy API Address
            </button>
          </div>

          <section class="card subtle">
            <button class="disclosure" onclick={() => (showDetails = !showDetails)}>
              <span>{showDetails ? "▾" : "▸"} Verification & device details</span>
            </button>
            {#if showDetails}
              <div class="details-grid">
                <div><strong>Execution:</strong> Whole-model serving with signed receipts</div>
                {#if selectedModel.cid}
                  <div><strong>Model fingerprint:</strong> <span class="mono">{selectedModel.cid.slice(0, 16)}…</span></div>
                {/if}
                {#if overview?.probe}
                  <div><strong>This device:</strong> {overview.probe.hardware.summary}</div>
                  <div><strong>Inference engine:</strong> {overview.probe.runtime.status}</div>
                {/if}
              </div>
            {/if}
          </section>

          <p class="privacy-note">
            Remote contributors can read prompts they process. Everything runs inside a private sandbox on loopback only — never exposed to the public internet.
          </p>
        </div>
      {:else}
        <div class="welcome-empty">
          <h1>Your models. Your network.</h1>
          <p class="subtitle">
            Connect an invite link to use models from your network, or add a local model folder to choose what this machine helps provide.
          </p>
          <div class="cta-row">
            <button class="primary" onclick={() => (showJoinModal = true)}>Join with Invite</button>
            <button onclick={handleAddFolder}>+ Add Model Folder</button>
          </div>
          {#if overview?.probe}
            <div class="probe-chip">
              {overview.probe.hardware.summary}
            </div>
          {/if}
        </div>
      {/if}
    </main>
  </div>
</div>

<!-- Permission-First Onboarding Modal (brand-new machine) -->
{#if showOnboarding && overview?.probe}
  <div class="backdrop">
    <div class="modal wide">
      <h2>Welcome to LUCID</h2>
      <p class="subtitle">
        We ran a quick, read-only check of this machine. Nothing is shared until you explicitly grant permission.
      </p>

      <div class="card compact">
        <div><strong>Hardware:</strong> {overview.probe.hardware.summary}</div>
        <div>
          <strong>Existing inference:</strong>
          {#if overview.probe.existing_inference.ollama_running}
            Ollama running (v{overview.probe.existing_inference.ollama_version})
          {:else if overview.probe.existing_inference.ollama_store_found}
            Ollama local models detected
          {:else if overview.probe.existing_inference.lmstudio_store_found}
            LM Studio models detected
          {:else}
            None detected
          {/if}
        </div>
        <div>
          <strong>Inference engine:</strong>
          {#if overview.probe.runtime.status === "ready"}
            Verified llama-server ready
          {:else if overview.probe.runtime.status === "detected_unpinned"}
            Existing llama-server found (will pin SHA-256 on permission)
          {:else}
            Pinned llama-server will download on first share ({formatBytes(overview.probe.runtime.downloadable_size_bytes ?? 0)})
          {/if}
        </div>
      </div>

      {#if overview.probe.discovered_models.length > 0}
        <div class="section-title">Choose any models you give permission to provide (optional):</div>
        <div class="check-list">
          {#each overview.probe.discovered_models as m (m.path)}
            <label class="check-row">
              <input
                type="checkbox"
                checked={Boolean(onboardingSelected[m.name])}
                onchange={(e) => {
                  onboardingSelected[m.name] = (e.currentTarget as HTMLInputElement).checked;
                }}
              />
              <span class="check-name">{m.name}</span>
              <span class="check-meta">
                {formatBytes(m.size_bytes)} · {m.fits_hardware ? "Fits well" : "Tight fit"}
              </span>
            </label>
          {/each}
        </div>
      {:else}
        <p class="row-help">
          No local `.gguf` models were found yet. You can start privately right now and join a network or add a model folder any time.
        </p>
      {/if}

      <div class="modal-actions">
        <button onclick={() => completeOnboarding(false)} disabled={busy}>
          Start Privately (Don't Share)
        </button>
        {#if overview.probe.discovered_models.length > 0}
          <button
            class="primary"
            onclick={() => completeOnboarding(true)}
            disabled={busy || !Object.values(onboardingSelected).some(Boolean)}
          >
            {runtimeBusy ? "Verifying engine…" : "Grant Permission & Share Selected"}
          </button>
        {/if}
      </div>
    </div>
  </div>
{/if}

<!-- Permission Confirmation Before Providing a Model -->
{#if pendingProvideModel}
  <div class="backdrop">
    <div class="modal">
      <h2>Permission to provide {pendingProvideModel.name}?</h2>
      <p class="subtitle">
        Only people you explicitly invite will be able to send inference requests to this machine. LUCID automatically pauses when on battery or above 75 °C.
      </p>
      {#if overview?.probe && overview.probe.runtime.status !== "ready"}
        <div class="card compact">
          {#if overview.probe.runtime.status === "detected_unpinned"}
            LUCID will verify and pin your existing <span class="mono">llama-server</span> by SHA-256.
          {:else}
            LUCID will download the official SHA-256-verified <span class="mono">llama-server</span>
            ({overview.probe.runtime.downloadable_backend}, {formatBytes(overview.probe.runtime.downloadable_size_bytes ?? 0)})
            into your private app sandbox.
          {/if}
        </div>
      {/if}
      <div class="modal-actions">
        <button onclick={() => (pendingProvideModel = null)} disabled={busy}>Cancel</button>
        <button
          class="primary"
          onclick={() => pendingProvideModel && applyProvide(pendingProvideModel, true)}
          disabled={busy}
        >
          {runtimeBusy ? "Installing & verifying engine…" : "Allow & Start Providing"}
        </button>
      </div>
    </div>
  </div>
{/if}

<!-- Join Invite Modal -->
{#if showJoinModal}
  <div class="backdrop">
    <div class="modal">
      <h2>Join a Network</h2>
      <p class="subtitle">
        Paste a <span class="mono">lucid://join/…</span> invitation link from someone you trust. Your private key never leaves this device.
      </p>
      <input
        type="password"
        placeholder="Paste lucid://join/… invitation"
        bind:value={joinInput}
        onkeydown={(e) => e.key === "Enter" && handleJoinInvite()}
      />
      <div class="modal-actions">
        <button onclick={() => (showJoinModal = false)}>Cancel</button>
        <button class="primary" onclick={handleJoinInvite} disabled={busy || !joinInput.trim()}>
          Join Network
        </button>
      </div>
    </div>
  </div>
{/if}

<!-- Share Invite Modal -->
{#if showInviteModal && activeInvite}
  <div class="backdrop">
    <div class="modal">
      <h2>Share Network Invite</h2>
      <p class="subtitle">
        Send this one-time link to a device you trust. Clicking it opens LUCID directly. Valid for 30 minutes.
      </p>
      <div class="invite-box mono selectable">{activeInvite.invitation}</div>
      <div class="modal-actions">
        <button onclick={() => (showInviteModal = false)}>Done</button>
        <button
          class="primary"
          onclick={() => activeInvite && copyText(activeInvite.invitation, "invite link")}
        >
          Copy Invite Link
        </button>
      </div>
    </div>
  </div>
{/if}

{#snippet connectionList(peers: PeerEntry[] | null | undefined)}
  {#if !peers}
    <p class="row-help">Connection details unavailable.</p>
  {:else if peers.length === 0}
    <p class="row-help">No connected peers reported.</p>
  {:else}
    {@const orderedPeers = [...peers].sort(
      (a, b) => Number(b.authorized === true) - Number(a.authorized === true),
    )}
    <ul class="peer-list connection-list">
      {#each orderedPeers as peer (peer.peer_id)}
        <li class="peer-row">
          <span class="mono">{shortPeerId(peer.peer_id)}</span>
          <span class="peer-badges">
            <span class="chip" class:sharing={peer.authorized === true}>
              {#if peer.authorized === true}
                Circle authorized
              {:else if peer.authorized === false}
                Other connection
              {:else}
                Authorization unknown
              {/if}
            </span>
            <span class="chip">{connectionPathLabel(peer.path)}</span>
          </span>
        </li>
      {/each}
    </ul>
  {/if}
{/snippet}

<!-- Peers Modal -->
{#if showPeersModal && overview}
  <div class="backdrop">
    <div class="modal peers-modal" role="dialog" aria-modal="true" aria-labelledby="peers-title">
      <h2 id="peers-title">Peers</h2>
      <section class="card compact">
        <h3 class="row-label">Network enrollment</h3>
        <p class="row-help">{deviceEnrollment}.</p>
      </section>
      <section class="connection-section">
        <h3 class="row-label">
          Network connections{#if overview.peers} ({overview.peers.length}){/if}
        </h3>
        <p class="row-help">Active network path · {connectionPathLabel(overview.status.active_path)}</p>
        {@render connectionList(overview.peers)}
      </section>
      <div class="modal-actions">
        <button class="primary" onclick={() => (showPeersModal = false)}>Close</button>
      </div>
    </div>
  </div>
{/if}

<!-- Trusted-Circle Cluster Modal -->
{#if showClusterModal}
  <div class="backdrop">
    <div class="modal wide cluster-modal" role="dialog" aria-modal="true" aria-labelledby="cluster-title">
      <h2 id="cluster-title">Trusted-Circle Cluster</h2>
      <p class="subtitle">
        Tier 1 shares model activations with enrolled circle devices so they can run a model together.
        Everyone operating those devices must be fully trusted with the activations. Per llama.cpp's
        RPC security model, granting a peer access to your RPC bridge carries full machine-level trust
        comparable to SSH access—only enroll devices and operators you completely trust. Open-network
        sharding is disabled.
      </p>

      {#if clusterError}
        <p class="row-help" role="alert">{clusterError}</p>
      {/if}

      {#if clusterLoading}
        <p class="row-help" role="status">Loading cluster status…</p>
      {:else if clusterStatus}
        <section class="card compact">
          <div class="toggle-row">
            <div>
              <h3 class="row-label">Participate in Tier 1</h3>
              <p class="row-help">
                {#if clusterStatus.cluster_enabled === true}Enabled.{:else if clusterStatus.cluster_enabled === false}Disabled.{/if}
                {deviceEnrollment}.
              </p>
            </div>
            {#if typeof clusterStatus.cluster_enabled === "boolean"}
              <button
                class:primary={!clusterStatus.cluster_enabled}
                disabled={busy}
                onclick={handleToggleCluster}
              >
                {clusterStatus.cluster_enabled ? "Disable" : "Enable"}
              </button>
            {:else}
              <span class="chip">Setting unavailable</span>
            {/if}
          </div>

          {#if clusterStatus.trust_label === "trusted-circle-sharded" || clusterResponse?.response_trust_label}
            <div class="cluster-labels">
              {#if clusterStatus.trust_label === "trusted-circle-sharded"}
                <span>Cluster mode <span class="chip">{clusterStatus.trust_label}</span></span>
              {/if}
              {#if clusterResponse?.response_trust_label}
                <span>API response <span class="chip">{clusterResponse.response_trust_label}</span></span>
              {/if}
            </div>
          {/if}
        </section>

        <section class="card compact" aria-labelledby="cluster-memory-title">
          <h3 class="row-label" id="cluster-memory-title">Available memory</h3>
          <dl class="cluster-memory">
            <div>
              <dt>This device · reported capacity</dt>
              <dd>
                {clusterMemory === "—" ? "Unavailable" : clusterMemory}
                {#if clusterMemory !== "—" && clusterStatus.local_node?.backend}
                  <span class="chip">{clusterStatus.local_node.backend}</span>
                {/if}
              </dd>
            </div>
            <div>
              <dt>Pool across eligible devices</dt>
              <dd>Unavailable</dd>
            </div>
          </dl>
          <p class="row-help" id="cluster-preview-unavailable">
            Pooled memory and layer split preview need per-device capacity, which this daemon
            does not report.
          </p>
        </section>

        <section class="connection-section">
          <h3 class="row-label">Network connections</h3>
          {@render connectionList(clusterStatus.connected_peers)}
        </section>
      {/if}

      <div class="modal-actions">
        {#if clusterStatus}
          <button disabled aria-describedby="cluster-preview-unavailable">Preview Layer Split</button>
        {:else if !clusterLoading}
          <button onclick={loadClusterStatus}>Retry</button>
        {/if}
        <button class="primary" onclick={handleCloseCluster}>Done</button>
      </div>
    </div>
  </div>
{/if}

<!-- Built-in Local Chat Modal -->
{#if showChatModal}
  <div class="backdrop">
    <div class="modal wide">
      <ChatPanel models={chatModels} initialModel={chatInitialModel} />
      <div class="modal-actions">
        <button onclick={() => (showChatModal = false)}>Close</button>
      </div>
    </div>
  </div>
{/if}

<!-- Settings & Safety Modal -->
{#if showSettingsModal && overview}
  <div class="backdrop">
    <div class="modal wide">
      <h2>Settings & Safety</h2>

      <div class="card compact">
        <div class="toggle-row">
          <div>
            <div class="row-label">Pause all providing</div>
            <div class="row-help">Immediately refuse remote requests while keeping local chat working</div>
          </div>
          <button
            onclick={() =>
              handleTogglePolicy(
                { manual_pause: !overview?.status.policy.manual_pause },
                overview?.status.policy.manual_pause ? "Providing resumed" : "Providing paused"
              )}
          >
            {overview.status.policy.manual_pause ? "Resume" : "Pause"}
          </button>
        </div>

        <div class="divider"></div>

        <div class="toggle-row">
          <div>
            <div class="row-label">Auto-pause on battery</div>
            <div class="row-help">Protects laptop battery life automatically</div>
          </div>
          <button
            onclick={() =>
              handleTogglePolicy(
                { auto_pause_on_battery: !overview?.status.policy.auto_pause_on_battery },
                "Updated battery protection"
              )}
          >
            {overview.status.policy.auto_pause_on_battery ? "On" : "Off"}
          </button>
        </div>
      </div>

      <div class="field">
        <label for="chat-url">Chat App URL (Open WebUI)</label>
        <div class="inline-field">
          <input id="chat-url" type="text" bind:value={chatUrlDraft} />
          <button
            onclick={() =>
              handleTogglePolicy({ chat_app_url: chatUrlDraft }, "Saved chat app URL")}
          >
            Save
          </button>
        </div>
      </div>

      {#if overview.model_dirs.length > 0}
        <div class="field">
          <div class="section-title">Approved model folders</div>
          {#each overview.model_dirs as dir (dir)}
            <div class="peer-row">
              <span class="mono">{dir}</span>
              <button class="ghost" onclick={() => handleRemoveFolder(dir)}>Remove</button>
            </div>
          {/each}
        </div>
      {/if}

      <div class="modal-actions">
        <button class="primary" onclick={() => (showSettingsModal = false)}>Done</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .shell {
    display: flex;
    flex-direction: column;
    height: 100%;
  }

  .topbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 18px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-elevated);
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .brand-name {
    font-weight: 700;
    letter-spacing: 0.04em;
    font-size: 13px;
  }

  .dot {
    width: 9px;
    height: 9px;
    border-radius: 50%;
    background: var(--text-secondary);
  }

  .dot.green {
    background: var(--green);
  }

  .dot.amber {
    background: var(--amber);
  }

  .pill {
    font-size: 12px;
    color: var(--text-secondary);
    padding: 3px 9px;
    border-radius: 999px;
    background: var(--bg-subtle);
  }

  .actions {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 18px;
    font-size: 13px;
  }

  .banner.error {
    background: rgba(220, 38, 38, 0.12);
    color: var(--red);
    border-bottom: 1px solid rgba(220, 38, 38, 0.25);
  }

  .toast {
    position: fixed;
    bottom: 20px;
    right: 20px;
    background: var(--text);
    color: var(--bg);
    padding: 9px 14px;
    border-radius: 999px;
    font-size: 13px;
    font-weight: 500;
    box-shadow: var(--shadow);
    z-index: 60;
  }

  .workspace {
    display: grid;
    grid-template-columns: 290px 1fr;
    flex: 1;
    min-height: 0;
  }

  .sidebar {
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--border);
    background: var(--bg-sidebar);
    min-height: 0;
  }

  .sidebar-controls {
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    border-bottom: 1px solid var(--border);
  }

  .segmented {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    background: var(--bg-subtle);
    padding: 3px;
    border-radius: var(--radius-sm);
    gap: 2px;
  }

  .segmented button {
    border: none;
    background: transparent;
    padding: 5px 6px;
    font-size: 12px;
    border-radius: 7px;
  }

  .segmented button.active {
    background: var(--bg-elevated);
    box-shadow: 0 1px 3px rgba(0, 0, 0, 0.08);
  }

  .model-list {
    flex: 1;
    overflow-y: auto;
    padding: 8px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .model-row {
    text-align: left;
    border: 1px solid transparent;
    background: transparent;
    padding: 10px 11px;
    border-radius: 10px;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }

  .model-row.selected {
    background: var(--bg-elevated);
    border-color: var(--border);
    box-shadow: 0 1px 4px rgba(0, 0, 0, 0.05);
  }

  .row-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
  }

  .row-name {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .row-meta {
    font-size: 12px;
    color: var(--text-secondary);
    display: flex;
    gap: 5px;
  }

  .chip {
    font-size: 11px;
    padding: 2px 7px;
    border-radius: 999px;
    background: var(--bg-subtle);
    color: var(--text-secondary);
    flex-shrink: 0;
  }

  .chip.sharing {
    background: rgba(40, 167, 69, 0.16);
    color: var(--green);
  }

  .sidebar-footer {
    padding: 10px 12px;
    border-top: 1px solid var(--border);
  }

  .full-btn {
    width: 100%;
  }

  .empty-list {
    padding: 28px 14px;
    text-align: center;
    color: var(--text-secondary);
  }

  .empty-title {
    margin: 0 0 6px;
    font-weight: 600;
    color: var(--text);
  }

  .empty-sub {
    margin: 0;
    font-size: 12px;
  }

  .detail {
    overflow-y: auto;
    padding: 28px 34px;
  }

  .detail-inner {
    max-width: 680px;
    display: flex;
    flex-direction: column;
    gap: 18px;
  }

  h1 {
    margin: 0;
    font-size: 26px;
    letter-spacing: -0.02em;
  }

  h2 {
    margin: 0 0 8px;
    font-size: 19px;
  }

  .subtitle {
    margin: 4px 0 0;
    color: var(--text-secondary);
  }

  .card {
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 16px 18px;
    box-shadow: var(--shadow);
  }

  .card.compact {
    padding: 12px 14px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 13px;
    box-shadow: none;
  }

  .card.subtle {
    box-shadow: none;
    background: var(--bg-subtle);
  }

  .toggle-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
  }

  .row-label {
    font-weight: 600;
  }

  .row-help {
    font-size: 12.5px;
    color: var(--text-secondary);
    margin-top: 2px;
  }

  .divider {
    height: 1px;
    background: var(--border);
    margin: 14px 0;
  }

  .badge {
    font-size: 12px;
    font-weight: 600;
    padding: 4px 10px;
    border-radius: 999px;
    background: var(--bg-subtle);
    color: var(--text-secondary);
  }

  .badge.on {
    background: rgba(40, 167, 69, 0.16);
    color: var(--green);
  }

  .cta-row {
    display: flex;
    gap: 10px;
    flex-wrap: wrap;
  }

  .disclosure {
    background: transparent;
    border: none;
    padding: 0;
    font-size: 13px;
    color: var(--text-secondary);
  }

  .details-grid {
    margin-top: 10px;
    display: flex;
    flex-direction: column;
    gap: 5px;
    font-size: 12.5px;
    color: var(--text-secondary);
  }

  .privacy-note {
    margin: 4px 0 0;
    font-size: 12px;
    color: var(--text-secondary);
  }

  .welcome-empty {
    max-width: 520px;
    margin: 60px auto;
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  .probe-chip {
    display: inline-block;
    align-self: flex-start;
    font-size: 12px;
    color: var(--text-secondary);
    background: var(--bg-subtle);
    padding: 6px 12px;
    border-radius: 999px;
  }

  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.45);
    backdrop-filter: blur(6px);
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 20px;
    z-index: 40;
  }

  .modal {
    width: 100%;
    max-width: 460px;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: 16px;
    padding: 22px 24px;
    box-shadow: 0 18px 48px rgba(0, 0, 0, 0.25);
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  .modal.wide {
    max-width: 560px;
  }

  .cluster-modal,
  .peers-modal {
    max-height: 100%;
    overflow-y: auto;
  }

  .cluster-modal h3,
  .peers-modal h3,
  .cluster-modal .row-help,
  .peers-modal .row-help {
    margin: 0;
  }

  .connection-section,
  .cluster-labels,
  .cluster-memory {
    display: grid;
    gap: 8px;
  }

  .cluster-labels,
  .cluster-memory dt {
    color: var(--text-secondary);
    font-size: 12.5px;
  }

  .cluster-labels {
    grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
  }

  .cluster-labels > span,
  .cluster-memory > div {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 6px 12px;
  }

  .cluster-memory,
  .cluster-memory dd,
  .connection-list {
    margin: 0;
  }

  .cluster-memory dd {
    font-weight: 600;
  }

  .connection-list {
    list-style: none;
    padding: 0;
  }

  .peer-badges {
    display: flex;
    flex-wrap: wrap;
    justify-content: flex-end;
    gap: 6px;
  }

  .modal-actions {
    display: flex;
    justify-content: flex-end;
    gap: 10px;
    margin-top: 4px;
  }

  .section-title {
    font-size: 13px;
    font-weight: 600;
  }

  .check-list {
    max-height: 210px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .check-row {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px;
    border-radius: 9px;
    border: 1px solid var(--border);
    cursor: pointer;
  }

  .check-name {
    font-weight: 500;
    flex: 1;
  }

  .check-meta {
    font-size: 12px;
    color: var(--text-secondary);
  }

  .invite-box {
    background: var(--bg-subtle);
    padding: 10px 12px;
    border-radius: 9px;
    font-size: 12px;
    word-break: break-all;
    max-height: 110px;
    overflow-y: auto;
  }

  .peer-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-height: 240px;
    overflow-y: auto;
  }

  .peer-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 10px;
    padding: 8px 10px;
    border-radius: 9px;
    background: var(--bg-subtle);
    font-size: 12.5px;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .field label {
    font-size: 12.5px;
    font-weight: 600;
  }

  .inline-field {
    display: flex;
    gap: 8px;
  }
</style>
