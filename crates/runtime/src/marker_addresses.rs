//! Outbe runtime addresses that receive `0xEF` EIP-161 marker bytecode in every
//! block's pre-execution step (`OutbeBlockExecutor::apply_pre_execution_changes`).
//! The marker lets their persistent EVM storage survive state-root computation.
//! EIP-161 emptiness (`nonce == 0 && balance == 0 && empty code`) ignores storage;
//! without the marker, a stateful account holding only storage would be pruned.
//!
//! This list MUST contain every *stateful* runtime precompile from
//! [`crate::precompiles::outbe_exact_precompile_addresses`] (except stateless
//! verifiers and genesis-seeded accounts). It MUST also contain the system-only
//! storage markers that have no dispatch registration. The
//! `marker_list_covers_stateful_precompiles` test pins the superset invariant.

use alloy_primitives::Address;
use outbe_system_common::addresses::*;

pub const OUTBE_RUNTIME_MARKER_ADDRESSES: [Address; 35] = [
    GRATIS_ADDRESS,
    GRATIS_FACTORY_ADDRESS,
    CREDIS_ADDRESS,
    CREDIS_FACTORY_ADDRESS,
    PROMIS_ADDRESS,
    PROMIS_FACTORY_ADDRESS,
    TRIBUTE_ADDRESS,
    NOD_ADDRESS,
    NOD_FACTORY_ADDRESS,
    TRIBUTE_FACTORY_ADDRESS,
    GEM_ADDRESS,
    GEM_FACTORY_ADDRESS,
    INTEX_ADDRESS,
    INTEX_FACTORY_ADDRESS,
    DESIS_ADDRESS,
    AGENT_REWARD_ADDRESS,
    FIDELITY_ADDRESS,
    EMISSION_ADDRESS,
    METADOSIS_ADDRESS,
    PROMIS_LIMIT_ADDRESS,
    CYCLE_ADDRESS,
    CCA_REGISTRY_ADDRESS,
    VALIDATOR_SET_ADDRESS,
    SLASH_INDICATOR_ADDRESS,
    STAKING_ADDRESS,
    REWARDS_ADDRESS,
    ACCOUNTING_PROGRESS_ADDRESS,
    ORACLE_ADDRESS,
    OUTBE_SYSTEM_TX_ADDRESS,
    TEE_REGISTRY_ADDRESS,
    L2_REGISTRY_ADDRESS,
    HYPERLANE_CONTROLLER_ADDRESS,
    CROSSCHAIN_BRIDGE_ADDRESS,
    UPDATE_ADDRESS,
    VOTE_ADDRESS,
];
