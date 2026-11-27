// SPDX-License-Identifier: AGPL-3.0-or-later

//! Whole-model independent-job baseline benchmark harness (`lucidd bench`).
//!
//! Directly supports the sequencing gate in
//! `memory-bank/releases/lucid-v0.2/shardworker.md`: before cross-peer tensor
//! sharding, LUCID characterizes bounded concurrent complete-job throughput,
//! TTFT, latency percentiles, peer distribution, and receipt verification
//! through the unmodified Ollama `/api/chat` surface on loopback.
//!
//! Privacy rule: reports contain ONLY sanitized timings, counts, route labels,
//! and receipt verification booleans — never raw prompt text or generated
//! completions.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::ollama::{HEADER_RECEIPT_VERIFIED, HEADER_ROUTED_VIA};

#[derive(Debug, Clone)]
pub struct BenchConfig {
    pub api_base: String,
    pub model: String,
    pub total_requests: usize,
    pub concurrency: usize,
    pub max_tokens: u32,
    pub timeout_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchSample {
    pub index: usize,
    pub ok: bool,
    pub http_status: u16,
    pub routed_via: String,
    pub receipt_verified: bool,
    #[serde(default)]
    pub local_commitment: bool,
    pub ttft_ms: Option<f64>,
    pub total_ms: f64,
    pub chunks: usize,
    pub eval_tokens: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    pub model: String,
    pub total_requests: usize,
    pub concurrency: usize,
    pub max_tokens: u32,
    pub wall_seconds: f64,
    pub succeeded: usize,
    pub failed: usize,
    pub receipt_verified_count: usize,
    #[serde(default)]
    pub local_commitments: usize,
    pub jobs_per_second: f64,
    pub total_chunks: usize,
    pub chunks_per_second: f64,
    pub total_eval_tokens: u64,
    #[serde(default)]
    pub eval_count_source: String,
    pub tokens_per_second: f64,
    pub ttft_p50_ms: Option<f64>,
    pub ttft_p95_ms: Option<f64>,
    pub duration_p50_ms: Option<f64>,
    pub duration_p95_ms: Option<f64>,
    pub route_distribution: BTreeMap<String, usize>,
    pub samples: Vec<BenchSample>,
}

/// Run the bounded whole-model benchmark against a loopback `lucidd` HTTP API.
pub async fn run_benchmark(cfg: BenchConfig) -> Result<BenchReport> {
    if cfg.total_requests == 0 || cfg.total_requests > 10_000 {
        bail!("--requests must be in 1..=10000");
    }
    if cfg.concurrency == 0 || cfg.concurrency > 64 {
        bail!("--concurrency must be in 1..=64");
    }
    if cfg.max_tokens == 0 || cfg.max_tokens > 4096 {
        bail!("--max-tokens must be in 1..=4096");
    }

    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(cfg.timeout_secs.max(5)))
        .build()
        .context("failed to build benchmark HTTP client")?;

    let sem = Arc::new(Semaphore::new(cfg.concurrency));
    let url = format!("{}/api/chat", cfg.api_base.trim_end_matches('/'));
    let wall_start = Instant::now();

    let mut handles = Vec::with_capacity(cfg.total_requests);
    for index in 0..cfg.total_requests {
        let permit = sem.clone().acquire_owned().await?;
        let client = client.clone();
        let url = url.clone();
        let model = cfg.model.clone();
        let max_tokens = cfg.max_tokens;
        handles.push(tokio::spawn(async move {
            let _permit = permit;
            execute_single_sample(&client, &url, &model, max_tokens, index).await
        }));
    }

    let mut samples = Vec::with_capacity(cfg.total_requests);
    for handle in handles {
        samples.push(handle.await.context("benchmark worker panicked")?);
    }
    samples.sort_by_key(|s| s.index);

    let wall_seconds = wall_start.elapsed().as_secs_f64().max(1e-6);
    Ok(summarize_samples(&cfg, samples, wall_seconds))
}

async fn execute_single_sample(
    client: &reqwest::Client,
    url: &str,
    model: &str,
    max_tokens: u32,
    index: usize,
) -> BenchSample {
    let started = Instant::now();
    // Non-sensitive deterministic benchmark prompt.
    let payload = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "user", "content": format!("List three facts about evergreen trees (run {index}).") }
        ],
        "stream": true,
        "options": {
            "num_predict": max_tokens,
            "temperature": 0.0,
            "seed": 42
        }
    });

    let resp = match client.post(url).json(&payload).send().await {
        Ok(r) => r,
        Err(err) => {
            return BenchSample {
                index,
                ok: false,
                http_status: 0,
                routed_via: "none".to_string(),
                receipt_verified: false,
                local_commitment: false,
                ttft_ms: None,
                total_ms: started.elapsed().as_secs_f64() * 1000.0,
                chunks: 0,
                eval_tokens: 0,
                error: Some(err.to_string()),
            };
        }
    };

    let http_status = resp.status().as_u16();
    let routed_via = resp
        .headers()
        .get(HEADER_ROUTED_VIA)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("unknown")
        .to_string();
    let header_verified = resp
        .headers()
        .get(HEADER_RECEIPT_VERIFIED)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return BenchSample {
            index,
            ok: false,
            http_status,
            routed_via,
            receipt_verified: false,
            local_commitment: false,
            ttft_ms: None,
            total_ms: started.elapsed().as_secs_f64() * 1000.0,
            chunks: 0,
            eval_tokens: 0,
            error: Some(body.chars().take(160).collect()),
        };
    }

    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    let mut ttft_ms: Option<f64> = None;
    let mut chunks = 0usize;
    let mut eval_tokens = 0u64;
    let mut done_seen = false;
    let mut terminal_verified = false;
    let mut terminal_commitment = false;
    let mut stream_err: Option<String> = None;

    while let Some(item) = stream.next().await {
        let bytes = match item {
            Ok(b) => b,
            Err(err) => {
                stream_err = Some(err.to_string());
                break;
            }
        };
        buf.extend_from_slice(&bytes);
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let text = String::from_utf8_lossy(&line);
            let trimmed = text.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(frame) = serde_json::from_str::<serde_json::Value>(trimmed) {
                if let Some(err) = frame.get("error").and_then(|v| v.as_str()) {
                    stream_err = Some(err.to_string());
                }
                let content = frame
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !content.is_empty() {
                    if ttft_ms.is_none() {
                        ttft_ms = Some(started.elapsed().as_secs_f64() * 1000.0);
                    }
                    chunks += 1;
                }
                if frame.get("done").and_then(|v| v.as_bool()) == Some(true) {
                    done_seen = true;
                    if let Some(count) = frame.get("eval_count").and_then(|v| v.as_u64()) {
                        eval_tokens = count;
                    }
                    if let Some(rv) = frame.get("x_lucid_receipt_verified") {
                        if rv.as_bool() == Some(true) || rv.as_str() == Some("true") {
                            terminal_verified = true;
                        }
                    }
                    terminal_commitment = frame
                        .get("x_phase_commitment")
                        .and_then(|value| value.as_str())
                        .is_some_and(|digest| {
                            digest.len() == 64
                                && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                        });
                }
            }
        }
    }

    let ok = done_seen && stream_err.is_none();
    let receipt_verified = ok
        && is_peer_route(&routed_via)
        && (terminal_verified || header_verified.as_deref() == Some("true"));
    let local_commitment = ok && routed_via == "local" && terminal_commitment;

    BenchSample {
        index,
        ok,
        http_status,
        routed_via,
        receipt_verified,
        local_commitment,
        ttft_ms,
        total_ms: started.elapsed().as_secs_f64() * 1000.0,
        chunks,
        eval_tokens,
        error: stream_err,
    }
}

fn is_peer_route(route: &str) -> bool {
    route == "peer" || route.starts_with("peer:")
}

pub fn summarize_samples(
    cfg: &BenchConfig,
    samples: Vec<BenchSample>,
    wall_seconds: f64,
) -> BenchReport {
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut receipt_verified_count = 0usize;
    let mut local_commitments = 0usize;
    let mut total_chunks = 0usize;
    let mut total_eval_tokens = 0u64;
    let mut ttfts = Vec::new();
    let mut durations = Vec::new();
    let mut route_distribution: BTreeMap<String, usize> = BTreeMap::new();

    for s in &samples {
        if s.ok {
            succeeded += 1;
            durations.push(s.total_ms);
            if let Some(t) = s.ttft_ms {
                ttfts.push(t);
            }
            total_chunks += s.chunks;
            total_eval_tokens = total_eval_tokens.saturating_add(s.eval_tokens);
            if s.receipt_verified && is_peer_route(&s.routed_via) {
                receipt_verified_count += 1;
            }
            if s.local_commitment && s.routed_via == "local" {
                local_commitments += 1;
            }
            *route_distribution.entry(s.routed_via.clone()).or_insert(0) += 1;
        } else {
            failed += 1;
        }
    }

    BenchReport {
        model: cfg.model.clone(),
        total_requests: cfg.total_requests,
        concurrency: cfg.concurrency,
        max_tokens: cfg.max_tokens,
        wall_seconds,
        succeeded,
        failed,
        receipt_verified_count,
        local_commitments,
        jobs_per_second: (succeeded as f64) / wall_seconds,
        total_chunks,
        chunks_per_second: (total_chunks as f64) / wall_seconds,
        total_eval_tokens,
        eval_count_source: "self_reported".to_string(),
        tokens_per_second: (total_eval_tokens as f64) / wall_seconds,
        ttft_p50_ms: percentile(&mut ttfts, 0.50),
        ttft_p95_ms: percentile(&mut ttfts, 0.95),
        duration_p50_ms: percentile(&mut durations, 0.50),
        duration_p95_ms: percentile(&mut durations, 0.95),
        route_distribution,
        samples,
    }
}

pub fn percentile(values: &mut [f64], p: f64) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(|a, b| a.total_cmp(b));
    let idx = ((values.len() - 1) as f64 * p.clamp(0.0, 1.0)).round() as usize;
    Some(values[idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarize_samples_computes_percentiles_and_route_distribution() {
        let cfg = BenchConfig {
            api_base: "http://127.0.0.1:11435".to_string(),
            model: "qwen2.5:14b".to_string(),
            total_requests: 4,
            concurrency: 2,
            max_tokens: 64,
            timeout_secs: 30,
        };
        let samples = vec![
            BenchSample {
                index: 0,
                ok: true,
                http_status: 200,
                routed_via: "peer:peerA".to_string(),
                receipt_verified: true,
                local_commitment: false,
                ttft_ms: Some(120.0),
                total_ms: 800.0,
                chunks: 30,
                eval_tokens: 30,
                error: None,
            },
            BenchSample {
                index: 1,
                ok: true,
                http_status: 200,
                routed_via: "local".to_string(),
                // Even reports with a stale verification flag count local work
                // exclusively as a local commitment.
                receipt_verified: true,
                local_commitment: true,
                ttft_ms: Some(180.0),
                total_ms: 1000.0,
                chunks: 32,
                eval_tokens: 32,
                error: None,
            },
            BenchSample {
                index: 2,
                ok: true,
                http_status: 200,
                routed_via: "peer:peerA".to_string(),
                receipt_verified: true,
                local_commitment: false,
                ttft_ms: Some(150.0),
                total_ms: 900.0,
                chunks: 28,
                eval_tokens: 28,
                error: None,
            },
            BenchSample {
                index: 3,
                ok: false,
                http_status: 503,
                routed_via: "none".to_string(),
                receipt_verified: false,
                local_commitment: false,
                ttft_ms: None,
                total_ms: 50.0,
                chunks: 0,
                eval_tokens: 0,
                error: Some("capacity".to_string()),
            },
        ];

        let report = summarize_samples(&cfg, samples, 2.0);
        assert_eq!(report.succeeded, 3);
        assert_eq!(report.failed, 1);
        assert_eq!(report.receipt_verified_count, 2);
        assert_eq!(report.local_commitments, 1);
        assert_eq!(report.eval_count_source, "self_reported");
        assert_eq!(report.total_chunks, 90);
        assert_eq!(report.total_eval_tokens, 90);
        assert!((report.tokens_per_second - 45.0).abs() < 1e-6);
        assert_eq!(report.ttft_p50_ms, Some(150.0));
        assert_eq!(report.duration_p50_ms, Some(900.0));
        assert_eq!(report.route_distribution.get("peer:peerA"), Some(&2));
        assert_eq!(report.route_distribution.get("local"), Some(&1));
    }
}
