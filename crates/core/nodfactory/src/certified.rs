//! Certified OCOMP installation boundary for Nod generations.
//!
//! This path installs only constant-size root authority and metadata. It never
//! decodes or iterates `NodActionV1`, and it has no public precompile selector.

use alloy_primitives::U256;
use alloy_sol_types::SolEvent;
use outbe_common::WorldwideDay;
use outbe_nod::{NodCertifiedGenerationProjection, NodContract};
use outbe_ocomp_protocol::{
    intent::NodTargetPreconditionV1,
    receipts::{
        nod_state_event_digest, EffectBindingV1, NodBatchReceiptV1, NodStateEventProjectionV1,
    },
    result::{ExactCountsV1, ResultRootsV1},
    SchemaLimits,
};
use outbe_primitives::{
    addresses::NOD_FACTORY_ADDRESS,
    error::{PrecompileError, Result},
    storage::{CertifiedLysisActivation, StorageHandle},
};

use crate::precompile::INodFactory;

/// Closed, constant-size Nod owner input derived from a verified Lysis apply
/// plan. The raw values carry no authority; installation additionally requires
/// the runtime-only [`CertifiedLysisActivation`] capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertifiedNodGenerationV1 {
    pub binding: EffectBindingV1,
    pub precondition: NodTargetPreconditionV1,
    pub roots: ResultRootsV1,
    pub counts: ExactCountsV1,
    pub nod_amount_total: U256,
    pub nod_gratis_consumed: U256,
    pub issued_at: u64,
}

/// Compare-and-set one certified per-WWD Nod generation.
///
/// The method is Rust-visible for the Metadosis activation coordinator, but is
/// unreachable from ABI dispatch and cannot succeed without the private
/// execution-frame capability.
pub fn install_certified_generation(
    storage: &StorageHandle<'_>,
    capability: &mut CertifiedLysisActivation<'_>,
    input: &CertifiedNodGenerationV1,
    limits: &SchemaLimits,
) -> Result<NodBatchReceiptV1> {
    validate_input(capability, input)?;

    let worldwide_day = WorldwideDay::new(input.precondition.wwd);
    let nod = NodContract::new(storage.clone());
    let current = nod.ocomp_target_projection(worldwide_day)?;
    if current.target_generation != input.precondition.target_generation
        || current.namespace_root_before != input.precondition.namespace_root_before
    {
        return Err(revert("certified Nod target precondition changed"));
    }

    let next_generation = current
        .target_generation
        .checked_add(1)
        .ok_or_else(|| revert("certified Nod generation overflow"))?;
    let state_projection = NodStateEventProjectionV1 {
        wwd: input.precondition.wwd,
        target_generation: input.precondition.target_generation,
        namespace_root_before: input.precondition.namespace_root_before,
        nod_count: input.counts.nod_count,
        nod_root: input.roots.nod_root,
        nod_amount_total: input.nod_amount_total,
        nod_gratis_consumed: input.nod_gratis_consumed,
        issued_at: input.issued_at,
    };
    let state_event_digest = nod_state_event_digest(&input.binding, &state_projection, limits)
        .map_err(protocol_error)?;
    let receipt = NodBatchReceiptV1 {
        binding: input.binding.clone(),
        nod_target_precondition: input.precondition.clone(),
        nod_count: input.counts.nod_count,
        nod_root: input.roots.nod_root,
        nod_amount_total: input.nod_amount_total,
        nod_gratis_consumed: input.nod_gratis_consumed,
        issued_at: input.issued_at,
        state_event_digest,
    };
    receipt
        .validate_projection(&state_projection, limits)
        .map_err(protocol_error)?;
    receipt.receipt_hash(limits).map_err(protocol_error)?;

    let installed = NodCertifiedGenerationProjection {
        worldwide_day,
        generation: next_generation,
        nod_root: input.roots.nod_root,
        bucket_root: input.roots.bucket_root,
        output_manifest_root: input.roots.output_manifest_root,
        tribute_count: input.counts.tribute_count,
        nod_count: input.counts.nod_count,
        bucket_count: input.counts.bucket_count,
        nod_amount_total: input.nod_amount_total,
        nod_gratis_consumed: input.nod_gratis_consumed,
        issued_at: input.issued_at,
    };

    storage.with_checkpoint(|| {
        nod.ocomp_namespace_root
            .write(&worldwide_day, installed.nod_root)?;
        nod.ocomp_bucket_root
            .write(&worldwide_day, installed.bucket_root)?;
        nod.ocomp_output_manifest_root
            .write(&worldwide_day, installed.output_manifest_root)?;
        nod.ocomp_generation_metadata
            .write(&worldwide_day, installed.metadata_word())?;
        nod.ocomp_nod_amount_total
            .write(&worldwide_day, installed.nod_amount_total)?;
        nod.ocomp_nod_gratis_consumed
            .write(&worldwide_day, installed.nod_gratis_consumed)?;
        // The generation is the active-state selector and therefore switches
        // only after every root and scalar has been written successfully.
        nod.ocomp_target_generation
            .write(&worldwide_day, installed.generation)?;
        storage.emit_event(
            NOD_FACTORY_ADDRESS,
            INodFactory::CertifiedNodGenerationInstalled {
                activationCallId: input.binding.activation_call_id,
                worldwideDay: input.precondition.wwd,
                targetGeneration: input.precondition.target_generation,
                namespaceRootBefore: input.precondition.namespace_root_before,
                tributeCount: input.counts.tribute_count,
                nodCount: input.counts.nod_count,
                bucketCount: input.counts.bucket_count,
                nodRoot: input.roots.nod_root,
                bucketRoot: input.roots.bucket_root,
                outputManifestRoot: input.roots.output_manifest_root,
                nodAmountTotal: input.nod_amount_total,
                nodGratisConsumed: input.nod_gratis_consumed,
                issuedAt: input.issued_at,
                stateEventDigest: state_event_digest,
            }
            .encode_log_data(),
        )?;
        // The capability cursor is not journaled storage. Advance it only
        // after every owner write and event has succeeded, so a caught
        // mutation failure cannot skip the Nod owner step.
        capability.authorize_nod_installation()?;
        Ok(())
    })?;

    Ok(receipt)
}

fn validate_input(
    capability: &CertifiedLysisActivation<'_>,
    input: &CertifiedNodGenerationV1,
) -> Result<()> {
    if capability.activation_call_id() != input.binding.activation_call_id {
        return Err(revert("certified Nod activation binding mismatch"));
    }
    if input.issued_at == 0 {
        return Err(revert("certified Nod logical issuance time is zero"));
    }
    if input.roots.nod_root.is_zero()
        || input.roots.bucket_root.is_zero()
        || input.roots.output_manifest_root.is_zero()
    {
        return Err(revert("certified Nod generation contains a zero root"));
    }
    if input.counts.tribute_count == 0
        || input.counts.nod_count != input.counts.tribute_count
        || input.counts.nod_count != input.precondition.max_nod_count
        || input.counts.bucket_count > input.counts.nod_count
    {
        return Err(revert("certified Nod generation count mismatch"));
    }
    Ok(())
}

fn protocol_error(error: outbe_ocomp_protocol::ProtocolError) -> PrecompileError {
    PrecompileError::Fatal(format!("invalid certified Nod protocol value: {error}"))
}

fn revert(reason: &'static str) -> PrecompileError {
    PrecompileError::Revert(reason.into())
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{Address, LogData, B256};
    use alloy_sol_types::SolEvent;
    use outbe_ocomp_protocol::{
        profile::poc_schema_limits,
        receipts::{EffectBindingV1, NodStateEventProjectionV1},
        result::{ExactCountsV1, ResultRootsV1},
    };
    use outbe_primitives::{
        error::{PrecompileError, Result},
        storage::{
            hashmap::HashMapStorageProvider, PrecompileStorageProvider, StorageHandle,
            SubCallError, SubCallInput, SubCallOutput,
        },
    };
    use revm::{
        context::journaled_state::JournalCheckpoint,
        state::{AccountInfo, Bytecode},
    };

    use super::*;

    struct ActivationTestProvider {
        inner: HashMapStorageProvider,
        active_call: Option<B256>,
    }

    impl ActivationTestProvider {
        fn new() -> Self {
            Self {
                inner: HashMapStorageProvider::new(1),
                active_call: None,
            }
        }

        fn run(&mut self, input: &CertifiedNodGenerationV1) -> Result<NodBatchReceiptV1> {
            StorageHandle::enter(self, |storage| {
                storage.with_lysis_activation_frame(
                    input.binding.activation_call_id,
                    |capability| {
                        let receipt = install_certified_generation(
                            &storage,
                            capability,
                            input,
                            &poc_schema_limits(),
                        )?;
                        capability.authorize_contributor_installation()?;
                        capability.authorize_tribute_retirement()?;
                        capability.authorize_carry_over_credit()?;
                        capability.authorize_terminal_receipt()?;
                        Ok(receipt)
                    },
                )
            })
        }

        fn generation(&mut self, wwd: WorldwideDay) -> Option<NodCertifiedGenerationProjection> {
            StorageHandle::enter(self, |storage| {
                NodContract::new(storage)
                    .ocomp_certified_generation(wwd)
                    .unwrap()
            })
        }
    }

    impl PrecompileStorageProvider for ActivationTestProvider {
        fn begin_lysis_activation_frame(&mut self, activation_call_id: B256) -> Result<()> {
            if self.active_call.replace(activation_call_id).is_some() {
                return Err(PrecompileError::Fatal(
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
            if self.active_call.take() != Some(activation_call_id) {
                return Err(PrecompileError::Fatal(
                    "test activation lease identity mismatch".into(),
                ));
            }
            Ok(())
        }

        fn chain_id(&self) -> u64 {
            self.inner.chain_id()
        }

        fn timestamp(&self) -> U256 {
            self.inner.timestamp()
        }

        fn set_block_timestamp(&mut self, timestamp: U256) {
            self.inner.set_block_timestamp(timestamp);
        }

        fn beneficiary(&self) -> Address {
            self.inner.beneficiary()
        }

        fn block_number(&self) -> u64 {
            self.inner.block_number()
        }

        fn canonical_block_hash(&mut self, number: u64) -> Result<Option<B256>> {
            self.inner.canonical_block_hash(number)
        }

        fn set_code(&mut self, address: Address, code: Bytecode) -> Result<()> {
            self.inner.set_code(address, code)
        }

        fn account_info(&mut self, address: Address) -> Result<AccountInfo> {
            self.inner.account_info(address)
        }

        fn sload(&mut self, address: Address, key: U256) -> Result<U256> {
            self.inner.sload(address, key)
        }

        fn tload(&mut self, address: Address, key: U256) -> Result<U256> {
            self.inner.tload(address, key)
        }

        fn sstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
            self.inner.sstore(address, key, value)
        }

        fn tstore(&mut self, address: Address, key: U256, value: U256) -> Result<()> {
            self.inner.tstore(address, key, value)
        }

        fn emit_event(&mut self, address: Address, event: LogData) -> Result<()> {
            self.inner.emit_event(address, event)
        }

        fn deduct_gas(&mut self, gas: u64) -> Result<()> {
            self.inner.deduct_gas(gas)
        }

        fn refund_gas(&mut self, gas: i64) {
            self.inner.refund_gas(gas);
        }

        fn gas_used(&self) -> u64 {
            self.inner.gas_used()
        }

        fn gas_refunded(&self) -> i64 {
            self.inner.gas_refunded()
        }

        fn is_static(&self) -> bool {
            self.inner.is_static()
        }

        fn checkpoint(&mut self) -> JournalCheckpoint {
            self.inner.checkpoint()
        }

        fn checkpoint_commit(&mut self) {
            self.inner.checkpoint_commit();
        }

        fn checkpoint_revert(&mut self, checkpoint: JournalCheckpoint) {
            self.inner.checkpoint_revert(checkpoint);
        }

        fn transfer_balance(&mut self, from: Address, to: Address, amount: U256) -> Result<()> {
            self.inner.transfer_balance(from, to, amount)
        }

        fn increase_balance(&mut self, address: Address, amount: U256) -> Result<()> {
            self.inner.increase_balance(address, amount)
        }

        fn decrease_balance(&mut self, address: Address, amount: U256) -> Result<()> {
            self.inner.decrease_balance(address, amount)
        }

        fn sub_call(
            &mut self,
            input: SubCallInput,
        ) -> std::result::Result<SubCallOutput, SubCallError> {
            self.inner.sub_call(input)
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

    fn input(wwd: u32, call: u8) -> CertifiedNodGenerationV1 {
        CertifiedNodGenerationV1 {
            binding: binding(call),
            precondition: NodTargetPreconditionV1 {
                wwd,
                target_generation: 0,
                namespace_root_before: B256::ZERO,
                max_nod_count: 3,
            },
            roots: ResultRootsV1 {
                nod_root: B256::repeat_byte(11),
                bucket_root: B256::repeat_byte(12),
                contributor_root: B256::repeat_byte(13),
                output_manifest_root: B256::repeat_byte(14),
            },
            counts: ExactCountsV1 {
                tribute_count: 3,
                nod_count: 3,
                bucket_count: 2,
                contributor_count: 2,
                semantic_event_count: 0,
            },
            nod_amount_total: U256::from(700),
            nod_gratis_consumed: U256::from(500),
            issued_at: 1_700_000_123,
        }
    }

    #[test]
    fn valid_capability_installs_exact_generation_receipt_and_event() {
        let mut provider = ActivationTestProvider::new();
        provider.inner.set_timestamp(U256::from(1_900_000_000));
        let input = input(20_260_725, 21);
        let receipt = provider.run(&input).unwrap();

        assert_eq!(
            provider.generation(WorldwideDay::new(input.precondition.wwd)),
            Some(NodCertifiedGenerationProjection {
                worldwide_day: WorldwideDay::new(input.precondition.wwd),
                generation: 1,
                nod_root: input.roots.nod_root,
                bucket_root: input.roots.bucket_root,
                output_manifest_root: input.roots.output_manifest_root,
                tribute_count: input.counts.tribute_count,
                nod_count: input.counts.nod_count,
                bucket_count: input.counts.bucket_count,
                nod_amount_total: input.nod_amount_total,
                nod_gratis_consumed: input.nod_gratis_consumed,
                issued_at: input.issued_at,
            })
        );
        assert_eq!(receipt.issued_at, input.issued_at);
        assert_ne!(receipt.issued_at, 1_900_000_000);
        assert_eq!(receipt.nod_count, input.counts.nod_count);
        assert_eq!(receipt.nod_root, input.roots.nod_root);
        assert_eq!(receipt.nod_amount_total, input.nod_amount_total);
        assert_eq!(receipt.nod_gratis_consumed, input.nod_gratis_consumed);

        let projection = NodStateEventProjectionV1 {
            wwd: input.precondition.wwd,
            target_generation: 0,
            namespace_root_before: B256::ZERO,
            nod_count: input.counts.nod_count,
            nod_root: input.roots.nod_root,
            nod_amount_total: input.nod_amount_total,
            nod_gratis_consumed: input.nod_gratis_consumed,
            issued_at: input.issued_at,
        };
        receipt
            .validate_projection(&projection, &poc_schema_limits())
            .unwrap();
        assert!(!receipt
            .receipt_hash(&poc_schema_limits())
            .unwrap()
            .is_zero());

        let events = provider.inner.get_ordered_events();
        assert_eq!(events.len(), 1);
        let event = INodFactory::CertifiedNodGenerationInstalled::decode_log(&events[0]).unwrap();
        assert_eq!(
            event.data.activationCallId,
            input.binding.activation_call_id
        );
        assert_eq!(event.data.worldwideDay, input.precondition.wwd);
        assert_eq!(event.data.targetGeneration, 0);
        assert_eq!(event.data.namespaceRootBefore, B256::ZERO);
        assert_eq!(event.data.tributeCount, input.counts.tribute_count);
        assert_eq!(event.data.nodCount, input.counts.nod_count);
        assert_eq!(event.data.bucketCount, input.counts.bucket_count);
        assert_eq!(event.data.nodRoot, input.roots.nod_root);
        assert_eq!(event.data.bucketRoot, input.roots.bucket_root);
        assert_eq!(
            event.data.outputManifestRoot,
            input.roots.output_manifest_root
        );
        assert_eq!(event.data.nodAmountTotal, input.nod_amount_total);
        assert_eq!(event.data.nodGratisConsumed, input.nod_gratis_consumed);
        assert_eq!(event.data.issuedAt, input.issued_at);
        assert_eq!(event.data.stateEventDigest, receipt.state_event_digest);
    }

    #[test]
    fn wrong_generation_root_and_exact_replay_are_side_effect_free() {
        let mut provider = ActivationTestProvider::new();
        let first = input(20_260_726, 22);
        provider.run(&first).unwrap();
        let state_after_first = provider.inner.storage.clone();
        let events_after_first = provider.inner.get_ordered_events().to_vec();

        assert!(provider.run(&first).is_err());
        assert_eq!(provider.inner.storage, state_after_first);
        assert_eq!(provider.inner.get_ordered_events(), events_after_first);

        let mut wrong = input(20_260_727, 23);
        wrong.precondition.target_generation = 7;
        wrong.precondition.namespace_root_before = B256::repeat_byte(77);
        let empty_state = provider.inner.storage.clone();
        let empty_events = provider.inner.get_ordered_events().to_vec();
        assert!(provider.run(&wrong).is_err());
        assert_eq!(provider.inner.storage, empty_state);
        assert_eq!(provider.inner.get_ordered_events(), empty_events);
    }

    #[test]
    fn exact_nonzero_generation_precondition_advances_once() {
        let mut provider = ActivationTestProvider::new();
        let first = input(20_260_733, 29);
        provider.run(&first).unwrap();

        let mut next = input(first.precondition.wwd, 30);
        next.precondition.target_generation = 1;
        next.precondition.namespace_root_before = first.roots.nod_root;
        next.roots.nod_root = B256::repeat_byte(41);
        next.roots.bucket_root = B256::repeat_byte(42);
        next.roots.output_manifest_root = B256::repeat_byte(43);
        next.nod_amount_total = U256::from(701);
        next.nod_gratis_consumed = U256::from(501);
        next.issued_at += 1;

        provider.run(&next).unwrap();
        assert_eq!(
            provider
                .generation(WorldwideDay::new(next.precondition.wwd))
                .unwrap(),
            NodCertifiedGenerationProjection {
                worldwide_day: WorldwideDay::new(next.precondition.wwd),
                generation: 2,
                nod_root: next.roots.nod_root,
                bucket_root: next.roots.bucket_root,
                output_manifest_root: next.roots.output_manifest_root,
                tribute_count: next.counts.tribute_count,
                nod_count: next.counts.nod_count,
                bucket_count: next.counts.bucket_count,
                nod_amount_total: next.nod_amount_total,
                nod_gratis_consumed: next.nod_gratis_consumed,
                issued_at: next.issued_at,
            }
        );
    }

    #[test]
    fn invalid_roots_counts_binding_and_time_never_mutate_state() {
        let base = input(20_260_728, 24);
        let mut invalid = Vec::new();

        let mut value = base.clone();
        value.roots.nod_root = B256::ZERO;
        invalid.push(value);

        let mut value = base.clone();
        value.counts.nod_count = 2;
        invalid.push(value);

        let mut value = base.clone();
        value.counts.bucket_count = 4;
        invalid.push(value);

        let mut value = base.clone();
        value.issued_at = 0;
        invalid.push(value);

        let mut value = base;
        value.binding.activation_call_id = B256::repeat_byte(99);
        invalid.push(value);

        for value in invalid {
            let mut provider = ActivationTestProvider::new();
            let call_id = if value.binding.activation_call_id == B256::repeat_byte(99) {
                B256::repeat_byte(24)
            } else {
                value.binding.activation_call_id
            };
            let result = StorageHandle::enter(&mut provider, |storage| {
                storage.with_lysis_activation_frame(call_id, |capability| {
                    install_certified_generation(&storage, capability, &value, &poc_schema_limits())
                })
            });
            assert!(result.is_err());
            assert!(provider.inner.storage.is_empty());
            assert!(provider.inner.get_ordered_events().is_empty());
        }
    }

    #[test]
    fn storage_and_event_failures_roll_back_every_owner_write() {
        for operation in 0..=7 {
            let mut provider = ActivationTestProvider::new();
            provider.inner.fail_after_mutation_at(operation);
            assert!(provider.run(&input(20_260_729, 25)).is_err());
            assert!(provider.inner.storage.is_empty());
            assert!(provider.inner.get_ordered_events().is_empty());
            provider.inner.clear_mutation_failure();
        }
    }

    #[test]
    fn caught_owner_failure_cannot_advance_the_activation_cursor() {
        let value = input(20_260_734, 31);
        let mut provider = ActivationTestProvider::new();
        provider.inner.fail_after_mutation_at(0);

        let receipt = StorageHandle::enter(&mut provider, |storage| {
            storage.with_lysis_activation_frame(value.binding.activation_call_id, |capability| {
                assert!(install_certified_generation(
                    &storage,
                    capability,
                    &value,
                    &poc_schema_limits(),
                )
                .is_err());

                let receipt = install_certified_generation(
                    &storage,
                    capability,
                    &value,
                    &poc_schema_limits(),
                )?;
                capability.authorize_contributor_installation()?;
                capability.authorize_tribute_retirement()?;
                capability.authorize_carry_over_credit()?;
                capability.authorize_terminal_receipt()?;
                Ok(receipt)
            })
        })
        .unwrap();

        assert_eq!(receipt.nod_root, value.roots.nod_root);
        assert_eq!(
            provider
                .generation(WorldwideDay::new(value.precondition.wwd))
                .unwrap()
                .generation,
            1
        );
        assert_eq!(provider.inner.get_ordered_events().len(), 1);
    }

    #[test]
    fn generations_are_isolated_by_worldwide_day() {
        let mut provider = ActivationTestProvider::new();
        let first = input(20_260_730, 26);
        let mut second = input(20_260_731, 27);
        second.roots.nod_root = B256::repeat_byte(31);
        second.roots.bucket_root = B256::repeat_byte(32);
        second.roots.output_manifest_root = B256::repeat_byte(33);

        provider.run(&first).unwrap();
        provider.run(&second).unwrap();

        assert_eq!(
            provider
                .generation(WorldwideDay::new(first.precondition.wwd))
                .unwrap()
                .nod_root,
            first.roots.nod_root
        );
        assert_eq!(
            provider
                .generation(WorldwideDay::new(second.precondition.wwd))
                .unwrap()
                .nod_root,
            second.roots.nod_root
        );
    }

    #[test]
    fn receipt_hash_is_stable_across_activation_block_times() {
        let input = input(20_260_732, 28);
        let mut first = ActivationTestProvider::new();
        first.inner.set_timestamp(U256::from(1));
        let first_receipt = first.run(&input).unwrap();

        let mut second = ActivationTestProvider::new();
        second.inner.set_timestamp(U256::from(u64::MAX));
        let second_receipt = second.run(&input).unwrap();

        assert_eq!(first_receipt, second_receipt);
        let receipt_hash = first_receipt.receipt_hash(&poc_schema_limits()).unwrap();
        assert_eq!(
            receipt_hash,
            alloy_primitives::b256!(
                "f71abea87390788dcbafe1eedc5a3f5c0e3f4c1df4c06833eba6e3badd5a62b8"
            )
        );
        assert_eq!(
            receipt_hash,
            second_receipt.receipt_hash(&poc_schema_limits()).unwrap()
        );
    }
}
