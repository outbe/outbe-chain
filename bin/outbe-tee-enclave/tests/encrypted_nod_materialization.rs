use alloy_primitives::{Address, B256, U256};
use outbe_ocomp_protocol::{
    list::{leaf_hash, root_hash},
    nod_materialization::{NodMaterializationBatchV1, NodMaterializationHeadV1},
    profile::poc_schema_limits,
    registry::ListKind,
    result::NodActionV1,
};
use outbe_primitives::{
    time::WorldwideDay,
    tribute_encryption::{TributeAmountsV2, TributeContextV2},
    wwd_entity_id::WwdEntityId,
};
use outbe_tee::{
    nod_materialization::LeagueFractionInputV2,
    nod_state_materialization::{PrepareStateNodsRequest, StateNodAuthority},
    tribute_source::{TributeSourceCheckpoint, TributeSourceSubject, TributeStateSource},
};
use outbe_tee_enclave::{
    nod_encryption::decrypt_nod,
    nod_state_materialization::{open, prepare, request_sources},
    tribute_encryption::encrypt_tribute,
};
use x25519_dalek::{PublicKey, StaticSecret};

#[test]
fn materialization_binds_current_authority_and_exact_authenticated_source() {
    let secret = [7; 32];
    let creator = PublicKey::from(&StaticSecret::from([11; 32])).to_bytes();
    let day = WorldwideDay::new(20250115);
    let owner = Address::repeat_byte(3);
    let tribute_id =
        outbe_primitives::wwd_entity_id::derive_poseidon_entity_id(owner, day).unwrap();
    let context = TributeContextV2 {
        chain_id: 1,
        tribute_id,
        owner,
        worldwide_day: day,
        issuance_currency: 840,
        reference_currency: 978,
        tribute_price_minor: U256::ONE,
        offer_input_hash: B256::repeat_byte(5),
    };
    let amounts = TributeAmountsV2 {
        issuance_amount_minor: U256::from(1000),
        nominal_amount_minor: U256::from(123),
    };
    let tribute = encrypt_tribute(&secret, &creator, context.clone(), &amounts).unwrap();
    let (subject, source) = native_source(tribute);
    let nod_id = WwdEntityId::from_day_and_digest(day, B256::repeat_byte(6));
    let action = NodActionV1 {
        raw_ordinal: 0,
        tribute_id: B256::from_slice(tribute_id.as_slice()),
        nod_id: B256::from_slice(nod_id.as_slice()),
        owner,
        wwd: 20250115,
        league_id: 0,
        entry_price_minor: U256::from(700),
        issuance_currency: 840,
        reference_currency: 978,
    };
    let limits = poc_schema_limits();
    let leaf = leaf_hash(
        ListKind::NodActions,
        0,
        &action.encode_canonical_record(&limits).unwrap(),
    )
    .unwrap();
    let root = root_hash(ListKind::NodActions, 1, 0, leaf).unwrap();
    let head = NodMaterializationHeadV1 {
        queue_sequence: 1,
        job_id: B256::repeat_byte(10),
        program_semantics_hash: B256::repeat_byte(11),
        worldwide_day: 20250115,
        generation: 1,
        nod_root: root,
        nod_count: 1,
        next_nod_ordinal: 0,
        last_progress_height: 1,
    };
    let batch = NodMaterializationBatchV1 {
        queue_sequence: 1,
        first_nod_ordinal: 0,
        actions: vec![action],
        root_path: vec![],
    };
    let mut authority = StateNodAuthority {
        source: subject,
        head: head.encode_canonical(&limits).unwrap(),
        subtree_height: 3,
        source_bindings: Vec::new(),
        nod_gratis_load_max: U256::from(100),
    };
    let mut request = PrepareStateNodsRequest {
        authority: authority.clone(),
        batch: batch.encode_canonical(&limits).unwrap(),
        sources: Vec::new(),
        league_fractions: vec![LeagueFractionInputV2 {
            league_id: 0,
            fraction_fp: U256::from(1_000_000),
        }],
    };
    let lookup = request_sources(&request).unwrap();
    assert_eq!(lookup.subject, subject);
    assert_eq!(lookup.tribute_ids, [tribute_id]);
    authority.source_bindings = vec![source.binding().unwrap()];
    request.authority = authority.clone();
    request.sources = vec![source];
    let carrier = prepare(&secret, &request).unwrap();
    assert_eq!(carrier, prepare(&secret, &request).unwrap());
    let nods = open(&secret, &authority, &carrier).unwrap();
    assert_eq!(decrypt_nod(&secret, &nods[0]).unwrap(), U256::from(100));
    for cap in [U256::from(99), U256::from(101)] {
        let mut forged = request.clone();
        forged.authority.nod_gratis_load_max = cap;
        let forged_carrier = prepare(&secret, &forged).unwrap();
        assert!(open(&secret, &authority, &forged_carrier).is_err());
    }
    for cap in [U256::from(123), U256::from(124)] {
        let mut uncapped = request.clone();
        uncapped.authority.nod_gratis_load_max = cap;
        let carrier = prepare(&secret, &uncapped).unwrap();
        let nods = open(&secret, &uncapped.authority, &carrier).unwrap();
        assert_eq!(decrypt_nod(&secret, &nods[0]).unwrap(), U256::from(123));
    }
    let mut zero_cap = request.clone();
    zero_cap.authority.nod_gratis_load_max = U256::ZERO;
    assert!(prepare(&secret, &zero_cap).is_err());
    let mut old_authority = serde_json::to_value(&authority).unwrap();
    old_authority
        .as_object_mut()
        .unwrap()
        .remove("nod_gratis_load_max");
    assert!(serde_json::from_value::<NodMaterializationAuthorityV2>(old_authority).is_err());
    let mut stale = authority.clone();
    stale.source.checkpoint.state_root = B256::repeat_byte(99);
    assert!(open(&secret, &stale, &carrier).is_err());
    let mut substituted = request;
    let other_creator = PublicKey::from(&StaticSecret::from([12; 32])).to_bytes();
    substituted.sources[0].tribute =
        encrypt_tribute(&secret, &other_creator, context, &amounts).unwrap();
    assert!(prepare(&secret, &substituted).is_err());
    let mut tampered = carrier;
    tampered.encrypted_nods[0].0[20] ^= 1;
    assert!(open(&secret, &authority, &tampered).is_err());
}

fn native_source(
    tribute: outbe_primitives::tribute_encryption::EncryptedTributeV2,
) -> (TributeSourceSubject, TributeStateSource) {
    use alloy_primitives::keccak256;
    use outbe_ocomp_protocol::test_utils::{account_mpt_with_proofs, storage_trie};
    use outbe_primitives::{
        addresses::TRIBUTE_ADDRESS,
        storage::{
            hashmap::HashMapStorageProvider,
            proof::{AccountStorageProof, StorageSlotProof},
            StorageHandle,
        },
    };
    use outbe_tribute::{storage_records::StoredTribute, TributeContract};
    use std::collections::BTreeMap;

    let mut subject = TributeSourceSubject {
        chain_id: tribute.context.chain_id,
        worldwide_day: tribute.context.worldwide_day,
        tribute_count: 1,
        checkpoint: TributeSourceCheckpoint {
            block_number: 42,
            block_hash: B256::repeat_byte(0x42),
            state_root: B256::repeat_byte(0x43),
        },
    };
    let mut provider = HashMapStorageProvider::new(subject.chain_id);
    let mut expected_words = StorageHandle::enter(&mut provider, |storage| {
        let contract = TributeContract::new(storage);
        contract
            .insert_state_record(&StoredTribute::from(&tribute))
            .unwrap();
        outbe_tribute::state_proof::expected_record_words(&contract, &subject.into(), 0, &tribute)
            .unwrap()
    });
    expected_words.sort_by_key(|(slot, _)| *slot);
    let words = provider
        .storage
        .iter()
        .filter(|((address, _), _)| *address == TRIBUTE_ADDRESS)
        .map(|((_, slot), value)| (*slot, *value))
        .collect::<BTreeMap<_, _>>()
        .into_iter()
        .collect::<Vec<_>>();
    let (storage_root, nodes) = storage_trie(&words);
    let account = alloy_consensus::TrieAccount {
        nonce: 0,
        balance: U256::ZERO,
        storage_root,
        code_hash: keccak256([]),
    };
    let (root, account_nodes) = account_mpt_with_proofs(&[(TRIBUTE_ADDRESS, account)]);
    subject.checkpoint.state_root = root;
    let proofs = words
        .into_iter()
        .zip(nodes)
        .map(|((slot, value), nodes)| {
            let slot = B256::from(slot.to_be_bytes::<32>());
            (slot, StorageSlotProof { slot, value, nodes })
        })
        .collect::<BTreeMap<_, _>>();
    let proof = AccountStorageProof {
        address: TRIBUTE_ADDRESS,
        nonce: account.nonce,
        balance: account.balance,
        storage_root,
        code_hash: account.code_hash,
        account_nodes: account_nodes[&TRIBUTE_ADDRESS].clone(),
        slots: expected_words
            .iter()
            .map(|(slot, _)| proofs[slot].clone())
            .collect(),
    };
    (
        subject,
        TributeStateSource {
            tribute,
            day_index: 0,
            expected_words,
            proof,
        },
    )
}
