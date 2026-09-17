use crate::schema::Gratis;
use alloy_primitives::U256;
use outbe_primitives::error::Result;

impl Gratis<'_> {
    pub fn name(&self) -> &str {
        "gratis"
    }
    pub fn symbol(&self) -> &str {
        "GRATIS"
    }
    pub fn decimals(&self) -> u8 {
        6
    }
    pub fn total_supply(&self) -> Result<U256> {
        self.total_supply.read()
    }
    pub fn pledged_total_supply(&self) -> Result<U256> {
        self.pledged_total_supply.read()
    }
}
