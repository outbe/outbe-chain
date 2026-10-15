//! Independent Rust reference for bounded Lysis Program V1.
//!
//! Independence is structural: this crate has no dependency on the production
//! Lysis crate, Outbe domain crates, Alloy primitives or shared arithmetic
//! helpers. It consumes the frozen JSONL contract and implements the specified
//! operation order with arbitrary-precision integers.

mod arithmetic;
mod corpus;
mod distribution;
mod evaluate;
mod failure;
mod json;
#[cfg(test)]
mod tests;

use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use eyre::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub use crate::json::canonical_json;
use crate::{corpus::load_cases, evaluate::evaluate, json::strict_json};

const REFERENCE_PATH: &str = "testing/lysis-v1-reference/src";

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CheckReport {
    pub status: String,
    pub case_count: usize,
    pub case_ids: Vec<String>,
    pub expected_outputs_sha256: String,
    pub reference_sha256: String,
    pub case_file_sha256: String,
    pub mismatches: Vec<String>,
    pub integrity_errors: Vec<String>,
}

#[must_use]
pub fn repository_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("reference crate is under testing")
        .to_path_buf()
}

#[must_use]
pub fn default_vectors_path() -> PathBuf {
    repository_root().join("crates/core/lysis/vectors/lysis-v1/cases.jsonl")
}

#[must_use]
pub fn default_manifest_path() -> PathBuf {
    repository_root().join("crates/core/lysis/vectors/lysis-v1/manifest.json")
}

#[must_use]
pub fn reference_source_dir() -> PathBuf {
    repository_root().join(REFERENCE_PATH)
}

/// Hashes the `LC_ALL=C sha256sum *.rs` listing of the reference source directory.
pub fn reference_sha256() -> Result<String> {
    let directory = reference_source_dir();
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&directory)
        .wrap_err_with(|| format!("failed reading {}", directory.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_file() && !name.starts_with('.') && name.ends_with(".rs") {
            names.push(name);
        }
    }
    names.sort();
    let mut listing = String::new();
    for name in names {
        let digest = sha256_file(&directory.join(&name))?;
        listing.push_str(&format!("{digest}  {name}\n"));
    }
    Ok(sha256_bytes(listing.as_bytes()))
}

pub fn render(cases_path: &Path) -> Result<Vec<Vec<u8>>> {
    load_cases(cases_path)?
        .into_iter()
        .map(|mut case| {
            case.expected = evaluate(&case);
            canonical_json(&case)
        })
        .collect()
}

pub fn write(cases_path: &Path) -> Result<()> {
    let rendered = render(cases_path)?;
    let temporary = cases_path.with_extension("jsonl.tmp");
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)
        .wrap_err_with(|| format!("failed creating {}", temporary.display()))?;
    for line in rendered {
        output.write_all(&line)?;
        output.write_all(b"\n")?;
    }
    output.sync_all()?;
    drop(output);
    std::fs::rename(&temporary, cases_path)?;
    Ok(())
}

pub fn check(cases_path: &Path, manifest_path: &Path) -> Result<CheckReport> {
    let cases = load_cases(cases_path)?;
    let mut mismatches = Vec::new();
    let mut expected_outputs = Vec::with_capacity(cases.len());
    for case in &cases {
        let actual = evaluate(case);
        expected_outputs.push(actual.clone());
        if case.expected != actual {
            mismatches.push(case.case_id.clone());
        }
    }

    let manifest: Value = strict_json(
        &std::fs::read_to_string(manifest_path)
            .wrap_err_with(|| format!("failed reading {}", manifest_path.display()))?,
    )?;
    let reference_sha256 = reference_sha256()?;
    let case_file_sha256 = sha256_file(cases_path)?;
    let mut integrity_errors = Vec::new();
    if manifest["case_count"].as_u64() != u64::try_from(cases.len()).ok() {
        integrity_errors.push("case_count".to_owned());
    }
    if manifest["case_file_sha256"].as_str() != Some(&case_file_sha256) {
        integrity_errors.push("case_file_sha256".to_owned());
    }
    if manifest["reference_sha256"].as_str() != Some(&reference_sha256) {
        integrity_errors.push("reference_sha256".to_owned());
    }
    if manifest["reference_path"].as_str() != Some(REFERENCE_PATH) {
        integrity_errors.push("reference_path".to_owned());
    }
    if cases.iter().any(|case| case.requirements.is_empty()) {
        integrity_errors.push("case_requirements".to_owned());
    }

    let mut output_bytes = Vec::new();
    for value in &expected_outputs {
        output_bytes.extend_from_slice(&canonical_json(value)?);
        output_bytes.push(b'\n');
    }
    let status = if mismatches.is_empty() && integrity_errors.is_empty() {
        "PASS"
    } else {
        "FAIL"
    };
    Ok(CheckReport {
        status: status.to_owned(),
        case_count: cases.len(),
        case_ids: cases.into_iter().map(|case| case.case_id).collect(),
        expected_outputs_sha256: sha256_bytes(&output_bytes),
        reference_sha256,
        case_file_sha256,
        mismatches,
        integrity_errors,
    })
}

pub fn sha256_file(path: &Path) -> Result<String> {
    Ok(sha256_bytes(&std::fs::read(path).wrap_err_with(|| {
        format!("failed reading {}", path.display())
    })?))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
