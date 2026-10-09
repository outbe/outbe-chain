//! Rust-only fixture for cross-crate OCOMP activation tests.
//!
//! This module is available only through the explicit `test-utils` feature.
//! It builds consensus-valid activation bytes and seeds the real Metadosis and
//! owner storage APIs. It does not provide a production activation shortcut.

#[cfg(test)]
use std::cell::Cell;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use alloy_primitives::{keccak256, Address, Bytes, Log, B256, U256};
use k256::ecdsa::{signature::hazmat::PrehashSigner, Signature, SigningKey};
use outbe_compressed_entities::{
    begin_block, partition_collection_key, AuthenticatedParentTree, CeWorkCheckpoint, CeWorkConfig,
    EntityRef, ExecutionScope, FinalLeafMutation, PartitionRef, ProvisionalTreeBatch,
};
#[cfg(test)]
use outbe_lysis::activation_v1::LysisOwnerReceiptsV1;
use outbe_nod::schema::NodContract;
#[cfg(test)]
use outbe_ocomp_protocol::league_snapshot::league_snapshot_key;
#[cfg(test)]
use outbe_ocomp_protocol::state::OcompJobStatus;
use outbe_ocomp_protocol::{
    committee::{
        validator_identity_hash_v1, OcompKeyRegistrationCoreV1, OcompKeyRegistrationV1,
        RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    common::{BoundedBytes, ProofBytes},
    hash::hash_framed,
    intent::{
        intent_storage_key, ActivationPreconditionsV1, ContributorTargetPreconditionV1, DayType,
        ExpectedFinalizedIntentBindingV1, FinalizedIntentProofV1, FinalizedIntentVerificationError,
        FinalizedRequestBindingV1, FrozenMetadosisValuesV1, JobIntentV1,
        MetadosisAttemptPreconditionV1, MetadosisExpectedStatus, NodTargetPreconditionV1,
        TributeInputBindingV1, VerifiedFinalizedIntentV1,
    },
    profile::{CapacityProfileV1, ProtocolBundleV1},
    receipts::{
        desis_request_brief_hash, ActivationOutcome, LimitSplitDestination,
        RequestLimitSplitReceiptV1,
    },
    registry::HashDomain,
    result::{
        lysis_v1_empty_semantic_event_root, CarryOverCreditActionV1, CarryOverReason,
        CompletionStatus, ConservationTotalsV1, ExactCountsV1, LysisArithmeticSummaryV1,
        LysisResultV1, MetadosisCompletionSummaryV1, ResultRootsV1,
    },
    state::RESULT_VOTE_MIN_FINALITY_DEPTH,
    vote::ResultVoteV1,
    SchemaLimits,
};
#[cfg(test)]
use outbe_primitives::addresses::STAKING_ADDRESS;
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::{COMPRESSED_ENTITIES_ADDRESS, METADOSIS_ADDRESS},
    error::{PrecompileError, Result as PrecompileResult},
    storage::{hashmap::HashMapStorageProvider, MetadosisMutationPurposeTag, StorageHandle},
};
use outbe_tribute::{DayPreAdmission, DayTotals, TributeContract};
use outbe_validatorset::{
    contract::ValidatorSet, read_ocomp_snapshot_extension, OcompSnapshotExtensionV1,
};

#[cfg(test)]
use crate::constants::{FORMING_PERIOD_HOURS, SECONDS_PER_HOUR, WAITING_PERIOD_HOURS};
#[cfg(test)]
use crate::errors::MetadosisError;
#[cfg(test)]
use crate::schema::{DayLimitFormationReceiptStateEntryExt, WorldwideDayEntryExt};
use crate::{
    constants::MAX_ACTIVE_WWDS,
    ocomp::{
        activation::{OcompFinalityAuthorityError, OcompFinalizedIntentAuthority},
        fork::{OcompForkInstallClassification, OcompForkInstallV1},
        schema::{poc_schema_limits, OcompRequestProfile},
    },
    schema::{day_type, status, MetadosisContract, WorldwideDay as WorldwideDayRecord},
};
pub const TEST_WWD: WorldwideDay = WorldwideDay::new(20_260_723);
pub const TEST_REQUEST_HEIGHT: u64 = 10;
pub const TEST_LOGICAL_TIME: u64 = 1_000;

/// Closed, test-only receipt corruptions retained from the original atomic
/// activation regression suite. The enum never crosses the private fixture
/// kernel or becomes part of the public semantic facade.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(crate) enum ActivationReceiptFault {
    Nod,
    Contributor,
    Tribute,
    CarryOver,
    RequestSplit,
}

#[cfg(test)]
thread_local! {
    static ACTIVATION_RECEIPT_FAULT: Cell<Option<ActivationReceiptFault>> =
        const { Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn inject_receipt_fault(
    request_receipt: &mut RequestLimitSplitReceiptV1,
    receipts: &mut LysisOwnerReceiptsV1,
) {
    match ACTIVATION_RECEIPT_FAULT.with(Cell::take) {
        Some(ActivationReceiptFault::Nod) => receipts.nod.nod_root = hash(210),
        Some(ActivationReceiptFault::Contributor) => {
            receipts.contributor.contributor_count += 1;
        }
        Some(ActivationReceiptFault::Tribute) => receipts.tribute.retired_generation += 1,
        Some(ActivationReceiptFault::CarryOver) => {
            receipts.carry_over.credited_unused_lysis_limit_minor += U256::from(1);
            receipts.carry_over.after_value += U256::from(1);
        }
        Some(ActivationReceiptFault::RequestSplit) => {
            request_receipt.logical_anchor += 1;
        }
        None => {}
    }
}

#[path = "test_support/activation.rs"]
mod activation;
#[path = "test_support/artifacts.rs"]
mod artifacts;
#[path = "test_support/committee.rs"]
mod committee;
#[path = "test_support/parent_tree.rs"]
mod parent_tree;
#[path = "test_support/wwd.rs"]
mod wwd;

pub use activation::{ActivationFixture, ActivationScenario, RollbackSnapshot};
#[cfg(test)]
pub use activation::{ActivationMetadata, SemanticSnapshot};
#[cfg(test)]
pub use artifacts::fixture_authority;
use artifacts::{bundle, capacity_profile, finality_proof, hash, intent, request_receipt, result};
pub use artifacts::{
    fork_install_fixture, lysis_result_for_intent, seed_registry_authority, FixedFinality,
};
pub use committee::signed_result_vote_for_intent;
pub(crate) use committee::{founder_registrations_for_validators, seed_validator_snapshot};
use committee::{ocomp_key_hash, sign, signing_key};
use parent_tree::begin_activation_scope;
pub(crate) use wwd::FixtureKernelExt;

/// Installs the persisted CE predecessor owned by the fixture kernel.
#[cfg(test)]
pub(crate) fn seed_ce_parent(storage: &StorageHandle<'_>, root: B256) -> PrecompileResult<()> {
    for (slot, value) in [
        (U256::ZERO, U256::from(4)),
        (U256::from(1), U256::from_be_slice(root.as_slice())),
    ] {
        storage.sstore(COMPRESSED_ENTITIES_ADDRESS, slot, value)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn seed_ce_genesis(storage: &StorageHandle<'_>) -> PrecompileResult<()> {
    let root = outbe_compressed_entities::sealed_root(B256::ZERO)
        .map_err(|error| PrecompileError::Fatal(format!("fixture CE genesis root: {error}")))?;
    seed_ce_parent(storage, root)
}
