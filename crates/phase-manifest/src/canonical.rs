// SPDX-License-Identifier: Apache-2.0

//! Canonical JSON serialization for signing.
//!
//! The signing format must be deterministic across implementations (Rust
//! verifier, PHP SDK in `php-sdk/`, future Go/Python clients). Plain
//! `serde_json::to_vec` is not deterministic because hash-map fields and
//! `serde_json::Value::Object` use insertion order, not lexicographic order.
//!
//! We therefore re-serialize a `serde_json::Value` with a small recursive
//! pass that orders all object keys lexicographically and emits no
//! insignificant whitespace. This matches RFC 8785 (JCS) for the subset of
//! JSON we care about — Phase payloads do not use floats with edge cases
//! like `-0.0` or `NaN`, so we don't need full JCS number formatting.

use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::ManifestError;

/// Serialize `value` to canonical JSON bytes:
///   - object keys sorted lexicographically,
///   - no whitespace,
///   - arrays in original order,
///   - numbers in the form `serde_json` emits (which is shortest-roundtrip
///     for finite f64 and exact for integers — sufficient for Phase use).
pub(crate) fn to_canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, ManifestError> {
    let v = serde_json::to_value(value)
        .map_err(|e| ManifestError::Canonicalization(e.to_string()))?;
    let sorted = sort_value(v);
    serde_json::to_vec(&sorted).map_err(|e| ManifestError::Canonicalization(e.to_string()))
}

fn sort_value(v: Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut sorted = Map::new();
            // BTreeMap-style ordering by sorting keys first.
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            for k in keys {
                // `map` was consumed by `keys()` above — we need to retrieve
                // the entry. `Map` doesn't let us move out, so we clone the
                // value (cheap enough for manifest payloads). To avoid the
                // clone we'd need to drain via `into_iter` and sort the vec.
                let val = map.get(&k).cloned().unwrap_or(Value::Null);
                sorted.insert(k, sort_value(val));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sort_value).collect()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;

    #[derive(Serialize)]
    struct Outer {
        zeta: u32,
        alpha: Inner,
        beta: Vec<i32>,
    }

    #[derive(Serialize)]
    struct Inner {
        y: bool,
        x: String,
    }

    #[test]
    fn canonical_orders_object_keys() {
        let v = Outer {
            zeta: 1,
            alpha: Inner {
                y: true,
                x: "hi".into(),
            },
            beta: vec![3, 2, 1],
        };
        let bytes = to_canonical_bytes(&v).expect("canonical");
        let s = String::from_utf8(bytes).expect("utf8");
        // Outer keys sorted: alpha, beta, zeta.
        // Inner keys sorted: x, y.
        // Array order preserved.
        assert_eq!(s, r#"{"alpha":{"x":"hi","y":true},"beta":[3,2,1],"zeta":1}"#);
    }

    #[test]
    fn canonical_is_stable_across_calls() {
        let v = Outer {
            zeta: 1,
            alpha: Inner {
                y: true,
                x: "hi".into(),
            },
            beta: vec![3, 2, 1],
        };
        let a = to_canonical_bytes(&v).unwrap();
        let b = to_canonical_bytes(&v).unwrap();
        assert_eq!(a, b);
    }
}
