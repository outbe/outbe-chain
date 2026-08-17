//! Business logic for the CCA program.
//!
//! Two things live here: the registry lifecycle (§8.1) and the accountability
//! arithmetic (§8.4). The economic shape of the multiplier is that origination
//! never moves it, voids cut it in proportion to what was actually left unpaid,
//! and only repayment brings it back — so marketing volume cannot restore
//! standing, and idle time cannot either.

use alloy_primitives::{Address, U256};
use outbe_common::WorldwideDay;
use outbe_primitives::error::Result;

use crate::constants::{
    MAX_DECAY_HALVINGS, MIN_PRINCIPAL_FOR_UNIT, MULTIPLIER_ONE, ORIGINATION_FLOOR, RECOVERY_STEP,
    RECOVERY_VALUE_UNIT, VOID_PENALTY_STEP,
};
use crate::errors::CcaError;
use crate::precompile::ICca;
use crate::schema::{CcaContract, CcaRecord, CcaState};

impl CcaContract<'_> {
    // ---------------------------------------------------------------------
    // Registry lifecycle (§8.1)
    // ---------------------------------------------------------------------

    /// Registers `cca` as an agent, starting at an unpenalized multiplier.
    ///
    /// Permissionless by design — §8.1 has no approvals and no allowlist. The
    /// COEN bond that is supposed to price identity reset is a separate
    /// subsystem and is not enforced here yet, so on this build registration is
    /// free and carries no sybil cost.
    pub fn register(&mut self, cca: Address, now: u64) -> Result<()> {
        if cca.is_zero() {
            return Err(CcaError::InvalidAddress.into());
        }
        if self.cca_exists(cca)? {
            return Err(CcaError::AlreadyRegistered.into());
        }
        let record = CcaRecord {
            cca,
            registered: true,
            multiplier: MULTIPLIER_ONE,
            registered_at: now,
            state: CcaState::Active as u8,
            recovery_progress: U256::ZERO,
        };
        self.create_cca_record(&record)?;
        self.emit(ICca::CcaRegistered {
            cca,
            multiplier: MULTIPLIER_ONE,
        })?;
        Ok(())
    }

    /// Retires an agent and releases the identity: the presence flag is cleared,
    /// so the address stops resolving to a record and is free to register again.
    ///
    /// Exit is total. Positions the agent originated still settle or void on
    /// their own terms, but with no record left there is no standing for either
    /// to move, and a re-registration starts over at an unpenalized multiplier.
    /// §8.2 is what prices that exit — a haircut on the bond. Until the bond
    /// lands, leaving and coming back clean costs nothing.
    pub fn deregister(&mut self, cca: Address) -> Result<()> {
        let mut record = self.load_cca(cca)?;
        record.state = CcaState::Deregistered as u8;
        record.registered = false;
        self.update_cca_record(&record)?;
        self.emit(ICca::CcaDeregistered { cca })?;
        Ok(())
    }

    /// Whether `cca` may open new positions: registered, active, and out of
    /// §8.4 quarantine. An unregistered address is simply `false` rather than an
    /// error, so callers can gate on it without catching a revert.
    pub fn can_originate(&self, cca: Address) -> Result<bool> {
        let Some(record) = self.ccas.get(cca)? else {
            return Ok(false);
        };
        Ok(record.registration_state()? == CcaState::Active
            && record.multiplier >= ORIGINATION_FLOOR)
    }

    // ---------------------------------------------------------------------
    // Origination units (§8.3)
    // ---------------------------------------------------------------------

    /// Credits `cca` with the reward units earned by originating a position of
    /// `principal` for `owner` on `day`, and returns the units credited.
    ///
    /// One origination is one unit, moderated exactly as §8.3 describes: a
    /// principal below the dust guard earns nothing, an owner already counted
    /// today earns nothing, and otherwise the unit halves once per prior
    /// position this agent opened for this owner. Only an origination that
    /// actually earns a unit consumes the owner's slot for the day or advances
    /// the decay, so a dust position cannot be used to burn either.
    ///
    /// The multiplier is **not** applied here. It is a property of the agent at
    /// distribution time, not of the origination, and applying it now would
    /// freeze in a multiplier that a later void or repayment should have moved.
    pub fn record_origination(
        &mut self,
        cca: Address,
        owner: Address,
        principal: U256,
        day: WorldwideDay,
    ) -> Result<U256> {
        if !self.cca_exists(cca)? {
            return Err(CcaError::NotRegistered.into());
        }
        if principal < MIN_PRINCIPAL_FOR_UNIT || self.read_owner_day_seen(day, cca, owner)? {
            return Ok(U256::ZERO);
        }

        let prior = self.read_owner_origination_count(cca, owner)?;
        let units = MULTIPLIER_ONE >> prior.min(MAX_DECAY_HALVINGS);

        self.bump_owner_origination_count(cca, owner)?;
        self.mark_owner_day_seen(day, cca, owner)?;
        self.add_day_units(day, cca, units)?;

        self.emit(ICca::OriginationRecorded {
            cca,
            smartAccount: owner,
            worldwideDay: day.value(),
            units,
        })?;
        Ok(units)
    }

    // ---------------------------------------------------------------------
    // Accountability (§8.4)
    // ---------------------------------------------------------------------

    /// Cuts the multiplier for a void, in proportion to the share of principal
    /// left unpaid: `m ← m × (1 − 0.10 × unpaid_share)`.
    ///
    /// A fully unpaid void costs the whole 10% step; a void of a 25% remainder
    /// costs a quarter of it. The cut is multiplicative, so repeated voids
    /// compound downward and the multiplier can approach zero without ever
    /// going negative. A void by an agent that never registered — or that has
    /// since deregistered — is ignored: there is no record to penalize.
    pub fn record_void(&mut self, cca: Address, unpaid_share: U256) -> Result<()> {
        let Some(mut record) = self.ccas.get(cca)? else {
            return Ok(());
        };
        let cut = VOID_PENALTY_STEP
            .checked_mul(unpaid_share.min(MULTIPLIER_ONE))
            .ok_or(CcaError::ArithmeticOverflow)?
            / MULTIPLIER_ONE;
        let factor = MULTIPLIER_ONE
            .checked_sub(cut)
            .ok_or(CcaError::ArithmeticOverflow)?;
        record.multiplier = record
            .multiplier
            .checked_mul(factor)
            .ok_or(CcaError::ArithmeticOverflow)?
            / MULTIPLIER_ONE;
        // A penalty invalidates progress banked toward the previous shortfall:
        // recovery is measured from where the agent now stands.
        record.recovery_progress = U256::ZERO;
        let multiplier = record.multiplier;
        self.update_cca_record(&record)?;
        self.emit(ICca::MultiplierPenalized {
            cca,
            multiplier,
            unpaidShare: unpaid_share,
        })?;
        Ok(())
    }

    /// Restores the multiplier as the agent's book repays: every
    /// `RECOVERY_VALUE_UNIT` of settled principal earns one `RECOVERY_STEP`,
    /// capped at 1.
    ///
    /// An unpenalized agent banks nothing. Letting a clean run accumulate credit
    /// would let an agent pre-pay for a future void, which is the opposite of
    /// what §8.4 asks for — recovery has to be work done *after* the failure.
    pub fn record_settlement(&mut self, cca: Address, settled_value: U256) -> Result<()> {
        let Some(mut record) = self.ccas.get(cca)? else {
            return Ok(());
        };
        if record.multiplier >= MULTIPLIER_ONE {
            if !record.recovery_progress.is_zero() {
                record.recovery_progress = U256::ZERO;
                self.update_cca_record(&record)?;
            }
            return Ok(());
        }

        let progress = record
            .recovery_progress
            .checked_add(settled_value)
            .ok_or(CcaError::ArithmeticOverflow)?;
        let steps = progress / RECOVERY_VALUE_UNIT;
        record.recovery_progress = progress % RECOVERY_VALUE_UNIT;
        if !steps.is_zero() {
            let gain = steps
                .checked_mul(RECOVERY_STEP)
                .ok_or(CcaError::ArithmeticOverflow)?;
            record.multiplier = record
                .multiplier
                .checked_add(gain)
                .ok_or(CcaError::ArithmeticOverflow)?
                .min(MULTIPLIER_ONE);
        }
        let multiplier = record.multiplier;
        self.update_cca_record(&record)?;
        if !steps.is_zero() {
            self.emit(ICca::MultiplierRecovered { cca, multiplier })?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------------
    // Reads
    // ---------------------------------------------------------------------

    /// The agent's record. Reverts when it is not registered.
    pub fn get_cca(&self, cca: Address) -> Result<CcaRecord> {
        self.load_cca(cca)
    }

    /// The agent's multiplier, or 1 for an address that never registered — the
    /// neutral weight, so an unregistered agent contributes nothing rather than
    /// distorting a distribution.
    pub fn multiplier_of(&self, cca: Address) -> Result<U256> {
        Ok(self
            .ccas
            .get(cca)?
            .map_or(MULTIPLIER_ONE, |record| record.multiplier))
    }

    /// Raw origination units `cca` earned on `day`, before the multiplier.
    pub fn origination_units(&self, day: WorldwideDay, cca: Address) -> Result<U256> {
        self.read_day_units(day, cca)
    }

    /// Every agent that earned units on `day`, paired with those units. This is
    /// what the daily reward distribution weights.
    pub fn day_originators(&self, day: WorldwideDay) -> Result<Vec<(Address, U256)>> {
        let count = self.read_day_cca_count(day)?;
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count {
            let cca = self.read_day_cca_at(day, i)?;
            out.push((cca, self.read_day_units(day, cca)?));
        }
        Ok(out)
    }
}
