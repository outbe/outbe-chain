use super::*;

/// Canonical plaintext v1 gem payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GemBodyV1 {
    pub gem_id: WwdEntityId,
    pub owner: Address,
    pub gem_type: u32,
    pub promis_load_minor: U256,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    pub issued_at: u64,
    pub bucket_key: B256,
    pub state: u32,
    pub called_at: u64,
    pub settled_at: u64,
}

impl GemBodyV1 {
    #[must_use]
    pub fn entity_id(&self) -> WwdEntityId {
        self.gem_id
    }
}

pub fn encode_gem_v1(body: &GemBodyV1) -> Result<Vec<u8>, CanonicalBodyError> {
    validate_gem_state(body.state)?;
    let mut output = Vec::new();
    encode_bytes_field(1, body.gem_id.as_slice(), &mut output);
    encode_bytes_field(2, body.owner.as_slice(), &mut output);
    encode_optional_varint_field(3, u64::from(body.gem_type), &mut output);
    encode_bytes_field(4, &body.promis_load_minor.to_be_bytes::<32>(), &mut output);
    encode_optional_varint_field(5, u64::from(body.issuance_currency), &mut output);
    encode_optional_varint_field(6, u64::from(body.reference_currency), &mut output);
    encode_optional_varint_field(7, body.issued_at, &mut output);
    encode_bytes_field(8, body.bucket_key.as_slice(), &mut output);
    encode_optional_varint_field(9, u64::from(body.state), &mut output);
    encode_optional_varint_field(10, body.called_at, &mut output);
    encode_optional_varint_field(11, body.settled_at, &mut output);
    Ok(output)
}

pub fn decode_gem_v1(bytes: &[u8]) -> Result<GemBodyV1, CanonicalBodyError> {
    let mut fields = Fields::new(bytes);
    let gem_id = WwdEntityId::try_from(required_bytes(&mut fields, 1)?)?;
    let owner = Address::from(fixed_bytes::<20>(required_bytes(&mut fields, 2)?, 2)?);
    let gem_type = optional_u32(&mut fields, 3)?;
    let promis_load_minor = decode_u256(required_bytes(&mut fields, 4)?, 4)?;
    let issuance_currency = optional_u16(&mut fields, 5)?;
    let reference_currency = optional_u16(&mut fields, 6)?;
    let issued_at = optional_varint(&mut fields, 7)?;
    let bucket_key = B256::from(fixed_bytes::<32>(required_bytes(&mut fields, 8)?, 8)?);
    let state = optional_u32(&mut fields, 9)?;
    let called_at = optional_varint(&mut fields, 10)?;
    let settled_at = optional_varint(&mut fields, 11)?;
    fields.finish()?;
    let body = GemBodyV1 {
        gem_id,
        owner,
        gem_type,
        promis_load_minor,
        issuance_currency,
        reference_currency,
        issued_at,
        bucket_key,
        state,
        called_at,
        settled_at,
    };
    if encode_gem_v1(&body)? != bytes {
        return Err(CanonicalBodyError::NonCanonicalEncoding);
    }
    Ok(body)
}

pub fn decode_stored_gem_v1(bytes: &[u8]) -> Result<GemBodyV1, CanonicalBodyError> {
    let stored = decode_active_stored_body(bytes)?;
    decode_gem_v1(stored.payload())
}

/// Canonical plaintext v1 gem bucket payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GemBucketBodyV1 {
    pub bucket_key: B256,
    pub worldwide_day: WorldwideDay,
    pub entry_price_minor: U256,
    pub reference_currency: u16,
    pub settled_gems: u64,
}

impl GemBucketBodyV1 {
    #[must_use]
    pub fn entity_id(&self) -> WwdEntityId {
        WwdEntityId::from_day_and_digest(self.worldwide_day, self.bucket_key)
    }
}

pub fn encode_gem_bucket_v1(body: &GemBucketBodyV1) -> Result<Vec<u8>, CanonicalBodyError> {
    let mut output = Vec::new();
    encode_bytes_field(1, body.bucket_key.as_slice(), &mut output);
    encode_optional_varint_field(2, u64::from(body.worldwide_day.value()), &mut output);
    encode_bytes_field(3, &body.entry_price_minor.to_be_bytes::<32>(), &mut output);
    encode_optional_varint_field(4, u64::from(body.reference_currency), &mut output);
    encode_optional_varint_field(5, body.settled_gems, &mut output);
    Ok(output)
}

pub fn decode_gem_bucket_v1(bytes: &[u8]) -> Result<GemBucketBodyV1, CanonicalBodyError> {
    let fields = decode_bucket_fields(bytes)?;
    let body = GemBucketBodyV1 {
        bucket_key: fields.bucket_key,
        worldwide_day: fields.worldwide_day,
        entry_price_minor: fields.entry_price_minor,
        reference_currency: fields.reference_currency,
        settled_gems: fields.settled,
    };
    canonical_decoded(body, bytes, encode_gem_bucket_v1)
}

pub fn decode_stored_gem_bucket_v1(bytes: &[u8]) -> Result<GemBucketBodyV1, CanonicalBodyError> {
    let stored = decode_active_stored_body(bytes)?;
    decode_gem_bucket_v1(stored.payload())
}

fn validate_gem_state(state: u32) -> Result<(), CanonicalBodyError> {
    if state != 0 && state != 2 {
        return Err(CanonicalBodyError::IntegerOutOfRange { field: 9 });
    }
    Ok(())
}
