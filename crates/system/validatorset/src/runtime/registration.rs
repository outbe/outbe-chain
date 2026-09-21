use super::{status, MAX_SELF_REGISTERED_UNSTAKED};
use crate::precompile::IValidatorSet;
use crate::schema::ValidatorSet;
use crate::state_machine::{self, P2pInfo, ValidatorLifecycle};
use alloy_primitives::{Address, B256};
use outbe_primitives::consensus_p2p::{
    decode_versioned, MAX_P2P_ADDRESS_ENCODED_LEN, P2P_ADDRESS_VERSION_V1,
};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::slashing_journal::{iso8601_now, record as journal_record, JournalRecord};
use outbe_primitives::validators::{validator_registration_message, VALIDATOR_REGISTRATION_DST};
use std::num::NonZeroU64;
use tracing::info;

/// Verifies a BLS MinPk registration signature.
///
/// Uses the `blst` crate directly to verify the signature without needing
/// the full commonware cryptography stack in the EVM precompile crate.
///
/// The signed message is `chain_id (u64 big-endian) || validator address`.
fn verify_bls_registration_sig(
    pubkey_bytes: &[u8; 48],
    sig_bytes: &[u8; 96],
    chain_id: u64,
    validator_addr: Address,
    radicle_node_id: B256,
) -> Result<()> {
    use blst::min_pk::{PublicKey, Signature};
    use blst::BLST_ERROR;

    let pk = PublicKey::from_bytes(pubkey_bytes)
        .map_err(|_| PrecompileError::Revert("invalid BLS public key".into()))?;
    let sig = Signature::from_bytes(sig_bytes)
        .map_err(|_| PrecompileError::Revert("invalid BLS signature".into()))?;

    let message = validator_registration_message(chain_id, validator_addr, radicle_node_id);
    let result = sig.verify(true, &message, VALIDATOR_REGISTRATION_DST, &[], &pk, true);
    if result != BLST_ERROR::BLST_SUCCESS {
        return Err(PrecompileError::Revert(
            "invalid BLS registration signature".into(),
        ));
    }
    Ok(())
}

impl ValidatorSet<'_> {
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
        let lifecycle = state_machine::with_p2p(before.lifecycle().clone(), P2pInfo::V1(decoded))?;
        let after = before.clone().with_lifecycle(lifecycle)?;
        let guard = self.storage.checkpoint_guard();
        self.persist_validator_state_delta(&before, &after)?;
        guard.commit();
        Ok(())
    }

    /// Returns the stored versioned P2P address payload, if one is registered.
    pub fn get_p2p_address(&self, validator_addr: Address) -> Result<Option<(u8, Vec<u8>)>> {
        let state = self.validator_state(validator_addr)?;
        let Some(p2p) = state.p2p() else {
            return Err(PrecompileError::Revert("validator not registered".into()));
        };
        if matches!(p2p, P2pInfo::Unset) {
            return Ok(None);
        }
        Ok(Some(p2p.encode_stored()))
    }

    /// Registers a new validator with BLS proof-of-possession verification.
    ///
    /// When `bls_signature` is `Some`, verifies that the BLS MinPk key was used to
    /// sign the chain-bound registration message under the "outbe_REGISTER"
    /// namespace.
    /// `None` is rejected by this production API. Genesis is storage-seeded;
    /// feature-gated tests use [`Self::register_validator`] explicitly.
    ///
    /// `consensus_pubkey` is a 48-byte BLS12-381 MinPk public key.
    /// `bls_signature` is an optional 96-byte BLS MinPk signature.
    pub fn register_validator_with_sig(
        &mut self,
        caller: Address,
        validator_addr: Address,
        consensus_pubkey: &[u8; 48],
        radicle_node_id: B256,
        bls_signature: Option<&[u8; 96]>,
    ) -> Result<()> {
        self.register_validator_inner(
            caller,
            validator_addr,
            consensus_pubkey,
            radicle_node_id,
            bls_signature,
            false,
        )
    }

    pub(super) fn register_validator_inner(
        &mut self,
        caller: Address,
        validator_addr: Address,
        consensus_pubkey: &[u8; 48],
        radicle_node_id: B256,
        bls_signature: Option<&[u8; 96]>,
        allow_bootstrap_without_pop: bool,
    ) -> Result<()> {
        let owner = self.config_owner.read()?;
        if *consensus_pubkey == [0; 48] {
            return Err(PrecompileError::Revert(
                "consensus public key must not be zero".into(),
            ));
        }

        // Authorization: owner or self-registration
        if caller != owner && caller != validator_addr {
            return Err(PrecompileError::Revert(
                "unauthorized: caller must be owner or validator itself".into(),
            ));
        }
        self.ensure_not_operational_delegate(validator_addr)?;
        if radicle_node_id.is_zero() {
            return Err(PrecompileError::Revert(
                "Radicle NodeId must not be zero".into(),
            ));
        }

        let radicle_owner = self.radicle_node_id_to_validator.read(&radicle_node_id)?;
        if !radicle_owner.is_zero() && radicle_owner != validator_addr {
            return Err(PrecompileError::Revert(
                "Radicle NodeId already registered by another validator".into(),
            ));
        }

        // Every runtime registration requires proof of possession. The only
        // no-PoP path is the feature-gated bootstrap/test helper above; normal
        // owner authority does not weaken the consensus-key invariant.
        if let Some(sig_bytes) = bls_signature {
            verify_bls_registration_sig(
                consensus_pubkey,
                sig_bytes,
                self.storage.chain_id()?,
                validator_addr,
                radicle_node_id,
            )?;
        } else if !allow_bootstrap_without_pop {
            return Err(PrecompileError::Revert(
                "validator registration requires BLS proof-of-possession signature".into(),
            ));
        }

        // bound the free, permissionless self-registration Sybil surface.
        // A self-registered REGISTERED node is admitted to the consensus P2P
        // secondary tier (the TEE verifier flow), so cap how many unstaked
        // REGISTERED validators can exist at once - far below
        // `config_max_validators` - so an attacker cannot fill the validator set
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
        let block_number = self.storage.block_number()?;
        if let Some(existing_index) = existing_state.registry_index() {
            let inactive = match existing_state.lifecycle().clone() {
                ValidatorLifecycle::Inactive(inactive) => inactive,
                _ => {
                    return Err(PrecompileError::Revert(
                        "validator already registered".into(),
                    ));
                }
            };
            let stored_node_id = self.val_radicle_node_id.read(&validator_addr)?;
            if stored_node_id != radicle_node_id {
                return Err(PrecompileError::Revert(
                    "inactive validator must keep its Radicle NodeId until final cleanup".into(),
                ));
            }
            if radicle_owner != validator_addr {
                return Err(PrecompileError::Fatal(
                    "registered validator has inconsistent Radicle NodeId reverse binding".into(),
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
                let ready_at = deactivated_at
                    .map(|height| {
                        height.checked_add(u64::from(cooldown)).ok_or_else(|| {
                            PrecompileError::Fatal(
                                "re-registration cooldown height overflow".into(),
                            )
                        })
                    })
                    .transpose()?;
                if ready_at.is_some_and(|height| block_number < height) {
                    return Err(PrecompileError::Revert(
                        "re-registration cooldown not expired".into(),
                    ));
                }
            }

            let old_pubkey = existing_state.consensus_pubkey().ok_or_else(|| {
                PrecompileError::Fatal("registered validator is missing consensus pubkey".into())
            })?;
            let old_pk_hash = Self::consensus_pubkey_hash(old_pubkey);
            let pk_hash = Self::consensus_pubkey_hash(consensus_pubkey);
            let lifecycle = ValidatorLifecycle::WaitingForStake(state_machine::reregister(
                inactive,
                *consensus_pubkey,
                block_number,
            )?);
            let after = existing_state.clone().with_lifecycle(lifecycle)?;
            if self.val_ocomp_recovery_deadline.read(&validator_addr)? != 0 {
                return Err(PrecompileError::Revert(
                    "cannot re-register while an OCOMP recovery window is open".into(),
                ));
            }

            let guard = self.storage.checkpoint_guard();
            self.consensus_pubkey_hash_to_address
                .write(&old_pk_hash, Address::ZERO)?;
            self.consensus_pubkey_hash_to_address
                .write(&pk_hash, validator_addr)?;
            self.persist_registry_state_delta(&existing_state, &after)?;
            self.pending_set_change.write(true)?;
            self.emit(IValidatorSet::ValidatorRegistered {
                validator: validator_addr,
                index: existing_index.get(),
            })?;
            guard.commit();

            crate::metrics::record_validator_status(validator_addr, status::REGISTERED);
            crate::metrics::record_validator_register(validator_addr, true);
            crate::metrics::record_pending_set_change(true);
            journal_record(JournalRecord::ValidatorReregistered {
                wall_clock: iso8601_now(),
                block_number,
                validator: format!("{validator_addr:?}"),
                index: existing_index.get(),
            });

            info!(
                target: "outbe::validatorset",
                event = "validator_reregistered",
                validator = %validator_addr,
                index = existing_index.get(),
                block_number,
                "validator re-registered (was INACTIVE, lifecycle metadata reset)",
            );

            return Ok(());
        }

        // Check capacity
        let count = self.validator_count.read()?;
        let max = self.config_max_validators.read()?;
        if max > 0 && count >= max {
            return Err(PrecompileError::Revert("max validators reached".into()));
        }

        // Assign 1-based index
        let new_index = count
            .checked_add(1)
            .ok_or_else(|| PrecompileError::Fatal("validator count overflow".into()))?;
        let new_index_u64 = new_index as u64;

        // Construct and persist the complete first-time registry bundle. The
        // typed decoder guarantees that absent addresses carry no stake residue.
        let registered_state = state_machine::register(
            existing_state.clone(),
            NonZeroU64::new(new_index_u64).ok_or_else(|| {
                PrecompileError::Fatal("validator registry index must be non-zero".into())
            })?,
            *consensus_pubkey,
            block_number,
        )?;

        let guard = self.storage.checkpoint_guard();
        if radicle_owner == validator_addr {
            return Err(PrecompileError::Fatal(
                "unregistered validator retains a Radicle NodeId reservation".into(),
            ));
        }
        self.address_to_index
            .write(&validator_addr, new_index_u64)?;
        self.index_to_address
            .write(&new_index_u64, validator_addr)?;
        self.persist_registry_state_delta(&existing_state, &registered_state)?;

        // Pubkey reverse lookup (keyed by keccak256 of full 48-byte pubkey)
        let pk_hash = Self::consensus_pubkey_hash(consensus_pubkey);
        self.consensus_pubkey_hash_to_address
            .write(&pk_hash, validator_addr)?;
        self.val_radicle_node_id
            .write(&validator_addr, radicle_node_id)?;
        self.radicle_node_id_to_validator
            .write(&radicle_node_id, validator_addr)?;

        // Increment count
        self.validator_count.write(new_index)?;

        // Signal pending set change so consensus detects the new validator
        self.pending_set_change.write(true)?;
        self.emit(IValidatorSet::ValidatorRegistered {
            validator: validator_addr,
            index: new_index as u64,
        })?;
        guard.commit();

        crate::metrics::record_validator_status(validator_addr, status::REGISTERED);
        crate::metrics::record_validator_register(validator_addr, false);
        crate::metrics::record_pending_set_change(true);

        journal_record(JournalRecord::ValidatorRegistered {
            wall_clock: iso8601_now(),
            block_number,
            validator: format!("{validator_addr:?}"),
            index: new_index as u64,
        });

        info!(
            target: "outbe::validatorset",
            event = "validator_registered",
            validator = %validator_addr,
            index = new_index as u64,
            block_number,
            "validator registered (first-time)",
        );

        Ok(())
    }
}
