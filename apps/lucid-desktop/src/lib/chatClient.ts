import { invoke } from "@tauri-apps/api/core";
import type { TrustLabel } from "./api";

export type ChatRole = "user" | "assistant";

export interface ChatRequestMessage {
  role: ChatRole;
  content: string;
}

export interface ChatReply {
  reply: string;
  routed_via: "local" | "peer" | "trusted-circle-sharded" | null;
  receipt_verified: boolean | null;
  trust_label: TrustLabel | null;
}

export function sendChatMessage(
  model: string,
  messages: ChatRequestMessage[],
  localOnly: boolean
): Promise<ChatReply> {
  return invoke<ChatReply>("send_chat_message", {
    model,
    messages,
    localOnly,
  });
}
