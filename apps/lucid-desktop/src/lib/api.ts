import { invoke } from "@tauri-apps/api/core";

export interface EnrollmentStatus {
  state: string;
  message: string;
}

export interface PolicySummary {
  manual_pause: boolean;
  auto_pause_on_battery: boolean;
  thermal_limit_c: number | null;
  max_concurrent_remote_jobs?: number;
}

export interface DaemonStatus {
  running: boolean;
  state: string;
  peer_count: number;
  authorized_peer_count?: number;
  active_path: string;
  model_count: number;
  api_address: string;
  chat_app_url: string;
  onboarding_completed: boolean;
  providing_enabled: boolean;
  providing_models: string[];
  paused_reason: string | null;
  enrollment: EnrollmentStatus | null;
  policy: PolicySummary;
}

export interface HardwareCapability {
  os: string;
  arch: string;
  cpu_cores: number;
  total_memory_bytes: number;
  available_memory_bytes: number;
  gpu_backend: string;
  gpu_name: string | null;
  gpu_vram_bytes: number | null;
  recommended_max_model_bytes: number;
  summary: string;
}

export interface ExistingInferenceStatus {
  ollama_running: boolean;
  ollama_version: string | null;
  ollama_store_found: boolean;
  lmstudio_store_found: boolean;
}

export interface RuntimeBinaryStatus {
  status: "ready" | "detected_unpinned" | "not_installed" | "tampered" | string;
  pinned_path: string | null;
  pinned_sha256: string | null;
  detected_path: string | null;
  downloadable_tag: string | null;
  downloadable_backend: string | null;
  downloadable_size_bytes: number | null;
}

export interface DiscoveredLocalModel {
  name: string;
  path: string;
  size_bytes: number;
  source: string;
  quantization: string | null;
  fits_hardware: boolean;
}

export interface SystemProbe {
  hardware: HardwareCapability;
  existing_inference: ExistingInferenceStatus;
  runtime: RuntimeBinaryStatus;
  discovered_models: DiscoveredLocalModel[];
  onboarding_completed: boolean;
  providing_enabled: boolean;
}

export interface ModelEntry {
  id: string;
  name: string;
  cid: string | null;
  source: string;
  size_bytes: number;
  quantization: string | null;
  peer_count: number;
  on_device: boolean;
  available_locally: boolean;
  providing: boolean;
  can_request: boolean;
  needs_restart?: boolean;
  fits_hardware: boolean;
  local_path: string | null;
}

export interface PeerEntry {
  peer_id: string;
  path?: string | null;
  authorized?: boolean;
}

export interface OverviewPayload {
  status: DaemonStatus;
  probe: SystemProbe | null;
  models: ModelEntry[];
  model_dirs: string[];
  peers: PeerEntry[] | null;
}

export interface ProvideResponse {
  providing_enabled: boolean;
  approved_provide_models: string[];
  runtime_ready: boolean;
  restart_required: boolean;
}

export interface InviteResponse {
  invitation: string;
  expires_at: number;
  grant_expires_at: number;
}

export interface ClusterNodeInfo {
  available_memory_bytes?: number | null;
  backend?: string | null;
}

export interface ClusterStatus {
  cluster_enabled?: boolean | null;
  trust_label?: string | null;
  local_node?: ClusterNodeInfo | null;
  connected_peers?: PeerEntry[] | null;
}

export type TrustLabel =
  | "local"
  | "remote-pending"
  | "remote-receipt-verified"
  | "remote-spot-checked"
  | "trusted-circle-sharded";

export interface ClusterStatusResponse {
  status: ClusterStatus;
  response_trust_label: TrustLabel | null;
}

export const lucidApi = {
  getOverview: () => invoke<OverviewPayload>("get_overview"),
  refreshProbe: () => invoke<SystemProbe>("refresh_probe"),
  setProvidePermission: (
    modelName: string,
    enable: boolean,
    confirmPermission: boolean,
    makeAvailableLocally?: boolean,
    localPath?: string | null
  ) =>
    invoke<ProvideResponse>("set_provide_permission", {
      modelName,
      enable,
      confirmPermission,
      makeAvailableLocally: makeAvailableLocally ?? null,
      localPath: localPath ?? null,
    }),
  pullModel: (model: string, cid?: string | null) =>
    invoke<{ status: string; model_id: string; cid: string; size_bytes: number }>("pull_model", {
      model,
      cid: cid ?? null,
    }),
  setupRuntime: (action: "pin_detected" | "download_pinned", confirmPermission: boolean) =>
    invoke<{ detail: string; runtime: RuntimeBinaryStatus }>("setup_runtime", {
      action,
      confirmPermission,
    }),
  addModelFolder: (path: string) => invoke<OverviewPayload>("add_model_folder", { path }),
  removeModelFolder: (path: string) => invoke<OverviewPayload>("remove_model_folder", { path }),
  updateSettings: (settings: {
    onboarding_completed?: boolean;
    manual_pause?: boolean;
    auto_pause_on_battery?: boolean;
    thermal_limit_c?: number | null;
    cluster_enabled?: boolean;
    chat_app_url?: string;
  }) => invoke<DaemonStatus>("update_settings", { settings }),
  createInvite: (ttlSeconds = 1800) => invoke<InviteResponse>("create_invite", { ttlSeconds }),
  joinInvite: (invitation: string) => invoke<EnrollmentStatus>("join_invite", { invitation }),
  openChat: (modelId: string) => invoke<void>("open_chat", { modelId }),
  restartDaemon: () => invoke<OverviewPayload>("restart_daemon"),
  getClusterStatus: () => invoke<ClusterStatusResponse>("get_cluster_status"),
};
