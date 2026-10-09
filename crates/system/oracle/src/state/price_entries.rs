//! The `(pair, rate, volume)` entry columns of one vote or one price snapshot.

use alloy_primitives::{Address, U256};
use outbe_primitives::address_pair::AddressPair;
use outbe_primitives::error::Result;
use outbe_primitives::storage::types::Mapping;

use crate::schema::{OracleContract, PairIndex};

/// Entry columns stored under one key: a validator vote or a snapshot index.
pub(crate) struct PriceEntries<'storage> {
    pairs: Mapping<'storage, PairIndex, AddressPair>,
    rates: Mapping<'storage, PairIndex, U256>,
    volumes: Mapping<'storage, PairIndex, U256>,
}

impl PriceEntries<'_> {
    pub(crate) fn pair(&self, entry: PairIndex) -> Result<AddressPair> {
        self.pairs.read_pair(&entry)
    }

    pub(crate) fn rate(&self, entry: PairIndex) -> Result<U256> {
        self.rates.read(&entry)
    }

    pub(crate) fn volume(&self, entry: PairIndex) -> Result<U256> {
        self.volumes.read(&entry)
    }

    /// Reads entries `0..count`. Each entry reads its pair, rate and volume in
    /// that order.
    pub(crate) fn read_all(&self, count: PairIndex) -> Result<Vec<(AddressPair, U256, U256)>> {
        let mut entries = Vec::with_capacity(count as usize);
        for entry in 0..count {
            entries.push((self.pair(entry)?, self.rate(entry)?, self.volume(entry)?));
        }
        Ok(entries)
    }

    /// Writes the pair, rate and volume of `entry` in that order.
    pub(crate) fn write(
        &self,
        entry: PairIndex,
        pair: AddressPair,
        rate: U256,
        volume: U256,
    ) -> Result<()> {
        self.pairs.write_pair(&entry, pair)?;
        self.rates.write(&entry, rate)?;
        self.volumes.write(&entry, volume)
    }
}

impl<'storage> OracleContract<'storage> {
    /// Entry columns of the pending vote of `voter`.
    pub(crate) fn vote_entries(&self, voter: &Address) -> PriceEntries<'storage> {
        PriceEntries {
            pairs: self.vote_pair.get_nested(voter),
            rates: self.vote_rate.get_nested(voter),
            volumes: self.vote_volume.get_nested(voter),
        }
    }

    /// Entry columns of price snapshot `idx`.
    pub(crate) fn snapshot_entries(&self, idx: u64) -> PriceEntries<'storage> {
        PriceEntries {
            pairs: self.snapshot_pair.get_nested(&idx),
            rates: self.snapshot_rate.get_nested(&idx),
            volumes: self.snapshot_volume.get_nested(&idx),
        }
    }
}
