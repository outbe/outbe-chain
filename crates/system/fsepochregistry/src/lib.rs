//! Native precompile for the PSO FsEpochRegistry (`0x5200000000000000000000000000000000000003`).

pub mod bls;
pub mod errors;
pub mod precompile;
pub mod runtime;
pub mod schema;

pub use bls::{
    decode_eip2537_g1, decode_eip2537_g2, encode_eip2537_g1, encode_eip2537_g2,
    verify_eip2537_min_sig, EIP2537_G1_LEN, EIP2537_G2_LEN,
};
pub use errors::FsEpochRegistryError;
pub use precompile::IFsEpochRegistry;
pub use runtime::{digest_of, EpochKey, FsEpochHead};
pub use schema::FsEpochRegistryContract;

#[cfg(test)]
mod tests;
