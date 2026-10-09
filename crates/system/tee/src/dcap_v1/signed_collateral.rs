//! Signed Intel collateral JSON grammar for DCAP V1.
//!
//! This module checks the canonical TCB-info and QE-identity envelopes and
//! reads the metadata that consensus compares with the policy and the PCK
//! identity. Signature checks stay in the native QVL.

use std::{collections::BTreeSet, fmt};

use outbe_primitives::tee_attestation_v1::TeePolicyV1;
use serde::{
    de::{DeserializeSeed, Error as _, MapAccess, SeqAccess, Visitor},
    Deserialize,
};
use serde_json::value::RawValue;

use crate::dcap_protocol::DcapRejectCodeV1;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedTcbInfo<'a> {
    #[serde(borrow, rename = "tcbInfo")]
    body: &'a RawValue,
    signature: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedQeIdentity<'a> {
    #[serde(borrow, rename = "enclaveIdentity")]
    body: &'a RawValue,
    signature: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TcbInfoBody<'a> {
    id: &'a str,
    version: u8,
    issue_date: &'a str,
    next_update: &'a str,
    fmspc: &'a str,
    pce_id: &'a str,
    tcb_evaluation_data_number: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct QeIdentityBody<'a> {
    id: &'a str,
    version: u8,
    issue_date: &'a str,
    next_update: &'a str,
    tcb_evaluation_data_number: u32,
}

pub(super) struct TcbInfoMetadata {
    pub(super) issue_date: u64,
    pub(super) next_update: u64,
    pub(super) fmspc: [u8; 6],
    pub(super) pce_id: [u8; 2],
    pub(super) tcb_evaluation_data_number: u32,
}

pub(super) struct QeIdentityMetadata {
    pub(super) issue_date: u64,
    pub(super) next_update: u64,
    pub(super) tcb_evaluation_data_number: u32,
}

pub(super) fn parse_signed_tcb_info(
    bytes: &[u8],
    policy: &TeePolicyV1,
) -> Result<TcbInfoMetadata, DcapRejectCodeV1> {
    let signed: SignedTcbInfo<'_> =
        serde_json::from_slice(bytes).map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    validate_signed_json_wrapper(bytes, "tcbInfo", signed.body, signed.signature)?;
    let body: TcbInfoBody<'_> = serde_json::from_str(signed.body.get())
        .map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    if body.id != "SGX" || body.version != policy.tcb_info_schema_version {
        return Err(DcapRejectCodeV1::PlatformIdentityMismatch);
    }
    let issue_date = parse_canonical_timestamp(body.issue_date)?;
    let next_update = parse_canonical_timestamp(body.next_update)?;
    if issue_date >= next_update {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    Ok(TcbInfoMetadata {
        issue_date,
        next_update,
        fmspc: decode_upper_hex(body.fmspc)?,
        pce_id: decode_upper_hex(body.pce_id)?,
        tcb_evaluation_data_number: body.tcb_evaluation_data_number,
    })
}

pub(super) fn parse_signed_qe_identity(
    bytes: &[u8],
    policy: &TeePolicyV1,
) -> Result<QeIdentityMetadata, DcapRejectCodeV1> {
    let signed: SignedQeIdentity<'_> =
        serde_json::from_slice(bytes).map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    validate_signed_json_wrapper(bytes, "enclaveIdentity", signed.body, signed.signature)?;
    let body: QeIdentityBody<'_> = serde_json::from_str(signed.body.get())
        .map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?;
    if body.id != "QE" || body.version != policy.qe_identity_schema_version {
        return Err(DcapRejectCodeV1::QeTcbRejected);
    }
    let issue_date = parse_canonical_timestamp(body.issue_date)?;
    let next_update = parse_canonical_timestamp(body.next_update)?;
    if issue_date >= next_update {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    Ok(QeIdentityMetadata {
        issue_date,
        next_update,
        tcb_evaluation_data_number: body.tcb_evaluation_data_number,
    })
}

fn validate_signed_json_wrapper(
    bytes: &[u8],
    field: &str,
    body: &RawValue,
    signature: &str,
) -> Result<(), DcapRejectCodeV1> {
    reject_duplicate_json_keys(bytes)?;
    if !is_json_object_text(body.get()) || !is_lowercase_hex_signature(signature) {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    let canonical = format!(r#"{{"{field}":{},"signature":"{signature}"}}"#, body.get());
    if canonical.as_bytes() != bytes {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    Ok(())
}

fn is_json_object_text(value: &str) -> bool {
    value.starts_with('{') && value.ends_with('}')
}

/// Intel signs collateral with a 64-byte signature in lowercase hexadecimal.
fn is_lowercase_hex_signature(signature: &str) -> bool {
    signature.len() == 128
        && signature
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn reject_duplicate_json_keys(bytes: &[u8]) -> Result<(), DcapRejectCodeV1> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    RejectDuplicateJsonKeys
        .deserialize(&mut deserializer)
        .and_then(|()| deserializer.end())
        .map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)
}

#[derive(Clone, Copy)]
struct RejectDuplicateJsonKeys;

impl<'de> DeserializeSeed<'de> for RejectDuplicateJsonKeys {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(RejectDuplicateJsonKeysVisitor)
    }
}

struct RejectDuplicateJsonKeysVisitor;

impl<'de> Visitor<'de> for RejectDuplicateJsonKeysVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("canonical JSON without duplicate object keys")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<(), E> {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<(), E> {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<(), E> {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<(), E> {
        Ok(())
    }

    fn visit_str<E>(self, _value: &str) -> Result<(), E> {
        Ok(())
    }

    fn visit_string<E>(self, _value: String) -> Result<(), E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_some<D>(self, deserializer: D) -> Result<(), D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        RejectDuplicateJsonKeys.deserialize(deserializer)
    }

    fn visit_unit<E>(self) -> Result<(), E> {
        Ok(())
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<(), A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence
            .next_element_seed(RejectDuplicateJsonKeys)?
            .is_some()
        {}
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> Result<(), A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key) {
                return Err(A::Error::custom("duplicate JSON object key"));
            }
            map.next_value_seed(RejectDuplicateJsonKeys)?;
        }
        Ok(())
    }
}

fn parse_canonical_timestamp(value: &str) -> Result<u64, DcapRejectCodeV1> {
    let bytes = value.as_bytes();
    if bytes.len() != 20 {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    for (index, separator) in [
        (4, b'-'),
        (7, b'-'),
        (10, b'T'),
        (13, b':'),
        (16, b':'),
        (19, b'Z'),
    ] {
        if bytes[index] != separator {
            return Err(DcapRejectCodeV1::CollateralNonCanonical);
        }
    }
    for (index, byte) in bytes.iter().enumerate() {
        if matches!(index, 4 | 7 | 10 | 13 | 16 | 19) {
            continue;
        }
        if !byte.is_ascii_digit() {
            return Err(DcapRejectCodeV1::CollateralNonCanonical);
        }
    }
    let timestamp =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
            .map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)?
            .unix_timestamp();
    u64::try_from(timestamp).map_err(|_| DcapRejectCodeV1::CollateralNonCanonical)
}

fn decode_upper_hex<const N: usize>(value: &str) -> Result<[u8; N], DcapRejectCodeV1> {
    if value.len() != N * 2 {
        return Err(DcapRejectCodeV1::CollateralNonCanonical);
    }
    let mut decoded = [0; N];
    for (index, pair) in value.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        let high = upper_hex_nibble(pair[0])?;
        let low = upper_hex_nibble(pair[1])?;
        decoded[index] = (high << 4) | low;
    }
    Ok(decoded)
}

fn upper_hex_nibble(value: u8) -> Result<u8, DcapRejectCodeV1> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => Err(DcapRejectCodeV1::CollateralNonCanonical),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wrapper_result(document: &str, body: &str, signature: &str) -> Result<(), DcapRejectCodeV1> {
        let body: &RawValue = serde_json::from_str(body).unwrap();
        validate_signed_json_wrapper(document.as_bytes(), "tcbInfo", body, signature)
    }

    #[test]
    fn signed_json_wrapper_needs_an_object_body_and_a_lowercase_hex_signature() {
        let signature = "0123456789abcdef".repeat(8);
        let document = |body: &str, signature: &str| {
            format!(r#"{{"tcbInfo":{body},"signature":"{signature}"}}"#)
        };
        assert_eq!(
            wrapper_result(
                &document(r#"{"a":1}"#, &signature),
                r#"{"a":1}"#,
                &signature
            ),
            Ok(())
        );
        let uppercase = signature.to_uppercase();
        let short = &signature[..127];
        let long = format!("{signature}0");
        let non_hex = format!("{}g", &signature[..127]);
        for (body, signature) in [
            ("[1]", signature.as_str()),
            ("\"{}\"", signature.as_str()),
            (r#"{"a":1}"#, uppercase.as_str()),
            (r#"{"a":1}"#, short),
            (r#"{"a":1}"#, long.as_str()),
            (r#"{"a":1}"#, non_hex.as_str()),
        ] {
            assert_eq!(
                wrapper_result(&document(body, signature), body, signature),
                Err(DcapRejectCodeV1::CollateralNonCanonical)
            );
        }
        assert_eq!(
            wrapper_result(
                &document(r#"{"a":1}"#, &signature),
                r#"{"a":2}"#,
                &signature
            ),
            Err(DcapRejectCodeV1::CollateralNonCanonical)
        );
    }
}
