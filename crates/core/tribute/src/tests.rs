use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use alloy_primitives::{address, U256};
use alloy_sol_types::SolEvent;
use outbe_compressed_entities::{decode_tribute_v1, derive_poseidon_entity_id, EntityId36};
use outbe_offchain_storage::{MemoryStorage, StorageReaderHandle, StorageWriterHandle};
use outbe_primitives::addresses::TRIBUTE_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result as PrecompileResult};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::{TributeContract, TributeData, TributeRepositoryReader, TributeRepositoryWriter};

struct TestTribute<'a> {
    contract: TributeContract<'a>,
    reader: TributeRepositoryReader,
    writer: TributeRepositoryWriter,
}

impl<'a> Deref for TestTribute<'a> {
    type Target = TributeContract<'a>;

    fn deref(&self) -> &Self::Target {
        &self.contract
    }
}

impl DerefMut for TestTribute<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.contract
    }
}

impl TestTribute<'_> {
    fn issue(&mut self, tribute: &TributeData) -> PrecompileResult<()> {
        self.contract.issue(&self.reader, tribute)?;
        self.writer.put(tribute).map_err(PrecompileError::from)
    }

    fn burn(&mut self, tribute_id: EntityId36) -> PrecompileResult<()> {
        self.contract.burn(&self.reader, tribute_id)?;
        self.writer
            .delete(tribute_id)
            .map_err(PrecompileError::from)
    }

    fn burn_all_by_wwd(&mut self, day: outbe_common::WorldwideDay) -> PrecompileResult<()> {
        let ids = self.contract.get_tribute_ids_by_day(&self.reader, day)?;
        self.contract.burn_all_by_wwd(&self.reader, day)?;
        for tribute_id in ids {
            self.writer
                .delete(tribute_id)
                .map_err(PrecompileError::from)?;
        }
        Ok(())
    }

    fn get_tribute(&self, tribute_id: EntityId36) -> PrecompileResult<Option<TributeData>> {
        self.contract.get_tribute(&self.reader, tribute_id)
    }

    fn balance_of(&self, owner: alloy_primitives::Address) -> PrecompileResult<u64> {
        self.contract.balance_of(&self.reader, owner)
    }

    fn token_uri(&self, tribute_id: EntityId36) -> PrecompileResult<String> {
        self.contract.token_uri(&self.reader, tribute_id)
    }

    fn get_tribute_ids_by_owner(
        &self,
        owner: alloy_primitives::Address,
    ) -> PrecompileResult<Vec<EntityId36>> {
        self.contract.get_tribute_ids_by_owner(&self.reader, owner)
    }

    fn get_tribute_ids_by_day(
        &self,
        day: outbe_common::WorldwideDay,
    ) -> PrecompileResult<Vec<EntityId36>> {
        self.contract.get_tribute_ids_by_day(&self.reader, day)
    }

    fn get_tributes_by_owner(
        &self,
        owner: alloy_primitives::Address,
    ) -> PrecompileResult<Vec<TributeData>> {
        self.contract.get_tributes_by_owner(&self.reader, owner)
    }

    fn get_all_day_tributes(
        &self,
        day: outbe_common::WorldwideDay,
    ) -> PrecompileResult<Vec<TributeData>> {
        self.contract.get_all_day_tributes(&self.reader, day)
    }
}

fn body_repository() -> (TributeRepositoryReader, TributeRepositoryWriter) {
    let storage = Arc::new(MemoryStorage::new());
    let reader: StorageReaderHandle = storage.clone();
    let writer: StorageWriterHandle = storage;
    (
        TributeRepositoryReader::new(reader.clone()),
        TributeRepositoryWriter::new(reader, writer),
    )
}

fn with_tribute<R>(f: impl FnOnce(&mut TestTribute<'_>) -> R) -> R {
    let mut storage = HashMapStorageProvider::new(1);
    let (reader, writer) = body_repository();
    StorageHandle::enter(&mut storage, |storage| {
        let mut tc = TestTribute {
            contract: TributeContract::new(storage.clone()),
            reader,
            writer,
        };
        f(&mut tc)
    })
}

fn with_provider<R>(
    f: impl FnOnce(&mut HashMapStorageProvider, &TributeRepositoryReader, &TributeRepositoryWriter) -> R,
) -> R {
    let mut storage = HashMapStorageProvider::new(1);
    let (reader, writer) = body_repository();
    f(&mut storage, &reader, &writer)
}

fn sample_tribute() -> TributeData {
    let worldwide_day = 20241220u32.into();
    let owner = address!("0x1111111111111111111111111111111111111111");
    TributeData {
        tribute_id: derive_poseidon_entity_id(owner, worldwide_day).unwrap(),
        owner,
        worldwide_day,
        issuance_amount_minor: U256::from(1_000_000_000_000_000_000u128),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(500_000_000_000_000_000u128),
        reference_currency: 840,
        exclude_from_intex_issuance: false,
        tribute_price_minor: U256::from(2_000_000_000_000_000_000u128),
    }
}

fn entity_id(seed: u64, day: outbe_common::WorldwideDay) -> EntityId36 {
    derive_poseidon_entity_id(alloy_primitives::Address::repeat_byte(seed as u8), day).unwrap()
}

fn set_owner(tribute: &mut TributeData, owner: alloy_primitives::Address) {
    tribute.owner = owner;
    tribute.tribute_id = derive_poseidon_entity_id(owner, tribute.worldwide_day).unwrap();
}

fn set_day(tribute: &mut TributeData, day: outbe_common::WorldwideDay) {
    tribute.worldwide_day = day;
    tribute.tribute_id = derive_poseidon_entity_id(tribute.owner, day).unwrap();
}

fn open_sample_day(tc: &mut TributeContract) {
    tc.unseal_day(20241220u32.into()).unwrap();
}

#[test]
fn test_initial_state() {
    with_tribute(|tc| {
        assert_eq!(tc.total_supply().unwrap(), 0);
        let totals = tc.get_day_totals(20241220u32.into()).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_amount, U256::ZERO);
        assert!(!totals.is_sealed);
    });
}

#[test]
fn test_issue_requires_initialized_unsealed_day() {
    with_tribute(|tc| {
        let tribute = sample_tribute();
        assert!(tc.issue(&tribute).is_err());

        open_sample_day(tc);
        tc.issue(&tribute).unwrap();
        assert_eq!(tc.total_supply().unwrap(), 1);
    });
}

#[test]
fn test_issue_duplicate_fails() {
    with_tribute(|tc| {
        let tribute = sample_tribute();
        open_sample_day(tc);
        tc.issue(&tribute).unwrap();
        assert!(tc.issue(&tribute).is_err());
    });
}

#[test]
fn test_day_bucket_tracks_nominal_and_gratis() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        set_owner(
            &mut t2,
            address!("0x2222222222222222222222222222222222222222"),
        );
        t2.nominal_amount_minor = U256::from(300_000_000_000_000_000u128);

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();

        let totals = tc.get_day_totals(20241220u32.into()).unwrap();
        assert_eq!(totals.tribute_count, 2);
        assert_eq!(
            totals.tribute_nominal_amount,
            t1.nominal_amount_minor + t2.nominal_amount_minor
        );
        assert!(!totals.is_sealed);
    });
}

#[test]
fn test_burn_tribute() {
    with_tribute(|tc| {
        let tribute = sample_tribute();
        open_sample_day(tc);
        tc.issue(&tribute).unwrap();

        tc.burn(tribute.tribute_id).unwrap();

        assert_eq!(tc.total_supply().unwrap(), 0);
        assert!(tc.get_tribute(tribute.tribute_id).unwrap().is_none());

        let totals = tc.get_day_totals(20241220u32.into()).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_amount, U256::ZERO);
    });
}

#[test]
fn test_burn_nonexistent_fails() {
    with_tribute(|tc| {
        let tribute_id = entity_id(0x99, 20241220u32.into());
        assert!(tc.burn(tribute_id).is_err());
    });
}

#[test]
fn test_seal_day() {
    with_tribute(|tc| {
        assert!(!tc.is_day_sealed(20241220u32.into()).unwrap());

        tc.seal_day(20241220u32.into()).unwrap();
        assert!(tc.is_day_sealed(20241220u32.into()).unwrap());

        let tribute = sample_tribute();
        assert!(tc.issue(&tribute).is_err());

        tc.unseal_day(20241220u32.into()).unwrap();
        assert!(!tc.is_day_sealed(20241220u32.into()).unwrap());
        tc.issue(&tribute).unwrap();
    });
}

#[test]
fn test_balance_of_tracks_live_owner_tributes() {
    with_tribute(|tc| {
        let alice = address!("0x1111111111111111111111111111111111111111");

        let mut t1 = sample_tribute();
        set_owner(&mut t1, alice);

        let mut t2 = sample_tribute();
        set_owner(&mut t2, alice);
        set_day(&mut t2, 20241221u32.into());

        open_sample_day(tc);
        tc.unseal_day(t2.worldwide_day).unwrap();
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        assert_eq!(tc.balance_of(alice).unwrap(), 2);

        tc.burn(t1.tribute_id).unwrap();
        assert_eq!(tc.balance_of(alice).unwrap(), 1);
    });
}

#[test]
fn test_token_uri_returns_metadata_json() {
    with_tribute(|tc| {
        let tribute = sample_tribute();
        open_sample_day(tc);
        tc.issue(&tribute).unwrap();

        let token_uri = tc.token_uri(tribute.tribute_id).unwrap();
        assert!(token_uri.starts_with("data:application/json;utf8,"));
        assert!(token_uri.contains("Outbe Tribute"));
        assert!(token_uri.contains("worldwide_day"));
        assert!(token_uri.contains("issuance_amount_minor"));
        assert!(token_uri.contains("\"trait_type\":\"reference_currency\""));
        assert!(token_uri.contains("\"trait_type\":\"exclude_from_intex_issuance\""));
    });
}

#[test]
fn test_get_tribute_ids_by_owner() {
    with_tribute(|tc| {
        let alice = address!("0x1111111111111111111111111111111111111111");
        let bob = address!("0x2222222222222222222222222222222222222222");

        let mut t1 = sample_tribute();
        set_owner(&mut t1, alice);

        let mut t2 = sample_tribute();
        set_owner(&mut t2, alice);
        set_day(&mut t2, 20241221u32.into());

        let mut t3 = sample_tribute();
        set_owner(&mut t3, bob);

        open_sample_day(tc);
        tc.unseal_day(t2.worldwide_day).unwrap();
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        tc.issue(&t3).unwrap();

        let alice_ids = tc.get_tribute_ids_by_owner(alice).unwrap();
        assert_eq!(alice_ids, vec![t1.tribute_id, t2.tribute_id]);

        let bob_ids = tc.get_tribute_ids_by_owner(bob).unwrap();
        assert_eq!(bob_ids, vec![t3.tribute_id]);
    });
}

#[test]
fn test_get_tribute_ids_by_day() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        set_owner(
            &mut t2,
            address!("0x2222222222222222222222222222222222222222"),
        );

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();

        let day_ids = tc.get_tribute_ids_by_day(20241220u32.into()).unwrap();
        let mut expected = vec![t1.tribute_id, t2.tribute_id];
        expected.sort_unstable();
        assert_eq!(day_ids, expected);
    });
}

#[test]
fn test_get_tributes_by_owner_sparse_after_burn() {
    with_tribute(|tc| {
        let alice = address!("0x1111111111111111111111111111111111111111");

        let mut t1 = sample_tribute();
        set_owner(&mut t1, alice);

        let mut t2 = sample_tribute();
        set_owner(&mut t2, alice);
        set_day(&mut t2, 20241221u32.into());

        open_sample_day(tc);
        tc.unseal_day(t2.worldwide_day).unwrap();
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        tc.burn(t1.tribute_id).unwrap();

        let tributes = tc.get_tributes_by_owner(alice).unwrap();
        assert_eq!(tributes.len(), 1);
        assert_eq!(tributes[0].tribute_id, t2.tribute_id);
    });
}

#[test]
fn test_get_all_day_tributes_sparse_after_burn() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        set_owner(
            &mut t2,
            address!("0x2222222222222222222222222222222222222222"),
        );

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        tc.burn(t1.tribute_id).unwrap();

        let tributes = tc.get_all_day_tributes(20241220u32.into()).unwrap();
        assert_eq!(tributes.len(), 1);
        assert_eq!(tributes[0].tribute_id, t2.tribute_id);
    });
}

#[test]
fn test_burn_all_by_wwd() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        set_owner(
            &mut t2,
            address!("0x2222222222222222222222222222222222222222"),
        );

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();

        tc.burn_all_by_wwd(20241220u32.into()).unwrap();

        assert_eq!(tc.total_supply().unwrap(), 0);
        assert!(tc.get_tribute(t1.tribute_id).unwrap().is_none());
        assert!(tc.get_tribute(t2.tribute_id).unwrap().is_none());
        assert!(tc
            .get_tribute_ids_by_day(20241220u32.into())
            .unwrap()
            .is_empty());

        let totals = tc.get_day_totals(20241220u32.into()).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_amount, U256::ZERO);
    });
}

#[test]
fn test_events_emitted_for_issue_and_burn() {
    with_provider(|provider, reader, writer| {
        let tribute = sample_tribute();
        StorageHandle::enter(provider, |storage| {
            let mut tc = TributeContract::new(storage.clone());
            tc.unseal_day(tribute.worldwide_day).unwrap();
            tc.issue(reader, &tribute).unwrap();
        });
        writer.put(&tribute).unwrap();

        let events = provider.get_events(TRIBUTE_ADDRESS).to_vec();
        assert_eq!(
            events.len(),
            3,
            "unseal + issue projection/product events expected"
        );
        let stored = crate::precompile::ITribute::TributeBodyStored::decode_log_data(&events[1])
            .expect("issue must emit a decodable full-body event first");
        let persisted = reader.get(tribute.tribute_id).unwrap().unwrap();
        assert_eq!(stored.tributeId.as_ref(), persisted.tribute_id.as_bytes());
        let event_body =
            crate::from_canonical_body(decode_tribute_v1(&stored.canonicalPayload).unwrap());
        assert_eq!(event_body.tribute_id, persisted.tribute_id);
        assert_eq!(event_body.owner, persisted.owner);
        assert_eq!(event_body.worldwide_day, persisted.worldwide_day);
        assert_eq!(
            event_body.issuance_amount_minor,
            persisted.issuance_amount_minor
        );
        assert_eq!(
            event_body.nominal_amount_minor,
            persisted.nominal_amount_minor
        );
        assert!(!stored.newCommitment.is_zero());
        assert_eq!(
            events[2].topics()[0],
            crate::precompile::ITribute::TributeIssued::SIGNATURE_HASH
        );

        StorageHandle::enter(provider, |storage| {
            TributeContract::new(storage)
                .burn(reader, tribute.tribute_id)
                .unwrap();
        });
        let events = provider.get_events(TRIBUTE_ADDRESS);
        assert_eq!(events.len(), 5, "burn projection/product events expected");
        let deleted = crate::precompile::ITribute::TributeBodyDeleted::decode_log_data(&events[3])
            .expect("burn must emit identity-only deletion first");
        assert_eq!(deleted.tributeId.as_ref(), tribute.tribute_id.as_bytes());
        assert_eq!(deleted.previousCommitment, stored.newCommitment);
        assert_eq!(
            events[4].topics()[0],
            crate::precompile::ITribute::TributeBurned::SIGNATURE_HASH
        );
    });
}

#[test]
fn failed_reverted_and_control_operations_leave_no_tribute_projection_event() {
    let mut provider = HashMapStorageProvider::new(1);
    let (reader, writer) = body_repository();
    let tribute = sample_tribute();
    StorageHandle::enter(&mut provider, |storage| {
        TributeContract::new(storage.clone())
            .unseal_day(tribute.worldwide_day)
            .unwrap();
    });
    provider.clear_events(TRIBUTE_ADDRESS);

    StorageHandle::enter(&mut provider, |storage| {
        let reverted: PrecompileResult<()> = storage.with_checkpoint(|| {
            TributeContract::new(storage.clone()).issue(&reader, &tribute)?;
            assert!(
                outbe_compressed_entities::CommitmentState::new(storage.clone())
                    .tribute(tribute.tribute_id)?
                    .is_some()
            );
            Err(PrecompileError::Revert("nested caller reverted".into()))
        });
        assert!(reverted.is_err());
        assert!(reader.get(tribute.tribute_id).unwrap().is_none());
        assert!(
            outbe_compressed_entities::CommitmentState::new(storage.clone())
                .tribute(tribute.tribute_id)
                .unwrap()
                .is_none()
        );
        let contract = TributeContract::new(storage);
        assert_eq!(contract.total_supply().unwrap(), 0);
        assert_eq!(
            contract
                .get_day_totals(tribute.worldwide_day)
                .unwrap()
                .tribute_count,
            0
        );
    });
    assert!(provider.get_events(TRIBUTE_ADDRESS).is_empty());

    let mut oog_tribute = sample_tribute();
    set_owner(
        &mut oog_tribute,
        address!("0x2222222222222222222222222222222222222222"),
    );
    StorageHandle::enter(&mut provider, |storage| {
        let out_of_gas: PrecompileResult<()> = storage.with_checkpoint(|| {
            TributeContract::new(storage.clone()).issue(&reader, &oog_tribute)?;
            assert!(
                outbe_compressed_entities::CommitmentState::new(storage.clone())
                    .tribute(oog_tribute.tribute_id)?
                    .is_some()
            );
            Err(PrecompileError::OutOfGas)
        });
        assert!(matches!(out_of_gas, Err(PrecompileError::OutOfGas)));
        assert!(reader.get(oog_tribute.tribute_id).unwrap().is_none());
        assert!(
            outbe_compressed_entities::CommitmentState::new(storage.clone())
                .tribute(oog_tribute.tribute_id)
                .unwrap()
                .is_none()
        );
        let contract = TributeContract::new(storage);
        assert_eq!(contract.total_supply().unwrap(), 0);
        assert_eq!(
            contract
                .get_day_totals(oog_tribute.worldwide_day)
                .unwrap()
                .tribute_count,
            0
        );
    });
    assert!(provider.get_events(TRIBUTE_ADDRESS).is_empty());

    StorageHandle::enter(&mut provider, |storage| {
        TributeContract::new(storage)
            .issue(&reader, &tribute)
            .unwrap();
    });
    writer.put(&tribute).unwrap();
    provider.clear_events(TRIBUTE_ADDRESS);
    StorageHandle::enter(&mut provider, |storage| {
        assert!(TributeContract::new(storage)
            .issue(&reader, &tribute)
            .is_err());
    });
    assert!(provider.get_events(TRIBUTE_ADDRESS).is_empty());

    StorageHandle::enter(&mut provider, |storage| {
        TributeContract::new(storage)
            .burn(&reader, tribute.tribute_id)
            .unwrap();
    });
    writer.delete(tribute.tribute_id).unwrap();
    provider.clear_events(TRIBUTE_ADDRESS);
    StorageHandle::enter(&mut provider, |storage| {
        assert!(TributeContract::new(storage)
            .burn(&reader, tribute.tribute_id)
            .is_err());
    });
    assert!(provider.get_events(TRIBUTE_ADDRESS).is_empty());

    StorageHandle::enter(&mut provider, |storage| {
        TributeContract::new(storage)
            .seal_day(tribute.worldwide_day)
            .unwrap();
    });
    let body_topics = [
        crate::precompile::ITribute::TributeBodyStored::SIGNATURE_HASH,
        crate::precompile::ITribute::TributeBodyDeleted::SIGNATURE_HASH,
    ];
    assert!(provider
        .get_events(TRIBUTE_ADDRESS)
        .iter()
        .all(|event| !body_topics.contains(&event.topics()[0])));
}

#[test]
fn failed_burn_transaction_restores_body_commitment_compact_state_and_events() {
    let mut provider = HashMapStorageProvider::new(1);
    let (reader, writer) = body_repository();
    let tribute = sample_tribute();
    StorageHandle::enter(&mut provider, |storage| {
        let mut contract = TributeContract::new(storage);
        contract.unseal_day(tribute.worldwide_day).unwrap();
        contract.issue(&reader, &tribute).unwrap();
    });
    writer.put(&tribute).unwrap();
    provider.clear_events(TRIBUTE_ADDRESS);

    for out_of_gas in [false, true] {
        StorageHandle::enter(&mut provider, |storage| {
            let before = outbe_compressed_entities::CommitmentState::new(storage.clone())
                .tribute(tribute.tribute_id)
                .unwrap()
                .unwrap();
            let failed: PrecompileResult<()> = storage.with_checkpoint(|| {
                TributeContract::new(storage.clone()).burn(&reader, tribute.tribute_id)?;
                assert!(
                    outbe_compressed_entities::CommitmentState::new(storage.clone())
                        .tribute(tribute.tribute_id)?
                        .is_none()
                );
                if out_of_gas {
                    Err(PrecompileError::OutOfGas)
                } else {
                    Err(PrecompileError::Revert("transaction reverted".into()))
                }
            });
            assert!(failed.is_err());
            assert_eq!(
                outbe_compressed_entities::CommitmentState::new(storage.clone())
                    .tribute(tribute.tribute_id)
                    .unwrap(),
                Some(before)
            );
            let contract = TributeContract::new(storage);
            assert_eq!(contract.total_supply().unwrap(), 1);
            assert_eq!(
                contract
                    .get_day_totals(tribute.worldwide_day)
                    .unwrap()
                    .tribute_count,
                1
            );
        });
        assert!(provider.get_events(TRIBUTE_ADDRESS).is_empty());
    }
}
