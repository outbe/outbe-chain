//! Fidelity orchestration over its own global encrypted journal. Cohort lookup
//! stays inside the enclave. Public league snapshots remain an intentional
//! disclosure for OCOMP; see the confidential-state privacy boundary.

use alloy_primitives::{Address, B256, U256};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_tee::protocol::{
    FidelityCohortOp, FidelityCohortRequest, FidelityOpOutcome, FidelityOpSection,
    FidelityQueryRequest, FidelityQueryResult, FidelitySnapshotEntry, FidelitySnapshotRequest,
};

use crate::enclave_client;
use crate::math::t_dec;
use crate::schema::FidelityContract;

fn chain_id_b256(storage: &StorageHandle<'_>) -> Result<B256> {
    Ok(B256::from(U256::from(storage.chain_id()?)))
}

impl FidelityContract<'_> {
    fn now(&self) -> Result<u64> {
        Ok(self.storage.timestamp()?.to::<u64>())
    }

    /// Build the cohort op section from committed storage (current blob + the
    /// plaintext global anchor), for folding into a co-located gratis op.
    pub fn cohort_section(
        &self,
        _account: Address,
        op: FidelityCohortOp,
        timestamp: u64,
    ) -> Result<FidelityOpSection> {
        Ok(FidelityOpSection {
            op,
            timestamp,
            first_qualified_start: self.first_qualified_start()?,
            current_blob: Vec::new(),
        })
    }

    /// Persist a cohort outcome (from a folded gratis op or the standalone
    /// path): store the new ciphertext and, on the account's first acquisition,
    /// anchor the global `first_qualified_start` (set-once). A probe outcome
    /// (empty blob, no init) is a no-op.
    pub fn apply_outcome(&self, _account: Address, outcome: &FidelityOpOutcome) -> Result<()> {
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
        self.storage.with_checkpoint(|| {
            // Zero-amount In/Out are no-ops (the enclave would also no-op); skip the
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
            let result = enclave_client::apply_cohort_op(&self.storage, req)?;
            self.apply_outcome(account, &result.outcome)
        })
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
                cohort_blob: Vec::new(),
            });
        }
        let req = FidelitySnapshotRequest {
            timestamp,
            first_qualified_start: self.first_qualified_start()?,
            entries,
        };
        Ok(enclave_client::snapshot_leagues(&self.storage, req)?
            .into_iter()
            .map(|e| (e.owner, e.league))
            .collect())
    }

    /// League for `account` at `timestamp` (a single-owner snapshot).
    pub fn league_at(&self, account: Address, timestamp: u64) -> Result<u16> {
        self.snapshot_leagues(timestamp, &[account])?
            .first()
            .map(|(_, league)| *league)
            .ok_or_else(|| {
                PrecompileError::Fatal("fidelity snapshot returned no league".to_string())
            })
    }

    /// League for `account` at the current block time.
    pub fn league(&self, account: Address) -> Result<u16> {
        self.league_at(account, self.now()?)
    }

    /// Synthetic-max RCFI at `timestamp`: `t_dec(timestamp - first_qualified_start)`.
    /// Pure function of the plaintext anchor - computed on-chain, no enclave.
    /// Zero before any account has qualified.
    pub fn max_rcfi_at(&self, timestamp: u64) -> Result<U256> {
        let first = self.first_qualified_start()?;
        if first == 0 {
            return Ok(U256::ZERO);
        }
        Ok(t_dec(timestamp.saturating_sub(first)))
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
            cohort_blob: Vec::new(),
            query_timestamp,
            block_timestamp: self.now()?,
            first_qualified_start: self.first_qualified_start()?,
            expiry,
            owner_sig,
        };
        enclave_client::query_index(&self.storage, req)
    }

    /// Owner-authorized RCFI/league query at the current block time.
    pub fn query_index_now(
        &self,
        account: Address,
        expiry: u64,
        owner_sig: Vec<u8>,
    ) -> Result<FidelityQueryResult> {
        self.query_index_at(account, self.now()?, expiry, owner_sig)
    }
}
