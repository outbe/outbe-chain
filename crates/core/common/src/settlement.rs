//! Settlement terms shared by Nod, Intex, Gem and Credis. The cost rule itself lives in
//! `outbe_primitives::settlement`, where Lysis reaches it too.

use alloy_primitives::{Address, U256};

pub use outbe_primitives::settlement::{
    floor_to_asset_units, settlement_cost_units, RoundingError, SettlementCost, COST_DECIMALS,
};

/// The last second a called right may still be settled: `called_at` plus its notice period.
#[must_use]
pub fn settlement_deadline_of(called_at: u64, notice_period: u32) -> u64 {
    called_at.saturating_add(u64::from(notice_period))
}

/// Who pays a settlement, in which asset, and against which VWAP snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlementPayment {
    pub payer: Address,
    pub asset: Address,
    pub snapshot_id: U256,
}

/// Which of a right's two currencies a payment asset is denominated in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaymentCurrency {
    Reference,
    Issuance,
}

impl PaymentCurrency {
    /// The cross rate and its snapshot a payment converts at. Only the issuance
    /// rail converts, so only it calls `cross_rate`.
    pub fn conversion<R, S, E>(
        self,
        cross_rate: impl FnOnce() -> Result<(R, S), E>,
    ) -> Result<(Option<R>, Option<S>), E> {
        match self {
            Self::Reference => Ok((None, None)),
            Self::Issuance => {
                let (rate, snapshot) = cross_rate()?;
                Ok((Some(rate), Some(snapshot)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_issuance_rail_converts() {
        let unavailable = || Err::<(U256, u8), _>("unavailable");
        assert_eq!(
            PaymentCurrency::Reference.conversion(unavailable),
            Ok((None, None))
        );
        assert_eq!(
            PaymentCurrency::Issuance.conversion(unavailable),
            Err("unavailable")
        );
        assert_eq!(
            PaymentCurrency::Issuance.conversion(|| Ok::<_, &str>((U256::from(2u64), 7u8))),
            Ok((Some(U256::from(2u64)), Some(7u8)))
        );
    }
}
