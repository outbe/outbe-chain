use super::*;

use crate::transport::wire::evm_chain_of;
use alloy_sol_types::{sol, SolValue};
use outbe_core_common::addresses::{
    INTEX_AUCTION_TARGET_ROUTER_ADDRESS, WCOEN_ADDRESS, WCOEN_BRIDGE_ADDRESS,
};
use outbe_promislimit::{BurnSource, PromisLimitContract};

sol! {
    function transferFrom(address src, address dst, uint256 wad) external returns (bool);
}

/// What the WCOEN token bridge delivers with `onCrosschainTokensReceived`.
pub struct CrosschainProceeds<'a> {
    pub src_chain_id: u32,
    pub from: &'a [u8],
    pub amount: U256,
    pub extra_data: &'a [u8],
}

/// The proceeds of one page of winners from IntexAuctionTargetRouter of `src_chain_id`, which the WCOEN token
/// bridge credited to the factory. They burn with code 43.
pub fn receive_crosschain_proceeds(
    storage: &StorageHandle<'_>,
    caller: Address,
    proceeds: CrosschainProceeds<'_>,
) -> Result<()> {
    let CrosschainProceeds {
        src_chain_id,
        from,
        amount,
        extra_data,
    } = proceeds;
    if caller != WCOEN_BRIDGE_ADDRESS {
        return Err(IntexFactoryError::UnauthorizedProceedsCaller(caller).into());
    }
    let (emission_day, page_index) = <(u32, u32)>::abi_decode_params(extra_data)
        .map_err(|_| IntexFactoryError::UndecodableProceedsData)?;
    require_day_target(storage, emission_day, src_chain_id)?;
    if evm_chain_of(from, INTEX_AUCTION_TARGET_ROUTER_ADDRESS) != Some(src_chain_id) {
        return Err(IntexFactoryError::UnauthorizedProceedsSender(from.to_vec().into()).into());
    }
    burn_intex_auction_proceeds(storage, emission_day, page_index, src_chain_id, amount)
}

/// The proceeds of one page of winners that IntexAuctionTargetRouter on Outbe approved to the factory.
pub fn receive_local_proceeds(
    storage: &StorageHandle<'_>,
    caller: Address,
    emission_day: u32,
    page_index: u32,
    amount: U256,
) -> Result<()> {
    if caller != INTEX_AUCTION_TARGET_ROUTER_ADDRESS {
        return Err(IntexFactoryError::UnauthorizedProceedsCaller(caller).into());
    }
    let local =
        u32::try_from(storage.chain_id()?).map_err(|_| IntexFactoryError::ChainIdExceedsU32)?;
    require_day_target(storage, emission_day, local)?;
    let returned = storage.call(
        WCOEN_ADDRESS,
        U256::ZERO,
        transferFromCall {
            src: caller,
            dst: INTEX_FACTORY_ADDRESS,
            wad: amount,
        }
        .abi_encode()
        .into(),
    )?;
    if transferFromCall::abi_decode_returns(&returned) != Ok(true) {
        return Err(IntexFactoryError::ProceedsTransferRefused.into());
    }
    burn_intex_auction_proceeds(storage, emission_day, page_index, local, amount)
}

fn require_day_target(storage: &StorageHandle<'_>, emission_day: u32, chain_id: u32) -> Result<()> {
    if !crate::targets::day_targets(storage, EmissionDate::new(emission_day))?.contains(&chain_id) {
        return Err(IntexFactoryError::UnexpectedProceedsSource(chain_id).into());
    }
    Ok(())
}

fn burn_intex_auction_proceeds(
    storage: &StorageHandle<'_>,
    emission_day: u32,
    page_index: u32,
    src_chain_id: u32,
    amount: U256,
) -> Result<()> {
    if amount.is_zero() {
        return Err(IntexFactoryError::ZeroAmount.into());
    }
    PromisLimitContract::new(storage.clone())
        .burn_wcoen(BurnSource::IntexAuctionProceeds, amount)?;
    emit_event(
        storage,
        crate::precompile::IIntexFactory::IntexAuctionProceedsBurned {
            emissionDay: emission_day,
            srcChainId: src_chain_id,
            pageIndex: page_index,
            amountMinor: amount,
        },
    )
}
