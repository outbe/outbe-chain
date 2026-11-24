use alloy_sol_types::SolEvent;
use outbe_nod::precompile::INod;
use outbe_protocol_benchmarks::{
    run_scenario, scenarios::nod::NodScenario, GasLedger, Profile, RunConfig, StorageOperationKind,
};
use outbe_system_common::addresses::NOD_ADDRESS;

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
        let nod_emitter = format!("{NOD_ADDRESS:#x}");
        let issued = format!("{:#x}", INod::NodIssued::SIGNATURE_HASH);
        assert_eq!(
            report
                .events
                .iter()
                .filter(|event| event.emitter == nod_emitter && event.event == issued)
                .map(|event| event.count)
                .sum::<u64>(),
            expected_count
        );
    }
}
