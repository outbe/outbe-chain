use alloy_primitives::{Address, B256, U256};
use outbe_compressed_entities::{
    encode_tribute_v2, test_support::single_collection_body_proof, CeDomain, StoredBody,
    TRIBUTE_BODY_SCHEMA_V2,
};
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
use outbe_tee::nod_materialization::{
    NodMaterializationAuthorityV2, NodSourceV2, PrepareEncryptedNodsRequestV2,
};
use outbe_tee_enclave::{
    nod_encryption::decrypt_nod,
    nod_materialization::{open, prepare},
    tribute_encryption::encrypt_tribute,
};
use x25519_dalek::{PublicKey, StaticSecret};

#[test]
fn materialization_binds_current_authority_and_exact_authenticated_source() {
    let secret = [7; 32];
    let creator = PublicKey::from(&StaticSecret::from([11; 32])).to_bytes();
    let day = WorldwideDay::new(20250115);
    let owner = Address::repeat_byte(3);
    let tribute_id = WwdEntityId::from_day_and_digest(day, B256::repeat_byte(4));
    let context = TributeContextV2 {
        chain_id: 1,
        tribute_id,
        owner,
        worldwide_day: day,
        issuance_currency: 840,
        reference_currency: 978,
        tribute_price_minor: U256::ONE,
        exclude_from_intex_issuance: false,
        offer_input_hash: B256::repeat_byte(5),
    };
    let amounts = TributeAmountsV2 {
        issuance_amount_minor: U256::from(1000),
        nominal_amount_minor: U256::from(900),
    };
    let tribute = encrypt_tribute(&secret, &creator, context.clone(), &amounts).unwrap();
    let source_body = StoredBody::new(TRIBUTE_BODY_SCHEMA_V2, encode_tribute_v2(&tribute).unwrap())
        .unwrap()
        .encode();
    let (source_root, source_proof) =
        single_collection_body_proof(CeDomain::Tribute, tribute_id, &source_body).unwrap();
    let nod_id = WwdEntityId::from_day_and_digest(day, B256::repeat_byte(6));
    let action = NodActionV1 {
        raw_ordinal: 0,
        tribute_id: B256::from_slice(tribute_id.as_slice()),
        nod_id: B256::from_slice(nod_id.as_slice()),
        owner,
        wwd: 20250115,
        league_id: 0,
        gratis_load_minor: U256::from(123),
        entry_price_minor: U256::from(700),
        settlement_cost_minor: U256::from(123),
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
    let authority = NodMaterializationAuthorityV2 {
        chain_id: 1,
        head: head.encode_canonical(&limits).unwrap(),
        subtree_height: 3,
        sealed_tribute_root: source_root,
    };
    let request = PrepareEncryptedNodsRequestV2 {
        authority: authority.clone(),
        batch: batch.encode_canonical(&limits).unwrap(),
        sources: vec![NodSourceV2 {
            tribute,
            proof: postcard::to_allocvec(&source_proof).unwrap(),
        }],
    };
    let carrier = prepare(&secret, &request).unwrap();
    assert_eq!(carrier, prepare(&secret, &request).unwrap());
    let nods = open(&secret, &authority, &carrier).unwrap();
    assert_eq!(decrypt_nod(&secret, &nods[0]).unwrap(), U256::from(123));
    let mut stale = authority.clone();
    stale.sealed_tribute_root = B256::repeat_byte(99);
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
