export function formatBytes(bytes: number, binaryUnits = false): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "—";
  const units = binaryUnits ? ["B", "KiB", "MiB", "GiB", "TiB"] : ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let idx = 0;
  while (value >= 1024 && idx < units.length - 1) {
    value /= 1024;
    idx += 1;
  }
  return `${value >= 10 || idx === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[idx]}`;
}

export function enrollmentLabel(state?: string | null): string {
  switch (state) {
    case "authorized":
      return "This device has an active enrollment";
    case "pending":
      return "This device is waiting for enrollment approval";
    case "idle":
      return "This device has no active enrollment";
    case "failed":
      return "Enrollment could not be confirmed for this device";
    default:
      return "Enrollment status unavailable";
  }
}

export function connectionPathLabel(path?: string | null): string {
  switch (path) {
    case "direct":
      return "Direct";
    case "relayed":
      return "Via relay";
    case "dcutr":
      return "Direct via hole punching";
    default:
      return "Path unavailable";
  }
}

export function shortPeerId(peerId: string): string {
  return peerId.length > 25 ? `${peerId.slice(0, 18)}…${peerId.slice(-6)}` : peerId;
}

export function sourceLabel(source: string): string {
  switch (source) {
    case "ollama":
      return "Ollama (on this device)";
    case "lmstudio":
      return "LM Studio (on this device)";
    case "local":
      return "On this device";
    case "network":
      return "From network";
    default:
      return source;
  }
}

export function statusPillText(
  state: string,
  peerCount: number,
  pausedReason: string | null,
  authorizedPeerCount?: number
): string {
  if (pausedReason === "battery") return "Paused · On battery";
  if (pausedReason === "thermal") return "Paused · Cooling down";
  if (pausedReason === "manual_pause") return "Providing paused";
  if (state === "connected") {
    if (typeof authorizedPeerCount === "number") {
      if (authorizedPeerCount > 0) {
        return authorizedPeerCount === 1
          ? "Connected · 1 authorized"
          : `Connected · ${authorizedPeerCount} authorized`;
      }
      return peerCount === 1
        ? "Connected · 1 network (0 authorized)"
        : `Connected · ${peerCount} network (0 authorized)`;
    }
    return peerCount === 1 ? "Connected · 1 peer" : `Connected · ${peerCount} peers`;
  }
  if (state === "connecting") return "Connecting…";
  if (state === "starting") return "Starting…";
  return "Ready · No peers yet";
}
