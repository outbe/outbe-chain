use std::num::NonZeroU64;

use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::consensus_p2p::{
    decode_versioned, MAX_P2P_ADDRESS_ENCODED_LEN, P2P_ADDRESS_VERSION_V1,
};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::slashing_journal::{iso8601_now, record as journal_record, JournalRecord};
use tracing::{info, warn};

use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;
use crate::state_machine::{
    ActiveState, P2pInfo, PendingState, StakeProjection, ValidatorHistory, ValidatorLifecycle,
    ValidatorState,
};

/// Validator status constants.
///
/// Lifecycle: Registered → Pending → Active → Exiting → Unbonding → Inactive.
///
/// JAILED branches off the active path on a consensus/oracle felony: instead of
/// being force-exited out of the registry, the validator is slashed and frozen in
/// JAILED. It keeps its current-epoch consensus accountability until the next
/// reshare drops it (same as EXITING — a member cannot leave a threshold committee
/// mid-epoch), then it stops voting. From JAILED there are two exits:
///   - return: `unjailValidator()` (self, stake ≥ min_stake, cooldown) → PENDING →
///     (confirm-ready + reshare) → ACTIVE;
///   - leave: unstake the full stake → EXITING → UNBONDING → INACTIVE.
pub mod status {
    pub const REGISTERED: u8 = 0;
    pub const PENDING: u8 = 1;
    pub const ACTIVE: u8 = 2;
    pub const EXITING: u8 = 3;
    pub const UNBONDING: u8 = 4;
    pub const INACTIVE: u8 = 5;
    pub const JAILED: u8 = 6;
}

/// maximum number of validators that may be in the `REGISTERED`
/// (self-registered, not-yet-staked) state at once.
///
/// `REGISTERED` self-registration is permissionless and free on the ZeroFee
/// chain, and a `REGISTERED` node is intentionally admitted to the consensus
/// P2P secondary tier so a TEE verifier full-node can sync and execute offer
/// blocks before staking (see
/// [`ValidatorSet::get_admitted_non_consensus_validators`]). That admission is
/// by design, but without a bound an attacker can self-register up to
/// `config_max_validators` free Sybil identities — consuming registration slots
/// (griefing legitimate staked joins with "max validators reached") and
/// consensus-P2P connection / handshake / decode slots. This caps the unstaked
/// self-registration surface well below `config_max_validators` (default 128),
/// so legitimate verifiers (few) still register while Sybils cannot fill the
/// validator set. The owner (`config_owner`) is NOT subject to this cap and may
/// register validators directly beyond it.
pub const MAX_SELF_REGISTERED_UNSTAKED: u32 = 32;

/// Legacy flat read/ABI projection.
///
/// Lifecycle decisions must use [`ValidatorState`] or [`ValidatorLifecycle`].
/// This shape remains public for compatibility with existing Rust consumers
/// and the Solidity `validatorByAddress` / `validatorByIndex` tuples.
#[derive(Debug, Clone, PartialEq)]
pub struct ValidatorRecord {
    pub validator_address: Address,
    /// 48-byte BLS MinPk consensus public key.
    pub consensus_pubkey: [u8; 48],
    pub stake: U256,
    pub status: u8,
    pub slash_count: u64,
    pub missed_blocks: u64,
    pub missed_votes: u64,
    pub blocks_proposed: u64,
    pub joined_at_height: u64,
    pub deactivated_at_height: u64,
    pub unbonding_end: u64,
    pub has_bls_share: bool,
}

/// Read-only epoch metadata exposed without leaking raw storage slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EpochSnapshot {
    pub number: U256,
    pub start_timestamp: u64,
    pub start_block: u64,
    pub length_blocks: u32,
}

/// Read-only participation counters exposed independently of lifecycle writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValidatorParticipation {
    pub blocks_proposed: u64,
    pub missed_blocks: u64,
    pub missed_votes: u64,
}

fn registered_status(lifecycle: &ValidatorLifecycle) -> Result<u8> {
    lifecycle.stored_status().ok_or_else(|| {
        PrecompileError::Fatal("Unregistered lifecycle has no persisted status".into())
    })
}

impl ValidatorSet<'_> {
    /// Reads the 48-byte BLS MinPk consensus pubkey from two storage slots.
    fn read_consensus_pubkey(&self, addr: &Address) -> Result<[u8; 48]> {
        let lo: B256 = self.val_consensus_pubkey_lo.read(addr)?;
        let hi: B256 = self.val_consensus_pubkey_hi.read(addr)?;
        let mut pubkey = [0u8; 48];
        pubkey[..32].copy_from_slice(&lo.0);
        pubkey[32..48].copy_from_slice(&hi.0[..16]);
        Ok(pubkey)
    }

    /// Writes the 48-byte BLS MinPk consensus pubkey across two storage slots.
    fn write_consensus_pubkey(&mut self, addr: &Address, pubkey: &[u8; 48]) -> Result<()> {
        let lo = B256::from_slice(&pubkey[..32]);
        let mut hi_bytes = [0u8; 32];
        hi_bytes[..16].copy_from_slice(&pubkey[32..48]);
        let hi = B256::from(hi_bytes);
        self.val_consensus_pubkey_lo.write(addr, lo)?;
        self.val_consensus_pubkey_hi.write(addr, hi)?;
        Ok(())
    }

    /// Returns the complete typed state for an address.
    ///
    /// Unlike [`Self::get_validator`], this also represents an address that has
    /// staked before registration as [`ValidatorLifecycle::Unregistered`] with a
    /// non-zero [`StakeProjection`]. Unknown status bytes and malformed coupled
    /// storage fail closed.
    pub fn validator_state(&self, addr: Address) -> Result<ValidatorState> {
        let registry_index = self.address_to_index.read(&addr)?;
        let stored_status = self.val_status.read(&addr)?;

        let consensus_pubkey = self.read_consensus_pubkey(&addr)?;
        let bonded = self.val_stake.read(&addr)?;
        let unbonding_end = self.val_unbonding_end.read(&addr)?;
        let stake = StakeProjection::new(bonded, (unbonding_end != 0).then_some(unbonding_end));
        let slash_count = self.val_slash_count.read(&addr)?;
        let missed_blocks = self.val_missed_blocks.read(&addr)?;
        let missed_votes = self.val_missed_votes.read(&addr)?;
        let blocks_proposed = self.val_blocks_proposed.read(&addr)?;
        let joined_at_height = self.val_joined_at_height.read(&addr)?;
        let deactivated_at_height = self.val_deactivated_at_height.read(&addr)?;
        let has_bls_share = self.val_has_bls_share.read(&addr)?;
        let join_confirmed = self.val_join_confirmed.read(&addr)?;
        let jailed_at = self.val_jailed_at_height.read(&addr)?;
        let p2p_version = self.val_p2p_address_version.read(&addr)?;
        let p2p_payload = self.val_p2p_address_payload.get_bytes(&addr).read()?;

        let Some(registry_index) = NonZeroU64::new(registry_index) else {
            let metadata_is_zero = consensus_pubkey == [0; 48]
                && stored_status == status::REGISTERED
                && slash_count == 0
                && missed_blocks == 0
                && missed_votes == 0
                && blocks_proposed == 0
                && joined_at_height == 0
                && deactivated_at_height == 0
                && !has_bls_share
                && !join_confirmed
                && jailed_at == 0
                && p2p_version == 0
                && p2p_payload.is_empty();
            if !metadata_is_zero {
                return Err(PrecompileError::Fatal(format!(
                    "corrupt validator state for {addr}: unregistered address has non-zero validator metadata"
                )));
            }
            return ValidatorState::hydrate_unregistered(addr, stake);
        };

        ValidatorLifecycle::validate_stored_status(stored_status).map_err(|_| {
            PrecompileError::Fatal(format!(
                "corrupt validator state for {addr}: unknown validator status {stored_status}"
            ))
        })?;

        let lifecycle = ValidatorLifecycle::decode_stored(
            stored_status,
            has_bls_share,
            join_confirmed,
            jailed_at,
        )?;
        let p2p = P2pInfo::decode_stored(addr, p2p_version, &p2p_payload)?;
        let history = ValidatorHistory::new(
            joined_at_height,
            (deactivated_at_height != 0).then_some(deactivated_at_height),
            slash_count,
            missed_blocks,
            missed_votes,
            blocks_proposed,
        );
        ValidatorState::hydrate_registered(
            addr,
            registry_index,
            consensus_pubkey,
            stake,
            lifecycle,
            p2p,
            history,
        )
    }

    /// Reads only the coupled lifecycle slots, avoiding identity/history/P2P
    /// hydration on hot authorization and consensus-membership paths.
    pub fn validator_lifecycle(&self, addr: Address) -> Result<ValidatorLifecycle> {
        let registry_index = self.address_to_index.read(&addr)?;
        if registry_index == 0 {
            return Ok(ValidatorLifecycle::Unregistered);
        }
        let stored_status = self.val_status.read(&addr)?;
        ValidatorLifecycle::validate_stored_status(stored_status)?;
        let has_share = self.val_has_bls_share.read(&addr)?;
        let join_confirmed = self.val_join_confirmed.read(&addr)?;
        let jailed_at = self.val_jailed_at_height.read(&addr)?;
        ValidatorLifecycle::decode_stored(stored_status, has_share, join_confirmed, jailed_at)
    }

    /// Writes only changed fields of an already-decoded validator aggregate.
    /// Registry identity is deliberately excluded: registration, re-registration,
    /// and cleanup own the dense-index and consensus-key invariants.
    pub(crate) fn persist_validator_state_delta(
        &mut self,
        before: &ValidatorState,
        after: &ValidatorState,
    ) -> Result<()> {
        self.persist_validator_state_delta_inner(before, after, false)
    }

    fn persist_registry_state_delta(
        &mut self,
        before: &ValidatorState,
        after: &ValidatorState,
    ) -> Result<()> {
        self.persist_validator_state_delta_inner(before, after, true)
    }

    fn persist_validator_state_delta_inner(
        &mut self,
        before: &ValidatorState,
        after: &ValidatorState,
        allow_registry_identity_change: bool,
    ) -> Result<()> {
        before.validate()?;
        after.validate()?;
        if before.address() != after.address()
            || (!allow_registry_identity_change
                && (before.registry_index() != after.registry_index()
                    || before.consensus_pubkey() != after.consensus_pubkey()))
        {
            return Err(PrecompileError::Fatal(
                "validator lifecycle transition attempted to change registry identity".into(),
            ));
        }

        if allow_registry_identity_change {
            let pubkey = after.consensus_pubkey().ok_or_else(|| {
                PrecompileError::Fatal("registered validator is missing consensus pubkey".into())
            })?;
            if after.registry_index().is_none() {
                return Err(PrecompileError::Fatal(
                    "registered validator is missing registry index".into(),
                ));
            }
            if before.consensus_pubkey() != Some(pubkey) {
                self.write_consensus_pubkey(&after.address(), pubkey)?;
            }
        }

        let addr = after.address();
        if before.stake().bonded() != after.stake().bonded() {
            self.val_stake.write(&addr, after.stake().bonded())?;
        }
        if before.stored_status() != after.stored_status() {
            self.val_status.write(&addr, after.stored_status())?;
        }

        let before_history = before.history();
        let after_history = after.history();
        let before_slash_count = before_history.map_or(0, ValidatorHistory::slash_count);
        let after_slash_count = after_history.map_or(0, ValidatorHistory::slash_count);
        if before_slash_count != after_slash_count {
            self.val_slash_count.write(&addr, after_slash_count)?;
        }
        let before_missed_blocks = before_history.map_or(0, ValidatorHistory::missed_blocks);
        let after_missed_blocks = after_history.map_or(0, ValidatorHistory::missed_blocks);
        if before_missed_blocks != after_missed_blocks {
            self.val_missed_blocks.write(&addr, after_missed_blocks)?;
        }
        let before_missed_votes = before_history.map_or(0, ValidatorHistory::missed_votes);
        let after_missed_votes = after_history.map_or(0, ValidatorHistory::missed_votes);
        if before_missed_votes != after_missed_votes {
            self.val_missed_votes.write(&addr, after_missed_votes)?;
        }
        let before_blocks_proposed = before_history.map_or(0, ValidatorHistory::blocks_proposed);
        let after_blocks_proposed = after_history.map_or(0, ValidatorHistory::blocks_proposed);
        if before_blocks_proposed != after_blocks_proposed {
            self.val_blocks_proposed
                .write(&addr, after_blocks_proposed)?;
        }
        let before_joined_at = before_history.map_or(0, ValidatorHistory::joined_at_height);
        let after_joined_at = after_history.map_or(0, ValidatorHistory::joined_at_height);
        if before_joined_at != after_joined_at {
            self.val_joined_at_height.write(&addr, after_joined_at)?;
        }
        let before_deactivated_at = before_history
            .and_then(ValidatorHistory::last_deactivated_at_height)
            .unwrap_or(0);
        let after_deactivated_at = after_history
            .and_then(ValidatorHistory::last_deactivated_at_height)
            .unwrap_or(0);
        if before_deactivated_at != after_deactivated_at {
            self.val_deactivated_at_height
                .write(&addr, after_deactivated_at)?;
        }
        let before_unbonding_end = before.stake().unbonding_end_hint().unwrap_or(0);
        let after_unbonding_end = after.stake().unbonding_end_hint().unwrap_or(0);
        if before_unbonding_end != after_unbonding_end {
            self.val_unbonding_end.write(&addr, after_unbonding_end)?;
        }
        if before.has_bls_share() != after.has_bls_share() {
            self.val_has_bls_share.write(&addr, after.has_bls_share())?;
        }
        if before.join_confirmed() != after.join_confirmed() {
            self.val_join_confirmed
                .write(&addr, after.join_confirmed())?;
        }
        if before.stored_jailed_at() != after.stored_jailed_at() {
            self.val_jailed_at_height
                .write(&addr, after.stored_jailed_at())?;
        }
        let before_p2p = before
            .p2p()
            .map_or((0, Vec::new()), |p2p| (p2p.version(), p2p.encode_stored()));
        let after_p2p = after
            .p2p()
            .map_or((0, Vec::new()), |p2p| (p2p.version(), p2p.encode_stored()));
        if before_p2p != after_p2p {
            self.val_p2p_address_version.write(&addr, after_p2p.0)?;
            if after_p2p.1.is_empty() {
                self.val_p2p_address_payload.get_bytes(&addr).clear()?;
            } else {
                self.val_p2p_address_payload
                    .get_bytes(&addr)
                    .write(&after_p2p.1)?;
            }
        }

        Ok(())
    }

    /// Returns the keccak256 hash of a 48-byte consensus pubkey (for reverse lookup).
    pub fn consensus_pubkey_hash(pubkey: &[u8; 48]) -> B256 {
        keccak256(pubkey)
    }

    fn read_validator_record(&self, addr: Address) -> Result<ValidatorRecord> {
        let stored_status = self.val_status.read(&addr)?;
        ValidatorLifecycle::validate_stored_status(stored_status)?;
        Ok(ValidatorRecord {
            validator_address: addr,
            consensus_pubkey: self.read_consensus_pubkey(&addr)?,
            stake: self.val_stake.read(&addr)?,
            status: stored_status,
            slash_count: self.val_slash_count.read(&addr)?,
            missed_blocks: self.val_missed_blocks.read(&addr)?,
            missed_votes: self.val_missed_votes.read(&addr)?,
            blocks_proposed: self.val_blocks_proposed.read(&addr)?,
            joined_at_height: self.val_joined_at_height.read(&addr)?,
            deactivated_at_height: self.val_deactivated_at_height.read(&addr)?,
            unbonding_end: self.val_unbonding_end.read(&addr)?,
            has_bls_share: self.val_has_bls_share.read(&addr)?,
        })
    }

    /// Returns the full ABI-compatible record for a validator address, or
    /// `None` if it has no registry identity. Unknown status bytes fail closed.
    pub fn get_validator(&self, addr: Address) -> Result<Option<ValidatorRecord>> {
        if self.address_to_index.read(&addr)? == 0 {
            return Ok(None);
        }
        Ok(Some(self.read_validator_record(addr)?))
    }

    /// Returns all registered validators, including inactive and exiting ones.
    pub fn get_all_validators(&self) -> Result<Vec<ValidatorRecord>> {
        let addresses = self.registered_validator_addresses()?;
        let mut result = Vec::with_capacity(addresses.len());
        for addr in addresses {
            result.push(self.read_validator_record(addr)?);
        }
        Ok(result)
    }

    fn validator_addresses_matching(
        &self,
        predicate: impl Fn(&ValidatorLifecycle) -> bool,
    ) -> Result<Vec<Address>> {
        let mut result = Vec::new();
        for addr in self.registered_validator_addresses()? {
            if predicate(&self.validator_lifecycle(addr)?) {
                result.push(addr);
            }
        }
        Ok(result)
    }

    fn get_validators_matching(
        &self,
        predicate: impl Fn(&ValidatorLifecycle) -> bool,
    ) -> Result<Vec<ValidatorRecord>> {
        self.validator_addresses_matching(predicate)?
            .into_iter()
            .map(|addr| self.read_validator_record(addr))
            .collect()
    }

    /// Returns only validators with `status == ACTIVE`.
    pub fn get_active_validators(&self) -> Result<Vec<ValidatorRecord>> {
        self.get_validators_matching(ValidatorLifecycle::is_active_status)
    }

    /// Returns validators eligible to be in the NEXT consensus committee — the DKG
    /// reshare target / `next_players` set: `status ∈ {ACTIVE, PENDING}`. ACTIVE
    /// members stay; PENDING members are staked joiners awaiting their first share.
    /// EXITING validators are excluded (a reshare removes them). This is distinct
    /// from [`Self::get_active_validators`] (voting set, ACTIVE-only): a PENDING
    /// joiner must be in the reshare target so the ceremony grants it a share and
    /// [`Self::activate_reshared_set`] promotes it PENDING→ACTIVE.
    pub fn get_reshare_target_set(&self) -> Result<Vec<ValidatorRecord>> {
        self.get_validators_matching(ValidatorLifecycle::is_reshare_target)
    }

    /// Returns validators with `status == PENDING` — staked joiners admitted to the
    /// validator set but not yet granted a threshold share. Used to admit them to
    /// consensus P2P as SECONDARY peers so they can sync to head before the reshare
    /// that makes them signers; they are NOT consensus participants (no share).
    pub fn get_pending_validators(&self) -> Result<Vec<ValidatorRecord>> {
        self.get_validators_matching(ValidatorLifecycle::is_pending)
    }

    /// Returns validators admitted to consensus P2P as non-voting SECONDARY peers so
    /// they sync to head: `status ∈ {REGISTERED, PENDING}`. This is the
    /// TEE full-node admission: a REGISTERED node (registered +
    /// P2P-announced + enclave-registered, but NOT yet staked) syncs and executes
    /// offer blocks as a verifier WITHOUT voting; a PENDING joiner is the staked case
    /// on its way to ACTIVE. Voting requires `has_bls_share` (granted only by a
    /// reshare), so admitting these peers cannot affect consensus. Distinct from
    /// [`Self::get_reshare_target_set`] ({ACTIVE, PENDING}) — REGISTERED nodes are not
    /// staked and must NOT receive a threshold share. Peers without a registered P2P
    /// address are dropped downstream (the address read yields `Missing`).
    pub fn get_admitted_non_consensus_validators(&self) -> Result<Vec<ValidatorRecord>> {
        self.get_validators_matching(ValidatorLifecycle::is_secondary_admission)
    }

    /// Returns validators in the current consensus set.
    ///
    /// EXITING validators retain current-epoch consensus accountability until a
    /// successful reshare excludes them and clears their BLS share.
    pub fn get_active_consensus_set(&self) -> Result<Vec<ValidatorRecord>> {
        self.get_validators_matching(ValidatorLifecycle::is_current_consensus_participant)
    }

    /// Returns the number of active validators.
    pub fn active_validator_count(&self) -> Result<u32> {
        let count: u32 = self
            .validator_addresses_matching(ValidatorLifecycle::is_active_status)?
            .len()
            .try_into()
            .map_err(|_| PrecompileError::Revert("active validator count exceeds u32".into()))?;
        Ok(count)
    }

    /// number of validators currently in the `REGISTERED` (self-registered,
    /// not-yet-staked) state. Used to bound the free, permissionless
    /// self-registration Sybil surface; see [`MAX_SELF_REGISTERED_UNSTAKED`].
    pub fn registered_count(&self) -> Result<u32> {
        let count: u32 = self
            .validator_addresses_matching(ValidatorLifecycle::is_registered_status)?
            .len()
            .try_into()
            .map_err(|_| {
                PrecompileError::Revert("registered validator count exceeds u32".into())
            })?;
        Ok(count)
    }

    /// Returns the number of validators in the active consensus set.
    pub fn active_consensus_count(&self) -> Result<u32> {
        let count: u32 = self
            .validator_addresses_matching(ValidatorLifecycle::is_current_consensus_participant)?
            .len()
            .try_into()
            .map_err(|_| PrecompileError::Revert("active consensus count exceeds u32".into()))?;
        Ok(count)
    }

    /// Returns true if the validator is a current consensus participant.
    pub fn is_consensus_participant(&self, addr: Address) -> Result<bool> {
        Ok(self
            .validator_lifecycle(addr)?
            .is_current_consensus_participant())
    }

    /// Returns whether there is a pending validator set change that consensus should detect.
    pub fn has_pending_set_change(&self) -> Result<bool> {
        self.pending_set_change.read()
    }

    /// Returns the number of entries in the dense validator registry.
    pub fn validator_count(&self) -> Result<u32> {
        self.validator_count.read()
    }

    /// Returns the persisted active-set hash without exposing its storage slot.
    pub fn active_consensus_set_hash(&self) -> Result<B256> {
        self.active_consensus_set_hash.read()
    }

    /// Returns the current epoch as `u64`; an oversized persisted value is
    /// deterministic state corruption and therefore fails closed.
    pub fn current_epoch_u64(&self) -> Result<u64> {
        self.epoch_number
            .read()?
            .try_into()
            .map_err(|_| PrecompileError::Fatal("ValidatorSet.epoch_number exceeds u64".into()))
    }

    /// Returns all public epoch metadata as one consistent read projection.
    pub fn epoch_snapshot(&self) -> Result<EpochSnapshot> {
        Ok(EpochSnapshot {
            number: self.epoch_number.read()?,
            start_timestamp: self.epoch_start_timestamp.read()?,
            start_block: self.epoch_start_block.read()?,
            length_blocks: self.config_epoch_length_blocks.read()?,
        })
    }

    /// Returns registered addresses in dense registry-index order.
    pub fn registered_validator_addresses(&self) -> Result<Vec<Address>> {
        let count = self.validator_count.read()?;
        let mut addresses = Vec::with_capacity(count as usize);
        for index in 1..=u64::from(count) {
            let address = self.validator_address_at(index)?.ok_or_else(|| {
                PrecompileError::Fatal(format!(
                    "validator registry index {index} is empty below validator_count {count}"
                ))
            })?;
            addresses.push(address);
        }
        Ok(addresses)
    }

    /// Resolves a one-based dense registry index and checks its reverse mapping.
    pub fn validator_address_at(&self, index: u64) -> Result<Option<Address>> {
        let count = u64::from(self.validator_count.read()?);
        if index == 0 || index > count {
            return Ok(None);
        }
        let address = self.index_to_address.read(&index)?;
        if address.is_zero() {
            return Err(PrecompileError::Fatal(format!(
                "validator registry index {index} is empty below validator_count {count}"
            )));
        }
        let reverse_index = self.address_to_index.read(&address)?;
        if reverse_index != index {
            return Err(PrecompileError::Fatal(format!(
                "validator registry reverse index mismatch for {address}: expected {index}, got {reverse_index}"
            )));
        }
        Ok(Some(address))
    }

    /// Returns the registered validator's consensus identity key.
    pub fn consensus_pubkey_of(&self, addr: Address) -> Result<Option<[u8; 48]>> {
        if self.address_to_index.read(&addr)? == 0 {
            return Ok(None);
        }
        Ok(Some(self.read_consensus_pubkey(&addr)?))
    }

    /// Returns participation counters, preserving the legacy all-zero result for
    /// an address that has no registry history.
    pub fn participation(&self, addr: Address) -> Result<ValidatorParticipation> {
        let state = self.validator_state(addr)?;
        let Some(history) = state.history() else {
            return Ok(ValidatorParticipation::default());
        };
        Ok(ValidatorParticipation {
            blocks_proposed: history.blocks_proposed(),
            missed_blocks: history.missed_blocks(),
            missed_votes: history.missed_votes(),
        })
    }

    /// Stores a validator's versioned Commonware P2P address payload.
    ///
    /// The stable ABI is Outbe-owned `(version, bytes)`, not Commonware's raw
    /// codec. The payload is fully validated before any storage write.
    pub fn set_p2p_address(
        &mut self,
        caller: Address,
        validator_addr: Address,
        version: u8,
        encoded: &[u8],
    ) -> Result<()> {
        let owner = self.config_owner.read()?;
        if caller != owner && caller != validator_addr {
            return Err(PrecompileError::Revert(
                "unauthorized: caller must be owner or validator itself".into(),
            ));
        }
        if self.address_to_index.read(&validator_addr)? == 0 {
            return Err(PrecompileError::Revert("validator not registered".into()));
        }
        if version != P2P_ADDRESS_VERSION_V1 {
            return Err(PrecompileError::Revert(format!(
                "unsupported p2p address version {version}"
            )));
        }
        if encoded.len() > MAX_P2P_ADDRESS_ENCODED_LEN {
            return Err(PrecompileError::Revert(format!(
                "p2p address payload exceeds max length {}",
                MAX_P2P_ADDRESS_ENCODED_LEN
            )));
        }
        let decoded = decode_versioned(version, encoded)
            .map_err(|err| PrecompileError::Revert(format!("invalid p2p address: {err}")))?;

        let before = self.validator_state(validator_addr)?;
        let after = before.clone().with_p2p(Some(P2pInfo::V1(decoded)));
        self.persist_validator_state_delta(&before, &after)?;
        Ok(())
    }

    /// Returns the stored versioned P2P address payload, if one is registered.
    pub fn get_p2p_address(&self, validator_addr: Address) -> Result<Option<(u8, Vec<u8>)>> {
        if self.address_to_index.read(&validator_addr)? == 0 {
            return Err(PrecompileError::Revert("validator not registered".into()));
        }
        let version = self.val_p2p_address_version.read(&validator_addr)?;
        let encoded = self
            .val_p2p_address_payload
            .get_bytes(&validator_addr)
            .read()?;
        if version == 0 && encoded.is_empty() {
            return Ok(None);
        }
        Ok(Some((version, encoded)))
    }

    /// Registers a new validator.
    ///
    /// The caller must be either the config owner or the validator address itself.
    /// The address must not already be registered, and the count must be below max.
    /// Initial status is REGISTERED (waiting for DKG reshare to become Active).
    ///
    /// `consensus_pubkey` is a 48-byte BLS12-381 MinPk public key.
    pub fn register_validator(
        &mut self,
        caller: Address,
        validator_addr: Address,
        consensus_pubkey: &[u8; 48],
    ) -> Result<()> {
        self.register_validator_with_sig(caller, validator_addr, consensus_pubkey, None)
    }

    /// Registers a new validator with optional BLS signature verification.
    ///
    /// When `bls_signature` is `Some`, verifies that the BLS MinPk key was used to
    /// sign `validator_addr` (20 bytes) under the "outbe_REGISTER" namespace.
    /// When `None`, signature verification is skipped (used by system/owner
    /// registrations and tests).
    ///
    /// `consensus_pubkey` is a 48-byte BLS12-381 MinPk public key.
    /// `bls_signature` is an optional 96-byte BLS MinPk signature.
    pub fn register_validator_with_sig(
        &mut self,
        caller: Address,
        validator_addr: Address,
        consensus_pubkey: &[u8; 48],
        bls_signature: Option<&[u8; 96]>,
    ) -> Result<()> {
        let owner = self.config_owner.read()?;

        // Authorization: owner or self-registration
        if caller != owner && caller != validator_addr {
            return Err(PrecompileError::Revert(
                "unauthorized: caller must be owner or validator itself".into(),
            ));
        }
        self.ensure_not_operational_delegate(validator_addr)?;

        // BLS proof-of-key-ownership is mandatory for self-registration.
        // Owner registrations (caller == owner && caller != validator_addr) may
        // skip the signature for system bootstrapping.
        if caller == validator_addr {
            // Self-registration: signature is required
            match bls_signature {
                Some(sig_bytes) => {
                    verify_bls_registration_sig(consensus_pubkey, sig_bytes, &validator_addr)?;
                }
                None => {
                    return Err(PrecompileError::Revert(
                        "self-registration requires BLS proof-of-key-ownership signature".into(),
                    ));
                }
            }
        } else if let Some(sig_bytes) = bls_signature {
            // Owner registration WITH a proof-of-key-ownership signature: verify
            // it (defence against the owner inserting a key it does not possess).
            verify_bls_registration_sig(consensus_pubkey, sig_bytes, &validator_addr)?;
        } else {
            // owner registration WITHOUT a PoP signature. Permitted because
            // the owner is a trusted role used for system/genesis bootstrapping,
            // but the committee's MinPk aggregate vote uses the rogue-key-vulnerable
            // same-message construction, so an externally-supplied key whose
            // possession the owner did not verify is a rogue-key surface. TRUST
            // ASSUMPTION: the owner MUST verify proof-of-possession out-of-band for
            // any externally-supplied consensus key (genesis-set collusion is out
            // of the BFT model). The full on-chain defence — mandatory PoP for every
            // committee-bound key, including genesis-seeded keys — would break the
            // bootstrap flow and is disproportionate to a privilege-gated threat;
            // see audit.md. Surface the unverified insertion so it is
            // auditable.
            warn!(
                target: "outbe::validatorset",
                event = "owner_registration_without_pop",
                validator = %validator_addr,
                "owner registered a validator WITHOUT a BLS proof-of-possession signature; the \
                 owner must verify key possession out-of-band (rogue-key surface on the MinPk \
                 aggregate —)"
            );
        }

        // bound the free, permissionless self-registration Sybil surface.
        // A self-registered REGISTERED node is admitted to the consensus P2P
        // secondary tier (the TEE verifier flow), so cap how many unstaked
        // REGISTERED validators can exist at once — far below
        // `config_max_validators` — so an attacker cannot fill the validator set
        // (or the consensus P2P set) with free Sybils. Owner registrations
        // (`caller == owner`) bypass this cap. Checked before any state mutation
        // (including the re-registration path), so an over-cap self-registration
        // never consumes a registration slot.
        if caller == validator_addr && self.registered_count()? >= MAX_SELF_REGISTERED_UNSTAKED {
            return Err(PrecompileError::Revert(
                "self-registration limit reached: too many unstaked REGISTERED validators \
                 (owner may register directly)"
                    .into(),
            ));
        }

        // Verify BLS pubkey is not already used by another validator.
        // Without this check, two validators could register the same BLS key,
        // causing undefined behavior during DKG/reshare.
        let pk_hash = Self::consensus_pubkey_hash(consensus_pubkey);
        let existing_owner = self.consensus_pubkey_hash_to_address.read(&pk_hash)?;
        if !existing_owner.is_zero() && existing_owner != validator_addr {
            return Err(PrecompileError::Revert(
                "BLS consensus pubkey already registered by another validator".into(),
            ));
        }

        // Decode registry presence and lifecycle before selecting first-time vs
        // re-registration. This is the sole raw-to-typed construction boundary.
        let existing_state = self.validator_state(validator_addr)?;
        if let Some(existing_index) = existing_state.registry_index() {
            if !matches!(
                existing_state.lifecycle().phase(),
                ValidatorLifecycle::Inactive
            ) {
                return Err(PrecompileError::Revert(
                    "validator already registered".into(),
                ));
            }
            // Re-registration path: check cooldown then reuse existing index
            let cooldown = self.config_reregistration_cooldown.read()?;
            if cooldown > 0 {
                let deactivated_at = existing_state
                    .history()
                    .ok_or_else(|| {
                        PrecompileError::Fatal("registered validator is missing history".into())
                    })?
                    .last_deactivated_at_height();
                let current_height = self.storage.block_number()?;
                if deactivated_at
                    .is_some_and(|height| current_height < height + u64::from(cooldown))
                {
                    return Err(PrecompileError::Revert(
                        "re-registration cooldown not expired".into(),
                    ));
                }
            }

            // Reset lifecycle metadata without changing stake accounting. Staking
            // remains the source of truth for stake and mirrors into val_stake.
            let old_pubkey = existing_state.consensus_pubkey().ok_or_else(|| {
                PrecompileError::Fatal("registered validator is missing consensus pubkey".into())
            })?;
            let old_pk_hash = Self::consensus_pubkey_hash(old_pubkey);
            self.consensus_pubkey_hash_to_address
                .write(&old_pk_hash, Address::ZERO)?;

            let pk_hash = Self::consensus_pubkey_hash(consensus_pubkey);
            self.consensus_pubkey_hash_to_address
                .write(&pk_hash, validator_addr)?;
            let after = existing_state
                .clone()
                .reregister(*consensus_pubkey, self.storage.block_number()?)?;
            self.persist_registry_state_delta(&existing_state, &after)?;

            self.pending_set_change.write(true)?;

            crate::metrics::record_validator_status(validator_addr, status::REGISTERED);
            crate::metrics::record_validator_register(validator_addr, true);
            crate::metrics::record_pending_set_change(true);

            journal_record(JournalRecord::ValidatorReregistered {
                wall_clock: iso8601_now(),
                block_number: self.storage.block_number().unwrap_or(0),
                validator: format!("{validator_addr:?}"),
                index: existing_index.get(),
            });

            info!(
                target: "outbe::validatorset",
                event = "validator_reregistered",
                validator = %validator_addr,
                index = existing_index.get(),
                block_number = self.storage.block_number().unwrap_or(0),
                "validator re-registered (was INACTIVE, lifecycle metadata reset)",
            );

            self.emit(IValidatorSet::ValidatorRegistered {
                validator: validator_addr,
                index: existing_index.get(),
            })?;

            return Ok(());
        }

        // Check capacity
        let count = self.validator_count.read()?;
        let max = self.config_max_validators.read()?;
        if max > 0 && count >= max {
            return Err(PrecompileError::Revert("max validators reached".into()));
        }

        // Assign 1-based index
        let new_index = count + 1;
        let new_index_u64 = new_index as u64;
        self.address_to_index
            .write(&validator_addr, new_index_u64)?;
        self.index_to_address
            .write(&new_index_u64, validator_addr)?;

        // Construct and persist the complete first-time registry bundle. A
        // pre-registration stake projection remains intact.
        let registered_state = existing_state.clone().register(
            new_index_u64,
            *consensus_pubkey,
            self.storage.block_number()?,
        )?;
        self.persist_registry_state_delta(&existing_state, &registered_state)?;

        // Pubkey reverse lookup (keyed by keccak256 of full 48-byte pubkey)
        let pk_hash = Self::consensus_pubkey_hash(consensus_pubkey);
        self.consensus_pubkey_hash_to_address
            .write(&pk_hash, validator_addr)?;

        // Increment count
        self.validator_count.write(new_index)?;

        // Signal pending set change so consensus detects the new validator
        self.pending_set_change.write(true)?;

        crate::metrics::record_validator_status(validator_addr, status::REGISTERED);
        crate::metrics::record_validator_register(validator_addr, false);
        crate::metrics::record_pending_set_change(true);

        journal_record(JournalRecord::ValidatorRegistered {
            wall_clock: iso8601_now(),
            block_number: self.storage.block_number().unwrap_or(0),
            validator: format!("{validator_addr:?}"),
            index: new_index as u64,
        });

        info!(
            target: "outbe::validatorset",
            event = "validator_registered",
            validator = %validator_addr,
            index = new_index as u64,
            block_number = self.storage.block_number().unwrap_or(0),
            "validator registered (first-time)",
        );

        self.emit(IValidatorSet::ValidatorRegistered {
            validator: validator_addr,
            index: new_index as u64,
        })?;

        Ok(())
    }

    /// Records a successful stake increase from Staking and performs the
    /// REGISTERED -> PENDING threshold transition when required.
    ///
    /// Staking owns the authoritative balance. This method is the only
    /// production write seam for the ValidatorSet mirror and its coupled
    /// lifecycle fields.
    pub fn record_stake_increase(
        &mut self,
        addr: Address,
        bonded: U256,
        minimum: U256,
    ) -> Result<()> {
        let before = self.validator_state(addr)?;
        let retained_share = before.has_bls_share();
        let hint = before.stake().unbonding_end_hint();
        let mut after = before
            .clone()
            .with_stake_projection(StakeProjection::new(bonded, hint));
        let mut became_pending = false;

        if bonded >= minimum && matches!(before.lifecycle().phase(), ValidatorLifecycle::Registered)
        {
            let pending = if retained_share {
                PendingState::AwaitingConfirmationWithRetainedShare
            } else {
                PendingState::AwaitingConfirmation
            };
            let lifecycle = before
                .lifecycle()
                .clone()
                .replace_phase(ValidatorLifecycle::Pending(pending))
                .without_readiness_residue()
                .without_share_residue();
            after = after.with_lifecycle(lifecycle);
            became_pending = true;
        }

        self.persist_validator_state_delta(&before, &after)?;
        if became_pending {
            self.pending_set_change.write(true)?;
            crate::metrics::record_validator_status(addr, status::PENDING);
            crate::metrics::record_pending_set_change(true);
        }
        Ok(())
    }

    /// Records a voluntary withdrawal and preserves the existing compatibility
    /// transitions, including S-01 readiness residue and the current D-07 jail
    /// exit behavior.
    pub fn record_unstake(
        &mut self,
        addr: Address,
        bonded: U256,
        minimum: U256,
        unbonding_end_hint: u64,
    ) -> Result<()> {
        let before = self.validator_state(addr)?;
        let lifecycle = before.lifecycle().phase().clone();
        let mut after = before.clone().with_stake_projection(StakeProjection::new(
            bonded,
            (unbonding_end_hint != 0).then_some(unbonding_end_hint),
        ));
        let mut set_change = false;

        if bonded < minimum {
            match lifecycle {
                ValidatorLifecycle::Active(active) => {
                    let height = self.storage.block_number()?;
                    let history = before.history().copied().ok_or_else(|| {
                        PrecompileError::Fatal("registered validator is missing history".into())
                    })?;
                    let lifecycle = before
                        .lifecycle()
                        .clone()
                        .replace_phase(ValidatorLifecycle::Exiting(active.begin_exit()));
                    after = after
                        .with_lifecycle(lifecycle)
                        .with_history(history.with_last_deactivated_at_height(Some(height)));
                    set_change = true;
                }
                ValidatorLifecycle::Pending(pending) => {
                    let mut lifecycle = before
                        .lifecycle()
                        .clone()
                        .replace_phase(ValidatorLifecycle::Registered);
                    if pending.join_confirmed() {
                        lifecycle = lifecycle.with_readiness_residue();
                    }
                    if pending.has_share() {
                        lifecycle = lifecycle.with_share_residue();
                    }
                    after = after.with_lifecycle(lifecycle);
                    set_change = true;
                }
                ValidatorLifecycle::Jailed(jailed) => {
                    let height = self.storage.block_number()?;
                    let history = before.history().copied().ok_or_else(|| {
                        PrecompileError::Fatal("registered validator is missing history".into())
                    })?;
                    let lifecycle = before
                        .lifecycle()
                        .clone()
                        .replace_phase(ValidatorLifecycle::Exiting(jailed.leave_below_minimum()));
                    after = after
                        .with_lifecycle(lifecycle)
                        .with_history(history.with_last_deactivated_at_height(Some(height)));
                    set_change = true;
                }
                _ => {}
            }
        }

        self.persist_validator_state_delta(&before, &after)?;
        if set_change {
            self.pending_set_change.write(true)?;
            crate::metrics::record_pending_set_change(true);
        }
        Ok(())
    }

    /// Records a stake slash. This is intentionally distinct from voluntary
    /// withdrawal: a JAILED validator remains JAILED after the slash.
    pub fn record_stake_slash(
        &mut self,
        addr: Address,
        bonded: U256,
        minimum: U256,
        unbonding_end_hint: Option<u64>,
    ) -> Result<()> {
        let before = self.validator_state(addr)?;
        let lifecycle = before.lifecycle().phase().clone();
        let hint = unbonding_end_hint.or(before.stake().unbonding_end_hint());
        let mut after = before
            .clone()
            .with_stake_projection(StakeProjection::new(bonded, hint));
        let mut set_change = false;

        if !minimum.is_zero() && bonded < minimum {
            match lifecycle {
                ValidatorLifecycle::Active(active) => {
                    let height = self.storage.block_number()?;
                    let history = before.history().copied().ok_or_else(|| {
                        PrecompileError::Fatal("registered validator is missing history".into())
                    })?;
                    let lifecycle = before
                        .lifecycle()
                        .clone()
                        .replace_phase(ValidatorLifecycle::Exiting(active.begin_exit()));
                    after = after
                        .with_lifecycle(lifecycle)
                        .with_history(history.with_last_deactivated_at_height(Some(height)));
                    set_change = true;
                }
                ValidatorLifecycle::Pending(pending) => {
                    let mut lifecycle = before
                        .lifecycle()
                        .clone()
                        .replace_phase(ValidatorLifecycle::Registered);
                    if pending.join_confirmed() {
                        lifecycle = lifecycle.with_readiness_residue();
                    }
                    if pending.has_share() {
                        lifecycle = lifecycle.with_share_residue();
                    }
                    after = after.with_lifecycle(lifecycle);
                    set_change = true;
                }
                _ => {}
            }
        }

        self.persist_validator_state_delta(&before, &after)?;
        if set_change {
            self.pending_set_change.write(true)?;
            crate::metrics::record_pending_set_change(true);
        }
        Ok(())
    }

    /// Completes UNBONDING after Staking has verified zero bonded stake and no
    /// remaining live claims.
    pub fn complete_unbonding(&mut self, addr: Address) -> Result<()> {
        let before = self.validator_state(addr)?;
        if !matches!(before.lifecycle().phase(), ValidatorLifecycle::Unbonding) {
            return Ok(());
        }
        let lifecycle = before
            .lifecycle()
            .clone()
            .replace_phase(ValidatorLifecycle::Inactive)
            .without_share_residue();
        let after = before
            .clone()
            .with_lifecycle(lifecycle)
            .with_stake_projection(StakeProjection::new(before.stake().bonded(), None));
        self.persist_validator_state_delta(&before, &after)
    }

    /// Marks a REGISTERED validator as PENDING — staked and admitted to the
    /// validator set, but NOT yet a consensus participant (no threshold share).
    ///
    /// This is the staking entrypoint (PoS): reaching `min_stake` moves a validator
    /// REGISTERED→PENDING (not directly ACTIVE). The validator then syncs to head and
    /// is included in the next DKG reshare target; only when the reshare grants it a
    /// share does [`Self::activate_reshared_set`] promote it PENDING→ACTIVE. Signals
    /// `pending_set_change` so consensus schedules that reshare. Idempotent for a
    /// validator already PENDING/ACTIVE.
    pub fn mark_pending(&mut self, addr: Address) -> Result<()> {
        let before = self.validator_state(addr)?;
        if matches!(before.lifecycle().phase(), ValidatorLifecycle::Unregistered) {
            return Err(PrecompileError::Revert("validator not registered".into()));
        }
        // Only a freshly-REGISTERED validator transitions to PENDING. A validator
        // already PENDING or ACTIVE is left untouched (no spurious churn / no
        // demotion of an active validator on a top-up stake).
        if !matches!(before.lifecycle().phase(), ValidatorLifecycle::Registered) {
            return Ok(());
        }
        let retained_share = before.has_bls_share();
        let pending = if retained_share {
            PendingState::AwaitingConfirmationWithRetainedShare
        } else {
            PendingState::AwaitingConfirmation
        };
        let lifecycle = before
            .lifecycle()
            .clone()
            .replace_phase(ValidatorLifecycle::Pending(pending))
            .without_readiness_residue()
            .without_share_residue();
        let after = before.clone().with_lifecycle(lifecycle);
        self.persist_validator_state_delta(&before, &after)?;
        // Signal consensus to include this validator in the next reshare target.
        self.pending_set_change.write(true)?;

        crate::metrics::record_validator_status(addr, status::PENDING);
        crate::metrics::record_pending_set_change(true);

        Ok(())
    }

    /// Stale-join guard: a PENDING joiner confirms, on-chain, that its node has
    /// caught up to head and is ready to be frozen into the next DKG reshare
    /// target. The operator sends this only after `outbe_syncStatus` shows the
    /// node at the finalized tip; until then the joiner stays PENDING and is
    /// excluded from [`Self::get_reshare_target_set`]. Caller must be the
    /// validator itself and currently PENDING.
    pub fn confirm_validator_ready(&mut self, caller: Address) -> Result<()> {
        let before = self.validator_state(caller)?;
        if matches!(before.lifecycle().phase(), ValidatorLifecycle::Unregistered) {
            return Err(PrecompileError::Revert("validator not registered".into()));
        }
        let pending = match before.lifecycle().phase().clone() {
            ValidatorLifecycle::Pending(pending) => pending,
            lifecycle => {
                return Err(PrecompileError::Revert(format!(
                    "confirmValidatorReady requires PENDING status, got {}",
                    registered_status(&lifecycle)?
                )))
            }
        };
        let lifecycle = before
            .lifecycle()
            .clone()
            .replace_phase(ValidatorLifecycle::Pending(pending.confirm()));
        let after = before.clone().with_lifecycle(lifecycle);
        self.persist_validator_state_delta(&before, &after)?;
        // Re-signal so consensus schedules a reshare now that a confirmed joiner
        // is eligible (the stake-time signal may already have lapsed).
        self.pending_set_change.write(true)?;
        crate::metrics::record_pending_set_change(true);
        Ok(())
    }

    /// Activates a registered validator (sets status to ACTIVE).
    ///
    /// Only REGISTERED and PENDING statuses are allowed as source states.
    /// Also signals `pending_set_change` so the consensus layer triggers a DKG
    /// reshare to include the newly-activated validator. Retained for owner/manual
    /// activation; the normal PoS path is [`Self::mark_pending`] →
    /// [`Self::activate_reshared_set`].
    pub fn activate_validator(&mut self, addr: Address) -> Result<()> {
        let before = self.validator_state(addr)?;
        if matches!(before.lifecycle().phase(), ValidatorLifecycle::Unregistered) {
            return Err(PrecompileError::Revert("validator not registered".into()));
        }
        if matches!(before.lifecycle().phase(), ValidatorLifecycle::Active(_)) {
            return Ok(()); // already active — no spurious churn
        }
        let has_share = before.has_bls_share();
        let source_phase = before.lifecycle().phase().clone();
        let active = match source_phase.clone() {
            ValidatorLifecycle::Registered | ValidatorLifecycle::Pending(_) => {
                if has_share {
                    ActiveState::Participating
                } else {
                    ActiveState::AwaitingShareRepair
                }
            }
            lifecycle => {
                return Err(PrecompileError::Revert(format!(
                    "cannot activate validator with status {}: only REGISTERED or PENDING allowed",
                    registered_status(&lifecycle)?
                )))
            }
        };
        let history = before.history().copied().ok_or_else(|| {
            PrecompileError::Fatal("registered validator is missing history".into())
        })?;
        let mut lifecycle = before
            .lifecycle()
            .clone()
            .replace_phase(ValidatorLifecycle::Active(active))
            .without_share_residue();
        if matches!(source_phase, ValidatorLifecycle::Pending(ref pending) if pending.join_confirmed())
        {
            lifecycle = lifecycle.with_readiness_residue();
        }
        let after = before
            .clone()
            .with_lifecycle(lifecycle)
            .with_history(history.with_last_deactivated_at_height(None));
        self.persist_validator_state_delta(&before, &after)?;

        // Signal consensus to include this validator in the next reshare.
        self.pending_set_change.write(true)?;

        self.emit(IValidatorSet::ValidatorActivated { validator: addr })?;

        Ok(())
    }

    /// Deactivates a validator — transitions to EXITING (awaiting DKG reshare to exclude).
    ///
    /// The caller must be the config owner or the validator itself.
    pub fn deactivate_validator(&mut self, caller: Address, addr: Address) -> Result<()> {
        let owner = self.config_owner.read()?;
        if caller != owner && caller != addr {
            return Err(PrecompileError::Revert(
                "unauthorized: caller must be owner or validator itself".into(),
            ));
        }
        let before = self.validator_state(addr)?;
        let active = match before.lifecycle().phase().clone() {
            ValidatorLifecycle::Active(active) => active,
            ValidatorLifecycle::Unregistered => {
                return Err(PrecompileError::Revert("validator not registered".into()))
            }
            _ => {
                return Err(PrecompileError::Revert(
                    "can only deactivate an active validator".into(),
                ))
            }
        };
        let height = self.storage.block_number()?;
        let history = before.history().copied().ok_or_else(|| {
            PrecompileError::Fatal("registered validator is missing history".into())
        })?;
        let lifecycle = before
            .lifecycle()
            .clone()
            .replace_phase(ValidatorLifecycle::Exiting(active.begin_exit()));
        let after = before
            .clone()
            .with_lifecycle(lifecycle)
            .with_history(history.with_last_deactivated_at_height(Some(height)));
        self.persist_validator_state_delta(&before, &after)?;

        // Signal pending set change so consensus triggers DKG reshare to exclude
        self.pending_set_change.write(true)?;

        crate::metrics::record_validator_status(addr, status::EXITING);
        crate::metrics::record_validator_deactivate(addr);
        crate::metrics::record_pending_set_change(true);

        journal_record(JournalRecord::ValidatorDeactivated {
            wall_clock: iso8601_now(),
            block_number: height,
            validator: format!("{addr:?}"),
            caller: format!("{caller:?}"),
            self_initiated: caller == addr,
        });

        info!(
            target: "outbe::validatorset",
            event = "validator_deactivated",
            validator = %addr,
            %caller,
            self_initiated = (caller == addr),
            block_number = height,
            "validator transitioned ACTIVE -> EXITING (voluntary deactivation)",
        );

        self.emit(IValidatorSet::ValidatorDeactivated {
            validator: addr,
            atHeight: height,
        })?;

        Ok(())
    }

    /// Forces a validator out of consensus because of a severe fault.
    ///
    /// The validator enters EXITING and is removed from consensus on the next
    /// successful reshare. Stake withdrawal is handled by Staking after the
    /// validator reaches UNBONDING.
    pub fn force_exit_validator(&mut self, addr: Address) -> Result<()> {
        self.punish_validator(addr, false)
    }

    /// Jails a validator for a severe consensus/oracle fault (felony). Unlike
    /// [`Self::force_exit_validator`], the validator is NOT removed from the
    /// registry: it is frozen in JAILED, excluded from the next reshare target
    /// (so the reshare clears its share), and may later return via
    /// `unjailValidator` (→ PENDING → ACTIVE) or leave via a full unstake
    /// (→ EXITING → UNBONDING → INACTIVE). The slash itself is applied by the
    /// caller AFTER this call (slash_stake leaves a JAILED status untouched).
    /// Increments `slash_count` (mirrors force-exit). The status is idempotent
    /// for JAILED, but D-05 compatibility means the raw call's counter/event
    /// effects are not; production callers must retain their replay guard.
    pub fn jail_validator(&mut self, addr: Address) -> Result<()> {
        self.punish_validator(addr, true)
    }

    /// Shared punitive transition for [`Self::force_exit_validator`] (`jail =
    /// false` → ACTIVE→EXITING, the validator leaves the registry via UNBONDING)
    /// and [`Self::jail_validator`] (`jail = true` → ACTIVE→JAILED, the validator
    /// is frozen in the registry). Both signal a reshare and bump `slash_count`.
    /// Their lifecycle status is idempotent once punished; D-05 is preserved:
    /// repeat raw calls still increment `slash_count`; SlashIndicator supplies the
    /// production replay guard.
    fn punish_validator(&mut self, addr: Address, jail: bool) -> Result<()> {
        let before = self.validator_state(addr)?;
        if matches!(before.lifecycle().phase(), ValidatorLifecycle::Unregistered) {
            return Err(PrecompileError::Revert("validator not registered".into()));
        }

        let lifecycle = before.lifecycle().phase().clone();
        let current_status = registered_status(&lifecycle)?;
        let block_number = self.storage.block_number()?;
        let (target, target_label, action) = if jail {
            (status::JAILED, "JAILED", "jail")
        } else {
            (status::EXITING, "EXITING", "force-exit")
        };

        let history = before.history().copied().ok_or_else(|| {
            PrecompileError::Fatal("registered validator is missing history".into())
        })?;
        let mut after = before.clone();
        match lifecycle.clone() {
            ValidatorLifecycle::Active(active) => {
                let next_phase = if jail {
                    ValidatorLifecycle::Jailed(active.jail(block_number))
                } else {
                    ValidatorLifecycle::Exiting(active.begin_exit())
                };
                let mut next = before.lifecycle().clone().replace_phase(next_phase);
                if jail {
                    next = next.without_jail_height_residue();
                }
                after = after
                    .with_lifecycle(next)
                    .with_history(history.with_last_deactivated_at_height(Some(block_number)));
            }
            ValidatorLifecycle::Jailed(_) if jail => {}
            ValidatorLifecycle::Exiting(_)
            | ValidatorLifecycle::Unbonding
            | ValidatorLifecycle::Inactive => {}
            _ => {
                return Err(PrecompileError::Revert(format!(
                    "cannot {action} validator with status {current_status}: only ACTIVE, EXITING, UNBONDING, or INACTIVE allowed"
                )));
            }
        }

        let updated_history = *after.history().ok_or_else(|| {
            PrecompileError::Fatal("registered validator is missing history".into())
        })?;
        after = after.with_history(ValidatorHistory::new(
            updated_history.joined_at_height(),
            updated_history.last_deactivated_at_height(),
            updated_history.slash_count() + 1,
            updated_history.missed_blocks(),
            updated_history.missed_votes(),
            updated_history.blocks_proposed(),
        ));
        self.persist_validator_state_delta(&before, &after)?;

        match lifecycle {
            ValidatorLifecycle::Active(_) => {
                self.pending_set_change.write(true)?;
                crate::metrics::record_validator_status(addr, target);
                crate::metrics::record_validator_force_exit(addr);
                crate::metrics::record_pending_set_change(true);
                journal_record(JournalRecord::ValidatorForcedExit {
                    wall_clock: iso8601_now(),
                    block_number,
                    validator: format!("{addr:?}"),
                    status_before: "ACTIVE".into(),
                    status_after: target_label.into(),
                });
                warn!(
                    target: "outbe::validatorset",
                    event = if jail { "validator_jailed" } else { "validator_force_exit" },
                    validator = %addr,
                    status_after = target_label,
                    block_number,
                    "validator punished from ACTIVE (force-exit/jail)",
                );
                if jail {
                    self.emit(IValidatorSet::ValidatorJailed {
                        validator: addr,
                        atHeight: block_number,
                    })?;
                } else {
                    self.emit(IValidatorSet::ValidatorDeactivated {
                        validator: addr,
                        atHeight: block_number,
                    })?;
                    self.emit(IValidatorSet::ValidatorForcedExit {
                        validator: addr,
                        atHeight: block_number,
                    })?;
                }
            }
            ValidatorLifecycle::Jailed(jailed) => {
                self.pending_set_change.write(true)?;
                self.emit(IValidatorSet::ValidatorJailed {
                    validator: addr,
                    atHeight: jailed.jailed_at(),
                })?;
            }
            ValidatorLifecycle::Exiting(_) => {
                self.pending_set_change.write(true)?;
                crate::metrics::record_validator_force_exit(addr);
                crate::metrics::record_pending_set_change(true);
                self.emit(IValidatorSet::ValidatorForcedExit {
                    validator: addr,
                    atHeight: history.last_deactivated_at_height().unwrap_or(0),
                })?;
            }
            ValidatorLifecycle::Unbonding | ValidatorLifecycle::Inactive => {
                info!(
                    target: "outbe::validatorset",
                    event = "validator_punish_noop",
                    validator = %addr,
                    status = current_status,
                    block_number,
                    "punish no-op: validator already in UNBONDING or INACTIVE",
                );
            }
            _ => unreachable!("unsupported punishment states were rejected before persistence"),
        }

        Ok(())
    }

    /// Unjails a JAILED validator back to PENDING. Called by Staking's
    /// `unjailValidator` (which first verifies the caller's stake ≥ min_stake);
    /// the caller must be the validator itself. Enforces the unjail cooldown,
    /// resets the stale-join readiness flag (the node must re-confirm before the
    /// next reshare) and the per-epoch miss metrics, and signals a reshare so the
    /// normal PENDING → ACTIVE promotion runs.
    pub fn unjail_after_stake_check(&mut self, addr: Address) -> Result<()> {
        let before = self.validator_state(addr)?;
        let jailed = match before.lifecycle().phase().clone() {
            ValidatorLifecycle::Jailed(jailed) => jailed,
            ValidatorLifecycle::Unregistered => {
                return Err(PrecompileError::Revert("validator not registered".into()));
            }
            lifecycle => {
                return Err(PrecompileError::Revert(format!(
                    "unjailValidator requires JAILED status, got {}",
                    registered_status(&lifecycle)?
                )));
            }
        };
        let block_number = self.storage.block_number()?;
        let jailed_at = jailed.jailed_at();
        let cooldown = self.unjail_cooldown_blocks()?;
        let ready_at = jailed_at.saturating_add(cooldown);
        if block_number < ready_at {
            return Err(PrecompileError::Revert(format!(
                "unjail cooldown not elapsed: jailed_at {jailed_at} + cooldown {cooldown} = {ready_at}, current {block_number}"
            )));
        }

        let pending = jailed.unjail();
        let history = before.history().copied().ok_or_else(|| {
            PrecompileError::Fatal("registered validator is missing history".into())
        })?;
        let after = before
            .clone()
            .with_lifecycle(ValidatorLifecycle::Pending(pending))
            .with_history(ValidatorHistory::new(
                history.joined_at_height(),
                history.last_deactivated_at_height(),
                history.slash_count(),
                0,
                0,
                history.blocks_proposed(),
            ));
        self.persist_validator_state_delta(&before, &after)?;

        // Re-joining via PENDING: must re-confirm readiness (stale-join guard) and
        // start from a clean per-epoch miss slate so stale counts cannot trip a
        // felony immediately on return.
        self.pending_set_change.write(true)?;

        crate::metrics::record_validator_status(addr, status::PENDING);
        crate::metrics::record_pending_set_change(true);

        self.emit(IValidatorSet::ValidatorUnjailed {
            validator: addr,
            atHeight: block_number,
        })?;
        Ok(())
    }

    /// Compatibility wrapper retained while callers migrate to the named
    /// Staking-checked transition.
    pub fn unjail_to_pending(&mut self, addr: Address) -> Result<()> {
        self.unjail_after_stake_check(addr)
    }

    /// Unjail cooldown in blocks (default 0 — immediate unjail allowed).
    pub fn unjail_cooldown_blocks(&self) -> Result<u64> {
        self.config_unjail_cooldown_blocks.read()
    }

    /// Compatibility entrypoint for direct Rust callers. The owner ABI routes
    /// through the explicitly named legacy path; consensus uses the validated
    /// boundary path in `hooks`.
    pub fn activate_reshared_set(
        &mut self,
        new_active_set: &[Address],
        active_set_hash: B256,
    ) -> Result<()> {
        self.legacy_activate_reshared_set(new_active_set, active_set_hash)
    }

    /// Existing permissive owner behavior (S-02/S-03), retained for ABI
    /// compatibility and deliberately kept separate from consensus orchestration.
    pub(crate) fn legacy_activate_reshared_set(
        &mut self,
        new_active_set: &[Address],
        active_set_hash: B256,
    ) -> Result<()> {
        self.apply_reshared_set(new_active_set, active_set_hash, &[])
    }

    /// Applies inputs already validated against the locally expected consensus
    /// boundary artifact. Snapshot/hash validation remains in the EVM boundary
    /// orchestrator; this method owns only the ValidatorSet state transition.
    pub(crate) fn activate_validated_boundary_set(
        &mut self,
        new_active_set: &[Address],
        active_set_hash: B256,
    ) -> Result<()> {
        self.apply_reshared_set(new_active_set, active_set_hash, &[])
    }

    /// Applies a validated boundary and its certified TEE-expiry exclusions.
    pub(crate) fn activate_validated_boundary_set_with_expiry_exclusions(
        &mut self,
        new_active_set: &[Address],
        active_set_hash: B256,
        tee_expired_target_exclusions: &[Address],
    ) -> Result<()> {
        if tee_expired_target_exclusions.is_empty() {
            return self.activate_validated_boundary_set(new_active_set, active_set_hash);
        }
        if tee_expired_target_exclusions.len()
            > outbe_primitives::validators::MAX_TEE_EXPIRED_TARGET_EXCLUSIONS
        {
            return Err(PrecompileError::Fatal(format!(
                "TEE expiry exclusions exceed protocol cap: {} > {}",
                tee_expired_target_exclusions.len(),
                outbe_primitives::validators::MAX_TEE_EXPIRED_TARGET_EXCLUSIONS
            )));
        }
        let mut unique_exclusions = std::collections::BTreeSet::new();
        for address in tee_expired_target_exclusions {
            if address.is_zero() || !unique_exclusions.insert(*address) {
                return Err(PrecompileError::Fatal(
                    "TEE expiry exclusions must contain unique non-zero validators".into(),
                ));
            }
            if new_active_set.contains(address) {
                return Err(PrecompileError::Fatal(format!(
                    "TEE-expired validator {address} is also present in the new active set"
                )));
            }
            if self.address_to_index.read(address)? == 0 {
                return Err(PrecompileError::Fatal(format!(
                    "TEE expiry exclusions contain unregistered validator {address}"
                )));
            }
        }
        self.apply_reshared_set(
            new_active_set,
            active_set_hash,
            tee_expired_target_exclusions,
        )
    }

    fn apply_reshared_set(
        &mut self,
        new_active_set: &[Address],
        active_set_hash: B256,
        tee_expired_target_exclusions: &[Address],
    ) -> Result<()> {
        let active_count: u32 = new_active_set
            .len()
            .try_into()
            .map_err(|_| PrecompileError::Revert("active set count exceeds u32".into()))?;
        let addresses = self.registered_validator_addresses()?;
        let mut states = Vec::with_capacity(addresses.len());
        for addr in addresses {
            states.push(self.validator_state(addr)?);
        }

        // Validate the complete input before the first write. Duplicate members,
        // non-canonical order, and caller-supplied hashes intentionally remain
        // accepted by the legacy ABI (S-02/S-03 compatibility behavior).
        for addr in new_active_set {
            let Some(state) = states.iter().find(|state| state.address() == *addr) else {
                return Err(PrecompileError::Revert(format!(
                    "reshared active set contains unregistered validator {addr}"
                )));
            };
            if !matches!(
                state.lifecycle().phase(),
                ValidatorLifecycle::Registered
                    | ValidatorLifecycle::Pending(_)
                    | ValidatorLifecycle::Active(_)
            ) {
                return Err(PrecompileError::Revert(format!(
                    "reshared active set contains validator {addr} with non-active status {}",
                    registered_status(state.lifecycle())?
                )));
            }
        }

        let mut transitions = Vec::with_capacity(states.len());
        let mut transitioned_to_unbonding = Vec::new();
        let mut tee_expired_active = Vec::new();
        let mut tee_expired_pending = Vec::new();
        for before in states {
            let included = new_active_set.contains(&before.address());
            let tee_expired = tee_expired_target_exclusions.contains(&before.address());
            let lifecycle = before.lifecycle().clone();
            let phase = lifecycle.phase().clone();
            let next_lifecycle = if tee_expired && matches!(phase, ValidatorLifecycle::Active(_)) {
                tee_expired_active.push(before.address());
                lifecycle
                    .replace_phase(ValidatorLifecycle::Pending(
                        PendingState::AwaitingConfirmation,
                    ))
                    .without_readiness_residue()
                    .without_share_residue()
            } else if tee_expired && matches!(phase, ValidatorLifecycle::Pending(_)) {
                tee_expired_pending.push(before.address());
                lifecycle
                    .replace_phase(ValidatorLifecycle::Pending(
                        PendingState::AwaitingConfirmation,
                    ))
                    .without_readiness_residue()
                    .without_share_residue()
            } else if included {
                match phase {
                    ValidatorLifecycle::Registered => lifecycle
                        .replace_phase(ValidatorLifecycle::Active(ActiveState::Participating))
                        .without_readiness_residue()
                        .without_share_residue(),
                    ValidatorLifecycle::Pending(pending) => lifecycle
                        .replace_phase(ValidatorLifecycle::Active(pending.activate_at_boundary()))
                        .without_readiness_residue()
                        .without_share_residue(),
                    ValidatorLifecycle::Active(active) => lifecycle
                        .replace_phase(ValidatorLifecycle::Active(active.included_at_boundary()))
                        .without_share_residue(),
                    _ => unreachable!("new-set status was validated before planning"),
                }
            } else {
                match phase {
                    ValidatorLifecycle::Active(active) => lifecycle
                        .replace_phase(ValidatorLifecycle::Active(active.omitted_at_boundary()))
                        .without_share_residue(),
                    ValidatorLifecycle::Exiting(exiting) => {
                        transitioned_to_unbonding.push(before.address());
                        lifecycle
                            .replace_phase(exiting.excluded_at_boundary())
                            .without_share_residue()
                    }
                    ValidatorLifecycle::Jailed(jailed) => lifecycle
                        .replace_phase(ValidatorLifecycle::Jailed(jailed.excluded_at_boundary()))
                        .without_share_residue(),
                    ValidatorLifecycle::Pending(
                        PendingState::AwaitingConfirmationWithRetainedShare,
                    ) => lifecycle
                        .replace_phase(ValidatorLifecycle::Pending(
                            PendingState::AwaitingConfirmation,
                        ))
                        .without_share_residue(),
                    ValidatorLifecycle::Pending(PendingState::ConfirmedWithRetainedShare) => {
                        lifecycle
                            .replace_phase(ValidatorLifecycle::Pending(PendingState::Confirmed))
                            .without_share_residue()
                    }
                    _ => lifecycle.without_share_residue(),
                }
            };
            let after = before.clone().with_lifecycle(next_lifecycle);
            transitions.push((before, after));
        }

        let all_covered = transitions.iter().all(|(_, after)| {
            !matches!(
                after.lifecycle().phase(),
                ValidatorLifecycle::Active(ActiveState::AwaitingShareRepair)
            )
        });

        // The planner above performs every fallible semantic check before this
        // checkpoint. Storage writes, hash, repair flag, and event commit as one
        // bundle even for direct legacy calls.
        let guard = self.storage.checkpoint_guard();
        for (before, after) in &transitions {
            self.persist_validator_state_delta(before, after)?;
        }
        self.active_consensus_set_hash.write(active_set_hash)?;
        self.pending_set_change.write(!all_covered)?;
        self.emit(IValidatorSet::ConsensusSetUpdated {
            activeCount: active_count,
        })?;
        guard.commit();

        crate::metrics::record_reshared_set_activated(
            active_count,
            transitioned_to_unbonding.len(),
        );
        crate::metrics::record_pending_set_change(!all_covered);
        for addr in new_active_set {
            crate::metrics::record_validator_status(*addr, status::ACTIVE);
        }
        for addr in &transitioned_to_unbonding {
            crate::metrics::record_validator_status(*addr, status::UNBONDING);
        }
        for addr in &tee_expired_active {
            crate::metrics::record_validator_status(*addr, status::PENDING);
            crate::metrics::record_validator_tee_expiry(*addr, "active_demoted");
        }
        for addr in &tee_expired_pending {
            crate::metrics::record_validator_tee_expiry(*addr, "pending_cleared");
        }
        crate::metrics::record_tee_expiry_exclusions(
            tee_expired_active.len(),
            tee_expired_pending.len(),
        );

        let block_number = self.storage.block_number().unwrap_or(0);
        journal_record(JournalRecord::ResharedSetActivated {
            wall_clock: iso8601_now(),
            block_number,
            active_count,
            transitioned_to_unbonding: transitioned_to_unbonding.len() as u64,
            pending_set_change: !all_covered,
            active_set_hash: format!("{active_set_hash:?}"),
        });
        for addr in &transitioned_to_unbonding {
            journal_record(JournalRecord::ValidatorUnbonding {
                wall_clock: iso8601_now(),
                block_number,
                validator: format!("{addr:?}"),
            });
        }

        let mut active = 0usize;
        let mut exiting = 0usize;
        let mut unbonding = 0usize;
        for (_, after) in &transitions {
            match after.lifecycle().phase() {
                ValidatorLifecycle::Active(_) => active += 1,
                ValidatorLifecycle::Exiting(_) => exiting += 1,
                ValidatorLifecycle::Unbonding => unbonding += 1,
                _ => {}
            }
        }
        crate::metrics::record_aggregate_status_counts(active, exiting, unbonding);

        info!(
            target: "outbe::validatorset",
            event = "reshared_set_activated",
            active_count,
            transitioned_to_unbonding = transitioned_to_unbonding.len(),
            pending_set_change = !all_covered,
            block_number,
            active_set_hash = %active_set_hash,
            "DKG reshare activated; new active set committed",
        );
        for addr in &transitioned_to_unbonding {
            info!(
                target: "outbe::validatorset",
                event = "validator_unbonding",
                validator = %addr,
                block_number,
                "validator transitioned EXITING -> UNBONDING (excluded from new set)",
            );
        }
        for addr in &tee_expired_active {
            warn!(
                target: "outbe::validatorset",
                event = "validator_tee_expired_demoted",
                validator = %addr,
                block_number = self.storage.block_number().unwrap_or(0),
                "certified freeze-height TEE expiry demoted ACTIVE validator to PENDING"
            );
        }
        for addr in &tee_expired_pending {
            warn!(
                target: "outbe::validatorset",
                event = "validator_tee_expired_readiness_cleared",
                validator = %addr,
                block_number = self.storage.block_number().unwrap_or(0),
                "certified freeze-height TEE expiry cleared PENDING validator readiness"
            );
        }

        Ok(())
    }

    /// Records a block proposal by the given validator.
    ///
    /// Increments `blocks_proposed` for a current consensus participant.
    pub fn record_proposer(&mut self, addr: Address) -> Result<()> {
        if !self.is_consensus_participant(addr)? {
            return Err(PrecompileError::Revert(format!(
                "proposer is not a current consensus participant: {addr}"
            )));
        }
        let proposed = self.val_blocks_proposed.read(&addr)?;
        self.val_blocks_proposed.write(&addr, proposed + 1)?;

        Ok(())
    }

    /// Records a missed block for the given validator.
    pub fn record_missed_block(&mut self, addr: Address) -> Result<()> {
        let missed = self.val_missed_blocks.read(&addr)?;
        self.val_missed_blocks.write(&addr, missed + 1)?;
        Ok(())
    }

    /// Records vote participation: increments `missed_votes` for each absent validator.
    pub fn record_participation(&mut self, voters: &[Address], absent: &[Address]) -> Result<()> {
        for addr in voters {
            if !self.is_consensus_participant(*addr)? {
                return Err(PrecompileError::Revert(format!(
                    "voter is not a current consensus participant: {addr}"
                )));
            }
        }
        for addr in absent {
            if !self.is_consensus_participant(*addr)? {
                return Err(PrecompileError::Revert(format!(
                    "absent voter is not a current consensus participant: {addr}"
                )));
            }
            let missed = self.val_missed_votes.read(addr)?;
            self.val_missed_votes.write(addr, missed + 1)?;
        }
        Ok(())
    }

    /// Records vote participation for a historical (finalized-parent) committee.
    ///
    /// Finalized-parent metadata describes a committee captured at a previous
    /// finalized block. By the time it is applied here, some members may no
    /// longer be current consensus participants (e.g. transitioned to
    /// `UNBONDING` after a reshare). This entrypoint validates that every
    /// supplied address is a registered validator but does not require current
    /// `ACTIVE`/`EXITING` + `has_bls_share` membership.
    pub fn record_finalized_participation(
        &mut self,
        voters: &[Address],
        absent: &[Address],
    ) -> Result<()> {
        for addr in voters {
            if !self.is_validator(*addr)? {
                return Err(PrecompileError::Revert(format!(
                    "finalized voter is not a registered validator: {addr}"
                )));
            }
        }
        for addr in absent {
            if !self.is_validator(*addr)? {
                return Err(PrecompileError::Revert(format!(
                    "finalized absent voter is not a registered validator: {addr}"
                )));
            }
            let missed = self.val_missed_votes.read(addr)?;
            self.val_missed_votes.write(addr, missed + 1)?;
        }
        Ok(())
    }

    /// Transitions to a new epoch.
    ///
    /// Resets per-epoch counters for active/exiting validators, increments `epoch_number`,
    /// and updates the epoch start timestamp and block.
    ///
    /// NOTE: O(n) scan over all validators. Acceptable because epoch transitions
    /// happen every configured epoch length in blocks.
    pub fn update_epoch(&mut self, timestamp: u64, block_number: u64) -> Result<()> {
        for addr in self.registered_validator_addresses()? {
            // Only reset counters for validators that accumulate them.
            // Include EXITING — they still participate in consensus
            // until reshare completes and accumulate per-epoch counters.
            // JAILED is likewise still in the live committee until the next
            // reshare clears its share, so reset its counters too.
            if !matches!(
                self.validator_lifecycle(addr)?.phase(),
                ValidatorLifecycle::Active(_)
                    | ValidatorLifecycle::Exiting(_)
                    | ValidatorLifecycle::Jailed(_)
            ) {
                continue;
            }
            self.val_missed_blocks.write(&addr, 0)?;
            self.val_missed_votes.write(&addr, 0)?;
            self.val_blocks_proposed.write(&addr, 0)?;
        }

        let epoch = self.epoch_number.read()?;
        let new_epoch = epoch + U256::from(1);
        self.epoch_number.write(new_epoch)?;
        self.epoch_start_timestamp.write(timestamp)?;
        self.epoch_start_block.write(block_number)?;

        let active_count = self.active_validator_count()?;
        self.emit(IValidatorSet::EpochTransition {
            newEpochNumber: new_epoch,
            timestamp,
            activeValidatorCount: active_count,
        })?;

        Ok(())
    }

    /// Removes INACTIVE validator entries from the registry via swap-remove.
    ///
    /// `max_removals` caps how many entries are cleaned per call (0 = unlimited).
    /// Returns the number of entries removed.
    pub fn cleanup_inactive_validators(&mut self, max_removals: u32) -> Result<u32> {
        let mut count = self.validator_count.read()?;
        let mut removed = 0u32;
        let mut i = 1u64;

        while i <= count as u64 {
            if max_removals > 0 && removed >= max_removals {
                break;
            }
            let addr = self.index_to_address.read(&i)?;
            if addr.is_zero() {
                i += 1;
                continue;
            }
            if !matches!(
                self.validator_lifecycle(addr)?.phase(),
                ValidatorLifecycle::Inactive
            ) {
                i += 1;
                continue;
            }

            // Clear all per-validator storage
            self.clear_validator_storage(&addr)?;

            // Swap with last entry
            let count_u64 = count as u64;
            if i < count_u64 {
                let last_addr = self.index_to_address.read(&count_u64)?;
                self.index_to_address.write(&i, last_addr)?;
                self.address_to_index.write(&last_addr, i)?;
            }
            // Clear the last slot
            self.index_to_address.write(&count_u64, Address::ZERO)?;
            self.address_to_index.write(&addr, 0)?;
            count -= 1;
            removed += 1;
            // Don't increment i — the swapped-in entry needs checking
        }

        self.validator_count.write(count)?;
        Ok(removed)
    }

    /// Clears all per-validator storage fields for an address.
    fn clear_validator_storage(&mut self, addr: &Address) -> Result<()> {
        let pubkey = self.read_consensus_pubkey(addr)?;
        let pk_hash = Self::consensus_pubkey_hash(&pubkey);
        self.consensus_pubkey_hash_to_address
            .write(&pk_hash, Address::ZERO)?;

        self.write_consensus_pubkey(addr, &[0u8; 48])?;
        self.val_stake.write(addr, U256::ZERO)?;
        self.val_status.write(addr, 0)?;
        self.val_slash_count.write(addr, 0)?;
        self.val_missed_blocks.write(addr, 0)?;
        self.val_missed_votes.write(addr, 0)?;
        self.val_blocks_proposed.write(addr, 0)?;
        self.val_joined_at_height.write(addr, 0)?;
        self.val_deactivated_at_height.write(addr, 0)?;
        self.val_unbonding_end.write(addr, 0)?;
        self.val_has_bls_share.write(addr, false)?;
        self.val_p2p_address_version.write(addr, 0)?;
        self.val_p2p_address_payload.get_bytes(addr).clear()?;
        // Stale-join + jail per-validator state must be cleared too, so a future
        // re-registration at the same address starts clean (a leaked
        // `val_join_confirmed = true` would bypass the stale-join guard).
        self.val_join_confirmed.write(addr, false)?;
        self.val_jailed_at_height.write(addr, 0)?;
        Ok(())
    }

    /// Returns `true` if the address is a registered validator.
    pub fn is_validator(&self, addr: Address) -> Result<bool> {
        let index = self.address_to_index.read(&addr)?;
        Ok(index > 0)
    }

    /// Looks up a validator address by consensus pubkey hash.
    ///
    /// The hash is `keccak256(48-byte BLS MinPk pubkey)`.
    pub fn lookup_by_pubkey_hash(&self, pubkey_hash: B256) -> Result<Address> {
        self.consensus_pubkey_hash_to_address.read(&pubkey_hash)
    }
}

/// Verifies a BLS MinPk registration signature.
///
/// Uses the `blst` crate directly to verify the signature without needing
/// the full commonware cryptography stack in the EVM precompile crate.
///
/// The signed message is the validator's Ethereum address (20 bytes).
/// The domain separation tag (DST) is "BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_outbe_REGISTER".
fn verify_bls_registration_sig(
    pubkey_bytes: &[u8; 48],
    sig_bytes: &[u8; 96],
    validator_addr: &Address,
) -> Result<()> {
    use blst::min_pk::{PublicKey, Signature};
    use blst::BLST_ERROR;

    let pk = PublicKey::from_bytes(pubkey_bytes)
        .map_err(|_| PrecompileError::Revert("invalid BLS public key".into()))?;
    let sig = Signature::from_bytes(sig_bytes)
        .map_err(|_| PrecompileError::Revert("invalid BLS signature".into()))?;

    let dst = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_NUL_outbe_REGISTER";
    let result = sig.verify(true, validator_addr.as_slice(), dst, &[], &pk, true);
    if result != BLST_ERROR::BLST_SUCCESS {
        return Err(PrecompileError::Revert(
            "invalid BLS registration signature".into(),
        ));
    }
    Ok(())
}
