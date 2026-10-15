//! Local storage helpers for the IntexFactory module: the minePromis sequence.

use alloy_primitives::Address;
use outbe_intex::SeriesId;
use outbe_primitives::error::Result;

use crate::schema::IntexFactoryContract;

impl IntexFactoryContract<'_> {
    // --- mineSeq ---

    pub(crate) fn read_mine_seq(&self, series_id: SeriesId, owner: Address) -> Result<u32> {
        let key = Self::mine_seq_key(series_id, owner);
        self.mine_seq.read(&key)
    }

    pub(crate) fn write_mine_seq(
        &mut self,
        series_id: SeriesId,
        owner: Address,
        value: u32,
    ) -> Result<()> {
        let key = Self::mine_seq_key(series_id, owner);
        self.mine_seq.write(&key, value)
    }
}
