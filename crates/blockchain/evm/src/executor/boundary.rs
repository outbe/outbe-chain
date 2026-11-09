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

    pub const OUTBE_RUNTIME_MARKER_ADDRESSES: [Address; 35] = [
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
        CROSSCHAIN_BRIDGE_ADDRESS,
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
#[cfg(any(test, feature = "test-hooks", feature = "test-utils"))]
pub fn apply_boundary_outcome(
    host: &dyn crate::EvmRuntimeHost,
    storage: StorageHandle,
    boundary: &outbe_primitives::consensus::DkgBoundaryArtifact,
    block_number: u64,
    timestamp: u64,
) -> outbe_primitives::error::Result<()> {
    host.apply_boundary_outcome(storage, boundary, block_number, timestamp)
}

/// Resets outgoing-epoch counters only for a block that actually carries the
/// next certified BoundaryOutcome. This runs before receipt-visible
/// LateFinalizeCredits. The BoundaryOutcome later advances epoch/set/snapshot
/// without erasing misses recorded by that earlier phase.
pub fn prepare_boundary_epoch_counters(
    host: &dyn crate::EvmRuntimeHost,
    storage: StorageHandle,
    boundary: &outbe_primitives::consensus::DkgBoundaryArtifact,
    block_number: u64,
) -> outbe_primitives::error::Result<()> {
    host.prepare_boundary(storage, boundary, block_number)
}

#[cfg(any(test, feature = "test-hooks"))]
pub fn hash_boundary_active_set(addresses: &[Address]) -> B256 {
    let mut bytes = Vec::with_capacity(8 + addresses.len() * 20);
    bytes.extend_from_slice(&(addresses.len() as u64).to_be_bytes());
    for address in addresses {
        bytes.extend_from_slice(address.as_slice());
    }
    keccak256(bytes)
}

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv> + ZeroFeeCfgAccess,
    E::Error: std::fmt::Display,
{
    pub fn begin_zone_proposer(
        &self,
        block_number: u64,
    ) -> Result<Option<Address>, BlockExecutionError> {
        if !BlockHeightRegime::for_block(block_number).has_begin_zone_system_txs() {
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

    pub fn validate_proposer_identity(
        &mut self,
        proposer: Address,
        allow_boundary_proposer: bool,
    ) -> Result<(), BlockExecutionError> {
        use crate::errors::ExecutorValidationError;

        let block_number = self.inner.evm.block().number().saturating_to::<u64>();
        let timestamp = outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
            .map_err(ExecutorValidationError::from)?;
        let ctx = self.block_context(block_number, timestamp, proposer);
        let host = std::sync::Arc::clone(&self.runtime_host);
        let db = self.inner.evm.db_mut();
        let mut provider = DirectStorageProvider::new(db, ctx);
        let storage = StorageHandle::new(&mut provider);
        let host = host.as_ref();
        let map_identity = |error: PrecompileError| {
            if error.is_node_local() || matches!(error, PrecompileError::Fatal(_)) {
                BlockExecutionError::other(error)
            } else {
                ExecutorValidationError::validate_proposer_identity(error).into()
            }
        };
        if host
            .is_consensus_participant(storage.clone(), proposer)
            .map_err(map_identity)?
        {
            return Ok(());
        }
        if allow_boundary_proposer && host.is_validator(storage, proposer).map_err(map_identity)? {
            return Ok(());
        }
        Err(ExecutorValidationError::ProposerNotActiveConsensusParticipant { proposer }.into())
    }

    pub fn boundary_allows_proposer(
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
