use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::error::Result;

use crate::errors::GemError;
use crate::schema::{GemContract, GemData};

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IGem.sol"
);

pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, IGem::IGemCalls::abi_decode, |call| {
        let gem = GemContract::new(storage.clone());
        use IGem::IGemCalls::*;
        match call {
            name(_) => metadata::<IGem::nameCall>(|| Ok(GemContract::name().to_string())),
            symbol(_) => metadata::<IGem::symbolCall>(|| Ok(GemContract::symbol().to_string())),
            totalSupply(_) => {
                metadata::<IGem::totalSupplyCall>(|| gem.total_supply().map(U256::from))
            }
            balanceOf(c) => view(c, |c| gem.balance_of(c.owner).map(U256::from)),
            ownerOf(c) => view(c, |c| gem.owner_of(c.gemId)),
            tokenURI(c) => view(c, |c| gem.token_uri(c.gemId)),
            tokenOfOwnerByIndex(c) => view(c, |c| {
                let idx = u32::try_from(c.index).map_err(|_| GemError::IndexOutOfBounds)?;
                gem.token_of_owner_by_index(c.owner, idx)
            }),
            getGemStatus(c) => view(c, |c| {
                let item = gem.get_gem(c.gemId)?.ok_or(GemError::GemNotFound)?;
                Ok(to_abi_data(&item))
            }),

            transferFrom(_) | safeTransferFrom(_) | approve(_) | setApprovalForAll(_) => {
                Err(GemError::NonTransferable.into())
            }

            getApproved(_) => view(IGem::getApprovedCall { gemId: U256::ZERO }, |_| {
                Ok(Address::ZERO)
            }),
            isApprovedForAll(_) => view(
                IGem::isApprovedForAllCall {
                    owner: Address::ZERO,
                    operator: Address::ZERO,
                },
                |_| Ok(false),
            ),
        }
    })
}

fn to_abi_data(item: &GemData) -> IGem::GemData {
    IGem::GemData {
        gemId: item.gem_id,
        owner: item.owner,
        gemType: item.gem_type,
        state: item.state,
        gemLoad: item.gem_load,
        entryPrice: item.entry_price,
        costAmount: item.cost_amount,
        floorPrice: item.floor_price,
        issuanceCurrency: item.issuance_currency,
        referenceCurrency: item.reference_currency,
        issuedAt: item.issued_at,
    }
}
