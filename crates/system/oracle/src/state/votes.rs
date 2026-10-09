//! Pending aggregate votes of the current vote period.

use alloy_primitives::{Address, U256};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;

use crate::schema::OracleContract;

/// `(exists, bases, quotes, rates, volumes)` - pending aggregate vote.
type AggregateVote = (bool, Vec<Address>, Vec<Address>, Vec<U256>, Vec<U256>);

impl OracleContract<'_> {
    /// Clears all votes and resets the voter list. Called after tally.
    pub fn clear_votes(&mut self) -> Result<()> {
        let count = self.voter_list.len()?;

        for i in 0..count {
            let voter = self.voter_list.get(i)?.unwrap_or(Address::ZERO);
            self.vote_exists.write(&voter, false)?;

            let tuple_count = self.vote_tuple_count.read(&voter)?;
            let entries = self.vote_entries(&voter);

            for j in 0..tuple_count {
                // Clears to the zero pair, which is never registered. Readers
                // stay bounded by `vote_tuple_count` and do not probe for it.
                entries.write(j, AddressPair::ZERO, U256::ZERO, U256::ZERO)?;
            }
            self.vote_tuple_count.write(&voter, 0)?;
        }

        self.voter_list.clear()?;
        Ok(())
    }

    /// Returns the pending aggregate vote for a validator.
    ///
    /// Returns `(exists, bases, quotes, rates, volumes)`.
    pub fn get_aggregate_vote(&self, validator: &Address) -> Result<AggregateVote> {
        let exists = self.vote_exists.read(validator)?;
        if !exists {
            return Ok((false, vec![], vec![], vec![], vec![]));
        }

        let tuple_count = self.vote_tuple_count.read(validator)?;
        let entries = self.vote_entries(validator).read_all(tuple_count)?;

        let bases = entries.iter().map(|(pair, _, _)| pair.address1()).collect();
        let quotes = entries.iter().map(|(pair, _, _)| pair.address2()).collect();
        let rates = entries.iter().map(|(_, rate, _)| *rate).collect();
        let volumes = entries.iter().map(|(_, _, volume)| *volume).collect();

        Ok((true, bases, quotes, rates, volumes))
    }
}
