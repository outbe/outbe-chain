//! Block-open witness mode, leader attestation key, and the initial tape.
use super::super::*;
use super::{witness_fault_to_block_error, witness_lock_poisoned};
use outbe_primitives::error::PrecompileError;
use outbe_primitives::tee_bootstrap_v2::TeeBootstrapV2;
use outbe_tee::witness::{
    decode_witness_payload_v1, encode_witness_payload_v1, witness_policy, BlockWitnessFault,
    EnclaveWitnessTapeV1, WitnessExecutionMode, WitnessPolicy, WitnessState,
};
use std::sync::{Arc, Mutex};

impl<'a, Evm> OutbeBlockExecutor<'a, Evm> {
    pub(crate) fn final_enclave_witness_bytes(
        &mut self,
    ) -> Result<Option<Bytes>, BlockExecutionError> {
        if let Some(cached) = self.frozen_enclave_witness.as_ref() {
            return Ok(Some(cached.clone()));
        }
        let Some(state) = self.witness_state.as_ref() else {
            return Ok(None);
        };
        let mut guard = state.lock().map_err(|_| witness_lock_poisoned())?;
        if let Some(fault) = guard.first_witness_fault {
            return Err(witness_fault_to_block_error(
                fault,
                "witness fault before final header artifact encoding",
            ));
        }
        match guard.mode {
            WitnessExecutionMode::ProposerRecord => {
                if guard.tape.prefix.is_empty() && guard.tape.txs.is_empty() {
                    Ok(None)
                } else {
                    let encoded = encode_witness_payload_v1(&guard.tape).map_err(|err| {
                        guard.latch(BlockWitnessFault::TapeInvalid);
                        witness_fault_to_block_error(
                            BlockWitnessFault::TapeInvalid,
                            format!("encode enclave witness payload: {err}"),
                        )
                    })?;
                    self.frozen_enclave_witness = Some(encoded.clone());
                    Ok(Some(encoded))
                }
            }
            WitnessExecutionMode::VerifierWithEnclave | WitnessExecutionMode::VerifierReplay => {
                let artifacts = decode_outbe_block_artifacts(self.final_extra_data.as_ref())
                    .map_err(crate::errors::ExecutorValidationError::block_artifacts)?;
                Ok(artifacts.enclave_witness)
            }
            WitnessExecutionMode::RpcDirect => Ok(None),
        }
    }
}

#[allow(private_bounds)]
impl<DB, E> OutbeBlockExecutor<'_, E>
where
    DB: StateDB,
    DB::Error: std::fmt::Display,
    E: Evm<DB = DB, Tx = TxEnv> + ZeroFeeCfgAccess,
    E::Error: std::fmt::Display,
{
    pub(in crate::executor) fn resolve_leader_attestation_pub(
        &mut self,
        block_number: u64,
        proposer: Address,
    ) -> Result<Option<[u8; 32]>, BlockExecutionError> {
        if let Some(pub_bytes) = self
            .block_one_bootstrap_attestation(block_number, proposer)
            .filter(|key| *key != [0u8; 32])
        {
            return Ok(Some(pub_bytes));
        }
        if let Some(pub_bytes) = self
            .registry_attestation_pub(block_number, proposer)?
            .filter(|key| *key != [0u8; 32])
        {
            return Ok(Some(pub_bytes));
        }
        if let Some(pub_bytes) = self
            .canary_attestation_fallback()
            .filter(|key| *key != [0u8; 32])
        {
            return Ok(Some(pub_bytes));
        }
        Ok(None)
    }

    pub(in crate::executor) fn init_block_witness_state(
        &mut self,
        block_number: u64,
        proposer: Address,
        block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    ) -> Result<(), BlockExecutionError> {
        let mode = witness_mode_for_block(self.is_verifier_witness_mode());
        let leader_attestation_pub = self.resolve_leader_attestation_pub(block_number, proposer)?;
        let tape = tape_for_witness_mode(mode, block_artifacts, leader_attestation_pub)?;
        self.witness_state = Some(Arc::new(Mutex::new(WitnessState::new(
            mode,
            leader_attestation_pub,
            tape,
        ))));
        self.inner.evm.set_witness_state(self.witness_state.clone());
        self.body_tx_index = 0;
        self.frozen_enclave_witness = None;
        Ok(())
    }

    fn registry_attestation_pub(
        &mut self,
        block_number: u64,
        proposer: Address,
    ) -> Result<Option<[u8; 32]>, BlockExecutionError> {
        let timestamp = outbe_primitives::time::timestamp_secs(self.inner.evm.block().timestamp())
            .map_err(crate::errors::ExecutorValidationError::from)?;
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
        let registry = outbe_teeregistry::TeeRegistry::new(storage);
        Ok(registry
            .validator_enclave_binding_v1(proposer)
            .map_err(binding_lookup_error)?
            .map(|binding| binding.attestation_ed25519.0))
    }

    fn block_one_bootstrap_attestation(
        &self,
        block_number: u64,
        proposer: Address,
    ) -> Option<[u8; 32]> {
        if block_number != 1 {
            return None;
        }
        if let Some(pub_bytes) = self.pending_bootstrap_attestation(proposer) {
            return Some(pub_bytes);
        }
        self.begin_tx_bootstrap_attestation(proposer)
    }

    fn pending_bootstrap_attestation(&self, proposer: Address) -> Option<[u8; 32]> {
        self.pending_tee_bootstrap
            .as_ref()
            .and_then(|payload| attestation_for_proposer(payload, proposer))
    }

    fn begin_tx_bootstrap_attestation(&self, proposer: Address) -> Option<[u8; 32]> {
        for tx in &self.expected_begin_system_txs {
            let Ok(SystemTxInputV2::TeeBootstrap { payload }) =
                SystemTxInputV2::decode(tx.input().as_ref())
            else {
                continue;
            };
            if let Some(pub_bytes) = attestation_for_proposer(&payload, proposer) {
                return Some(pub_bytes);
            }
        }
        None
    }

    fn canary_attestation_fallback(&self) -> Option<[u8; 32]> {
        #[cfg(test)]
        {
            if witness_policy().is_some() || self.is_verifier_witness_mode() {
                return None;
            }
            outbe_tee::client_global::canary_attestation_pub()
        }
        #[cfg(not(test))]
        {
            let _ = self;
            None
        }
    }
}

fn binding_lookup_error(error: PrecompileError) -> BlockExecutionError {
    if error.is_node_local() || matches!(error, PrecompileError::Fatal(_)) {
        BlockExecutionError::other(error)
    } else {
        crate::errors::ExecutorValidationError::validate_proposer_identity(error).into()
    }
}

fn attestation_for_proposer(payload: &TeeBootstrapV2, proposer: Address) -> Option<[u8; 32]> {
    payload
        .participants
        .iter()
        .find(|participant| Address::from(participant.validator_binding.validator) == proposer)
        .map(|participant| participant.intent.attestation_ed25519)
}

fn witness_mode_for_block(verifier_mode: bool) -> WitnessExecutionMode {
    if !verifier_mode {
        return WitnessExecutionMode::ProposerRecord;
    }
    match witness_policy() {
        Some(WitnessPolicy::ReplayOnly) => WitnessExecutionMode::VerifierReplay,
        Some(WitnessPolicy::EnclaveBacked) => WitnessExecutionMode::VerifierWithEnclave,
        None if outbe_tee::is_enclave_configured() => WitnessExecutionMode::VerifierWithEnclave,
        None => WitnessExecutionMode::VerifierReplay,
    }
}

fn tape_for_witness_mode(
    mode: WitnessExecutionMode,
    block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    leader_attestation_pub: Option<[u8; 32]>,
) -> Result<EnclaveWitnessTapeV1, BlockExecutionError> {
    match mode {
        WitnessExecutionMode::ProposerRecord | WitnessExecutionMode::RpcDirect => {
            proposer_tape(block_artifacts)
        }
        WitnessExecutionMode::VerifierWithEnclave | WitnessExecutionMode::VerifierReplay => {
            verifier_tape(block_artifacts, leader_attestation_pub)
        }
    }
}

fn proposer_tape(
    block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
) -> Result<EnclaveWitnessTapeV1, BlockExecutionError> {
    if block_artifacts.enclave_witness.is_some() {
        return Err(witness_fault_to_block_error(
            BlockWitnessFault::TapeInvalid,
            "prefinal proposer extra_data must not carry enclave_witness",
        ));
    }
    Ok(EnclaveWitnessTapeV1::default())
}

fn verifier_tape(
    block_artifacts: &outbe_primitives::reshare_artifact::OutbeBlockArtifacts,
    leader_attestation_pub: Option<[u8; 32]>,
) -> Result<EnclaveWitnessTapeV1, BlockExecutionError> {
    let Some(payload) = block_artifacts.enclave_witness.as_ref() else {
        return Ok(EnclaveWitnessTapeV1::default());
    };
    if leader_attestation_pub.is_none() {
        return Err(witness_fault_to_block_error(
            BlockWitnessFault::TapeInvalid,
            "block carries enclave_witness but proposer has no enclave attestation key",
        ));
    }
    decode_witness_payload_v1(payload.as_ref()).map_err(|err| {
        witness_fault_to_block_error(
            BlockWitnessFault::TapeInvalid,
            format!("decode block enclave_witness payload: {err}"),
        )
    })
}
