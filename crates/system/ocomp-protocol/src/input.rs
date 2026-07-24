use alloy_primitives::{B256, U256};

use crate::{
    common::BoundedBytes,
    error::ProtocolError,
    hash::hash_framed,
    registry::HashDomain,
    schema::{impl_top_level_codec, require, wire_enum_u8, wire_struct, SchemaLimits},
};

wire_enum_u8! {
    pub enum InputChunkKind {
        Tribute = 1,
        Fidelity = 2,
        Oracle = 3,
    }
}

wire_enum_u8! {
    pub enum OpeningSourceKind {
        Fidelity = 1,
        Oracle = 2,
    }
}

wire_enum_u8! {
    pub enum Compression {
        None = 0,
    }
}

wire_struct! {
    pub struct CheckpointIdentityV1 {
        pub finalized_block_number: u64,
        pub finalized_block_hash: B256,
        pub finalized_state_root: B256,
        pub finalized_ce_root: B256,
        pub ce_schema_version: u16,
    }
}

wire_struct! {
    pub struct InputChunkRefV1 {
        pub kind: InputChunkKind,
        pub ordinal: u32,
        pub record_count: u32,
        pub first_key: BoundedBytes,
        pub last_key_inclusive: BoundedBytes,
        pub encoded_bytes: u64,
        pub semantic_digest: B256,
        pub transport_digest: B256,
    }
}

wire_struct! {
    pub struct AuthenticatedOpeningV1 {
        pub source_kind: OpeningSourceKind,
        pub canonical_subject_key: BoundedBytes,
        pub canonical_value: BoundedBytes,
        pub opening_codec_id: B256,
        pub canonical_opening: BoundedBytes,
    }
}

wire_struct! {
    pub struct AuthenticatedInputChunkV1 {
        pub protocol_bundle_hash: B256,
        pub job_id: B256,
        pub kind: InputChunkKind,
        pub ordinal: u32,
        pub canonical_records_or_openings: Vec<BoundedBytes>,
    }
    validate = validate_input_chunk;
}
impl_top_level_codec!(AuthenticatedInputChunkV1, AuthenticatedInputChunkV1);

wire_struct! {
    pub struct InputManifestV1 {
        pub protocol_bundle_hash: B256,
        pub job_id: B256,
        pub attempt: u32,
        pub checkpoint: CheckpointIdentityV1,
        pub wwd: u32,
        pub sealed_tribute_collection_key: B256,
        pub sealed_tribute_collection_root: B256,
        pub tribute_count: u32,
        pub tribute_nominal_total: U256,
        pub input_chunk_count: u32,
        pub input_chunk_list_root: B256,
        pub fidelity_opening_root: B256,
        pub oracle_opening_root: B256,
        pub exact_encoded_bytes: u64,
        pub exact_record_count: u32,
        pub body_codec_id: B256,
        pub opening_codec_registry_hash: B256,
        pub compression: Compression,
    }
    validate = validate_input_manifest;
}
impl_top_level_codec!(InputManifestV1, InputManifestV1);

impl AuthenticatedInputChunkV1 {
    pub fn semantic_digest(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        require(
            self.canonical_records_or_openings.len() <= limits.max_chunk_items,
            "input chunk item cap",
        )?;
        hash_framed(HashDomain::InputChunk, &self.encode_canonical(limits)?)
    }
}

impl InputManifestV1 {
    pub fn validate_semantics(&self, limits: &SchemaLimits) -> Result<(), ProtocolError> {
        require(
            self.tribute_count > 0
                && self.input_chunk_count > 0
                && self.exact_record_count >= self.tribute_count
                && self.exact_encoded_bytes > 0
                && !self.input_chunk_list_root.is_zero(),
            "input manifest committed population",
        )?;
        let _ = limits;
        Ok(())
    }

    pub fn manifest_hash(&self, limits: &SchemaLimits) -> Result<B256, ProtocolError> {
        self.validate_semantics(limits)?;
        hash_framed(HashDomain::InputManifest, &self.encode_canonical(limits)?)
    }
}

fn validate_input_chunk(
    chunk: &AuthenticatedInputChunkV1,
    limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    require(
        chunk.canonical_records_or_openings.len() <= limits.max_chunk_items,
        "input chunk item cap",
    )
}

fn validate_input_manifest(
    manifest: &InputManifestV1,
    limits: &SchemaLimits,
) -> Result<(), ProtocolError> {
    manifest.validate_semantics(limits)
}
