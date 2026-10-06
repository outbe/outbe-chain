//! Semantic, feature-gated scenarios for cross-crate Metadosis tests.
//!
//! This interface deliberately hides storage providers, execution scopes,
//! schema entries, and mutation permits. Raw setup remains an implementation
//! detail of the crate-private fixture kernel while actions under test cross a
//! production command seam.

use alloy_primitives::{Address, Bytes, B256, U256};
use outbe_ocomp_protocol::{
    intent::JobIntentV1, receipts::ActivationOutcome, result::LysisResultV1, vote::ResultVoteV1,
    SchemaLimits,
};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{error::Result, storage::StorageHandle};
use std::fmt;

use crate::OcompForkInstallV1;

/// Seeds an exact retained READY population for cross-crate capacity tests.
///
/// The helper owns only predecessor construction. The capacity decision under
/// test must still enter through the production lifecycle command.
pub fn seed_ready_worldwide_days_for_capacity(
    storage: StorageHandle<'_>,
    worldwide_days: &[WorldwideDay],
) -> Result<()> {
    use crate::fixture_kernel::FixtureKernelExt;

    if worldwide_days.len() > crate::constants::MAX_RETAINED_WWDS {
        return Err(outbe_primitives::error::PrecompileError::Fatal(
            "capacity fixture exceeds MAX_RETAINED_WWDS".into(),
        ));
    }
    let mut contract = crate::schema::MetadosisContract::new(storage);
    for worldwide_day in worldwide_days {
        if !worldwide_day.is_valid() {
            return Err(outbe_primitives::error::PrecompileError::Fatal(
                "capacity fixture contains an invalid WorldwideDay".into(),
            ));
        }
        contract.fixture_create_ready_day(*worldwide_day, U256::ZERO, U256::ZERO, U256::ZERO)?;
        contract.add_active_wwd(*worldwide_day)?;
    }
    Ok(())
}

/// Seeds the bootstrap boundary for cross-crate tests that classify by it,
/// without standing up a genesis Worldwide Day.
pub fn seed_bootstrap_end_time(storage: StorageHandle<'_>, end_time: u64) -> Result<()> {
    crate::schema::MetadosisContract::new(storage).set_bootstrap_end_time(end_time)
}

/// Test/evidence-only sentinel probe for a fresh-devnet genesis snapshot.
///
/// This is intentionally absent from the production API. The probe checks the
/// named sentinel across every per-day durable surface. It does not expose raw
/// keys or mutation access to the harness.
pub fn fresh_devnet_sentinel_is_pristine(
    storage: StorageHandle<'_>,
    sentinel_day: WorldwideDay,
) -> Result<bool> {
    let contract = crate::schema::MetadosisContract::new(storage.clone());
    if !crate::api::worldwide_days(storage.clone())?.is_empty() {
        return Ok(false);
    }
    let indexes_empty = contract.ocomp_scheduler.is_empty()?
        && contract.ocomp_ready_index.is_empty()?
        && contract.ocomp_response_deadline_index.is_empty()?;
    if !indexes_empty || contract.terminal_intent_count(sentinel_day)? != 0 {
        return Ok(false);
    }
    if !sentinel_day_artifacts_empty(&contract, sentinel_day)? {
        return Ok(false);
    }
    let zero_intent_empty = contract
        .ocomp_job_records
        .get_bytes(&B256::ZERO)
        .is_empty()?
        && contract
            .ocomp_vote_accountability
            .get_bytes(&B256::ZERO)
            .is_empty()?;
    if !zero_intent_empty {
        return Ok(false);
    }
    Ok(
        crate::api::missed_offering_receipt(storage.clone(), sentinel_day)?.is_none()
            && crate::api::capacity_forfeiture_receipt(storage.clone(), sentinel_day)?.is_none()
            && crate::api::day_limit_formation_receipt(storage, sentinel_day)?.is_none(),
    )
}

fn sentinel_day_artifacts_empty(
    contract: &crate::schema::MetadosisContract<'_>,
    day: WorldwideDay,
) -> Result<bool> {
    for bytes in [
        contract.ocomp_fsm_states.get_bytes(&day),
        contract.ocomp_request_limit_receipts.get_bytes(&day),
        contract.ocomp_pre_admission_envelopes.get_bytes(&day),
        contract.ocomp_active_lysis_generations.get_bytes(&day),
    ] {
        if !bytes.is_empty()? {
            return Ok(false);
        }
    }
    Ok(true)
}

mod kernel {
    use alloy_primitives::{Address, Bytes, B256};
    use outbe_ocomp_protocol::{
        committee::OcompKeyRegistrationV1, intent::JobIntentV1, receipts::ActivationOutcome,
        result::LysisResultV1, vote::ResultVoteV1, SchemaLimits,
    };
    use outbe_primitives::error::Result;

    use crate::{
        fixture_kernel::{fork_install_fixture, ActivationFixture, RollbackSnapshot},
        ocomp::schema::poc_schema_limits,
        OcompForkInstallClassification, OcompForkInstallV1,
    };

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub(super) struct ActivationCheckpointState(RollbackSnapshot);

    pub(super) struct ActivationKernel {
        inner: ActivationFixture,
    }

    impl ActivationKernel {
        pub(super) fn q_forming_success(current_height: u64, current_time: u64) -> Self {
            Self {
                inner: ActivationFixture::new(current_height, current_time, true),
            }
        }

        pub(super) fn submit_q_forming_vote(&mut self) -> Result<Bytes> {
            self.inner.apply()
        }

        pub(super) fn corrupt_request_receipt_mismatch(&mut self) {
            self.inner.corrupt_request_receipt_mismatch();
        }

        pub(super) fn checkpoint(&mut self) -> ActivationCheckpointState {
            ActivationCheckpointState(self.inner.rollback_snapshot())
        }

        pub(super) fn terminal_outcome(&mut self) -> ActivationOutcome {
            self.inner.terminal_outcome()
        }
    }

    pub(super) fn fork_install(
        classification: OcompForkInstallClassification,
        activation_height: u64,
        chain_id: u64,
        genesis_hash: B256,
    ) -> Result<OcompForkInstallV1> {
        let install =
            fork_install_fixture(classification, activation_height, chain_id, genesis_hash);
        install.validate_for_chain(chain_id, genesis_hash, &poc_schema_limits())?;
        Ok(install)
    }

    pub(super) fn founder_registrations(
        validators: &[(Address, [u8; 48])],
        chain_id: u64,
        genesis_hash: B256,
    ) -> Result<Vec<OcompKeyRegistrationV1>> {
        crate::fixture_kernel::founder_registrations_for_validators(
            validators,
            chain_id,
            genesis_hash,
            &poc_schema_limits(),
        )
    }

    pub(super) fn result_for_intent(
        intent: &JobIntentV1,
        job_id: B256,
    ) -> (LysisResultV1, SchemaLimits) {
        let limits = poc_schema_limits();
        let result = crate::fixture_kernel::lysis_result_for_intent(intent, job_id, &limits);
        (result, limits)
    }

    pub(super) fn signed_vote(
        intent: &JobIntentV1,
        result: &LysisResultV1,
        validator_index: u8,
        limits: &SchemaLimits,
    ) -> ResultVoteV1 {
        crate::fixture_kernel::signed_result_vote_for_intent(
            intent,
            result,
            validator_index,
            limits,
        )
    }

    pub(super) fn persisted_open_result_vote() -> super::PersistedOpenResultVote {
        use crate::api::{verify_result_vote_carrier, ResultVoteCarrierAdmission};
        use crate::fixture_kernel::{ActivationFixture, TEST_REQUEST_HEIGHT};
        use crate::schema::MetadosisContract;
        use outbe_ocomp_protocol::{
            abi::encode_submit_lysis_result_calldata, state::RESULT_VOTE_MIN_FINALITY_DEPTH,
        };
        use outbe_primitives::storage::StorageHandle;

        const MEMBER_INDEX: u8 = 2;
        let open_height = TEST_REQUEST_HEIGHT + RESULT_VOTE_MIN_FINALITY_DEPTH;
        let signer = Address::repeat_byte(0xB0 + MEMBER_INDEX);
        let mut fixture = ActivationFixture::new(open_height, 1_700_000_000, true);
        let vote = fixture.signed_result_vote(MEMBER_INDEX);
        let valid_calldata = Bytes::from(
            encode_submit_lysis_result_calldata(&vote, &fixture.limits)
                .expect("canonical vote encodes"),
        );
        let admit = |provider: &mut outbe_primitives::storage::hashmap::HashMapStorageProvider,
                     height| {
            StorageHandle::enter(provider, |storage| {
                verify_result_vote_carrier(
                    storage,
                    valid_calldata.as_ref(),
                    signer,
                    height,
                    &fixture.limits,
                )
            })
        };
        assert!(
            matches!(
                admit(&mut fixture.provider, open_height),
                ResultVoteCarrierAdmission::Valid { .. }
            ),
            "member {MEMBER_INDEX} must be authorized at the open height"
        );
        let due_height = StorageHandle::enter(&mut fixture.provider, |storage| {
            MetadosisContract::new(storage)
                .ocomp_job_record(fixture.intent_id, &fixture.limits)
                .expect("persisted result-vote job is readable")
                .expect("persisted result-vote job exists")
                .finalized
                .expect("persisted result-vote job is finalized")
                .deadline_height
        });
        assert!(
            matches!(
                admit(&mut fixture.provider, due_height - 1),
                ResultVoteCarrierAdmission::Valid { .. }
            ),
            "the vote must remain admissible immediately before the persisted deadline"
        );
        assert!(
            matches!(
                admit(&mut fixture.provider, due_height),
                ResultVoteCarrierAdmission::DeadlineDueUnclosed { deadline_height }
                    if deadline_height == due_height
            ),
            "the persisted deadline must be the first due height"
        );
        let mut tampered = vote;
        tampered.signature_rs[63] ^= 0x01;
        let tampered_calldata = Bytes::from(
            encode_submit_lysis_result_calldata(&tampered, &fixture.limits)
                .expect("a flipped signature byte keeps the canonical encoding"),
        );
        let slots = fixture
            .provider
            .storage
            .iter()
            .map(|((address, slot), value)| (*address, B256::from(slot.to_be_bytes()), *value))
            .collect();
        super::PersistedOpenResultVote {
            slots,
            signer,
            open_height,
            due_height,
            valid_calldata,
            tampered_calldata,
        }
    }
}

/// Named invariant violations supported by the activation scenario.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationCorruption {
    /// The persisted request-split receipt no longer matches the certified
    /// result binding.
    RequestReceiptMismatch,
}

/// Opaque equality snapshot of all journaled activation state.
#[derive(Clone, Eq, PartialEq)]
pub struct ActivationCheckpoint(kernel::ActivationCheckpointState);

impl fmt::Debug for ActivationCheckpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ActivationCheckpoint")
            .field(&"<opaque>")
            .finish()
    }
}

/// Opaque builder for one immutable, production-valid fork-install artifact.
pub struct ForkInstallScenario {
    install: OcompForkInstallV1,
}

impl ForkInstallScenario {
    /// Builds a production-valid final fork-install artifact.
    pub fn final_at(activation_height: u64, chain_id: u64, genesis_hash: B256) -> Result<Self> {
        Ok(Self {
            install: kernel::fork_install(
                crate::OcompForkInstallClassification::Final,
                activation_height,
                chain_id,
                genesis_hash,
            )?,
        })
    }

    /// Builds a production-valid measurement fork-install artifact.
    pub fn measurement_at(
        activation_height: u64,
        chain_id: u64,
        genesis_hash: B256,
    ) -> Result<Self> {
        Ok(Self {
            install: kernel::fork_install(
                crate::OcompForkInstallClassification::Measurement,
                activation_height,
                chain_id,
                genesis_hash,
            )?,
        })
    }

    /// Rebinds the test artifact to the exact ordered validator snapshot used
    /// by [`ResultVotingScenario`]. The registration keys and result-vote keys
    /// share one deterministic index mapping, so cross-crate fixtures exercise
    /// real signature verification rather than a bypass.
    pub fn with_founder_validators(mut self, validators: &[(Address, [u8; 48])]) -> Result<Self> {
        self.install.founder_registrations = kernel::founder_registrations(
            validators,
            self.install.request_profile.chain_id,
            self.install.request_profile.genesis_hash,
        )?;
        self.install.validate_for_chain(
            self.install.request_profile.chain_id,
            self.install.request_profile.genesis_hash,
            &crate::ocomp::schema::poc_schema_limits(),
        )?;
        Ok(self)
    }

    /// Borrows the immutable artifact consumed by production genesis/config
    /// adapters.
    #[must_use]
    pub const fn install(&self) -> &OcompForkInstallV1 {
        &self.install
    }

    /// Consumes the scenario into the immutable artifact.
    #[must_use]
    pub fn into_install(self) -> OcompForkInstallV1 {
        self.install
    }
}

/// Opaque deterministic result/vote material for a persisted production job.
pub struct ResultVotingScenario {
    intent: JobIntentV1,
    result: LysisResultV1,
    limits: SchemaLimits,
}

impl ResultVotingScenario {
    #[must_use]
    pub fn for_intent(intent: &JobIntentV1, job_id: B256) -> Self {
        let (result, limits) = kernel::result_for_intent(intent, job_id);
        Self {
            intent: intent.clone(),
            result,
            limits,
        }
    }

    #[must_use]
    pub const fn result(&self) -> &LysisResultV1 {
        &self.result
    }

    #[must_use]
    pub fn signed_vote(&self, validator_index: u8) -> ResultVoteV1 {
        kernel::signed_vote(&self.intent, &self.result, validator_index, &self.limits)
    }
}

/// Committed open-job state a Reth state provider can serve to the pool.
///
/// Heights are the verifier's own boundaries for this state: `open_height`
/// is the first admissible inclusion, and `due_height` is the first height
/// the still-open window reports as due.
pub struct PersistedOpenResultVote {
    pub slots: Vec<(Address, B256, U256)>,
    pub signer: Address,
    pub open_height: u64,
    pub due_height: u64,
    pub valid_calldata: Bytes,
    pub tampered_calldata: Bytes,
}

/// Builds one authorized result vote against a persisted open job.
///
/// The next committee member has not voted. `tampered_calldata` is that vote
/// with one signature byte flipped.
#[must_use]
pub fn persisted_open_result_vote() -> PersistedOpenResultVote {
    kernel::persisted_open_result_vote()
}

/// Opaque q-forming activation scenario.
pub struct ActivationScenario {
    inner: kernel::ActivationKernel,
}

impl ActivationScenario {
    /// Creates a production-valid state in which the next result vote forms
    /// quorum and all certified owner effects can succeed.
    #[must_use]
    pub fn q_forming_success(current_height: u64, current_time: u64) -> Self {
        Self {
            inner: kernel::ActivationKernel::q_forming_success(current_height, current_time),
        }
    }

    /// Submits the q-forming vote through the production Metadosis command.
    pub fn submit_q_forming_vote(&mut self) -> Result<Bytes> {
        self.inner.submit_q_forming_vote()
    }

    /// Applies one explicitly named invalid precondition through the private
    /// fixture kernel.
    pub fn corrupt(&mut self, corruption: ActivationCorruption) {
        match corruption {
            ActivationCorruption::RequestReceiptMismatch => {
                self.inner.corrupt_request_receipt_mismatch();
            }
        }
    }

    /// Captures opaque journaled state for rollback and retry assertions.
    pub fn checkpoint(&mut self) -> ActivationCheckpoint {
        ActivationCheckpoint(self.inner.checkpoint())
    }

    /// Returns the durable typed terminal outcome after successful submission.
    pub fn terminal_outcome(&mut self) -> ActivationOutcome {
        self.inner.terminal_outcome()
    }
}
