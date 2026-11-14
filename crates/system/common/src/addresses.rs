use alloy_primitives::{address, Address};

pub use outbe_primitives::addresses::*;

/// Gratis token precompile address.
pub const GRATIS_ADDRESS: Address = address!("0x0000000000000000000000000000000000001003");

/// Gratis factory precompile address.
pub const GRATIS_FACTORY_ADDRESS: Address = address!("0x0000000000000000000000000000000000002003");

/// Promis token precompile address.
pub const PROMIS_ADDRESS: Address = address!("0x0000000000000000000000000000000000001337");

/// Promis factory precompile address (orchestrator: mint/burn orchestration via
/// cross-module API, `mineCoen` on the ABI). Wraps the Promis token at
/// [`PROMIS_ADDRESS`], records Fidelity cohorts, and mints native COEN on
/// `mineCoen`. Carries no persistent storage of its own.
pub const PROMIS_FACTORY_ADDRESS: Address = address!("0x0000000000000000000000000000000000002337");

/// Tribute NFT precompile address.
pub const TRIBUTE_ADDRESS: Address = address!("0x0000000000000000000000000000000000001101");

/// Nod NFT precompile address.
pub const NOD_ADDRESS: Address = address!("0x0000000000000000000000000000000000001006");

/// Nod factory precompile address (orchestrator: issuance via cross-module
/// API, `mineGratis` ABI). State lives in the Nod entity store at
/// [`NOD_ADDRESS`]. NodFactory carries no persistent storage of its own.
pub const NOD_FACTORY_ADDRESS: Address = address!("0x0000000000000000000000000000000000001007");

/// Gem NFT precompile address. ERC-721 non-transferable registry.
pub const GEM_ADDRESS: Address = address!("0x0000000000000000000000000000000000001013");

/// Intex address. Canonical, cross-chain Intex series ledger
/// (identity + lifecycle). Writes are Rust-to-Rust only (IntexFactory). The
/// precompile at this address dispatches read-only series views for off-chain
/// observability.
pub const INTEX_ADDRESS: Address = address!("0x0000000000000000000000000000000000001014");

/// IntexFactory address. Drives Intex issuance, the autonomous
/// Issued->Called lifecycle, and two-step settlement. Series state is
/// written to [`INTEX_ADDRESS`].
pub const INTEX_FACTORY_ADDRESS: Address = address!("0x0000000000000000000000000000000000001015");

/// Gem factory precompile address (orchestrator: `issue_gem` via cross-module
/// API, `settleGem` / `minePromis` on the ABI). Per-Gem state lives at
/// [`GEM_ADDRESS`]. GemFactory carries module-stats storage.
pub const GEM_FACTORY_ADDRESS: Address = address!("0x0000000000000000000000000000000000002013");

/// Credis precompile address.
pub const CREDIS_ADDRESS: Address = address!("0x000000000000000000000000000000000000100A");

/// Credis factory precompile address.
pub const CREDIS_FACTORY_ADDRESS: Address = address!("0x0000000000000000000000000000000000001009");

/// Tribute factory precompile address.
pub const TRIBUTE_FACTORY_ADDRESS: Address = address!("0x0000000000000000000000000000000000001100");

/// Agent reward precompile address.
pub const AGENT_REWARD_ADDRESS: Address = address!("0x000000000000000000000000000000000000100B");

/// Fidelity precompile address.
pub const FIDELITY_ADDRESS: Address = address!("0x000000000000000000000000000000000000100C");

/// Emission precompile address (`IEmission`).
pub const EMISSION_ADDRESS: Address = address!("0x000000000000000000000000000000000000100D");

/// Legacy alias for [`EMISSION_ADDRESS`].
pub const EMISSION_LIMIT_ADDRESS: Address = EMISSION_ADDRESS;

/// Metadosis precompile address. Its registered dispatch serves public view
/// methods (for example `getWorldwideDay`) to any caller. It also routes the
/// `submitLysisResult` result-vote selector.
pub const METADOSIS_ADDRESS: Address = address!("0x000000000000000000000000000000000000100E");

/// Promis limit precompile address. Its registered dispatch serves the public
/// view method `promisLimit` to any caller.
pub const PROMIS_LIMIT_ADDRESS: Address = address!("0x000000000000000000000000000000000000100F");

/// Cycle precompile address (system-only). Hosts the storage of the
/// code-defined trigger registry (`outbe_cycle::schema::Cycle`). The hourly
/// `ProcessEmissionDay` trigger runs `outbe_cycle::handler::process_emission_day`.
pub const CYCLE_ADDRESS: Address = address!("0x0000000000000000000000000000000000001010");

/// Checkout Credis Agent registry precompile. Holds agent bonds and daily CCA
/// emission rewards, and exposes agent standing (`ICcaRegistry.getCcaState`).
/// See `outbe_ccaregistry::precompile`.
pub const CCA_REGISTRY_ADDRESS: Address = address!("0x0000000000000000000000000000000000001011");

/// Desis precompile address. Runs the Intex auction and clearing engine
/// (bid ingestion, reveal, clearing, issuance handoff to IntexFactory).
pub const DESIS_ADDRESS: Address = address!("0x0000000000000000000000000000000000001016");

/// VaultRouter precompile address. Reserve liquidity router: registers
/// ERC-4626 vaults per asset.
pub const VAULT_ROUTER_ADDRESS: Address = address!("0x0000000000000000000000000000000000001017");

// 0x0000000000000000000000000000000000001018 is retired; never reassign it.
// Cutover requires a new genesis or no pending Vote proposals targeting it.

// ---------------------------------------------------------------------------
// System & infrastructure domain precompiles (0xEE00 range)
// ---------------------------------------------------------------------------

/// Slash indicator precompile address.
pub const SLASH_INDICATOR_ADDRESS: Address = address!("0x000000000000000000000000000000000000EE01");

/// Staking precompile address.
pub const STAKING_ADDRESS: Address = address!("0x000000000000000000000000000000000000EE02");

/// Accounting progress marker address.
///
/// Holds persistent V2 Phase 1 accounting progress (slot 0 =
/// `last_accounted_block_number: u64`). System-only: there is **no** EVM
/// precompile dispatch registered at this address, so user transactions
/// CALLing it execute as no-op accounts. Only the V2 executor Phase 1 path
/// may write slot 0. The runtime EIP-161 marker-bytecode allowlist includes
/// the address so the slot survives state-root computation across all blocks.
pub const ACCOUNTING_PROGRESS_ADDRESS: Address =
    address!("0x000000000000000000000000000000000000EE04");

/// Oracle precompile address.
pub const ORACLE_ADDRESS: Address = address!("0x000000000000000000000000000000000000EE05");

/// UltraHonkKeccak proof verifier precompile address (stateless).
pub const ZKPROOF_VERIFY_ADDRESS: Address = address!("0x000000000000000000000000000000000000EE08");

/// Generic on-chain vote precompile address.
pub const VOTE_ADDRESS: Address = address!("0x000000000000000000000000000000000000EE0C");

/// L2 network registry precompile (storage-backed).
pub const L2_REGISTRY_ADDRESS: Address = address!("0x000000000000000000000000000000000000EE0E");

/// Stablecoin Factory precompile address.
pub const STABLECOIN_FACTORY_ADDRESS: Address =
    address!("0x000000000000000000000000000000000000EE0F");

/// Shared stablecoin Policy Registry precompile address.
pub const STABLECOIN_POLICY_REGISTRY_ADDRESS: Address =
    address!("0x000000000000000000000000000000000000EE10");

/// HyperlaneController precompile address (stateful).
pub const HYPERLANE_CONTROLLER_ADDRESS: Address =
    address!("0x000000000000000000000000000000000000EE14");

/// CrosschainBridge precompile address (stateful).
pub const CROSSCHAIN_BRIDGE_ADDRESS: Address =
    address!("0x000000000000000000000000000000000000EE15");

/// Genesis-reserved two-byte class for dynamic stablecoin token addresses.
pub const STABLECOIN_ADDRESS_PREFIX: [u8; 2] = RESERVED_DYNAMIC_PRECOMPILE_PREFIX;

/// Exact marker bytecode installed at stablecoin fixed and dynamic precompile accounts.
pub const STABLECOIN_MARKER_CODE: [u8; 1] = [0xef];

/// Returns whether an address belongs to the reserved dynamic stablecoin class.
#[must_use]
pub const fn is_stablecoin_address(address: Address) -> bool {
    is_reserved_dynamic_precompile_address(address)
}
