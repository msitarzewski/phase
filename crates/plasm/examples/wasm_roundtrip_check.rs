//! SEC-02 verification harness: run examples/hello.wasm through wasmtime + WASI
//! preview1 with a piped stdin ("Hello, World") and capture stdout. Asserts the
//! output is byte-identical "dlroW ,olleH" and prints the module SHA-256 hash.
//!
//! This mirrors `plasm::wasm::runtime::Wasm3Runtime::execute_sync` exactly
//! (same Engine/Config/Linker/WASI-preview1 setup) but pipes stdin and captures
//! stdout so the round-trip result is observable. Used to prove the wasmtime
//! bump does not change execution semantics.
//!
//! Run: cargo run -p plasm --example wasm_roundtrip_check

use std::io::Read;
use wasmtime::*;
use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::preview1::WasiP1Ctx;
use wasmtime_wasi::WasiCtxBuilder;

fn main() -> anyhow::Result<()> {
    let wasm_bytes = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../examples/hello.wasm"
    ))?;

    // Module hash (same computation as runtime.rs compute_module_hash).
    let module_hash = {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(&wasm_bytes);
        format!("sha256:{}", hex::encode(h.finalize()))
    };

    let mut config = Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config)?;

    let stdin = MemoryInputPipe::new("Hello, World");
    let stdout = MemoryOutputPipe::new(4096);
    let wasi: WasiP1Ctx = WasiCtxBuilder::new()
        .stdin(stdin)
        .stdout(stdout.clone())
        .inherit_stderr()
        .build_p1();

    let mut store = Store::new(&engine, wasi);
    store.set_fuel(300 * 1_000_000)?;

    let module = Module::from_binary(&engine, &wasm_bytes)?;
    let mut linker: Linker<WasiP1Ctx> = Linker::new(&engine);
    wasmtime_wasi::preview1::add_to_linker_sync(&mut linker, |ctx| ctx)?;

    let instance = linker.instantiate(&mut store, &module)?;
    let func = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
    let _ = func.call(&mut store, ());

    drop(store);
    let bytes = stdout.contents();
    let mut out = String::new();
    bytes.as_ref().read_to_string(&mut out)?;

    println!("module_hash = {module_hash}");
    println!("stdout      = {out:?}");

    assert_eq!(out, "dlroW ,olleH", "WASM round-trip output mismatch");
    assert_eq!(
        module_hash,
        "sha256:11bfd18c60e980ed9375a91d9e9de49b1a75cbac66781ae34ff148e2008b769c",
        "module hash changed"
    );
    println!("OK: hello.wasm round-trip byte-identical, module hash unchanged");
    Ok(())
}
