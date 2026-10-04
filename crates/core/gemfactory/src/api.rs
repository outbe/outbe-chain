use alloy_primitives::{Address, U256};
use outbe_intex::SeriesId;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::runtime;
use crate::schema::GemTypes;

pub fn issue_gem(
    storage: &StorageHandle<'_>,
    owner: Address,
    gem_type: GemTypes,
    promis_load: U256,
    issuance_currency: u16,
    reference_currency: u16,
    entry_price: U256,
) -> Result<U256> {
    // Classification is fixed by this call. Rewards is the production Genesis issuer.
    // A zero creation count is not "never": days retained before this slot existed
    // still read as zero. Membership of the bounded aggregate closes that case.
    // The count stays set after those days leave the aggregate.
    let issued_before_first_wwd = gem_type == GemTypes::Genesis
        && !outbe_metadosis::api::has_created_worldwide_day(storage.clone())?;
    // Gem creation and the privilege bit commit together. Every Genesis issue
    // records 1 or 0: burn does not clear the map, and the id is only owner,
    // load, and block, so skipping 0 would keep a stale 1. A failed bit write
    // rolls the gem back for a direct caller; Rewards also has its own checkpoint.
    storage.clone().with_checkpoint(|| {
        let gem_id = runtime::issue_gem(
            storage,
            owner,
            gem_type,
            promis_load,
            issuance_currency,
            reference_currency,
            entry_price,
        )?;
        if gem_type == GemTypes::Genesis {
            outbe_gem::api::record_genesis_issuance_privilege(
                storage,
                gem_id,
                issued_before_first_wwd,
            )?;
        }
        Ok(gem_id)
    })
}

pub fn issue_gem_position(
    storage: &StorageHandle<'_>,
    caller: Address,
    source_intex_id: SeriesId,
    units: U256,
) -> Result<U256> {
    runtime::issue_gem_position(storage, caller, source_intex_id, units)
}

pub fn issue_merchant_gem(
    storage: &StorageHandle<'_>,
    caller: Address,
    position_id: U256,
    owner: Address,
    promis_load: U256,
) -> Result<U256> {
    runtime::issue_merchant_gem(storage, caller, position_id, owner, promis_load)
}

pub fn settle_gem(
    storage: &StorageHandle<'_>,
    caller: Address,
    gem_id: U256,
    asset: Address,
    snapshot_id: U256,
) -> Result<()> {
    runtime::settle_gem(storage, caller, gem_id, asset, snapshot_id)
}

pub fn settle_gem_with_paynote(
    storage: &StorageHandle<'_>,
    caller: Address,
    gem_id: U256,
    paynote_proof: &[u8],
) -> Result<()> {
    runtime::settle_gem_with_paynote(storage, caller, gem_id, paynote_proof)
}

pub fn mine_promis(
    storage: &StorageHandle<'_>,
    gem_id: U256,
    nonce: u64,
    auth: outbe_promisfactory::api::ModifyAuth,
) -> Result<U256> {
    runtime::mine_promis(storage, gem_id, nonce, auth)
}
