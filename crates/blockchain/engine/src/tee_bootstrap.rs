//! Consensus-thread TEE bootstrap: run the one-time committee coordination at
//! startup (exactly like the consensus DKG), assemble the canonical OST3 payload,
//! and hand it to the payload builder via the bridge so the **block-1** proposer
//! injects it (slice 5.1). `committee_snapshot_block` is the fixed block 1 - the
//! known injection target, mirroring how `BoundaryOutcome` lands at block 1 - so
//! there is no run-time block-number ambiguity.
//!
//! The secret operations stay in the enclave; this glue only adapts the
//! consensus P2P channel to [`BootstrapGossip`] and signs the payload with the
//! validator's EVM key.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use alloy_primitives::{keccak256, Address, B256};
use commonware_codec::Encode as _;
use commonware_cryptography::bls12381;
use commonware_p2p::{Receiver as P2pReceiver, Recipients, Sender as P2pSender};
use commonware_runtime::Clock;

use outbe_primitives::signer::OutbeEvmSigner;
use outbe_primitives::tee_attestation_v1::{
    AttestationEvidenceV1, AttestationMode, AttestationOperationV1, DcapEvidenceV1,
    EnclaveInitializationManifestV1, GramineDirectEvidenceV1, NodeIdV1, RegistrationIntentV1,
    TeePolicyV1, ValidatorNodeBindingV1, ENCLAVE_ID_DOMAIN_V1, MAX_ATTESTATION_EVIDENCE_BYTES,
};
use outbe_primitives::tee_bootstrap_v2::{
    TeeBootstrapAuthorityV2, TeeBootstrapParticipantSubmissionV2, TeeBootstrapV2,
};
use outbe_primitives::tee_signatures::recover_signer;
use outbe_tee::protocol::{EnclaveRequest, EnclaveResponse};
use outbe_tee::tee_dkg::{
    run_tee_dkg_ceremony, CeremonyCoordinator, CeremonyError, DkgGossip, DkgWireMessage,
};
use outbe_tee::{acquire_dcap_collateral_v1, RuntimeEnclaveClient};

/// Minimal opaque carrier used by the one-time OST3 startup coordinator.
#[allow(async_fn_in_trait)]
pub trait BootstrapGossip {
    async fn broadcast(&mut self, bytes: Vec<u8>) -> Result<(), CeremonyError>;
    async fn recv(&mut self) -> Option<Vec<u8>>;
}

const DELIVERY_DATA: u8 = 0xd0;
const DELIVERY_ACK: u8 = 0xd1;
const DELIVERY_ID_LEN: usize = 32;
const DELIVERY_INITIAL_RETRY_TICKS: u32 = 1;
const DELIVERY_MAX_RETRY_TICKS: u32 = 16;
const DELIVERY_MAX_PENDING_MESSAGES: usize = 1024;
const DELIVERY_MAX_PENDING_BYTES: usize = 16 * 1024 * 1024;
const DELIVERY_ID_DOMAIN: &[u8] = b"outbe:tee-delivery:v1";
const OST3_SUBMISSION: u8 = 0x30;
const OST3_SIGNATURE: u8 = 0x31;
const OST3_SUBMISSION_FIXED_BYTES: usize =
    1 + 4 + ValidatorNodeBindingV1::CANONICAL_LEN + 65 + 65 + 65 + 64;
const OST3_SIGNATURE_BYTES: usize = 1 + 32 + 20 + 65;
const OST3_SCOPE_DOMAIN: &[u8] = b"outbe/tee/bootstrap-v2-gossip/v1";
const OST3_DEV_NODE_HOST_DOMAIN: &[u8] = b"outbe/tee/dev-node-host/v1";
const OST3_BINDING_ID_DOMAIN: &[u8] = b"outbe/tee/bootstrap-binding/v1";

mod coordination;
mod message;
mod submission;
mod validation;

pub use coordination::coordinate_tee_bootstrap_v2;
use message::Ost3WireMessage;
pub use submission::build_local_tee_bootstrap_submission_v2;
use validation::{bootstrap_evidence_kind, validate_submission, BootstrapEvidenceKind};

mod delivery;
mod gossip;
mod identity;
mod startup;
#[cfg(test)]
mod tests;

use delivery::{ack_envelope, new_delivery_tracker, receive_delivery, DeliveryTracker};
pub use gossip::{CommonwareBootstrapGossip, CommonwareDkgGossip};
pub use startup::{
    query_enclave_offer_public, run_tee_bootstrap_v2_at_startup, run_tee_dkg_at_startup,
};

/// Envelope tag for a ceremony DKG message on the TEE-DKG channel.
const DKG_ENV_CEREMONY: u8 = 0x00;
/// Envelope tag for an enclave-identity announcement on the TEE-DKG channel.
const DKG_ENV_IDENTITY: u8 = 0x01;
