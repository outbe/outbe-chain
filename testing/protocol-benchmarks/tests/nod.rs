use outbe_primitives::addresses::NOD_FACTORY_ADDRESS;
use outbe_protocol_benchmarks::{
    run_scenario, scenarios::nod::NodScenario, GasLedger, Profile, RunConfig, StorageOperationKind,
};

#[test]
fn direct_nod_creation_covers_single_and_typical_profiles() {
    for (profile, expected_count) in [(Profile::Single, 1_u64), (Profile::Typical, 10_u64)] {
        let report = run_scenario(
            &NodScenario::direct(profile),
            RunConfig {
                samples: 3,
                warmups: 0,
            },
        )
        .unwrap();

        assert_eq!(
            report.postconditions["nod.created_count"],
            expected_count.to_string()
        );
        assert_eq!(report.postconditions["nod.all_readable"], "true");
        assert!(report.gas_totals[&GasLedger::SystemInternal] > 0);
        assert!(report
            .storage
            .iter()
            .any(|entry| entry.operation == StorageOperationKind::Write));
        let nod_factory_emitter = format!("{NOD_FACTORY_ADDRESS:#x}");
        assert_eq!(
            report
                .events
                .iter()
                .filter(|event| event.emitter == nod_factory_emitter)
                .map(|event| event.count)
                .sum::<u64>(),
            expected_count
        );
    }
}

#[test]
fn certified_nod_materialization_covers_single_and_two_batch_typical_profiles() {
    for (profile, expected_count, expected_batches) in [
        (Profile::Single, 1_u64, "1"),
        (Profile::Typical, 10_u64, "2"),
    ] {
        let report = run_scenario(
            &NodScenario::certified(profile),
            RunConfig {
                samples: 3,
                warmups: 0,
            },
        )
        .unwrap();

        assert_eq!(
            report.postconditions["nod.created_count"],
            expected_count.to_string()
        );
        assert_eq!(
            report.postconditions["nod.materialization_batches"],
            expected_batches
        );
        assert_eq!(
            report.postconditions["nod.materialization_completed"],
            "true"
        );
        assert_eq!(report.postconditions["nod.all_readable"], "true");
        assert!(report.gas_totals[&GasLedger::SystemVisible] > 0);
    }
}
