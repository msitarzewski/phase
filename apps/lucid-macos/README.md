# LUCID for macOS

Native SwiftUI control app for the existing `lucidd` daemon. Requires macOS 14
or newer and the Xcode command-line tools to build. It runs as the logged-in
user, starts only its own bundled consume-only daemon, and stops that process
when disconnecting or quitting. It never installs a privileged helper.

## Build and run

From the repository root:

```sh
bash apps/lucid-macos/build-app.sh
open target/lucid-macos/LUCID.app
```

The script reuses the existing Cargo binary and builds the native app with
SwiftPM. `--daemon /absolute/path/to/lucidd` uses an explicitly selected existing
binary, which must support outbound-only networking and the authenticated local
control API. The daemon is copied into the app, ad-hoc signed, checksummed and
checked against that checksum before each launch. Build output stays under
ignored `target/lucid-macos/` by default.

The output is a local development build. Public distribution still requires
Developer ID signing, notarization, and the project release-qualification gates.
The Swift and LUCID source is licensed AGPL-3.0-or-later; the bundled Phase
license covers the reused protocol substrate.

## Connect and choose a model

LUCID joins the daemon's configured foundation network on Connect. A fresh,
unavailable, or unauthorized network produces an honest empty model list; no
models or serving hosts are hardcoded in the app.

The native model list and Open WebUI use the same `/api/tags` catalog. Select a
model and choose **Chat with this model** to open the configured Open WebUI URL
with its documented `model` parameter. This selects a model without sending a
prompt in the URL. Other Ollama-compatible clients can use the copied local API
address directly. Their exact option compatibility must be qualified separately.

The default API address is `http://127.0.0.1:11435`, leaving Ollama's usual port
available. Set it as an Ollama connection in the chat app. Docker-based chat
clients need the project's qualified private local connection method; do not
publish LUCID's API on `0.0.0.0` to work around container networking.

### Qualified Open WebUI configuration

The local integration run used Open WebUI **0.12.3** on Docker Desktop for macOS
with the Ollama connection URL `http://host.docker.internal:11435`. The native
daemon remained bound to loopback. This connection was checked on Docker Desktop;
other container runtimes and operating systems need their own qualification.

Set the model's **Function Calling** to **Legacy** and disable native/built-in
tools for the chat. Keep the maximum output tokens (`max_tokens`/Ollama
`num_predict`) within the contributor's operator limit. LUCID currently rejects
tool calls, structured output (`format`), and `keep_alive`; clients must omit
those request fields. The integration confirmed remote dispatch and a completion
receipt; model answer quality is a separate model/runtime qualification.

## Network access and local state

Use **Join a network** to paste an operator-issued invitation or import a
`.lucidinvite`/JSON file. The invitation is passed opaquely to the daemon's
authenticated enrollment API, then removed from app memory. It does not bypass
contributor policy or enable unauthenticated execution.

Identity and daemon state live privately in `~/Library/Application Support/LUCID/`.
A fresh 64-hex control-token file is created for each owned daemon session; it is
read only for authenticated local control requests and removed after shutdown.
Neither the token nor the invitation is copied into chat-app URLs or logged by
the native app. Diagnostics are local and may contain internal peer addresses.

The app disables HTTP redirects and proxy inheritance for its loopback client,
refuses privileged/invalid API ports, and never adopts or stops an unrelated
daemon. Connection, model listing, enrollment and UI all reuse the production
daemon; no separate discovery or inference implementation is embedded here.

For isolated acceptance runs or a packaged private network, the launching process
may set `LUCID_RELAY_PEERS` to a JSON array of at most four direct, peer-pinned
relay multiaddrs (512 bytes each). This disables foundation bootstrap and passes
the explicit relays to the existing daemon. `LUCID_APP_STATE_DIR` selects an
absolute private state directory and keeps QA preference edits out of the normal
app preferences; `LUCID_CHAT_URL` selects the isolated chat client. These settings
are validated, never shown in the ordinary model UI, and do not enable inbound
listeners. An explicitly empty relay array creates an offline acceptance profile.

## Checks

```sh
swift test --package-path apps/lucid-macos --scratch-path target/lucid-macos/swift-build
```

Tests cover loopback endpoint construction, model-list decoding without invented
metadata, safe model-selection links, invitation size limits, and private token
file ownership/permissions. Native app launch and end-to-end Open WebUI chat
remain separate acceptance checks.

Open WebUI's model-selection link contract:
<https://docs.openwebui.com/features/chat-conversations/chat-features/url-params/>
