use super::{status, EpochSnapshot, ValidatorParticipation, ValidatorRecord};
use crate::schema::ValidatorSet;
use crate::state_machine::{
    P2pInfo, StakeProjection, ValidatorHistory, ValidatorLifecycle, ValidatorState,
};
use alloy_primitives::{keccak256, Address, B256};
use outbe_ocomp_protocol::{committee::OcompKeyRegistrationV1, profile::poc_schema_limits};
use outbe_primitives::error::{PrecompileError, Result};

impl ValidatorSet<'_> {
    /// Returns the canonical OCOMP registration admitted for `addr`.
    /// Stored bytes that no longer decode are consensus-state corruption.
    pub fn ocomp_registration(&self, addr: Address) -> Result<Option<OcompKeyRegistrationV1>> {
        let bytes = self.val_ocomp_registration.get_bytes(&addr).read()?;
        if bytes.is_empty() {
            return Ok(None);
        }
        OcompKeyRegistrationV1::decode_canonical(&bytes, &poc_schema_limits())
            .map(Some)
            .map_err(|error| {
                PrecompileError::Fatal(format!(
                    "stored OCOMP registration for {addr} is invalid: {error}"
                ))
            })
    }
}

impl ValidatorSet<'_> {
    /// Reads the 48-byte BLS MinPk consensus pubkey from two storage slots.
    pub(super) fn read_consensus_pubkey(&self, addr: &Address) -> Result<[u8; 48]> {
        let lo: B256 = self.val_consensus_pubkey_lo.read(addr)?;
        let hi: B256 = self.val_consensus_pubkey_hi.read(addr)?;
        let mut pubkey = [0u8; 48];
        pubkey[..32].copy_from_slice(&lo.0);
        pubkey[32..48].copy_from_slice(&hi.0[..16]);
        Ok(pubkey)
    }

    /// Writes the 48-byte BLS MinPk consensus pubkey across two storage slots.
    pub(super) fn write_consensus_pubkey(
        &mut self,
        addr: &Address,
        pubkey: &[u8; 48],
    ) -> Result<()> {
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
    /// Unlike [`Self::get_validator`], this also represents an absent address as
    /// [`ValidatorLifecycle::Absent`]. Unknown status bytes and malformed coupled
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

        let history = ValidatorHistory::new(
            joined_at_height,
            (deactivated_at_height != 0).then_some(deactivated_at_height),
            slash_count,
            missed_blocks,
            missed_votes,
            blocks_proposed,
        );
        let state = ValidatorState::decode_stored(
            addr,
            registry_index,
            consensus_pubkey,
            stake,
            stored_status,
            p2p_version,
            &p2p_payload,
            history,
            has_bls_share,
            join_confirmed,
            jailed_at,
        )?;

        if let Some(index) = state.registry_index() {
            let index = index.get();
            let count = u64::from(self.validator_count.read()?);
            if index > count {
                return Err(PrecompileError::Fatal(format!(
                    "validator {addr} registry index {index} exceeds validator_count {count}"
                )));
            }
            let indexed_address = self.index_to_address.read(&index)?;
            if indexed_address != addr {
                return Err(PrecompileError::Fatal(format!(
                    "validator registry forward index mismatch at {index}: expected {addr}, got {indexed_address}"
                )));
            }
            let pubkey = state.consensus_pubkey().ok_or_else(|| {
                PrecompileError::Fatal("registered validator is missing consensus pubkey".into())
            })?;
            let pubkey_owner = self
                .consensus_pubkey_hash_to_address
                .read(&Self::consensus_pubkey_hash(pubkey))?;
            if pubkey_owner != addr {
                return Err(PrecompileError::Fatal(format!(
                    "validator consensus pubkey reverse mapping mismatch for {addr}: got {pubkey_owner}"
                )));
            }
        }

        Ok(state)
    }

    /// Returns the lifecycle from the fully validated validator aggregate.
    ///
    /// Hydrating the complete aggregate is intentional: lifecycle payloads own
    /// registry identity, stake, P2P data, and history, so every query observes
    /// the same coupled-field and index invariants.
    pub fn validator_lifecycle(&self, addr: Address) -> Result<ValidatorLifecycle> {
        Ok(self.validator_state(addr)?.into_lifecycle())
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

    pub(super) fn persist_registry_state_delta(
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
        if before.bonded_stake() != after.bonded_stake() {
            self.val_stake.write(&addr, after.bonded_stake())?;
        }
        if before.stored_status() != after.stored_status() {
            let status = after.stored_status().ok_or_else(|| {
                PrecompileError::Fatal(
                    "generic validator persistence cannot write an absent lifecycle".into(),
                )
            })?;
            self.val_status.write(&addr, status)?;
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
        let before_unbonding_end = before.unbonding_end_hint().unwrap_or(0);
        let after_unbonding_end = after.unbonding_end_hint().unwrap_or(0);
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
        let before_p2p = before.p2p().map_or((0, Vec::new()), P2pInfo::encode_stored);
        let after_p2p = after.p2p().map_or((0, Vec::new()), P2pInfo::encode_stored);
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
        let state = self.validator_state(addr)?;
        let stored_status = state.stored_status().ok_or_else(|| {
            PrecompileError::Fatal("cannot project absent validator into ValidatorRecord".into())
        })?;
        let history = state.history().ok_or_else(|| {
            PrecompileError::Fatal("registered validator is missing history".into())
        })?;
        let consensus_pubkey = state.consensus_pubkey().copied().ok_or_else(|| {
            PrecompileError::Fatal("registered validator is missing consensus public key".into())
        })?;
        Ok(ValidatorRecord {
            validator_address: addr,
            consensus_pubkey,
            stake: state.bonded_stake(),
            status: stored_status,
            slash_count: history.slash_count(),
            missed_blocks: history.missed_blocks(),
            missed_votes: history.missed_votes(),
            blocks_proposed: history.blocks_proposed(),
            joined_at_height: history.joined_at_height(),
            deactivated_at_height: history.last_deactivated_at_height().unwrap_or(0),
            unbonding_end: state.unbonding_end_hint().unwrap_or(0),
            has_bls_share: state.has_bls_share(),
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

    /// Returns validators eligible for the NEXT consensus committee: `Active`
    /// plus readiness-confirmed `Joining`. `WaitingForReadiness`, `Exiting`, and
    /// both jailed phases are excluded. Boundary activation grants each included
    /// joiner a share while changing it to `Active` atomically.
    pub fn get_reshare_target_set(&self) -> Result<Vec<ValidatorRecord>> {
        let all = self.get_validators_matching(ValidatorLifecycle::is_reshare_target)?;
        let mut target = Vec::new();
        for v in all {
            if v.status == status::ACTIVE
                || (v.status == status::PENDING
                    && self.ocomp_registration(v.validator_address)?.is_some())
            {
                target.push(v);
            }
        }
        Ok(target)
    }

    /// Returns validators with `status == PENDING` - staked joiners admitted to the
    /// validator set but not yet granted a threshold share. Used to admit them to
    /// consensus P2P as SECONDARY peers so they can sync to head before the reshare
    /// that makes them signers; they are NOT consensus participants (no share).
    pub fn get_pending_validators(&self) -> Result<Vec<ValidatorRecord>> {
        self.get_validators_matching(ValidatorLifecycle::is_pending)
    }

    /// Returns validators admitted to consensus P2P as secondary peers:
    /// `WaitingForStake`, both pending phases, and both jailed phases. This view
    /// controls network admission only; the current-participant predicate remains
    /// independently gated by a live share. Peers without P2P information are
    /// dropped downstream.
    pub fn get_admitted_non_consensus_validators(&self) -> Result<Vec<ValidatorRecord>> {
        self.get_validators_matching(ValidatorLifecycle::is_secondary_admission)
    }

    /// Returns validators in the current consensus set.
    ///
    /// `Exiting` and `JailRetained` validators retain consensus accountability
    /// until a successful boundary excludes them and clears their BLS share.
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
        Ok(self.validator_state(addr)?.consensus_pubkey().copied())
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

    /// Returns `true` if the address is a registered validator.
    pub fn is_validator(&self, addr: Address) -> Result<bool> {
        Ok(self.validator_state(addr)?.is_registered())
    }

    /// Looks up a validator address by consensus pubkey hash.
    ///
    /// The hash is `keccak256(48-byte BLS MinPk pubkey)`.
    pub fn lookup_by_pubkey_hash(&self, pubkey_hash: B256) -> Result<Address> {
        self.consensus_pubkey_hash_to_address.read(&pubkey_hash)
    }

    /// Returns the Radicle NodeId bound to `validator`, or zero when absent.
    pub fn get_radicle_node_id(&self, validator: Address) -> Result<B256> {
        self.val_radicle_node_id.read(&validator)
    }

    /// Returns the validator that owns `node_id`, or zero when unbound.
    pub fn validator_by_radicle_node_id(&self, node_id: B256) -> Result<Address> {
        self.radicle_node_id_to_validator.read(&node_id)
    }
}
