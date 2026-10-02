use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::erc::{
    ERC165_INTERFACE_ID, ERC4906_INTERFACE_ID, ERC721_INTERFACE_ID, ERC721_METADATA_INTERFACE_ID,
};
use outbe_primitives::error::Result;

use crate::constants::{TOKEN_NAME, TOKEN_SYMBOL};
use crate::errors::CredisError;
use crate::schema::CredisContract;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

const SUPPORTED_INTERFACES: [[u8; 4]; 4] = [
    ERC165_INTERFACE_ID,
    ERC721_INTERFACE_ID,
    ERC721_METADATA_INTERFACE_ID,
    ERC4906_INTERFACE_ID,
];

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
            name(_) => metadata::<ICredis::nameCall>(|| Ok(TOKEN_NAME.to_string())),
            symbol(_) => metadata::<ICredis::symbolCall>(|| Ok(TOKEN_SYMBOL.to_string())),
            tokenURI(c) => view(c, |c| {
                let position = contract.get_position(c.positionId)?;
                let now = contract.storage.timestamp()?.to::<u64>();
                crate::metadata::token_uri(&position, now)
            }),
            totalSupply(c) => view(c, |_| Ok(U256::from(contract.total_positions()?))),
            getPosition(c) => view(c, |c| {
                let position = contract.get_position(c.positionId)?;
                Ok(abi_position(&position))
            }),
            ownerOf(c) => view(c, |c| {
                let position = contract.get_position(c.positionId)?;
                Ok(position.smart_account)
            }),
            transferFrom(_)
            | safeTransferFrom_0(_)
            | safeTransferFrom_1(_)
            | approve(_)
            | setApprovalForAll(_) => Err(CredisError::NonTransferable.into()),
            getApproved(c) => view(c, |_| Ok(Address::ZERO)),
            isApprovedForAll(c) => view(c, |_| Ok(false)),
            positionByIndex(c) => view(c, |c| {
                let index = u64::try_from(c.index).map_err(|_| CredisError::IndexOutOfBounds)?;
                Ok(abi_position(&contract.position_at(index)?))
            }),
            balanceOf(c) => view(c, |c| {
                Ok(U256::from(contract.position_count_of(c.smartAccount)?))
            }),
            positionOfAddressByIndex(c) => view(c, |c| {
                let index = u32::try_from(c.index).map_err(|_| CredisError::IndexOutOfBounds)?;
                let position = contract.position_of_address_at(c.smartAccount, index)?;
                Ok(abi_position(&position))
            }),
            hasCalledPosition(c) => view(c, |c| contract.has_called_position(c.smartAccount)),
            interestAccruedMinor(c) => view(c, |c| {
                let position = contract.get_position(c.positionId)?;
                let timestamp = contract.storage.timestamp()?.to::<u64>();
                CredisContract::accrued_interest(&position, timestamp)
            }),
            interestPaidMinor(c) => view(c, |c| {
                Ok(contract.get_position(c.positionId)?.interest_paid_minor)
            }),
            credisPrincipalAndOutstandingOf(c) => view(c, |c| {
                let (principal, outstanding) =
                    contract.principal_and_outstanding_of(c.smartAccount)?;
                Ok(ICredis::credisPrincipalAndOutstandingOfReturn {
                    principalMinor: principal,
                    outstandingPrincipalMinor: outstanding,
                })
            }),
            supportsInterface(c) => {
                view(c, |c| Ok(SUPPORTED_INTERFACES.contains(&c.interfaceId.0)))
            }
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
        referenceCurrency: p.reference_currency,
        eoaCiphertext: p.eoa_ct.clone().into(),
        principalMinor: p.principal_minor,
        outstandingPrincipalMinor: p.outstanding_principal_minor,
        gratisMinor: p.gratis_minor,
        outstandingGratisMinor: p.outstanding_gratis_minor,
        policyRate: p.policy_rate,
        entryPriceMinor: p.entry_price_minor,
        callPriceMinor: p.call_price_minor,
        issuedAt: p.issued_at,
        lastSettledAt: p.last_settled_at,
        calledAt: p.called_at,
        state: p.state,
        callAnchorPriceMinor: p.call_anchor_price_minor,
        interestPaidMinor: p.interest_paid_minor,
        settlementDeadline: if p.called_at == 0 {
            0
        } else {
            crate::runtime::settlement_deadline(p)
        },
        callNoticePeriod: p.call_notice_period_seconds,
        callWindow: p.call_window_seconds,
        callThreshold: p.call_threshold_seconds,
    }
}
