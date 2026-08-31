use outbe_protocol_benchmarks::{
    render_text, run_scenario, BenchmarkRunReport, BenchmarkScenario, ChildFrameFidelity,
    ChildFrameTrace, ExecutionClass, ExecutionLayer, GasComponent, GasLedger, Observation, Profile,
    RunConfig, ScenarioFidelity, ScenarioMetadata,
};

struct PartialBoundaryScenario;

impl BenchmarkScenario for PartialBoundaryScenario {
    type Prepared = ();

    fn metadata(&self) -> ScenarioMetadata {
        ScenarioMetadata::new(
            "credis/create/request/partial-evm/single",
            "Credis request (partial EVM boundary)",
            ExecutionClass::UserTransaction,
            Profile::Single,
        )
        .with_execution_layer(ExecutionLayer::Marginal)
        .with_fidelity(ScenarioFidelity::PartialStubbed)
    }

    fn prepare(&self, _profile: Profile) -> Result<Self::Prepared, String> {
        Ok(())
    }

    fn run_once(&self, _prepared: &Self::Prepared) -> Result<Observation, String> {
        let mut observation = Observation::new(
            [(GasLedger::UserTransaction, 12_345)],
            [GasComponent::new(
                GasLedger::UserTransaction,
                "partial_evm.total",
                12_345,
                1,
            )],
        )
        .with_total_latency(1)
        .with_postcondition(
            "credis.full_smart_account_gas",
            "unavailable; TODO outbe-chain-6le.5",
        );
        observation.child_frames = vec![
            ChildFrameTrace {
                label: "vault_router.withdraw".to_owned(),
                target: "0x1017".to_owned(),
                selector: "0x12345678".to_owned(),
                status: "success".to_owned(),
                gas_used: 10_000,
                fidelity: ChildFrameFidelity::Production,
            },
            ChildFrameTrace {
                label: "token_bundle.top_up".to_owned(),
                target: "0xaaaa".to_owned(),
                selector: "0x87654321".to_owned(),
                status: "success".to_owned(),
                gas_used: 2_345,
                fidelity: ChildFrameFidelity::BenchmarkStub,
            },
        ];
        Ok(observation)
    }
}

#[test]
fn partial_boundary_fidelity_and_stub_frames_are_unmissable_in_text_and_json() {
    let scenario = run_scenario(
        &PartialBoundaryScenario,
        RunConfig {
            samples: 3,
            warmups: 0,
        },
    )
    .unwrap();
    let report = BenchmarkRunReport::new(vec![scenario]);

    let text = render_text(&report);
    assert!(text.contains("fidelity:            PARTIAL / STUBBED"));
    assert!(text.contains("token_bundle.top_up"));
    assert!(text.contains("BenchmarkStub"));
    assert!(text.contains("credis.full_smart_account_gas"));
    assert!(text.contains("TODO outbe-chain-6le.5"));

    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(
        json["scenarios"][0]["metadata"]["fidelity"],
        "partial_stubbed"
    );
    assert_eq!(
        json["scenarios"][0]["child_frames"][1]["fidelity"],
        "benchmark_stub"
    );
}
