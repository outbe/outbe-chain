//! Reject ambiguous custom-error selectors in the published precompile ABIs.

use std::{collections::BTreeMap, fs, path::Path};

use alloy_primitives::keccak256;
use serde_json::Value;

fn canonical_type(input: &Value) -> String {
    let ty = input["type"].as_str().expect("ABI parameter has a type");
    match ty.strip_prefix("tuple") {
        Some(suffix) => {
            let members = input["components"]
                .as_array()
                .expect("tuple has components")
                .iter()
                .map(canonical_type)
                .collect::<Vec<_>>()
                .join(",");
            format!("({members}){suffix}")
        }
        None => ty.to_owned(),
    }
}

fn error_signature(error: &Value) -> String {
    let name = error["name"].as_str().expect("ABI error has a name");
    let inputs = error["inputs"]
        .as_array()
        .expect("ABI error has inputs")
        .iter()
        .map(canonical_type)
        .collect::<Vec<_>>()
        .join(",");
    format!("{name}({inputs})")
}

#[test]
fn published_precompile_errors_have_unambiguous_selectors() {
    let directory =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../contracts/precompiles/abi-export");
    let mut signatures = BTreeMap::<[u8; 4], String>::new();
    for entry in fs::read_dir(directory).expect("published ABI directory exists") {
        let path = entry.expect("ABI entry is readable").path();
        if path.extension().is_none_or(|extension| extension != "json") {
            continue;
        }
        let abi: Value = serde_json::from_slice(&fs::read(&path).expect("ABI is readable"))
            .expect("published ABI is valid JSON");
        for error in abi.as_array().expect("published ABI is an array") {
            if error["type"] != "error" {
                continue;
            }
            let signature = error_signature(error);
            let selector = keccak256(&signature).0[..4]
                .try_into()
                .expect("selector has four bytes");
            if let Some(previous) = signatures.insert(selector, signature.clone()) {
                // Shared signatures across interfaces describe the same ABI condition.
                assert_eq!(
                    previous,
                    signature,
                    "selector collision in {}",
                    path.display()
                );
            }
        }
    }
    assert!(!signatures.is_empty(), "published ABIs must declare errors");
}
