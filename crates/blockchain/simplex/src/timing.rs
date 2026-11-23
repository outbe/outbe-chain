//! Consensus block-timing defaults and the gas <-> consensus-timeout contract.
//!
//! This module is the single source of *default* values for the two
//! consensus-sync timing knobs. The live values come from `genesis.json`
//! (`config.leaderTimeoutMs` / `config.certificationTimeoutMs`). Each value
//! falls back to the default here when absent. There is **no CLI flag** for
//! either of the two: the only two sources of truth are these Rust defaults and
//! `genesis.json`. A third (per-node CLI) source would let operators desync
//! their timings and fork the network. For this reason, it is deliberately not
//! offered.
//!
//! # The model (execute-then-agree)
//!
//! Every block's transactions run through EVM (evm2) **twice, serially**: once on the
//! proposer (build) and once on each validator (verify, on the critical path to
//! the vote). The proposal is a fully executed, sealed block. Its hash binds the
//! post-execution state root. Thus a validator cannot start verifying until it
//! receives the proposer's executed block. Simplex arms `leader_deadline` and
//! `certification_deadline` from the same view-entry instant `t0`.
//!
//! There is **no separate EVM (evm2) execution timeout**. Two limits that already exist
//! bound EVM (evm2):
//!
//! - `leader_timeout` bounds it from above. The proposer self-nullifies if it
//!   overruns its leader window.
//! - `block_gas_limit` bounds it in volume.
//!
//! A wall-clock kill on EVM (evm2) is pointless on the proposer, because the leader
//! timeout already bounds it. It is forbidden on validators, because a per-node
//! wall-clock verdict on the same block would split consensus.
//!
//! # Gas <-> consensus-timeout formula
//!
//! The certification window splits into two named sub-windows:
//!
//! - **Max block-*creation* time = `leaderTimeoutMs`** - the proposer's window to
//!   build and deliver the proposal. The leader self-nullifies (forfeits its
//!   slot) if it overruns. Nothing else bounds build time.
//! - **Max block-*validation* time = `certificationTimeoutMs - leaderTimeoutMs`**
//!   what remains of the certification window after proposal delivery, for
//!   every validator's `new_payload` re-execution **plus** the 2f+1 vote round.
//!
//! **Calibration rule (the single dial):** size `gasLimit` against a *full*
//! (gas-saturated) block on the *slowest* validator. That block's `new_payload`,
//! plus the vote round, must fit strictly inside the validation window:
//!
//! ```text
//! full_block_exec_time(gasLimit, slowest_validator) + vote_round
//!     <  certificationTimeoutMs - leaderTimeoutMs
//! ```
//!
//! With the defaults below (`leader 4000`, `cert 8000`) the validation window is
//! 4000 ms. Targeting full-block exec <= ~2 s leaves ~2 s for votes + margin.
//! `gasLimit` is the *only* knob. Turn it, then re-derive `leader`/`cert` to fit.
//! ZeroFee admission means resizing `gasLimit` has no fee-market side effect.
//!
//! # Startup invariants
//!
//! The genesis reader enforces these at node start (structured error, no panic):
//! `0 < leaderTimeoutMs <= certificationTimeoutMs`.

/// Default Simplex leader (proposal) timeout, in milliseconds.
///
/// The proposer must build and deliver its proposal within this window or it
/// self-nullifies. Also the max block-*creation* time in the gas<->timeout formula.
pub const DEFAULT_LEADER_TIMEOUT_MS: u64 = 4000;

/// Default Simplex certification (notarization) timeout, in milliseconds.
///
/// Spans leader delivery + validator re-execution + the 2f+1 vote round. The
/// validation window is
/// `DEFAULT_CERTIFICATION_TIMEOUT_MS - DEFAULT_LEADER_TIMEOUT_MS`.
pub const DEFAULT_CERTIFICATION_TIMEOUT_MS: u64 = 8000;

/// Default payload warm-up before the first `resolve_kind`, in milliseconds.
///
/// Mirrors the existing payload-resolve preparation window. It stays here so
/// all timing defaults live together.
pub const DEFAULT_PAYLOAD_WARMUP_MS: u64 = 200;
