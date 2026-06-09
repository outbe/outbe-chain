/// Maximum number of Poseidon inputs supported by `outbe-poseidon`'s
/// Circom-compatible parameter table.
pub const MAX_INPUTS: usize = 12;

/// Poseidon precompile base gas.
pub const POSEIDON_GAS_BASE: u64 = 1_500;

/// Poseidon precompile per-input gas (per 32-byte BN254 field element).
pub const POSEIDON_GAS_PER_INPUT: u64 = 500;

/// Flat gas cost for one UltraHonkKeccak verification. Placeholder
/// pending hardware benchmark.
pub const ZK_VERIFY_GAS: u64 = 3_000_000;
