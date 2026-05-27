// SPDX-License-Identifier: Apache-2.0

//! Canonical JSON serialization for signing.
//!
//! Identical algorithm to `phase-manifest::canonical`: re-serializes via
//! `serde_json::Value` with object keys ordered lexicographically and no
//! whitespace. Kept as a per-crate copy (rather than extracted into a
//! shared utility) so neither crate has to depend on a third one for what
//! is ~60 lines of stable code; if a third Phase crate needs the same
//! function later, both should switch to a `phase-canonical-json` crate.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::ReceiptError;

pub(crate) fn to_canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, ReceiptError> {
    let v = serde_json::to_value(value)
        .map_err(|e| ReceiptError::Canonicalization(e.to_string()))?;
    let sorted = sort_value(v);
    serde_json::to_vec(&sorted).map_err(|e| ReceiptError::Canonicalization(e.to_string()))
}

fn sort_value(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut sorted = Map::new();
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            for k in keys {
                let val = map.get(&k).cloned().unwrap_or(Value::Null);
                sorted.insert(k, sort_value(val));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sort_value).collect()),
        other => other,
    }
}
