//! Pure enclave business logic and cryptography, independent of the sidecar daemon.

pub mod compute;
pub mod confidential;
pub mod crypto;
pub mod dev;
pub mod encrypted_tribute_offer;
pub mod errors;
pub mod fidelity;
pub mod fidelity_cipher;
pub mod gratis;
pub mod gratis_cipher;
pub mod lysis;
pub mod nod_encryption;
pub mod nod_mine;
pub mod payload;
pub mod process;
pub mod promis;
pub mod promis_cipher;
pub mod tribute_day;
pub mod tribute_encryption;
pub mod zk_claim;

#[cfg(test)]
mod lysis_fixture;
