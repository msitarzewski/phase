// SPDX-License-Identifier: AGPL-3.0-or-later

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, CONTENT_TYPE, HOST};
use reqwest::{Response, StatusCode};
use serde::{Deserialize, Serialize};
use tauri::{Emitter, EventTarget, Manager, State, WebviewWindow};
use tokio::sync::oneshot;
use url::Url;
use uuid::Uuid;

use super::{response_trust_label, AppController};

const CHAT_PATH: &str = "/api/chat";
const CHAT_CHUNK: &str = "lucid://chat-chunk";
const CHAT_DONE: &str = "lucid://chat-done";
const CHAT_ERROR: &str = "lucid://chat-error";
const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;
const MAX_ERROR_BYTES: usize = 4096;
const INTERRUPTED: &str = "The reply was interrupted before it finished. Try again.";
const INVALID_STREAM: &str = "The LUCID service returned an invalid chat stream.";
const OFFLINE_PEER: &str =
    "The model’s device may be offline. Ask its owner to check LUCID, or choose another model.";
const NO_LOCAL_MODEL: &str =
    "This model can’t run on this device with Local Only on. Choose a local model or add one in Models.";

type ChatResult<T> = std::result::Result<T, &'static str>;

pub(super) struct ActiveChat {
    id: Uuid,
    window: String,
    cancel: Option<oneshot::Sender<()>>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
struct ChatEvidence {
    routed_via: Option<&'static str>,
    receipt_verified: Option<bool>,
    trust_label: Option<&'static str>,
}

#[derive(Clone, Serialize)]
struct ChatChunk<'a> {
    request_id: &'a str,
    delta: &'a str,
    evidence: Option<ChatEvidence>,
}

#[derive(Clone, Serialize)]
struct ChatDone<'a> {
    request_id: &'a str,
    cancelled: bool,
    evidence: Option<ChatEvidence>,
}

#[derive(Clone, Serialize)]
struct ChatError<'a> {
    request_id: &'a str,
    message: &'static str,
}

#[derive(Deserialize)]
struct ChatFrame {
    done: Option<bool>,
    done_reason: Option<String>,
    message: Option<ChatMessage>,
    error: Option<String>,
    x_lucid_trust_label: Option<String>,
    x_lucid_receipt_verified: Option<String>,
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(value)
}

fn response_routed_via(headers: &HeaderMap) -> Option<&'static str> {
    match single_header(headers, "x-lucid-routed-via")? {
        "local" => Some("local"),
        "peer" => Some("peer"),
        "trusted-circle-sharded" => Some("trusted-circle-sharded"),
        _ => None,
    }
}

fn validate_loopback_chat_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).context("invalid local chat URL")?;
    let port = url
        .port_or_known_default()
        .filter(|port| *port != 0)
        .context("missing local chat port")?;
    if raw != format!("http://127.0.0.1:{port}{CHAT_PATH}") {
        bail!("refusing non-loopback local chat URL");
    }
    Ok(url)
}

fn loopback_chat_url(port: u16) -> Result<Url> {
    validate_loopback_chat_url(&format!("http://127.0.0.1:{port}{CHAT_PATH}"))
}

#[tauri::command]
pub async fn send_chat_message(
    window: WebviewWindow,
    ctrl: State<'_, Arc<AppController>>,
    request_id: String,
    model: String,
    messages: Vec<ChatMessage>,
    local_only: bool,
) -> Result<(), String> {
    let id = Uuid::parse_str(&request_id).map_err(|_| "Invalid chat request ID.")?;
    let (cancel, cancelled) = oneshot::channel();
    let mut active = ctrl.active_chat.lock().await;
    if active.is_some() {
        return Err("A reply is already in progress.".to_string());
    }
    *active = Some(ActiveChat {
        id,
        window: window.label().to_string(),
        cancel: Some(cancel),
    });
    drop(active);

    let ctrl = ctrl.inner().clone();
    tauri::async_runtime::spawn(async move {
        let target = EventTarget::webview_window(window.label());
        let result = until_cancelled(cancelled, async {
            let (port, _) = ctrl
                .ensure_started(window.app_handle())
                .await
                .map_err(|_| {
                    "Couldn’t start the local LUCID service. Reopen LUCID and try again."
                })?;
            stream_chat(
                &ctrl.http,
                port,
                model,
                messages,
                local_only,
                |delta, evidence| {
                    window
                        .emit_to(
                            target.clone(),
                            CHAT_CHUNK,
                            ChatChunk {
                                request_id: &request_id,
                                delta,
                                evidence,
                            },
                        )
                        .map_err(|_| INTERRUPTED)
                },
            )
            .await
        })
        .await;

        *ctrl.active_chat.lock().await = None;
        match result {
            Some(Err(message)) => {
                let _ = window.emit_to(
                    target,
                    CHAT_ERROR,
                    ChatError {
                        request_id: &request_id,
                        message,
                    },
                );
            }
            result => {
                let _ = window.emit_to(
                    target,
                    CHAT_DONE,
                    ChatDone {
                        request_id: &request_id,
                        cancelled: result.is_none(),
                        evidence: result.and_then(std::result::Result::ok),
                    },
                );
            }
        }
    });
    Ok(())
}

#[tauri::command]
pub async fn cancel_chat_message(
    window: WebviewWindow,
    ctrl: State<'_, Arc<AppController>>,
    request_id: String,
) -> Result<(), String> {
    let id = Uuid::parse_str(&request_id).map_err(|_| "Invalid chat request ID.")?;
    let mut active = ctrl.active_chat.lock().await;
    if let Some(cancel) = active
        .as_mut()
        .filter(|active| active.id == id && active.window == window.label())
        .and_then(|active| active.cancel.take())
    {
        let _ = cancel.send(());
    }
    Ok(())
}

/// Returning on cancellation drops the request or response future before we
/// report completion to the WebView.
async fn until_cancelled<T>(
    mut cancelled: oneshot::Receiver<()>,
    stream: impl Future<Output = T>,
) -> Option<T> {
    tokio::select! {
        biased;
        _ = &mut cancelled => None,
        result = stream => Some(result),
    }
}

async fn stream_chat(
    client: &reqwest::Client,
    port: u16,
    model: String,
    messages: Vec<ChatMessage>,
    local_only: bool,
    on_chunk: impl FnMut(&str, Option<ChatEvidence>) -> ChatResult<()> + Send,
) -> ChatResult<ChatEvidence> {
    let url = loopback_chat_url(port).map_err(|_| "Invalid local LUCID chat endpoint.")?;
    let mut request = client
        .post(url)
        .header(HOST, format!("127.0.0.1:{port}"))
        .json(&serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": true,
        }))
        // Per-request override. Control API requests keep the shorter client timeout.
        .timeout(Duration::from_secs(5 * 60));
    if local_only {
        request = request.header("X-Lucid-Local-Only", "true");
    }

    let response = request.send().await.map_err(|error| {
        if error.is_timeout() && !local_only {
            OFFLINE_PEER
        } else {
            "Couldn’t get a reply from the local LUCID service. Try again."
        }
    })?;
    if !response.status().is_success() {
        return Err(chat_refusal(response, local_only).await);
    }
    read_chat_stream(response, on_chunk).await
}

async fn chat_refusal(mut response: Response, local_only: bool) -> &'static str {
    let status = response.status();
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if chunk.len() > MAX_ERROR_BYTES - body.len() {
            break;
        }
        body.extend_from_slice(&chunk);
    }
    let body = String::from_utf8_lossy(&body);
    if local_only
        && status == StatusCode::SERVICE_UNAVAILABLE
        && body.starts_with("router refused: local-only requested")
    {
        NO_LOCAL_MODEL
    } else if status == StatusCode::SERVICE_UNAVAILABLE
        && (body.starts_with("router refused: no peers serving model")
            || body.starts_with("router refused: no authorized peers serving model"))
        || status == StatusCode::INTERNAL_SERVER_ERROR
            && body.starts_with("Network model request failed.")
    {
        OFFLINE_PEER
    } else if body.starts_with("Access is not authorized.") {
        "This device doesn’t have access to the model. Ask its owner for a valid invitation."
    } else {
        "LUCID couldn’t start this reply. Check the selected model and try again."
    }
}

async fn read_chat_stream(
    mut response: Response,
    mut on_chunk: impl FnMut(&str, Option<ChatEvidence>) -> ChatResult<()>,
) -> ChatResult<ChatEvidence> {
    let headers = response.headers();
    let media_type = single_header(headers, CONTENT_TYPE.as_str())
        .and_then(|value| value.split(';').next())
        .map(str::trim);
    if media_type != Some("application/x-ndjson") {
        return Err(INVALID_STREAM);
    }
    let routed_via = response_routed_via(headers);
    let initial = ChatEvidence {
        routed_via,
        trust_label: match response_trust_label(headers) {
            label @ (Some("local") | Some("remote-pending")) => label,
            _ => None,
        },
        // Only the terminal frame can report a verified receipt.
        receipt_verified: None,
    };
    on_chunk("", Some(initial))?;

    let mut pending = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| INTERRUPTED)? {
        for part in chunk.split_inclusive(|byte| *byte == b'\n') {
            if part.len() > MAX_FRAME_BYTES - pending.len() {
                return Err(INVALID_STREAM);
            }
            pending.extend_from_slice(part);
            if part.ends_with(b"\n") {
                if let Some(final_evidence) = apply_frame(&pending, routed_via, &mut on_chunk)? {
                    return Ok(final_evidence);
                }
                pending.clear();
            }
        }
    }
    apply_frame(&pending, routed_via, &mut on_chunk)?.ok_or(INTERRUPTED)
}

fn apply_frame(
    line: &[u8],
    routed_via: Option<&'static str>,
    on_chunk: &mut impl FnMut(&str, Option<ChatEvidence>) -> ChatResult<()>,
) -> ChatResult<Option<ChatEvidence>> {
    if line.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    let frame: ChatFrame = serde_json::from_slice(line).map_err(|_| INVALID_STREAM)?;
    if frame.error.is_some() || matches!(frame.done_reason.as_deref(), Some("error" | "cancelled"))
    {
        return Err(if routed_via == Some("peer") {
            "The remote reply couldn’t be completed or verified. Check the model’s device and try again."
        } else {
            INTERRUPTED
        });
    }
    let done = frame.done.ok_or(INVALID_STREAM)?;
    if let Some(message) = frame.message {
        if message.role != "assistant" {
            return Err(INVALID_STREAM);
        }
        if !message.content.is_empty() {
            on_chunk(&message.content, None)?;
        }
    } else if !done {
        return Err(INVALID_STREAM);
    }

    Ok(done.then(|| {
        let receipt_verified = match frame.x_lucid_receipt_verified.as_deref() {
            Some("true") => Some(true),
            Some("false") => Some(false),
            _ => None,
        };
        let trust_label = match super::parse_trust_label(
            frame.x_lucid_trust_label.as_deref().unwrap_or_default(),
        ) {
            Some("local") if routed_via == Some("local") => Some("local"),
            Some("trusted-circle-sharded") if routed_via == Some("trusted-circle-sharded") => {
                Some("trusted-circle-sharded")
            }
            label @ (Some("remote-receipt-verified") | Some("remote-spot-checked"))
                if routed_via == Some("peer") && receipt_verified == Some(true) =>
            {
                label
            }
            _ => None,
        };
        ChatEvidence {
            routed_via,
            receipt_verified,
            trust_label,
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn chat_url_accepts_only_explicit_loopback_endpoint() {
        let url = validate_loopback_chat_url("http://127.0.0.1:11435/api/chat").unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:11435/api/chat");
        assert!(validate_loopback_chat_url("http://127.0.0.1:80/api/chat").is_ok());

        for invalid in [
            "https://127.0.0.1:11435/api/chat",
            "http://localhost:11435/api/chat",
            "http://127.0.0.2:11435/api/chat",
            "http://2130706433:11435/api/chat",
            "http://127.0.0.1:0/api/chat",
            "http://203.0.113.10:11435/api/chat",
            "http://127.0.0.1:11435/api/generate",
            "http://127.0.0.1:11435/other/../api/chat",
            "http://127.0.0.1:11435/api/chat#fragment",
            "http://127.0.0.1:11435/api/chat?stream=true",
            "http://user@127.0.0.1:11435/api/chat",
            "http://127.0.0.1/api/chat",
        ] {
            assert!(
                validate_loopback_chat_url(invalid).is_err(),
                "accepted invalid URL: {invalid}"
            );
        }
    }

    #[test]
    fn chat_headers_require_single_allowlisted_values() {
        let mut headers = HeaderMap::new();
        headers.insert("x-lucid-routed-via", HeaderValue::from_static("peer"));
        headers.insert("x-lucid-trust-label", HeaderValue::from_static("local"));
        assert_eq!(response_routed_via(&headers).as_deref(), Some("peer"));
        assert_eq!(response_trust_label(&headers), Some("local"));

        headers.append("x-lucid-routed-via", HeaderValue::from_static("local"));
        headers.append(
            "x-lucid-trust-label",
            HeaderValue::from_static("remote-receipt-verified"),
        );
        assert_eq!(response_routed_via(&headers), None);
        assert_eq!(response_trust_label(&headers), None);

        headers.insert("x-lucid-routed-via", HeaderValue::from_static("unknown"));
        headers.insert("x-lucid-trust-label", HeaderValue::from_static("unknown"));
        assert_eq!(response_routed_via(&headers), None);
        assert_eq!(response_trust_label(&headers), None);
    }

    async fn reply_to(
        listener: tokio::net::TcpListener,
        response_head: &[u8],
        local_only: bool,
    ) -> tokio::net::TcpStream {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let port = listener.local_addr().unwrap().port();
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let body_start = loop {
            assert_ne!(socket.read_buf(&mut request).await.unwrap(), 0);
            if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break end + 4;
            }
        };
        let header = std::str::from_utf8(&request[..body_start]).unwrap();
        assert!(header.starts_with("POST /api/chat HTTP/1.1\r\n"));
        let header = header.to_ascii_lowercase();
        assert!(header.contains(&format!("\r\nhost: 127.0.0.1:{port}\r\n")));
        assert_eq!(
            header.contains("\r\nx-lucid-local-only: true\r\n"),
            local_only
        );
        let length: usize = header
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .unwrap()
            .parse()
            .unwrap();
        while request.len() < body_start + length {
            assert_ne!(socket.read_buf(&mut request).await.unwrap(), 0);
        }
        let body: serde_json::Value =
            serde_json::from_slice(&request[body_start..body_start + length]).unwrap();
        assert_eq!(body["stream"], true);
        socket.write_all(response_head).await.unwrap();
        socket
    }

    async fn write_chunk(socket: &mut tokio::net::TcpStream, bytes: &[u8]) {
        use tokio::io::AsyncWriteExt;

        socket
            .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
            .await
            .unwrap();
        socket.write_all(bytes).await.unwrap();
        socket.write_all(b"\r\n").await.unwrap();
    }

    #[tokio::test]
    async fn chat_stream_delivers_split_utf8_before_final_evidence() {
        use tokio::io::AsyncWriteExt;

        tokio::time::timeout(Duration::from_secs(5), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let (seen_first, wait_for_first) = oneshot::channel();

            let server = tokio::spawn(async move {
                let mut socket = reply_to(
                    listener,
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\nX-Lucid-Routed-Via: peer\r\nX-Lucid-Trust-Label: remote-pending\r\nX-Lucid-Receipt-Verified: true\r\n\r\n",
                    false,
                )
                .await;
                let line = "{\"done\":false,\"message\":{\"role\":\"assistant\",\"content\":\"hé\"},\"x_lucid_trust_label\":\"remote-spot-checked\"}\n";
                let split = line.find('é').unwrap() + 1;
                write_chunk(&mut socket, &line.as_bytes()[..split]).await;
                write_chunk(&mut socket, &line.as_bytes()[split..]).await;
                wait_for_first.await.unwrap();
                write_chunk(
                    &mut socket,
                    b"{\"done\":true,\"done_reason\":\"stop\",\"x_lucid_trust_label\":\"remote-receipt-verified\",\"x_lucid_receipt_verified\":\"true\"}\n",
                )
                .await;
                socket.write_all(b"0\r\n\r\n").await.unwrap();
            });

            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let mut seen_first = Some(seen_first);
            let mut events = Vec::new();
            let final_evidence = stream_chat(&client, port, "echo".into(), Vec::new(), false, |delta, evidence| {
                events.push((delta.to_string(), evidence));
                if delta == "hé" {
                    seen_first.take().unwrap().send(()).unwrap();
                }
                Ok(())
            })
            .await
            .unwrap();

            assert_eq!(
                events,
                [
                    (
                        String::new(),
                        Some(ChatEvidence {
                            routed_via: Some("peer"),
                            receipt_verified: None,
                            trust_label: Some("remote-pending"),
                        }),
                    ),
                    ("hé".to_string(), None),
                ]
            );
            assert_eq!(final_evidence.trust_label, Some("remote-receipt-verified"));
            assert_eq!(final_evidence.receipt_verified, Some(true));
            server.await.unwrap();
        })
        .await
        .expect("streaming test timed out");
    }

    #[tokio::test]
    async fn stopping_chat_closes_an_incomplete_http_response() {
        use tokio::io::AsyncReadExt;

        tokio::time::timeout(Duration::from_secs(5), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let server = tokio::spawn(async move {
                let mut socket = reply_to(
                    listener,
                    b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\n\r\n",
                    true,
                )
                .await;
                write_chunk(
                    &mut socket,
                    b"{\"done\":false,\"message\":{\"role\":\"assistant\",\"content\":\"partial\"}}\n",
                )
                .await;
                assert_eq!(socket.read(&mut [0; 1]).await.unwrap(), 0);
            });

            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let (cancel, cancelled) = oneshot::channel();
            let mut cancel = Some(cancel);
            let result = until_cancelled(
                cancelled,
                stream_chat(&client, port, "echo".into(), Vec::new(), true, |delta, _| {
                    if delta == "partial" {
                        cancel.take().unwrap().send(()).unwrap();
                    }
                    Ok(())
                }),
            )
            .await;
            assert!(result.is_none());
            server.await.unwrap();
        })
        .await
        .expect("cancellation test timed out");
    }

    #[test]
    fn chat_frame_refuses_unearned_or_failed_terminal_evidence() {
        let mut ignore = |_: &str, _| Ok(());
        let final_frame = br#"{"done":true,"x_lucid_trust_label":"remote-spot-checked","x_lucid_receipt_verified":"false"}"#;
        let evidence = apply_frame(final_frame, Some("peer"), &mut ignore)
            .unwrap()
            .unwrap();
        assert_eq!(evidence.trust_label, None);
        assert_eq!(evidence.receipt_verified, Some(false));

        let error_frame = br#"{"done":true,"done_reason":"error","error":"failed","x_lucid_trust_label":"remote-spot-checked","x_lucid_receipt_verified":"true"}"#;
        assert!(apply_frame(error_frame, Some("peer"), &mut ignore).is_err());
    }
}
