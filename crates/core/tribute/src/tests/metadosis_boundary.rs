use super::sample_tribute;
use crate::TributeContract;
use alloy_primitives::U256;
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

#[test]
fn process_ready_mutators_reject_a_missing_lease_without_writes() {
    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        let _enclave = crate::enclave_client::test_enclave::scope();
        let mut tc = TributeContract::new(storage);
        let tribute = sample_tribute();
        tc.unseal_day(tribute.worldwide_day).unwrap();
        tc.issue(&tribute).unwrap();
        tc.seal_day(tribute.worldwide_day).unwrap();
        let totals = tc.get_day_totals(tribute.worldwide_day).unwrap();

        assert!(tc.freeze_day_nominal(&totals).is_err());
        assert!(tc
            .compute_and_record_metadosis_fractions(tribute.worldwide_day, U256::from(1u64), 1,)
            .is_err());
        assert!(tc.forfeit_sealed_partition(tribute.worldwide_day).is_err());
        assert!(tc
            .consume_lysis_partition(
                tribute.worldwide_day,
                totals.tribute_count,
                totals.tribute_nominal_total_minor,
            )
            .is_err());

        assert_eq!(tc.total_supply().unwrap(), 1);
        assert_eq!(
            tc.get_day_totals(tribute.worldwide_day)
                .unwrap()
                .tribute_count,
            1
        );
        assert_eq!(
            tc.get_day_totals(tribute.worldwide_day)
                .unwrap()
                .tribute_nominal_total_minor,
            tribute.nominal_amount_minor
        );
        assert!(tc
            .encrypted_day_nominal(tribute.worldwide_day, true)
            .unwrap()
            .is_empty());
        assert!(tc.get_record(tribute.tribute_id).unwrap().is_some());
    });
}
