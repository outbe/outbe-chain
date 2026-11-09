use super::*;

#[test]
fn dispatch_rejects_external_caller_before_state_mutation() {
    let mut provider = configured_storage(1, 1);
    provider.enter(|storage| {
        let input = SystemTx::CycleTick.encode().unwrap();
        let err = dispatch(storage, &input, VALIDATOR, U256::ZERO).unwrap_err();
        assert!(matches!(err, PrecompileError::Revert(_)));
    });
}

#[test]
fn dispatch_rejects_native_value() {
    let mut provider = configured_storage(1, 1);
    provider.enter(|storage| {
        let input = SystemTx::CycleTick.encode().unwrap();
        let err = dispatch(storage, &input, SYSTEM_ADDRESS, U256::from(1u64)).unwrap_err();
        assert!(matches!(err, PrecompileError::Revert(_)));
    });
}

#[test]
fn dispatch_rejects_unknown_selector() {
    let mut provider = configured_storage(1, 1);
    provider.enter(|storage| {
        let input = [
            0xff,
            0xff,
            0xff,
            0xff,
            outbe_evm::system_tx::SYSTEM_TX_INPUT_VERSION,
        ];
        let err = dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO).unwrap_err();
        assert!(matches!(err, PrecompileError::Revert(_)));
        assert!(err.to_string().contains("unknown system tx selector"));
    });
}

#[test]
fn dispatch_rejects_wrong_version() {
    let mut provider = configured_storage(1, 1);
    provider.enter(|storage| {
        let mut input = SystemTx::CycleTick.encode().unwrap().to_vec();
        input[4] = outbe_evm::system_tx::SYSTEM_TX_INPUT_VERSION.saturating_add(1);
        let err = dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO).unwrap_err();
        assert!(matches!(err, PrecompileError::Revert(_)));
        assert!(err
            .to_string()
            .contains("unsupported system tx input version"));
    });
}

#[test]
fn dispatch_cycle_tick_records_preloaded_proposer() {
    let mut provider = configured_storage(1, 1_700_000_000);
    provider.enable_production_storage_gas_metering();
    provider.set_gas_limit(outbe_primitives::system_tx::SYSTEM_TX_ARTIFACT_GAS_LIMIT);
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    provider.enter(|storage| {
        let input = SystemTx::CycleTick.encode().unwrap();
        with_preloaded_system_tx_context(
            PreloadedSystemTxContext {
                proposer: VALIDATOR,
                finalized_summary: None,
                allow_boundary_proposer: false,
                canonical_vrf_proof_hash: B256::ZERO,
                verified_late_credit_signers: None,
                block_gas_limit: 0,
                user_gas_used: 0,
            },
            || {
                let liveness = SystemTx::ValidatorLiveness.encode().unwrap();
                dispatch(storage.clone(), &liveness, SYSTEM_ADDRESS, U256::ZERO).unwrap();
                dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO)
            },
        )
        .unwrap();
    });

    provider.enter(|storage| {
        let cycle = storage.contract::<outbe_cycle::schema::Cycle<'_>>();
        assert_eq!(
            cycle
                .last_executed_at
                .read(&outbe_cycle::triggers::TriggerId::ProtocolCycle.as_u32())
                .unwrap(),
            1_700_000_000
        );
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        let record = vs.get_validator(VALIDATOR).unwrap().unwrap();
        assert_eq!(record.blocks_proposed, 1);
    });
}
