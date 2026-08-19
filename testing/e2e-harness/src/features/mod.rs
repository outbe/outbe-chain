//! Step definitions — the code behind the Gherkin fixtures in `features/`.
//!
//! Steps are registered with cucumber's `#[given]`/`#[when]`/`#[then]` macros
//! (collected via `inventory`), so simply compiling these modules wires them
//! up. Each step drives the `crate::world` handles and nothing else.
//!
//! - [`common`] holds the shared localnet setup + parity steps.
//! - [`update`] and [`governance`] back `features/governance.feature`.
//! - [`downtime`] / [`stale_join`] / [`lifecycle`] / [`restart`] / [`dkg`] /
//!   [`follower`] back the S1-S7 + follower validator-lifecycle scenarios.

pub mod common;
pub mod governance;
pub mod update;

#[cfg(feature = "ocomp-integration")]
pub mod contributor_payout;
pub mod dcap_onboarding;
pub mod dkg;
pub mod downtime;
pub mod follower;
pub mod l2_zk_gate;
pub mod lifecycle;
#[cfg(feature = "ocomp-integration")]
pub mod ocomp;
pub mod restart;
pub mod stablecoin;
pub mod stale_join;
pub mod tribute_projection;
pub mod zerofee;

pub mod validator_consistency_accounting;
pub mod validator_consistency_dkg;
pub mod validator_consistency_evidence;
pub mod validator_registration_consistency;
