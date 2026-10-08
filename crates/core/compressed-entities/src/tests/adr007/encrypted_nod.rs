use super::*;
use outbe_primitives::nod_encryption::{EncryptedNodV2, NodTermsV2};

fn encrypted_body() -> crate::NodItemBodyV2 {
    let owner = Address::repeat_byte(0x33);
    let day = WorldwideDay::new(20261007);
    let mut blob = vec![0x77; 56];
    blob[..8].copy_from_slice(&1u64.to_be_bytes());
    crate::NodItemBodyV2 {
        encrypted: EncryptedNodV2 {
            terms: NodTermsV2 {
                chain_id: 7,
                nod_id: crate::derive_poseidon_entity_id(owner, day).unwrap(),
                owner,
                worldwide_day: day,
                league_id: 4,
                entry_price_minor: U256::from(17),
                issuance_currency: 840,
                reference_currency: 978,
            },
            encryption_binding: B256::repeat_byte(0x42),
            encrypted_creator_public_key: blob.clone(),
            encrypted_gratis_amount: blob,
        },
        bucket_key: B256::repeat_byte(0x63),
        issued_at: 101,
        is_settled: false,
    }
}

#[test]
fn encrypted_nod_preserves_queries_settlement_delete_and_canonical_replay() {
    let mut body = encrypted_body();
    let id = body.encrypted.terms.nod_id;
    let owner = body.encrypted.terms.owner;
    let parent = MemoryParent::default();
    let scope = ExecutionScope::default();
    let mut provider = HashMapStorageProvider::new(7);
    StorageHandle::enter(&mut provider, |storage| {
        begin_block(storage.clone(), &scope).unwrap();
        mint(storage.clone(), &scope, BodyInput::EncryptedNodItem(&body)).unwrap();
        let current = read(storage.clone(), &scope, &parent, EntityRef::NodItem(id))
            .unwrap()
            .unwrap();
        assert_eq!(current.payload().as_encrypted_nod_item(), Some(&body));
        assert!(current.payload().as_nod_item().is_none());
        assert_eq!(
            current.stored_body().schema_version(),
            crate::NOD_BODY_SCHEMA_V2
        );
        for query in [QueryRef::NodByOwner(owner), QueryRef::NodAll] {
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
                page.bodies()[0].payload().as_encrypted_nod_item(),
                Some(&body)
            );
        }
        body.is_settled = true;
        update(
            storage.clone(),
            &scope,
            current,
            BodyInput::EncryptedNodItem(&body),
        )
        .unwrap();
        let settled = read(storage.clone(), &scope, &parent, EntityRef::NodItem(id))
            .unwrap()
            .unwrap();
        assert_eq!(settled.payload().as_encrypted_nod_item(), Some(&body));
        delete(storage.clone(), &scope, settled).unwrap();
        assert!(
            read(storage.clone(), &scope, &parent, EntityRef::NodItem(id))
                .unwrap()
                .is_none()
        );
        for query in [QueryRef::NodByOwner(owner), QueryRef::NodAll] {
            assert!(list(
                storage.clone(),
                &scope,
                &parent,
                query,
                IdPageRequest {
                    after: None,
                    limit: 4
                }
            )
            .unwrap()
            .bodies()
            .is_empty());
        }
    });
    for log in provider.get_events(NOD_ADDRESS) {
        let decoded = crate::decode_canonical_body_event(NOD_ADDRESS, log).unwrap();
        assert!(decoded.is_some());
    }
}
