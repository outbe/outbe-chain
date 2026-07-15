use std::ops::{Deref, DerefMut};
use std::sync::Arc;

use alloy_primitives::{address, U256};
use alloy_sol_types::SolEvent;
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

    fn burn(&mut self, token_id: U256) -> PrecompileResult<()> {
        self.contract.burn(&self.reader, token_id)?;
        self.writer.delete(token_id).map_err(PrecompileError::from)
    }

    fn burn_all_by_wwd(&mut self, day: outbe_common::WorldwideDay) -> PrecompileResult<()> {
        let ids = self.contract.get_tribute_ids_by_day(&self.reader, day)?;
        self.contract.burn_all_by_wwd(&self.reader, day)?;
        for token_id in ids {
            self.writer
                .delete(token_id)
                .map_err(PrecompileError::from)?;
        }
        Ok(())
    }

    fn get_tribute(&self, token_id: U256) -> PrecompileResult<Option<TributeData>> {
        self.contract.get_tribute(&self.reader, token_id)
    }

    fn balance_of(&self, owner: alloy_primitives::Address) -> PrecompileResult<u64> {
        self.contract.balance_of(&self.reader, owner)
    }

    fn token_uri(&self, token_id: U256) -> PrecompileResult<String> {
        self.contract.token_uri(&self.reader, token_id)
    }

    fn get_tribute_ids_by_owner(
        &self,
        owner: alloy_primitives::Address,
    ) -> PrecompileResult<Vec<U256>> {
        self.contract.get_tribute_ids_by_owner(&self.reader, owner)
    }

    fn get_tribute_ids_by_day(
        &self,
        day: outbe_common::WorldwideDay,
    ) -> PrecompileResult<Vec<U256>> {
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
    TributeData {
        token_id: U256::from(1u64),
        owner: address!("0x1111111111111111111111111111111111111111"),
        worldwide_day: 20241220u32.into(),
        issuance_amount_minor: U256::from(1_000_000_000_000_000_000u128),
        issuance_currency: 840,
        nominal_amount_minor: U256::from(500_000_000_000_000_000u128),
        reference_currency: 840,
        exclude_from_intex_issuance: false,
        tribute_price_minor: U256::from(2_000_000_000_000_000_000u128),
    }
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
        t2.token_id = U256::from(2u64);
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

        tc.burn(tribute.token_id).unwrap();

        assert_eq!(tc.total_supply().unwrap(), 0);
        assert!(tc.get_tribute(tribute.token_id).unwrap().is_none());

        let totals = tc.get_day_totals(20241220u32.into()).unwrap();
        assert_eq!(totals.tribute_count, 0);
        assert_eq!(totals.tribute_nominal_amount, U256::ZERO);
    });
}

#[test]
fn test_burn_nonexistent_fails() {
    with_tribute(|tc| {
        let token_id = U256::from(0x99u64);
        assert!(tc.burn(token_id).is_err());
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
        t1.owner = alice;

        let mut t2 = sample_tribute();
        t2.token_id = U256::from(2u64);
        t2.owner = alice;

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        assert_eq!(tc.balance_of(alice).unwrap(), 2);

        tc.burn(t1.token_id).unwrap();
        assert_eq!(tc.balance_of(alice).unwrap(), 1);
    });
}

#[test]
fn test_token_uri_returns_metadata_json() {
    with_tribute(|tc| {
        let tribute = sample_tribute();
        open_sample_day(tc);
        tc.issue(&tribute).unwrap();

        let token_uri = tc.token_uri(tribute.token_id).unwrap();
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
        t1.owner = alice;

        let mut t2 = sample_tribute();
        t2.token_id = U256::from(2u64);
        t2.owner = alice;

        let mut t3 = sample_tribute();
        t3.token_id = U256::from(3u64);
        t3.owner = bob;

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        tc.issue(&t3).unwrap();

        let alice_ids = tc.get_tribute_ids_by_owner(alice).unwrap();
        assert_eq!(alice_ids, vec![t1.token_id, t2.token_id]);

        let bob_ids = tc.get_tribute_ids_by_owner(bob).unwrap();
        assert_eq!(bob_ids, vec![t3.token_id]);
    });
}

#[test]
fn test_get_tribute_ids_by_day() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        t2.token_id = U256::from(2u64);

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();

        let day_ids = tc.get_tribute_ids_by_day(20241220u32.into()).unwrap();
        assert_eq!(day_ids, vec![t1.token_id, t2.token_id]);
    });
}

#[test]
fn test_get_tributes_by_owner_sparse_after_burn() {
    with_tribute(|tc| {
        let alice = address!("0x1111111111111111111111111111111111111111");

        let mut t1 = sample_tribute();
        t1.owner = alice;

        let mut t2 = sample_tribute();
        t2.token_id = U256::from(2u64);
        t2.owner = alice;

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        tc.burn(t1.token_id).unwrap();

        let tributes = tc.get_tributes_by_owner(alice).unwrap();
        assert_eq!(tributes.len(), 1);
        assert_eq!(tributes[0].token_id, t2.token_id);
    });
}

#[test]
fn test_get_all_day_tributes_sparse_after_burn() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        t2.token_id = U256::from(2u64);

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();
        tc.burn(t1.token_id).unwrap();

        let tributes = tc.get_all_day_tributes(20241220u32.into()).unwrap();
        assert_eq!(tributes.len(), 1);
        assert_eq!(tributes[0].token_id, t2.token_id);
    });
}

#[test]
fn test_burn_all_by_wwd() {
    with_tribute(|tc| {
        let t1 = sample_tribute();
        let mut t2 = sample_tribute();
        t2.token_id = U256::from(2u64);

        open_sample_day(tc);
        tc.issue(&t1).unwrap();
        tc.issue(&t2).unwrap();

        tc.burn_all_by_wwd(20241220u32.into()).unwrap();

        assert_eq!(tc.total_supply().unwrap(), 0);
        assert!(tc.get_tribute(t1.token_id).unwrap().is_none());
        assert!(tc.get_tribute(t2.token_id).unwrap().is_none());
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
        let persisted = reader.get(tribute.token_id).unwrap().unwrap();
        assert_eq!(stored.tokenId, persisted.token_id);
        assert_eq!(stored.owner, persisted.owner);
        assert_eq!(stored.worldwideDay, u32::from(persisted.worldwide_day));
        assert_eq!(stored.issuanceAmountMinor, persisted.issuance_amount_minor);
        assert_eq!(stored.issuanceCurrency, persisted.issuance_currency);
        assert_eq!(stored.nominalAmountMinor, persisted.nominal_amount_minor);
        assert_eq!(stored.referenceCurrency, persisted.reference_currency);
        assert_eq!(stored.tributePriceMinor, persisted.tribute_price_minor);
        assert_eq!(
            stored.excludeFromIntexIssuance,
            persisted.exclude_from_intex_issuance
        );
        assert_eq!(
            events[2].topics()[0],
            crate::precompile::ITribute::TributeIssued::SIGNATURE_HASH
        );

        StorageHandle::enter(provider, |storage| {
            TributeContract::new(storage)
                .burn(reader, tribute.token_id)
                .unwrap();
        });
        let events = provider.get_events(TRIBUTE_ADDRESS);
        assert_eq!(events.len(), 5, "burn projection/product events expected");
        let deleted = crate::precompile::ITribute::TributeBodyDeleted::decode_log_data(&events[3])
            .expect("burn must emit identity-only deletion first");
        assert_eq!(deleted.tokenId, tribute.token_id);
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
            Err(PrecompileError::Revert("nested caller reverted".into()))
        });
        assert!(reverted.is_err());
        assert!(reader.get(tribute.token_id).unwrap().is_none());
    });
    assert!(provider.get_events(TRIBUTE_ADDRESS).is_empty());

    let mut oog_tribute = sample_tribute();
    oog_tribute.token_id = U256::from(2u64);
    StorageHandle::enter(&mut provider, |storage| {
        let out_of_gas: PrecompileResult<()> = storage.with_checkpoint(|| {
            TributeContract::new(storage.clone()).issue(&reader, &oog_tribute)?;
            Err(PrecompileError::OutOfGas)
        });
        assert!(matches!(out_of_gas, Err(PrecompileError::OutOfGas)));
        assert!(reader.get(oog_tribute.token_id).unwrap().is_none());
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
            .burn(&reader, tribute.token_id)
            .unwrap();
    });
    writer.delete(tribute.token_id).unwrap();
    provider.clear_events(TRIBUTE_ADDRESS);
    StorageHandle::enter(&mut provider, |storage| {
        assert!(TributeContract::new(storage)
            .burn(&reader, tribute.token_id)
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
