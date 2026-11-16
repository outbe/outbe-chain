use std::collections::BTreeMap;

use eyre::Result;
use serde_json::Value;

pub mod elf;
pub mod software;
pub mod toolchain;

pub fn canonical_json(value: &Value) -> Result<Vec<u8>> {
    let mut encoded = serde_json::to_vec(&sort_json(value.clone()))?;
    encoded.push(b'\n');
    Ok(encoded)
}

fn sort_json(value: Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.into_iter().map(sort_json).collect()),
        Value::Object(values) => {
            let sorted = values
                .into_iter()
                .map(|(key, value)| (key, sort_json(value)))
                .collect::<BTreeMap<_, _>>();
            Value::Object(sorted.into_iter().collect())
        }
        scalar => scalar,
    }
}
