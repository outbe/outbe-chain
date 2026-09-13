use super::*;

#[test]
fn dispatch_rejects_external_caller_before_state_mutation() {
    let mut provider = configured_storage(1, 1);
    provider.enter(|storage| {
        let input = SystemTxInputV2::CycleTick.encode().unwrap();
        let err = dispatch(storage, &input, VALIDATOR, U256::ZERO).unwrap_err();
        assert!(matches!(err, PrecompileError::Revert(_)));
    });
}

#[test]
fn dispatch_rejects_native_value() {
    let mut provider = configured_storage(1, 1);
    provider.enter(|storage| {
        let input = SystemTxInputV2::CycleTick.encode().unwrap();
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
            crate::system_tx::SYSTEM_TX_INPUT_VERSION,
        ];
        let err = dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO).unwrap_err();
        assert!(matches!(err, PrecompileError::Fatal(_)));
        assert!(err.to_string().contains("unknown system tx selector"));
    });
}

#[test]
fn dispatch_rejects_wrong_version() {
    let mut provider = configured_storage(1, 1);
    provider.enter(|storage| {
        let mut input = SystemTxInputV2::CycleTick.encode().unwrap().to_vec();
        input[4] = crate::system_tx::SYSTEM_TX_INPUT_VERSION.saturating_add(1);
        let err = dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO).unwrap_err();
        assert!(matches!(err, PrecompileError::Fatal(_)));
        assert!(err
            .to_string()
            .contains("unsupported system tx input version"));
    });
}

#[test]
fn dispatch_cycle_tick_records_preloaded_proposer() {
    let mut provider = configured_storage(1, 1_700_000_000);
    provider.enable_metadosis_mutation_frame(
        outbe_primitives::storage::MetadosisMutationPurposeTag::CycleLifecycle,
    );
    provider.enter(|storage| {
        let input = SystemTxInputV2::CycleTick.encode().unwrap();
        with_preloaded_system_tx_context(
            PreloadedSystemTxContext {
                proposer: VALIDATOR,
                finalized_summary: None,
                allow_boundary_proposer: false,
                canonical_vrf_proof_hash: B256::ZERO,
            },
            || dispatch(storage, &input, SYSTEM_ADDRESS, U256::ZERO),
        )
        .unwrap();
    });

    provider.enter(|storage| {
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        let record = vs.get_validator(VALIDATOR).unwrap().unwrap();
        assert_eq!(record.blocks_proposed, 1);
    });
}
