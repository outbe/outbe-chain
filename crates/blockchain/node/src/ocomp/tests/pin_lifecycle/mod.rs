//! OCM-PIN-001: finalized-event retention and journal on a real filesystem.
// OCOMP-TEST-ID: OCM-PIN-001

use std::{
    collections::BTreeMap,
    fs::{self, File},
    io,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use alloy_eips::BlockNumHash;
use alloy_primitives::{b256, keccak256, Address, Bytes, Log, B256, U256};
use alloy_sol_types::SolEvent as _;
use k256::ecdsa::{signature::hazmat::PrehashSigner as _, Signature, SigningKey};
use outbe_compressed_entities::WwdEntityId;
use outbe_consensus::{
    block::ConsensusBlock, finalization::parent_cert_store::FinalizedParentCertStore,
};
use outbe_metadosis::{
    config::poc_schema_limits, precompile::IMetadosis, proof_layout::OCOMP_JOB_RECORDS_BASE_SLOT,
};
use outbe_ocomp_protocol::{
    committee::{
        validator_identity_hash_v1, OcompKeyRegistrationCoreV1, OcompKeyRegistrationV1,
        POC_KEY_EPOCH, RESULT_SIGNATURE_PURPOSE_BITMAP,
    },
    common::{BoundedBytes, ProofBytes},
    generated_shape::OCOMP_POC_CANDIDATE_LIMITS_V1,
    intent::{
        intent_storage_key, job_id_from_intent_id, ActivationPreconditionsV1,
        CertifiedParentAccountingMetadataV2, ContributorTargetPreconditionV1, DayType,
        FinalizedIntentProofV1, FrozenMetadosisValuesV1, JobIntentV1,
        MetadosisAttemptPreconditionV1, MetadosisExpectedStatus, NodTargetPreconditionV1,
        ParentProofKind, TributeInputBindingV1,
    },
    state::{OcompFinalizedJobV1, OcompJobRecordV1, OcompJobStatus},
};
use outbe_offchain_data::TributeRetentionSelector;
use outbe_offchain_storage::{AtomicWriteBatch, MemoryStorage, StorageError, StorageWriter};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::{METADOSIS_ADDRESS, VALIDATOR_SET_ADDRESS},
    storage::{hashmap::HashMapStorageProvider, types::StorageKey as _, StorageHandle},
    OutbeHeader, OutbePrimitives,
};
use outbe_tribute::{
    RetainedTributePin, RetainedTributeReader, RetainedTributeWriter, TributeData,
    TributeRepositoryWriter,
};
use reth_chainspec::{ChainSpec, ChainSpecBuilder};
use reth_ethereum::{primitives::SealedBlock, Block, Receipt, TxType};
use reth_provider::test_utils::{ExtendedAccount, MockEthProvider};

use outbe_validatorset::{
    contract::ValidatorSet, read_ocomp_snapshot_extension, write_committee_snapshot,
    CommitteeEntry, CommitteeSnapshot,
};

use crate::finalized_frame::FinalizedFrame;
use crate::ocomp::retention::{
    inspect_retention_journal, journal_recovery_backoff, observe_finalized_request,
    ocomp_snapshot_contains_key_at, retained_gc_next_wake_delay,
    retention_pressure_watermark_for_test, retention_terminal_height_for_status,
    seed_retention_journal_for_test, CandidatePinV1, ExportAuthorityV1, FinalizedInputProofSource,
    FinalizedJobPinV1, FinalizedRequestObservationV1, JournalDurability, OcompRetentionCoordinator,
    OcompSnapshotEligibilityV1, PinRecordV1, PinStateV1, RetainedGcRetrySchedule, RetentionError,
    RetentionStatus, RethFinalizedInputProofSource, SharedOcompRetentionSelector,
};

mod fixtures;
use fixtures::{
    block, block_extending, candidate, candidate_for_intent, canonical_terminal_fixture,
    fixture_job_id, frame_for_block, production_candidate_source, production_intent, ready_record,
    CandidateProvider, ProductionCandidateFixture,
};
use fixtures::{DeterministicProofSource, FinalizedFrameDriver};
use fixtures::{FailOnceDurability, FailSync};

mod canonical_replay;
mod eligibility;
mod exports;
mod finality;
mod journal_recovery;
mod pressure;
mod retained_gc;
mod terminal;
