//! Pair registry: registration, lookup, orientation checks and vote targets.

use alloy_primitives::{Address, U256};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::asset_type::AssetType;
use outbe_primitives::error::Result;
use outbe_primitives::storage::types::Mapping;

use crate::errors::OracleError;
use crate::schema::{OracleContract, PairIndex};

impl OracleContract<'_> {
    /// Registers a new trading pair and marks it as a vote target.
    ///
    /// `pair_by_index` keeps the configured orientation. The storage key sorts
    /// independently, so this function rejects the inverse of an existing pair
    /// as a duplicate. COEN/ISO markets are the sole directional exception.
    /// They must be registered as COEN base, ISO quote.
    pub(crate) fn register_pair(&mut self, pair: AddressPair) -> Result<PairIndex> {
        if pair.address1() == pair.address2() {
            return Err(OracleError::PairBaseQuoteIdentical.into());
        }
        if matches!(
            (pair.asset1(), pair.asset2()),
            (AssetType::IsoCurrency(_), AssetType::Native)
        ) {
            return Err(OracleError::PairNotCanonical { pair }.into());
        }

        if self.pair_index_of(pair)? != 0 {
            return Err(OracleError::PairAlreadyRegistered { pair }.into());
        }

        let count = self.pair_count.read()?;
        let new_index = count + 1;

        self.pair_count.write(new_index)?;
        self.pair_to_index.write(&pair, new_index)?;
        self.pair_by_index.write_pair(&new_index, pair)?;
        self.vote_target.write(&pair, true)?;

        Ok(new_index)
    }

    /// The pair registered at `index`, in its configured orientation.
    ///
    /// An index that was never written reads back as the zero pair. For that
    /// reason, [`Self::pair_index_of`] decides membership, never a zero check.
    /// The zero address is a legitimate asset (native COEN). Callers that
    /// iterate `1..=pair_count` already established the bound. Code that takes
    /// an index from outside goes through [`Self::require_pair_at`].
    pub fn pair_at(&self, index: PairIndex) -> Result<AddressPair> {
        self.pair_by_index.read_pair(&index)
    }

    /// [`Self::pair_at`] for an index that arrived from a caller.
    ///
    /// An unwritten index reads back as the zero pair. The zero pair is
    /// indistinguishable from a genuine COEN/COEN registration, so this function
    /// checks the bound, not the value.
    pub fn require_pair_at(&self, index: PairIndex) -> Result<AddressPair> {
        if index == 0 || index > self.pair_count.read()? {
            return Err(OracleError::PairIndexOutOfRange { index }.into());
        }
        self.pair_at(index)
    }

    /// Deactivates a pair's vote target status (system-only).
    pub fn deactivate_vote_target(
        &mut self,
        caller: Address,
        base: Address,
        quote: Address,
    ) -> Result<()> {
        if caller != Address::ZERO {
            return Err(OracleError::OnlySystem("deactivate vote target").into());
        }
        let pair = self.require_pair_from(base, quote)?;
        self.vote_target.write(&pair, false)?;
        Ok(())
    }

    /// Activates a pair's vote target status (system-only).
    pub fn activate_vote_target(
        &mut self,
        caller: Address,
        base: Address,
        quote: Address,
    ) -> Result<()> {
        if caller != Address::ZERO {
            return Err(OracleError::OnlySystem("activate vote target").into());
        }
        let pair = self.require_pair_from(base, quote)?;
        self.vote_target.write(&pair, true)?;
        Ok(())
    }

    /// Removes exchange rates for deactivated pairs.
    pub fn remove_excess_feeds(&mut self) -> Result<()> {
        let pair_count = self.pair_count.read()?;
        for pid in 1..=pair_count {
            let pair = self.pair_at(pid)?;
            let is_target = self.vote_target.read(&pair)?;
            if !is_target {
                self.clear_exchange_rate(pid)?;
            }
        }
        Ok(())
    }

    /// Returns the enumeration index for a pair, or 0 if not registered.
    ///
    /// Order-independent: the lookup key sorts, so this answers the same for
    /// either quote direction.
    pub fn pair_index_of(&self, pair: AddressPair) -> Result<PairIndex> {
        self.pair_to_index.read(&pair)
    }

    /// Resolves an ABI-quoted pair that has to be quoted the way it is stored.
    ///
    /// Every value read through this resolver is a bare scalar with no direction
    /// of its own (a VWAP, an S-curve peak, a median input). The quote therefore
    /// has to match the configured orientation exactly. Only spot-rate reads may
    /// ask for the reciprocal.
    pub fn require_pair_from(&self, base: Address, quote: Address) -> Result<AddressPair> {
        let pair = AddressPair::from_addresses(base, quote);
        self.require_pair(pair)
    }

    pub fn require_pair(&self, pair: AddressPair) -> Result<AddressPair> {
        self.require_pair_index(pair)?;
        Ok(pair)
    }

    pub fn require_pair_index(&self, pair: AddressPair) -> Result<PairIndex> {
        let index = self.pair_to_index.read(&pair)?;
        if index == 0 {
            return Err(OracleError::PairNotRegistered { pair }.into());
        }
        if self.pair_at(index)? != pair {
            return Err(OracleError::PairNotCanonical { pair }.into());
        }
        Ok(index)
    }

    /// Returns whether a market is an active vote target.
    ///
    /// Direction-insensitive: being a vote target is a property of the market,
    /// not of how a caller quotes it. An answer of `false` for a direction that
    /// [`Self::get_exchange_rate`] prices would be an incoherence a caller could
    /// act on. This is a plain boolean query, so it returns `false` for an
    /// unregistered market and does not revert. Storage faults still propagate.
    pub fn is_vote_target(&self, base: Address, quote: Address) -> Result<bool> {
        let pair = AddressPair::from_addresses(base, quote);
        if self.pair_to_index.read(&pair)? == 0 {
            return Ok(false);
        }
        self.vote_target.read(&pair)
    }

    /// Returns `(bases, quotes)` of all active vote targets.
    pub fn get_vote_targets(&self) -> Result<(Vec<Address>, Vec<Address>)> {
        let count = self.pair_count.read()?;
        let mut bases = Vec::new();
        let mut quotes = Vec::new();

        for pid in 1..=count {
            let pair = self.pair_at(pid)?;
            let (base, quote) = (pair.address1(), pair.address2());
            if self.vote_target.read(&pair)? {
                bases.push(base);
                quotes.push(quote);
            }
        }

        Ok((bases, quotes))
    }

    /// Returns each registered pair whose value in `values` is nonzero, with
    /// that value, in registry order.
    pub(super) fn registered_nonzero_values(
        &self,
        values: &Mapping<'_, PairIndex, U256>,
    ) -> Result<Vec<(AddressPair, U256)>> {
        let pair_count = self.pair_count.read()?;
        let mut nonzero = Vec::new();
        for index in 1..=pair_count {
            let value = values.read(&index)?;
            if value.is_zero() {
                continue;
            }
            nonzero.push((self.pair_at(index)?, value));
        }
        Ok(nonzero)
    }
}
