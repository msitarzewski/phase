// SPDX-License-Identifier: AGPL-3.0-or-later

//! LUCID inference daemon — open GPU inference flagship built on the Phase
//! substrate. Implements the `phase-protocol::Worker` trait for inference
//! workloads.
//!
//! ## Current status: spike
//!
//! This crate currently ships a minimal Ollama-compatible `/api/chat`
//! endpoint backed by an in-tree `EchoWorker` that reverses the user's last
//! message. It exists to validate the `phase-protocol::Worker` trait against
//! a real external boundary (the Ollama wire format + a real Ollama client)
//! *before* the full LUCID M4 surface lands.
//!
//! Real inference (llama.cpp / MLX) and the full Ollama API surface are
//! LUCID M2 / M4 territory.

pub mod dht_transport;
pub mod echo;
pub mod ollama;
pub mod policy;
pub mod registry;
pub mod router;
pub mod worker_llama;

// LUCID M2: the production inference worker. Shells out to `llama-server`,
// streams tokens back through the protocol, and signs receipts. Exported at
// the crate root so the binary can switch between EchoWorker (no GPU
// required, used by CI) and LlamaCppWorker (production path) via CLI flag.
pub use worker_llama::{LlamaCppConfig, LlamaCppWorker};

// Public re-exports for the M6 model registry. Downstream code (the
// router in M5, the Ollama `/api/tags` handler in M4) consumes these as
// `lucidd::ModelRegistry` etc. without having to know about the module
// layout. See `registry` module docs for the trust model and TTL story.
pub use registry::{
    DhtTransport, ModelCapabilities, ModelCid, ModelRegistry,
    SignedModelAdvertisement, ADVERTISEMENT_SCHEMA_VERSION, ADVERTISEMENT_TTL,
    MODEL_KEY_PREFIX, TTL_REFRESH_INTERVAL,
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
    make_inbound_relay_handler, RouteDecision, RouteVia, Router, RouterError, RELAY_TIMEOUT,
};
