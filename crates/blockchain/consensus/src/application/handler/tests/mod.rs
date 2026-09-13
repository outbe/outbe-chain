use super::ProposalPayloadTrace;
use alloy_primitives::{address, Address, Bytes, B256};
use commonware_codec::Encode as _;
use commonware_consensus::{
    simplex::types::{Finalization, Proposal, Subject},
    types::{Epoch, Round, View},
};
use commonware_cryptography::{
    bls12381::{self, primitives::variant::MinSig},
    certificate::Scheme as _,
    Hasher, Sha256, Signer as _,
};
use commonware_parallel::Sequential;
use commonware_utils::ordered::Quorum as _;
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::reshare_artifact::ConsensusHeaderArtifact;

use crate::dkg_manager::{self, Mailbox as DkgManagerMailbox};
use crate::finalization::attestation::{validate_consensus_metadata, AttestationVerdict};
use crate::finalization::util::build_signer_bitmap;
use crate::hybrid::{HybridScheme, HybridSchemeProvider};

use super::{validate_header_consensus_artifacts, CommitteeProvider, Digest, ValidatorRole};
use crate::test_fixtures::*;

// Finalizer fatal forwarding and `ReplayClassification` tests moved.
// - The forwarding tests are gone with the deleted finalizer worker.
// - The replay-classification tests were ported alongside the helper
//   into `crate::finalization::util` (step 17). See
//   `finalization/util.rs::tests` for the same coverage.

mod header_artifacts;

mod proposal_trace;
