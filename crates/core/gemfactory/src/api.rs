use alloy_primitives::{Address, U256};
use outbe_intex::SeriesId;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_vaultrouter::settlement::SettlementPayment;

use crate::runtime;
use crate::schema::GemIssueParams;

pub fn issue_gem(storage: &StorageHandle<'_>, params: GemIssueParams) -> Result<U256> {
    runtime::issue_gem(storage, params)
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
    params: crate::schema::MerchantGemIssueParams,
) -> Result<U256> {
    runtime::issue_merchant_gem(storage, params)
}

pub fn settle_gem(
    storage: &StorageHandle<'_>,
    gem_id: U256,
    payment: SettlementPayment,
) -> Result<()> {
    runtime::settle_gem(storage, gem_id, payment)
}

pub fn mine_promis(
    storage: &StorageHandle<'_>,
    params: crate::schema::GemMiningProof,
    auth: outbe_promisfactory::api::ModifyAuth,
) -> Result<U256> {
    runtime::mine_promis(storage, params, auth)
}
