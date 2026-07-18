//! Local storage helpers for the Desis module.

use alloy_primitives::U256;
use outbe_primitives::error::Result;

use crate::schema::{AuctionConfig, AuctionStage, BidData, DesisContract, IntexCallTrigger};

impl DesisContract<'_> {
    // --- AuctionStage ---

    pub(crate) fn read_stage(&self, worldwide_day: u32) -> Result<AuctionStage> {
        let raw = self.auction_stage.read(&worldwide_day)?;
        AuctionStage::from_u8(raw).map_err(Into::into)
    }

    pub(crate) fn write_stage(&self, worldwide_day: u32, stage: AuctionStage) -> Result<()> {
        self.auction_stage.write(&worldwide_day, stage as u8)
    }

    // --- AuctionConfig ---

    pub(crate) fn read_auction_config(&self, worldwide_day: u32) -> Result<AuctionConfig> {
        let promis_load_minor = self.config_promis_load_minor.read(&worldwide_day)?;
        Ok(AuctionConfig {
            issuance_currency: self.config_issuance_currency.read(&worldwide_day)? as u16,
            reference_currency: self.config_reference_currency.read(&worldwide_day)? as u16,
            promis_load_minor: u128::try_from(promis_load_minor)
                .map_err(|_| crate::DesisError::InvalidWorldwideDay(worldwide_day))?,
            call_trigger: IntexCallTrigger {
                window_days: self.config_call_window_days.read(&worldwide_day)? as u16,
                threshold_days: self.config_call_threshold_days.read(&worldwide_day)? as u16,
                intex_call_period: self.config_intex_call_period.read(&worldwide_day)?,
            },
            min_intex_bid_rate: self.config_min_bid_rate.read(&worldwide_day)?,
            min_intex_bid_quantity: self.config_min_bid_quantity.read(&worldwide_day)? as u16,
            commit_bond_minor: u128::try_from(self.config_commit_bond_minor.read(&worldwide_day)?)
                .map_err(|_| crate::DesisError::InvalidWorldwideDay(worldwide_day))?,
            entry_price_minor: self.config_entry_price.read(&worldwide_day)?,
        })
    }

    pub(crate) fn write_auction_config(
        &self,
        worldwide_day: u32,
        cfg: &AuctionConfig,
    ) -> Result<()> {
        self.config_issuance_currency
            .write(&worldwide_day, u32::from(cfg.issuance_currency))?;
        self.config_reference_currency
            .write(&worldwide_day, u32::from(cfg.reference_currency))?;
        self.config_promis_load_minor
            .write(&worldwide_day, U256::from(cfg.promis_load_minor))?;
        self.config_call_window_days
            .write(&worldwide_day, u32::from(cfg.call_trigger.window_days))?;
        self.config_call_threshold_days
            .write(&worldwide_day, u32::from(cfg.call_trigger.threshold_days))?;
        self.config_intex_call_period
            .write(&worldwide_day, cfg.call_trigger.intex_call_period)?;
        self.config_min_bid_rate
            .write(&worldwide_day, cfg.min_intex_bid_rate)?;
        self.config_min_bid_quantity
            .write(&worldwide_day, u32::from(cfg.min_intex_bid_quantity))?;
        self.config_commit_bond_minor
            .write(&worldwide_day, U256::from(cfg.commit_bond_minor))?;
        self.config_entry_price
            .write(&worldwide_day, cfg.entry_price_minor)
    }

    // --- bid storage ---

    pub(crate) fn read_bid_count(&self, worldwide_day: u32) -> Result<u32> {
        self.bid_count.read(&worldwide_day)
    }

    pub(crate) fn append_bid(&self, worldwide_day: u32, bid: &BidData) -> Result<u32> {
        let index = self.bid_count.read(&worldwide_day)?;
        self.write_bid_at(worldwide_day, index, bid)?;
        self.bid_count.write(&worldwide_day, index + 1)?;
        Ok(index)
    }

    pub(crate) fn read_bid_at(&self, worldwide_day: u32, index: u32) -> Result<BidData> {
        let key = Self::bid_key(worldwide_day, index);
        let packed = self.bid_packed.read(&key)?;
        let limbs = packed.as_limbs();
        Ok(BidData {
            bidder_address: self.bid_bidder.read(&key)?,
            intex_bid_rate: limbs[0] as u32,
            timestamp: limbs[1] as u32,
            intex_quantity: (limbs[1] >> 32) as u16,
        })
    }

    fn write_bid_at(&self, worldwide_day: u32, index: u32, bid: &BidData) -> Result<()> {
        let key = Self::bid_key(worldwide_day, index);
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
    pub(crate) fn read_all_bids(&self, worldwide_day: u32) -> Result<Vec<BidData>> {
        let count = self.bid_count.read(&worldwide_day)?;
        (0..count)
            .map(|i| self.read_bid_at(worldwide_day, i))
            .collect()
    }

    // --- bid-batch metadata ---

    pub(crate) fn read_last_generation(&self, worldwide_day: u32) -> Result<u32> {
        self.last_bids_generation.read(&worldwide_day)
    }

    pub(crate) fn write_bid_batch_meta(
        &self,
        worldwide_day: u32,
        source_eid: u32,
        generation: u32,
    ) -> Result<()> {
        self.bid_source_eid.write(&worldwide_day, source_eid)?;
        self.last_bids_generation.write(&worldwide_day, generation)
    }

    // --- last cleared series ---

    pub(crate) fn read_last_cleared_worldwide_day(&self) -> Result<u32> {
        self.last_cleared_worldwide_day.read()
    }

    pub(crate) fn write_last_cleared_worldwide_day(&self, worldwide_day: u32) -> Result<()> {
        self.last_cleared_worldwide_day.write(worldwide_day)
    }

    pub(crate) fn read_last_clearing_issued_count(&self) -> Result<u32> {
        self.last_clearing_issued_count.read()
    }

    pub(crate) fn write_last_clearing_issued_count(&self, count: u32) -> Result<()> {
        self.last_clearing_issued_count.write(count)
    }
}
