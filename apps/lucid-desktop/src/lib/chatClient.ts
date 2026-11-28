import { invoke } from "@tauri-apps/api/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import type { TrustLabel } from "./api";

export type ChatRole = "user" | "assistant";

export interface ChatRequestMessage {
  role: ChatRole;
  content: string;
}

export interface ChatEvidence {
  routed_via: "local" | "peer" | "trusted-circle-sharded" | null;
  receipt_verified: boolean | null;
  trust_label: TrustLabel | null;
}

interface ChatChunk {
  request_id: string;
  delta: string;
  evidence: ChatEvidence | null;
}

interface ChatDone {
  request_id: string;
  cancelled: boolean;
  evidence: ChatEvidence | null;
}

interface ChatError {
  request_id: string;
  message: string;
}

function newRequestId() {
  return Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) =>
    byte.toString(16).padStart(2, "0"),
  ).join("");
}

export async function streamChatMessage(
  model: string,
  messages: ChatRequestMessage[],
  localOnly: boolean,
  signal: AbortSignal,
  onChunk: (chunk: ChatChunk) => void,
): Promise<ChatDone> {
  const requestId = newRequestId();
  const window = getCurrentWebviewWindow();
  const unlisten: UnlistenFn[] = [];
  let finish!: (result: ChatDone | ChatError) => void;
  const terminal = new Promise<ChatDone | ChatError>((resolve) => {
    finish = resolve;
  });
  const cancel = () => {
    void invoke("cancel_chat_message", { requestId }).catch(() => {
      finish({ request_id: requestId, message: "Couldn’t stop the reply. Try again." });
    });
  };

  try {
    // Subscribe before starting so even an immediate response reaches this turn.
    unlisten.push(await window.listen<ChatChunk>("lucid://chat-chunk", ({ payload }) => {
      if (payload.request_id === requestId) onChunk(payload);
    }));
    unlisten.push(await window.listen<ChatDone>("lucid://chat-done", ({ payload }) => {
      if (payload.request_id === requestId) finish(payload);
    }));
    unlisten.push(await window.listen<ChatError>("lucid://chat-error", ({ payload }) => {
      if (payload.request_id === requestId) finish(payload);
    }));
    if (signal.aborted) {
      return { request_id: requestId, cancelled: true, evidence: null };
    }

    await invoke("send_chat_message", { requestId, model, messages, localOnly });
    // Wait for the native command to register the request before asking it to stop.
    signal.addEventListener("abort", cancel, { once: true });
    if (signal.aborted) cancel();

    const result = await terminal;
    if ("message" in result) throw new Error(result.message);
    return result;
  } finally {
    signal.removeEventListener("abort", cancel);
    for (const remove of unlisten) remove();
  }
}
