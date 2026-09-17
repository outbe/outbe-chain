//! Fidelity cohort accounting inside the unified private PledgeLedger.
//! Cohorts are journaled with account state; there is no separate blob ledger.

use crate::errors::{Result, TeeError};
use alloy_primitives::U256;
use outbe_fidelity_math::{league_from_rcfi, t_dec, RcfiAccumulator};

fn err(msg: impl Into<String>) -> TeeError {
    TeeError::Fidelity(msg.into())
}

/// Private cohort history of one account.
///
/// `active` is a LIFO stack of acquisitions `(size, acquired_at)`; `sold` an
/// append-only log `(size, acquired_at, sold_at)`. Semantics are a 1:1 port of
/// the historical on-chain `FidelityContract::cohort_in/cohort_out`.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct CohortState {
    qualified_start: u64,
    active: Vec<(U256, u64)>,
    sold: Vec<(U256, u64, u64)>,
}

impl CohortState {
    /// ACQUISITION: push a new active cohort. Returns `Some(timestamp)` when
    /// this is the account's first qualified acquisition.
    pub(crate) fn cohort_in(&mut self, amount: U256, timestamp: u64) -> Option<u64> {
        if amount.is_zero() {
            return None;
        }
        let initialized = if self.qualified_start == 0 {
            self.qualified_start = timestamp;
            Some(timestamp)
        } else {
            None
        };
        self.active.push((amount, timestamp));
        initialized
    }

    /// SALE: consume active cohorts LIFO (youngest first). The boundary cohort
    /// is split proportionally - the sold slice keeps the ORIGINAL
    /// `acquired_at`, the remainder stays active. Clamps when the stack runs
    /// out (mirrors the on-chain defensive clamp).
    pub(crate) fn cohort_out(&mut self, amount: U256, timestamp: u64) {
        let mut remaining = amount;
        while !remaining.is_zero() {
            let Some((size, acquired_at)) = self.active.last().copied() else {
                break;
            };
            if size <= remaining {
                self.active.pop();
                self.sold.push((size, acquired_at, timestamp));
                remaining -= size;
            } else {
                self.sold.push((remaining, acquired_at, timestamp));
                if let Some(last) = self.active.last_mut() {
                    last.0 = size - remaining;
                }
                remaining = U256::ZERO;
            }
        }
    }

    /// `(rcfi, efficiency, league)` at `timestamp` - the same
    /// `RcfiAccumulator` + `league_from_rcfi` pipeline the chain historically
    /// ran over plaintext cohort slots.
    fn rcfi_triple(&self, timestamp: u64) -> Result<(U256, U256, U256)> {
        let mut acc = RcfiAccumulator::default();
        for (size, acquired_at) in &self.active {
            acc.add_active(*size, *acquired_at, timestamp)
                .ok_or_else(|| err("rcfi arithmetic overflow"))?;
        }
        for (size, acquired_at, sold_at) in &self.sold {
            acc.add_sold(*size, *acquired_at, *sold_at, timestamp)
                .ok_or_else(|| err("rcfi arithmetic overflow"))?;
        }
        acc.finish(self.qualified_start, timestamp)
            .ok_or_else(|| err("rcfi arithmetic overflow"))
    }

    /// `(rcfi, efficiency, league)` at `timestamp`. `first_qualified_start = 0`
    /// means no account has qualified (league floor).
    pub(crate) fn evaluate(
        &self,
        timestamp: u64,
        first_qualified_start: u64,
    ) -> Result<(U256, U256, u16)> {
        let (rcfi, efficiency, _) = self.rcfi_triple(timestamp)?;
        let max = if first_qualified_start == 0 {
            U256::ZERO
        } else {
            t_dec(timestamp.saturating_sub(first_qualified_start))
        };
        let league =
            league_from_rcfi(rcfi, max).ok_or_else(|| err("league arithmetic overflow"))?;
        Ok((rcfi, efficiency, league))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use outbe_fidelity_math::{MAX_LEAGUE, MIN_LEAGUE};
    const DAY: u64 = 86_400;

    #[test]
    fn lifo_split_preserves_original_acquired_at() {
        let mut state = CohortState::default();
        state.cohort_in(U256::from(1_000u64), 100);
        state.cohort_in(U256::from(500u64), 200);
        // Consume 700: full-consume the youngest (500 @200), split 200 off the
        // older (1000 @100) - sold slice keeps acquired_at 100.
        state.cohort_out(U256::from(700u64), 300);
        assert_eq!(state.active, vec![(U256::from(800u64), 100)]);
        assert_eq!(
            state.sold,
            vec![
                (U256::from(500u64), 200, 300),
                (U256::from(200u64), 100, 300)
            ]
        );
        // Over-consume clamps at an empty stack, mirroring the on-chain guard.
        state.cohort_out(U256::from(10_000u64), 400);
        assert!(state.active.is_empty());
        assert_eq!(state.sold.len(), 3);
    }

    #[test]
    fn evaluate_matches_direct_accumulator() {
        let mut state = CohortState::default();
        state.cohort_in(U256::from(1_000u64), 1_000_000);
        state.cohort_in(U256::from(500u64), 1_000_000 + 100 * DAY);
        state.cohort_out(U256::from(700u64), 1_000_000 + 200 * DAY);
        let now = 1_000_000 + 400 * DAY;

        let mut acc = RcfiAccumulator::default();
        for (s, a) in &state.active {
            acc.add_active(*s, *a, now).unwrap();
        }
        for (s, a, so) in &state.sold {
            acc.add_sold(*s, *a, *so, now).unwrap();
        }
        let (rcfi, eff, _) = acc.finish(state.qualified_start, now).unwrap();
        let max = t_dec(now - 1_000_000);
        let expected_league = league_from_rcfi(rcfi, max).unwrap();

        let (got_rcfi, got_eff, got_league) = state.evaluate(now, 1_000_000).unwrap();
        assert_eq!(got_rcfi, rcfi);
        assert_eq!(got_eff, eff);
        assert_eq!(got_league, expected_league);
        assert!((MIN_LEAGUE..=MAX_LEAGUE).contains(&got_league));
    }

    /// 1e18-scaled fixed point -> f64 (via micro-units to avoid precision loss).
    fn fp_to_f64(fp: U256) -> f64 {
        let micros: u128 = (fp / U256::from(1_000_000_000_000u128)).to::<u128>();
        micros as f64 / 1_000_000.0
    }

    /// Golden replay of the PDF `reference/decay.py` scenario through the enclave
    /// `CohortState` port - the on-chain math moved here, so this is where the
    /// float-model agreement is pinned (+/-1 decayed day, +/-1e-3 efficiency). The
    /// fixture lives in the fidelity crate (regenerated from `decay.py`); we read
    /// it across the workspace rather than duplicate the generated artifact.
    #[test]
    fn golden_matches_decay_py_reference() {
        let raw = include_str!("../../../crates/core/fidelity/tests/fixtures/rcfi_golden.json");
        let v: serde_json::Value = serde_json::from_str(raw).unwrap();
        let txs: Vec<(u64, bool, U256)> = v["transactions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["ts"].as_u64().unwrap(),
                    t["kind"].as_str().unwrap() == "deposit",
                    t["amount_units"].as_str().unwrap().parse::<U256>().unwrap(),
                )
            })
            .collect();
        let samples = v["samples"].as_array().unwrap();
        assert!(!samples.is_empty());

        for s in samples {
            let ts = s["ts"].as_u64().unwrap();
            let want_rcfi = s["rcfi"].as_f64().unwrap();
            let want_eff = s["efficiency"].as_f64().unwrap();
            let want_dage = s["d_age"].as_f64().unwrap();

            // Rebuild state from every tx up to and including the sample instant,
            // mirroring the reference's `tx.date <= current_date` loop.
            let mut state = CohortState::default();
            for (t_ts, deposit, amount) in &txs {
                if *t_ts <= ts {
                    if *deposit {
                        state.cohort_in(*amount, *t_ts);
                    } else {
                        state.cohort_out(*amount, *t_ts);
                    }
                }
            }
            let (rcfi_fp, eff_fp, dage_fp) = state.rcfi_triple(ts).unwrap();
            assert!(
                (fp_to_f64(rcfi_fp) - want_rcfi).abs() <= 1.0,
                "rcfi at ts={ts}: got {}, want {want_rcfi}",
                fp_to_f64(rcfi_fp)
            );
            assert!(
                (fp_to_f64(eff_fp) - want_eff).abs() <= 1e-3,
                "efficiency at ts={ts}: got {}, want {want_eff}",
                fp_to_f64(eff_fp)
            );
            assert!(
                (fp_to_f64(dage_fp) - want_dage).abs() <= 1.0,
                "d_age at ts={ts}: got {}, want {want_dage}",
                fp_to_f64(dage_fp)
            );
        }
    }
}
