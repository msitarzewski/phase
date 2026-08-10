// SPDX-License-Identifier: AGPL-3.0-or-later

//! LUCID inference daemon — open GPU inference flagship built on the Phase
//! substrate. Implements the `phase-protocol::Worker` trait for inference
//! workloads.
//!
//! The production path supports verified llama.cpp inference, signed
//! alias/CID and provider records, resumable content transfer, live libp2p
//! relay streaming, bounded reachability infrastructure, policy gating, and
//! signed receipt verification. [`echo`] is retained only as an explicit
//! development/test worker.

// SEC-11 (L7): forbid `unsafe` in lucidd's own code. Per-crate — wasmtime /
// libp2p pulled transitively still use `unsafe` internally; this only guards
// lucidd's source against an `unsafe` regression.
#![deny(unsafe_code)]

pub mod content;
pub mod dht_transport;
pub mod echo;
pub mod ollama;
pub mod policy;
pub mod registry;
pub mod reputation;
pub mod router;
pub mod worker_llama;
pub mod worker_mlx;

// LUCID M2: the production inference worker. Shells out to `llama-server`,
// streams tokens back through the protocol, and signs receipts. Exported at
// the crate root so the binary can switch between EchoWorker (no GPU
// required, used by CI) and LlamaCppWorker (production path) via CLI flag.
pub use worker_llama::{LlamaCppConfig, LlamaCppWorker};
pub use worker_mlx::{
    inspect_mlx_bundle, MlxBundleMetadata, MlxConfig, MlxWorker, MLX_BUNDLE_FORMAT,
    MLX_BUNDLE_ROOT_ALGORITHM, MLX_HARDWARE_ACCEPTANCE, MLX_PORT_BINDING_STATUS,
    MLX_RUNTIME_ATTESTATION, TARGET_MLX_LM_VERSION, TARGET_MLX_VERSION,
};

// Public re-exports for the M6 model registry. Downstream code (the
// router in M5, the Ollama `/api/tags` handler in M4) consumes these as
// `lucidd::ModelRegistry` etc. without having to know about the module
// layout. See `registry` module docs for the trust model and TTL story.
pub use registry::{
    alias_dht_key, normalize_model_alias, AliasRecord, DhtTransport, ModelCapabilities, ModelCid,
    ModelRegistry, ResolvedAlias, SignedAliasRecord, SignedModelAdvertisement,
    ADVERTISEMENT_SCHEMA_VERSION, ADVERTISEMENT_TTL, ALIAS_KEY_PREFIX, ALIAS_SCHEMA_VERSION,
    MAX_ALIAS_TTL, MAX_MODEL_SIZE_BYTES, MODEL_KEY_PREFIX, TTL_REFRESH_INTERVAL,
};

// Public re-exports for the M7 policy surface. The router (M5) calls
// `PolicyEngine::should_serve` on every remote inference request and
// honors the returned `PolicyDecision`. See `policy` module docs for the
// "pause, don't deprioritize" framing.
pub use policy::{
    PauseReason, PolicyConfig, PolicyDecision, PolicyEngine, PolicyState, TimeWindow,
    DEFAULT_CONFIG_TOML,
};

// LUCID M5 — local-or-DHT router. The Ollama HTTP layer wraps this
// instead of calling `Worker::execute` directly; the router decides
// per-request whether to dispatch locally, relay to a peer over
// `/phase/job-relay/1.0.0`, or refuse.
pub use dht_transport::PhaseNetDhtTransport;
pub use router::{
    make_inbound_relay_handler, make_inbound_relay_handlers, make_inbound_relay_stream_handler,
    DeterministicVerificationEligibility, InboundRelayHandlers, ReceiptVerification,
    RedundantCheckResult, RedundantVerificationConfig, RouteDecision, RouteVia, Router,
    RouterError, RELAY_TIMEOUT,
};
