//! Aggregate vote submission.

use alloy_primitives::{Address, U256};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;
use std::collections::BTreeSet;

use crate::constants::vote_caps;
use crate::errors::OracleError;
use crate::schema::OracleContract;

impl OracleContract<'_> {
    // -----------------------------------------------------------------------
    // Vote Submission
    // -----------------------------------------------------------------------

    /// Submits an aggregate oracle vote on behalf of a validator.
    ///
    /// The caller must be the validator itself or a delegated feeder.
    /// Each tuple contains (base, quote, rate, volume) for one pair, quoted in
    /// the direction the pair was registered in.
    pub fn submit_vote(
        &mut self,
        caller: Address,
        tuples: &[(Address, Address, U256, U256)],
    ) -> Result<Address> {
        let validator = self.resolve_validator_for_feeder(caller)?;
        let resolved = self.resolve_vote_pairs(tuples)?;

        // Check if already voted this period
        let already_voted = self.vote_exists.read(&validator)?;
        if already_voted {
            return Err(OracleError::AlreadyVotedThisPeriod.into());
        }

        // Reject an out-of-range quote before the voted flag, so the feeder can
        // resubmit a market-sized vote in the same period.
        for (pair, (_, _, rate, volume)) in resolved.iter().zip(tuples) {
            Self::reject_vote_outside_market_bounds(*pair, *rate, *volume)?;
        }

        // Mark as voted FIRST to prevent concurrent overwrite (ORC-AUD-037).
        // EVM executes transactions sequentially within a block, so a second
        // submitVote TX in the same block sees this flag immediately.
        self.vote_exists.write(&validator, true)?;

        self.store_vote_tuples(validator, &resolved, tuples)?;

        // Add to voter list for tally iteration
        self.voter_list.push(validator)?;

        Ok(validator)
    }

    /// Resolves the pair of every tuple in submission order. Rejects a
    /// submission with more tuples than registered pairs, an unregistered or
    /// flipped pair, a duplicate pair, or a pair that is not a vote target.
    fn resolve_vote_pairs(
        &self,
        tuples: &[(Address, Address, U256, U256)],
    ) -> Result<Vec<AddressPair>> {
        // Validate tuple count: cannot exceed active pair count
        let pair_count = self.pair_count.read()?;
        if tuples.len() as u32 > pair_count {
            return Err(OracleError::VoteTupleCountExceedsPairCount.into());
        }

        // Resolve every quoted pair up front. `require_pair` rejects an
        // unregistered pair and one quoted against the registered direction.
        // The rate is a bare scalar, so a flipped quote would otherwise feed an
        // uninverted price into the tally median. Nothing downstream would be
        // able to notice.
        let mut resolved = Vec::with_capacity(tuples.len());
        for (base, quote, _, _) in tuples {
            resolved.push(self.require_pair_from(*base, *quote)?);
        }

        // Check for duplicate pairs in the submission. Kept separate from the
        // vote-target loop below so the revert precedence for a submission that
        // is both duplicated and untargeted stays unchanged.
        let mut seen = BTreeSet::new();
        for pair in &resolved {
            if !seen.insert(*pair) {
                return Err(OracleError::DuplicatePairInVote.into());
            }
        }

        // Verify all pairs are vote targets
        for pair in &resolved {
            let is_target = self.vote_target.read(pair)?;
            if !is_target {
                return Err(OracleError::PairNotVoteTarget.into());
            }
        }
        Ok(resolved)
    }

    /// Stores the resolved pairs with their rates and volumes as the vote of
    /// `validator`.
    fn store_vote_tuples(
        &self,
        validator: Address,
        resolved: &[AddressPair],
        tuples: &[(Address, Address, U256, U256)],
    ) -> Result<()> {
        // Store vote tuples
        let tuple_count = tuples.len() as u32;
        self.vote_tuple_count.write(&validator, tuple_count)?;

        let pair_map = self.vote_pair.get_nested(&validator);
        let rate_map = self.vote_rate.get_nested(&validator);
        let volume_map = self.vote_volume.get_nested(&validator);

        // Store the pairs `require_pair` already resolved rather than re-packing
        // the raw tuples, so what lands in storage is exactly what was validated.
        for (i, (pair, (_, _, rate, volume))) in resolved.iter().zip(tuples).enumerate() {
            let idx = i as u32;
            pair_map.write_pair(&idx, *pair)?;
            rate_map.write(&idx, *rate)?;
            volume_map.write(&idx, *volume)?;
        }
        Ok(())
    }

    fn reject_vote_outside_market_bounds(
        pair: AddressPair,
        rate: U256,
        volume: U256,
    ) -> Result<()> {
        let caps = vote_caps(pair);
        if rate > caps.max_price {
            return Err(OracleError::VotePriceExceedsCap {
                price: rate,
                max: caps.max_price,
            }
            .into());
        }
        if volume > caps.max_volume {
            return Err(OracleError::VoteVolumeExceedsCap {
                volume,
                max: caps.max_volume,
            }
            .into());
        }
        Ok(())
    }
}
