//! Entity reads through the module storage DSL at one fixed EVM checkpoint.

use super::*;
use alloy_primitives::{Address, B256, U256};
use outbe_primitives::{
    error::{PrecompileError, Result as StorageResult},
    storage::{
        readonly::{ReadOnlyStorageProvider, StorageReader},
        StorageHandle,
    },
};
use std::{cell::RefCell, collections::BTreeMap, rc::Rc};

struct RpcStateReader {
    url: String,
    block_hash: B256,
    slots: Rc<RefCell<BTreeMap<(Address, B256), U256>>>,
}
impl StorageReader for RpcStateReader {
    fn read_storage(&self, address: Address, key: B256) -> StorageResult<U256> {
        if let Some(value) = self.slots.borrow().get(&(address, key)) {
            return Ok(*value);
        }
        let value = eth::raw_json_result(
            &self.url,
            "eth_getStorageAt",
            serde_json::json!([
                address, key, {"blockHash": self.block_hash, "requireCanonical": true},
            ]),
        )
        .map_err(|error| PrecompileError::fatal(format!("read fixed EVM state: {error:#}")))?;
        let value = serde_json::from_value(value)
            .map_err(|error| PrecompileError::fatal(format!("decode EVM state word: {error}")))?;
        self.slots.borrow_mut().insert((address, key), value);
        Ok(value)
    }
}

impl Rpc {
    fn read_state_at<T>(
        &self,
        port: u16,
        checkpoint: FinalizedCheckpoint,
        read: impl FnOnce(StorageHandle<'_>) -> StorageResult<T>,
    ) -> Result<T> {
        ensure!(
            self.checkpoint_at(port, checkpoint.height)? == checkpoint,
            "EVM checkpoint differs on port {port}"
        );
        let chain_id = self
            .chain_id(port)
            .ok_or_else(|| eyre!("missing chain identity on port {port}"))?;
        let observed_slots = Rc::new(RefCell::new(BTreeMap::new()));
        let reader = RpcStateReader {
            url: self.url(port),
            block_hash: checkpoint.block_hash,
            slots: Rc::clone(&observed_slots),
        };
        let mut provider =
            ReadOnlyStorageProvider::new_with_chain_identity(reader, chain_id, B256::ZERO);
        let result = StorageHandle::enter(&mut provider, read).map_err(|error| {
            eyre!(
                "read entity state at {} on port {port}: {error}",
                checkpoint.height
            )
        })?;
        let slots = observed_slots.borrow();
        let mut accounts = BTreeMap::<Address, Vec<B256>>::new();
        for &(address, key) in slots.keys() {
            accounts.entry(address).or_default().push(key);
        }
        for (address, keys) in accounts {
            let value = eth::raw_json_result(
                &self.url(port),
                "eth_getProof",
                serde_json::json!([
                    address, keys, {"blockHash": checkpoint.block_hash, "requireCanonical": true},
                ]),
            )?;
            let proof = decode_account_proof(value)?;
            ensure!(
                proof.slots.iter().map(|slot| slot.slot).collect::<Vec<_>>() == keys,
                "native state proof opens a different slot set"
            );
            let verified = proof.verify(
                address,
                checkpoint.state_root,
                outbe_primitives::storage::proof::StateProofLimits {
                    max_slots: 512,
                    max_nodes: 16_384,
                    max_bytes: 4 * 1024 * 1024,
                },
            )?;
            for key in keys {
                ensure!(
                    verified.read_storage(address, key)? == slots[&(address, key)],
                    "native state word differs from its MPT proof"
                );
            }
        }
        Ok(result)
    }

    pub fn desis_limit_at(
        &self,
        port: u16,
        checkpoint: FinalizedCheckpoint,
        day: WorldwideDay,
    ) -> Result<U256> {
        self.read_state_at(port, checkpoint, |storage| {
            outbe_desis::schema::DesisContract::new(storage)
                .pending_desis_limit_minor
                .read(&day)
        })
    }

    pub fn tribute_state_at(
        &self,
        port: u16,
        checkpoint: FinalizedCheckpoint,
        id: outbe_primitives::wwd_entity_id::WwdEntityId,
    ) -> Result<Option<outbe_tribute::TributeRecord>> {
        self.read_state_at(port, checkpoint, |storage| {
            outbe_tribute::state_api::get_retained_record(&storage, id)
        })
    }

    pub fn tribute_indexes_at(
        &self,
        port: u16,
        checkpoint: FinalizedCheckpoint,
        owner: Address,
        day: outbe_primitives::time::WorldwideDay,
    ) -> Result<(
        Vec<outbe_primitives::wwd_entity_id::WwdEntityId>,
        Vec<outbe_primitives::wwd_entity_id::WwdEntityId>,
    )> {
        self.read_state_at(port, checkpoint, |storage| {
            let contract = outbe_tribute::TributeContract::new(storage);
            Ok((
                contract.owner_ids.list(&owner).read_all()?,
                contract.day_ids.list(&day).read_all()?,
            ))
        })
    }

    pub fn tribute_supply_at(&self, port: u16, checkpoint: FinalizedCheckpoint) -> Result<u64> {
        self.read_state_at(port, checkpoint, |storage| {
            outbe_tribute::TributeContract::new(storage)
                .total_supply
                .read()
        })
    }

    pub fn authenticated_tribute_at(
        &self,
        port: u16,
        checkpoint: FinalizedCheckpoint,
        id: outbe_primitives::wwd_entity_id::WwdEntityId,
    ) -> Result<outbe_primitives::tribute_encryption::EncryptedTributeV2> {
        use outbe_tee::tribute_source::{
            TributeSourceCheckpoint, TributeSourceSubject, TributeStateSource,
        };
        use outbe_tribute::state_proof::{TributeRecordStateProof, TributeStateProofSubject};
        let count = self.read_state_at(port, checkpoint, |storage| {
            outbe_tribute::TributeContract::new(storage)
                .day_ids
                .list(&id.worldwide_day())
                .len()
        })?;
        let subject = TributeSourceSubject {
            chain_id: self
                .chain_id(port)
                .ok_or_else(|| eyre!("missing chain ID"))?,
            worldwide_day: id.worldwide_day(),
            tribute_count: count,
            checkpoint: TributeSourceCheckpoint {
                block_number: checkpoint.height,
                block_hash: checkpoint.block_hash,
                state_root: checkpoint.state_root,
            },
        };
        let value = eth::raw_json_result(
            &self.url(port),
            "outbe_getTributeStateSource",
            serde_json::json!([subject, id]),
        )?;
        let source: TributeStateSource = serde_json::from_value(value)?;
        ensure!(
            source.tribute.context.tribute_id == id,
            "Tribute proof identity differs"
        );
        let proof = TributeRecordStateProof {
            day_index: source.day_index,
            tribute: source.tribute,
            account: source.proof,
        };
        proof.verify(
            &TributeStateProofSubject::from(subject),
            outbe_tee::tribute_source::proof_limits(),
        )?;
        Ok(proof.tribute)
    }

    pub fn nod_state_at(
        &self,
        port: u16,
        checkpoint: FinalizedCheckpoint,
        id: outbe_primitives::wwd_entity_id::WwdEntityId,
    ) -> Result<Option<(outbe_nod::NodItemState, outbe_nod::NodBucketState)>> {
        self.read_state_at(port, checkpoint, |storage| {
            let Some(item) = outbe_nod::state_api::load_item(&storage, id)? else {
                return Ok(None);
            };
            let bucket = outbe_nod::state_api::load_bucket(&storage, item.body().bucket_key)?
                .ok_or_else(|| PrecompileError::fatal("Nod bucket is absent"))?;
            Ok(Some((
                item.body().clone(),
                snapshot_nod_bucket(bucket.body()),
            )))
        })
    }

    pub fn nod_bucket_state_at(
        &self,
        port: u16,
        checkpoint: FinalizedCheckpoint,
        key: B256,
    ) -> Result<Option<outbe_nod::NodBucketState>> {
        self.read_state_at(port, checkpoint, |storage| {
            Ok(outbe_nod::state_api::load_bucket(&storage, key)?
                .map(|bucket| snapshot_nod_bucket(bucket.body())))
        })
    }

    pub fn gem_state_at(
        &self,
        port: u16,
        checkpoint: FinalizedCheckpoint,
        id: outbe_primitives::wwd_entity_id::WwdEntityId,
    ) -> Result<Option<(outbe_gem::GemItemState, outbe_gem::GemGroupState)>> {
        self.read_state_at(port, checkpoint, |storage| {
            let Some(item) = outbe_gem::state_api::load_item(&storage, id)? else {
                return Ok(None);
            };
            let group = outbe_gem::state_api::load_group(&storage, item.body().group_key)?
                .ok_or_else(|| PrecompileError::fatal("Gem group is absent"))?;
            Ok(Some((item.body().clone(), group.body().clone())))
        })
    }
}

fn snapshot_nod_bucket(body: &outbe_nod::NodBucketState) -> outbe_nod::NodBucketState {
    outbe_nod::NodBucketState {
        bucket_key: body.bucket_key,
        worldwide_day: body.worldwide_day,
        entry_price_minor: body.entry_price_minor,
        reference_currency: body.reference_currency,
        settled_nods: body.settled_nods,
    }
}

fn decode_account_proof(
    value: serde_json::Value,
) -> Result<outbe_primitives::storage::proof::AccountStorageProof> {
    use outbe_primitives::storage::proof::{AccountStorageProof, StorageSlotProof};
    let mut slots = value["storageProof"]
        .as_array()
        .ok_or_else(|| eyre!("missing native storage proof"))?
        .iter()
        .map(|slot| {
            let key: U256 = serde_json::from_value(slot["key"].clone())?;
            Ok(StorageSlotProof {
                slot: B256::from(key),
                value: serde_json::from_value(slot["value"].clone())?,
                nodes: serde_json::from_value(slot["proof"].clone())?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    slots.sort_by_key(|slot| slot.slot);
    Ok(AccountStorageProof {
        address: serde_json::from_value(value["address"].clone())?,
        nonce: u64::try_from(serde_json::from_value::<U256>(value["nonce"].clone())?)?,
        balance: serde_json::from_value(value["balance"].clone())?,
        code_hash: serde_json::from_value(value["codeHash"].clone())?,
        storage_root: serde_json::from_value(value["storageHash"].clone())?,
        account_nodes: serde_json::from_value(value["accountProof"].clone())?,
        slots,
    })
}
