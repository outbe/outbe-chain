use outbe_protocol_benchmarks::{
    run_scenario, scenarios::intex::IntexScenario, GasLedger, Profile, RunConfig,
};

#[test]
fn intex_issue_and_send_cover_single_and_typical_profiles() {
    for (profile, expected_count) in [(Profile::Single, "1"), (Profile::Typical, "10")] {
        let config = RunConfig {
            samples: 3,
            warmups: 0,
        };
        let issue = run_scenario(&IntexScenario::issue(profile), config).unwrap();
        assert_eq!(issue.postconditions["intex.created_count"], expected_count);
        assert_eq!(issue.postconditions["intex.all_readable"], "true");
        assert!(issue.gas_totals[&GasLedger::SystemInternal] > 0);

        let send = run_scenario(&IntexScenario::send(profile), config).unwrap();
        assert_eq!(send.postconditions["intex.issuance_legs"], expected_count);
        assert_eq!(send.postconditions["intex.send_completed"], "true");
        assert!(send.gas_totals.contains_key(&GasLedger::SystemInternal));
    }
}
