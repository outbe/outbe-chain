use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};

use outbe_primitives::dispatch::{dispatch_call, metadata, view};
use outbe_primitives::erc::{
    ERC165_INTERFACE_ID, ERC4906_INTERFACE_ID, ERC721_ENUMERABLE_INTERFACE_ID, ERC721_INTERFACE_ID,
    ERC721_METADATA_INTERFACE_ID,
};
use outbe_primitives::error::Result;

use crate::constants::{TOKEN_NAME, TOKEN_SYMBOL};
use crate::errors::CredisError;
use crate::schema::CredisContract;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

const SUPPORTED_INTERFACES: [[u8; 4]; 5] = [
    ERC165_INTERFACE_ID,
    ERC721_INTERFACE_ID,
    ERC721_ENUMERABLE_INTERFACE_ID,
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
                let record = contract.get_credis(c.credisId)?;
                let now = contract.storage.timestamp()?.to::<u64>();
                crate::metadata::token_uri(&record, now)
            }),
            totalSupply(c) => view(c, |_| Ok(U256::from(contract.total_credis()?))),
            getCredis(c) => view(c, |c| {
                let record = contract.get_credis(c.credisId)?;
                abi_credis(&record, contract.storage.timestamp()?.to::<u64>())
            }),
            ownerOf(c) => view(c, |c| {
                let record = contract.get_credis(c.credisId)?;
                Ok(record.owner)
            }),
            transferFrom(_)
            | safeTransferFrom_0(_)
            | safeTransferFrom_1(_)
            | approve(_)
            | setApprovalForAll(_) => Err(CredisError::NonTransferable.into()),
            getApproved(c) => view(c, |_| Ok(Address::ZERO)),
            isApprovedForAll(c) => view(c, |_| Ok(false)),
            credisExists(c) => view(c, |c| contract.credis_exists(c.credisId)),
            tokenByIndex(c) => view(c, |c| {
                let index = u64::try_from(c.index).map_err(|_| CredisError::IndexOutOfBounds)?;
                contract.token_by_index(index)
            }),
            balanceOf(c) => view(c, |c| Ok(U256::from(contract.credis_count_of(c.owner)?))),
            tokenOfOwnerByIndex(c) => view(c, |c| {
                let index = u32::try_from(c.index).map_err(|_| CredisError::IndexOutOfBounds)?;
                contract.token_of_owner_by_index(c.owner, index)
            }),
            interestAccruedMinor(c) => view(c, |c| {
                let record = contract.get_credis(c.credisId)?;
                let timestamp = contract.storage.timestamp()?.to::<u64>();
                CredisContract::accrued_interest(&record, timestamp)
            }),
            interestPaidMinor(c) => view(c, |c| {
                Ok(contract.get_credis(c.credisId)?.interest_paid_minor)
            }),
            credisPrincipalAndOutstandingOf(c) => view(c, |c| {
                let now = contract.storage.timestamp()?.to::<u64>();
                let (principal, outstanding) =
                    contract.principal_and_outstanding_of(c.owner, now)?;
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

fn abi_credis(p: &crate::schema::Credis, now: u64) -> Result<ICredis::Credis> {
    let outcome = crate::runtime::outcome(p, now)?;
    Ok(ICredis::Credis {
        credisId: p.credis_id,
        owner: p.owner,
        cca: p.cca,
        asset: p.asset,
        issuanceCurrency: p.issuance_currency,
        referenceCurrency: p.reference_currency,
        source: p.source,
        principalMinor: p.principal_minor,
        outstandingPrincipalMinor: outcome.outstanding_principal_minor,
        gratisMinor: p.gratis_minor,
        outstandingGratisMinor: outcome.outstanding_gratis_minor,
        policyRate: p.policy_rate,
        entryPriceMinor: p.entry_price_minor,
        issuedAt: p.issued_at,
        lastSettledAt: p.last_settled_at,
        state: crate::runtime::effective_state(p, now)? as u8,
        interestPaidMinor: p.interest_paid_minor,
        call: ICredis::CallTerms {
            callAnchorPriceMinor: p.call_anchor_price_minor,
            callPriceMinor: p.call_price_minor,
            callWindow: p.call_window_seconds,
            callThreshold: p.call_threshold_seconds,
            callNoticePeriod: p.call_notice_period_seconds,
            calledAt: p.called_at,
            settlementDeadline: if p.called_at == 0 {
                0
            } else {
                crate::runtime::settlement_deadline(p)
            },
        },
        outcome: ICredis::Outcome {
            principalPaidMinor: outcome.principal_paid_minor,
            principalWrittenOffMinor: outcome.principal_written_off_minor,
            gratisReturnedMinor: outcome.gratis_returned_minor,
            gratisBurnedMinor: outcome.gratis_burned_minor,
        },
    })
}
