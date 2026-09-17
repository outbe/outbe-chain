//! Reservation storage transitions; token movement is owned by the runtime.

use alloy_primitives::B256;
use outbe_primitives::error::Result;

use crate::errors::VaultRouterError;
use crate::schema::{Reservation, VaultRouterContract};

pub(crate) const HELD: u8 = 1;
pub(crate) const REFUND_PENDING: u8 = 2;
pub(crate) const CLOSED: u8 = 3;

impl VaultRouterContract<'_> {
    pub(crate) fn reservation(&self, id: B256) -> Result<Reservation> {
        let status = self.reservation_statuses.read(&id)?;
        if status != HELD && status != REFUND_PENDING {
            return Err(VaultRouterError::ReservationUnavailable.into());
        }
        Ok(Reservation {
            asset: self.reservation_assets.read(&id)?,
            amount: self.reservation_amounts.read(&id)?,
            vault: self.reservation_vaults.read(&id)?,
            valid_until: self.reservation_deadlines.read(&id)?,
            status,
        })
    }

    pub(crate) fn insert_reservation(&self, id: B256, record: &Reservation) -> Result<()> {
        self.reservation_assets.write(&id, record.asset)?;
        self.reservation_amounts.write(&id, record.amount)?;
        self.reservation_vaults.write(&id, record.vault)?;
        self.reservation_deadlines.write(&id, record.valid_until)?;
        self.reservation_statuses.write(&id, HELD)?;
        let total = self
            .reserved_totals
            .read(&record.asset)?
            .checked_add(record.amount)
            .ok_or(VaultRouterError::ReservationAccounting)?;
        self.reserved_totals.write(&record.asset, total)?;
        let count = self
            .vault_reservations
            .read(&record.vault)?
            .checked_add(alloy_primitives::U256::from(1))
            .ok_or(VaultRouterError::ReservationAccounting)?;
        self.vault_reservations.write(&record.vault, count)?;
        self.reservation_expiries.push_back(id)
    }

    pub(crate) fn close_reservation(&self, id: B256, record: &Reservation) -> Result<()> {
        let total = self
            .reserved_totals
            .read(&record.asset)?
            .checked_sub(record.amount)
            .ok_or(VaultRouterError::ReservationAccounting)?;
        let count = self
            .vault_reservations
            .read(&record.vault)?
            .checked_sub(alloy_primitives::U256::from(1))
            .ok_or(VaultRouterError::ReservationAccounting)?;
        self.reserved_totals.write(&record.asset, total)?;
        self.vault_reservations.write(&record.vault, count)?;
        self.reservation_statuses.write(&id, CLOSED)?;
        self.reservation_assets.clear(&id)?;
        self.reservation_amounts.clear(&id)?;
        self.reservation_vaults.clear(&id)?;
        self.reservation_deadlines.clear(&id)?;
        self.reservation_retry_at.clear(&id)
    }
}
