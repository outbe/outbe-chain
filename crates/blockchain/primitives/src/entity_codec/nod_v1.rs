//! Canonical self-contained encrypted NOD bodies.
use super::*;
use crate::nod_encryption::{EncryptedNodV1, NodTermsV1};

pub const NOD_BODY_SCHEMA_V1: u32 = BODY_SCHEMA_V1;
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NodItemBodyV1 {
    pub encrypted: EncryptedNodV1,
    pub group_key: B256,
    pub issued_at: u64,
    pub is_settled: bool,
}
pub fn encode_nod_item_v1(body: &NodItemBodyV1) -> Result<Vec<u8>, CanonicalBodyError> {
    if !body.encrypted.has_valid_encoding() {
        return Err(CanonicalBodyError::InvalidEncryptedNod);
    }
    let t = &body.encrypted.terms;
    let mut out = Vec::with_capacity(320);
    encode_bytes_field(1, &t.nod_id.to_be_bytes::<32>(), &mut out);
    encode_bytes_field(2, t.owner.as_slice(), &mut out);
    encode_bytes_field(3, &body.encrypted.encrypted_gratis_amount, &mut out);
    // Field 4 is retired (formerly encrypted_creator_public_key); never reuse tag 4.
    encode_optional_varint_field(5, t.chain_id, &mut out);
    // Field 6 is retired (formerly worldwide_day); never reuse tag 6.
    encode_optional_varint_field(7, u64::from(t.league_id), &mut out);
    encode_bytes_field(8, &t.entry_price_minor.to_be_bytes::<32>(), &mut out);
    encode_optional_varint_field(9, u64::from(t.issuance_currency), &mut out);
    encode_optional_varint_field(10, u64::from(t.reference_currency), &mut out);
    encode_bytes_field(11, body.group_key.as_slice(), &mut out);
    encode_optional_varint_field(12, body.issued_at, &mut out);
    encode_optional_varint_field(13, u64::from(body.is_settled), &mut out);
    encode_bytes_field(14, body.encrypted.encryption_binding.as_slice(), &mut out);
    encode_optional_varint_field(15, u64::from(t.emission_day), &mut out);
    Ok(out)
}
pub fn decode_nod_item_v1(bytes: &[u8]) -> Result<NodItemBodyV1, CanonicalBodyError> {
    let mut f = Fields::new(bytes);
    let nod_id = decode_u256(required_bytes(&mut f, 1)?, 1)?;
    let owner = Address::from(fixed_bytes::<20>(required_bytes(&mut f, 2)?, 2)?);
    let encrypted_gratis_amount = required_bytes(&mut f, 3)?.to_vec();
    let chain_id = optional_varint(&mut f, 5)?;
    let league_id = optional_u16(&mut f, 7)?;
    let entry_price_minor = decode_u256(required_bytes(&mut f, 8)?, 8)?;
    let issuance_currency = optional_u16(&mut f, 9)?;
    let reference_currency = optional_u16(&mut f, 10)?;
    let group_key = B256::from(fixed_bytes::<32>(required_bytes(&mut f, 11)?, 11)?);
    let issued_at = optional_varint(&mut f, 12)?;
    let is_settled = optional_bool(&mut f, 13)?;
    let encryption_binding = B256::from(fixed_bytes::<32>(required_bytes(&mut f, 14)?, 14)?);
    let emission_day = optional_u32(&mut f, 15)?;
    f.finish()?;
    let body = NodItemBodyV1 {
        encrypted: EncryptedNodV1 {
            terms: NodTermsV1 {
                chain_id,
                nod_id,
                owner,
                emission_day,
                league_id,
                entry_price_minor,
                issuance_currency,
                reference_currency,
            },
            encryption_binding,
            encrypted_gratis_amount,
        },
        group_key,
        issued_at,
        is_settled,
    };
    if encode_nod_item_v1(&body)? != bytes {
        return Err(CanonicalBodyError::NonCanonicalEncoding);
    }
    Ok(body)
}
pub fn decode_stored_nod_item_v1(bytes: &[u8]) -> Result<NodItemBodyV1, CanonicalBodyError> {
    let stored = super::decode_stored_body(bytes)?;
    if stored.schema_version() != NOD_BODY_SCHEMA_V1 {
        return Err(CanonicalBodyError::UnsupportedSchema {
            actual: stored.schema_version(),
        });
    }
    decode_nod_item_v1(stored.payload())
}
