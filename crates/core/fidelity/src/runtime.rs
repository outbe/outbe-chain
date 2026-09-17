//! Fidelity cohorts live inside the same resident ledger as Gratis.
use crate::{math::t_dec, schema::FidelityContract};
use alloy_primitives::{Address, U256};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_tee::{pledge_ledger, pledgenote::Command, protocol::FidelityCohortOp};
impl FidelityContract<'_> {
    fn now(&self) -> Result<u64> {
        self.storage
            .timestamp()?
            .try_into()
            .map_err(|_| PrecompileError::Fatal("invalid Fidelity timestamp".into()))
    }
    pub fn cohort_in(&self, account: Address, amount: U256, timestamp: u64) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        pledge_ledger::execute(
            &self.storage,
            Command::Cohort {
                account,
                amount,
                op: FidelityCohortOp::In,
                timestamp,
            },
        )
        .map(|_| ())
    }
    pub fn cohort_out(&self, account: Address, amount: U256, timestamp: u64) -> Result<()> {
        if amount.is_zero() {
            return Ok(());
        }
        pledge_ledger::execute(
            &self.storage,
            Command::Cohort {
                account,
                amount,
                op: FidelityCohortOp::Out,
                timestamp,
            },
        )
        .map(|_| ())
    }
    pub fn snapshot_leagues(
        &self,
        timestamp: u64,
        owners: &[Address],
    ) -> Result<Vec<(Address, u16)>> {
        if owners.is_empty() {
            return Ok(Vec::new());
        }
        if pledge_ledger::head(&self.storage)?.sequence == 0 {
            return Ok(owners
                .iter()
                .map(|owner| (*owner, crate::math::MIN_LEAGUE))
                .collect());
        }
        let mut leagues = Vec::with_capacity(owners.len());
        for owners in owners.chunks(outbe_tee::pledgenote::SNAPSHOT_BATCH_OWNERS) {
            let outcome = pledge_ledger::execute(
                &self.storage,
                Command::Snapshot {
                    owners: owners.to_vec(),
                    timestamp,
                },
            )?;
            if outcome.leagues.len() != owners.len()
                || outcome
                    .leagues
                    .iter()
                    .zip(owners)
                    .any(|((owner, _), expected)| owner != expected)
            {
                return Err(PrecompileError::Fatal(
                    "invalid Fidelity snapshot owners".into(),
                ));
            }
            leagues.extend(outcome.leagues);
        }
        Ok(leagues)
    }
    pub fn query(&self, encrypted_request: Vec<u8>) -> Result<Vec<u8>> {
        Ok(pledge_ledger::execute(
            &self.storage,
            Command::Query {
                envelope: encrypted_request,
            },
        )?
        .encrypted_receipt)
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
}
