//! Step definitions - the code behind the Gherkin fixtures in `features/`.
//!
//! Steps are registered with cucumber's `#[given]`/`#[when]`/`#[then]` macros
//! (collected via `inventory`), so compiling these modules is enough to register
//! them. Each step drives the `crate::world` handles and nothing else.
//!
//! - [`common`] holds the shared localnet setup + parity steps.
//! - [`update`] and [`governance`] back `features/governance.feature`.
//! - [`downtime`] / [`stale_join`] / [`lifecycle`] / [`restart`] / [`dkg`] /
//!   [`follower`] back the S1-S7 + follower validator-lifecycle scenarios.

pub mod agent_reward;

mod auction_expectations;
mod byzantine_preannounce;
pub mod common;

pub mod credis;

mod credis_forfeit;

pub mod entity_lifecycle;

pub mod gem_lifecycle;
pub mod governance;

pub mod hyperlane_controller;

pub mod intex_lifecycle;

pub mod nod_lifecycle;

mod tribute_expectations;
pub mod update;

pub mod consensus_resilience;
pub mod dcap_onboarding;
pub mod dkg;
pub mod downtime;

mod encrypted_nod;

mod encrypted_tribute;
pub mod follower;
mod follower_handoff;

mod follower_snapshot;
mod l2_registration;
pub mod l2_zk_gate;
pub mod lifecycle;

mod lysis;
mod negative_assertions;

mod oracle_expectations;
pub mod origin_venue;
mod p2p_admission;
pub mod price_oracle;

pub mod radicle;
pub mod restart;

pub mod settlement;
pub mod stablecoin;
pub mod stale_join;
pub mod target_chain;
pub mod tee_lease;
pub mod tee_observability;
mod tribute_negatives;
pub mod tribute_projection;
pub mod txpool_eviction;
mod txpool_pending;
pub mod zerofee;

pub mod validator_consistency_accounting;
pub mod validator_consistency_dkg;
pub mod validator_consistency_evidence;
pub mod validator_registration_consistency;
