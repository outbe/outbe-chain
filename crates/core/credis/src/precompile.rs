use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::SolInterface;

use outbe_core_common::erc::{
    ERC165_INTERFACE_ID, ERC4906_INTERFACE_ID, ERC721_INTERFACE_ID, ERC721_METADATA_INTERFACE_ID,
};
use outbe_primitives::dispatch::{dispatch_call, metadata, view};
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

#[allow(clippy::too_many_arguments)]
mod abi {
    alloy_sol_types::sol!("../../../contracts/precompiles/src/ICredis.sol");
}
pub use abi::ICredis;

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
                let now = contract.storage.now_secs()?;
                crate::metadata::token_uri(&record, now)
            }),
            getCredis(c) => view(c, |c| {
                let record = contract.get_credis(c.credisId)?;
                abi_credis(&record, contract.storage.now_secs()?)
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
            balanceOf(c) => view(c, |c| Ok(U256::from(contract.credis_count_of(c.owner)?))),
            tokensByOwner(c) => view(c, |c| contract.tokens_by_owner(c.owner)),
            supportsInterface(c) => {
                view(c, |c| Ok(SUPPORTED_INTERFACES.contains(&c.interfaceId.0)))
            }
        }
    })
}

fn abi_credis(p: &crate::schema::CredisView, now: u64) -> Result<ICredis::CredisData> {
    let outcome = crate::runtime::outcome(p, now)?;
    Ok(ICredis::CredisData {
        credisId: p.credis_id,
        owner: p.owner,
        cca: p.cca,
        asset: p.asset,
        issuanceCurrency: p.issuance_currency,
        referenceCurrency: p.reference_currency,
        amountMinor: p.amount_minor,
        outstandingAmountMinor: outcome.outstanding_amount_minor,
        gratisMinor: p.gratis_minor,
        outstandingGratisMinor: outcome.outstanding_gratis_minor,
        policyRate: p.policy_rate,
        entryPriceMinor: p.entry_price_minor,
        issuedAt: p.issued_at,
        lastSettledAt: p.last_settled_at,
        state: crate::runtime::effective_state(p, now)? as u8,
        settledFeeMinor: p.settled_fee_minor,
        call: ICredis::CallTerms {
            callPriceMinor: p.call_price_minor,
            callWindow: p.terms.call_window_seconds,
            callThreshold: p.terms.call_threshold_seconds,
            callNoticePeriod: p.terms.call_notice_period_seconds,
            calledAt: p.called_at,
            settlementDeadline: if p.called_at == 0 {
                0
            } else {
                crate::runtime::settlement_deadline(p)
            },
        },
        outcome: ICredis::Outcome {
            settledAmountMinor: outcome.settled_amount_minor,
            forfeitedAmountMinor: outcome.forfeited_amount_minor,
            unpledgedGratisMinor: outcome.unpledged_gratis_minor,
            forfeitedGratisMinor: outcome.forfeited_gratis_minor,
        },
        seriesId: p.series_id.into(),
        subSeries: p.sub_series,
    })
}
