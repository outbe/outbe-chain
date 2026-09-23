use alloy_primitives::{Address, B256};
use commonware_codec::Encode;
use commonware_cryptography::bls12381::primitives::group::G2;
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::L2_REGISTRY_ADDRESS;
use outbe_primitives::error::Result;

/// Byte length of the EIP-2537 G2 public key used by all registry APIs.
pub const BLS_PUBLIC_KEY_LEN: usize = 256;

/// Registered L2 network keyed by `chain_id`.
///
/// Storage remains three compressed 32-byte words, preserving existing records.
/// Public inputs and outputs use the 256-byte EIP-2537 representation.
/// All three words zero selects live `IDaInbox(l1_address).groupPubKey()` lookup;
/// the registration still exists because `l1_address` remains nonzero.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = l1_address)]
pub struct L2NetworkRecord {
    #[key]
    pub chain_id: u64,

    /// L1 operator account managing the network, not a required Tribute submitter.
    /// Non-zero for every registered network; doubles as the existence marker.
    #[attribute(order = 0)]
    pub l1_address: Address,

    /// BLS MinSig group public key bytes 0..32.
    #[attribute(order = 1)]
    pub pubkey_lo: B256,

    /// BLS MinSig group public key bytes 32..64.
    #[attribute(order = 2)]
    pub pubkey_mid: B256,

    /// BLS MinSig group public key bytes 64..96.
    #[attribute(order = 3)]
    pub pubkey_hi: B256,
}

impl L2NetworkRecord {
    /// Returns the registered EIP-2537 key, or 256 zero bytes for inbox mode.
    pub fn public_key_bytes(&self) -> Result<[u8; BLS_PUBLIC_KEY_LEN]> {
        let compressed = self.compressed_public_key_bytes();
        if compressed == [0; 96] {
            return Ok([0; BLS_PUBLIC_KEY_LEN]);
        }
        crate::public_key::expand(&compressed)
    }

    /// Internal compressed encoding for storage and native BLS verification.
    pub(crate) fn compressed_public_key_bytes(&self) -> [u8; 96] {
        let mut out = [0u8; 96];
        out[..32].copy_from_slice(self.pubkey_lo.as_slice());
        out[32..64].copy_from_slice(self.pubkey_mid.as_slice());
        out[64..].copy_from_slice(self.pubkey_hi.as_slice());
        out
    }

    /// Splits a validated G2 point into the existing compact storage words.
    pub(crate) fn split_public_key(pubkey: &G2) -> (B256, B256, B256) {
        let pubkey = pubkey.encode();
        (
            B256::from_slice(&pubkey[..32]),
            B256::from_slice(&pubkey[32..64]),
            B256::from_slice(&pubkey[64..]),
        )
    }
}

/// EVM storage layout for the L2 network registry.
///
/// Storage slots:
///   0: networks - mapping(chain_id => L2NetworkRecord) (4 slots)
///   1: l1_to_chain - mapping(l1_address => chain_id), 0 = absent
#[storage_schema]
#[contract(addr = L2_REGISTRY_ADDRESS)]
pub struct L2RegistryContract {
    /// Registered networks keyed by chain id (non-zero for registered ids).
    #[attribute(order = 0)]
    pub networks: outbe_primitives::storage::dsl::Map<u64, L2NetworkRecord>,

    /// Reverse index: L1 operator address -> chain id. Zero means absent,
    /// which is why chain id 0 is rejected at registration.
    #[attribute(order = 1)]
    pub l1_to_chain: outbe_primitives::storage::dsl::Map<Address, u64>,
}
