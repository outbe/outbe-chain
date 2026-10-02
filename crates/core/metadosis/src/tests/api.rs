use super::*;
use crate::{aggregate::WwdMembership, api};

#[test]
fn typed_queries_distinguish_missing_days_from_offering_membership() {
    with_storage(|storage| {
        let wwd = 20270101.into();
        assert!(api::worldwide_day(storage.clone(), wwd).unwrap().is_none());
        assert!(!api::is_offering_day(storage.clone(), wwd).unwrap());
        assert!(api::offering_worldwide_days(storage.clone())
            .unwrap()
            .is_empty());
        assert!(api::bootstrap_end_time(storage.clone()).unwrap().is_none());
        super::precompile::seed_offering_day(storage.clone());
        let day = api::worldwide_day(storage.clone(), wwd).unwrap().unwrap();
        assert_eq!(
            (day.status, day.membership),
            (crate::WwdStatus::Offering, WwdMembership::Active)
        );
        assert!(api::is_offering_day(storage.clone(), wwd).unwrap());
        assert_eq!(
            api::offering_worldwide_days(storage.clone()).unwrap(),
            vec![wwd]
        );
        assert_eq!(api::worldwide_days(storage.clone()).unwrap(), vec![day]);
        MetadosisContract::new(storage.clone())
            .set_bootstrap_end_time(30)
            .unwrap();
        assert_eq!(api::bootstrap_end_time(storage).unwrap(), Some(30));
    });
}

#[test]
fn typed_day_query_rejects_each_invalid_membership_cardinality() {
    for active in [false, true] {
        with_storage(|storage| {
            let wwd = 20270101.into();
            super::precompile::seed_offering_day(storage.clone());
            let contract = MetadosisContract::new(storage.clone());
            if active {
                contract.closed_wwd.push_back(wwd).unwrap();
            } else {
                contract.active_wwd.remove(&wwd).unwrap();
            }
            let error = api::worldwide_day(storage, wwd).unwrap_err();
            assert!(
                matches!(error, outbe_primitives::error::PrecompileError::Fatal(message)
                if message == "Metadosis WWD membership is not exactly one of active/closed")
            );
        });
    }
}

#[test]
fn offering_query_fails_closed_for_dangling_active_key_and_unknown_status() {
    with_storage(|storage| {
        let wwd = 20270101.into();
        let contract = MetadosisContract::new(storage.clone());
        contract.active_wwd.insert(wwd).unwrap();
        assert!(api::offering_worldwide_days(storage.clone()).is_err());
        contract.active_wwd.remove(&wwd).unwrap();
        super::precompile::seed_offering_day(storage.clone());
        contract
            .worldwide_days
            .entry(wwd)
            .status()
            .write(255)
            .unwrap();
        assert!(api::offering_worldwide_days(storage.clone()).is_err());
        assert!(api::worldwide_day(storage, wwd).is_err());
    });
}

#[test]
fn semantic_queries_report_absent_authority_and_receipts_without_inventing_state() {
    with_storage(|storage| {
        let wwd = 20270101.into();
        assert!(!api::has_active_ocomp_profile(storage.clone()).unwrap());
        let install = crate::test_support::ForkInstallScenario::final_at(
            crate::config::OCOMP_POC_FINAL_ACTIVATION_HEIGHT,
            1,
            alloy_primitives::B256::repeat_byte(0x91),
        )
        .unwrap()
        .into_install();
        assert!(!api::is_active_ocomp_fork_install(storage.clone(), &install).unwrap());
        assert!(api::day_limit_formation_receipt(storage.clone(), wwd)
            .unwrap()
            .is_none());
        assert!(api::missed_offering_receipt(storage.clone(), wwd)
            .unwrap()
            .is_none());
        assert!(api::capacity_forfeiture_receipt(storage.clone(), wwd)
            .unwrap()
            .is_none());
        let admission = api::pre_admission_projection(storage, wwd).unwrap();
        assert!(!admission.initialized);
        assert_eq!(admission.state_version, 0);
        assert_eq!(admission.envelope_hash, alloy_primitives::B256::ZERO);
    });
}
