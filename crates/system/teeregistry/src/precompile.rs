// The macro-generated `registerEnclave` handler takes 6 ABI args + `caller`.
#![allow(clippy::too_many_arguments)]

use alloy_primitives::{Address, B256, U256};
#[allow(unused_imports)]
use outbe_macros::{contract_dispatch, contract_public, contract_view};
use outbe_primitives::error::Result;

use crate::schema::TeeRegistry;

/// ABI surface for the TEE Registry.
///
/// The bootstrap result is written natively by the block-1 `TeeBootstrap` system
/// transaction (Phase 3b). `registerEnclave` is the one mutating method: a
/// mid-chain validator records its enclave keys on-chain (mirroring Secret
/// Network's `x/registration`). 32-byte keys are returned as `uint256` (the same
/// 32 bytes); clients reinterpret them as `bytes32`.
#[contract_dispatch]
impl TeeRegistry<'_> {
    /// Mid-chain enclave registration. The caller (a validator) records its
    /// enclave keys + measurements on-chain. On-chain attestation verification is
    /// currently a stub that accepts (see `TeeRegistry::verify_enclave_registration`);
    /// the registration mechanism is otherwise real. The attestation `quote` is not
    /// yet carried in the ABI — the `#[contract_dispatch]` macro does not support
    /// `bytes` params, and the stub does not consume it. When real DCAP verification
    /// is wired, the quote gets delivered (manual dispatch or a macro `bytes` upgrade).
    #[contract_public(
        "registerEnclave(uint256, uint256, uint256, uint256, uint256, uint16) returns (bool)"
    )]
    fn _abi_register_enclave(
        &mut self,
        caller: Address,
        recipient_x25519: U256,
        attestation_pub: U256,
        noise_static_pub: U256,
        mrenclave: U256,
        mrsigner: U256,
        isv_svn: u16,
    ) -> Result<bool> {
        self.register_enclave(
            caller,
            u256_to_b256(recipient_x25519),
            u256_to_b256(attestation_pub),
            u256_to_b256(noise_static_pub),
            u256_to_b256(mrenclave),
            u256_to_b256(mrsigner),
            isv_svn,
        )
    }

    #[contract_public("isBootstrapped() view returns (bool)")]
    #[contract_view]
    fn _abi_is_bootstrapped(&mut self) -> Result<bool> {
        self.bootstrapped.read()
    }

    #[contract_public("tributeOfferPublicKey() view returns (uint256)")]
    #[contract_view]
    fn _abi_offer_public_key(&mut self) -> Result<U256> {
        Ok(b256_to_u256(self.tribute_offer_public_key.read()?))
    }

    #[contract_public("policyHash() view returns (uint256)")]
    #[contract_view]
    fn _abi_policy_hash(&mut self) -> Result<U256> {
        Ok(b256_to_u256(self.policy_hash.read()?))
    }

    #[contract_public("keyEpoch() view returns (uint256)")]
    #[contract_view]
    fn _abi_key_epoch(&mut self) -> Result<U256> {
        Ok(U256::from(self.key_epoch.read()?))
    }

    #[contract_public("tributeOfferEpoch() view returns (uint256)")]
    #[contract_view]
    fn _abi_tribute_offer_epoch(&mut self) -> Result<U256> {
        Ok(U256::from(self.tribute_offer_epoch.read()?))
    }

    #[contract_public("registeredCount() view returns (uint256)")]
    #[contract_view]
    fn _abi_registered_count(&mut self) -> Result<U256> {
        Ok(U256::from(self.registered_count.read()?))
    }

    #[contract_public("recipientX25519(address) view returns (uint256)")]
    #[contract_view]
    fn _abi_recipient_x25519(&mut self, validator: Address) -> Result<U256> {
        Ok(b256_to_u256(self.recipient_x25519.read(&validator)?))
    }

    #[contract_public("noiseStaticPubkey(address) view returns (uint256)")]
    #[contract_view]
    fn _abi_noise_static(&mut self, validator: Address) -> Result<U256> {
        Ok(b256_to_u256(self.noise_static_pub.read(&validator)?))
    }

    #[contract_public("keysHash(address) view returns (uint256)")]
    #[contract_view]
    fn _abi_keys_hash(&mut self, validator: Address) -> Result<U256> {
        Ok(b256_to_u256(self.keys_hash.read(&validator)?))
    }
}

/// Reinterpret a 32-byte hash as a big-endian `uint256`.
fn b256_to_u256(value: B256) -> U256 {
    U256::from_be_bytes(value.0)
}

/// Reinterpret a big-endian `uint256` as a 32-byte hash.
fn u256_to_b256(value: U256) -> B256 {
    B256::from(value.to_be_bytes::<32>())
}
