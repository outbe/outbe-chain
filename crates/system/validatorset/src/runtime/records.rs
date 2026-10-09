use crate::schema::ValidatorSet;
use crate::state_machine::{
    HistoryCounters, P2pInfo, StakeProjection, StoredValidatorFields, ValidatorHistory,
    ValidatorLifecycle, ValidatorState,
};
use alloy_primitives::{keccak256, Address, B256};
use outbe_ocomp_protocol::{committee::OcompKeyRegistrationV1, profile::poc_schema_limits};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::{Mapping, Storable};
use std::num::NonZeroU64;

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
        let fields = self.read_stored_fields(addr)?;
        let state = ValidatorState::decode_stored(addr, fields)?;
        if let Some(index) = state.registry_index() {
            self.ensure_registry_bindings(addr, index.get(), &state)?;
        }
        Ok(state)
    }

    /// Reads the raw validator columns in their storage order.
    fn read_stored_fields(&self, addr: Address) -> Result<StoredValidatorFields> {
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
            HistoryCounters {
                slash_count,
                missed_blocks,
                missed_votes,
                blocks_proposed,
            },
        );
        Ok(StoredValidatorFields {
            registry_index,
            consensus_pubkey,
            stake,
            stored_status,
            p2p_version,
            p2p_payload,
            history,
            has_bls_share,
            join_confirmed,
            jailed_at,
        })
    }

    /// Checks the dense index bound, the forward index and the consensus-key
    /// reverse mapping of a registered validator, in that order.
    fn ensure_registry_bindings(
        &self,
        addr: Address,
        index: u64,
        state: &ValidatorState,
    ) -> Result<()> {
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
        Ok(())
    }

    /// Returns the lifecycle from the fully validated validator aggregate.
    ///
    /// Hydrating the complete aggregate is intentional. Lifecycle payloads own
    /// registry identity, stake, P2P data, and history. Every query therefore
    /// observes the same coupled-field and index invariants.
    pub fn validator_lifecycle(&self, addr: Address) -> Result<ValidatorLifecycle> {
        Ok(self.validator_state(addr)?.into_lifecycle())
    }

    /// Writes only changed fields of an already-decoded validator aggregate.
    /// This function deliberately excludes registry identity: registration,
    /// re-registration, and cleanup own the dense-index and consensus-key invariants.
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
        let identity_changed = if allow_registry_identity_change {
            before.address() != after.address()
        } else {
            registry_identity(before) != registry_identity(after)
        };
        if identity_changed {
            return Err(PrecompileError::Fatal(
                "validator lifecycle transition attempted to change registry identity".into(),
            ));
        }

        if allow_registry_identity_change {
            self.persist_registry_identity(before, after)?;
        }
        let addr = after.address();
        write_changed(
            &self.val_stake,
            &addr,
            before.bonded_stake(),
            after.bonded_stake(),
        )?;
        if before.stored_status() != after.stored_status() {
            let status = after.stored_status().ok_or_else(|| {
                PrecompileError::Fatal(
                    "generic validator persistence cannot write an absent lifecycle".into(),
                )
            })?;
            self.val_status.write(&addr, status)?;
        }
        self.persist_history_delta(&addr, before.history(), after.history())?;
        self.persist_lifecycle_flags(&addr, before, after)?;
        self.persist_p2p_delta(&addr, before, after)
    }

    /// Writes a changed consensus key of a registry transition. The new state
    /// must keep a key and a registry index.
    fn persist_registry_identity(
        &mut self,
        before: &ValidatorState,
        after: &ValidatorState,
    ) -> Result<()> {
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
        Ok(())
    }

    /// Writes the changed history columns: slash count, missed blocks, missed
    /// votes, blocks proposed, join height and deactivation height.
    fn persist_history_delta(
        &self,
        addr: &Address,
        before: Option<&ValidatorHistory>,
        after: Option<&ValidatorHistory>,
    ) -> Result<()> {
        let column = |history: Option<&ValidatorHistory>, read: fn(&ValidatorHistory) -> u64| {
            history.map_or(0, read)
        };
        let deactivated_at =
            |history: &ValidatorHistory| history.last_deactivated_at_height().unwrap_or(0);
        let columns: [HistoryColumn<'_, '_>; 6] = [
            (&self.val_slash_count, ValidatorHistory::slash_count),
            (&self.val_missed_blocks, ValidatorHistory::missed_blocks),
            (&self.val_missed_votes, ValidatorHistory::missed_votes),
            (&self.val_blocks_proposed, ValidatorHistory::blocks_proposed),
            (
                &self.val_joined_at_height,
                ValidatorHistory::joined_at_height,
            ),
            (&self.val_deactivated_at_height, deactivated_at),
        ];
        for (map, read) in columns {
            write_changed(map, addr, column(before, read), column(after, read))?;
        }
        Ok(())
    }

    /// Writes the changed unbonding hint, BLS-share flag, readiness flag and
    /// jail height.
    fn persist_lifecycle_flags(
        &self,
        addr: &Address,
        before: &ValidatorState,
        after: &ValidatorState,
    ) -> Result<()> {
        write_changed(
            &self.val_unbonding_end,
            addr,
            before.unbonding_end_hint().unwrap_or(0),
            after.unbonding_end_hint().unwrap_or(0),
        )?;
        write_changed(
            &self.val_has_bls_share,
            addr,
            before.has_bls_share(),
            after.has_bls_share(),
        )?;
        write_changed(
            &self.val_join_confirmed,
            addr,
            before.join_confirmed(),
            after.join_confirmed(),
        )?;
        write_changed(
            &self.val_jailed_at_height,
            addr,
            before.stored_jailed_at(),
            after.stored_jailed_at(),
        )
    }

    /// Writes a changed P2P version, then its payload.
    fn persist_p2p_delta(
        &self,
        addr: &Address,
        before: &ValidatorState,
        after: &ValidatorState,
    ) -> Result<()> {
        let before_p2p = before.p2p().map_or((0, Vec::new()), P2pInfo::encode_stored);
        let after_p2p = after.p2p().map_or((0, Vec::new()), P2pInfo::encode_stored);
        if before_p2p == after_p2p {
            return Ok(());
        }
        self.val_p2p_address_version.write(addr, after_p2p.0)?;
        if after_p2p.1.is_empty() {
            self.val_p2p_address_payload.get_bytes(addr).clear()
        } else {
            self.val_p2p_address_payload
                .get_bytes(addr)
                .write(&after_p2p.1)
        }
    }

    /// Persists one validator transition in its own checkpoint. With
    /// `signal_set_change`, the same checkpoint also raises the pending
    /// set-change flag after the validator fields.
    pub(super) fn commit_transition(
        &mut self,
        before: &ValidatorState,
        after: &ValidatorState,
        signal_set_change: bool,
    ) -> Result<()> {
        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(before, after)?;
        if signal_set_change {
            self.pending_set_change.write(true)?;
        }
        guard.commit();
        Ok(())
    }

    /// Returns the keccak256 hash of a 48-byte consensus pubkey (for reverse lookup).
    pub fn consensus_pubkey_hash(pubkey: &[u8; 48]) -> B256 {
        keccak256(pubkey)
    }
}

/// The address, registry index and consensus key of a validator state.
fn registry_identity(state: &ValidatorState) -> (Address, Option<NonZeroU64>, Option<&[u8; 48]>) {
    (
        state.address(),
        state.registry_index(),
        state.consensus_pubkey(),
    )
}

/// One history column: its storage mapping and the history field it stores.
type HistoryColumn<'map, 'storage> = (
    &'map Mapping<'storage, Address, u64>,
    fn(&ValidatorHistory) -> u64,
);

/// Writes `after` to `map[addr]` when it differs from `before`.
fn write_changed<V: Storable + PartialEq>(
    map: &Mapping<'_, Address, V>,
    addr: &Address,
    before: V,
    after: V,
) -> Result<()> {
    if before != after {
        map.write(addr, after)?;
    }
    Ok(())
}
