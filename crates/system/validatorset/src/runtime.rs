use alloy_primitives::{keccak256, Address, B256, U256};
use outbe_primitives::consensus_p2p::{
    validate_versioned, MAX_P2P_ADDRESS_ENCODED_LEN, P2P_ADDRESS_VERSION_V1,
};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::slashing_journal::{iso8601_now, record as journal_record, JournalRecord};
use tracing::{info, warn};

use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;

/// Validator status constants.
///
/// Lifecycle: Registered → Pending → Active → Exiting → Unbonding → Inactive.
///
pub mod status {
    pub const REGISTERED: u8 = 0;
    pub const PENDING: u8 = 1;
    pub const ACTIVE: u8 = 2;
    pub const EXITING: u8 = 3;
    pub const UNBONDING: u8 = 4;
    pub const INACTIVE: u8 = 5;
}

/// A fully-hydrated validator record read from storage.
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

impl ValidatorSet<'_> {
    fn is_current_consensus_participant_status(validator_status: u8, has_bls_share: bool) -> bool {
        matches!(validator_status, status::ACTIVE | status::EXITING) && has_bls_share
    }

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

    /// Returns the keccak256 hash of a 48-byte consensus pubkey (for reverse lookup).
    pub fn consensus_pubkey_hash(pubkey: &[u8; 48]) -> B256 {
        keccak256(pubkey)
    }

    /// Returns the full record for a given validator address, or `None` if not registered.
    pub fn get_validator(&self, addr: Address) -> Result<Option<ValidatorRecord>> {
        let index = self.address_to_index.read(&addr)?;
        if index == 0 {
            return Ok(None);
        }
        Ok(Some(ValidatorRecord {
            validator_address: addr,
            consensus_pubkey: self.read_consensus_pubkey(&addr)?,
            stake: self.val_stake.read(&addr)?,
            status: self.val_status.read(&addr)?,
            slash_count: self.val_slash_count.read(&addr)?,
            missed_blocks: self.val_missed_blocks.read(&addr)?,
            missed_votes: self.val_missed_votes.read(&addr)?,
            blocks_proposed: self.val_blocks_proposed.read(&addr)?,
            joined_at_height: self.val_joined_at_height.read(&addr)?,
            deactivated_at_height: self.val_deactivated_at_height.read(&addr)?,
            unbonding_end: self.val_unbonding_end.read(&addr)?,
            has_bls_share: self.val_has_bls_share.read(&addr)?,
        }))
    }

    /// Returns all registered validators, including inactive and exiting ones.
    pub fn get_all_validators(&self) -> Result<Vec<ValidatorRecord>> {
        let count = self.validator_count.read()?;
        let mut result = Vec::with_capacity(count as usize);
        for i in 1..=count as u64 {
            let addr = self.index_to_address.read(&i)?;
            if addr.is_zero() {
                continue;
            }
            if let Some(record) = self.get_validator(addr)? {
                result.push(record);
            }
        }
        Ok(result)
    }

    /// Returns only validators with `status == ACTIVE`.
    pub fn get_active_validators(&self) -> Result<Vec<ValidatorRecord>> {
        let all = self.get_all_validators()?;
        Ok(all
            .into_iter()
            .filter(|v| v.status == status::ACTIVE)
            .collect())
    }

    /// Returns validators in the current consensus set.
    ///
    /// EXITING validators retain current-epoch consensus accountability until a
    /// successful reshare excludes them and clears their BLS share.
    pub fn get_active_consensus_set(&self) -> Result<Vec<ValidatorRecord>> {
        let all = self.get_all_validators()?;
        Ok(all
            .into_iter()
            .filter(|v| Self::is_current_consensus_participant_status(v.status, v.has_bls_share))
            .collect())
    }

    /// Returns the number of active validators.
    pub fn active_validator_count(&self) -> Result<u32> {
        let all = self.get_all_validators()?;
        let count: u32 = all
            .iter()
            .filter(|v| v.status == status::ACTIVE)
            .count()
            .try_into()
            .map_err(|_| PrecompileError::Revert("active validator count exceeds u32".into()))?;
        Ok(count)
    }

    /// Returns the number of validators in the active consensus set.
    pub fn active_consensus_count(&self) -> Result<u32> {
        let all = self.get_all_validators()?;
        let count: u32 = all
            .iter()
            .filter(|v| Self::is_current_consensus_participant_status(v.status, v.has_bls_share))
            .count()
            .try_into()
            .map_err(|_| PrecompileError::Revert("active consensus count exceeds u32".into()))?;
        Ok(count)
    }

    /// Returns true if the validator is a current consensus participant.
    pub fn is_consensus_participant(&self, addr: Address) -> Result<bool> {
        let index = self.address_to_index.read(&addr)?;
        if index == 0 {
            return Ok(false);
        }
        let st = self.val_status.read(&addr)?;
        let has_bls = self.val_has_bls_share.read(&addr)?;
        Ok(Self::is_current_consensus_participant_status(st, has_bls))
    }

    /// Returns whether there is a pending validator set change that consensus should detect.
    pub fn has_pending_set_change(&self) -> Result<bool> {
        self.pending_set_change.read()
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
        validate_versioned(version, encoded)
            .map_err(|err| PrecompileError::Revert(format!("invalid p2p address: {err}")))?;

        self.val_p2p_address_version
            .write(&validator_addr, version)?;
        self.val_p2p_address_payload
            .get_bytes(&validator_addr)
            .write(encoded)?;
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

        // A-45: BLS proof-of-key-ownership is mandatory for self-registration.
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
            // Owner registration with optional signature verification
            verify_bls_registration_sig(consensus_pubkey, sig_bytes, &validator_addr)?;
        }

        // A-18: Verify BLS pubkey is not already used by another validator.
        // Without this check, two validators could register the same BLS key,
        // causing undefined behavior during DKG/reshare.
        let pk_hash = Self::consensus_pubkey_hash(consensus_pubkey);
        let existing_owner = self.consensus_pubkey_hash_to_address.read(&pk_hash)?;
        if !existing_owner.is_zero() && existing_owner != validator_addr {
            return Err(PrecompileError::Revert(
                "BLS consensus pubkey already registered by another validator".into(),
            ));
        }

        // Check not already registered (allow re-registration of INACTIVE validators)
        let existing_index = self.address_to_index.read(&validator_addr)?;
        if existing_index != 0 {
            let current_status = self.val_status.read(&validator_addr)?;
            if current_status != status::INACTIVE {
                return Err(PrecompileError::Revert(
                    "validator already registered".into(),
                ));
            }
            // Re-registration path: check cooldown then reuse existing index
            let cooldown = self.config_reregistration_cooldown.read()?;
            if cooldown > 0 {
                let deactivated_at = self.val_deactivated_at_height.read(&validator_addr)?;
                let current_height = self.storage.block_number()?;
                if deactivated_at > 0 && current_height < deactivated_at + cooldown as u64 {
                    return Err(PrecompileError::Revert(
                        "re-registration cooldown not expired".into(),
                    ));
                }
            }

            // Reset lifecycle metadata without changing stake accounting. Staking
            // remains the source of truth for stake and mirrors into val_stake.
            let old_pubkey = self.read_consensus_pubkey(&validator_addr)?;
            let old_pk_hash = Self::consensus_pubkey_hash(&old_pubkey);
            self.consensus_pubkey_hash_to_address
                .write(&old_pk_hash, Address::ZERO)?;

            self.write_consensus_pubkey(&validator_addr, consensus_pubkey)?;
            let pk_hash = Self::consensus_pubkey_hash(consensus_pubkey);
            self.consensus_pubkey_hash_to_address
                .write(&pk_hash, validator_addr)?;

            self.val_status.write(&validator_addr, status::REGISTERED)?;
            self.val_slash_count.write(&validator_addr, 0)?;
            self.val_missed_blocks.write(&validator_addr, 0)?;
            self.val_missed_votes.write(&validator_addr, 0)?;
            self.val_blocks_proposed.write(&validator_addr, 0)?;
            self.val_joined_at_height
                .write(&validator_addr, self.storage.block_number()?)?;
            self.val_deactivated_at_height.write(&validator_addr, 0)?;
            self.val_unbonding_end.write(&validator_addr, 0)?;
            self.val_has_bls_share.write(&validator_addr, false)?;
            self.val_p2p_address_version.write(&validator_addr, 0)?;
            self.val_p2p_address_payload
                .get_bytes(&validator_addr)
                .clear()?;

            self.pending_set_change.write(true)?;

            crate::metrics::record_validator_status(validator_addr, status::REGISTERED);
            crate::metrics::record_validator_register(validator_addr, true);
            crate::metrics::record_pending_set_change(true);

            journal_record(JournalRecord::ValidatorReregistered {
                wall_clock: iso8601_now(),
                block_number: self.storage.block_number().unwrap_or(0),
                validator: format!("{validator_addr:?}"),
                index: existing_index,
            });

            info!(
                target: "outbe::validatorset",
                event = "validator_reregistered",
                validator = %validator_addr,
                index = existing_index,
                block_number = self.storage.block_number().unwrap_or(0),
                "validator re-registered (was INACTIVE, lifecycle metadata reset)",
            );

            self.emit(IValidatorSet::ValidatorRegistered {
                validator: validator_addr,
                index: existing_index,
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

        // Store per-validator fields; initial status is REGISTERED
        self.write_consensus_pubkey(&validator_addr, consensus_pubkey)?;
        self.val_status.write(&validator_addr, status::REGISTERED)?;
        self.val_joined_at_height
            .write(&validator_addr, self.storage.block_number()?)?;

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

    /// Activates a registered validator (sets status to ACTIVE).
    ///
    /// A-21: Only REGISTERED and PENDING statuses are allowed as source states.
    /// Also signals `pending_set_change` so the consensus layer triggers a DKG
    /// reshare to include the newly-activated validator.
    pub fn activate_validator(&mut self, addr: Address) -> Result<()> {
        let index = self.address_to_index.read(&addr)?;
        if index == 0 {
            return Err(PrecompileError::Revert("validator not registered".into()));
        }
        let current_status = self.val_status.read(&addr)?;
        if current_status == status::ACTIVE {
            return Ok(()); // already active — no spurious churn
        }
        // A-21: Only REGISTERED and PENDING can transition to ACTIVE.
        // This prevents exiting/unbonding validators from bypassing
        // their lifecycle constraints.
        if current_status != status::REGISTERED && current_status != status::PENDING {
            return Err(PrecompileError::Revert(format!(
                "cannot activate validator with status {current_status}: only REGISTERED or PENDING allowed"
            )));
        }
        self.val_status.write(&addr, status::ACTIVE)?;
        self.val_deactivated_at_height.write(&addr, 0)?;

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
        let index = self.address_to_index.read(&addr)?;
        if index == 0 {
            return Err(PrecompileError::Revert("validator not registered".into()));
        }
        let current_status = self.val_status.read(&addr)?;
        if current_status != status::ACTIVE {
            return Err(PrecompileError::Revert(
                "can only deactivate an active validator".into(),
            ));
        }
        self.val_status.write(&addr, status::EXITING)?;
        let height = self.storage.block_number()?;
        self.val_deactivated_at_height.write(&addr, height)?;

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
        let index = self.address_to_index.read(&addr)?;
        if index == 0 {
            return Err(PrecompileError::Revert("validator not registered".into()));
        }

        let current_status = self.val_status.read(&addr)?;
        let block_number = self.storage.block_number()?;

        match current_status {
            status::ACTIVE => {
                self.val_status.write(&addr, status::EXITING)?;
                self.val_deactivated_at_height.write(&addr, block_number)?;
                self.pending_set_change.write(true)?;

                crate::metrics::record_validator_status(addr, status::EXITING);
                crate::metrics::record_validator_force_exit(addr);
                crate::metrics::record_pending_set_change(true);

                journal_record(JournalRecord::ValidatorForcedExit {
                    wall_clock: iso8601_now(),
                    block_number,
                    validator: format!("{addr:?}"),
                    status_before: "ACTIVE".into(),
                    status_after: "EXITING".into(),
                });

                warn!(
                    target: "outbe::validatorset",
                    event = "validator_force_exit",
                    validator = %addr,
                    status_before = "ACTIVE",
                    status_after = "EXITING",
                    block_number,
                    "validator force-exited from ACTIVE",
                );

                self.emit(IValidatorSet::ValidatorDeactivated {
                    validator: addr,
                    atHeight: block_number,
                })?;
                self.emit(IValidatorSet::ValidatorForcedExit {
                    validator: addr,
                    atHeight: block_number,
                })?;
            }
            status::EXITING => {
                self.pending_set_change.write(true)?;
                let height = self.val_deactivated_at_height.read(&addr)?;

                crate::metrics::record_validator_force_exit(addr);
                crate::metrics::record_pending_set_change(true);

                journal_record(JournalRecord::ValidatorForcedExit {
                    wall_clock: iso8601_now(),
                    block_number,
                    validator: format!("{addr:?}"),
                    status_before: "EXITING".into(),
                    status_after: "EXITING".into(),
                });

                warn!(
                    target: "outbe::validatorset",
                    event = "validator_force_exit",
                    validator = %addr,
                    status_before = "EXITING",
                    status_after = "EXITING",
                    deactivated_at_height = height,
                    block_number,
                    "validator already EXITING; re-emitting force-exit signal",
                );

                self.emit(IValidatorSet::ValidatorForcedExit {
                    validator: addr,
                    atHeight: height,
                })?;
            }
            status::UNBONDING | status::INACTIVE => {
                // Already excluded from consensus.
                info!(
                    target: "outbe::validatorset",
                    event = "validator_force_exit_noop",
                    validator = %addr,
                    status = current_status,
                    block_number,
                    "force-exit no-op: validator already in UNBONDING or INACTIVE",
                );
            }
            _ => {
                return Err(PrecompileError::Revert(format!(
                    "cannot force-exit validator with status {current_status}: only ACTIVE, EXITING, UNBONDING, or INACTIVE allowed"
                )));
            }
        }

        let sc = self.val_slash_count.read(&addr)?;
        self.val_slash_count.write(&addr, sc + 1)?;

        Ok(())
    }

    /// Called by consensus after DKG reshare completes.
    ///
    /// Transitions:
    /// - REGISTERED/PENDING validators in `new_active_set` → ACTIVE + has_bls_share = true
    /// - EXITING validators NOT in `new_active_set` → UNBONDING + has_bls_share = false
    /// - Updates active_consensus_set_hash
    /// - Clears pending_set_change only if ALL active validators have shares
    ///
    /// NOTE: The initial clear-all-shares loop is O(n). Acceptable because DKG
    /// reshare events are rare (validator join/leave) and never occur more than
    /// once per epoch.
    pub fn activate_reshared_set(
        &mut self,
        new_active_set: &[Address],
        active_set_hash: B256,
    ) -> Result<()> {
        // First, clear has_bls_share for all validators
        let all = self.get_all_validators()?;
        for v in &all {
            if v.has_bls_share {
                self.val_has_bls_share.write(&v.validator_address, false)?;
            }
        }

        // Set has_bls_share and activate validators in the new set
        for &addr in new_active_set {
            let index = self.address_to_index.read(&addr)?;
            if index == 0 {
                return Err(PrecompileError::Revert(format!(
                    "reshared active set contains unregistered validator {addr}"
                )));
            }

            let st = self.val_status.read(&addr)?;
            match st {
                status::REGISTERED | status::PENDING => {
                    self.val_status.write(&addr, status::ACTIVE)?;
                    self.val_has_bls_share.write(&addr, true)?;
                }
                status::ACTIVE => {
                    self.val_has_bls_share.write(&addr, true)?;
                }
                _ => {
                    return Err(PrecompileError::Revert(format!(
                        "reshared active set contains validator {addr} with non-active status {st}"
                    )));
                }
            }
        }

        // Transition EXITING validators not in new set → UNBONDING
        let mut transitioned_to_unbonding: Vec<Address> = Vec::new();
        for v in &all {
            if v.status == status::EXITING {
                let in_new_set = new_active_set.contains(&v.validator_address);
                if !in_new_set {
                    self.val_status
                        .write(&v.validator_address, status::UNBONDING)?;
                    self.val_has_bls_share.write(&v.validator_address, false)?;
                    transitioned_to_unbonding.push(v.validator_address);
                }
            }
        }

        // Store deterministic active consensus set hash.
        self.active_consensus_set_hash.write(active_set_hash)?;

        // Only clear pending_set_change if ALL active validators now have shares.
        // If an ACTIVE validator missed the ceremony (not in new_active_set),
        // keep pending = true so a new reshare is triggered automatically.
        let active_validators = self.get_active_validators()?;
        let all_covered = active_validators
            .iter()
            .all(|v| new_active_set.contains(&v.validator_address));
        self.pending_set_change.write(!all_covered)?;

        let active_count: u32 = new_active_set
            .len()
            .try_into()
            .map_err(|_| PrecompileError::Revert("active set count exceeds u32".into()))?;

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
        // Aggregate counts after all transitions written.
        if let Ok(all_after) = self.get_all_validators() {
            let mut active = 0usize;
            let mut exiting = 0usize;
            let mut unbonding = 0usize;
            for v in &all_after {
                match v.status {
                    status::ACTIVE => active += 1,
                    status::EXITING => exiting += 1,
                    status::UNBONDING => unbonding += 1,
                    _ => {}
                }
            }
            crate::metrics::record_aggregate_status_counts(active, exiting, unbonding);
        }

        info!(
            target: "outbe::validatorset",
            event = "reshared_set_activated",
            active_count,
            transitioned_to_unbonding = transitioned_to_unbonding.len(),
            pending_set_change = !all_covered,
            block_number = self.storage.block_number().unwrap_or(0),
            active_set_hash = %active_set_hash,
            "DKG reshare activated; new active set committed",
        );
        for addr in &transitioned_to_unbonding {
            info!(
                target: "outbe::validatorset",
                event = "validator_unbonding",
                validator = %addr,
                block_number = self.storage.block_number().unwrap_or(0),
                "validator transitioned EXITING -> UNBONDING (excluded from new set)",
            );
        }

        self.emit(IValidatorSet::ConsensusSetUpdated {
            activeCount: active_count,
        })?;

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
        let all = self.get_all_validators()?;
        for v in all {
            // Only reset counters for validators that accumulate them.
            // A-44: Include EXITING — they still participate in consensus
            // until reshare completes and accumulate per-epoch counters.
            if v.status != status::ACTIVE && v.status != status::EXITING {
                continue;
            }
            let addr = v.validator_address;
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
            let st = self.val_status.read(&addr)?;
            if st != status::INACTIVE {
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
