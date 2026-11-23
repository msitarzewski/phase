export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes <= 0) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let idx = 0;
  while (value >= 1024 && idx < units.length - 1) {
    value /= 1024;
    idx += 1;
  }
  return `${value >= 10 || idx === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[idx]}`;
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
  pausedReason: string | null
): string {
  if (pausedReason === "battery") return "Paused · On battery";
  if (pausedReason === "thermal") return "Paused · Cooling down";
  if (pausedReason === "manual_pause") return "Providing paused";
  if (state === "connected") {
    return peerCount === 1 ? "Connected · 1 peer" : `Connected · ${peerCount} peers`;
  }
  if (state === "connecting") return "Connecting…";
  if (state === "starting") return "Starting…";
  return "Ready · No peers yet";
}
