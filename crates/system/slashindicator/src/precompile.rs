use alloy_primitives::{Address, Bytes, U256};
use alloy_sol_types::{sol, SolInterface};
use outbe_primitives::dispatch::{dispatch_call, mutate_void, reject_value, view};
use outbe_primitives::error::Result;

use crate::schema::SlashIndicator;

sol!(
    #![sol(alloy_sol_types = alloy_sol_types, extra_derives(Debug, PartialEq))]
    "../../../contracts/precompiles/src/ISlashIndicator.sol"
);

/// Dispatches an ABI-encoded call to the SlashIndicator precompile.
pub fn dispatch(
    storage: outbe_primitives::storage::StorageHandle,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    reject_value(&value)?;
    dispatch_call(
        data,
        ISlashIndicator::ISlashIndicatorCalls::abi_decode,
        |call| {
            use ISlashIndicator::ISlashIndicatorCalls::*;
            match call {
                submitDoubleProposalEvidence(c) => mutate_void(c, caller, |sender, c| {
                    let mut si = SlashIndicator::new(storage);
                    si.submit_double_proposal_evidence(sender, &c.block1, &c.block2)
                }),
                submitConflictingVoteEvidence(c) => mutate_void(c, caller, |sender, c| {
                    let mut si = SlashIndicator::new(storage);
                    si.submit_conflicting_vote_evidence(sender, &c.vote1, &c.vote2)
                }),
                submitInvalidVrfProofEvidence(c) => mutate_void(c, caller, |sender, c| {
                    let mut si = SlashIndicator::new(storage);
                    si.submit_invalid_vrf_evidence(sender, &c.evidence)
                }),
                getProposerMissCount(c) => view(c, |c| {
                    let si = SlashIndicator::new(storage);
                    si.get_proposer_miss_count(c.validator)
                }),
                getVoterMissCount(c) => view(c, |c| {
                    let si = SlashIndicator::new(storage);
                    si.get_voter_miss_count(c.validator)
                }),
                getFelonyCount(c) => view(c, |c| {
                    let si = SlashIndicator::new(storage);
                    si.get_felony_count(c.validator)
                }),
            }
        },
    )
}
