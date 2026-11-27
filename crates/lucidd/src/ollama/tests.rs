// SPDX-License-Identifier: AGPL-3.0-or-later

use super::*;

fn output_chunk(seq: u64, kind: &str, data: impl Into<Bytes>) -> OutputChunk {
    OutputChunk {
        kind: kind.to_string(),
        data: data.into(),
        seq,
    }
}

#[test]
fn http_manifest_is_short_lived_and_remote_valid() {
    let identity = NodeIdentity::generate();
    let job_spec = JobSpec::Embedding(phase_protocol::EmbeddingJobSpec {
        model_cid: "test-model".to_string(),
        input: vec!["hello".to_string()],
    });
    let manifest = http_manifest(job_spec)
        .sign_with(&identity)
        .expect("sign HTTP manifest");

    manifest
        .verify_for_remote_execution()
        .expect("HTTP manifest meets remote policy");
    let ttl = manifest
        .expires_at
        .expect("HTTP manifest has expiry")
        .signed_duration_since(manifest.created_at);
    assert!(ttl > chrono::Duration::minutes(4));
    assert!(ttl <= chrono::Duration::minutes(5));
}

#[test]
fn registry_metadata_uses_real_capabilities_without_fake_zero_fields() {
    let cid = crate::registry::ModelCid([9u8; 32]);
    let mut caps =
        crate::registry::ModelCapabilities::now("qwen3", cid, "Q4_K_M", 32_768, 2, "llama.cpp");
    caps.advertised_at = 0;

    let listing = serde_json::to_value(tag_model_from_caps_with_size(None, &caps))
        .expect("serialize registry-backed tag");
    assert_eq!(listing["name"], "qwen3:latest");
    assert_eq!(listing["model"], "qwen3:latest");
    assert_eq!(listing["digest"], format!("sha256:{}", cid.to_hex()));
    assert_ne!(listing["digest"], format!("sha256:{}", "0".repeat(64)));
    assert_eq!(listing["modified_at"], "1970-01-01T00:00:00.000000000Z");
    assert!(
        listing.get("size").is_none(),
        "unknown size must be omitted"
    );
    assert_eq!(listing["details"]["format"], "gguf");
    assert_eq!(listing["details"]["quantization_level"], "Q4_K_M");

    let shown = serde_json::to_value(show_response_from_caps(&caps))
        .expect("serialize registry-backed show response");
    assert_eq!(shown["model_info"]["phase.model_cid"], cid.to_hex());
    assert_eq!(shown["model_info"]["phase.context_length"], 32_768);
    assert!(shown.get("modelfile").is_none());
    assert!(shown.get("template").is_none());
    assert!(shown.get("parameters").is_none());
}

#[test]
fn show_merges_verified_install_metadata_with_loaded_capabilities() {
    let cid = crate::registry::ModelCid([17u8; 32]);
    let installed = crate::registry::InstalledModel {
        model_id: "qwen3".to_string(),
        model_cid: cid,
        format: "gguf".to_string(),
        size_bytes: 42_000,
        installed_at: 123,
    };
    let caps =
        crate::registry::ModelCapabilities::now("qwen3", cid, "Q4_K_M", 32_768, 2, "llama.cpp");

    let shown = serde_json::to_value(show_response_from_installed_and_caps(
        &installed,
        Some(&caps),
    ))
    .expect("serialize merged show response");
    assert_eq!(shown["model_info"]["phase.model_cid"], cid.to_hex());
    assert_eq!(
        shown["model_info"]["phase.verification"],
        "content_verified"
    );
    assert_eq!(shown["model_info"]["phase.size_bytes"], 42_000);
    assert_eq!(shown["model_info"]["phase.backend"], "llama.cpp");
    assert_eq!(shown["model_info"]["phase.context_length"], 32_768);
    assert_eq!(shown["details"]["format"], "gguf");
    assert_eq!(shown["details"]["quantization_level"], "Q4_K_M");
    assert_eq!(shown["capabilities"], serde_json::json!(["completion"]));
}

#[test]
fn ollama_latest_suffix_routes_to_verified_base_alias() {
    assert_eq!(ollama_routing_model_name("echo:latest"), "echo");
    assert_eq!(ollama_routing_model_name("QWEN3:latest"), "qwen3");
    assert_eq!(ollama_routing_model_name("qwen3:q4"), "qwen3:q4");
}

#[tokio::test]
async fn loopback_host_guard_requires_one_host_header() {
    use tower::ServiceExt as _;

    let app = Router::new()
        .route("/", get(|| async { StatusCode::OK }))
        .layer(middleware::from_fn(require_loopback_host));
    for hosts in [Vec::new(), vec!["localhost", "127.0.0.1"]] {
        let mut request = Request::new(Body::empty());
        for host in hosts {
            request
                .headers_mut()
                .append(header::HOST, HeaderValue::from_static(host));
        }
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}

#[tokio::test]
async fn ps_does_not_invent_runtime_residency_records() {
    let Json(response) = handle_ps().await;
    assert!(response.models.is_empty());
}

#[test]
fn unknown_show_and_unconfigured_pull_are_explicit_errors() {
    assert_eq!(
        unknown_model_response("not-loaded").status(),
        StatusCode::NOT_FOUND
    );

    for streaming in [false, true] {
        let response = pull_unavailable_response("model store not configured", streaming);
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        if streaming {
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .and_then(|value| value.to_str().ok()),
                Some("application/x-ndjson")
            );
        }
    }
}

#[test]
fn live_peer_verification_stays_pending_until_receipt_arrives() {
    assert_eq!(ReceiptVerification::Pending.header_value(), Some("pending"));
    assert_eq!(
        finalized_receipt_verification(ReceiptVerification::Pending, true),
        ReceiptVerification::Verified
    );
    assert_eq!(
        finalized_receipt_verification(ReceiptVerification::Pending, false),
        ReceiptVerification::Failed
    );
    assert_eq!(
        finalized_receipt_verification(ReceiptVerification::Verified, false),
        ReceiptVerification::Failed,
        "a pre-verified route still must surface its delivered receipt"
    );
}

#[test]
fn terminal_error_and_missing_final_are_http_failures() {
    let missing = HttpTerminal::default();
    assert!(missing
        .failure_reason()
        .expect("missing Final fails")
        .contains("without a terminal"));

    let mut errored = HttpTerminal::default();
    errored.observe(Completion::Error, Some("backend exploded".to_string()));
    assert_eq!(
        execution_failure_reason(&errored, ReceiptVerification::Verified).as_deref(),
        Some("model execution failed")
    );

    let mut duplicate = HttpTerminal::default();
    duplicate.observe(Completion::Stop, None);
    duplicate.observe(Completion::Stop, None);
    assert!(duplicate
        .failure_reason()
        .expect("duplicate Final fails")
        .contains("multiple terminal"));
}

#[test]
fn model_api_keeps_peer_addresses_and_backend_paths_private() {
    let detail = "dial /ip4/203.0.113.17/tcp/4001/p2p/private-peer at /private/operator/model.gguf";
    let error = RouterError::Relay(detail.to_string());
    let public = public_dispatch_error(&error);
    assert!(!public.contains("203.0.113.17"));
    assert!(!public.contains("private-peer"));
    let mut terminal = HttpTerminal::default();
    terminal.observe(Completion::Error, Some(detail.to_string()));
    assert_eq!(
        execution_failure_reason(&terminal, ReceiptVerification::Verified).as_deref(),
        Some("model execution failed")
    );
    let decision = RouteDecision {
        via: RouteVia::Peer {
            peer_id: PeerId::random(),
        },
        model_id: "gemma".into(),
        fallback_peers: Vec::new(),
        explanation: String::new(),
    };
    assert_eq!(public_routed_via(&decision).as_deref(), Some("peer"));
}

#[test]
fn remote_model_picker_dispatches_exact_cid_without_claiming_installation_or_access() {
    let caps = crate::registry::ModelCapabilities::now(
        "gemma",
        crate::registry::ModelCid([7; 32]),
        "Q4_K_M",
        2048,
        1,
        "llama.cpp",
    );
    let entry = crate::registry::NetworkModel {
        caps,
        size_bytes: None,
        format: Some("gguf".into()),
        providers: vec![PeerId::random()],
    };
    let tag = serde_json::to_value(tag_model_from_network(&entry, "access_not_confirmed")).unwrap();
    assert_eq!(tag["name"], "gemma:latest");
    assert_eq!(tag["model"], entry.caps.model_cid.to_hex());
    assert_eq!(tag["x_lucid_access"], "access_not_confirmed");
    assert!(tag.get("size").is_none());
    assert!(tag.get("providers").is_none());
    assert!(tag.get("host").is_none());
}

#[test]
fn peer_failure_response_is_non_success_with_truthful_header() {
    for (verification, expected_header) in [
        (ReceiptVerification::Failed, "false"),
        (ReceiptVerification::Unverifiable, "unverifiable"),
    ] {
        let response = execution_failure_response(
            "untrusted peer result".to_string(),
            Some("peer:test"),
            verification,
            Some("bounded route summary"),
        );
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            response
                .headers()
                .get(HEADER_RECEIPT_VERIFIED)
                .and_then(|value| value.to_str().ok()),
            Some(expected_header)
        );
        assert!(!response.headers().contains_key("x-phase-receipt"));
    }

    let verified_error = execution_failure_response(
        "verified worker failure".to_string(),
        Some("peer:test"),
        ReceiptVerification::Verified,
        None,
    );
    assert_eq!(verified_error.status(), StatusCode::BAD_GATEWAY);
    assert_eq!(
        verified_error
            .headers()
            .get(HEADER_RECEIPT_VERIFIED)
            .and_then(|value| value.to_str().ok()),
        Some("true"),
        "receipt validity and execution success are distinct"
    );
}

#[test]
fn stream_terminal_carries_final_verification_or_error() {
    let mut successful_terminal = HttpTerminal::default();
    successful_terminal.observe(Completion::Stop, None);
    let verified_receipt = FinishedReceipt {
        verification: ReceiptVerification::Verified,
        commitment: Some([7u8; 32]),
    };
    let mut successful = serde_json::json!({ "done": true, "done_reason": "stop" });
    annotate_stream_terminal(&mut successful, &successful_terminal, &verified_receipt);
    assert_eq!(successful["x_lucid_receipt_verified"], "true");
    assert_eq!(successful["x_phase_commitment"], hex32(&[7u8; 32]));
    assert!(successful.get("error").is_none());

    let failed_receipt = FinishedReceipt {
        verification: ReceiptVerification::Failed,
        commitment: None,
    };
    let mut failed = serde_json::json!({ "done": true, "done_reason": "stop" });
    annotate_stream_terminal(&mut failed, &successful_terminal, &failed_receipt);
    assert_eq!(failed["x_lucid_receipt_verified"], "false");
    assert_eq!(failed["done_reason"], "error");
    assert!(failed["error"].as_str().unwrap().contains("receipt"));
    assert!(failed.get("x_phase_commitment").is_none());

    let mut errored_terminal = HttpTerminal::default();
    errored_terminal.observe(Completion::Error, Some("worker failed".to_string()));
    let mut verified_error = serde_json::json!({ "done": true, "done_reason": "error" });
    annotate_stream_terminal(&mut verified_error, &errored_terminal, &verified_receipt);
    assert_eq!(verified_error["x_lucid_receipt_verified"], "true");
    assert_eq!(verified_error["error"], "model execution failed");
    assert!(verified_error.get("x_phase_commitment").is_none());
}

#[tokio::test(start_paused = true)]
async fn remote_trust_label_requires_a_matching_and_completed_spot_check() {
    let mut terminal = HttpTerminal::default();
    terminal.observe(Completion::Stop, None);
    let receipt = FinishedReceipt {
        verification: ReceiptVerification::Verified,
        commitment: Some([7; 32]),
    };
    let initial = inference_stream_response(
        futures::stream::empty(),
        Some("peer"),
        "test route",
        ReceiptVerification::Pending,
    );
    assert_eq!(initial.headers()[HEADER_TRUST_LABEL], "remote-pending");
    assert_eq!(initial.headers()[header::TRAILER], HEADER_TRUST_LABEL);

    for (result, expected) in [
        (
            RedundantCheckResult::Agreement {
                commitment: [7; 32],
            },
            TrustLabel::RemoteSpotChecked,
        ),
        (
            RedundantCheckResult::Agreement {
                commitment: [8; 32],
            },
            TrustLabel::RemoteReceiptVerified,
        ),
        (
            RedundantCheckResult::Disagreement {
                primary_commitment: [7; 32],
                duplicate_commitment: [8; 32],
            },
            TrustLabel::RemoteReceiptVerified,
        ),
    ] {
        let (tx, rx) = tokio::sync::oneshot::channel();
        tx.send(result).unwrap();
        let mut payload = serde_json::json!({"done": true});
        assert_eq!(
            stream_terminal_trust_label(Some("peer"), &mut payload, &terminal, &receipt, Some(rx))
                .await,
            Some(expected)
        );
        assert_eq!(payload[TERMINAL_TRUST_LABEL], expected.as_str());
        let trailer = trust_label_trailer(expected).into_trailers().unwrap();
        assert_eq!(trailer[HEADER_TRUST_LABEL], expected.as_str());
    }

    let (_tx, slow_check) = tokio::sync::oneshot::channel();
    assert_eq!(
        completed_trust_label(Some("peer"), &receipt, Some(slow_check)).await,
        Some(TrustLabel::RemoteReceiptVerified)
    );
    let local_sharded = FinishedReceipt {
        verification: ReceiptVerification::Local,
        commitment: Some([9; 32]),
    };
    assert_eq!(
        completed_trust_label(Some("trusted-circle-sharded"), &local_sharded, None).await,
        Some(TrustLabel::TrustedCircleSharded)
    );
    assert_eq!(
        completed_trust_label(Some("local"), &local_sharded, None).await,
        Some(TrustLabel::Local)
    );
    let invalid = FinishedReceipt {
        verification: ReceiptVerification::Failed,
        commitment: None,
    };
    let (tx, rx) = tokio::sync::oneshot::channel();
    tx.send(RedundantCheckResult::Agreement {
        commitment: [7; 32],
    })
    .unwrap();
    let mut payload = serde_json::json!({"done": true});
    assert_eq!(
        stream_terminal_trust_label(Some("peer"), &mut payload, &terminal, &invalid, Some(rx))
            .await,
        None
    );
    assert!(payload.get(TERMINAL_TRUST_LABEL).is_none());
}

#[test]
fn sanitize_strips_crlf_and_ansi() {
    // SEC-10: CRLF (log forging) and ESC/CSI (ANSI abuse) are dropped,
    // yielding a single-line, escape-free string.
    let evil = "/api/\r\nFAKE-LOG-LINE\x1b[31mred\x1b[0m\x00\x7f";
    let s = sanitize_for_log(evil);
    assert!(!s.contains('\r'));
    assert!(!s.contains('\n'));
    assert!(!s.contains('\x1b'));
    assert!(!s.contains('\x00'));
    assert!(!s.contains('\x7f'));
    // Visible content survives.
    assert!(s.contains("/api/"));
    assert!(s.contains("FAKE-LOG-LINE"));
    assert!(s.contains("red"));
}

#[test]
fn sanitize_caps_length() {
    // SEC-10: a 10 KB path is capped at LOG_FIELD_CAP (+ ellipsis).
    let huge = "/".to_string() + &"a".repeat(10_000);
    let s = sanitize_for_log(&huge);
    // chars(): cap content chars plus the trailing ellipsis marker.
    assert!(s.chars().count() <= LOG_FIELD_CAP + 1);
    assert!(s.ends_with('…'));
}

#[test]
fn sanitize_passes_clean_path_through() {
    // SEC-10: a normal path is unchanged.
    let clean = "/api/chat";
    assert_eq!(sanitize_for_log(clean), clean);
}

#[test]
fn embed_input_accepts_single_string() {
    // Ollama's `/api/embed` allows `input` as a bare string; the
    // `untagged` enum should pick `One` and normalize to a 1-element vec.
    let req: EmbedRequest =
        serde_json::from_str(r#"{"model":"m","input":"hello"}"#).expect("parse single");
    let inputs = req.input.expect("input present").into_vec();
    assert_eq!(inputs, vec!["hello".to_string()]);
}

#[test]
fn embed_input_accepts_array_of_strings() {
    // …and as an array; the same enum should pick `Many` and pass it
    // through unchanged, preserving order.
    let req: EmbedRequest =
        serde_json::from_str(r#"{"model":"m","input":["x","y"]}"#).expect("parse array");
    let inputs = req.input.expect("input present").into_vec();
    assert_eq!(inputs, vec!["x".to_string(), "y".to_string()]);
}

#[test]
fn embed_input_refuses_unknown_fields() {
    let parsed = serde_json::from_str::<EmbedRequest>(
        r#"{"model":"m","input":"z","future_backend_knob":true}"#,
    );
    assert!(parsed.is_err());
}

#[test]
fn nested_messages_and_legacy_embeddings_refuse_unknown_fields() {
    assert!(serde_json::from_str::<WireMessage>(
        r#"{"role":"user","content":"x","future_field":true}"#
    )
    .is_err());
    assert!(serde_json::from_str::<EmbeddingsRequest>(
        r#"{"model":"m","prompt":"x","future_field":true}"#
    )
    .is_err());
}

#[test]
fn text_output_requires_token_kind_contiguous_sequence_and_utf8() {
    let mut next = 0;
    let first = output_chunk(0, "token", Bytes::from_static(b"hello"));
    assert_eq!(validated_text_chunk(&first, &mut next).unwrap(), "hello");
    assert_eq!(next, 1);

    let wrong_kind = output_chunk(1, "embedding", Bytes::from_static(b"[]"));
    assert!(validated_text_chunk(&wrong_kind, &mut next)
        .unwrap_err()
        .contains("non-token"));

    let wrong_seq = output_chunk(2, "token", Bytes::from_static(b"later"));
    assert!(validated_text_chunk(&wrong_seq, &mut next)
        .unwrap_err()
        .contains("out-of-sequence"));

    let non_utf8 = output_chunk(1, "token", Bytes::from_static(&[0xff, 0xfe]));
    assert!(validated_text_chunk(&non_utf8, &mut next)
        .unwrap_err()
        .contains("non-UTF-8"));
    assert_eq!(next, 1, "invalid chunks must not advance the sequence");
}

#[test]
fn embedding_collector_preserves_input_order_and_exact_count() {
    let mut collector = EmbeddingCollector::new(2);
    collector.observe(&output_chunk(
        1,
        "embedding",
        Bytes::from_static(br#"[3.0,4.0]"#),
    ));
    collector.observe(&output_chunk(
        0,
        "embedding",
        Bytes::from_static(br#"[1.0,2.0]"#),
    ));
    assert_eq!(
        collector.finish().unwrap(),
        vec![vec![1.0, 2.0], vec![3.0, 4.0]]
    );

    let missing = EmbeddingCollector::new(1);
    assert!(missing.finish().unwrap_err().contains("fewer"));

    let mut extra = EmbeddingCollector::new(1);
    extra.observe(&output_chunk(
        1,
        "embedding",
        Bytes::from_static(br#"[1.0]"#),
    ));
    assert!(extra.finish().unwrap_err().contains("extra"));

    let mut duplicate = EmbeddingCollector::new(1);
    let vector = output_chunk(0, "embedding", Bytes::from_static(br#"[1.0]"#));
    duplicate.observe(&vector);
    duplicate.observe(&vector);
    assert!(duplicate.finish().unwrap_err().contains("duplicate"));
}

#[test]
fn embedding_collector_rejects_wrong_kind_malformed_and_invalid_vectors() {
    for (chunk, expected) in [
        (
            output_chunk(0, "token", Bytes::from_static(b"text")),
            "non-embedding",
        ),
        (
            output_chunk(0, "embedding", Bytes::from_static(b"not-json")),
            "malformed",
        ),
        (
            output_chunk(0, "embedding", Bytes::from_static(b"[]")),
            "dimension",
        ),
    ] {
        let mut collector = EmbeddingCollector::new(1);
        collector.observe(&chunk);
        assert!(collector.finish().unwrap_err().contains(expected));
    }

    let mut inconsistent = EmbeddingCollector::new(2);
    inconsistent.observe(&output_chunk(
        0,
        "embedding",
        Bytes::from_static(br#"[1.0]"#),
    ));
    inconsistent.observe(&output_chunk(
        1,
        "embedding",
        Bytes::from_static(br#"[1.0,2.0]"#),
    ));
    assert!(inconsistent.finish().unwrap_err().contains("inconsistent"));

    assert!(validate_embedding_values(&[f32::NAN])
        .unwrap_err()
        .contains("non-finite"));
    assert!(validate_embedding_values(&[f32::INFINITY])
        .unwrap_err()
        .contains("non-finite"));
}

#[test]
fn route_explanation_is_bounded_control_safe_and_identifier_redacted() {
    let peer_shaped = "A".repeat(52);
    let raw = format!(
        "local evidence\r\nselected {peer_shaped} \u{1b}[31m{}",
        "x".repeat(400)
    );
    let public = public_route_explanation(&raw);
    assert!(public.is_ascii());
    assert!(public.len() <= MAX_ROUTE_EXPLANATION_BYTES);
    assert!(!public.contains('\r'));
    assert!(!public.contains('\n'));
    assert!(!public.contains('\u{1b}'));
    assert!(!public.contains(&peer_shaped));
    assert!(public.contains("[redacted]"));
    assert!(HeaderValue::from_str(&public).is_ok());
}

#[test]
fn ollama_options_are_bounded_and_signed_into_sampling() {
    let options = serde_json::json!({
        "temperature": 0.7,
        "top_p": 0.9,
        "repeat_penalty": 1.1,
        "seed": 42,
        "stop": ["END"],
        "num_predict": 128,
    });
    let (sampling, max_tokens) = parse_ollama_options(Some(&options)).unwrap();
    assert_eq!(max_tokens, 128);
    assert_eq!(sampling.params["temperature"], "0.7");
    assert_eq!(sampling.params["repetition_penalty"], "1.1");
    assert_eq!(sampling.params["stop"], r#"["END"]"#);
}

#[test]
fn ollama_options_refuse_unknown_unbounded_and_duplicate_controls() {
    for options in [
        serde_json::json!({"future_backend_knob": true}),
        serde_json::json!({"num_predict": 0}),
        serde_json::json!({"num_predict": 8193}),
        serde_json::json!({"repeat_penalty": 1.1, "repetition_penalty": 1.2}),
        serde_json::json!(["not", "an", "object"]),
    ] {
        assert!(
            parse_ollama_options(Some(&options)).is_err(),
            "expected options to be refused: {options}"
        );
    }
}

#[test]
fn request_validators_refuse_ignored_controls_and_unbounded_inputs() {
    for (field, value, expected) in [
        (
            "keep_alive",
            serde_json::json!("5m"),
            "keep_alive is not supported; contributor policy controls model residency",
        ),
        (
            "format",
            serde_json::json!("json"),
            "structured format is not supported",
        ),
        (
            "tools",
            serde_json::json!([{"type":"function"}]),
            "tools are not supported",
        ),
    ] {
        let mut value_object = serde_json::json!({"model":"m","messages":[]});
        value_object[field] = value;
        let request: ChatRequest = serde_json::from_value(value_object).unwrap();
        assert_eq!(validate_chat_request(&request), Err(expected));
    }
    let chat: ChatRequest = serde_json::from_value(serde_json::json!({
        "model": "m",
        "messages": [{"role": "alien", "content": "x"}],
    }))
    .unwrap();
    assert!(validate_chat_request(&chat).is_err());

    let chat_with_image: ChatRequest = serde_json::from_value(serde_json::json!({
        "model": "m",
        "messages": [{"role": "user", "content": "x", "images": ["abc"]}],
    }))
    .unwrap();
    assert!(validate_chat_request(&chat_with_image).is_err());

    let generate: GenerateRequest = serde_json::from_value(serde_json::json!({
        "model": "m", "prompt": "x", "template": "ignored-before-this-fix"
    }))
    .unwrap();
    assert!(validate_generate_request(&generate).is_err());

    assert!(validate_embedding_input(&vec!["x".to_string(); MAX_EMBED_INPUTS + 1]).is_err());
    assert!(validate_embedding_input(&["x".repeat(MAX_HTTP_INPUT_CHARS + 1)]).is_err());
}

#[test]
fn pull_name_hygiene_accepts_plain_names() {
    // Ordinary model names (the only thing that can resolve to a local
    // GGUF) pass the guard.
    for name in ["qwen3", "llama3.1", "nomic-embed-text", "gpt-oss"] {
        assert!(pull_name_is_safe(name), "expected {name:?} to be allowed");
    }
}

#[test]
fn pull_name_hygiene_rejects_traversal_and_flags() {
    // SEC-04 parity with worker_llama::resolve_model_path: separators,
    // `..`, leading `-`, and empty are all rejected before the filesystem
    // is ever touched.
    for name in [
        "",
        "../etc/passwd",
        "..",
        "foo/bar",
        "foo\\bar",
        "-rf",
        "--flag",
        "a/../b",
    ] {
        assert!(!pull_name_is_safe(name), "expected {name:?} to be rejected");
    }
}

#[test]
fn pull_selection_parses_exact_cid_and_publisher_pins_strictly() {
    let cid = crate::registry::ModelCid([0x7a; 32]);
    let publisher = PeerId::random();
    assert_eq!(
        parse_pull_selection(Some(&cid.to_hex()), Some(&publisher.to_string())).unwrap(),
        PullSelection {
            exact_cid: Some(cid),
            publisher: Some(publisher),
        }
    );
    assert!(parse_pull_selection(Some("sha256:not-canonical"), None).is_err());
    assert!(parse_pull_selection(None, Some("not-a-peer-id")).is_err());
}

#[test]
fn public_pull_progress_and_errors_hide_provider_and_backend_details() {
    let progress = pull_progress_line(PullProgress::Downloading {
        cid: crate::registry::ModelCid([0x7a; 32]).to_hex(),
        completed: 5,
        total: 10,
        provider: "private-host-peer".into(),
    });
    let value: serde_json::Value = serde_json::from_slice(&progress).unwrap();
    assert_eq!(value["completed"], 5);
    assert_eq!(value["total"], 10);
    assert!(value.get("provider").is_none());
    for error in [
        ContentError::Transfer("/ip4/192.0.2.10/tcp/4001 private-host-peer".into()),
        ContentError::ProvidersExhausted {
            attempts: 2,
            last_error: "private-host-peer".into(),
        },
        ContentError::Verification("/private/model/path".into()),
    ] {
        let public = public_content_error(&error);
        assert!(!public.contains("private"));
        assert!(!public.contains("192.0.2.10"));
    }
}
