pub mod elf;
pub mod software;
pub mod toolchain;

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use eyre::{ensure, Result, WrapErr as _};
use serde::Serialize;
use serde_json::Value;

pub fn repository_root() -> Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .wrap_err("resolve repository root")?;
    ensure!(
        output.status.success(),
        "git rev-parse --show-toplevel failed"
    );
    let root = String::from_utf8(output.stdout)?;
    fs::canonicalize(root.trim()).wrap_err("canonicalize repository root")
}

pub fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value).wrap_err("serialize canonical JSON value")?;
    let value = sort_json(value);
    let mut encoded = serde_json::to_vec(&value).wrap_err("encode canonical JSON")?;
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
