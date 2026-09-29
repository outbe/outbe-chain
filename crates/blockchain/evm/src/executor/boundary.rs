use super::*;

/// Outbe runtime addresses that receive `0xEF` EIP-161 marker bytecode in every
/// block's pre-execution step ([`OutbeBlockExecutor::apply_pre_execution_changes`])
/// so their persistent EVM storage survives state-root computation - EIP-161
/// emptiness (nonce==0 && balance==0 && empty code) ignores storage, so without
/// the marker a stateful account holding only storage is pruned.
///
/// This MUST contain every *stateful* runtime precompile from
/// [`crate::precompiles::outbe_precompile_addresses`] (except stateless verifiers
/// and genesis-seeded accounts), plus the system-only storage markers that have
/// no dispatch registration. The superset invariant is pinned by the
/// `marker_list_covers_stateful_precompiles` test.
pub mod marker_addresses {
    use alloy_primitives::Address;
    use outbe_primitives::addresses::*;

    pub const OUTBE_RUNTIME_MARKER_ADDRESSES: [Address; 40] = [
        GRATIS_ADDRESS,
        GRATIS_FACTORY_ADDRESS,
        CREDIS_ADDRESS,
        CREDIS_FACTORY_ADDRESS,
        PROMIS_ADDRESS,
        // PromisFactory is a live stateful precompile (in
        // `outbe_precompile_addresses`) and is NOT genesis-seeded, so this
        // per-block runtime marker is its only EIP-161 preservation path -
        // mirroring GRATIS_FACTORY / GEM_FACTORY above.
        PROMIS_FACTORY_ADDRESS,
        TRIBUTE_ADDRESS,
        NOD_ADDRESS,
        NOD_FACTORY_ADDRESS,
        TRIBUTE_FACTORY_ADDRESS,
        // reth22-1 fix: GEM and GEM_FACTORY are live stateful precompiles
        // (in `outbe_precompile_addresses`) that were absent from this list, so
        // their storage was silently pruned at state-root time under EIP-161.
        // They are NOT seeded with genesis bytecode either, so this per-block
        // runtime marker is their only preservation path.
        GEM_ADDRESS,
        GEM_FACTORY_ADDRESS,
        INTEX_ADDRESS,
        INTEX_FACTORY_ADDRESS,
        DESIS_ADDRESS,
        AGENT_REWARD_ADDRESS,
        FIDELITY_ADDRESS,
        EMISSION_LIMIT_ADDRESS,
        METADOSIS_ADDRESS,
        PROMIS_LIMIT_ADDRESS,
        CYCLE_ADDRESS,
        CCA_REGISTRY_ADDRESS,
        GEM_ADDRESS,
        GEM_FACTORY_ADDRESS,
        VALIDATOR_SET_ADDRESS,
        SLASH_INDICATOR_ADDRESS,
        STAKING_ADDRESS,
        REWARDS_ADDRESS,
        // V2 Phase 1 accounting-progress marker. System-only (no precompile
        // dispatch); the `[0xef]` marker preserves slot 0 across EIP-161 cleanup.
        ACCOUNTING_PROGRESS_ADDRESS,
        ORACLE_ADDRESS,
        OUTBE_SYSTEM_TX_ADDRESS,
        // TEE Registry (storage-backed, system-written at Phase 3b). Not
        // genesis-seeded, so the runtime 0xEF marker is its only EIP-161
        // preservation path (reth22-1 class).
        TEE_REGISTRY_ADDRESS,
        // L2 network registry (storage-backed, permissionless writes). Not
        // genesis-seeded, so the runtime 0xEF marker is its only EIP-161
        // preservation path (reth22-1 class).
        L2_REGISTRY_ADDRESS,
        // Hyperlane controller (router + domain->ISM table). Not genesis-seeded,
        // so the runtime 0xEF marker is its only EIP-161 preservation path.
        HYPERLANE_CONTROLLER_ADDRESS,
        // OCOMP authority and lineage registry. Fresh genesis initializes it
        // at height 32; the marker preserves successor/refcount state.
        OCOMP_REGISTRY_ADDRESS,
        UPDATE_ADDRESS,
        VOTE_ADDRESS,
        // System-only compressed-entity commitment state (no public dispatch).
        COMPRESSED_ENTITIES_ADDRESS,
        // PayNote pool. All data live in its storage,
        // and it is not genesis-seeded, so this marker
        // is its only EIP-161 preservation path (reth22-1 class).
        PAYNOTE_ADDRESS,
        // Emit private-note pool (storage-backed, lazily initialized by the
        // first burn). Genesis-reserved but not genesis-seeded with storage,
        // so the runtime 0xEF marker preserves its tree/nullifier state.
        EMIT_ADDRESS,
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
    let reshare = &boundary.reshare;

    let expected_tee_expiry_hash =
        outbe_primitives::reshare_artifact::tee_expired_target_exclusions_hash(
            &boundary.tee_expired_target_exclusions,
        )?;
    if boundary.tee_expired_target_exclusions_hash != expected_tee_expiry_hash {
        return Err(PrecompileError::Fatal(format!(
            "boundary TEE expiry exclusions commitment mismatch: expected {expected_tee_expiry_hash}, got {}",
            boundary.tee_expired_target_exclusions_hash
        )));
    }

    let expected_active_set_hash = hash_boundary_active_set(&reshare.new_active_set);
    if reshare.active_set_hash != expected_active_set_hash {
        return Err(PrecompileError::Fatal(format!(
            "boundary active_set_hash mismatch: expected {expected_active_set_hash}, got {}",
            reshare.active_set_hash
        )));
    }

    let expected_vrf_group_public_key = keccak256(boundary.vrf_group_public_key_bytes.as_ref());
    if boundary.vrf_group_public_key != expected_vrf_group_public_key {
        return Err(PrecompileError::Fatal(format!(
            "boundary VRF group public key hash mismatch: expected {expected_vrf_group_public_key}, got {}",
            boundary.vrf_group_public_key
        )));
    }

    let incoming_snapshot = committee_snapshot_from_boundary(storage.clone(), boundary)?;
    let expected_committee_set_hash =
        outbe_validatorset::committee_set_hash_v2(boundary.epoch, &incoming_snapshot);
    if boundary.committee_set_hash != expected_committee_set_hash {
        return Err(PrecompileError::Fatal(format!(
            "boundary committee_set_hash mismatch: expected {expected_committee_set_hash}, got {}",
            boundary.committee_set_hash
        )));
    }

    let vs_check = outbe_validatorset::contract::ValidatorSet::new(storage.clone());
    let current_hash = vs_check.active_consensus_set_hash()?;
    let current_epoch: u64 = vs_check
        .epoch_snapshot()?
        .number
        .try_into()
        .map_err(|_| PrecompileError::Fatal("ValidatorSet epoch exceeds u64".into()))?;

    if current_hash != reshare.active_set_hash && !boundary.is_validator_set_change {
        return Err(PrecompileError::Fatal(format!(
            "boundary active_set_hash changed without validator-set change: current={current_hash}, boundary={}",
            reshare.active_set_hash
        )));
    }

    let advances_epoch =
        validate_boundary_epoch_transition(current_epoch, boundary.epoch, block_number)?;

    // The activated epoch, active membership/hash and its consensus+OCOMP
    // snapshot are one state transition. A nominal epoch height is not
    // activation authority; only this certified BoundaryOutcome is.
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
    if block_number == 1 && current_epoch == 0 && incoming_epoch == 0 {
        return Ok(false);
    }
    let next_epoch = current_epoch.checked_add(1).ok_or_else(|| {
        PrecompileError::Fatal("ValidatorSet epoch overflow at BoundaryOutcome".into())
    })?;
    if block_number > 1 && incoming_epoch == next_epoch {
        return Ok(true);
    }
    Err(PrecompileError::Fatal(format!(
        "BoundaryOutcome epoch must bootstrap epoch 0 at block 1 or activate current+1: current={current_epoch}, incoming={incoming_epoch}, block={block_number}"
    )))
}

/// Resets outgoing-epoch counters only for a block that actually carries the
/// next certified BoundaryOutcome. This runs before receipt-visible
/// LateFinalizeCredits; the BoundaryOutcome later advances epoch/set/snapshot
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
    let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
    let mut committee = Vec::with_capacity(boundary.reshare.new_active_set.len());
    for address in &boundary.reshare.new_active_set {
        let Some(consensus_pubkey) = vs.consensus_pubkey_of(*address)? else {
            return Err(PrecompileError::Fatal(format!(
                "boundary active set contains unregistered validator {address}"
            )));
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
        // full DKG output), so a proposer cannot forge it. Lets SlashIndicator
        // verify an invalid-seed-partial slash; ZERO when no full polynomial is
        // carried (group-key-only bootstrap).
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
            .or_else(|| self.evm_signer.as_ref().map(|signer| signer.address()))
            .or_else(|| {
                self.expected_begin_system_txs
                    .first()
                    .map(|tx| Address::from(*tx.signer()))
            })
            .ok_or_else(|| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    "missing proposer EVM address for begin-zone system txs".into(),
                ))
            })
            .map(Some)
    }

    pub(in crate::executor) fn validate_proposer_identity(
        &mut self,
        proposer: Address,
        allow_boundary_proposer: bool,
    ) -> Result<(), BlockExecutionError> {
        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let timestamp = self.inner.evm.block().timestamp().saturating_to::<u64>();
        let chain_id = self.inner.evm.chain_id();
        let db = self.inner.evm.db_mut();
        let ctx = BlockContext::new_with_genesis_hash(
            block_number,
            timestamp,
            chain_id,
            self.genesis_hash,
            proposer,
            Vec::new(),
        );
        let mut provider = DirectStorageProvider::new(db, ctx);
        let storage = StorageHandle::new(&mut provider);
        let vs = outbe_validatorset::contract::ValidatorSet::new(storage);
        if vs.is_consensus_participant(proposer).map_err(|error| {
            BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                format!("validate proposer identity: {error}").into(),
            ))
        })? {
            return Ok(());
        }
        if allow_boundary_proposer
            && vs.is_validator(proposer).map_err(|error| {
                BlockExecutionError::Internal(InternalBlockExecutionError::Other(
                    format!("validate boundary proposer identity: {error}").into(),
                ))
            })?
        {
            return Ok(());
        }
        Err(BlockExecutionError::Internal(
            InternalBlockExecutionError::Other(
                format!("proposer EVM address is not an active consensus participant: {proposer}")
                    .into(),
            ),
        ))
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
