# Your first chat and joining a circle

LUCID starts in **consume-only** mode by default: it never shares your GPU or uploads model files unless you explicitly enable sharing. You can chat locally on your own models, join a friend's private circle with a one-time link, or connect any Ollama-compatible app.

## Using the built-in Desktop Chat

1. Open **LUCID** and complete the brief onboarding screen (you can click **Start Privately (Don't Share)** to stay consume-only).
2. Pick any model in the left sidebar—either an **On Device** model or a model shared by an authorized circle peer—and click **Chat**.
3. Type a message and press Send. Tokens stream live as they arrive.

What the chat panel guarantees:

- **Memory-only history:** Conversations live only in the window's in-memory state. Nothing is written to disk or `localStorage`, and clicking **Clear** resets the conversation immediately.
- **Stop cancels upstream:** Clicking **Stop** cancels the in-flight request and closes the upstream HTTP stream to `lucidd` (and any remote peer stream). Stopped or interrupted replies remain visible on screen as partial output and are excluded from future prompt context.
- **Copy:** One-click copy on any assistant reply.
- **Local Only toggle:** Sends `X-Lucid-Local-Only: true`. `lucidd` refuses remote peer routing *and* refuses local RPC-offloaded models when Local Only is checked.

## Reading trust badges

Every reply displays three provenance badges derived directly from verified HTTP headers and terminal NDJSON evidence (the Desktop Rust backend rejects unearned badges):

| Badge (`X-Lucid-Trust-Label`) | Meaning |
|---|---|
| `local` | Executed entirely on your own machine. |
| `remote-pending` | Streaming from an invited peer; terminal Ed25519 receipt has not been checked yet. |
| `remote-receipt-verified` | Stream completed and the peer's Ed25519 `SignedReceipt` matched the job manifest, delivering `PeerId`, and replayed SHA-256 token commitment. |
| `remote-spot-checked` | Verified receipt **plus** a deterministic redundant spot-check peer returned an agreeing commitment. |
| `trusted-circle-sharded` | Executed across your own opted-in Tier-1 Trusted-Circle GPUs. |

> [!NOTE]
> A verified receipt proves *which peer* generated the exact tokens you received and that they were not modified in transit. It does not hide your prompt from the serving peer or prove the peer's hidden weight quantization. See [Cryptography, receipts and verification](cryptography-and-receipts.md).

## Joining a trusted circle

Serving nodes are **default-deny**: uninvited peers cannot run jobs on your GPU. Circle access works through single-use `lucid://join/...` invitations.

### On the sharing machine (create an invite)

- **Desktop app:** Click **Share Invite** to mint a one-time link (default 30-minute expiry, configurable up to 1 hour).
- **CLI:**
  ```bash
  lucidd invite --ttl-seconds 1800
  ```

Each invitation embeds a signed descriptor and an ephemeral delegation seed (which is never stored in the ledger or published to the DHT). Invitations are single-use and grant up to 24 hours of circle authorization by default.

### On your machine (accept an invite)

- **Click a link or `.lucidinvite` file:** LUCID Desktop registers the `lucid://` URL scheme and `.lucidinvite` file association—clicking either opens the **Join Invite** dialog pre-filled.
- **Paste manually in Desktop:** Click **Join Invite** (the input field is masked like a password) and confirm.
- **CLI:**
  ```bash
  lucidd join "lucid://join/..."
  ```

Behind the scenes, your node publishes a doubly signed `EnrollmentRequest` to the issuer's DHT mailbox and polls for a signed `Approval` (typically completing in 3–6 seconds). Once approved, the issuer's models appear in your sidebar and `/api/tags`.

## Connecting Ollama-compatible clients

Point any Ollama-compatible tool (Open WebUI, Continue, Zed, LangChain, `curl`) at LUCID's loopback HTTP address:

- **Standalone `lucidd` CLI:** `http://127.0.0.1:11434`
- **LUCID Desktop sidecar:** `http://127.0.0.1:11435` (click **Copy API Address** in the model detail pane, or click **Open WebUI ↗** after configuring its URL in Settings)

Example streaming chat via `curl`:

```bash
curl -s http://127.0.0.1:11434/api/chat \
  -H "Content-Type: application/json" \
  -d '{
    "model": "my-model",
    "messages": [{"role": "user", "content": "Say hello in one sentence."}]
  }'
```

Force local-only execution from any client by adding `-H "X-Lucid-Local-Only: true"`. See the complete [HTTP API reference](http-api.md) for all endpoints, parameters, and headers.
