use super::*;
use outbe_primitives::tribute_encryption::{EncryptedTributeV2, TributeContextV2};

fn encrypted_body() -> EncryptedTributeV2 {
    let owner = Address::repeat_byte(0x33);
    let day = WorldwideDay::new(20250115);
    let immutable_blob = |len| {
        let mut blob = vec![0x77; len];
        blob[..8].copy_from_slice(&1u64.to_be_bytes());
        blob
    };
    EncryptedTributeV2 {
        context: TributeContextV2 {
            chain_id: 54322345,
            tribute_id: crate::derive_poseidon_entity_id(owner, day).unwrap(),
            owner,
            worldwide_day: day,
            issuance_currency: 840,
            reference_currency: 978,
            tribute_price_minor: U256::from(2_000_000),
            exclude_from_intex_issuance: false,
            offer_input_hash: B256::repeat_byte(0x44),
        },
        encrypted_creator_public_key: immutable_blob(56),
        encrypted_amounts: immutable_blob(88),
    }
}

#[test]
fn encrypted_tribute_survives_ce_reads_queries_and_event_replay_as_ciphertext() {
    let body = encrypted_body();
    let context = &body.context;
    let parent = MemoryParent::default();
    let scope = ExecutionScope::new();
    let mut provider = HashMapStorageProvider::new(context.chain_id);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::EncryptedTribute(&body)).unwrap();
        let current = read(
            storage.clone(),
            &scope,
            &parent,
            EntityRef::Tribute(context.tribute_id),
        )
        .unwrap()
        .unwrap();
        assert_eq!(current.payload().as_encrypted_tribute(), Some(&body));
        assert!(current.payload().as_tribute().is_none());
        assert_eq!(
            current.stored_body().schema_version(),
            crate::TRIBUTE_BODY_SCHEMA_V2
        );
        assert_eq!(
            crate::decode_stored_tribute_v2(&current.stored_body().encode()).unwrap(),
            body
        );
        for query in [
            QueryRef::TributeByOwner(context.owner),
            QueryRef::TributeByDay(context.worldwide_day),
        ] {
            let page = list(
                storage.clone(),
                &scope,
                &parent,
                query,
                IdPageRequest {
                    after: None,
                    limit: 4,
                },
            )
            .unwrap();
            assert_eq!(
                page.bodies()[0].payload().as_encrypted_tribute(),
                Some(&body)
            );
        }
        let payload = current.stored_body().payload().to_vec();
        let leaf = body_commitment(
            ACTIVE_COMMITMENT_SCHEME,
            crate::TRIBUTE_BODY_SCHEMA_V2,
            context.tribute_id,
            &payload,
        )
        .unwrap();
        let event = TributeBodyStored {
            tributeId: context.tribute_id.to_u256(),
            commitmentSchemeVersion: ACTIVE_COMMITMENT_SCHEME,
            schemaVersion: crate::TRIBUTE_BODY_SCHEMA_V2,
            previousCommitment: B256::ZERO,
            newCommitment: B256::from(*leaf.as_bytes()),
            canonicalPayload: payload.into(),
        }
        .encode_log_data();
        let replayed = crate::decode_canonical_body_event(TRIBUTE_ADDRESS, &event)
            .unwrap()
            .unwrap();
        assert_eq!(replayed.entity, EntityRef::Tribute(context.tribute_id));
        assert_eq!(replayed.next, Some(leaf));
        delete(storage.clone(), &scope, current).unwrap();
        assert!(read(
            storage,
            &scope,
            &parent,
            EntityRef::Tribute(context.tribute_id)
        )
        .unwrap()
        .is_none());
    });
}

#[test]
fn encrypted_schema_rejects_plain_bodies_malformed_blobs_and_noncanonical_bytes() {
    let body = encrypted_body();
    let payload = crate::encode_tribute_v2(&body).unwrap();
    let v1_envelope = StoredBody::new_v1(payload.clone()).unwrap().encode();
    assert!(crate::decode_stored_tribute_v2(&v1_envelope).is_err());
    for change in [
        (|body: &mut EncryptedTributeV2| body.encrypted_amounts.clear()) as fn(&mut _),
        |body| body.encrypted_creator_public_key[7] = 2,
        |body| body.context.worldwide_day = WorldwideDay::new(20250116),
    ] {
        let mut changed = body.clone();
        change(&mut changed);
        assert!(crate::encode_tribute_v2(&changed).is_err());
    }
    let mut duplicated = payload.clone();
    duplicated.extend_from_slice(&payload);
    assert!(crate::decode_tribute_v2(&duplicated).is_err());
}
