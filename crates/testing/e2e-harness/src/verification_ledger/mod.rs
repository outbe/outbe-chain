//! Versioned, domain-neutral verification-ledger primitives.
//!
//! Domain modules own their requirement taxonomy and closure policy. This
//! module owns only strict parsing, source/evidence identity and fail-closed
//! references shared by every domain pack.

mod ledger;
mod schema;
mod verify;

pub use ledger::{read_yaml_strict, VerificationLedger};
pub use schema::{
    AssertionRecordV1, AssertionStatus, ClosurePolicyV1, DomainEvidenceV1, DomainPack,
    DomainPackIndexEntry, DomainPackVerificationV1, EvidenceIdentityPolicyV1, EvidenceTestV1,
    MemberDigestV1, RequirementDefinitionV1, SourceIdentityV1, TestDefinitionV1, TestDiscoveryV1,
    VerificationLedgerIndex, DOMAIN_PACK_SCHEMA_VERSION, VERIFICATION_LEDGER_INDEX_KIND,
    VERIFICATION_LEDGER_SCHEMA_VERSION,
};
pub use verify::{
    capture_source_identity, validate_domain_evidence_manifest, validate_source_identity,
    verify_domain_evidence, verify_evidence_set, verify_member_digests, verify_source_checkout,
    DomainEvidenceBundle,
};
