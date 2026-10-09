//! First block of each UTC hour. Window coverage reads it.

use outbe_primitives::error::Result;

use crate::constants::VWAP_HOUR_SECONDS;
use crate::schema::OracleContract;

impl OracleContract<'_> {
    /// Records the first block of the UTC hour that contains `timestamp`. This
    /// runs every block, so the window's block span is known even without votes.
    pub fn record_hour_block(&mut self, timestamp: u64, block_number: u64) -> Result<()> {
        let hour_start = timestamp - timestamp % VWAP_HOUR_SECONDS;
        if self.hour_first_block.read(&hour_start)? == 0 {
            self.hour_first_block.write(&hour_start, block_number)?;
        }
        Ok(())
    }
}
