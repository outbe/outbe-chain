//! Cross-module surface: ZK merkle-root signature verification for
//! `TributeFactory.offerTribute`.

use commonware_codec::DecodeExt;
use commonware_cryptography::bls12381::primitives::{
    group::G1, ops::verify_message, variant::MinSig,
};
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;
use outbe_zk_canonical::L2CircuitVersion;

use crate::errors::L2RegistryError;
use crate::runtime::decode_public_key;
use crate::schema::L2RegistryContract;

/// Domain-separation namespace for L2 signatures over `zkMerkleRoot`.
///
/// Must match the L2 committee's root-certificate signing namespace.
pub const ZK_MERKLE_ROOT_NAMESPACE: &[u8] = b"_PSO_CHAIN_COMMITMENT_ROOT";

/// Outcome of the offer-time ZK signature check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZkOfferCheck {
    /// The selected L2 chain is not registered.
    NotRegistered,
    /// The signature over `zkMerkleRoot` verified against the network key.
    Verified { chain_id: u64 },
}

/// Verifies `signature` over `zk_merkle_root` for `l2_chain_id`.
///
/// - Chain not registered: [`ZkOfferCheck::NotRegistered`].
/// - Registered chain: `zk_merkle_root` must be 32 bytes and `signature`
///   must be a valid BLS MinSig G1 signature over it under
///   [`ZK_MERKLE_ROOT_NAMESPACE`]; any failure reverts.
pub fn check_zk_merkle_root_signature(
    storage: StorageHandle<'_>,
    l2_chain_id: u64,
    zk_merkle_root: &[u8],
    signature: &[u8],
) -> Result<ZkOfferCheck> {
    let registry = L2RegistryContract::new(storage);
    let Some(record) = registry.networks.get(l2_chain_id)? else {
        return Ok(ZkOfferCheck::NotRegistered);
    };
    let chain_id = record.chain_id;
    if zk_merkle_root.len() != 32 {
        return Err(L2RegistryError::ZkMerkleRootRequired.into());
    }

    let pubkey = decode_public_key(&record.public_key_bytes())?;
    let sig = G1::decode(signature).map_err(|_| L2RegistryError::InvalidZkSignature)?;
    verify_message::<MinSig>(&pubkey, ZK_MERKLE_ROOT_NAMESPACE, zk_merkle_root, &sig)
        .map_err(|_| L2RegistryError::InvalidZkSignature)?;
    Ok(ZkOfferCheck::Verified { chain_id })
}

/// Exact deployment bindings; Devnet's extra fixture L2s reuse chain 57005.
///
/// Basic fixtures use the declared 57005 binding directly. Additional L2s
/// retain real signature and proof verification, and never gain bindings on
/// non-development host chains.
pub fn l2_circuits(host_chain_id: u64, l2_chain_id: u64) -> &'static [L2CircuitVersion] {
    let declared = outbe_zk_canonical::l2_circuits(l2_chain_id);
    if declared.is_empty() && outbe_primitives::chain::is_devnet(host_chain_id) && l2_chain_id != 0
    {
        outbe_zk_canonical::l2_circuits(57_005)
    } else {
        declared
    }
}
