<script lang="ts">
  import {
    sendChatMessage,
    type ChatReply,
    type ChatRequestMessage,
  } from "./chatClient";

  interface ChatMessage extends ChatRequestMessage {
    evidence?: Pick<
      ChatReply,
      "routed_via" | "receipt_verified" | "trust_label"
    >;
  }

  interface Props {
    models: string[];
    initialModel?: string | null;
  }

  let { models, initialModel = null }: Props = $props();
  let selectedModel = $state("");
  let initializedModel = $state(false);
  let messages = $state<ChatMessage[]>([]);
  let draft = $state("");
  let localOnly = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    if (models.length === 0) return;

    if (!initializedModel) {
      selectedModel =
        initialModel && models.includes(initialModel)
          ? initialModel
          : (models[0] ?? "");
      initializedModel = true;
    } else if (selectedModel && !models.includes(selectedModel)) {
      selectedModel = models[0] ?? "";
    } else if (!selectedModel && models.length > 0) {
      selectedModel = models[0];
    }
  });

  async function send() {
    const content = draft.trim();
    if (!content || !selectedModel || busy) return;

    busy = true;
    error = null;
    messages = [...messages, { role: "user", content }];
    draft = "";

    const requestMessages = messages.map(({ role, content: message }) => ({
      role,
      content: message,
    }));

    try {
      const reply = await sendChatMessage(selectedModel, requestMessages, localOnly);
      messages = [
        ...messages,
        {
          role: "assistant",
          content: reply.reply,
          evidence: {
            routed_via: reply.routed_via,
            receipt_verified: reply.receipt_verified,
            trust_label: reply.trust_label,
          },
        },
      ];
    } catch (cause) {
      error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      busy = false;
    }
  }

  function handleKeydown(event: KeyboardEvent) {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void send();
    }
  }

  function clearHistory() {
    messages = [];
    error = null;
  }
</script>

<section class="chat-panel" aria-label="LUCID chat">
  <header class="chat-header">
    <div>
      <h2>Chat</h2>
      <p class="subtitle">Conversation history stays in memory for this session only.</p>
    </div>
    <button class="ghost" onclick={clearHistory} disabled={busy || messages.length === 0}>
      Clear
    </button>
  </header>

  <div class="chat-controls">
    <label>
      <span>Model</span>
      <select bind:value={selectedModel} disabled={busy || models.length === 0}>
        {#if models.length === 0}
          <option value="">No models available</option>
        {:else}
          {#each models as model}
            <option value={model}>{model}</option>
          {/each}
        {/if}
      </select>
    </label>

    <label class="toggle">
      <input type="checkbox" bind:checked={localOnly} disabled={busy} />
      <span>Local Only</span>
    </label>
  </div>

  {#if error}
    <div class="error-banner" role="alert">{error}</div>
  {/if}

  <div class="messages" aria-live="polite">
    {#if messages.length === 0}
      <div class="empty-state">
        {#if localOnly}
          <strong>Local-only chat</strong>
          <span>Requests stay strictly on this device (X-Lucid-Local-Only). History is kept in memory only.</span>
        {:else}
          <strong>Start a chat</strong>
          <span>History is kept in memory only and sent with each turn to whichever local or enrolled circle device serves the selected model. Check "Local Only" to keep prompts on this device.</span>
        {/if}
      </div>
    {:else}
      {#each messages as message}
        <article class:user={message.role === "user"} class="message">
          <div class="message-role">{message.role === "user" ? "You" : "Assistant"}</div>
          <div class="message-content">{message.content}</div>
          {#if message.role === "assistant" && message.evidence}
            <div class="evidence" aria-label="Response provenance">
              {#if message.evidence.routed_via}
                <span class="badge">Route: {message.evidence.routed_via}</span>
              {/if}
              {#if message.evidence.trust_label}
                <span class="badge">Trust: {message.evidence.trust_label}</span>
              {/if}
              {#if message.evidence.receipt_verified !== null}
                <span
                  class="badge"
                  class:verified={message.evidence.receipt_verified}
                  class:unverified={!message.evidence.receipt_verified}
                >
                  Receipt: {message.evidence.receipt_verified ? "verified" : "unverified"}
                </span>
              {/if}
            </div>
          {/if}
        </article>
      {/each}
    {/if}
  </div>

  <div class="composer">
    <textarea
      aria-label="Message"
      rows="3"
      placeholder="Message the selected model…"
      bind:value={draft}
      onkeydown={handleKeydown}
      disabled={busy || !selectedModel}
    ></textarea>
    <button class="primary" onclick={send} disabled={busy || !draft.trim() || !selectedModel}>
      {busy ? "Sending…" : "Send"}
    </button>
  </div>
</section>

<style>
  .chat-panel {
    display: flex;
    min-height: 480px;
    max-height: 100%;
    flex-direction: column;
    overflow: hidden;
    background: var(--bg-elevated);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: var(--shadow);
  }

  .chat-header,
  .chat-controls,
  .composer {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 14px 16px;
  }

  .chat-header {
    justify-content: space-between;
    border-bottom: 1px solid var(--border);
  }

  h2 {
    margin: 0;
    font-size: 19px;
  }

  .subtitle {
    margin: 4px 0 0;
    font-size: 12.5px;
    color: var(--text-secondary);
  }

  .chat-controls {
    flex-wrap: wrap;
    border-bottom: 1px solid var(--border);
    background: var(--bg-subtle);
  }

  .chat-controls label {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
  }

  select {
    min-width: 220px;
    max-width: min(420px, 60vw);
  }

  .toggle {
    margin-left: auto;
  }

  .toggle input {
    width: auto;
  }

  .error-banner {
    padding: 10px 16px;
    color: var(--red);
    background: rgba(220, 38, 38, 0.12);
    border-bottom: 1px solid rgba(220, 38, 38, 0.25);
    font-size: 13px;
  }

  .messages {
    display: flex;
    flex: 1;
    min-height: 0;
    flex-direction: column;
    gap: 12px;
    overflow-y: auto;
    padding: 18px;
  }

  .empty-state {
    display: flex;
    flex: 1;
    min-height: 180px;
    align-items: center;
    justify-content: center;
    flex-direction: column;
    gap: 4px;
    color: var(--text-secondary);
  }

  .empty-state strong {
    color: var(--text);
  }

  .message {
    max-width: 86%;
    align-self: flex-start;
    padding: 11px 13px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-subtle);
  }

  .message.user {
    align-self: flex-end;
    background: var(--bg-elevated);
  }

  .message-role {
    margin-bottom: 5px;
    font-size: 11px;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--text-secondary);
  }

  .message-content {
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    font-size: 14px;
    line-height: 1.5;
  }

  .evidence {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 9px;
  }

  .badge {
    padding: 3px 8px;
    border-radius: 999px;
    background: var(--bg-elevated);
    color: var(--text-secondary);
    font-size: 11px;
    font-weight: 600;
  }

  .badge.verified {
    color: var(--green);
    background: rgba(40, 167, 69, 0.16);
  }

  .badge.unverified {
    color: var(--amber);
    background: rgba(217, 119, 6, 0.16);
  }

  .composer {
    align-items: flex-end;
    border-top: 1px solid var(--border);
  }

  textarea {
    flex: 1;
    min-height: 70px;
    resize: vertical;
  }

  @media (max-width: 640px) {
    .chat-controls label,
    select {
      width: 100%;
      max-width: none;
    }

    .toggle {
      margin-left: 0;
    }

    .composer {
      align-items: stretch;
      flex-direction: column;
    }
  }
</style>
