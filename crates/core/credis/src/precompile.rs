use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_primitives::dispatch::{dispatch_call, view};
use outbe_primitives::erc::ERC165_INTERFACE_ID;
use outbe_primitives::error::Result;

use crate::schema::CredisContract;

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
                let positions = contract.get_positions_by_address(c.bundleAccount)?;
                Ok(positions.iter().map(abi_position).collect())
            }),
            getAllPositions(c) => view(c, |_| {
                let positions = contract.get_all_positions()?;
                Ok(positions.iter().map(abi_position).collect())
            }),
            hasOverdueAnadosis(c) => view(c, |c| {
                let timestamp = contract.storage.timestamp()?.to::<u64>();
                contract.has_overdue_anadosis(c.bundleAccount, timestamp)
            }),
            getNextAnadosis(c) => view(c, |c| {
                let anadosis = contract.get_next_anadosis(c.positionId)?.ok_or_else(|| {
                    outbe_primitives::error::PrecompileError::Revert(
                        "position already completed".into(),
                    )
                })?;
                Ok(abi_anadosis(&anadosis))
            }),
            getPositionAnadosis(c) => view(c, |c| {
                let records = contract.get_position_anadosis(c.positionId)?;
                Ok(records.iter().map(abi_anadosis).collect())
            }),
            credisOf(c) => view(c, |c| {
                let mut total = U256::ZERO;
                for position in contract.get_positions_by_address(c.bundleAccount)? {
                    total = total
                        .checked_add(position.total_anadosis_amount)
                        .ok_or_else(|| {
                            outbe_primitives::error::PrecompileError::Revert(
                                "credis total sum overflow".into(),
                            )
                        })?;
                }
                Ok(total)
            }),
            outstandingAnadosisOf(c) => {
                view(c, |c| contract.get_outstanding_amount(c.bundleAccount))
            }
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
        vaultProvider: p.vault_provider,
        asset: p.asset,
        bundleAccount: p.bundle_account,
        totalAnadosisAmount: p.total_anadosis_amount,
        outstandingAnadosisAmount: p.outstanding_anadosis_amount,
        totalGratisAmount: p.total_gratis_amount,
        outstandingGratisAmount: p.outstanding_gratis_amount,
        nextAnadosisNumber: p.next_anadosis_number,
        createdAt: p.created_at,
    }
}

fn abi_anadosis(a: &crate::schema::Anadosis) -> ICredis::Anadosis {
    ICredis::Anadosis {
        anadosisNumber: a.anadosis_number,
        dueDate: a.due_date,
        paidAt: a.paid_at,
        anadosisAmount: a.anadosis_amount,
        gratisAmount: a.gratis_amount,
    }
}
