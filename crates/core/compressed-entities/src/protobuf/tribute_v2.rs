//! Canonical encrypted Tribute body. Other entity schemas remain at V1.

use outbe_primitives::tribute_encryption::{EncryptedTributeV2, TributeContextV2};

use super::*;

pub const TRIBUTE_BODY_SCHEMA_V2: u32 = 2;

pub fn encode_tribute_v2(body: &EncryptedTributeV2) -> Result<Vec<u8>, CanonicalBodyError> {
    if !body.has_valid_encoding() {
        return Err(CanonicalBodyError::InvalidEncryptedTribute);
    }
    let context = &body.context;
    let mut output = Vec::with_capacity(320);
    encode_bytes_field(1, context.tribute_id.as_slice(), &mut output);
    encode_bytes_field(2, context.owner.as_slice(), &mut output);
    encode_optional_varint_field(3, u64::from(context.worldwide_day.value()), &mut output);
    // One ciphertext holds issuance BE32 followed by nominal BE32.
    encode_bytes_field(4, &body.encrypted_amounts, &mut output);
    encode_optional_varint_field(5, u64::from(context.issuance_currency), &mut output);
    encode_bytes_field(6, &body.encrypted_creator_public_key, &mut output);
    encode_optional_varint_field(7, u64::from(context.reference_currency), &mut output);
    encode_bytes_field(
        8,
        &context.tribute_price_minor.to_be_bytes::<32>(),
        &mut output,
    );
    encode_optional_varint_field(
        9,
        u64::from(context.exclude_from_intex_issuance),
        &mut output,
    );
    encode_optional_varint_field(10, context.chain_id, &mut output);
    encode_bytes_field(11, context.offer_input_hash.as_slice(), &mut output);
    Ok(output)
}

pub fn decode_tribute_v2(bytes: &[u8]) -> Result<EncryptedTributeV2, CanonicalBodyError> {
    let mut fields = Fields::new(bytes);
    let tribute_id = WwdEntityId::try_from(required_bytes(&mut fields, 1)?)?;
    let owner = Address::from(fixed_bytes::<20>(required_bytes(&mut fields, 2)?, 2)?);
    let worldwide_day = WorldwideDay::new(optional_u32(&mut fields, 3)?);
    let encrypted_amounts = required_bytes(&mut fields, 4)?.to_vec();
    let issuance_currency = optional_u16(&mut fields, 5)?;
    let encrypted_creator_public_key = required_bytes(&mut fields, 6)?.to_vec();
    let reference_currency = optional_u16(&mut fields, 7)?;
    let tribute_price_minor = decode_u256(required_bytes(&mut fields, 8)?, 8)?;
    let exclude_from_intex_issuance = optional_bool(&mut fields, 9)?;
    let chain_id = optional_varint(&mut fields, 10)?;
    let offer_input_hash = B256::from(fixed_bytes::<32>(required_bytes(&mut fields, 11)?, 11)?);
    fields.finish()?;
    let body = EncryptedTributeV2 {
        context: TributeContextV2 {
            chain_id,
            tribute_id,
            owner,
            worldwide_day,
            issuance_currency,
            reference_currency,
            tribute_price_minor,
            exclude_from_intex_issuance,
            offer_input_hash,
        },
        encrypted_creator_public_key,
        encrypted_amounts,
    };
    if encode_tribute_v2(&body)? != bytes {
        return Err(CanonicalBodyError::NonCanonicalEncoding);
    }
    Ok(body)
}

pub fn decode_stored_tribute_v2(bytes: &[u8]) -> Result<EncryptedTributeV2, CanonicalBodyError> {
    let stored = crate::decode_stored_body(bytes)?;
    if stored.schema_version() != TRIBUTE_BODY_SCHEMA_V2 {
        return Err(CanonicalBodyError::UnsupportedSchema {
            actual: stored.schema_version(),
        });
    }
    decode_tribute_v2(stored.payload())
}
