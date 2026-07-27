//! Stablecoin Factory V1 hard-fork manifest.
//!
//! This module is the protocol-lock source for Stablecoin V1 activation, bounds,
//! gas metadata and tooling ownership. Runtime crates consume these values rather
//! than copying literals. Any value change is hard-fork-equivalent and requires
//! updated vectors and ADRs.

use alloy_primitives::{uint, U256};

/// The first schema written by Stablecoin V1 Factory, Registry and token accounts.
pub const STABLECOIN_V1_SCHEMA_VERSION: u32 = 1;

/// Update protocol version `0.2`, encoded as `u8 major | u24 minor`.
pub const STABLECOIN_V1_PROTOCOL_VERSION_RAW: u32 = 2;
/// External dotted form accepted by the Update proposal payload.
pub const STABLECOIN_V1_PROTOCOL_VERSION: &str = "0.2";

/// Exact StablecoinCreate proposal bond: 1,000,000 COEN at 18 decimals.
pub const STABLECOIN_CREATE_BOND: U256 = uint!(1_000_000_000_000_000_000_000_000_U256);

/// Public bonded proposals may consume at most one quarter of Vote's 64-slot
/// global pending index, preserving 48 slots for validator governance.
pub const MAX_PENDING_PUBLIC_BONDED_PROPOSALS: u32 = 16;
/// One public bonded StablecoinCreate proposal per proposer at a time.
pub const MAX_PENDING_PUBLIC_BONDED_PROPOSALS_PER_PROPOSER: u32 = 1;
/// Maximum accounts accepted by one atomic Policy Registry add/remove call.
pub const STABLECOIN_POLICY_MEMBER_BATCH_CAP: usize = 64;

/// Stablecoin V1 EIP-712 domain version. This is intentionally independent of
/// protocol version `0.2` and schema version `1`.
pub const STABLECOIN_EIP712_DOMAIN_VERSION: &str = "1";

/// V1 ships an operator CLI, not a maintained language SDK package.
pub const STABLECOIN_V1_TOOLING_OWNER: &str = "bin/outbe-cli";
pub const STABLECOIN_V1_MAINTAINED_SDK: bool = false;

/// Semantic owners/classifications mirrored into the machine-readable manifest.
pub const STABLECOIN_V1_ACCOUNT_ACCESS_OWNER: &str = "revm";
pub const STABLECOIN_V1_GAS_FORMULA: &str =
    "dispatchBase + explicitCryptoAndHash + actualStorageOperations";
pub const STABLECOIN_V1_WALL_TIME_CLASSIFICATION: &str = "non-consensus-telemetry";

/// Conditions that require reopening G0 instead of silently moving a bound.
pub const STABLECOIN_V1_REOPEN_RULES: [&str; 4] = [
    "SCF-034-SCF-047-or-SCF-055-measurement-plus-25-percent-rounded-up-to-10000-exceeds-its-gas-ceiling",
    "nested-and-top-level-calls-do-not-produce-identical-native-charges-for-identical-work",
    "shipping-binary-cannot-activate-protocol-version-0.2",
    "supported-network-genesis-does-not-preserve-the-reserved-namespace",
];

/// Outbe-native dispatch and state accounting. The normal EVM CALL/account
/// access charge remains revm-owned and is not duplicated here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StablecoinGasSchedule {
    pub dispatch_base: u64,
    pub storage_read: u64,
    pub storage_write: u64,
    pub storage_refund: u64,
    pub native_cold_surcharge: u64,
    pub ecrecover: u64,
    pub keccak_base: u64,
    pub keccak_word: u64,
    pub dynamic_class_enumerated_as_warm: bool,
}

/// Initial Stablecoin V1 gas schedule. Storage prices intentionally match the
/// pinned Outbe stateful-precompile schedule.
pub const STABLECOIN_V1_GAS: StablecoinGasSchedule = StablecoinGasSchedule {
    dispatch_base: 200,
    storage_read: 100,
    storage_write: 5_000,
    storage_refund: 0,
    native_cold_surcharge: 0,
    ecrecover: 3_000,
    keccak_base: 30,
    keccak_word: 6,
    dynamic_class_enumerated_as_warm: false,
};

/// Deterministic gas ceilings are release gates. Wall-time values are
/// non-consensus regression telemetry for the pinned benchmark environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StablecoinBenchmarkBudgets {
    pub policy_authorization_gas: u64,
    pub policy_member_batch_gas: u64,
    pub transfer_with_policy_gas: u64,
    pub permit_gas: u64,
    pub factory_creation_gas: u64,
    pub o1_p99_micros: u64,
    pub batch_or_creation_p99_micros: u64,
    pub measurement_margin_percent: u64,
    pub gas_rounding_quantum: u64,
}

pub const STABLECOIN_V1_BENCHMARK_BUDGETS: StablecoinBenchmarkBudgets =
    StablecoinBenchmarkBudgets {
        policy_authorization_gas: 10_000,
        policy_member_batch_gas: 750_000,
        transfer_with_policy_gas: 100_000,
        permit_gas: 125_000,
        factory_creation_gas: 500_000,
        o1_p99_micros: 1_000,
        batch_or_creation_p99_micros: 5_000,
        measurement_margin_percent: 125,
        gas_rounding_quantum: 10_000,
    };

/// Applies the frozen 25% measurement margin and rounds upward to the next
/// 10,000-gas quantum. `None` makes overflow an explicit gate failure.
pub const fn stablecoin_v1_budget_for_measurement(measured_gas: u64) -> Option<u64> {
    let budgets = STABLECOIN_V1_BENCHMARK_BUDGETS;
    let scaled = match measured_gas.checked_mul(budgets.measurement_margin_percent) {
        Some(value) => value,
        None => return None,
    };
    let with_margin = match scaled.checked_add(99) {
        Some(value) => value / 100,
        None => return None,
    };
    if with_margin == 0 {
        return Some(0);
    }
    let rounded = match with_margin.checked_add(budgets.gas_rounding_quantum - 1) {
        Some(value) => value / budgets.gas_rounding_quantum,
        None => return None,
    };
    rounded.checked_mul(budgets.gas_rounding_quantum)
}

/// Stablecoin routes are active exactly when Update's canonical active version
/// reaches `0.2`. Update activation itself is begin-block inclusive.
pub const fn stablecoin_v1_is_active(active_protocol_version_raw: u32) -> bool {
    active_protocol_version_raw >= STABLECOIN_V1_PROTOCOL_VERSION_RAW
}
