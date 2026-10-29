use super::*;

/// Outbe runtime addresses that receive `0xEF` EIP-161 marker bytecode in every
/// block's pre-execution step ([`OutbeBlockExecutor::apply_pre_execution_changes`]).
/// The marker lets their persistent EVM storage survive state-root computation.
/// EIP-161 emptiness (nonce==0 && balance==0 && empty code) ignores storage. Without
/// the marker, a stateful account that holds only storage is pruned.
///
/// This list MUST contain every *stateful* runtime precompile from
/// [`crate::precompiles::outbe_precompile_addresses`] (except stateless verifiers
/// and genesis-seeded accounts). It MUST also contain the system-only storage
/// markers that have no dispatch registration. The
/// `marker_list_covers_stateful_precompiles` test pins the superset invariant.
pub mod marker_addresses {
    use alloy_primitives::Address;
    use outbe_primitives::addresses::*;

    pub const OUTBE_RUNTIME_MARKER_ADDRESSES: [Address; 34] = [
        GRATIS_ADDRESS,
        GRATIS_FACTORY_ADDRESS,
        CREDIS_ADDRESS,
        CREDIS_FACTORY_ADDRESS,
        PROMIS_ADDRESS,
        PROMIS_FACTORY_ADDRESS,
        TRIBUTE_ADDRESS,
        NOD_ADDRESS,
        NOD_FACTORY_ADDRESS,
        TRIBUTE_FACTORY_ADDRESS,
        GEM_ADDRESS,
        GEM_FACTORY_ADDRESS,
        INTEX_ADDRESS,
        INTEX_FACTORY_ADDRESS,
        DESIS_ADDRESS,
        AGENT_REWARD_ADDRESS,
        FIDELITY_ADDRESS,
        EMISSION_ADDRESS,
        METADOSIS_ADDRESS,
        PROMIS_LIMIT_ADDRESS,
        CYCLE_ADDRESS,
        CCA_REGISTRY_ADDRESS,
        VALIDATOR_SET_ADDRESS,
        SLASH_INDICATOR_ADDRESS,
        STAKING_ADDRESS,
        REWARDS_ADDRESS,
        ACCOUNTING_PROGRESS_ADDRESS,
        ORACLE_ADDRESS,
        OUTBE_SYSTEM_TX_ADDRESS,
        TEE_REGISTRY_ADDRESS,
        L2_REGISTRY_ADDRESS,
        HYPERLANE_CONTROLLER_ADDRESS,
        UPDATE_ADDRESS,
        VOTE_ADDRESS,
    ];
}

/// Applies a DKG/reshare `BoundaryOutcome` from `header.extra_data` against
/// on-chain validator-set state and writes the V2 committee snapshot activated
/// by the boundary.
///
/// The on-chain `active_consensus_set_hash` is derived from validator
/// addresses only, so a same-membership DKG/VRF rotation must not change it.
/// Same-membership rotations still change the committee snapshot because they
/// bind new VRF material, so matching active-set hash is not a no-op.
///
/// Tri-state behaviour:
/// - `current_hash == reshare.active_set_hash` -> write incoming snapshot and
///   re-activate the same active set atomically.
/// - mismatch + `is_validator_set_change == true` -> apply boundary activation
///   and write incoming snapshot atomically.
/// - mismatch + `is_validator_set_change == false` -> fatal.
pub(crate) fn apply_boundary_outcome(
    storage: StorageHandle,
    boundary: &outbe_primitives::consensus::DkgBoundaryArtifact,
    block_number: u64,
    timestamp: u64,
) -> outbe_primitives::error::Result<()> {
    use crate::errors::ExecutorHookError;

    let reshare = &boundary.reshare;

    let expected_tee_expiry_hash =
        outbe_primitives::reshare_artifact::tee_expired_target_exclusions_hash(
            &boundary.tee_expired_target_exclusions,
        )?;
    if boundary.tee_expired_target_exclusions_hash != expected_tee_expiry_hash {
        return Err(ExecutorHookError::TeeExpiryExclusionsHashMismatch {
            expected: expected_tee_expiry_hash,
            actual: boundary.tee_expired_target_exclusions_hash,
        }
        .into());
    }

    let expected_active_set_hash = hash_boundary_active_set(&reshare.new_active_set);
    if reshare.active_set_hash != expected_active_set_hash {
        return Err(ExecutorHookError::ActiveSetHashMismatch {
            expected: expected_active_set_hash,
            actual: reshare.active_set_hash,
        }
        .into());
    }

    let expected_vrf_group_public_key = keccak256(boundary.vrf_group_public_key_bytes.as_ref());
    if boundary.vrf_group_public_key != expected_vrf_group_public_key {
        return Err(ExecutorHookError::VrfGroupPublicKeyHashMismatch {
            expected: expected_vrf_group_public_key,
            actual: boundary.vrf_group_public_key,
        }
        .into());
    }

    let incoming_snapshot = committee_snapshot_from_boundary(storage.clone(), boundary)?;
    let expected_committee_set_hash =
        outbe_validatorset::committee_set_hash_v2(boundary.epoch, &incoming_snapshot);
    if boundary.committee_set_hash != expected_committee_set_hash {
        return Err(ExecutorHookError::CommitteeSetHashMismatch {
            expected: expected_committee_set_hash,
            actual: boundary.committee_set_hash,
        }
        .into());
    }

    let vs_check = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
    let current_hash = vs_check.active_consensus_set_hash()?;
    let current_epoch: u64 = vs_check
        .epoch_snapshot()?
        .number
        .try_into()
        .map_err(|_| ExecutorHookError::ValidatorSetEpochExceedsU64)?;

    if current_hash != reshare.active_set_hash && !boundary.is_validator_set_change {
        return Err(
            ExecutorHookError::ActiveSetHashChangedWithoutValidatorSetChange {
                current: current_hash,
                boundary: reshare.active_set_hash,
            }
            .into(),
        );
    }

    let advances_epoch =
        validate_boundary_epoch_transition(current_epoch, boundary.epoch, block_number)?;

    // The activated epoch, active membership/hash and its consensus
    // snapshot are one state transition. A nominal epoch height is not
    // activation authority. Only this certified BoundaryOutcome is.
    let activation_guard = storage.checkpoint_guard();
    if advances_epoch {
        outbe_validatorset::hooks::advance_epoch(storage.clone(), timestamp, block_number)?;
    }

    let inputs = outbe_validatorset::hooks::BoundaryActivationInputs {
        outgoing: None,
        incoming_epoch: boundary.epoch,
        incoming: incoming_snapshot,
        freeze_height: boundary.freeze_height,
        new_active_set: reshare.new_active_set.clone(),
        active_set_hash: reshare.active_set_hash,
        tee_expired_target_exclusions: boundary.tee_expired_target_exclusions.clone(),
    };
    outbe_validatorset::hooks::activate_boundary_atomic(storage.clone(), &inputs)?;
    if advances_epoch {
        outbe_validatorset::contract::ValidatorSet::new(storage).cleanup_inactive_validators(16)?;
    }
    activation_guard.commit();
    Ok(())
}

fn validate_boundary_epoch_transition(
    current_epoch: u64,
    incoming_epoch: u64,
    block_number: u64,
) -> outbe_primitives::error::Result<bool> {
    use crate::errors::ExecutorHookError;

    if block_number == 1 && current_epoch == 0 && incoming_epoch == 0 {
        return Ok(false);
    }
    let next_epoch = current_epoch
        .checked_add(1)
        .ok_or(ExecutorHookError::ValidatorSetEpochOverflow)?;
    if block_number > 1 && incoming_epoch == next_epoch {
        return Ok(true);
    }
    Err(ExecutorHookError::InvalidBoundaryEpochTransition {
        current: current_epoch,
        incoming: incoming_epoch,
        block: block_number,
    }
    .into())
}

/// Resets outgoing-epoch counters only for a block that actually carries the
/// next certified BoundaryOutcome. This runs before receipt-visible
/// LateFinalizeCredits. The BoundaryOutcome later advances epoch/set/snapshot
/// without erasing misses recorded by that earlier phase.
pub(crate) fn prepare_boundary_epoch_counters(
    storage: StorageHandle,
    boundary: &outbe_primitives::consensus::DkgBoundaryArtifact,
    block_number: u64,
) -> outbe_primitives::error::Result<()> {
    let validators = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
    let current_epoch = validators.current_epoch_u64()?;
    if !validate_boundary_epoch_transition(current_epoch, boundary.epoch, block_number)? {
        return Ok(());
    }
    let addresses: Vec<Address> = validators
        .get_all_validators()?
        .into_iter()
        .map(|validator| validator.validator_address)
        .collect();
    outbe_slashindicator::contract::SlashIndicator::new(storage.clone())
        .reset_epoch_counters(&addresses)?;
    outbe_validatorset::hooks::reset_epoch_counters(storage)
}

pub(in crate::executor) fn hash_boundary_active_set(addresses: &[Address]) -> B256 {
    let mut bytes = Vec::with_capacity(8 + addresses.len() * 20);
    bytes.extend_from_slice(&(addresses.len() as u64).to_be_bytes());
    for address in addresses {
        bytes.extend_from_slice(address.as_slice());
    }
    keccak256(bytes)
}

fn committee_snapshot_from_boundary(
    storage: StorageHandle,
    boundary: &outbe_primitives::consensus::DkgBoundaryArtifact,
) -> outbe_primitives::error::Result<outbe_validatorset::CommitteeSnapshot> {
    use crate::errors::ExecutorHookError;

    let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
    let mut committee = Vec::with_capacity(boundary.reshare.new_active_set.len());
    for address in &boundary.reshare.new_active_set {
        let Some(consensus_pubkey) = vs.consensus_pubkey_of(*address)? else {
            return Err(
                ExecutorHookError::UnregisteredBoundaryValidator { address: *address }.into(),
            );
        };
        committee.push(outbe_validatorset::CommitteeEntry {
            address: *address,
            consensus_pubkey,
        });
    }

    Ok(outbe_validatorset::CommitteeSnapshot {
        committee,
        vrf_material_version: boundary.vrf_material_version,
        vrf_group_public_key_bytes: boundary.vrf_group_public_key_bytes.to_vec(),
        // Derived from the already-consensus-validated boundary `outcome` (the
        // full DKG output), so a proposer cannot forge it. It lets SlashIndicator
        // verify an invalid-seed-partial slash. It is ZERO when no full
        // polynomial is carried (group-key-only bootstrap).
        vrf_public_polynomial_hash: outbe_consensus::dkg_manager::boundary_outcome_polynomial_hash(
            boundary.outcome.as_ref(),
        ),
    })
}

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv> + ZeroFeeCfgAccess,
    E::Error: std::fmt::Display,
{
    pub(in crate::executor) fn begin_zone_proposer(
        &self,
        block_number: u64,
    ) -> Result<Option<Address>, BlockExecutionError> {
        if block_number == 0 {
            return Ok(None);
        }
        self.proposer_evm_address
            .or_else(|| {
                self.expected_begin_system_txs
                    .first()
                    .map(|tx| Address::from(*tx.signer()))
            })
            .or_else(|| {
                (!self.is_verifier_witness_mode())
                    .then(|| self.evm_signer.as_ref().map(|signer| signer.address()))
                    .flatten()
            })
            .ok_or_else(|| {
                crate::errors::ExecutorValidationError::MissingBeginZoneProposerAddress.into()
            })
            .map(Some)
    }

    pub(in crate::executor) fn validate_proposer_identity(
        &mut self,
        proposer: Address,
        allow_boundary_proposer: bool,
    ) -> Result<(), BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let timestamp = outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
            .map_err(ExecutorValidationError::from)?;
        let chain_id = self.inner.evm.chain_id();
        let db = self.inner.evm.db_mut();
        let ctx = BlockContext::new_with_genesis_hash(outbe_primitives::block::BlockContextInput {
            block_number,
            timestamp,
            chain_id,
            genesis_hash: self.genesis_hash,
            genesis_timestamp: self.genesis_timestamp,
            proposer,
            validators: Vec::new(),
        });
        let mut provider = DirectStorageProvider::new(db, ctx);
        let storage = StorageHandle::new(&mut provider);
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        if vs.is_consensus_participant(proposer).map_err(|error| {
            if error.is_node_local() || matches!(error, PrecompileError::Fatal(_)) {
                BlockExecutionError::other(error)
            } else {
                ExecutorValidationError::validate_proposer_identity(error).into()
            }
        })? {
            return Ok(());
        }
        if allow_boundary_proposer
            && vs.is_validator(proposer).map_err(|error| {
                if error.is_node_local() || matches!(error, PrecompileError::Fatal(_)) {
                    BlockExecutionError::other(error)
                } else {
                    ExecutorValidationError::validate_proposer_identity(error).into()
                }
            })?
        {
            return Ok(());
        }
        Err(ExecutorValidationError::ProposerNotActiveConsensusParticipant { proposer }.into())
    }

    pub(in crate::executor) fn boundary_allows_proposer(
        &self,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
        proposer: Address,
    ) -> bool {
        matches!(
            &block_artifacts.consensus_header_artifact,
            Some(ConsensusHeaderArtifact::BoundaryOutcome(artifact))
                if artifact.is_validator_set_change && artifact.reshare.new_active_set.contains(&proposer)
        )
    }
}
