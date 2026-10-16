//! Certified OCOMP consume and logical-retirement boundary for Tribute.
//!
//! The transition verifies frozen day aggregates, marks the day retired, and
//! advances its source generation. It retains all ciphertext records and indexes.

use crate::runtime::RetirementOutcome;
use alloy_primitives::{B256, U256};
use alloy_sol_types::SolEvent;
use outbe_ocomp_protocol::{
    intent::TributeInputBindingV1,
    receipts::{
        tribute_state_event_digest, EffectBindingV1, TributeReceiptV1,
        TributeStateEventProjectionV1,
    },
    SchemaLimits,
};
use outbe_primitives::time::WorldwideDay;
use outbe_primitives::{
    addresses::TRIBUTE_ADDRESS,
    error::Result,
    storage::{CertifiedLysisActivation, StorageHandle},
};

use crate::{precompile::ITribute, TributeContract};

/// Closed, constant-size Tribute owner input derived from a verified Lysis
/// apply plan. Raw values do not grant mutation authority.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedTributeRetirementV1 {
    pub binding: EffectBindingV1,
    pub input_binding: TributeInputBindingV1,
    pub consumed_count: u32,
    pub consumed_nominal_total: U256,
    pub retired_generation: u64,
}

/// Fully validated, mutation-free preparation for one certified retirement.
/// Its fields are private so only this module can authorize the apply step.
pub struct PreparedCertifiedTributeRetirementV1 {
    input: CertifiedTributeRetirementV1,
    receipt: TributeReceiptV1,
    day: WorldwideDay,
    state_event_digest: B256,
}

impl PreparedCertifiedTributeRetirementV1 {
    #[must_use]
    pub const fn receipt(&self) -> &TributeReceiptV1 {
        &self.receipt
    }
}

/// Validates the complete certified Tribute input and constructs its receipt
/// without mutating Tribute state.
pub fn prepare_certified_partition_retirement(
    storage: &StorageHandle<'_>,
    capability: &CertifiedLysisActivation<'_>,
    input: &CertifiedTributeRetirementV1,
    limits: &SchemaLimits,
) -> Result<PreparedCertifiedTributeRetirementV1> {
    validate_input(capability, input)?;

    let day = WorldwideDay::new(input.input_binding.wwd);
    let tribute = TributeContract::new(storage.clone());
    let current = tribute.pre_admission_projection(day)?;
    if !current.profile_ready
        || !current.is_sealed
        || current.source_generation != input.input_binding.source_generation
    {
        return Err(crate::errors::TributeError::CertifiedInputDiffersFromSealedGeneration.into());
    }
    if current.tribute_count != input.input_binding.exact_count
        || current.tribute_nominal_total_minor != input.input_binding.exact_nominal_total
    {
        return Err(crate::errors::TributeError::CertifiedInputDiffersFromSealedGeneration.into());
    }
    if current.source_generation != 0 {
        return Err(crate::errors::TributeError::CertifiedGenerationAlreadyRetired.into());
    }

    let state_projection = TributeStateEventProjectionV1 {
        wwd: input.input_binding.wwd,
        source_generation: input.input_binding.source_generation,
        consumed_count: input.consumed_count,
        consumed_nominal_total: input.consumed_nominal_total,
        retired_generation: input.retired_generation,
    };
    let state_event_digest = tribute_state_event_digest(&input.binding, &state_projection, limits)
        .map_err(crate::errors::TributeError::invalid_certified_protocol_value)?;
    let receipt = TributeReceiptV1 {
        binding: input.binding.clone(),
        tribute_input_binding: input.input_binding.clone(),
        consumed_count: input.consumed_count,
        consumed_nominal_total: input.consumed_nominal_total,
        retired_generation: input.retired_generation,
        state_event_digest,
    };
    receipt
        .validate_projection(&state_projection, limits)
        .map_err(crate::errors::TributeError::invalid_certified_protocol_value)?;
    receipt
        .receipt_hash(limits)
        .map_err(crate::errors::TributeError::invalid_certified_protocol_value)?;

    Ok(PreparedCertifiedTributeRetirementV1 {
        input: input.clone(),
        receipt,
        day,
        state_event_digest,
    })
}

/// Consumes one sealed generation and marks its day retired in EVM state.
/// The transition retains all ciphertext records and indexes.
pub fn retire_certified_partition(
    storage: &StorageHandle<'_>,
    capability: &mut CertifiedLysisActivation<'_>,
    input: &CertifiedTributeRetirementV1,
    limits: &SchemaLimits,
) -> Result<TributeReceiptV1> {
    let prepared = prepare_certified_partition_retirement(storage, capability, input, limits)?;
    retire_prepared_certified_partition(storage, capability, prepared)
}

/// Applies a previously validated retirement. Callers prepare and verify every
/// receipt gate before this final state mutation.
pub fn retire_prepared_certified_partition(
    storage: &StorageHandle<'_>,
    capability: &mut CertifiedLysisActivation<'_>,
    prepared: PreparedCertifiedTributeRetirementV1,
) -> Result<TributeReceiptV1> {
    let PreparedCertifiedTributeRetirementV1 {
        input,
        receipt,
        day,
        state_event_digest,
    } = prepared;
    let mut tribute = TributeContract::new(storage.clone());

    storage.with_checkpoint(|| {
        tribute.consume_lysis_partition_inner(
            day,
            input.consumed_count,
            input.consumed_nominal_total,
        )?;
        if tribute.retire_completed_partition_inner(day)? != RetirementOutcome::Requested {
            return Err(
                crate::errors::TributeError::SealedProjectionContradictsAuthenticatedCeGeneration
                    .into(),
            );
        }

        if !tribute.day_pre_admission.exists(day)? {
            return Err(crate::errors::TributeError::CertifiedPreAdmissionRecordAbsent.into());
        }
        let mut admission = tribute.read_day_pre_admission(day)?;
        if admission.source_generation != input.input_binding.source_generation {
            return Err(
                crate::errors::TributeError::CertifiedSourceGenerationChangedDuringActivation
                    .into(),
            );
        }
        admission.source_generation = input.retired_generation;
        tribute.store_day_pre_admission(&admission)?;

        storage.emit_event(
            TRIBUTE_ADDRESS,
            ITribute::CertifiedTributePartitionRetired {
                activationCallId: input.binding.activation_call_id,
                worldwideDay: input.input_binding.wwd,
                sourceGeneration: input.input_binding.source_generation,
                consumedCount: input.consumed_count,
                consumedNominalTotalMinor: tribute.encrypted_day_nominal(day, true)?.into(),
                retiredGeneration: input.retired_generation,
                stateEventDigest: state_event_digest,
            }
            .encode_log_data(),
        )?;
        // The cursor is not journaled. Advance only after every consensus write
        // and event succeeds, so caught failures cannot skip the Tribute owner.
        capability.authorize_tribute_retirement()?;
        Ok(())
    })?;

    Ok(receipt)
}

fn validate_input(
    capability: &CertifiedLysisActivation<'_>,
    input: &CertifiedTributeRetirementV1,
) -> Result<()> {
    if capability.activation_call_id() != input.binding.activation_call_id {
        return Err(crate::errors::TributeError::CertifiedActivationBindingMismatch.into());
    }
    if input.input_binding.exact_count == 0 || input.input_binding.source_generation != 0 {
        return Err(crate::errors::TributeError::InvalidCertifiedRetirementAggregate.into());
    }
    if input.consumed_count != input.input_binding.exact_count
        || input.consumed_nominal_total != input.input_binding.exact_nominal_total
        || input.retired_generation != 1
    {
        return Err(crate::errors::TributeError::InvalidCertifiedRetirementAggregate.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, LogData};
    use alloy_sol_types::SolEvent;
    use outbe_ocomp_protocol::profile::poc_schema_limits;
    use outbe_primitives::storage::{
        hashmap::HashMapStorageProvider, PrecompileStorageProvider, SubCallError, SubCallInput,
        SubCallOutput,
    };
    use revm::{
        context::journaled_state::JournalCheckpoint,
        state::{AccountInfo, Bytecode},
    };

    use crate::{DayPreAdmission, DayTotals};
    use outbe_primitives::error::PrecompileError;

    use super::*;

    struct ActivationTestProvider {
        authority: ActivationStorage,
    }

    /// Owns the storage adapter and the active lease for the same fixture.
    struct ActivationStorage {
        storage: HashMapStorageProvider,
        active_call: Option<B256>,
    }

    impl std::ops::Deref for ActivationTestProvider {
        type Target = HashMapStorageProvider;

        fn deref(&self) -> &Self::Target {
            &self.authority.storage
        }
    }

    impl std::ops::DerefMut for ActivationTestProvider {
        fn deref_mut(&mut self) -> &mut Self::Target {
            &mut self.authority.storage
        }
    }

    impl ActivationTestProvider {
        fn new() -> Self {
            Self {
                authority: ActivationStorage {
                    storage: HashMapStorageProvider::new(1),
                    active_call: None,
                },
            }
        }
    }

    impl PrecompileStorageProvider for ActivationTestProvider {
        fn begin_lysis_activation_frame(&mut self, activation_call_id: B256) -> Result<()> {
            if self
                .authority
                .active_call
                .replace(activation_call_id)
                .is_some()
            {
                return Err(PrecompileError::Revert(
                    "test activation lease is already active".into(),
                ));
            }
            Ok(())
        }

        fn finish_lysis_activation_frame(
            &mut self,
            activation_call_id: B256,
            _completed: bool,
        ) -> Result<()> {
            if self.authority.active_call.take() != Some(activation_call_id) {
                return Err(PrecompileError::Revert(
                    "test activation lease identity mismatch".into(),
                ));
            }
            Ok(())
        }

        fn chain_id(&self) -> u64 {
            self.authority.storage.chain_id()
        }

        fn genesis_hash(&self) -> B256 {
            self.authority.storage.genesis_hash()
        }

        fn timestamp(&self) -> U256 {
            self.authority.storage.timestamp()
        }

        fn set_block_timestamp(&mut self, timestamp: U256) {
            self.authority.storage.set_block_timestamp(timestamp);
        }

        fn beneficiary(&self) -> Address {
            self.authority.storage.beneficiary()
        }

        fn block_number(&self) -> u64 {
            self.authority.storage.block_number()
        }

        fn canonical_block_hash(&mut self, number: u64) -> Result<Option<B256>> {
            self.authority.storage.canonical_block_hash(number)
        }

        fn set_code(&mut self, address: Address, code: Bytecode) -> Result<()> {
            self.authority.storage.set_code(address, code)
        }

        fn account_info(&mut self, address: Address) -> Result<AccountInfo> {
            self.authority.storage.account_info(address)
        }

        fn sload(&mut self, address: Address, key: U256) -> Result<U256> {
            self.authority.storage.sload(address, key)
        }

        fn tload(&mut self, address: Address, key: U256) -> Result<U256> {
            self.authority.storage.tload(address, key)
        }

        fn sstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
            self.authority.storage.sstore(address, key, value)
        }

        fn tstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
            self.authority.storage.tstore(address, key, value)
        }

        fn emit_event(&mut self, address: Address, event: LogData) -> Result<()> {
            self.authority.storage.emit_event(address, event)
        }

        fn deduct_gas(&mut self, gas: u64) -> Result<()> {
            self.authority.storage.deduct_gas(gas)
        }

        fn refund_gas(&mut self, gas: i64) {
            self.authority.storage.refund_gas(gas);
        }

        fn gas_used(&self) -> u64 {
            self.authority.storage.gas_used()
        }

        fn gas_refunded(&self) -> i64 {
            self.authority.storage.gas_refunded()
        }

        fn is_static(&self) -> bool {
            self.authority.storage.is_static()
        }

        fn checkpoint(&mut self) -> JournalCheckpoint {
            self.authority.storage.checkpoint()
        }

        fn checkpoint_commit(&mut self) {
            self.authority.storage.checkpoint_commit();
        }

        fn checkpoint_revert(&mut self, checkpoint: JournalCheckpoint) {
            self.authority.storage.checkpoint_revert(checkpoint);
        }

        fn transfer_balance(&mut self, from: Address, to: Address, amount: U256) -> Result<()> {
            self.authority.storage.transfer_balance(from, to, amount)
        }

        fn increase_balance(&mut self, address: Address, amount: U256) -> Result<()> {
            self.authority.storage.increase_balance(address, amount)
        }

        fn decrease_balance(&mut self, address: Address, amount: U256) -> Result<()> {
            self.authority.storage.decrease_balance(address, amount)
        }

        fn sub_call(
            &mut self,
            input: SubCallInput,
        ) -> std::result::Result<SubCallOutput, SubCallError> {
            self.authority.storage.sub_call(input)
        }
    }

    struct Fixture {
        provider: ActivationTestProvider,
    }

    impl Fixture {
        fn new(expected: &CertifiedTributeRetirementV1) -> Self {
            let mut provider = ActivationTestProvider::new();
            StorageHandle::enter(&mut provider, |storage| {
                let _enclave = crate::enclave_client::test_enclave::scope();
                let day = WorldwideDay::new(expected.input_binding.wwd);
                let mut tribute = TributeContract::new(storage);
                tribute.ocomp_profile_ready.write(true).unwrap();
                tribute
                    .total_supply
                    .write(u64::from(expected.input_binding.exact_count))
                    .unwrap();

                let mut totals = DayTotals::with_key(day);
                totals.initialized = true;
                totals.is_sealed = true;
                totals.tribute_count = expected.input_binding.exact_count;
                totals.tribute_nominal_total_minor = expected.input_binding.exact_nominal_total;
                tribute
                    .apply_day_amount(
                        day,
                        outbe_tee::tribute_day::TributeDayOperationV2::AdjustTransient {
                            nominal_amount_minor: totals.tribute_nominal_total_minor,
                            add: true,
                        },
                        B256::repeat_byte(0x31),
                    )
                    .unwrap();
                tribute.store_day_totals(&totals).unwrap();

                let mut admission = DayPreAdmission::with_key(day);
                admission.initialized = true;
                admission.is_sealed = true;
                admission.sealed_tribute_count = expected.input_binding.exact_count;
                admission.sealed_tribute_nominal_total_minor =
                    expected.input_binding.exact_nominal_total;
                admission.source_generation = expected.input_binding.source_generation;
                tribute.store_day_pre_admission(&admission).unwrap();
            });
            Self { provider }
        }

        fn run(&mut self, input: &CertifiedTributeRetirementV1) -> Result<TributeReceiptV1> {
            StorageHandle::enter(&mut self.provider, |storage| {
                let _enclave = crate::enclave_client::test_enclave::scope();
                storage.with_lysis_activation_frame(
                    input.binding.activation_call_id,
                    |capability| {
                        capability.authorize_nod_installation()?;
                        capability.authorize_carry_over_credit()?;
                        let receipt = retire_certified_partition(
                            &storage,
                            capability,
                            input,
                            &poc_schema_limits(),
                        )?;
                        capability.authorize_terminal_receipt()?;
                        Ok(receipt)
                    },
                )
            })
        }

        fn state(&mut self, day: WorldwideDay) -> (u64, u32, U256, bool, u64) {
            StorageHandle::enter(&mut self.provider, |storage| {
                let _enclave = crate::enclave_client::test_enclave::scope();
                let tribute = TributeContract::new(storage);
                let generation = tribute
                    .pre_admission_projection(day)
                    .unwrap()
                    .source_generation;
                let totals = tribute.get_day_totals(day).unwrap();
                (
                    tribute.total_supply.read().unwrap(),
                    totals.tribute_count,
                    totals.tribute_nominal_total_minor,
                    totals.is_sealed,
                    generation,
                )
            })
        }
    }

    fn binding(call: u8) -> EffectBindingV1 {
        EffectBindingV1 {
            intent_id: B256::repeat_byte(1),
            job_id: B256::repeat_byte(2),
            attempt: 3,
            protocol_bundle_hash: B256::repeat_byte(4),
            result_digest: B256::repeat_byte(5),
            activation_preconditions_hash: B256::repeat_byte(6),
            activation_call_id: B256::repeat_byte(call),
        }
    }

    fn input(call: u8) -> CertifiedTributeRetirementV1 {
        let day = WorldwideDay::new(20_260_725);
        CertifiedTributeRetirementV1 {
            binding: binding(call),
            input_binding: TributeInputBindingV1 {
                wwd: day.value(),
                source_generation: 0,
                exact_count: 3,
                exact_nominal_total: U256::from(900),
            },
            consumed_count: 3,
            consumed_nominal_total: U256::from(900),
            retired_generation: 1,
        }
    }

    #[test]
    fn valid_capability_consumes_exact_generation_and_emits_canonical_receipt() {
        let input = input(21);
        let mut fixture = Fixture::new(&input);
        let receipt = fixture.run(&input).unwrap();

        let (supply, count, nominal, is_sealed, generation) =
            fixture.state(WorldwideDay::new(input.input_binding.wwd));
        assert_eq!(supply, 0);
        assert_eq!(count, 0);
        assert_eq!(nominal, U256::ZERO);
        assert!(is_sealed);
        assert_eq!(generation, 1);
        assert_eq!(receipt.tribute_input_binding, input.input_binding);
        assert_eq!(receipt.consumed_count, input.consumed_count);
        assert_eq!(receipt.consumed_nominal_total, input.consumed_nominal_total);
        assert_eq!(receipt.retired_generation, 1);
        assert!(!receipt
            .receipt_hash(&poc_schema_limits())
            .unwrap()
            .is_zero());

        let events = fixture.provider.get_ordered_events();
        assert_eq!(events.len(), 2);
        ITribute::TributePartitionRetired::decode_log(&events[0]).unwrap();
        let certified = ITribute::CertifiedTributePartitionRetired::decode_log(&events[1]).unwrap();
        assert_eq!(
            certified.data.activationCallId,
            input.binding.activation_call_id
        );
        assert_eq!(certified.data.worldwideDay, input.input_binding.wwd);
        assert_eq!(certified.data.sourceGeneration, 0);
        assert_eq!(certified.data.consumedCount, input.consumed_count);
        let encrypted: outbe_primitives::tribute_day_encryption::EncryptedTributeDayAmountV2 =
            postcard::from_bytes(&certified.data.consumedNominalTotalMinor).unwrap();
        assert_eq!(
            outbe_tee_enclave::tribute_day::read_day_amount(
                &crate::enclave_client::test_enclave::NETWORK_SECRET,
                &encrypted
            )
            .unwrap(),
            input.consumed_nominal_total
        );
        assert_eq!(certified.data.retiredGeneration, 1);
        assert_eq!(certified.data.stateEventDigest, receipt.state_event_digest);
    }

    #[test]
    fn wrong_day_count_nominal_or_generation_is_side_effect_free() {
        let expected = input(22);
        for mutation in 0..5 {
            let mut candidate = expected.clone();
            match mutation {
                0 => candidate.input_binding.wwd += 1,
                1 => {
                    candidate.input_binding.exact_count += 1;
                    candidate.consumed_count += 1;
                }
                2 => {
                    candidate.input_binding.exact_nominal_total += U256::from(1);
                    candidate.consumed_nominal_total += U256::from(1);
                }
                3 => {
                    candidate.input_binding.source_generation = 1;
                    candidate.retired_generation = 2;
                }
                4 => candidate.consumed_count += 1,
                _ => unreachable!(),
            }
            let mut fixture = Fixture::new(&expected);
            let before = fixture.state(WorldwideDay::new(expected.input_binding.wwd));
            assert!(fixture.run(&candidate).is_err());
            assert_eq!(
                fixture.state(WorldwideDay::new(expected.input_binding.wwd)),
                before
            );
            assert!(fixture.provider.get_ordered_events().is_empty());
        }
    }

    #[test]
    fn every_storage_and_event_failure_rolls_back_and_allows_exact_retry() {
        let input = input(24);
        let mut probe = Fixture::new(&input);
        probe.provider.fail_after_mutation_at(usize::MAX);
        probe.run(&input).unwrap();
        let mutation_count = probe.provider.clear_mutation_failure();
        assert!(
            mutation_count >= 2,
            "certified retirement must include storage and event mutations"
        );

        for operation in 0..mutation_count {
            let mut fixture = Fixture::new(&input);
            let day = WorldwideDay::new(input.input_binding.wwd);
            let before_state = fixture.state(day);
            let before_storage = fixture.provider.storage.clone();
            fixture.provider.fail_after_mutation_at(operation);

            assert!(fixture.run(&input).is_err());
            assert_eq!(fixture.provider.clear_mutation_failure(), operation + 1);
            assert_eq!(fixture.state(day), before_state);
            assert_eq!(fixture.provider.storage, before_storage);
            assert!(fixture.provider.get_ordered_events().is_empty());

            let receipt = fixture.run(&input).unwrap();
            assert_eq!(receipt.consumed_count, input.consumed_count);
            assert_eq!(fixture.state(day).4, 1);
            assert_eq!(fixture.provider.get_ordered_events().len(), 2);
        }
    }

    #[test]
    fn caught_late_failure_or_wrong_phase_cannot_skip_the_tribute_cursor() {
        let input = input(25);
        let mut probe = Fixture::new(&input);
        probe.provider.fail_after_mutation_at(usize::MAX);
        probe.run(&input).unwrap();
        let final_mutation = probe
            .provider
            .clear_mutation_failure()
            .checked_sub(1)
            .expect("certified retirement must mutate state");

        let mut late_failure = Fixture::new(&input);
        late_failure.provider.fail_after_mutation_at(final_mutation);
        let receipt = StorageHandle::enter(&mut late_failure.provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            storage.with_lysis_activation_frame(input.binding.activation_call_id, |capability| {
                capability.authorize_nod_installation()?;
                capability.authorize_carry_over_credit()?;
                assert!(retire_certified_partition(
                    &storage,
                    capability,
                    &input,
                    &poc_schema_limits(),
                )
                .is_err());
                let receipt =
                    retire_certified_partition(&storage, capability, &input, &poc_schema_limits())?;
                capability.authorize_terminal_receipt()?;
                Ok(receipt)
            })
        })
        .unwrap();
        assert_eq!(receipt.consumed_count, input.consumed_count);
        assert_eq!(late_failure.provider.get_ordered_events().len(), 2);

        let mut wrong_phase = Fixture::new(&input);
        let receipt = StorageHandle::enter(&mut wrong_phase.provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            storage.with_lysis_activation_frame(input.binding.activation_call_id, |capability| {
                capability.authorize_nod_installation()?;
                assert!(retire_certified_partition(
                    &storage,
                    capability,
                    &input,
                    &poc_schema_limits(),
                )
                .is_err());
                capability.authorize_carry_over_credit()?;
                let receipt =
                    retire_certified_partition(&storage, capability, &input, &poc_schema_limits())?;
                capability.authorize_terminal_receipt()?;
                Ok(receipt)
            })
        })
        .unwrap();
        assert_eq!(receipt.consumed_count, input.consumed_count);
        assert_eq!(wrong_phase.provider.get_ordered_events().len(), 2);
    }

    #[test]
    fn exact_retry_cannot_consume_or_retire_twice() {
        let input = input(26);
        let mut fixture = Fixture::new(&input);
        fixture.run(&input).unwrap();
        let state = fixture.state(WorldwideDay::new(input.input_binding.wwd));
        let events = fixture.provider.get_ordered_events().to_vec();

        assert!(fixture.run(&input).is_err());
        assert_eq!(
            fixture.state(WorldwideDay::new(input.input_binding.wwd)),
            state
        );
        assert_eq!(fixture.provider.get_ordered_events(), events);
    }

    #[test]
    fn populated_postfork_partition_rejects_legacy_consume_and_retire() {
        let input = input(27);
        let mut fixture = Fixture::new(&input);
        StorageHandle::enter(&mut fixture.provider, |storage| {
            let _enclave = crate::enclave_client::test_enclave::scope();
            let mut tribute = TributeContract::new(storage);
            assert!(tribute
                .consume_lysis_partition(
                    WorldwideDay::new(input.input_binding.wwd),
                    input.consumed_count,
                    input.consumed_nominal_total,
                )
                .is_err());
            assert!(tribute
                .retire_completed_partition(WorldwideDay::new(input.input_binding.wwd),)
                .is_err());
        });
        let (supply, count, nominal, is_sealed, generation) =
            fixture.state(WorldwideDay::new(input.input_binding.wwd));
        assert_eq!(supply, u64::from(input.input_binding.exact_count));
        assert_eq!(count, input.input_binding.exact_count);
        assert_eq!(nominal, input.input_binding.exact_nominal_total);
        assert!(is_sealed);
        assert_eq!(generation, 0);
        assert!(fixture.provider.get_ordered_events().is_empty());
    }
}
