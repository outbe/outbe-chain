//! Names the golden files import from the domain crates.
pub use outbe_fidelity::{
    eip191_hash, fidelity_cohort_attestation_preimage, fidelity_cohort_canonical_hash,
    fidelity_query_attestation_preimage, fidelity_query_auth_message,
    fidelity_query_canonical_hash, fidelity_snapshot_attestation_preimage,
    fidelity_snapshot_canonical_hash, FidelityCohortRequest, FidelityCohortResult,
    FidelityLeagueEntry, FidelityQueryRequest, FidelityQueryResult, FidelitySnapshotRequest,
};
pub use outbe_gratis::{
    derive_gratis_keys_message, gratis_op_attestation_preimage, gratis_op_canonical_hash,
    GratisOpRequest, GratisOpResult,
};
pub use outbe_promis::{
    promis_op_attestation_preimage, promis_op_canonical_hash, PromisOpRequest, PromisOpResult,
};
pub use outbe_tribute::confidential::{
    inputs_canonical_hash, tribute_offer_attestation_preimage, EncryptedTributeOffer,
    TributeOfferResult,
};
