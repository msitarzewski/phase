// SPDX-License-Identifier: AGPL-3.0-or-later

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, HOST};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, State};
use url::Url;

use super::{response_trust_label, AppController};

const CHAT_PATH: &str = "/api/chat";

#[derive(Debug, Deserialize, Serialize)]
pub struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Serialize)]
pub struct ChatReply {
    reply: String,
    routed_via: Option<String>,
    receipt_verified: Option<bool>,
    trust_label: Option<&'static str>,
}

fn single_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(value)
}

fn response_routed_via(headers: &HeaderMap) -> Option<String> {
    match single_header(headers, "x-lucid-routed-via")? {
        "local" => Some("local".to_string()),
        "peer" => Some("peer".to_string()),
        "trusted-circle-sharded" => Some("trusted-circle-sharded".to_string()),
        _ => None,
    }
}

fn response_receipt_verified(headers: &HeaderMap) -> Option<bool> {
    match single_header(headers, "x-lucid-receipt-verified")? {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn validate_loopback_chat_url(raw: &str) -> Result<Url> {
    let url = Url::parse(raw).context("invalid local chat URL")?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != CHAT_PATH
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("refusing non-loopback local chat URL");
    }
    Ok(url)
}

fn loopback_chat_url(port: u16) -> Result<Url> {
    validate_loopback_chat_url(&format!("http://127.0.0.1:{port}{CHAT_PATH}"))
}

#[tauri::command]
pub async fn send_chat_message(
    app: AppHandle,
    ctrl: State<'_, Arc<AppController>>,
    model: String,
    messages: Vec<ChatMessage>,
    local_only: bool,
) -> Result<ChatReply, String> {
    send_chat_message_inner(&app, &ctrl, model, messages, local_only)
        .await
        .map_err(|error| error.to_string())
}

async fn send_chat_message_inner(
    app: &AppHandle,
    ctrl: &AppController,
    model: String,
    messages: Vec<ChatMessage>,
    local_only: bool,
) -> Result<ChatReply> {
    let (port, _) = ctrl.ensure_started(app).await?;
    let url = loopback_chat_url(port)?;
    let host = HeaderValue::from_str(&format!("127.0.0.1:{port}"))
        .context("failed to construct local Host header")?;
    let mut request = ctrl
        .http
        .post(url)
        .header(HOST, host)
        .json(&serde_json::json!({
            "model": model,
            "messages": messages,
            "stream": false,
        }))
        .timeout(Duration::from_secs(300));
    if local_only {
        request = request.header("X-Lucid-Local-Only", "true");
    }

    let response = request.send().await.context("local chat request failed")?;
    let status = response.status();
    let routed_via = response_routed_via(response.headers());
    let receipt_verified = response_receipt_verified(response.headers());
    let trust_label = response_trust_label(response.headers());
    let body: serde_json::Value = response
        .json()
        .await
        .context("local chat response was not valid JSON")?;

    if !status.is_success() {
        let message = body
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("Local LUCID chat request failed");
        bail!("{message}");
    }

    let reply = body
        .pointer("/message/content")
        .and_then(serde_json::Value::as_str)
        .context("local chat response omitted assistant content")?
        .to_string();

    Ok(ChatReply {
        reply,
        routed_via,
        receipt_verified,
        trust_label,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_url_accepts_only_explicit_loopback_endpoint() {
        let url = validate_loopback_chat_url("http://127.0.0.1:11435/api/chat").unwrap();
        assert_eq!(url.as_str(), "http://127.0.0.1:11435/api/chat");

        for invalid in [
            "https://127.0.0.1:11435/api/chat",
            "http://localhost:11435/api/chat",
            "http://127.0.0.2:11435/api/chat",
            "http://203.0.113.10:11435/api/chat",
            "http://127.0.0.1:11435/api/generate",
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
        headers.insert("x-lucid-receipt-verified", HeaderValue::from_static("true"));
        headers.insert("x-lucid-trust-label", HeaderValue::from_static("local"));
        assert_eq!(response_routed_via(&headers).as_deref(), Some("peer"));
        assert_eq!(response_receipt_verified(&headers), Some(true));
        assert_eq!(response_trust_label(&headers), Some("local"));

        headers.append("x-lucid-routed-via", HeaderValue::from_static("local"));
        headers.append(
            "x-lucid-receipt-verified",
            HeaderValue::from_static("false"),
        );
        headers.append(
            "x-lucid-trust-label",
            HeaderValue::from_static("remote-receipt-verified"),
        );
        assert_eq!(response_routed_via(&headers), None);
        assert_eq!(response_receipt_verified(&headers), None);
        assert_eq!(response_trust_label(&headers), None);

        headers.insert("x-lucid-routed-via", HeaderValue::from_static("unknown"));
        headers.insert(
            "x-lucid-receipt-verified",
            HeaderValue::from_static("definitely"),
        );
        headers.insert("x-lucid-trust-label", HeaderValue::from_static("unknown"));
        assert_eq!(response_routed_via(&headers), None);
        assert_eq!(response_receipt_verified(&headers), None);
        assert_eq!(response_trust_label(&headers), None);
    }
}
