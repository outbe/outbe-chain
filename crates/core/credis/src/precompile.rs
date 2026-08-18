use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_primitives::dispatch::{dispatch_call, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;

use crate::schema::CredisContract;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

sol!("../../../contracts/precompiles/src/ICredis.sol");

pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    outbe_primitives::dispatch::reject_value(&value)?;
    dispatch_call(data, ICredis::ICredisCalls::abi_decode, |call| {
        let contract = CredisContract::new(storage.clone());
        use ICredis::ICredisCalls::*;
        match call {
            getPosition(c) => view(c, |c| {
                let position = contract.get_position(c.positionId)?;
                Ok(abi_position(&position))
            }),
            getPositionsByAddress(c) => view(c, |c| {
                let positions = contract.get_positions_by_address(c.smartAccount)?;
                Ok(positions.iter().map(abi_position).collect())
            }),
            getAllPositions(c) => view(c, |_| {
                let positions = contract.get_all_positions()?;
                Ok(positions.iter().map(abi_position).collect())
            }),
            hasCalledPosition(c) => view(c, |c| contract.has_called_position(c.smartAccount)),
            accruedInterest(c) => view(c, |c| {
                let position = contract.get_position(c.positionId)?;
                let timestamp = contract.storage.timestamp()?.to::<u64>();
                CredisContract::accrued_interest(&position, timestamp)
            }),
            credisOf(c) => view(c, |c| contract.get_principal_amount(c.smartAccount)),
            outstandingOf(c) => view(c, |c| contract.get_outstanding_amount(c.smartAccount)),
            supportsInterface(c) => view(c, |c| {
                let id: [u8; 4] = c.interfaceId.0;
                Ok(id == ERC165_INTERFACE_ID)
            }),
        }
    })
}

fn abi_position(p: &crate::schema::Position) -> ICredis::Position {
    ICredis::Position {
        positionId: p.position_id,
        smartAccount: p.smart_account,
        cca: p.cca,
        asset: p.asset,
        issuanceCurrency: p.issuance_currency,
        eoaCiphertext: p.eoa_ct.clone().into(),
        principal: p.principal,
        outstanding: p.outstanding,
        collateral: p.collateral,
        collateralLocked: p.collateral_locked,
        policyRate: p.policy_rate,
        entryPrice: p.entry_price,
        floorPrice: p.floor_price,
        callPrice: p.call_price,
        originatedAt: p.originated_at,
        lastSettledAt: p.last_settled_at,
        calledAt: p.called_at,
        state: p.state,
    }
}
