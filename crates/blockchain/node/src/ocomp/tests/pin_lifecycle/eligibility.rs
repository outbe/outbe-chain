use super::*;

fn eligibility_registration(
    validator: Address,
    consensus_pubkey: &[u8; 48],
    key_seed: u8,
) -> (OcompKeyRegistrationV1, Vec<u8>) {
    let signing_key = SigningKey::from_bytes((&[key_seed; 32]).into()).unwrap();
    let mut registration = OcompKeyRegistrationV1 {
        core: OcompKeyRegistrationCoreV1 {
            chain_id: 42,
            genesis_hash: B256::ZERO,
            validator_identity_hash: validator_identity_hash_v1(validator, consensus_pubkey)
                .unwrap(),
            ocomp_public_key_sec1: signing_key
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                .try_into()
                .unwrap(),
            key_epoch: POC_KEY_EPOCH,
            allowed_purpose_bitmap: RESULT_SIGNATURE_PURPOSE_BITMAP,
        },
        proof_of_possession: [0; 64],
    };
    let limits = poc_schema_limits();
    let digest = registration.proof_of_possession_digest(&limits).unwrap();
    let signature: Signature = signing_key.sign_prehash(digest.as_slice()).unwrap();
    registration.proof_of_possession = signature
        .normalize_s()
        .unwrap_or(signature)
        .to_bytes()
        .into();
    let encoded = registration.encode_canonical(&limits).unwrap();
    (registration, encoded)
}

fn eligibility_snapshot_fixture(
    epoch: u64,
    member_count: u8,
    marker: u8,
) -> (CandidateProvider, ConsensusBlock, JobIntentV1, Vec<B256>) {
    let request = block(100 + epoch, B256::repeat_byte(marker), marker);
    let mut storage = HashMapStorageProvider::new(42);
    let (committee_set_hash, ocomp_binding_hash, key_hashes) =
        StorageHandle::enter(&mut storage, |storage| {
            let mut validator_set = ValidatorSet::new(storage.clone());
            validator_set.config_owner.write(Address::ZERO).unwrap();
            validator_set.set_config_max_validators(128).unwrap();

            let mut committee = Vec::with_capacity(usize::from(member_count));
            let mut key_hashes = Vec::with_capacity(usize::from(member_count));
            for index in 0..member_count {
                let validator = Address::repeat_byte(index.saturating_add(1));
                let consensus_pubkey = [index.saturating_add(0x31); 48];
                validator_set
                    .register_validator(Address::ZERO, validator, &consensus_pubkey)
                    .unwrap();
                validator_set.mark_pending(validator).unwrap();
                let (registration, encoded) = eligibility_registration(
                    validator,
                    &consensus_pubkey,
                    index.saturating_add(0x11),
                );
                validator_set
                    .confirm_validator_ready(validator, &encoded)
                    .unwrap();
                key_hashes.push(keccak256(registration.core.ocomp_public_key_sec1));
                committee.push(CommitteeEntry {
                    address: validator,
                    consensus_pubkey,
                });
            }
            let snapshot = CommitteeSnapshot {
                committee,
                vrf_material_version: epoch.saturating_add(1),
                vrf_group_public_key_bytes: vec![marker; 48],
                vrf_public_polynomial_hash: B256::ZERO,
            };
            let (committee_set_hash, snapshot_key) =
                write_committee_snapshot(storage.clone(), epoch, &snapshot).unwrap();
            let extension = read_ocomp_snapshot_extension(storage, snapshot_key)
                .unwrap()
                .expect("OCOMP snapshot extension");
            (committee_set_hash, extension.ocomp_binding_hash, key_hashes)
        });

    let chain_spec: ChainSpec<OutbeHeader> = ChainSpecBuilder::mainnet()
        .build()
        .map_header(OutbeHeader::new);
    let provider = MockEthProvider::<OutbePrimitives>::new().with_chain_spec(chain_spec);
    provider.add_header(request.block_hash(), request.header().clone());
    let validator_set_storage = storage
        .storage
        .into_iter()
        .filter_map(|((address, slot), value)| {
            (address == VALIDATOR_SET_ADDRESS)
                .then_some((B256::new(slot.to_be_bytes::<32>()), value))
        })
        .collect::<Vec<_>>();
    provider.add_account(
        VALIDATOR_SET_ADDRESS,
        ExtendedAccount::new(0, U256::ZERO)
            .with_bytecode(Bytes::from_static(&[0xef]))
            .extend_storage(validator_set_storage),
    );

    let mut intent = production_intent(request.number());
    intent.result_validator_set_epoch = epoch;
    intent.result_committee_set_hash = committee_set_hash;
    intent.result_ocomp_binding_hash = ocomp_binding_hash;
    intent.result_member_count = u16::from(member_count);
    intent.result_quorum_threshold = u16::from(member_count.saturating_mul(2) / 3 + 1);
    (provider, request, intent, key_hashes)
}

#[test]
fn ocomp_exact_request_eligibility_tracks_both_sides_of_activation_boundary() {
    let (old_provider, old_request, old_intent, old_keys) =
        eligibility_snapshot_fixture(0, 4, 0xa1);
    for key_hash in &old_keys {
        assert_eq!(
            ocomp_snapshot_contains_key_at(
                &old_provider,
                old_request.block_hash(),
                &old_intent,
                *key_hash,
            ),
            OcompSnapshotEligibilityV1::Eligible
        );
    }

    let (new_provider, new_request, new_intent, new_keys) =
        eligibility_snapshot_fixture(1, 5, 0xb1);
    assert_eq!(old_intent.result_member_count, 4);
    assert_eq!(new_intent.result_member_count, 5);
    assert_eq!(
        ocomp_snapshot_contains_key_at(
            &old_provider,
            old_request.block_hash(),
            &old_intent,
            new_keys[4],
        ),
        OcompSnapshotEligibilityV1::NotMember,
        "a request pinned before activation must exclude the incoming validator"
    );
    for key_hash in &new_keys {
        assert_eq!(
            ocomp_snapshot_contains_key_at(
                &new_provider,
                new_request.block_hash(),
                &new_intent,
                *key_hash,
            ),
            OcompSnapshotEligibilityV1::Eligible,
            "a request pinned after activation must include every advertised validator"
        );
    }
}
