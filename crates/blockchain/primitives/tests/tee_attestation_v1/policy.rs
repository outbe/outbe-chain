use super::*;

fn measurement_rule(marker: u8) -> TeeMeasurementRuleV1 {
    TeeMeasurementRuleV1 {
        mrenclave: B256::repeat_byte(marker),
        mrsigner: B256::repeat_byte(marker + 1),
        isv_prod_id: u16::from(marker),
        minimum_isv_svn: 2,
        admit_from_height: 1,
        admit_until_height_exclusive: 1_000,
    }
}

fn policy(
    policy_version: u64,
    activation_height: u64,
    predecessor_policy_hash: B256,
) -> TeePolicyV1 {
    let resources = ResourceScheduleV1::normative().unwrap();
    TeePolicyV1 {
        policy_version,
        chain_id: [0; 32],
        genesis_hash: B256::repeat_byte(0x11),
        activation_height,
        predecessor_policy_hash,
        attestation_mode: AttestationMode::DcapRequired,
        intel_root_der_hash: B256::repeat_byte(0x72),
        quote_version: 3,
        tee_type: 0,
        attestation_key_type: 2,
        qe_vendor_id: [
            0x93, 0x9a, 0x72, 0x33, 0xf7, 0x9c, 0x4c, 0xa9, 0x94, 0x0a, 0x0d, 0xb3, 0x95, 0x7f,
            0x06, 0x07,
        ],
        certification_data_type: 5,
        tcb_info_schema_version: 3,
        qe_identity_schema_version: 2,
        minimum_tcb_evaluation_data_number: 1,
        accepted_platform_tcb_statuses: PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded,
        accepted_qe_tcb_status: QvlTcbStatusV1::UpToDate,
        minimum_lease: 3_600,
        maximum_lease: 604_800,
        collateral_margin: 3_600,
        resource_schedule_hash: resources.schedule_hash().unwrap(),
        measurement_rules: vec![measurement_rule(1), measurement_rule(3)],
    }
}

#[test]
fn tee_policy_accepts_at_most_thirty_days() {
    const THIRTY_DAYS: u64 = 30 * 24 * 60 * 60;

    let mut at_limit = policy(1, 1, B256::ZERO);
    at_limit.maximum_lease = THIRTY_DAYS;
    assert!(at_limit.encode_canonical().is_ok());

    let mut above_limit = at_limit;
    above_limit.maximum_lease = THIRTY_DAYS + 1;
    assert_eq!(
        above_limit.encode_canonical().unwrap_err(),
        CodecError::NonCanonical("invalid TEE lease policy")
    );
}

#[test]
fn policy_and_schedule_roundtrip_with_height_selection() {
    let first = policy(1, 1, B256::ZERO);
    let first_hash = first.policy_hash().unwrap();
    assert_eq!(
        hex::encode(first_hash),
        "c89d53264d68786a3126be04cc47799598c92871b0088aad1d23c397d1f47847"
    );
    let mut second = policy(2, 100, first_hash);
    second.accepted_platform_tcb_statuses = PlatformTcbStatusSetV1::UpToDateOnly;
    let schedule = TeePolicyScheduleV1 {
        chain_id: [0; 32],
        genesis_hash: B256::repeat_byte(0x11),
        entries: vec![
            TeePolicyScheduleEntryV1 {
                activation_height: 1,
                policy: first.clone(),
            },
            TeePolicyScheduleEntryV1 {
                activation_height: 100,
                policy: second.clone(),
            },
        ],
    };

    let encoded = schedule.encode_canonical().unwrap();
    assert_eq!(
        hex::encode(schedule.schedule_hash().unwrap()),
        "7f30b7850c913f571dee9cb8dbb8290f15903771a43d2c7f8b4ba6629f3dedf9"
    );
    assert_eq!(
        TeePolicyScheduleV1::decode_canonical(&encoded).unwrap(),
        schedule
    );
    assert_eq!(schedule.active_policy(1).unwrap(), &first);
    assert_eq!(schedule.active_policy(99).unwrap(), &first);
    assert_eq!(schedule.active_policy(100).unwrap(), &second);
    assert_eq!(
        schedule
            .active_policy(1)
            .unwrap()
            .accepted_platform_tcb_statuses,
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded
    );
    assert_eq!(
        schedule
            .active_policy(100)
            .unwrap()
            .accepted_platform_tcb_statuses,
        PlatformTcbStatusSetV1::UpToDateOnly
    );
    assert!(schedule.schedule_hash().is_ok());

    // Fixed V1 layout through minimum_tcb_evaluation_data_number is 178 bytes.
    let mut unknown_platform_status_set = first.encode_canonical().unwrap();
    assert_eq!(
        unknown_platform_status_set[178],
        PlatformTcbStatusSetV1::UpToDateOrHardeningNeeded as u8
    );
    unknown_platform_status_set[178] = 0xff;
    assert!(matches!(
        TeePolicyV1::decode_canonical(&unknown_platform_status_set),
        Err(CodecError::UnknownDiscriminant {
            field: "accepted Platform TCB status set",
            value: 0xff
        })
    ));

    let mut trailing = encoded;
    trailing.push(0);
    assert_eq!(
        TeePolicyScheduleV1::decode_canonical(&trailing).unwrap_err(),
        CodecError::TrailingBytes(1)
    );
}

#[test]
fn policy_schedule_rejects_duplicate_rules_and_broken_predecessor_chain() {
    let mut duplicate_rule_policy = policy(1, 1, B256::ZERO);
    duplicate_rule_policy.measurement_rules[1] = duplicate_rule_policy.measurement_rules[0].clone();
    assert_eq!(
        duplicate_rule_policy.encode_canonical().unwrap_err(),
        CodecError::NonCanonical("measurement rules must be strictly sorted and unique")
    );

    let first = policy(1, 1, B256::ZERO);
    let broken = TeePolicyScheduleV1 {
        chain_id: [0; 32],
        genesis_hash: B256::repeat_byte(0x11),
        entries: vec![
            TeePolicyScheduleEntryV1 {
                activation_height: 1,
                policy: first,
            },
            TeePolicyScheduleEntryV1 {
                activation_height: 100,
                policy: policy(2, 100, B256::repeat_byte(0xff)),
            },
        ],
    };
    assert_eq!(
        broken.encode_canonical().unwrap_err(),
        CodecError::NonCanonical("policy predecessor hash mismatch")
    );
}

#[test]
fn measurement_admission_counts_overlapping_matches_instead_of_accepting_any() {
    let mut candidate = policy(1, 1, B256::ZERO);
    let original = candidate.measurement_rules[0].clone();
    let mut overlapping = original.clone();
    overlapping.minimum_isv_svn = 1;
    candidate.measurement_rules.insert(0, overlapping);
    candidate.encode_canonical().unwrap();

    assert_eq!(
        candidate.measurement_rule_match_count(
            original.mrenclave,
            original.mrsigner,
            original.isv_prod_id,
            3,
            10,
        ),
        2
    );
}
