//! Spot exchange rates per registry index, readable in either quote direction.

use alloy_primitives::{Address, U256};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;

use crate::constants::reciprocal_scale;
use crate::errors::OracleError;
use crate::schema::{OracleContract, PairIndex};

/// The reciprocal of a rate, in that market's canonical price scale.
///
/// Zero has no reciprocal and stays zero, so "no rate published" reads the same
/// from either side of the market instead of dividing by zero. A rate above the
/// market's `scale^2` floors to zero for the same reason it would in Solidity.
fn invert_rate(pair: AddressPair, rate: U256) -> U256 {
    if rate.is_zero() {
        U256::ZERO
    } else {
        let scale = reciprocal_scale(pair);
        scale * scale / rate
    }
}

impl OracleContract<'_> {
    /// Returns the current exchange rate in the market's configured scale,
    /// quoted in the caller's direction. COEN/ISO uses six decimals. Generic
    /// markets keep their decimal18 contract.
    ///
    /// The Oracle stores only the configured direction, so a backwards quote of
    /// the market returns the reciprocal. This is the one read that answers
    /// either quote direction. Everything else resolves through
    /// [`Self::require_pair_from`].
    pub fn get_exchange_rate(&self, base: Address, quote: Address) -> Result<U256> {
        let pair = AddressPair::from_addresses(base, quote);
        let index = self.pair_to_index.read(&pair)?;
        if index == 0 {
            return Err(OracleError::PairNotRegistered { pair }.into());
        }
        let registered = self.pair_at(index)?;
        let stored = self.exchange_rate.read(&index)?;
        let rate = if pair == registered {
            stored
        } else {
            invert_rate(registered, stored)
        };
        Ok(rate)
    }

    /// [`Self::get_exchange_rate`] together with the block and timestamp of the
    /// stored observation, which are the same for either quote direction.
    pub fn get_exchange_rate_data(
        &self,
        base: Address,
        quote: Address,
    ) -> Result<(U256, u64, u64)> {
        // Both quote directions read the same slot, matching `get_exchange_rate`.
        let pair = AddressPair::from_addresses(base, quote);
        let index = self.pair_to_index.read(&pair)?;
        if index == 0 {
            return Err(OracleError::PairNotRegistered { pair }.into());
        }
        let rate = self.get_exchange_rate(base, quote)?;
        let block = self.exchange_rate_block.read(&index)?;
        let ts = self.exchange_rate_timestamp.read(&index)?;
        Ok((rate, block, ts))
    }

    /// Sets the exchange rate for a pair (system-only bootstrap write).
    pub(crate) fn set_exchange_rate(
        &mut self,
        caller: Address,
        pair: AddressPair,
        rate: U256,
        block_number: u64,
        timestamp: u64,
    ) -> Result<()> {
        // Bootstrap write path: only the system (Address::ZERO) can call it.
        if caller != Address::ZERO {
            return Err(OracleError::OnlySystem("set exchange rate directly").into());
        }
        let index = self.require_pair_index(pair)?;
        self.update_exchange_rate(index, rate, block_number, timestamp)
    }

    /// Updates the exchange rate from tally results (internal, no caller check).
    ///
    /// `index` is the registry index of an already-registered pair. This function
    /// stores the rate in that pair's registered orientation.
    pub fn update_exchange_rate(
        &mut self,
        index: PairIndex,
        rate: U256,
        block_number: u64,
        timestamp: u64,
    ) -> Result<()> {
        self.exchange_rate.write(&index, rate)?;
        self.exchange_rate_block.write(&index, block_number)?;
        self.exchange_rate_timestamp.write(&index, timestamp)?;
        Ok(())
    }

    /// Zeroes the three rate columns for one registry index.
    pub(super) fn clear_exchange_rate(&mut self, index: PairIndex) -> Result<()> {
        self.update_exchange_rate(index, U256::ZERO, 0, 0)
    }
}
