//! Canonical compressed-body identities, encodings, and commitments.

mod commitment;
mod identity;
mod protobuf;
mod state;

pub use commitment::{
    body_commitment, derive_poseidon_entity_id, identity_field, pbytes, Commitment,
    CommitmentError, ACTIVE_COMMITMENT_SCHEME, CES1_TAG_BASE, TAG_BODY, TAG_BYTES_ABSORB,
    TAG_BYTES_FINAL, TAG_BYTES_INIT, TAG_ID, TAG_KEY, TAG_LEAF, TAG_SMT_BASE, TAG_SMT_NORMAL,
    TAG_SMT_ZERO,
};
pub use identity::{EntityId36, EntityIdError};
pub use protobuf::{
    decode_nod_bucket_v1, decode_nod_item_v1, decode_stored_nod_bucket_v1,
    decode_stored_nod_item_v1, decode_stored_tribute_v1, decode_tribute_v1, encode_nod_bucket_v1,
    encode_nod_item_v1, encode_tribute_v1, CanonicalBodyError, NodBucketBodyV1, NodItemBodyV1,
    StoredBody, TributeBodyV1, BODY_SCHEMA_V1,
};
pub use state::CommitmentState;

#[cfg(test)]
mod tests;
