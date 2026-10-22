use super::is_lower_hex;
use eyre::{bail, eyre, Result, WrapErr};
use serde_json::Value;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

pub(super) fn require_collateral(
    evidence: &Value,
    expected_pck_ca: &str,
    consensus_timestamp: u64,
) -> Result<()> {
    let string_at = |pointer: &str| {
        evidence
            .pointer(pointer)
            .and_then(Value::as_str)
            .ok_or_else(|| eyre!("{expected_pck_ca} DCAP evidence lacks {pointer}"))
    };
    let u64_at = |pointer: &str| {
        evidence
            .pointer(pointer)
            .and_then(Value::as_u64)
            .ok_or_else(|| eyre!("{expected_pck_ca} DCAP evidence lacks {pointer}"))
    };
    let consensus_timestamp_i64 = i64::try_from(consensus_timestamp)
        .map_err(|_| eyre!("{expected_pck_ca} consensus timestamp exceeds i64"))?;
    if u64_at("/collateral/component_count")? != 8
        || string_at("/collateral/pck_crl/kind")? != expected_pck_ca
        || string_at("/collateral/root_crl/kind")? != "root"
    {
        bail!("{expected_pck_ca} DCAP evidence lacks the exact collateral matrix");
    }
    for (label, pointer) in [
        ("PCK CRL", "/collateral/pck_crl"),
        ("root CRL", "/collateral/root_crl"),
    ] {
        let issuer = string_at(&format!("{pointer}/issuer"))?;
        let this_update = string_at(&format!("{pointer}/this_update"))?;
        let next_update = string_at(&format!("{pointer}/next_update"))?;
        let size = u64_at(&format!("{pointer}/size"))?;
        let sha256 = string_at(&format!("{pointer}/sha256"))?;
        let retained_path = string_at(&format!("{pointer}/path"))?;
        let expected_path = if pointer.ends_with("pck_crl") {
            "collateral/pck.crl.der"
        } else {
            "collateral/root-ca.crl.der"
        };
        let valid_issuer = !issuer.is_empty() && issuer.is_ascii();
        let valid_interval =
            is_utc_second(this_update) && is_utc_second(next_update) && this_update < next_update;
        if size == 0 || !is_lower_hex(sha256, 64) || !valid_issuer || !valid_interval {
            bail!("{expected_pck_ca} {label} provenance is invalid");
        }
        require_issuer(issuer, pointer, expected_pck_ca, label)?;
        let this_update_time = OffsetDateTime::parse(this_update, &Rfc3339)
            .wrap_err_with(|| format!("parse {expected_pck_ca} {label} this_update"))?;
        let next_update_time = OffsetDateTime::parse(next_update, &Rfc3339)
            .wrap_err_with(|| format!("parse {expected_pck_ca} {label} next_update"))?;
        if this_update_time.unix_timestamp() > consensus_timestamp_i64
            || consensus_timestamp_i64 >= next_update_time.unix_timestamp()
        {
            bail!("{expected_pck_ca} {label} was not current at the consensus timestamp");
        }
        let artifact = evidence
            .get("artifacts")
            .and_then(Value::as_object)
            .and_then(|artifacts| artifacts.get(expected_path));
        if retained_path != expected_path
            || artifact
                .and_then(|record| record.get("size"))
                .and_then(Value::as_u64)
                != Some(size)
            || artifact
                .and_then(|record| record.get("sha256"))
                .and_then(Value::as_str)
                != Some(sha256)
        {
            bail!("{expected_pck_ca} retained {label} does not match its provenance record");
        }
    }
    Ok(())
}

fn require_issuer(issuer: &str, pointer: &str, expected_pck_ca: &str, label: &str) -> Result<()> {
    let expected_issuer_marker = if pointer.ends_with("pck_crl") {
        if expected_pck_ca == "processor" {
            "Intel SGX PCK Processor CA"
        } else {
            "Intel SGX PCK Platform CA"
        }
    } else {
        "Intel SGX Root CA"
    };
    if !issuer.contains(expected_issuer_marker) {
        bail!("{expected_pck_ca} {label} provenance has the wrong issuer");
    }
    Ok(())
}

fn is_utc_second(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 20
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            10 => *byte == b'T',
            13 | 16 => *byte == b':',
            19 => *byte == b'Z',
            _ => byte.is_ascii_digit(),
        })
        && OffsetDateTime::parse(value, &Rfc3339).is_ok()
}
