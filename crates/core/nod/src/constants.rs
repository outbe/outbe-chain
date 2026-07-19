/// Legacy ERC-721 metadata surface values.
pub const TOKEN_NAME: &str = "Nod";
pub const TOKEN_SYMBOL: &str = "NOD";
pub const TOKEN_DESCRIPTION: &str = "Outbe Nod";
pub const TOKEN_IMAGE_BASE: &str = "https://api.outbe.io/nod/image/";

/// Per-bin multiplicative step in basis points. PancakeSwap LB default; each
/// bin spans a 0.25% price band. The LB-protocol constants used alongside
/// this value (`SCALE`, `SCALE_OFFSET`, `PRECISION`, `BASIS_POINT_MAX`,
/// `REAL_ID_SHIFT`, `MAX_BIN_ID`) live in `outbe_primitives::math::constants`.
pub const BIN_STEP_BP: u16 = 25;

/// Maximum number of off-chain bucket bodies inspected by the consensus
/// begin-block qualifier. Remaining work stays in the compact EVM worklist
/// and is resumed deterministically in the next block.
pub const MAX_BUCKET_QUALIFICATIONS_PER_BLOCK: u32 = 256;
