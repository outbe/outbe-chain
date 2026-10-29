use super::sample_tribute;
use crate::TributeContract;
use alloy_primitives::{B256, U256};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::{
    metadosis_advance_due_binding, metadosis_cycle_allocation_binding,
    metadosis_init_genesis_binding, metadosis_late_settlement_binding,
    metadosis_process_ready_binding, MetadosisCertifiedFinality, MetadosisCycleLifecycle,
    MetadosisMutationPurposeTag, StorageHandle,
};
use outbe_primitives::time::WorldwideDay;

const MISMATCH: &str = "revert: test provider Metadosis mutation lease identity mismatch";

fn open_cycle<R>(
    storage: &StorageHandle<'_>,
    binding_for: fn(u64, u64, u64) -> B256,
    body: impl FnOnce() -> outbe_primitives::error::Result<R>,
) -> R {
    let binding = binding_for(
        storage.chain_id().unwrap(),
        storage.block_number().unwrap(),
        storage.now_secs().unwrap(),
    );
    storage
        .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, body)
        .unwrap()
}

#[test]
fn day_mutators_reject_a_missing_lease_without_writes() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CycleLifecycle);
    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let mut tc = TributeContract::new(storage.clone());
        let tribute = sample_tribute();
        let day = tribute.worldwide_day;
        open_cycle(&storage, metadosis_process_ready_binding, || {
            tc.issue(&tribute)?;
            tc.seal_day(day)?;
            Ok(())
        });
        let totals = tc.get_day_totals(day).unwrap();

        assert_eq!(
            tc.freeze_day_nominal(&totals).unwrap_err().to_string(),
            MISMATCH
        );
        assert_eq!(
            tc.compute_and_record_metadosis_fractions(day, U256::from(1u64), 1)
                .unwrap_err()
                .to_string(),
            MISMATCH
        );
        assert_eq!(
            tc.forfeit_sealed_partition(day).unwrap_err().to_string(),
            MISMATCH
        );
        assert_eq!(
            tc.consume_lysis_partition(
                day,
                totals.tribute_count,
                totals.tribute_nominal_total_minor
            )
            .unwrap_err()
            .to_string(),
            MISMATCH
        );
        assert_eq!(tc.seal_day(day).unwrap_err().to_string(), MISMATCH);
        assert_eq!(
            tc.record_metadosis_fractions(
                day,
                U256::from(1u64),
                &std::collections::BTreeMap::new(),
                1
            )
            .unwrap_err()
            .to_string(),
            MISMATCH
        );
        assert_eq!(
            crate::state_api::retire_completed_day(&storage, day)
                .unwrap_err()
                .to_string(),
            MISMATCH
        );
        assert_eq!(tc.day_metadosis_completed_at.read(&day).unwrap(), 0);
        assert!(!tc.retired_days.read(&day).unwrap());

        assert_eq!(tc.total_supply().unwrap(), 1);
        assert_eq!(tc.get_day_totals(day).unwrap().tribute_count, 1);
        assert_eq!(
            tc.get_day_totals(day).unwrap().tribute_nominal_total_minor,
            tribute.nominal_amount_minor
        );
        assert!(tc.is_day_sealed(day).unwrap());
        assert!(tc.encrypted_day_nominal(day, true).unwrap().is_empty());
        assert!(tc.get_record(tribute.tribute_id).unwrap().is_some());
    });
}

#[test]
fn day_mutators_keep_a_timestamp_overflow() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        storage.set_block_timestamp(U256::MAX).unwrap();
        let mut tc = TributeContract::new(storage);
        let day = WorldwideDay::new(20260920);
        let error = tc.seal_day(day).unwrap_err();
        assert_eq!(error.to_string(), "revert: timestamp exceeds u64");
        assert!(!matches!(error, PrecompileError::Revert(message) if message.contains("lease")));
        assert!(!tc.is_day_sealed(day).unwrap());
    });
}

#[test]
fn day_mutators_follow_the_caller_binding() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.enable_metadosis_mutation_frames(MetadosisMutationPurposeTag::CycleLifecycle, 4);
    let day_a = WorldwideDay::new(20260920);
    let day_b = WorldwideDay::new(20260921);
    let day_c = WorldwideDay::new(20260922);
    let day_d = WorldwideDay::new(20260923);
    StorageHandle::enter(&mut provider, |storage| {
        let mut tc = TributeContract::new(storage.clone());
        open_cycle(&storage, metadosis_init_genesis_binding, || {
            tc.seal_day(day_a)?;
            assert!(tc.is_day_sealed(day_a)?);
            Ok(())
        });
        // Allocation targets a UTC day independently of the current execution time.
        let binding = metadosis_cycle_allocation_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            day_b.to_timestamp_utc(),
        );
        storage
            .with_metadosis_mutation::<MetadosisCycleLifecycle, _>(binding, || {
                tc.seal_day(day_b)?;
                assert!(tc.is_day_sealed(day_b)?);
                assert_eq!(tc.seal_day(day_c).unwrap_err().to_string(), MISMATCH);
                Ok(())
            })
            .unwrap();
        open_cycle(&storage, metadosis_advance_due_binding, || {
            tc.seal_day(day_a)?;
            assert!(tc.is_day_sealed(day_a)?);
            Ok(())
        });
        open_cycle(&storage, metadosis_process_ready_binding, || {
            tc.seal_day(day_d)?;
            assert!(tc.is_day_sealed(day_d)?);
            Ok(())
        });
    });
}

#[test]
fn certified_finality_rejects_day_mutation_without_writes() {
    let mut provider = HashMapStorageProvider::new(1);
    provider.enable_metadosis_mutation_frame(MetadosisMutationPurposeTag::CertifiedFinality);
    let day = WorldwideDay::new(20260920);
    StorageHandle::enter(&mut provider, |storage| {
        let binding = metadosis_late_settlement_binding(
            storage.chain_id().unwrap(),
            storage.block_number().unwrap(),
            storage.now_secs().unwrap(),
        );
        storage
            .with_metadosis_mutation::<MetadosisCertifiedFinality, _>(binding, || {
                let mut tc = TributeContract::new(storage.clone());
                assert_eq!(tc.seal_day(day).unwrap_err().to_string(), MISMATCH);
                assert!(!tc.is_day_sealed(day)?);
                Ok(())
            })
            .unwrap();
    });
}
