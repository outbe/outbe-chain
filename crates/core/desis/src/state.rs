//! Local storage helpers for the Desis module.

use alloy_primitives::U256;
use outbe_primitives::error::Result;

use crate::schema::{AuctionConfig, AuctionStage, BidData, DesisContract, IntexCallTrigger};

impl DesisContract<'_> {
    // --- AuctionStage ---

    pub(crate) fn read_stage(&self, series_id: u32) -> Result<AuctionStage> {
        let raw = self.auction_stage.read(&series_id)?;
        AuctionStage::from_u8(raw).map_err(Into::into)
    }

    pub(crate) fn write_stage(&self, series_id: u32, stage: AuctionStage) -> Result<()> {
        self.auction_stage.write(&series_id, stage as u8)
    }

    // --- AuctionConfig ---

    pub(crate) fn read_auction_config(&self, series_id: u32) -> Result<AuctionConfig> {
        let promis_load_minor = self.config_promis_load_minor.read(&series_id)?;
        Ok(AuctionConfig {
            issuance_currency: self.config_issuance_currency.read(&series_id)? as u16,
            reference_currency: self.config_reference_currency.read(&series_id)? as u16,
            promis_load_minor: u128::try_from(promis_load_minor)
                .map_err(|_| crate::DesisError::InvalidSeriesId(series_id))?,
            call_trigger: IntexCallTrigger {
                window_days: self.config_call_window_days.read(&series_id)? as u16,
                threshold_days: self.config_call_threshold_days.read(&series_id)? as u16,
                intex_call_period: self.config_intex_call_period.read(&series_id)?,
            },
            min_intex_bid_rate: self.config_min_bid_rate.read(&series_id)?,
            min_intex_bid_quantity: self.config_min_bid_quantity.read(&series_id)? as u16,
            entry_price_minor: self.config_entry_price.read(&series_id)?,
        })
    }

    pub(crate) fn write_auction_config(&self, series_id: u32, cfg: &AuctionConfig) -> Result<()> {
        self.config_issuance_currency
            .write(&series_id, u32::from(cfg.issuance_currency))?;
        self.config_reference_currency
            .write(&series_id, u32::from(cfg.reference_currency))?;
        self.config_promis_load_minor
            .write(&series_id, U256::from(cfg.promis_load_minor))?;
        self.config_call_window_days
            .write(&series_id, u32::from(cfg.call_trigger.window_days))?;
        self.config_call_threshold_days
            .write(&series_id, u32::from(cfg.call_trigger.threshold_days))?;
        self.config_intex_call_period
            .write(&series_id, cfg.call_trigger.intex_call_period)?;
        self.config_min_bid_rate
            .write(&series_id, cfg.min_intex_bid_rate)?;
        self.config_min_bid_quantity
            .write(&series_id, u32::from(cfg.min_intex_bid_quantity))?;
        self.config_entry_price
            .write(&series_id, cfg.entry_price_minor)
    }

    // --- bid storage ---

    pub(crate) fn read_bid_count(&self, series_id: u32) -> Result<u32> {
        self.bid_count.read(&series_id)
    }

    pub(crate) fn append_bid(&self, series_id: u32, bid: &BidData) -> Result<u32> {
        let index = self.bid_count.read(&series_id)?;
        self.write_bid_at(series_id, index, bid)?;
        self.bid_count.write(&series_id, index + 1)?;
        Ok(index)
    }

    /// Replace all bids for a series (called when a newer generation arrives).
    pub(crate) fn replace_bids(&self, series_id: u32, bids: &[BidData]) -> Result<()> {
        for (i, bid) in bids.iter().enumerate() {
            self.write_bid_at(series_id, i as u32, bid)?;
        }
        self.bid_count.write(&series_id, bids.len() as u32)
    }

    pub(crate) fn read_bid_at(&self, series_id: u32, index: u32) -> Result<BidData> {
        let key = Self::bid_key(series_id, index);
        let packed = self.bid_packed.read(&key)?;
        let limbs = packed.as_limbs();
        Ok(BidData {
            bidder_address: self.bid_bidder.read(&key)?,
            intex_bid_rate: limbs[0] as u32,
            timestamp: limbs[1] as u32,
            intex_quantity: (limbs[1] >> 32) as u16,
        })
    }

    fn write_bid_at(&self, series_id: u32, index: u32, bid: &BidData) -> Result<()> {
        let key = Self::bid_key(series_id, index);
        self.bid_bidder.write(&key, bid.bidder_address)?;
        let packed = U256::from_limbs([
            u64::from(bid.intex_bid_rate),
            (u64::from(bid.intex_quantity) << 32) | u64::from(bid.timestamp),
            0,
            0,
        ]);
        self.bid_packed.write(&key, packed)
    }

    /// Load all bids for a series into memory.
    pub(crate) fn read_all_bids(&self, series_id: u32) -> Result<Vec<BidData>> {
        let count = self.bid_count.read(&series_id)?;
        (0..count).map(|i| self.read_bid_at(series_id, i)).collect()
    }

    // --- bid-batch metadata ---

    pub(crate) fn read_last_generation(&self, series_id: u32) -> Result<u32> {
        self.last_bids_generation.read(&series_id)
    }

    pub(crate) fn write_bid_batch_meta(
        &self,
        series_id: u32,
        source_eid: u32,
        generation: u32,
    ) -> Result<()> {
        self.bid_source_eid.write(&series_id, source_eid)?;
        self.last_bids_generation.write(&series_id, generation)
    }

    // --- last cleared series ---

    pub(crate) fn read_last_cleared_series(&self) -> Result<u32> {
        self.last_cleared_series_id.read()
    }

    pub(crate) fn write_last_cleared_series(&self, series_id: u32) -> Result<()> {
        self.last_cleared_series_id.write(series_id)
    }

    pub(crate) fn read_last_clearing_issued_count(&self) -> Result<u32> {
        self.last_clearing_issued_count.read()
    }

    pub(crate) fn write_last_clearing_issued_count(&self, count: u32) -> Result<()> {
        self.last_clearing_issued_count.write(count)
    }
}
