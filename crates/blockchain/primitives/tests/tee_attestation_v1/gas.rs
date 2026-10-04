use super::*;

#[test]
fn resource_schedule_has_a_fixed_golden_vector() {
    let schedule = ResourceScheduleV1::normative().unwrap();
    let encoded = schedule.encode_canonical().unwrap();
    let expected = concat!(
        "01",
        "8879dd524fc4c5ccfc1c353b1f6840502f6e4f1eebc9825b27a8039bedf029a9",
        "11edf34f5614ee89ceb28c4597c309ad055cc67a752e335affa69fbc177c3da8",
        "000000001dcd6500",
        "0000000001c9c380"
    );

    assert_eq!(hex::encode(&encoded), expected);
    assert_eq!(
        ResourceScheduleV1::decode_canonical(&encoded).unwrap(),
        schedule
    );

    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        ResourceScheduleV1::decode_canonical(&trailing).unwrap_err(),
        CodecError::TrailingBytes(1)
    );

    let mut non_normative = schedule;
    non_normative.steady_block_gas_limit += 1;
    assert_eq!(
        non_normative.encode_canonical().unwrap_err(),
        CodecError::NonCanonical("non-normative block gas limits")
    );
}

#[test]
fn resource_schedule_binds_the_normative_system_and_registry_schedules() {
    let schedule = ResourceScheduleV1::normative().unwrap();
    assert_eq!(
        schedule.system_gas_schedule_hash,
        SystemGasScheduleV1::normative().schedule_hash().unwrap()
    );
    assert_eq!(
        schedule.tee_registry_gas_schedule_hash,
        TeeRegistryGasScheduleV1::normative()
            .schedule_hash()
            .unwrap()
    );

    let mut wrong_system_hash = schedule.encode_canonical().unwrap();
    wrong_system_hash[1] ^= 1;
    assert_eq!(
        ResourceScheduleV1::decode_canonical(&wrong_system_hash).unwrap_err(),
        CodecError::NonCanonical("non-normative resource schedule hashes")
    );
}

#[test]
fn normative_qvl_and_registry_gas_match_engineering_gate_vectors() {
    let gas = TeeRegistryGasScheduleV1::normative();
    assert_eq!(
        hex::encode(gas.schedule_hash().unwrap()),
        "11edf34f5614ee89ceb28c4597c309ad055cc67a752e335affa69fbc177c3da8"
    );
    assert_eq!(
        hex::encode(
            validator_intent(B256::repeat_byte(0x11))
                .intent_hash()
                .unwrap()
        ),
        "c93297665ac2c94b631f2adf0036b6b54031fdcb3e987c629fd86573ebed7660"
    );
    let encoded = gas.encode_canonical().unwrap();
    assert_eq!(
        TeeRegistryGasScheduleV1::decode_canonical(&encoded).unwrap(),
        gas
    );
    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        TeeRegistryGasScheduleV1::decode_canonical(&trailing).unwrap_err(),
        CodecError::TrailingBytes(1)
    );
    let mut non_normative = gas;
    non_normative.input_byte += 1;
    assert_eq!(
        non_normative.encode_canonical().unwrap_err(),
        CodecError::NonCanonical("non-normative TeeRegistry gas schedule")
    );
    let evidence_len = MAX_ATTESTATION_EVIDENCE_BYTES;
    let input_len = evidence_len + MAX_EVIDENCE_CALL_FRAMING_BYTES;

    assert!(
        gas.register_storage_gas_allowance() <= gas.register_fixed,
        "storage allowance must remain inside the normative fixed registration term"
    );
    for kind in [
        RegistryMutatorV1::RegisterEnclave,
        RegistryMutatorV1::RenewEnclave,
        RegistryMutatorV1::TransitionEnclaveMeasurement,
        RegistryMutatorV1::ReplaceEnclaveBinding,
    ] {
        assert!(gas.mutator_storage_gas_allowance(kind) <= 450_000);
    }
    assert_eq!(
        gas.qvl_dcap(evidence_len, MAX_ACTIVE_MEASUREMENT_RULES)
            .unwrap(),
        9_405_024
    );
    assert_eq!(
        gas.maximum_transaction_gas(
            RegistryMutatorV1::RegisterEnclave,
            input_len,
            evidence_len,
            MAX_ACTIVE_MEASUREMENT_RULES,
            AttestationMode::DcapRequired,
        )
        .unwrap(),
        28_848_784
    );
    assert_eq!(
        gas.maximum_transaction_gas(
            RegistryMutatorV1::RenewEnclave,
            input_len,
            evidence_len,
            MAX_ACTIVE_MEASUREMENT_RULES,
            AttestationMode::DcapRequired,
        )
        .unwrap(),
        28_668_784
    );
    assert_eq!(
        gas.maximum_transaction_gas(
            RegistryMutatorV1::ReplaceEnclaveBinding,
            input_len,
            evidence_len,
            MAX_ACTIVE_MEASUREMENT_RULES,
            AttestationMode::DcapRequired,
        )
        .unwrap(),
        29_133_784
    );
}

#[test]
fn dense_ost3_precharge_matches_the_consensus_vector() {
    let system = SystemGasScheduleV1::normative();
    let registry = TeeRegistryGasScheduleV1::normative();
    let logical_evidence_lengths = [MAX_ATTESTATION_EVIDENCE_BYTES; 32];

    assert_eq!(
        system
            .tee_bootstrap_precharge(
                &registry,
                TeeBootstrapGasInputV1 {
                    full_calldata_len: MAX_TEE_BOOTSTRAP_BYTES,
                    logical_evidence_lengths: &logical_evidence_lengths,
                    active_rule_count: MAX_ACTIVE_MEASUREMENT_RULES,
                    collateral_component_count: 32 * 8,
                    committee_signature_count: 32,
                },
            )
            .unwrap(),
        309_931_488
    );
}

#[test]
fn system_gas_schedule_has_canonical_bytes() {
    let schedule = SystemGasScheduleV1::normative();
    let encoded = schedule.encode_canonical().unwrap();
    assert_eq!(
        hex::encode(&encoded),
        concat!(
            "01",
            "00000000000493e0",
            "0000000000000001",
            "00000000000186a0",
            "0000000000003a98",
            "0000000000002710"
        )
    );
    assert_eq!(
        SystemGasScheduleV1::decode_canonical(&encoded).unwrap(),
        schedule
    );
    assert_eq!(
        hex::encode(schedule.schedule_hash().unwrap()),
        "8879dd524fc4c5ccfc1c353b1f6840502f6e4f1eebc9825b27a8039bedf029a9"
    );

    let mut trailing = encoded.clone();
    trailing.push(0);
    assert_eq!(
        SystemGasScheduleV1::decode_canonical(&trailing).unwrap_err(),
        CodecError::TrailingBytes(1)
    );

    let mut non_normative = encoded;
    non_normative[8] ^= 1;
    assert_eq!(
        SystemGasScheduleV1::decode_canonical(&non_normative).unwrap_err(),
        CodecError::NonCanonical("non-normative system gas schedule")
    );
}

#[test]
fn ost3_gas_rejects_caps_and_checked_overflow() {
    let system = SystemGasScheduleV1::normative();
    let registry = TeeRegistryGasScheduleV1::normative();
    let one_evidence = [1_usize];

    let calculate = |full_calldata_len, logical_evidence_lengths: &[usize], component_count| {
        system.tee_bootstrap_precharge(
            &registry,
            TeeBootstrapGasInputV1 {
                full_calldata_len,
                logical_evidence_lengths,
                active_rule_count: 1,
                collateral_component_count: component_count,
                committee_signature_count: 1,
            },
        )
    };

    assert!(matches!(
        calculate(MAX_TEE_BOOTSTRAP_BYTES + 1, &one_evidence, 8),
        Err(CodecError::LimitExceeded {
            field: "TeeBootstrapV2 full calldata",
            ..
        })
    ));
    assert!(matches!(
        calculate(
            MAX_TEE_BOOTSTRAP_BYTES,
            &[MAX_ATTESTATION_EVIDENCE_BYTES + 1],
            8,
        ),
        Err(CodecError::LimitExceeded {
            field: "attestation evidence",
            ..
        })
    ));
    assert_eq!(
        calculate(MAX_TEE_BOOTSTRAP_BYTES, &one_evidence, usize::MAX).unwrap_err(),
        CodecError::ArithmeticOverflow
    );
}

#[test]
fn report_data_has_fixed_intent_and_node_host_policy_commitments() {
    let intent = validator_intent(B256::repeat_byte(0x11));
    let report_data = intent.report_data().unwrap();

    assert_eq!(
        hex::encode(&report_data[..32]),
        "c93297665ac2c94b631f2adf0036b6b54031fdcb3e987c629fd86573ebed7660"
    );
    assert_eq!(
        hex::encode(&report_data[32..]),
        "378c9bbee1671eeb2d8447ba76919a81f2175148800244ff0ca20c2e907d5216"
    );
}

#[test]
fn gas_calculators_reject_cap_plus_one_and_checked_overflow() {
    let gas = TeeRegistryGasScheduleV1::normative();
    assert!(matches!(
        gas.qvl_dcap(MAX_ATTESTATION_EVIDENCE_BYTES + 1, 1),
        Err(CodecError::LimitExceeded {
            field: "attestation evidence",
            ..
        })
    ));
    assert!(matches!(
        gas.qvl_dcap(1, MAX_ACTIVE_MEASUREMENT_RULES + 1),
        Err(CodecError::LimitExceeded {
            field: "active measurement rules",
            ..
        })
    ));
    assert_eq!(
        gas.maximum_calldata_intrinsic_gas(usize::MAX).unwrap_err(),
        CodecError::ArithmeticOverflow
    );
}
