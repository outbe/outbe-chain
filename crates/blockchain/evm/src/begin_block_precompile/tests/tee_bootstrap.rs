use super::*;

// ---- Phase 3b: TeeBootstrap handler verification ----

use outbe_primitives::{
    tee_attestation_v1::{
        AttestationMode, AttestationOperationV1, DcapCollateralComponentV1, DcapCollateralKind,
        NodeIdV1, PlatformTcbStatusSetV1, QvlTcbStatusV1, RegistrationIntentV1, ResourceScheduleV1,
        TeeAttestationManifestV1, TeeMeasurementRuleV1, TeePolicyScheduleEntryV1,
        TeePolicyScheduleV1, TeePolicyV1, ValidatorNodeBindingV1,
    },
    tee_bootstrap_v2::{
        TeeBootstrapCommitteeSignatureV2, TeeBootstrapParticipantEvidenceV2,
        TeeBootstrapParticipantV2, TeeBootstrapV2,
    },
    tee_test_utils::{gramine_direct_bootstrap_v2, gramine_direct_policy_v1, DevValidatorV1},
};

fn tee_signing_key(seed: u8) -> k256::ecdsa::SigningKey {
    k256::ecdsa::SigningKey::from_slice(&[seed; 32]).expect("non-zero scalar")
}

fn tee_evm_address(key: &k256::ecdsa::SigningKey) -> Address {
    let point = key.verifying_key().to_encoded_point(false);
    Address::from_slice(&keccak256(&point.as_bytes()[1..])[12..])
}

fn tee_sign(key: &k256::ecdsa::SigningKey, prehash: &B256) -> [u8; 65] {
    use k256::ecdsa::signature::hazmat::PrehashSigner;
    let (sig, recid): (k256::ecdsa::Signature, k256::ecdsa::RecoveryId) =
        key.sign_prehash(prehash.as_slice()).expect("sign prehash");
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(sig.to_bytes().as_slice());
    out[64] = recid.to_byte();
    out
}

/// Storage seeded with `members` as ACTIVE consensus participants (status
/// ACTIVE + BLS share present), which is exactly `get_active_consensus_set`.
fn tee_committee_storage(block_number: u64, members: &[Address]) -> HashMapStorageProvider {
    let mut provider =
        HashMapStorageProvider::new_with_chain_identity(CHAIN_ID, B256::repeat_byte(0x11));
    provider.set_block_number(block_number);
    provider.set_timestamp(U256::from(block_number.max(1)));
    provider.set_beneficiary(members[0]);
    provider.enter(|storage| {
        let mut vs = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
        vs.config_owner.write(OWNER).unwrap();
        vs.set_config_max_validators(128).unwrap();
        vs.config_epoch_length_blocks.write(10).unwrap();
        for (i, member) in members.iter().enumerate() {
            // Distinct consensus pubkey per member (uniqueness is enforced).
            let mut pubkey = [7u8; 48];
            pubkey[0] = i as u8;
            vs.register_validator(OWNER, *member, &pubkey).unwrap();
            vs.activate_validator_via_boundary_for_test(*member)
                .unwrap();
        }
    });
    provider
}

fn is_bootstrapped(provider: &mut HashMapStorageProvider) -> bool {
    provider.enter(|storage| {
        outbe_teeregistry::TeeRegistry::new(storage)
            .is_bootstrapped()
            .unwrap()
    })
}

fn keys(seeds: &[u8]) -> Vec<k256::ecdsa::SigningKey> {
    seeds.iter().map(|s| tee_signing_key(*s)).collect()
}

fn members(keys: &[k256::ecdsa::SigningKey]) -> Vec<Address> {
    keys.iter().map(tee_evm_address).collect()
}

/// Write an epoch-0 `CommitteeSnapshot` (as block 1's `BoundaryOutcome` would)
/// and return its canonical V2 identity hash.
fn write_epoch0_snapshot(provider: &mut HashMapStorageProvider, members: &[Address]) -> B256 {
    use outbe_consensus::proof::{CommitteeEntry, CommitteeSnapshot};
    let snapshot = CommitteeSnapshot {
        committee: members
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let mut pk = [7u8; 48];
                pk[0] = i as u8;
                CommitteeEntry {
                    address: *a,
                    consensus_pubkey: pk,
                }
            })
            .collect(),
        vrf_material_version: 1,
        vrf_group_public_key_bytes: vec![0x11; 96],
        vrf_public_polynomial_hash: B256::ZERO,
    };
    provider.enter(|storage| {
        outbe_validatorset::state::write_committee_snapshot(storage, 0, &snapshot).unwrap();
    });
    outbe_validatorset::committee_set_hash_v2(0, &snapshot)
}

fn tee_policy_v1() -> TeePolicyV1 {
    TeePolicyV1 {
        policy_version: 1,
        chain_id: U256::from(CHAIN_ID).to_be_bytes(),
        genesis_hash: B256::repeat_byte(0x11),
        activation_height: 1,
        predecessor_policy_hash: B256::ZERO,
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
        resource_schedule_hash: ResourceScheduleV1::normative()
            .unwrap()
            .schedule_hash()
            .unwrap(),
        measurement_rules: vec![TeeMeasurementRuleV1 {
            mrenclave: B256::repeat_byte(0x81),
            mrsigner: B256::repeat_byte(0x82),
            isv_prod_id: 1,
            minimum_isv_svn: 2,
            admit_from_height: 1,
            admit_until_height_exclusive: 1_000,
        }],
    }
}

fn tee_payload_v1(
    block_number: u64,
    keys: &[k256::ecdsa::SigningKey],
    policy: TeePolicyV1,
    committee_snapshot_hash: B256,
) -> TeeBootstrapV2 {
    let mut ordered = keys.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|key| tee_evm_address(key));
    let policy_hash = policy.policy_hash().unwrap();
    let collateral_pool = (1_u8..=8)
        .map(|kind| DcapCollateralComponentV1 {
            kind: DcapCollateralKind::try_from(kind).unwrap(),
            bytes: vec![kind],
        })
        .collect();
    let participants = ordered
        .iter()
        .enumerate()
        .map(|(index, key)| {
            let validator = tee_evm_address(key);
            let intent = RegistrationIntentV1 {
                chain_id: U256::from(CHAIN_ID).to_be_bytes(),
                genesis_hash: B256::repeat_byte(0x11),
                operation: AttestationOperationV1::RegisterEnclave,
                attestation_mode: AttestationMode::DcapRequired,
                policy_hash,
                node_id: NodeIdV1 {
                    reth_p2p_public: key
                        .verifying_key()
                        .to_encoded_point(true)
                        .as_bytes()
                        .try_into()
                        .unwrap(),
                },
                enclave_id: B256::repeat_byte(0x41 + index as u8),
                binding_id: B256::repeat_byte(0x51 + index as u8),
                binding_version: 1,
                registration_version: 0,
                renewal_nonce: 0,
                transition_nonce: 0,
                requested_valid_until: 7_200,
                recipient_x25519: [0x61 + index as u8; 32],
                attestation_ed25519: [0x71 + index as u8; 32],
                noise_responder_x25519: [0x81 + index as u8; 32],
                node_host_authorization_hash: B256::repeat_byte(0x91 + index as u8),
            };
            let validator_binding = ValidatorNodeBindingV1 {
                chain_id: U256::from(CHAIN_ID).to_be_bytes(),
                genesis_hash: B256::repeat_byte(0x11),
                validator: validator.into_array(),
                node_id_hash: intent.node_id.node_id_hash().unwrap(),
            };
            let binding_hash = validator_binding.binding_hash().unwrap();
            TeeBootstrapParticipantV2 {
                validator_binding,
                validator_signature: tee_sign(key, &binding_hash),
                node_binding_signature: tee_sign(key, &binding_hash),
                intent,
                evidence: TeeBootstrapParticipantEvidenceV2::Dcap {
                    quote: vec![0xA1 + index as u8; 64],
                    collateral_component_indices: [0, 1, 2, 3, 4, 5, 6, 7],
                },
                node_signature: [0xB1 + index as u8; 65],
                enclave_signature: [0xC1 + index as u8; 64],
            }
        })
        .collect::<Vec<_>>();
    let committee_signatures = ordered
        .iter()
        .map(|key| TeeBootstrapCommitteeSignatureV2 {
            validator: tee_evm_address(key),
            signature: [0; 65],
        })
        .collect();
    let mut payload = TeeBootstrapV2 {
        policy,
        committee_snapshot_hash,
        committee_snapshot_block: block_number,
        key_epoch: 0,
        tribute_offer_epoch: 0,
        dkg_transcript_hash: B256::ZERO,
        tribute_offer_public_key: B256::repeat_byte(0x23),
        tribute_offer_group_public_key: Bytes::from(vec![0x24; 96]),
        collateral_pool,
        participants,
        committee_signatures,
    };
    let signing_hash = payload.signing_hash().unwrap();
    for (signature, key) in payload.committee_signatures.iter_mut().zip(ordered) {
        signature.signature = tee_sign(key, &signing_hash);
    }
    payload
}

fn run_bootstrap_v1(provider: &mut HashMapStorageProvider, payload: TeeBootstrapV2) -> Result<()> {
    let policy_schedule = TeePolicyScheduleV1 {
        chain_id: payload.policy.chain_id,
        genesis_hash: payload.policy.genesis_hash,
        entries: vec![TeePolicyScheduleEntryV1 {
            activation_height: payload.policy.activation_height,
            policy: payload.policy.clone(),
        }],
    };
    let activation = crate::tee_attestation_activation::TeeAttestationChainSpecStateV1::Active(
        std::sync::Arc::new(
            crate::tee_attestation_activation::TeeAttestationActivationV1 {
                manifest: TeeAttestationManifestV1 {
                    activation_height: 1,
                    policy_schedule_hash: policy_schedule.schedule_hash().unwrap(),
                    resource_schedule_hash: payload.policy.resource_schedule_hash,
                },
                policy_schedule,
            },
        ),
    );
    provider.enter(|storage| {
        let input = SystemTxInputV2::TeeBootstrap { payload }.encode().unwrap();
        dispatch_inner(
            storage,
            &input,
            SYSTEM_ADDRESS,
            U256::ZERO,
            None,
            None,
            &activation,
        )
        .map(|_| ())
    })
}

#[test]
fn tee_bootstrap_v1_rejects_committee_subset_before_qvl_and_writes_nothing() {
    let all_keys = keys(&[0x11, 0x22, 0x33]);
    let mut sorted_members = members(&all_keys);
    sorted_members.sort();
    let mut provider = tee_committee_storage(1, &sorted_members);
    let snapshot_hash = write_epoch0_snapshot(&mut provider, &sorted_members);
    let policy = tee_policy_v1();
    let payload = tee_payload_v1(1, &all_keys[..2], policy, snapshot_hash);

    let error = run_bootstrap_v1(&mut provider, payload)
        .expect_err("a committee subset must fail before DCAP verification");
    assert!(error
        .to_string()
        .contains("complete active consensus committee"));
    assert!(!is_bootstrapped(&mut provider));
    provider.enter(|storage| {
        let registry = outbe_teeregistry::TeeRegistry::new(storage);
        for validator in sorted_members {
            assert!(registry
                .validator_enclave_binding_v1(validator)
                .unwrap()
                .is_none());
        }
    });
}

#[test]
fn tee_bootstrap_v1_routes_canonical_invalid_evidence_and_writes_nothing() {
    let all_keys = keys(&[0x11]);
    let mut sorted_members = members(&all_keys);
    sorted_members.sort();
    let mut provider = tee_committee_storage(1, &sorted_members);
    let snapshot_hash = write_epoch0_snapshot(&mut provider, &sorted_members);
    let policy = tee_policy_v1();
    let payload = tee_payload_v1(1, &all_keys, policy, snapshot_hash);

    run_bootstrap_v1(&mut provider, payload)
        .expect_err("synthetic quote must reach and fail the public DCAP verifier");
    assert!(!is_bootstrapped(&mut provider));
    provider.enter(|storage| {
        assert!(outbe_teeregistry::TeeRegistry::new(storage)
            .validator_enclave_binding_v1(sorted_members[0])
            .unwrap()
            .is_none());
    });
    assert!(outbe_primitives::system_tx::SystemTxKind::TeeBootstrap.revert_fails_block());
}

#[test]
fn gramine_direct_ost3_bootstraps_once_through_the_activated_dispatch() {
    let seeds = [0x11_u8, 0x22, 0x33];
    let signing_keys = keys(&seeds);
    let committee = members(&signing_keys);
    let mut provider = tee_committee_storage(1, &committee);
    let snapshot_hash = write_epoch0_snapshot(&mut provider, &committee);
    let policy = gramine_direct_policy_v1(CHAIN_ID, B256::repeat_byte(0x11)).unwrap();
    let validators = seeds
        .iter()
        .enumerate()
        .map(|(index, seed)| {
            let mut bls_minpk_public = [7_u8; 48];
            bls_minpk_public[0] = index as u8;
            DevValidatorV1 {
                evm_secret: [*seed; 32],
                bls_minpk_public,
            }
        })
        .collect::<Vec<_>>();
    let payload =
        gramine_direct_bootstrap_v2(policy, snapshot_hash, 1, 7_201, &validators).unwrap();

    run_bootstrap_v1(&mut provider, payload.clone()).unwrap();
    assert!(is_bootstrapped(&mut provider));
    provider.enter(|storage| {
        let registry = outbe_teeregistry::TeeRegistry::new(storage);
        for validator in &committee {
            assert!(registry
                .validator_enclave_binding_v1(*validator)
                .unwrap()
                .is_some());
        }
    });

    let error = run_bootstrap_v1(&mut provider, payload).unwrap_err();
    assert!(
        error.to_string().contains("already bootstrapped"),
        "{error}"
    );
}

#[test]
fn gramine_direct_ost3_rejects_a_signed_wrong_snapshot_hash() {
    let seeds = [0x11_u8];
    let signing_keys = keys(&seeds);
    let committee = members(&signing_keys);
    let mut provider = tee_committee_storage(1, &committee);
    let snapshot_hash = write_epoch0_snapshot(&mut provider, &committee);
    let policy = gramine_direct_policy_v1(CHAIN_ID, B256::repeat_byte(0x11)).unwrap();
    let validators = [DevValidatorV1 {
        evm_secret: [seeds[0]; 32],
        bls_minpk_public: [7_u8; 48],
    }];
    let payload = gramine_direct_bootstrap_v2(
        policy,
        snapshot_hash ^ B256::repeat_byte(1),
        1,
        7_201,
        &validators,
    )
    .unwrap();

    let error = run_bootstrap_v1(&mut provider, payload).unwrap_err();
    assert!(
        error.to_string().contains("snapshot hash mismatch"),
        "{error}"
    );
    assert!(!is_bootstrapped(&mut provider));
}
