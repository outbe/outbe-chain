//! Fidelity runtime orchestration.
//!
//! RCFI/league are no longer computed on-chain: cohorts are encrypted and the
//! enclave is the sole evaluator. This layer reads/writes the cohort ciphertext
//! from committed storage. It drives the enclave for three flows:
//! - cohort mutations ([`FidelityContract::cohort_in`]/[`FidelityContract::cohort_out`])
//! - the per-owner league snapshot ([`FidelityContract::snapshot_leagues`],
//!   [`FidelityContract::league_at`])
//! - owner-authorized index queries ([`FidelityContract::query_index_at`])
//!
//! Only `max_rcfi_at` stays on-chain, as a pure function of the plaintext
//! `first_qualified_start` anchor.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_tee::protocol::{
    FidelityCohortOp, FidelityCohortRequest, FidelityOpOutcome, FidelityOpSection,
    FidelityQueryRequest, FidelityQueryResult, FidelitySnapshotEntry, FidelitySnapshotRequest,
};

use crate::enclave_client;
use crate::errors::FidelityFatalError;
use crate::math::t_dec;
use crate::schema::FidelityContract;

fn chain_id_b256(storage: &StorageHandle<'_>) -> Result<B256> {
    Ok(B256::from(U256::from(storage.chain_id()?)))
}

impl FidelityContract<'_> {
    /// Build the cohort op section from committed storage (current blob + the
    /// plaintext global anchor), for folding into a co-located gratis op.
    pub fn cohort_section(
        &self,
        account: Address,
        op: FidelityCohortOp,
        timestamp: u64,
    ) -> Result<FidelityOpSection> {
        Ok(FidelityOpSection {
            op,
            timestamp,
            first_qualified_start: self.first_qualified_start()?,
            current_blob: self.cohorts_ct_of(account)?,
        })
    }

    /// Persist a cohort outcome (from a folded gratis op or the standalone
    /// path). Store the new ciphertext. On the account's first acquisition, also
    /// anchor the global `first_qualified_start` (set-once). A probe outcome
    /// (empty blob, no init) is a no-op.
    pub fn apply_outcome(&self, account: Address, outcome: &FidelityOpOutcome) -> Result<()> {
        if !outcome.new_blob.is_empty() {
            self.write_cohorts_ct(account, &outcome.new_blob)?;
        }
        if let Some(ts) = outcome.qualified_start_initialized {
            self.init_first_qualified_start(ts)?;
        }
        Ok(())
    }

    fn run_cohort_op(
        &self,
        account: Address,
        amount: U256,
        op: FidelityCohortOp,
        timestamp: u64,
    ) -> Result<()> {
        // Zero-amount In/Out are no-ops (the enclave would also no-op). Skip the
        // round-trip entirely so a zero mint/burn never touches the enclave.
        if amount.is_zero() {
            return Ok(());
        }
        let section = self.cohort_section(account, op, timestamp)?;
        let req = FidelityCohortRequest {
            chain_id: chain_id_b256(&self.storage)?,
            account,
            amount,
            section,
        };
        let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&self.storage)?;
        let result = enclave_client::apply_cohort_op(req)?;
        self.apply_outcome(account, &result.outcome)
    }

    /// ACQUISITION hook: record a new active gratis cohort for `account` at block
    /// time `timestamp` (seconds). No-op on a zero amount.
    pub fn cohort_in(&self, account: Address, amount: U256, timestamp: u64) -> Result<()> {
        self.run_cohort_op(account, amount, FidelityCohortOp::In, timestamp)
    }

    /// SALE hook: destroy `account`'s active cohorts LIFO at block time
    /// `timestamp` (seconds), logging the sold slices. No-op on a zero amount.
    pub fn cohort_out(&self, account: Address, amount: U256, timestamp: u64) -> Result<()> {
        self.run_cohort_op(account, amount, FidelityCohortOp::Out, timestamp)
    }

    /// Batch league snapshot: read each owner's cohort blob and ask the enclave
    /// for one plaintext league per owner (in `owners` order).
    pub fn snapshot_leagues(
        &self,
        timestamp: u64,
        owners: &[Address],
    ) -> Result<Vec<(Address, u16)>> {
        if owners.is_empty() {
            return Ok(Vec::new());
        }
        let mut entries = Vec::with_capacity(owners.len());
        for owner in owners {
            entries.push(FidelitySnapshotEntry {
                owner: *owner,
                cohort_blob: self.cohorts_ct_of(*owner)?,
            });
        }
        let req = FidelitySnapshotRequest {
            timestamp,
            first_qualified_start: self.first_qualified_start()?,
            entries,
        };
        let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&self.storage)?;
        Ok(enclave_client::snapshot_leagues(req)?
            .into_iter()
            .map(|e| (e.owner, e.league))
            .collect())
    }

    /// League for `account` at `timestamp` (a single-owner snapshot).
    pub fn league_at(&self, account: Address, timestamp: u64) -> Result<u16> {
        self.snapshot_leagues(timestamp, &[account])?
            .first()
            .map(|(_, league)| *league)
            .ok_or_else(|| FidelityFatalError::SnapshotReturnedNoLeague.into())
    }

    /// League for `account` at the current block time.
    pub fn league(&self, account: Address) -> Result<u16> {
        self.league_at(account, self.storage.now_secs()?)
    }

    /// Synthetic-max RCFI at `timestamp`: `t_dec(utc_day(timestamp) - first_qualified_start)`.
    /// Pure function of the plaintext UTC-day anchor. It runs on-chain, with no enclave.
    /// Zero before any account has qualified.
    pub fn max_rcfi_at(&self, timestamp: u64) -> Result<U256> {
        let first = self.first_qualified_start()?;
        if first == 0 {
            return Ok(U256::ZERO);
        }
        let eval_day = outbe_primitives::time::utc_day_from_timestamp(timestamp);
        Ok(t_dec(eval_day.saturating_sub(first)))
    }

    /// Synthetic-max RCFI at the UTC day corresponding to `wwd`.
    pub fn synthetic_max_for_wwd(&self, wwd: outbe_primitives::time::WorldwideDay) -> Result<U256> {
        let first = self.first_qualified_start()?;
        if first == 0 || !wwd.is_valid() {
            return Ok(U256::ZERO);
        }
        let ts = outbe_primitives::time::date_key_to_utc_timestamp(wwd.value());
        let eval_day = outbe_primitives::time::utc_day_from_timestamp(ts);
        Ok(t_dec(eval_day.saturating_sub(first)))
    }

    /// Returns the fixed `max_fidelity` for `wwd` (using the locked snapshot if
    /// already locked, or `max(max_fidelity_prev, synthetic_max_for_wwd(wwd))`).
    pub fn max_fidelity_for_wwd(&self, wwd: outbe_primitives::time::WorldwideDay) -> Result<U256> {
        if let Some(locked) = self.wwd_max_fidelity_of(wwd)? {
            return Ok(locked);
        }
        Ok(self
            .max_fidelity_prev()?
            .max(self.synthetic_max_for_wwd(wwd)?))
    }

    /// Locks and returns the fixed `max_fidelity` for `wwd`.
    pub fn ensure_wwd_max_fidelity(
        &self,
        wwd: outbe_primitives::time::WorldwideDay,
    ) -> Result<U256> {
        if let Some(locked) = self.wwd_max_fidelity_of(wwd)? {
            return Ok(locked);
        }
        let max_fid = self
            .max_fidelity_prev()?
            .max(self.synthetic_max_for_wwd(wwd)?);
        self.lock_wwd_max_fidelity(wwd, max_fid)?;
        Ok(max_fid)
    }

    /// Computes the league (`1..=4096`) for a given `user_rcfi` against the fixed
    /// `max_fidelity` of `wwd`, and records `user_rcfi` in `wwd_running_max_fidelity[wwd]`.
    pub fn league_from_rcfi_for_wwd(
        &self,
        wwd: outbe_primitives::time::WorldwideDay,
        user_rcfi: U256,
    ) -> Result<u16> {
        let max_fid = self.ensure_wwd_max_fidelity(wwd)?;
        let league = crate::math::league_from_rcfi(user_rcfi, max_fid)
            .ok_or(FidelityFatalError::SnapshotReturnedNoLeague)?;
        if !user_rcfi.is_zero() {
            self.record_wwd_fidelity(wwd, user_rcfi)?;
        }
        Ok(league)
    }

    /// Evaluates the league (`1..=4096`) for `account` on `wwd` at `timestamp`
    /// (user fidelity by UTC day, max fidelity fixed per WWD).
    /// When the account has no cohort history (`cohorts_ct` is empty), returns
    /// `MIN_LEAGUE` (`1`) without requiring an enclave round-trip or storage writes.
    pub fn league_for_wwd(
        &self,
        account: Address,
        wwd: outbe_primitives::time::WorldwideDay,
        timestamp: u64,
    ) -> Result<u16> {
        let blob = self.cohorts_ct_of(account)?;
        if blob.is_empty() {
            return Ok(crate::MIN_LEAGUE);
        }
        let req = FidelitySnapshotRequest {
            timestamp,
            first_qualified_start: self.first_qualified_start()?,
            entries: vec![FidelitySnapshotEntry {
                owner: account,
                cohort_blob: blob,
            }],
        };
        let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&self.storage)?;
        let entry = enclave_client::snapshot_leagues(req)?
            .into_iter()
            .next()
            .ok_or(FidelityFatalError::SnapshotReturnedNoLeague)?;
        self.league_from_rcfi_for_wwd(wwd, entry.rcfi)
    }

    /// Finalizes `wwd` at Metadosis time by rolling `wwd_running_max_fidelity[wwd]`
    /// and the day's ceiling into `max_fidelity_prev` for the next WWD.
    pub fn finalize_wwd_max_fidelity(
        &self,
        wwd: outbe_primitives::time::WorldwideDay,
        _timestamp: u64,
    ) -> Result<U256> {
        let prev = self.max_fidelity_prev()?;
        let running = self.wwd_running_max_fidelity_of(wwd)?;
        let current_max = match self.wwd_max_fidelity_of(wwd)? {
            Some(locked) => locked,
            None => prev.max(self.synthetic_max_for_wwd(wwd)?),
        };
        let finalized = prev.max(running).max(current_max);
        if finalized != prev {
            self.set_max_fidelity_prev(finalized)?;
        }
        Ok(finalized)
    }

    /// Owner-authorized RCFI/league query at `query_timestamp` (the eth_call
    /// path). The enclave verifies the signed, expiring, chain-scoped
    /// authorization before decrypting.
    pub fn query_index_at(
        &self,
        account: Address,
        query_timestamp: u64,
        expiry: u64,
        owner_sig: Vec<u8>,
    ) -> Result<FidelityQueryResult> {
        let req = FidelityQueryRequest {
            chain_id: chain_id_b256(&self.storage)?,
            account,
            cohort_blob: self.cohorts_ct_of(account)?,
            query_timestamp,
            block_timestamp: self.storage.now_secs()?,
            first_qualified_start: self.first_qualified_start()?,
            expiry,
            owner_sig,
        };
        let _enclave_context = outbe_tee::call_context::ContextScope::from_storage(&self.storage)?;
        enclave_client::query_index(req)
    }

    /// Owner-authorized RCFI/league query at the current block time.
    pub fn query_index_now(
        &self,
        account: Address,
        expiry: u64,
        owner_sig: Vec<u8>,
    ) -> Result<FidelityQueryResult> {
        self.query_index_at(account, self.storage.now_secs()?, expiry, owner_sig)
    }
}
