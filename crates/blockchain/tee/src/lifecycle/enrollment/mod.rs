//! One enrollment step per finalized observation; all resident secrets remain in RAM.
use alloy_consensus::TxEip1559;
use alloy_primitives::{Address, Signature, B256, U256};
use outbe_primitives::tee_attestation_v1::{AttestationMode, RegistrationIntentV1};
use std::path::Path;

use super::{LifecycleChainPort, ServiceTxJournalV1};
use crate::in_process::InProcessTeeContext;

mod anchor;
mod ingest;
mod preflight;
mod submit;
pub use anchor::{
    load_live_join_admission_anchor_v1, persist_live_join_admission_anchor_v1, LiveJoinAnchorError,
};
pub use preflight::{EnrollmentMode, EnrollmentPreflightV1};
pub use submit::EnrollmentSignerV1;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnrollmentOutcome {
    WaitForUnjail,
    WaitForGasBalance,
    WaitForLeaseExpiry,
    RequiresStrictUpgradeLateRecovery,
    WaitForFinality,
    Submitted {
        tx_hash: B256,
    },
    Admitted {
        anchor: Box<crate::FinalizedJoinAdmissionAnchorV1>,
    },
}

/// Registration lease and gas budget. Fees use the signed EIP-1559 maximum.
#[derive(Clone, Copy, Debug)]
pub struct EnrollmentConfigV1 {
    pub sender: Address,
    pub binding_id: B256,
    pub valid_until: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
}

/// Borrowed inputs for one resident process; no key material is persisted by this value.
pub struct EnrollmentSessionV1<'a> {
    pub live: &'a InProcessTeeContext,
    pub config: EnrollmentConfigV1,
    pub node_data_dir: &'a Path,
}

#[derive(Debug, thiserror::Error)]
pub enum EnrollmentError<E: std::error::Error + 'static, S: std::error::Error + 'static> {
    #[error(transparent)]
    Port(#[from] super::LifecyclePortError<E>),
    #[error(transparent)]
    Journal(#[from] super::ServiceTxJournalError),
    #[error(transparent)]
    Reconcile(#[from] super::ReconcileError<E>),
    #[error(transparent)]
    Resident(#[from] crate::in_process::InProcessError),
    #[error(transparent)]
    Codec(#[from] outbe_primitives::tee_attestation_v1::CodecError),
    #[error(transparent)]
    Anchor(#[from] LiveJoinAnchorError),
    #[error("registration signing failed")]
    Signer(#[source] S),
    #[error("registration collateral or proof rejected")]
    Proof(#[from] crate::TransportError),
    #[error("finalized registration belongs to a competing binding")]
    CompetingLiveBinding,
    #[error("finalized chain, policy or node identity mismatch")]
    IdentityMismatch,
    #[error("registration counters exhausted")]
    CounterExhausted,
    #[error("registration lease, fee or binding id invalid")]
    InvalidConfiguration,
    #[error("pending journal is not a registration for this sender")]
    WrongServiceTransaction,
    #[error("registration transaction codec rejected")]
    TransactionDecode(#[from] alloy_rlp::Error),
    #[error("finalized admission witness codec rejected")]
    AdmissionCodec(#[from] crate::finalized_admission::FinalizedAdmissionCodecError),
    #[error("registration ABI rejected")]
    Abi(#[from] alloy_sol_types::Error),
    #[error("registration signature rejected")]
    Signature(#[from] alloy_primitives::SignatureError),
    #[error("signed transaction or signature differs from requested registration")]
    InvalidSignature,
    #[error("RPC returned a different transaction hash")]
    TransactionHashMismatch,
}

/// A tick performs bounded I/O, never sleeps, and keeps the caller's RAM context intact.
/// The runtime retries wait outcomes on a new finalized observation.
pub async fn enroll_step_v1<P: LifecycleChainPort, S: EnrollmentSignerV1>(
    port: &P,
    signer: &S,
    journal: &mut ServiceTxJournalV1,
    session: EnrollmentSessionV1<'_>,
) -> Result<EnrollmentOutcome, EnrollmentError<P::Error, S::Error>> {
    let EnrollmentSessionV1 {
        live,
        config,
        node_data_dir,
    } = session;
    if journal
        .load()?
        .is_some_and(|record| record.sender != config.sender)
    {
        return Err(EnrollmentError::WrongServiceTransaction);
    }
    let reconciliation = super::reconcile_service_tx_v1(port, journal, live, config.sender).await?;
    let same_process_pending_register = journal.load()?.is_some_and(|record| {
        record.kind == super::ServiceTxKindV1::Register
            && record.state == super::ServiceTxStateV1::Pending
            && live
                .manifest()
                .enclave_id()
                .is_ok_and(|id| record.public_intent.target.enclave_id == id)
    });
    if matches!(
        reconciliation,
        super::ServiceTxReconciliationV1::WaitForFinality { .. }
    ) && !same_process_pending_register
    {
        return Ok(EnrollmentOutcome::WaitForFinality);
    }
    let observation = preflight::observe(port, live, config.sender).await?;
    if let Some(intent) = preflight::registered_intent(&observation, live)? {
        return ingest::ingest(port, live, &observation, &intent, node_data_dir).await;
    }
    if let Some(outcome) = preflight::wait_or_reject(&observation)? {
        return Ok(outcome);
    }
    submit::advance(
        port,
        signer,
        journal,
        submit::SubmissionContext {
            live,
            config,
            view: &observation,
        },
        reconciliation,
    )
    .await
}

// The signer handles only public transactions and hashes, never resident key material.
fn signed_bytes(tx: TxEip1559, signature: Signature) -> alloy_primitives::Bytes {
    use alloy_consensus::transaction::RlpEcdsaEncodableTx as _;
    let mut bytes = vec![2];
    tx.rlp_encode_signed(&signature, &mut bytes);
    bytes.into()
}

#[cfg(test)]
mod tests;
