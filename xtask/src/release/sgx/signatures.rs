use super::read_canonical_json;
use super::require_nonempty_regular_file;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;

use eyre::bail;
use eyre::eyre;
use eyre::Result;
use eyre::WrapErr;

use serde_json::Value;

use std::path::Path;

pub(super) fn verify_cosign_image_signature(path: &Path, expected_digest: &str) -> Result<()> {
    let verification: Value = read_canonical_json(path)?;
    let entries = verification
        .as_array()
        .ok_or_else(|| eyre!("Cosign image verification must be a JSON array"))?;
    let expected = format!("sha256:{expected_digest}");
    let matched = entries.iter().any(|entry| {
        entry
            .pointer("/critical/image/docker-manifest-digest")
            .and_then(Value::as_str)
            == Some(expected.as_str())
            && entry.pointer("/critical/type").and_then(Value::as_str)
                == Some("cosign container image signature")
    });
    if !matched {
        bail!("Cosign image verification does not bind the exact OCI digest");
    }
    Ok(())
}

pub(super) fn verified_cosign_attestation(
    path: &Path,
    expected_digest: &str,
    expected_predicate_type: &str,
) -> Result<Value> {
    require_nonempty_regular_file(path, "release evidence")?;
    let verification: Value = read_canonical_json(path)?;
    let envelopes = verification
        .as_array()
        .ok_or_else(|| eyre!("Cosign attestation verification must be a JSON array"))?;
    for envelope in envelopes {
        let Some(payload) = envelope.get("payload").and_then(Value::as_str) else {
            continue;
        };
        let decoded = BASE64
            .decode(payload)
            .wrap_err("decode verified Cosign DSSE payload")?;
        let statement: Value =
            serde_json::from_slice(&decoded).wrap_err("parse verified Cosign statement")?;
        let subject_matches = statement
            .get("subject")
            .and_then(Value::as_array)
            .is_some_and(|subjects| {
                subjects.iter().any(|subject| {
                    subject.pointer("/digest/sha256").and_then(Value::as_str)
                        == Some(expected_digest)
                })
            });
        if statement.get("_type").and_then(Value::as_str)
            == Some("https://in-toto.io/Statement/v0.1")
            && statement.get("predicateType").and_then(Value::as_str)
                == Some(expected_predicate_type)
            && subject_matches
        {
            return Ok(statement);
        }
    }
    Err(eyre!(
        "Cosign attestation verification does not bind predicate {expected_predicate_type} to the exact OCI digest"
    ))
}
