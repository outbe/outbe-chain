use alloy_primitives::{Address, B256};
use outbe_macros::{contract, storage_record, storage_schema};
use outbe_primitives::addresses::L2_REGISTRY_ADDRESS;

/// Byte length of a BLS MinPk public key (G1, compressed) — the same variant
/// used for validator consensus keys.
pub const BLS_PUBLIC_KEY_LEN: usize = 48;

/// Registered L2 network keyed by `chain_id`.
///
/// The 48-byte BLS MinPk public key is chunked into two 32-byte words the same
/// way `ValidatorSet` stores consensus pubkeys: bytes 0..32 in `pubkey_lo`,
/// bytes 32..48 in `pubkey_hi` right-padded with zeros.
#[derive(Debug, Clone, PartialEq, Eq)]
#[storage_record(exists_field = l1_address)]
pub struct L2NetworkRecord {
    #[key]
    pub chain_id: u64,

    /// L1 account submitting on behalf of the network. Non-zero for every
    /// registered network (validated at registration; doubles as existence).
    #[attribute(order = 0)]
    pub l1_address: Address,

    /// BLS MinPk public key bytes 0..32.
    #[attribute(order = 1)]
    pub pubkey_lo: B256,

    /// BLS MinPk public key bytes 32..48, right-padded with zeros.
    #[attribute(order = 2)]
    pub pubkey_hi: B256,

    /// Whether ZK verification is enabled for this network.
    #[attribute(order = 3)]
    pub zk_enabled: bool,
}

impl L2NetworkRecord {
    /// Reassembles the 48-byte BLS MinPk public key from its stored chunks.
    pub fn public_key_bytes(&self) -> [u8; BLS_PUBLIC_KEY_LEN] {
        let mut out = [0u8; BLS_PUBLIC_KEY_LEN];
        out[..32].copy_from_slice(self.pubkey_lo.as_slice());
        out[32..].copy_from_slice(&self.pubkey_hi.as_slice()[..16]);
        out
    }

    /// Splits a 48-byte BLS MinPk public key into the stored chunk pair.
    pub fn split_public_key(pubkey: &[u8; BLS_PUBLIC_KEY_LEN]) -> (B256, B256) {
        let lo = B256::from_slice(&pubkey[..32]);
        let mut hi = [0u8; 32];
        hi[..16].copy_from_slice(&pubkey[32..]);
        (lo, B256::from(hi))
    }
}

/// EVM storage layout for the L2 network registry.
///
/// Storage slots:
///   0: networks — mapping(chain_id => L2NetworkRecord) (4 slots)
///   1: l1_to_chain — mapping(l1_address => chain_id), 0 = absent
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
