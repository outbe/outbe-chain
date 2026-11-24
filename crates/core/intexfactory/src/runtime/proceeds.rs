use super::*;

use alloy_sol_types::{sol, SolValue};
use outbe_core_common::addresses::{TARGET_ROUTER_ADDRESS, WCOEN_ADDRESS, WCOEN_BRIDGE_ADDRESS};
use outbe_intex::wire::evm_chain_of;
use outbe_promislimit::{BurnSource, PromisLimitContract};

sol! {
    function transferFrom(address src, address dst, uint256 wad) external returns (bool);
}

/// The proceeds of one page of winners from TargetRouter of `source_domain`, which the WCOEN token
/// bridge credited to the factory. They burn with code 43.
pub fn receive_crosschain_proceeds(
    storage: &StorageHandle<'_>,
    caller: Address,
    source_domain: u32,
    from: &[u8],
    amount: U256,
    extra_data: &[u8],
) -> Result<()> {
    if caller != WCOEN_BRIDGE_ADDRESS {
        return Err(IntexFactoryError::UnauthorizedProceedsCaller(caller).into());
    }
    let (emission_day, page_index) = <(u32, u32)>::abi_decode_params(extra_data)
        .map_err(|_| IntexFactoryError::UnexpectedProceedsSource(source_domain))?;
    require_day_target(storage, emission_day, source_domain)?;
    if evm_chain_of(from, TARGET_ROUTER_ADDRESS) != Some(source_domain) {
        return Err(IntexFactoryError::UnauthorizedProceedsSender(from.to_vec().into()).into());
    }
    burn_wcoen_proceeds(storage, emission_day, page_index, source_domain, amount)
}

/// The proceeds of one page of winners that TargetRouter on Outbe approved to the factory.
pub fn receive_local_proceeds(
    storage: &StorageHandle<'_>,
    caller: Address,
    emission_day: u32,
    page_index: u32,
    amount: U256,
) -> Result<()> {
    if caller != TARGET_ROUTER_ADDRESS {
        return Err(IntexFactoryError::UnauthorizedProceedsCaller(caller).into());
    }
    let local = u32::try_from(storage.chain_id()?)
        .map_err(|_| IntexFactoryError::UnexpectedProceedsSource(u32::MAX))?;
    require_day_target(storage, emission_day, local)?;
    storage.call(
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
    burn_wcoen_proceeds(storage, emission_day, page_index, local, amount)
}

fn require_day_target(storage: &StorageHandle<'_>, emission_day: u32, chain_id: u32) -> Result<()> {
    if !crate::targets::day_targets(storage, EmissionDate::new(emission_day))?.contains(&chain_id) {
        return Err(IntexFactoryError::UnexpectedProceedsSource(chain_id).into());
    }
    Ok(())
}

fn burn_wcoen_proceeds(
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
