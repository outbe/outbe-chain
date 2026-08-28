//! Protocol precompile addresses used by the harness.
//!
//! Mirrors `bin/outbe-cli/src/abi.rs`. Typed
//! `Address` consts (the `eth` layer calls contracts by `Address`, not string).

use alloy_primitives::{address, Address};

/// TEE registry precompile (`isBootstrapped()`).
pub(crate) const TEE_ADDR: Address = address!("0x000000000000000000000000000000000000EE0A");
/// ValidatorSet precompile.
pub(crate) const VS_ADDR: Address = address!("0x000000000000000000000000000000000000EE00");
/// Staking precompile.
pub(crate) const STK_ADDR: Address = address!("0x000000000000000000000000000000000000EE02");
/// SlashIndicator precompile.
pub(crate) const SLASH_ADDR: Address = address!("0x000000000000000000000000000000000000EE01");
/// Tribute precompile (`totalSupply()`).
pub(crate) const TRIBUTE_ADDR: Address = address!("0x0000000000000000000000000000000000001101");
/// Nod precompile (`totalSupply()`).
pub(crate) const NOD_ADDR: Address = address!("0x0000000000000000000000000000000000001006");
/// NodFactory precompile (`materializationHead`, `mineGratis`).
#[cfg(feature = "ocomp-integration")]
pub(crate) const NOD_FACTORY_ADDR: Address = address!("0x0000000000000000000000000000000000001007");
/// Confidential Gratis ledger (`opNonceOf`).
#[cfg(feature = "ocomp-integration")]
pub(crate) const GRATIS_ADDR: Address = address!("0x0000000000000000000000000000000000001003");
#[cfg(feature = "ocomp-integration")]
pub(crate) const GRATIS_FACTORY_ADDR: Address =
    address!("0x0000000000000000000000000000000000002003");
#[cfg(feature = "ocomp-integration")]
pub(crate) const PROMIS_ADDR: Address = address!("0x0000000000000000000000000000000000001337");
#[cfg(feature = "ocomp-integration")]
pub(crate) const PROMIS_FACTORY_ADDR: Address =
    address!("0x0000000000000000000000000000000000002337");
#[cfg(feature = "ocomp-integration")]
pub(crate) const GEM_ADDR: Address = address!("0x0000000000000000000000000000000000001013");
#[cfg(feature = "ocomp-integration")]
pub(crate) const GEM_FACTORY_ADDR: Address = address!("0x0000000000000000000000000000000000002013");
#[cfg(feature = "ocomp-integration")]
pub(crate) const VAULT_ROUTER_ADDR: Address =
    address!("0x0000000000000000000000000000000000001017");
/// PayNote shielded note pool; a Nod's cost is discharged by spending one.
#[cfg(feature = "ocomp-integration")]
pub(crate) const PAYNOTE_ADDR: Address = address!("0x0000000000000000000000000000000000001019");
/// Metadosis worldwide-day registry (`getWorldwideDay(uint32)`).
pub(crate) const WWD_ADDR: Address = address!("0x000000000000000000000000000000000000100E");
/// PromiseLimit carry-over ledger (`totalUnallocated()`).
#[cfg(feature = "ocomp-integration")]
pub(crate) const PROMIS_LIMIT_ADDR: Address =
    address!("0x000000000000000000000000000000000000100F");
/// Desis auction owner (`getAuctionStage(uint32)`).
#[cfg(feature = "ocomp-integration")]
pub(crate) const DESIS_ADDR: Address = address!("0x0000000000000000000000000000000000001016");
/// Update precompile (protocol-version governance).
pub(crate) const UPDATE_ADDR: Address = address!("0x000000000000000000000000000000000000EE0B");
#[cfg(feature = "ocomp-integration")]
pub(crate) const OCOMP_REGISTRY_ADDR: Address =
    address!("0x000000000000000000000000000000000000EE12");
/// Vote precompile (generic proposal/voting).
pub(crate) const VOTE_ADDR: Address = address!("0x000000000000000000000000000000000000EE0C");
/// EIP-7702 ZeroFee delegation target and view precompile.
pub(crate) const ZEROFEE_ADDR: Address = address!("0x000000000000000000000000000000000000EE09");
/// AgentReward target used by the canonical ZeroFee sponsored call.
pub(crate) const AGENT_REWARD_ADDR: Address =
    address!("0x000000000000000000000000000000000000100B");
/// Protocol log emitter for `OutbeFailure` soft failures.
pub(crate) const ZEROFEE_LOG_ADDR: Address = address!("0x000000000000000000000000000000000000EE06");
/// Governance precompile (canon / OIP / GIP).
pub(crate) const GOVERNANCE_ADDR: Address = address!("0x0000000000000000000000000000000000001018");
/// Governed L2 network registry view precompile and Vote target.
pub(crate) const L2_REGISTRY_ADDR: Address = address!("0x000000000000000000000000000000000000EE0E");
/// Stablecoin Factory discovery and Vote target.
pub(crate) const STABLECOIN_FACTORY_ADDR: Address =
    address!("0x000000000000000000000000000000000000EE0F");
/// Shared stablecoin policy registry.
pub(crate) const STABLECOIN_POLICY_ADDR: Address =
    address!("0x000000000000000000000000000000000000EE10");
