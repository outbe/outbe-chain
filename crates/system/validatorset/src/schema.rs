use alloy_primitives::{Address, B256, U256};
use outbe_macros::contract;
use outbe_primitives::addresses::VALIDATOR_SET_ADDRESS;
use outbe_primitives::storage::types::{Mapping, Slot, StorageBytes};

/// Slot of `Staking.config_min_stake` at `STAKING_ADDRESS`.
pub(crate) const AUTHORITATIVE_MIN_STAKE_SLOT: u64 = 0;

/// Base slot of `Staking.stake_amount` at `STAKING_ADDRESS`.
/// Amounts are native COEN base units: one whole COEN is 10^18 base units.
pub(crate) const AUTHORITATIVE_BONDED_BASE_SLOT: u64 = 3;

/// EVM storage layout for the ValidatorSet precompile.
///
/// Storage slots (`0..=46`):
///   0:  config_max_validators   - u32
///   1:  config_epoch_length_blocks - u32
///   2:  config_is_initialized   - bool
///   3:  val_consensus_pubkey_lo - mapping(address => bytes32)  [BLS MinPk pubkey bytes 0..32]
///   4:  val_consensus_pubkey_hi - mapping(address => bytes32)  [BLS MinPk pubkey bytes 32..48, right-padded]
///   5:  val_status              - mapping(address => uint8)
///   6:  val_slash_count         - mapping(address => uint64)
///   7:  val_missed_blocks       - mapping(address => uint64)
///   8:  val_missed_votes        - mapping(address => uint64)
///   9:  val_blocks_proposed     - mapping(address => uint64)
///   10: val_joined_at_height    - mapping(address => uint64)
///   11: val_deactivated_at_height - mapping(address => uint64)
///   12: val_unbonding_end       - mapping(address => uint64)
///   13: address_to_index        - mapping(address => uint64)  [1-indexed]
///   14: index_to_address        - mapping(uint64 => address)
///   15: consensus_pubkey_hash_to_address - mapping(bytes32 => address)  [keccak256(48-byte pubkey)]
///   16: validator_count         - u32
///   17: epoch_number            - U256
///   18: epoch_start_timestamp   - u64
///   19: epoch_start_block       - u64
///   20: val_has_bls_share       - mapping(address => bool)
///   21: pending_set_change      - bool
///   22: active_consensus_set_hash - B256
///   23: config_reregistration_cooldown - u32
///   24: val_p2p_address_version - mapping(address => uint8)
///   25: val_p2p_address_payload - mapping(address => bytes)
///   26: finalized_participation_recorded - mapping(B256 => bool),
///       idempotency guard for `record_finalized_participation`. Keyed by
///       the finalized block hash. The first successful record sets it, so
///       replays of the same metadata-tx are no-ops.
///   27: committee_snapshot_exists                  - mapping(B256 => bool)
///       gates every snapshot read on a fully-written record. Keyed by the
///       canonical `snapshot_key`.
///   28: committee_snapshot_len                     - mapping(B256 => u64)
///       length of the ordered committee at `snapshot_key`.
///   29: committee_snapshot_address_at              - mapping(B256 => mapping(u64 => Address))
///       i-th validator address in canonical Commonware public-key order.
///   30: committee_snapshot_pubkey_lo_at            - mapping(B256 => mapping(u64 => B256))
///       i-th BLS MinPk consensus pubkey bytes 0..32.
///   31: committee_snapshot_pubkey_hi_at            - mapping(B256 => mapping(u64 => B256))
///       i-th BLS MinPk consensus pubkey bytes 32..48 right-padded with zeros.
///   32: committee_snapshot_vrf_material_version    - mapping(B256 => u64)
///       VRF material version active when this snapshot was committed.
///   33: committee_snapshot_vrf_group_public_key_hash - mapping(B256 => B256)
///       `keccak256(commonware_codec::Encode(polynomial.public()))` of the
///       VRF group public key, matching the metadata `vrf_group_public_key_hash`.
///   34: committee_snapshot_vrf_group_public_key_len  - mapping(B256 => u64)
///       length in bytes of the encoded VRF group public key (so the reader
///       can trim the last 32-byte chunk).
///   35: committee_snapshot_vrf_group_public_key_chunk_at - mapping(B256 => mapping(u64 => B256))
///       32-byte chunks of `commonware_codec::Encode(polynomial.public())`.
///       Last chunk is right-padded with zeros.
///   36: val_join_confirmed       - mapping(address => bool)
///       stale-join activation guard. `confirmValidatorReady()` sets it once a
///       PENDING joiner's node has caught up to head. It gates the joiner's
///       inclusion in the DKG reshare target.
///   37: config_unjail_cooldown_blocks - u64 (default 0)
///       blocks a JAILED validator must wait before `unjailValidator()`.
///   38: val_jailed_at_height     - mapping(address => uint64)
///       block height the validator was JAILED. Drives the unjail cooldown.
///   39: committee_snapshot_key_ring - mapping(u64 => bytes32)
///       prune ring for the committee-snapshot record.
///       That record spans slots 27..=35, 39, and 42.
///   40: finalized_participation_ring - mapping(u64 => bytes32)
///       prune ring bounding the participation guard (slot 26).
///   41: finalized_participation_ring_seq - u64
///       monotonic index counter for slot 40.
///   42: committee_snapshot_vrf_public_polynomial_hash - mapping(B256 => B256)
///   43: delegate_by_validator_role - mapping(address => mapping(uint8 => address))
///       explicit operational key selected by a validator for one protocol role.
///   44: validator_by_role_delegate - mapping(uint8 => mapping(address => address))
///       reverse role-scoped lookup used by protocol consumers and ZeroFee.
///   45: val_radicle_node_id - mapping(address => bytes32)
///       immutable Radicle transport identity bound during validator registration.
///   46: radicle_node_id_to_validator - mapping(bytes32 => address)
///       reverse uniqueness reservation for Radicle transport identities.
#[contract(addr = VALIDATOR_SET_ADDRESS)]
pub struct ValidatorSet {
    // Config (slots 0-2)
    pub config_max_validators: Slot<u32>,
    pub config_epoch_length_blocks: Slot<u32>,
    pub config_is_initialized: Slot<bool>,

    // Per-validator fields keyed by Address (slots 3-12)
    /// BLS MinPk consensus public key - low 32 bytes (bytes[0..32]).
    pub(crate) val_consensus_pubkey_lo: Mapping<Address, B256>,
    /// BLS MinPk consensus public key - high 16 bytes (bytes[32..48]), right-padded with zeros.
    pub(crate) val_consensus_pubkey_hi: Mapping<Address, B256>,
    pub(crate) val_status: Mapping<Address, u8>,
    pub(crate) val_slash_count: Mapping<Address, u64>,
    pub(crate) val_missed_blocks: Mapping<Address, u64>,
    pub(crate) val_missed_votes: Mapping<Address, u64>,
    pub(crate) val_blocks_proposed: Mapping<Address, u64>,
    pub(crate) val_joined_at_height: Mapping<Address, u64>,
    pub(crate) val_deactivated_at_height: Mapping<Address, u64>,
    pub(crate) val_unbonding_end: Mapping<Address, u64>,

    // Validator list - 1-indexed array pattern (slots 13-14)
    pub(crate) address_to_index: Mapping<Address, u64>,
    pub(crate) index_to_address: Mapping<u64, Address>,

    // Pubkey reverse lookup (slot 15) - keyed by keccak256(48-byte BLS MinPk pubkey)
    pub(crate) consensus_pubkey_hash_to_address: Mapping<B256, Address>,

    // Counters + epoch (slots 16-19)
    pub(crate) validator_count: Slot<u32>,
    pub(crate) epoch_number: Slot<U256>,
    pub(crate) epoch_start_timestamp: Slot<u64>,
    pub(crate) epoch_start_block: Slot<u64>,

    // Consensus set tracking (slots 20-22)
    pub(crate) val_has_bls_share: Mapping<Address, bool>,
    pub(crate) pending_set_change: Slot<bool>,
    pub(crate) active_consensus_set_hash: Slot<B256>,

    // Re-registration cooldown (slot 23)
    // Number of blocks a validator must wait after deactivation before re-registering.
    // 0 = no cooldown.
    pub config_reregistration_cooldown: Slot<u32>,

    // Versioned Commonware P2P address registry (slots 24-25)
    pub(crate) val_p2p_address_version: Mapping<Address, u8>,
    pub(crate) val_p2p_address_payload: Mapping<Address, StorageBytes>,

    // Per-finalized-block idempotency guard for `record_finalized_participation`
    // (slot 26). Keyed by `metadata.finalized_block_hash`. The first successful
    // record sets it, so replays do not double-increment `val_missed_votes`.
    pub finalized_participation_recorded: Mapping<B256, bool>,

    // V2 `CommitteeSnapshotStore` core columns (slots 27..=35).
    // The same record also uses slots 39 and 42.
    // Keyed by the canonical `snapshot_key =
    //   keccak256("OUTBE_COMMITTEE_SNAPSHOT_KEY_V2" || epoch_be_u64 || committee_set_hash)`.
    // Writes are field-by-field. `committee_snapshot_exists` is written LAST so
    // checkpoint-rolled-back transactions never leave a half-snapshot reachable.
    /// Slot 27 - existence flag, gates every read path on a fully-written record.
    pub committee_snapshot_exists: Mapping<B256, bool>,
    /// Slot 28 - length of the ordered committee.
    pub committee_snapshot_len: Mapping<B256, u64>,
    /// Slot 29 - `i`-th validator address in Commonware public-key order.
    pub committee_snapshot_address_at: Mapping<B256, Mapping<u64, Address>>,
    /// Slot 30 - BLS MinPk pubkey bytes 0..32 for entry `i`.
    pub committee_snapshot_pubkey_lo_at: Mapping<B256, Mapping<u64, B256>>,
    /// Slot 31 - BLS MinPk pubkey bytes 32..48, right-padded with zeros.
    pub committee_snapshot_pubkey_hi_at: Mapping<B256, Mapping<u64, B256>>,
    /// Slot 32 - `vrf_material_version` active when the snapshot was committed.
    pub committee_snapshot_vrf_material_version: Mapping<B256, u64>,
    /// Slot 33 - `keccak256(commonware_codec::Encode(polynomial.public()))`.
    pub committee_snapshot_vrf_group_public_key_hash: Mapping<B256, B256>,
    /// Slot 34 - length in bytes of the encoded VRF group public key.
    pub committee_snapshot_vrf_group_public_key_len: Mapping<B256, u64>,
    /// Slot 35 - 32-byte chunks of `commonware_codec::Encode(polynomial.public())`.
    /// Last chunk is right-padded with zeros. Readers trim using slot-34 length.
    pub committee_snapshot_vrf_group_public_key_chunk_at: Mapping<B256, Mapping<u64, B256>>,

    /// Slot 36 - stale-join activation guard. A PENDING joiner is included in
    /// the DKG reshare target (and thus activated) only after confirmation.
    /// The joiner must confirm, on-chain, that its node has caught up to head.
    /// `confirmValidatorReady()` sets it (caller = the validator itself). It is
    /// cleared when the validator leaves PENDING. Without this flag, a
    /// behind/stale joiner would be frozen into the ceremony and flipped ACTIVE
    /// before it can vote.
    pub(crate) val_join_confirmed: Mapping<Address, bool>,

    /// Slot 37 - unjail cooldown in blocks. A JAILED validator may call
    /// `unjailValidator()` only once `block_number >= val_jailed_at_height +
    /// config_unjail_cooldown_blocks`. Default 0 (read via
    /// `unjail_cooldown_blocks()`) allows immediate unjail. The mechanism exists
    /// so that genesis can configure a non-zero cooldown.
    pub config_unjail_cooldown_blocks: Slot<u64>,

    /// Slot 38 - block height at which a validator was JAILED (set by
    /// `jail_validator`). Drives the unjail cooldown check. Meaningless unless
    /// the validator is currently JAILED.
    pub(crate) val_jailed_at_height: Mapping<Address, u64>,

    /// Slot 39 - committee-snapshot prune ring (`epoch % COMMITTEE_SNAPSHOT_RETAIN_EPOCHS
    /// -> snapshot_key`). `write_committee_snapshot` pushes each new key here and
    /// clears the snapshot it evicts. This bounds the V2 snapshot record
    /// (slots 27..=35, 39, and 42) to the last
    /// `COMMITTEE_SNAPSHOT_RETAIN_EPOCHS` epochs. Without
    /// it, the store grows by one full committee per epoch forever. This is safe
    /// because every reader only touches the current finalized epoch (+/- the
    /// K-block late-finalize window). See `state::write_committee_snapshot`.
    pub committee_snapshot_key_ring: Mapping<u64, B256>,

    /// Slot 40 - finalized-participation guard prune ring (`seq %
    /// FINALIZED_PARTICIPATION_RETAIN -> fb_hash`). Bounds slot 26
    /// (`finalized_participation_recorded`, one entry per finalized block) to the
    /// last `FINALIZED_PARTICIPATION_RETAIN` finalized blocks. Each new recorded
    /// block evicts the guard flag of the block `RETAIN` records ago. This is safe
    /// because a finalized block older than the K-block late-finalize window can
    /// never be replayed.
    pub finalized_participation_ring: Mapping<u64, B256>,

    /// Slot 41 - monotonic counter for the slot-40 ring index (incremented once
    /// per distinct finalized block recorded).
    pub finalized_participation_ring_seq: Slot<u64>,

    /// Slot 42 - `keccak256(Encode(full public polynomial))` per committee
    /// snapshot, keyed by `snapshot_key` (same key as slots 27..=35).
    /// `write_committee_snapshot` writes it and `clear_committee_snapshot` zeroes
    /// it, so it is pruned in lockstep with the snapshot it belongs to. It lets
    /// SlashIndicator derive any signer's threshold pubkey to verify an
    /// invalid-seed-partial slash offense. `B256::ZERO` means no full polynomial
    /// was available.
    pub committee_snapshot_vrf_public_polynomial_hash: Mapping<B256, B256>,

    /// Slot 43 - validator-owned operational-key assignment, keyed first by the
    /// validator identity and then by a stable protocol role id.
    pub delegate_by_validator_role: Mapping<Address, Mapping<u8, Address>>,

    /// Slot 44 - reverse role-scoped operational-key lookup. The same delegate
    /// may serve different roles, but a `(role, delegate)` pair resolves to at
    /// most one validator.
    pub validator_by_role_delegate: Mapping<u8, Mapping<Address, Address>>,

    /// Slot 45 - Radicle Ed25519 NodeId bound atomically during registration.
    pub val_radicle_node_id: Mapping<Address, B256>,

    /// Slot 46 - unique validator owner of a non-zero Radicle NodeId.
    pub radicle_node_id_to_validator: Mapping<B256, Address>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use outbe_primitives::storage::{hashmap::HashMapStorageProvider, StorageHandle};

    #[test]
    fn storage_slot_layout_is_pinned() {
        let mut provider = HashMapStorageProvider::new(1);
        let c = ValidatorSet::new(StorageHandle::new(&mut provider));

        assert_eq!(c.config_max_validators.slot(), U256::from(0));
        assert_eq!(c.config_epoch_length_blocks.slot(), U256::from(1));
        assert_eq!(c.config_is_initialized.slot(), U256::from(2));
        assert_eq!(c.val_consensus_pubkey_lo.base_slot(), U256::from(3));
        assert_eq!(c.val_consensus_pubkey_hi.base_slot(), U256::from(4));
        assert_eq!(c.val_status.base_slot(), U256::from(5));
        assert_eq!(c.val_slash_count.base_slot(), U256::from(6));
        assert_eq!(c.val_missed_blocks.base_slot(), U256::from(7));
        assert_eq!(c.val_missed_votes.base_slot(), U256::from(8));
        assert_eq!(c.val_blocks_proposed.base_slot(), U256::from(9));
        assert_eq!(c.val_joined_at_height.base_slot(), U256::from(10));
        assert_eq!(c.val_deactivated_at_height.base_slot(), U256::from(11));
        assert_eq!(c.val_unbonding_end.base_slot(), U256::from(12));
        assert_eq!(c.address_to_index.base_slot(), U256::from(13));
        assert_eq!(c.index_to_address.base_slot(), U256::from(14));
        assert_eq!(
            c.consensus_pubkey_hash_to_address.base_slot(),
            U256::from(15)
        );
        assert_eq!(c.validator_count.slot(), U256::from(16));
        assert_eq!(c.epoch_number.slot(), U256::from(17));
        assert_eq!(c.epoch_start_timestamp.slot(), U256::from(18));
        assert_eq!(c.epoch_start_block.slot(), U256::from(19));
        assert_eq!(c.val_has_bls_share.base_slot(), U256::from(20));
        assert_eq!(c.pending_set_change.slot(), U256::from(21));
        assert_eq!(c.active_consensus_set_hash.slot(), U256::from(22));
        assert_eq!(c.config_reregistration_cooldown.slot(), U256::from(23));
        assert_eq!(c.val_p2p_address_version.base_slot(), U256::from(24));
        assert_eq!(c.val_p2p_address_payload.base_slot(), U256::from(25));
        assert_eq!(
            c.finalized_participation_recorded.base_slot(),
            U256::from(26)
        );
        assert_eq!(c.committee_snapshot_exists.base_slot(), U256::from(27));
        assert_eq!(c.committee_snapshot_len.base_slot(), U256::from(28));
        assert_eq!(c.committee_snapshot_address_at.base_slot(), U256::from(29));
        assert_eq!(
            c.committee_snapshot_pubkey_lo_at.base_slot(),
            U256::from(30)
        );
        assert_eq!(
            c.committee_snapshot_pubkey_hi_at.base_slot(),
            U256::from(31)
        );
        assert_eq!(
            c.committee_snapshot_vrf_material_version.base_slot(),
            U256::from(32)
        );
        assert_eq!(
            c.committee_snapshot_vrf_group_public_key_hash.base_slot(),
            U256::from(33)
        );
        assert_eq!(
            c.committee_snapshot_vrf_group_public_key_len.base_slot(),
            U256::from(34)
        );
        assert_eq!(
            c.committee_snapshot_vrf_group_public_key_chunk_at
                .base_slot(),
            U256::from(35)
        );
        assert_eq!(c.val_join_confirmed.base_slot(), U256::from(36));
        assert_eq!(c.config_unjail_cooldown_blocks.slot(), U256::from(37));
        assert_eq!(c.val_jailed_at_height.base_slot(), U256::from(38));
        assert_eq!(c.committee_snapshot_key_ring.base_slot(), U256::from(39));
        assert_eq!(c.finalized_participation_ring.base_slot(), U256::from(40));
        assert_eq!(c.finalized_participation_ring_seq.slot(), U256::from(41));
        assert_eq!(
            c.committee_snapshot_vrf_public_polynomial_hash.base_slot(),
            U256::from(42)
        );
        assert_eq!(c.delegate_by_validator_role.base_slot(), U256::from(43));
        assert_eq!(c.validator_by_role_delegate.base_slot(), U256::from(44));
        assert_eq!(c.val_radicle_node_id.base_slot(), U256::from(45));
        assert_eq!(c.radicle_node_id_to_validator.base_slot(), U256::from(46));
    }
}
