# phase-protocol

The protocol surface of Phase: defines the `JobSpec` enum (Wasm, Inference, and future workload variants) and the async `Worker` trait. Implementing `Worker` is what makes a binary a Phase node — Plasm, LUCID, and any future implementation all sit behind this single interface.
