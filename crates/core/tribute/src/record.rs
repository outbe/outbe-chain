//! Public Tribute records and explicit transient calculation views.

use std::ops::Deref;

use alloy_primitives::{Address, U256};
use outbe_compressed_entities::{CanonicalBodyError, StoredBody, WwdEntityId};
use outbe_primitives::{time::WorldwideDay, tribute_encryption::EncryptedTributeV2};

use crate::TributeData;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TributeMetadata {
    pub tribute_id: WwdEntityId,
    pub owner: Address,
    pub worldwide_day: WorldwideDay,
    pub issuance_currency: u16,
    pub reference_currency: u16,
    pub tribute_price_minor: U256,
    pub exclude_from_intex_issuance: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Body {
    Legacy(TributeData),
    Encrypted(EncryptedTributeV2),
}

/// Public metadata is readable without an enclave. Amounts require an explicit
/// private calculation read; canonical serialization always retains ciphertext.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TributeRecord {
    metadata: TributeMetadata,
    body: Body,
}

impl Deref for TributeRecord {
    type Target = TributeMetadata;
    fn deref(&self) -> &Self::Target {
        &self.metadata
    }
}

pub fn from_encrypted(body: EncryptedTributeV2) -> TributeRecord {
    let context = &body.context;
    let metadata = TributeMetadata {
        tribute_id: context.tribute_id,
        owner: context.owner,
        worldwide_day: context.worldwide_day,
        issuance_currency: context.issuance_currency,
        reference_currency: context.reference_currency,
        tribute_price_minor: context.tribute_price_minor,
        exclude_from_intex_issuance: context.exclude_from_intex_issuance,
    };
    TributeRecord {
        metadata,
        body: Body::Encrypted(body),
    }
}

pub fn from_legacy(body: TributeData) -> TributeRecord {
    let metadata = TributeMetadata {
        tribute_id: body.tribute_id,
        owner: body.owner,
        worldwide_day: body.worldwide_day,
        issuance_currency: body.issuance_currency,
        reference_currency: body.reference_currency,
        tribute_price_minor: body.tribute_price_minor,
        exclude_from_intex_issuance: body.exclude_from_intex_issuance,
    };
    TributeRecord {
        metadata,
        body: Body::Legacy(body),
    }
}

pub fn decode_payload(schema: u32, payload: &[u8]) -> Result<TributeRecord, CanonicalBodyError> {
    match schema {
        outbe_compressed_entities::BODY_SCHEMA_V1 => {
            outbe_compressed_entities::decode_tribute_v1(payload)
                .map(crate::from_canonical_body)
                .map(from_legacy)
        }
        outbe_compressed_entities::TRIBUTE_BODY_SCHEMA_V2 => {
            outbe_compressed_entities::decode_tribute_v2(payload).map(from_encrypted)
        }
        actual => Err(CanonicalBodyError::UnsupportedSchema { actual }),
    }
}

pub fn decode_stored(bytes: &[u8]) -> Result<TributeRecord, CanonicalBodyError> {
    let stored = outbe_compressed_entities::decode_stored_body(bytes)?;
    decode_payload(stored.schema_version(), stored.payload())
}

/// Both canonical payload encodings are strict and disjoint: the V1 amount
/// is 32 bytes, while the V2 packed amount ciphertext is 88 bytes. This
/// preserves the original OCOMP chunk bytes and their authenticated digest.
pub fn decode_canonical(payload: &[u8]) -> Result<TributeRecord, CanonicalBodyError> {
    outbe_compressed_entities::decode_tribute_v2(payload)
        .map(from_encrypted)
        .or_else(|_| decode_payload(outbe_compressed_entities::BODY_SCHEMA_V1, payload))
}

impl TributeRecord {
    pub fn stored_body(&self) -> Result<StoredBody, CanonicalBodyError> {
        match &self.body {
            Body::Legacy(body) => StoredBody::new(
                outbe_compressed_entities::BODY_SCHEMA_V1,
                outbe_compressed_entities::encode_tribute_v1(&crate::canonical_body(body))?,
            ),
            Body::Encrypted(body) => StoredBody::new(
                outbe_compressed_entities::TRIBUTE_BODY_SCHEMA_V2,
                outbe_compressed_entities::encode_tribute_v2(body)?,
            ),
        }
    }

    pub(crate) fn public_amount_attributes(
        &self,
    ) -> Result<
        (serde_json::Value, serde_json::Value, serde_json::Value),
        outbe_primitives::error::PrecompileError,
    > {
        match &self.body {
            Body::Legacy(body) => Ok((
                serde_json::json!(body.issuance_amount_minor.to_string()),
                serde_json::json!(body.nominal_amount_minor.to_string()),
                serde_json::Value::Null,
            )),
            Body::Encrypted(body) => {
                let bundle =
                    alloy_primitives::Bytes::from(body.encrypted_amounts.clone()).to_string();
                Ok((
                    serde_json::json!({"ciphertext":bundle,"word":0}),
                    serde_json::json!({"ciphertext":bundle,"word":1}),
                    serde_json::json!(body),
                ))
            }
        }
    }

    pub fn encrypted(&self) -> Option<&EncryptedTributeV2> {
        match &self.body {
            Body::Encrypted(body) => Some(body),
            _ => None,
        }
    }

    /// Use only after authenticating the original encrypted canonical body.
    pub fn calculation_view(&self) -> Result<TributeData, outbe_tee::TransportError> {
        let body = match &self.body {
            Body::Legacy(body) => return Ok(body.clone()),
            Body::Encrypted(body) => body,
        };
        let amounts = crate::enclave_client::read_amounts(body)?;
        Ok(TributeData {
            tribute_id: self.tribute_id,
            owner: self.owner,
            worldwide_day: self.worldwide_day,
            issuance_amount_minor: amounts.issuance_amount_minor,
            nominal_amount_minor: amounts.nominal_amount_minor,
            issuance_currency: self.issuance_currency,
            reference_currency: self.reference_currency,
            tribute_price_minor: self.tribute_price_minor,
            exclude_from_intex_issuance: self.exclude_from_intex_issuance,
        })
    }
}
