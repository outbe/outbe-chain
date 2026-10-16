//! Cross-module NodFactory API.

use alloy_primitives::{Address, Bytes, U256};
use outbe_primitives::nod_encryption::EncryptedNodV2;
use outbe_primitives::wwd_entity_id::WwdEntityId;
use outbe_primitives::{error::Result, storage::StorageHandle};

use crate::runtime;

pub use crate::runtime::{issue_lysis_nod, MineGratisRequest, SettleNodRequest};

pub fn issue_nod(storage: &StorageHandle<'_>, encrypted: &EncryptedNodV2) -> Result<WwdEntityId> {
    runtime::issue_nod(storage, encrypted)
}

pub fn mine_gratis(storage: &StorageHandle<'_>, request: MineGratisRequest) -> Result<Bytes> {
    runtime::mine_gratis(storage, request)
}

/// What settling `nod_id` with `asset` costs, which of the Nod's two currencies
/// that asset settles on, and the VWAP snapshot an issuance-currency payment
/// must name.
pub fn quote_settlement(
    storage: &StorageHandle<'_>,
    nod_id: WwdEntityId,
    asset: Address,
) -> Result<(u16, U256, U256)> {
    runtime::quote_settlement(storage, nod_id, asset)
}

/// Pays a qualified or called Nod's cost in ERC20 base units.
pub fn settle_nod(storage: &StorageHandle<'_>, request: SettleNodRequest) -> Result<()> {
    runtime::settle_nod(storage, request)
}
