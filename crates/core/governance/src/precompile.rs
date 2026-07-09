use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{
    dispatch_call, metadata, mutate, mutate_void, reject_value, view,
};
use outbe_primitives::error::Result;

use crate::errors::GovernanceError;
use crate::schema::{Gip, GovernanceContract, Oip};

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/IGovernance.sol"
);

fn oip_to_sol(o: Oip) -> IGovernance::Oip {
    IGovernance::Oip {
        id: o.id,
        status: o.status,
        author: o.author,
        createdBlock: o.created_block,
        updatedBlock: o.updated_block,
        textHash: o.text_hash,
        text: o.text,
    }
}

fn gip_to_sol(g: Gip) -> IGovernance::Gip {
    IGovernance::Gip {
        id: g.id,
        status: g.status,
        author: g.author,
        createdBlock: g.created_block,
        updatedBlock: g.updated_block,
        textHash: g.text_hash,
        text: g.text,
    }
}

/// Resolves a diff `base` selector to the current base text.
/// `0 = canon`, `1 = meta-canon`.
fn diff_base_text(gov: &GovernanceContract, base: u8) -> Result<String> {
    match base {
        0 => Ok(gov.get_canon()?.0),
        1 => Ok(gov.get_meta_canon()?.0),
        _ => Err(GovernanceError::InvalidDiffBase.into()),
    }
}

pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    dispatch_call(data, IGovernance::IGovernanceCalls::abi_decode, |call| {
        let mut gov = GovernanceContract::new(storage);
        use IGovernance::IGovernanceCalls::*;
        match call {
            // --- canon / meta-canon reads ---
            getMetaCanon(_) => {
                metadata::<IGovernance::getMetaCanonCall>(|| Ok(gov.get_meta_canon()?.into()))
            }
            getCanon(_) => metadata::<IGovernance::getCanonCall>(|| Ok(gov.get_canon()?.into())),
            getMetaCanonRevisionHash(c) => view(c, |c| gov.meta_canon_revision_hash(c.version)),
            getCanonRevisionHash(c) => view(c, |c| gov.canon_revision_hash(c.version)),

            // --- canon / meta-canon writes ---
            updateMetaCanon(c) => mutate(c, caller, |sender, c| {
                gov.update_meta_canon(sender, &c.text)
            }),
            updateCanon(c) => mutate(c, caller, |sender, c| gov.update_canon(sender, &c.text)),

            // --- OIP ---
            submitOip(c) => mutate(c, caller, |sender, c| gov.submit_oip(sender, &c.text)),
            getOip(c) => view(c, |c| {
                let o = gov
                    .get_oip(c.id)?
                    .ok_or(GovernanceError::ProposalNotFound)?;
                Ok(oip_to_sol(o))
            }),
            updateOipText(c) => mutate_void(c, caller, |sender, c| {
                gov.update_oip_text(sender, c.id, &c.text)
            }),
            setOipStatus(c) => mutate_void(c, caller, |sender, c| {
                gov.set_oip_status(sender, c.id, c.newStatus)
            }),
            oipCount(_) => metadata::<IGovernance::oipCountCall>(|| gov.oip_count()),
            getOipDiff(c) => view(c, |c| {
                let o = gov
                    .get_oip(c.id)?
                    .ok_or(GovernanceError::ProposalNotFound)?;
                let base = diff_base_text(&gov, c.base)?;
                Ok(crate::diff::unified(&base, &o.text))
            }),

            // --- GIP ---
            submitGip(c) => mutate(c, caller, |sender, c| gov.submit_gip(sender, &c.text)),
            getGip(c) => view(c, |c| {
                let g = gov
                    .get_gip(c.id)?
                    .ok_or(GovernanceError::ProposalNotFound)?;
                Ok(gip_to_sol(g))
            }),
            updateGipText(c) => mutate_void(c, caller, |sender, c| {
                gov.update_gip_text(sender, c.id, &c.text)
            }),
            setGipStatus(c) => mutate_void(c, caller, |sender, c| {
                gov.set_gip_status(sender, c.id, c.newStatus)
            }),
            gipCount(_) => metadata::<IGovernance::gipCountCall>(|| gov.gip_count()),
            getGipDiff(c) => view(c, |c| {
                let g = gov
                    .get_gip(c.id)?
                    .ok_or(GovernanceError::ProposalNotFound)?;
                let base = diff_base_text(&gov, c.base)?;
                Ok(crate::diff::unified(&base, &g.text))
            }),

            // --- authorities ---
            isAuthority(c) => view(c, |c| gov.is_authority(c.who)),
        }
    })
}
