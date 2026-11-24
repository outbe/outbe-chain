use alloy_primitives::{Address, B256, U256};
use outbe_system_common::time::WorldwideDay;
use thiserror::Error;

use crate::wwd_entity_id::WwdEntityId;

mod nod_v1;
pub use nod_v1::{
    decode_nod_item_v1, decode_stored_nod_item_v1, encode_nod_item_v1, NodItemBodyV1,
    NOD_BODY_SCHEMA_V1,
};

/// Fork-supported schema for the canonical plaintext body messages.
pub const BODY_SCHEMA_V1: u32 = 1;

/// Canonical v1 Nod group payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodGroupBodyV1 {
    pub group_key: B256,
    pub full_day_after_issue: WorldwideDay,
    /// ISO 4217 numeric code the group's call price is quoted in, omitted when zero.
    pub reference_currency: u16,
    pub settled_nods: u64,
}

impl NodGroupBodyV1 {
    /// Returns the canonical group identity `WWD_BE4 || group_key`.
    #[must_use]
    pub fn entity_id(&self) -> WwdEntityId {
        WwdEntityId::from_day_and_digest(self.full_day_after_issue, self.group_key)
    }
}

/// Canonical stored value. The schema version is authenticated by the leaf,
/// while the payload is the exact typed Protobuf message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StoredBody {
    schema_version: u32,
    payload: Vec<u8>,
}

impl StoredBody {
    /// Wraps a non-empty payload with an explicit non-zero schema version.
    pub fn new(schema_version: u32, payload: Vec<u8>) -> Result<Self, CanonicalBodyError> {
        if schema_version == 0 {
            return Err(CanonicalBodyError::ZeroSchemaVersion);
        }
        if payload.is_empty() {
            return Err(CanonicalBodyError::EmptyPayload);
        }
        Ok(Self {
            schema_version,
            payload,
        })
    }

    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Encodes the one canonical StoredBody wire representation.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(self.payload.len() + 16);
        encode_varint_field(1, u64::from(self.schema_version), &mut output);
        encode_bytes_field(2, &self.payload, &mut output);
        output
    }
}

/// Decodes a stored body and requires its canonical wire representation.
pub fn decode_stored_body(bytes: &[u8]) -> Result<StoredBody, CanonicalBodyError> {
    let mut fields = Fields::new(bytes);
    let schema_version = required_varint(&mut fields, 1)?;
    let schema_version = u32::try_from(schema_version)
        .map_err(|_| CanonicalBodyError::IntegerOutOfRange { field: 1 })?;
    let payload = required_bytes(&mut fields, 2)?.to_vec();
    fields.finish()?;
    let body = StoredBody::new(schema_version, payload)?;
    if body.encode() != bytes {
        return Err(CanonicalBodyError::NonCanonicalEncoding);
    }
    Ok(body)
}

/// Encodes the canonical v1 Nod group payload.
pub fn encode_nod_group_v1(body: &NodGroupBodyV1) -> Result<Vec<u8>, CanonicalBodyError> {
    let mut output = Vec::with_capacity(128);
    encode_bytes_field(1, body.group_key.as_slice(), &mut output);
    encode_optional_varint_field(2, u64::from(body.full_day_after_issue.value()), &mut output);
    encode_optional_varint_field(4, u64::from(body.reference_currency), &mut output);
    encode_optional_varint_field(5, body.settled_nods, &mut output);
    Ok(output)
}

/// Strictly decodes one canonical v1 Nod group payload.
pub fn decode_nod_group_v1(bytes: &[u8]) -> Result<NodGroupBodyV1, CanonicalBodyError> {
    let fields = decode_group_fields(bytes)?;
    let body = NodGroupBodyV1 {
        group_key: fields.group_key,
        full_day_after_issue: fields.full_day_after_issue,
        reference_currency: fields.reference_currency,
        settled_nods: fields.settled,
    };
    canonical_decoded(body, bytes, encode_nod_group_v1)
}

/// Decodes a StoredBody and requires its fork-active Nod group schema.
pub fn decode_stored_nod_group_v1(bytes: &[u8]) -> Result<NodGroupBodyV1, CanonicalBodyError> {
    let stored = decode_active_stored_body(bytes)?;
    decode_nod_group_v1(stored.payload())
}

fn decode_active_stored_body(bytes: &[u8]) -> Result<StoredBody, CanonicalBodyError> {
    let stored = decode_stored_body(bytes)?;
    if stored.schema_version() != BODY_SCHEMA_V1 {
        return Err(CanonicalBodyError::UnsupportedSchema {
            actual: stored.schema_version(),
        });
    }
    Ok(stored)
}

fn decode_u256(bytes: &[u8], field: u32) -> Result<U256, CanonicalBodyError> {
    Ok(U256::from_be_bytes(fixed_bytes::<32>(bytes, field)?))
}

fn fixed_bytes<const N: usize>(bytes: &[u8], field: u32) -> Result<[u8; N], CanonicalBodyError> {
    bytes
        .try_into()
        .map_err(|_| CanonicalBodyError::InvalidFixedWidth {
            field,
            expected: N,
            actual: bytes.len(),
        })
}

fn encode_optional_varint_field(field: u32, value: u64, output: &mut Vec<u8>) {
    if value != 0 {
        encode_varint_field(field, value, output);
    }
}

fn encode_varint_field(field: u32, value: u64, output: &mut Vec<u8>) {
    encode_varint(u64::from(field) << 3, output);
    encode_varint(value, output);
}

fn encode_bytes_field(field: u32, value: &[u8], output: &mut Vec<u8>) {
    encode_varint((u64::from(field) << 3) | 2, output);
    encode_varint(value.len() as u64, output);
    output.extend_from_slice(value);
}

fn encode_varint(mut value: u64, output: &mut Vec<u8>) {
    while value >= 0x80 {
        output.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}

fn varint_len(mut value: u64) -> usize {
    let mut len = 1;
    while value >= 0x80 {
        value >>= 7;
        len += 1;
    }
    len
}

#[derive(Clone, Copy)]
enum FieldValue<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

struct Field<'a> {
    number: u32,
    value: FieldValue<'a>,
}

struct Fields<'a> {
    input: &'a [u8],
    offset: usize,
    last_number: u32,
    pending: Option<Field<'a>>,
}

impl<'a> Fields<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self {
            input,
            offset: 0,
            last_number: 0,
            pending: None,
        }
    }

    fn take(&mut self, expected: u32) -> Result<Option<FieldValue<'a>>, CanonicalBodyError> {
        if self.pending.is_none() {
            self.pending = self.next_field()?;
        }
        match self.pending.as_ref() {
            Some(field) if field.number == expected => {
                Ok(self.pending.take().map(|field| field.value))
            }
            Some(field) if field.number < expected => Err(CanonicalBodyError::UnknownField {
                field: field.number,
            }),
            _ => Ok(None),
        }
    }

    fn finish(&mut self) -> Result<(), CanonicalBodyError> {
        if self.pending.is_none() {
            self.pending = self.next_field()?;
        }
        if let Some(field) = self.pending.as_ref() {
            return Err(CanonicalBodyError::UnknownField {
                field: field.number,
            });
        }
        Ok(())
    }

    fn next_field(&mut self) -> Result<Option<Field<'a>>, CanonicalBodyError> {
        if self.offset == self.input.len() {
            return Ok(None);
        }
        let key = self.read_varint()?;
        let number = u32::try_from(key >> 3).map_err(|_| CanonicalBodyError::MalformedProtobuf)?;
        if number == 0 {
            return Err(CanonicalBodyError::MalformedProtobuf);
        }
        if number <= self.last_number {
            return Err(CanonicalBodyError::NonAscendingOrDuplicateField { field: number });
        }
        self.last_number = number;
        let value = match key & 7 {
            0 => FieldValue::Varint(self.read_varint()?),
            2 => {
                let len = usize::try_from(self.read_varint()?)
                    .map_err(|_| CanonicalBodyError::MalformedProtobuf)?;
                let end = self
                    .offset
                    .checked_add(len)
                    .filter(|end| *end <= self.input.len())
                    .ok_or(CanonicalBodyError::MalformedProtobuf)?;
                let bytes = &self.input[self.offset..end];
                self.offset = end;
                FieldValue::Bytes(bytes)
            }
            wire => return Err(CanonicalBodyError::UnsupportedWireType { wire }),
        };
        Ok(Some(Field { number, value }))
    }

    fn read_varint(&mut self) -> Result<u64, CanonicalBodyError> {
        let start = self.offset;
        let mut value = 0_u64;
        for shift in (0..70).step_by(7) {
            let byte = *self
                .input
                .get(self.offset)
                .ok_or(CanonicalBodyError::MalformedProtobuf)?;
            self.offset += 1;
            if shift == 63 && byte > 1 {
                return Err(CanonicalBodyError::MalformedProtobuf);
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                if self.offset - start != varint_len(value) {
                    return Err(CanonicalBodyError::NonMinimalVarint);
                }
                return Ok(value);
            }
        }
        Err(CanonicalBodyError::MalformedProtobuf)
    }
}

fn required_varint(fields: &mut Fields<'_>, field: u32) -> Result<u64, CanonicalBodyError> {
    match fields.take(field)? {
        Some(FieldValue::Varint(0)) => Err(CanonicalBodyError::ExplicitDefault { field }),
        Some(FieldValue::Varint(value)) => Ok(value),
        Some(FieldValue::Bytes(_)) => Err(CanonicalBodyError::WrongWireType { field }),
        None => Err(CanonicalBodyError::MissingField { field }),
    }
}

fn optional_varint(fields: &mut Fields<'_>, field: u32) -> Result<u64, CanonicalBodyError> {
    match fields.take(field)? {
        Some(FieldValue::Varint(0)) => Err(CanonicalBodyError::ExplicitDefault { field }),
        Some(FieldValue::Varint(value)) => Ok(value),
        Some(FieldValue::Bytes(_)) => Err(CanonicalBodyError::WrongWireType { field }),
        None => Ok(0),
    }
}

fn required_bytes<'a>(fields: &mut Fields<'a>, field: u32) -> Result<&'a [u8], CanonicalBodyError> {
    match fields.take(field)? {
        Some(FieldValue::Bytes([])) => Err(CanonicalBodyError::ExplicitDefault { field }),
        Some(FieldValue::Bytes(bytes)) => Ok(bytes),
        Some(FieldValue::Varint(_)) => Err(CanonicalBodyError::WrongWireType { field }),
        None => Err(CanonicalBodyError::MissingField { field }),
    }
}

fn optional_u32(fields: &mut Fields<'_>, field: u32) -> Result<u32, CanonicalBodyError> {
    u32::try_from(optional_varint(fields, field)?)
        .map_err(|_| CanonicalBodyError::IntegerOutOfRange { field })
}

fn optional_u16(fields: &mut Fields<'_>, field: u32) -> Result<u16, CanonicalBodyError> {
    u16::try_from(optional_varint(fields, field)?)
        .map_err(|_| CanonicalBodyError::IntegerOutOfRange { field })
}

fn optional_bool(fields: &mut Fields<'_>, field: u32) -> Result<bool, CanonicalBodyError> {
    match optional_varint(fields, field)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(CanonicalBodyError::IntegerOutOfRange { field }),
    }
}

/// Strict canonical body validation failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CanonicalBodyError {
    #[error(transparent)]
    InvalidEntityId(#[from] core::array::TryFromSliceError),
    #[error("schema version zero is invalid")]
    ZeroSchemaVersion,
    #[error("stored body payload must not be empty")]
    EmptyPayload,
    #[error("malformed Protobuf wire data")]
    MalformedProtobuf,
    #[error("unsupported Protobuf wire type {wire}")]
    UnsupportedWireType { wire: u64 },
    #[error("field {field} has the wrong wire type")]
    WrongWireType { field: u32 },
    #[error("field {field} is missing")]
    MissingField { field: u32 },
    #[error("unknown field {field} for the declared schema")]
    UnknownField { field: u32 },
    #[error("field {field} is duplicated or not in ascending order")]
    NonAscendingOrDuplicateField { field: u32 },
    #[error("non-minimal Protobuf varint")]
    NonMinimalVarint,
    #[error("explicit default for non-optional scalar field {field}")]
    ExplicitDefault { field: u32 },
    #[error("field {field} is outside its declared integer range")]
    IntegerOutOfRange { field: u32 },
    #[error("field {field} must be exactly {expected} bytes, got {actual}")]
    InvalidFixedWidth {
        field: u32,
        expected: usize,
        actual: usize,
    },
    #[error("invalid encrypted NOD blob or identity/day encoding")]
    InvalidEncryptedNod,
    #[error("input is not the canonical Protobuf representation")]
    NonCanonicalEncoding,
    #[error("unsupported body schema version {actual}")]
    UnsupportedSchema { actual: u32 },
}

struct GroupFields {
    group_key: B256,
    full_day_after_issue: WorldwideDay,
    reference_currency: u16,
    settled: u64,
}

fn decode_group_fields(bytes: &[u8]) -> Result<GroupFields, CanonicalBodyError> {
    let mut fields = Fields::new(bytes);
    let body = GroupFields {
        group_key: B256::from(fixed_bytes::<32>(required_bytes(&mut fields, 1)?, 1)?),
        full_day_after_issue: WorldwideDay::new(optional_u32(&mut fields, 2)?),
        reference_currency: optional_u16(&mut fields, 4)?,
        settled: optional_varint(&mut fields, 5)?,
    };
    fields.finish()?;
    Ok(body)
}

fn canonical_decoded<T>(
    body: T,
    bytes: &[u8],
    encode: fn(&T) -> Result<Vec<u8>, CanonicalBodyError>,
) -> Result<T, CanonicalBodyError> {
    if encode(&body)? != bytes {
        return Err(CanonicalBodyError::NonCanonicalEncoding);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nod_encryption::{EncryptedNodV1, NodTermsV1, NOD_BLOB_LEN};

    fn sample_day() -> WorldwideDay {
        WorldwideDay::new(20_260_101)
    }

    #[test]
    fn canonical_entity_bodies_roundtrip_and_reject_malformed_wire() {
        let day = sample_day();
        let owner = Address::repeat_byte(0x22);

        // V1 encrypted Nod roundtrip.
        let mut nod_amt_blob = vec![0xdd; NOD_BLOB_LEN];
        nod_amt_blob[..8].copy_from_slice(&1u64.to_be_bytes());
        let nod_v1 = NodItemBodyV1 {
            encrypted: EncryptedNodV1 {
                terms: NodTermsV1 {
                    chain_id: 2026,
                    nod_id: U256::MAX,
                    owner,
                    emission_day: day.value(),
                    entry_price_minor: U256::from(1_000_000u64),
                    issuance_currency: 840,
                    reference_currency: 840,
                },
                encryption_binding: B256::repeat_byte(0x55),
                encrypted_gratis_amount: nod_amt_blob.clone(),
                encrypted_nominal_amount: nod_amt_blob,
            },
            group_key: B256::repeat_byte(0x44),
            issued_at: 1_700_000_000,
            is_settled: true,
            sub_series_id: 7,
        };
        let encoded_nod_v1 = encode_nod_item_v1(&nod_v1).unwrap();
        assert_eq!(decode_nod_item_v1(&encoded_nod_v1).unwrap(), nod_v1);
        let stored_nod_v1 = StoredBody::new(NOD_BODY_SCHEMA_V1, encoded_nod_v1)
            .unwrap()
            .encode();
        assert_eq!(decode_stored_nod_item_v1(&stored_nod_v1).unwrap(), nod_v1);

        // Non-canonical wire rejections on StoredBody:
        // 1) non-minimal varint tag (0x88, 0x00 instead of 0x08 for field 1 varint)
        assert!(matches!(
            decode_stored_body(&[0x88, 0x00, 0x01, 0x12, 0x01, 0xff]),
            Err(CanonicalBodyError::NonMinimalVarint)
        ));
        // 2) non-minimal varint value (0x81, 0x00 for value 1)
        assert!(matches!(
            decode_stored_body(&[0x08, 0x81, 0x00, 0x12, 0x01, 0xff]),
            Err(CanonicalBodyError::NonMinimalVarint)
        ));
        // 3) duplicate / non-ascending field tags (duplicate field 1)
        assert!(matches!(
            decode_stored_body(&[0x08, 0x01, 0x08, 0x01, 0x12, 0x01, 0xff]),
            Err(CanonicalBodyError::NonAscendingOrDuplicateField { field: 1 })
        ));
        // 4) explicit default scalar on optional field (field 2 = 0 in a Nod group)
        let mut with_explicit_zero = Vec::new();
        encode_bytes_field(1, B256::repeat_byte(0x44).as_slice(), &mut with_explicit_zero);
        encode_varint_field(2, 0, &mut with_explicit_zero);
        assert!(matches!(
            decode_nod_group_v1(&with_explicit_zero),
            Err(CanonicalBodyError::ExplicitDefault { field: 2 })
        ));
    }
}
