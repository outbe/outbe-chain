//! Restart-safe public submission of one validator-domain OCOMP result vote.
//!
//! The Supervisor owns the validator's role-delegated EVM key, validates the
//! result binding, and locally builds the fixed-shape EIP-1559 transaction.
//! This module durably records every delivery transition. It treats a receipt
//! as final only after it checks the canonical block at its height and the
//! public finalized head.

mod journal;
mod rpc;

use std::path::PathBuf;

use alloy_consensus::{
    transaction::SignerRecoverable as _, EthereumTxEnvelope, Transaction as _, TxEip1559, TxEip4844,
};
use alloy_eips::eip2718::{Decodable2718 as _, Encodable2718 as _};
use alloy_primitives::{Address, Bytes, TxKind, B256, U256};
use outbe_ocomp_protocol::{
    abi::encode_submit_lysis_result_calldata,
    common::BoundedBytes,
    control::FinalizedJobSpecV1,
    system_carrier::{
        MAX_OCOMP_SYSTEM_CARRIER_CALLDATA_BYTES, MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
        OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
    },
    vote::ResultVoteV1,
    PreparedVoteTransactionV1, ProtocolError, SchemaLimits,
};
use outbe_primitives::addresses::METADOSIS_ADDRESS;
use outbe_primitives::signer::{OutbeEvmSigner, SignerError};
use thiserror::Error;

use self::journal::{record_nonce_bypass, record_stage_transition, VoteSubmissionJournalV1};
pub use self::rpc::{PublicVoteRpcClientV1, PublicVoteRpcErrorV1};
#[cfg(test)]
use self::rpc::{canonical_nonce_params, parse_rpc_response};
use crate::result_attestation::{LocalResultAttestationErrorV1, LocalResultVoteAttesterV1};

pub(self) const MAX_RAW_TRANSACTION_BYTES: usize = 4 * 1024;
/// Operational ceiling for the outer OCOMP transaction's fee cap.
///
/// ZeroFee waives the canonical execution debit, but a compromised RPC must
/// still be unable to induce the supervisor to sign an unbounded fee promise.
pub const MAX_OCOMP_SIGNER_MAX_FEE_PER_GAS: u128 = MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS * 2;

/// The price a submitter signs with, held inside the envelope it is about to be
/// checked against. The node's suggestion follows the base fee, which moves with
/// traffic. A submitter that signs it unclamped rejects its own transaction.
pub fn fee_within_envelope(suggested: u128, min_fee: u128, max_fee: u128) -> u128 {
    suggested.clamp(min_fee, max_fee)
}

#[derive(Clone, Debug)]
pub struct VoteSubmissionConfigV1 {
    pub journal_root: PathBuf,
    pub expected_chain_id: u64,
    pub sender_address: Address,
    pub limits: SchemaLimits,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum VoteSubmissionStageV1 {
    Prepared = 1,
    Submitted = 2,
    Included = 3,
    Finalized = 4,
}

impl VoteSubmissionStageV1 {
    fn decode(value: u8) -> Result<Self, VoteSubmissionErrorV1> {
        match value {
            1 => Ok(Self::Prepared),
            2 => Ok(Self::Submitted),
            3 => Ok(Self::Included),
            4 => Ok(Self::Finalized),
            _ => Err(VoteSubmissionErrorV1::InvalidJournal),
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Submitted => "submitted",
            Self::Included => "included",
            Self::Finalized => "finalized",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VoteInclusionV1 {
    pub block_number: u64,
    pub block_hash: B256,
    pub success: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoteSubmissionRecordV1 {
    pub generation: u64,
    pub stage: VoteSubmissionStageV1,
    pub job_id: B256,
    pub result_digest: B256,
    pub sender_address: Address,
    pub nonce: u64,
    pub max_fee_per_gas: u128,
    pub transaction_hash: B256,
    pub raw_transaction: Vec<u8>,
    pub inclusion: Option<VoteInclusionV1>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VoteSubmissionOutcomeV1 {
    Prepared,
    Submitted,
    Included(VoteInclusionV1),
    Finalized(VoteInclusionV1),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VoteSubmissionFailureClassV1 {
    Retryable,
    Unrecoverable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VoteReceiptV1 {
    pub transaction_hash: B256,
    pub block_number: u64,
    pub block_hash: B256,
    pub success: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VoteBlockV1 {
    pub number: u64,
    pub hash: B256,
}

pub trait VoteTransactionPreparerV1 {
    type Error: std::error::Error + Send + Sync + 'static;

    fn prepare_vote_transaction(
        &self,
        canonical_result: &[u8],
        finalized: &FinalizedJobSpecV1,
        canonical_height: u64,
        envelope: (u64, u128, u64),
    ) -> Result<PreparedVoteTransactionV1, Self::Error>;
}

pub struct LocalVoteTransactionPreparerV1 {
    signer: OutbeEvmSigner,
    attester: LocalResultVoteAttesterV1,
    chain_id: u64,
    limits: SchemaLimits,
}

impl LocalVoteTransactionPreparerV1 {
    pub fn new(
        signer: OutbeEvmSigner,
        attester: LocalResultVoteAttesterV1,
        chain_id: u64,
        limits: SchemaLimits,
    ) -> Result<Self, LocalVotePreparationErrorV1> {
        if chain_id == 0 {
            return Err(LocalVotePreparationErrorV1::InvalidChainId);
        }
        Ok(Self {
            signer,
            attester,
            chain_id,
            limits,
        })
    }

    pub const fn sender_address(&self) -> Address {
        self.signer.address()
    }
}

impl VoteTransactionPreparerV1 for LocalVoteTransactionPreparerV1 {
    type Error = LocalVotePreparationErrorV1;

    fn prepare_vote_transaction(
        &self,
        canonical_result: &[u8],
        finalized: &FinalizedJobSpecV1,
        canonical_height: u64,
        (nonce, max_fee_per_gas, gas_limit): (u64, u128, u64),
    ) -> Result<PreparedVoteTransactionV1, Self::Error> {
        if canonical_result.is_empty() {
            return Err(LocalVotePreparationErrorV1::EmptyCanonicalResult);
        }
        if gas_limit != OCOMP_SYSTEM_CARRIER_GAS_LIMIT
            || !(MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS..=MAX_OCOMP_SIGNER_MAX_FEE_PER_GAS)
                .contains(&max_fee_per_gas)
        {
            return Err(LocalVotePreparationErrorV1::InvalidFeeEnvelope {
                max_fee_per_gas,
                min_fee: MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
                max_fee: MAX_OCOMP_SIGNER_MAX_FEE_PER_GAS,
                gas_limit,
                expected_gas_limit: OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
            });
        }

        let vote = self
            .attester
            .attest(canonical_result, finalized, canonical_height)?;
        let canonical_vote = vote.encode_canonical(&self.limits)?;
        let calldata = encode_submit_lysis_result_calldata(&vote, &self.limits)?;
        if calldata.len() > MAX_OCOMP_SYSTEM_CARRIER_CALLDATA_BYTES {
            return Err(LocalVotePreparationErrorV1::CalldataTooLarge);
        }

        let signed = self.signer.sign_eip1559(TxEip1559 {
            chain_id: self.chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas: 0,
            to: TxKind::Call(METADOSIS_ADDRESS),
            value: U256::ZERO,
            input: Bytes::from(calldata),
            access_list: Default::default(),
        })?;
        let transaction_hash = *signed.hash();
        let mut raw_transaction = Vec::new();
        raw_transaction
            .try_reserve_exact(signed.encode_2718_len())
            .map_err(|_| LocalVotePreparationErrorV1::Allocation)?;
        signed.encode_2718(&mut raw_transaction);
        Ok(PreparedVoteTransactionV1 {
            canonical_vote: BoundedBytes(canonical_vote),
            raw_transaction: BoundedBytes(raw_transaction),
            transaction_hash,
        })
    }
}

#[derive(Debug, Error)]
pub enum LocalVotePreparationErrorV1 {
    #[error("OCOMP vote transaction chain id must not be zero")]
    InvalidChainId,
    #[error("OCOMP vote transaction result is empty")]
    EmptyCanonicalResult,
    #[error(
        "OCOMP vote transaction fee envelope is invalid: max fee {max_fee_per_gas} outside \
         {min_fee}..={max_fee}, gas limit {gas_limit} (expected {expected_gas_limit})"
    )]
    InvalidFeeEnvelope {
        max_fee_per_gas: u128,
        min_fee: u128,
        max_fee: u128,
        gas_limit: u64,
        expected_gas_limit: u64,
    },
    #[error("OCOMP vote transaction calldata exceeds the protocol cap")]
    CalldataTooLarge,
    #[error("OCOMP vote transaction allocation failed")]
    Allocation,
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error(transparent)]
    Attestation(#[from] LocalResultAttestationErrorV1),
    #[error(transparent)]
    Signer(#[from] SignerError),
}

pub trait VoteSubmissionRpcV1 {
    type Error: std::error::Error + Send + Sync + 'static;

    fn chain_id(&self) -> Result<u64, Self::Error>;
    fn canonical_nonce(&self, address: Address) -> Result<u64, Self::Error>;
    fn gas_price(&self) -> Result<u128, Self::Error>;
    fn send_raw_transaction(
        &self,
        raw_transaction: &[u8],
        expected_hash: B256,
    ) -> Result<B256, Self::Error>;
    fn transaction_receipt(
        &self,
        transaction_hash: B256,
    ) -> Result<Option<VoteReceiptV1>, Self::Error>;
    fn canonical_block(&self, number: u64) -> Result<Option<VoteBlockV1>, Self::Error>;
    fn finalized_block(&self) -> Result<VoteBlockV1, Self::Error>;
}

pub struct SupervisorVoteSubmitterV1<R> {
    config: VoteSubmissionConfigV1,
    rpc: R,
    journal: VoteSubmissionJournalV1,
}

impl<R: VoteSubmissionRpcV1> SupervisorVoteSubmitterV1<R> {
    pub fn open(config: VoteSubmissionConfigV1, rpc: R) -> Result<Self, VoteSubmissionErrorV1> {
        let journal = VoteSubmissionJournalV1::open(&config.journal_root)?;
        Ok(Self {
            config,
            rpc,
            journal,
        })
    }

    pub fn reconcile<P: VoteTransactionPreparerV1>(
        &mut self,
        preparer: &P,
        job_id: B256,
        result_digest: B256,
        canonical_result: &[u8],
        finalized: &FinalizedJobSpecV1,
    ) -> Result<VoteSubmissionOutcomeV1, VoteSubmissionErrorV1> {
        let record = self.journal.load(job_id)?;
        let Some(record) = record else {
            return self.prepare(
                preparer,
                (job_id, result_digest),
                canonical_result,
                finalized,
                1,
            );
        };
        self.require_record_binding(&record, result_digest)?;
        if matches!(
            record.stage,
            VoteSubmissionStageV1::Prepared | VoteSubmissionStageV1::Submitted
        ) {
            if let Some(canonical_nonce) = self.bypassed_canonical_nonce(&record)? {
                record_nonce_bypass(
                    &record,
                    canonical_nonce,
                    self.journal.stage_age(record.job_id),
                );
                // Same vote, fresh envelope: the pinned nonce was consumed elsewhere.
                return self.prepare(
                    preparer,
                    (job_id, result_digest),
                    canonical_result,
                    finalized,
                    next_generation(record.generation)?,
                );
            }
        }
        match record.stage {
            VoteSubmissionStageV1::Prepared => self.submit(record),
            VoteSubmissionStageV1::Submitted => self.observe_submission(record),
            VoteSubmissionStageV1::Included => self.observe_inclusion(
                preparer,
                (job_id, result_digest),
                canonical_result,
                finalized,
                record,
            ),
            VoteSubmissionStageV1::Finalized => {
                let inclusion = record
                    .inclusion
                    .ok_or(VoteSubmissionErrorV1::InvalidJournal)?;
                if inclusion.success {
                    Ok(VoteSubmissionOutcomeV1::Finalized(inclusion))
                } else {
                    self.prepare(
                        preparer,
                        (job_id, result_digest),
                        canonical_result,
                        finalized,
                        next_generation(record.generation)?,
                    )
                }
            }
        }
    }

    fn prepare<P: VoteTransactionPreparerV1>(
        &mut self,
        preparer: &P,
        (job_id, result_digest): (B256, B256),
        canonical_result: &[u8],
        finalized: &FinalizedJobSpecV1,
        generation: u64,
    ) -> Result<VoteSubmissionOutcomeV1, VoteSubmissionErrorV1> {
        if canonical_result.is_empty() {
            return Err(VoteSubmissionErrorV1::EmptyCanonicalResult);
        }
        let chain_id = self.rpc.chain_id().map_err(rpc_error)?;
        if chain_id != self.config.expected_chain_id {
            return Err(VoteSubmissionErrorV1::WrongRpcChain {
                expected: self.config.expected_chain_id,
                actual: chain_id,
            });
        }
        let nonce = self
            .rpc
            .canonical_nonce(self.config.sender_address)
            .map_err(rpc_error)?;
        let max_fee_per_gas = fee_within_envelope(
            self.rpc.gas_price().map_err(rpc_error)?,
            MIN_OCOMP_SYSTEM_CARRIER_MAX_FEE_PER_GAS,
            MAX_OCOMP_SIGNER_MAX_FEE_PER_GAS,
        );
        let canonical_height = self.rpc.finalized_block().map_err(rpc_error)?.number;
        let prepared = preparer
            .prepare_vote_transaction(
                canonical_result,
                finalized,
                canonical_height,
                (nonce, max_fee_per_gas, OCOMP_SYSTEM_CARRIER_GAS_LIMIT),
            )
            .map_err(preparer_error)?;
        self.validate_prepared(&prepared, job_id, result_digest, nonce, max_fee_per_gas)?;
        let record = VoteSubmissionRecordV1 {
            generation,
            stage: VoteSubmissionStageV1::Prepared,
            job_id,
            result_digest,
            sender_address: self.config.sender_address,
            nonce,
            max_fee_per_gas,
            transaction_hash: prepared.transaction_hash,
            raw_transaction: prepared.raw_transaction.0,
            inclusion: None,
        };
        self.journal.persist(record.clone())?;
        record_stage_transition(&record, None, None);
        Ok(VoteSubmissionOutcomeV1::Prepared)
    }

    fn submit(
        &mut self,
        mut record: VoteSubmissionRecordV1,
    ) -> Result<VoteSubmissionOutcomeV1, VoteSubmissionErrorV1> {
        let prior_stage = record.stage;
        let prior_age = self.journal.stage_age(record.job_id);
        self.broadcast(&record)?;
        record.stage = VoteSubmissionStageV1::Submitted;
        record.generation = next_generation(record.generation)?;
        self.journal.persist(record.clone())?;
        record_stage_transition(&record, Some(prior_stage), prior_age);
        Ok(VoteSubmissionOutcomeV1::Submitted)
    }

    fn observe_submission(
        &mut self,
        mut record: VoteSubmissionRecordV1,
    ) -> Result<VoteSubmissionOutcomeV1, VoteSubmissionErrorV1> {
        let Some(receipt) = self
            .rpc
            .transaction_receipt(record.transaction_hash)
            .map_err(rpc_error)?
        else {
            self.broadcast(&record)?;
            return Ok(VoteSubmissionOutcomeV1::Submitted);
        };
        self.require_receipt_hash(&record, receipt)?;
        if !self.receipt_is_canonical(receipt)? {
            self.broadcast(&record)?;
            return Ok(VoteSubmissionOutcomeV1::Submitted);
        }
        let inclusion = inclusion_from(receipt);
        let prior_stage = record.stage;
        let prior_age = self.journal.stage_age(record.job_id);
        record.stage = VoteSubmissionStageV1::Included;
        record.inclusion = Some(inclusion);
        record.generation = next_generation(record.generation)?;
        self.journal.persist(record.clone())?;
        record_stage_transition(&record, Some(prior_stage), prior_age);
        Ok(VoteSubmissionOutcomeV1::Included(inclusion))
    }

    fn observe_inclusion<P: VoteTransactionPreparerV1>(
        &mut self,
        preparer: &P,
        (job_id, result_digest): (B256, B256),
        canonical_result: &[u8],
        finalized_job: &FinalizedJobSpecV1,
        mut record: VoteSubmissionRecordV1,
    ) -> Result<VoteSubmissionOutcomeV1, VoteSubmissionErrorV1> {
        let prior = record
            .inclusion
            .ok_or(VoteSubmissionErrorV1::InvalidJournal)?;
        let current = self
            .rpc
            .transaction_receipt(record.transaction_hash)
            .map_err(rpc_error)?;
        let Some(receipt) = current else {
            return self.demote_and_rebroadcast(record);
        };
        self.require_receipt_hash(&record, receipt)?;
        if !self.receipt_is_canonical(receipt)? {
            return self.demote_and_rebroadcast(record);
        }
        let inclusion = inclusion_from(receipt);
        if inclusion != prior {
            let prior_age = self.journal.stage_age(record.job_id);
            record.inclusion = Some(inclusion);
            record.generation = next_generation(record.generation)?;
            self.journal.persist(record.clone())?;
            record_stage_transition(&record, Some(VoteSubmissionStageV1::Included), prior_age);
        }
        let finalized = self.rpc.finalized_block().map_err(rpc_error)?;
        if finalized.number < inclusion.block_number {
            return Ok(VoteSubmissionOutcomeV1::Included(inclusion));
        }
        if !inclusion.success {
            return self.prepare(
                preparer,
                (job_id, result_digest),
                canonical_result,
                finalized_job,
                next_generation(record.generation)?,
            );
        }
        let prior_stage = record.stage;
        let prior_age = self.journal.stage_age(record.job_id);
        record.stage = VoteSubmissionStageV1::Finalized;
        record.inclusion = Some(inclusion);
        record.generation = next_generation(record.generation)?;
        self.journal.persist(record.clone())?;
        record_stage_transition(&record, Some(prior_stage), prior_age);
        Ok(VoteSubmissionOutcomeV1::Finalized(inclusion))
    }

    fn demote_and_rebroadcast(
        &mut self,
        mut record: VoteSubmissionRecordV1,
    ) -> Result<VoteSubmissionOutcomeV1, VoteSubmissionErrorV1> {
        let prior_stage = record.stage;
        let prior_age = self.journal.stage_age(record.job_id);
        record.stage = VoteSubmissionStageV1::Submitted;
        record.inclusion = None;
        record.generation = next_generation(record.generation)?;
        self.journal.persist(record.clone())?;
        record_stage_transition(&record, Some(prior_stage), prior_age);
        self.broadcast(&record)?;
        Ok(VoteSubmissionOutcomeV1::Submitted)
    }

    fn broadcast(&self, record: &VoteSubmissionRecordV1) -> Result<(), VoteSubmissionErrorV1> {
        let actual = self
            .rpc
            .send_raw_transaction(&record.raw_transaction, record.transaction_hash)
            .map_err(rpc_error)?;
        if actual != record.transaction_hash {
            return Err(VoteSubmissionErrorV1::TransactionHashMismatch {
                expected: record.transaction_hash,
                actual,
            });
        }
        Ok(())
    }

    /// Receipt is checked after the nonce read, so a racing inclusion keeps
    /// the record.
    fn bypassed_canonical_nonce(
        &self,
        record: &VoteSubmissionRecordV1,
    ) -> Result<Option<u64>, VoteSubmissionErrorV1> {
        let canonical_nonce = self
            .rpc
            .canonical_nonce(record.sender_address)
            .map_err(rpc_error)?;
        if canonical_nonce <= record.nonce {
            return Ok(None);
        }
        let receipt = self
            .rpc
            .transaction_receipt(record.transaction_hash)
            .map_err(rpc_error)?;
        Ok(receipt.is_none().then_some(canonical_nonce))
    }

    fn receipt_is_canonical(&self, receipt: VoteReceiptV1) -> Result<bool, VoteSubmissionErrorV1> {
        Ok(self
            .rpc
            .canonical_block(receipt.block_number)
            .map_err(rpc_error)?
            .is_some_and(|block| {
                block.number == receipt.block_number && block.hash == receipt.block_hash
            }))
    }

    fn require_receipt_hash(
        &self,
        record: &VoteSubmissionRecordV1,
        receipt: VoteReceiptV1,
    ) -> Result<(), VoteSubmissionErrorV1> {
        if receipt.transaction_hash != record.transaction_hash {
            return Err(VoteSubmissionErrorV1::TransactionHashMismatch {
                expected: record.transaction_hash,
                actual: receipt.transaction_hash,
            });
        }
        Ok(())
    }

    fn require_record_binding(
        &self,
        record: &VoteSubmissionRecordV1,
        result_digest: B256,
    ) -> Result<(), VoteSubmissionErrorV1> {
        if record.result_digest != result_digest
            || record.sender_address != self.config.sender_address
        {
            return Err(VoteSubmissionErrorV1::DifferentResultForJournaledJob);
        }
        Ok(())
    }

    fn validate_prepared(
        &self,
        prepared: &PreparedVoteTransactionV1,
        job_id: B256,
        result_digest: B256,
        nonce: u64,
        max_fee_per_gas: u128,
    ) -> Result<(), VoteSubmissionErrorV1> {
        let vote = ResultVoteV1::decode_canonical(&prepared.canonical_vote.0, &self.config.limits)?;
        if vote.job_id != job_id || vote.result_digest(&self.config.limits)? != result_digest {
            return Err(VoteSubmissionErrorV1::PreparerChangedResult);
        }
        let expected_calldata = encode_submit_lysis_result_calldata(&vote, &self.config.limits)?;
        if prepared.raw_transaction.0.len() > MAX_RAW_TRANSACTION_BYTES {
            return Err(VoteSubmissionErrorV1::RawTransactionTooLarge);
        }
        let mut raw = prepared.raw_transaction.0.as_slice();
        let transaction =
            EthereumTxEnvelope::<TxEip4844>::decode_2718(&mut raw).map_err(|error| {
                VoteSubmissionErrorV1::InvalidPreparedTransaction(error.to_string())
            })?;
        if !raw.is_empty() || !matches!(&transaction, EthereumTxEnvelope::Eip1559(_)) {
            return Err(VoteSubmissionErrorV1::InvalidPreparedTransaction(
                "not one exact EIP-1559 envelope".to_owned(),
            ));
        }
        let recovered = transaction.recover_signer().map_err(|error| {
            VoteSubmissionErrorV1::InvalidPreparedTransaction(error.to_string())
        })?;
        let actual_fields = (
            recovered,
            transaction.chain_id(),
            transaction.nonce(),
            transaction.gas_limit(),
            transaction.max_fee_per_gas(),
            transaction.max_priority_fee_per_gas(),
            transaction.kind(),
            transaction.value(),
            transaction.input().as_ref(),
        );
        let expected_fields = (
            self.config.sender_address,
            Some(self.config.expected_chain_id),
            nonce,
            OCOMP_SYSTEM_CARRIER_GAS_LIMIT,
            max_fee_per_gas,
            Some(0),
            TxKind::Call(METADOSIS_ADDRESS),
            U256::ZERO,
            expected_calldata.as_slice(),
        );
        if actual_fields != expected_fields {
            return Err(VoteSubmissionErrorV1::InvalidPreparedTransaction(
                "restricted vote transaction fields changed".to_owned(),
            ));
        }
        let decoded_hash = *transaction.tx_hash();
        if decoded_hash != prepared.transaction_hash {
            return Err(VoteSubmissionErrorV1::TransactionHashMismatch {
                expected: prepared.transaction_hash,
                actual: decoded_hash,
            });
        }
        Ok(())
    }
}

fn inclusion_from(receipt: VoteReceiptV1) -> VoteInclusionV1 {
    VoteInclusionV1 {
        block_number: receipt.block_number,
        block_hash: receipt.block_hash,
        success: receipt.success,
    }
}

fn next_generation(generation: u64) -> Result<u64, VoteSubmissionErrorV1> {
    generation
        .checked_add(1)
        .ok_or(VoteSubmissionErrorV1::JournalGenerationOverflow)
}

fn rpc_error(error: impl std::error::Error + 'static) -> VoteSubmissionErrorV1 {
    let retryable = (&error as &dyn std::error::Error)
        .downcast_ref::<PublicVoteRpcErrorV1>()
        .is_none_or(|error| error.is_retryable());
    if retryable {
        VoteSubmissionErrorV1::RetryableRpc(error.to_string())
    } else {
        VoteSubmissionErrorV1::UnrecoverableRpc(error.to_string())
    }
}

fn preparer_error(error: impl std::error::Error) -> VoteSubmissionErrorV1 {
    VoteSubmissionErrorV1::Preparer(error.to_string())
}

#[derive(Debug, Error)]
pub enum VoteSubmissionErrorV1 {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    #[error("vote transaction preparation failed: {0}")]
    Preparer(String),
    #[error("vote submission public RPC temporarily failed: {0}")]
    RetryableRpc(String),
    #[error("vote submission public RPC returned an unrecoverable response: {0}")]
    UnrecoverableRpc(String),
    #[error("vote submission RPC is for chain {actual}, expected {expected}")]
    WrongRpcChain { expected: u64, actual: u64 },
    #[error("transaction preparer returned a result for another job or digest")]
    PreparerChangedResult,
    #[error("transaction preparer returned an invalid restricted vote transaction: {0}")]
    InvalidPreparedTransaction(String),
    #[error("transaction preparer returned an oversized raw vote transaction")]
    RawTransactionTooLarge,
    #[error("empty canonical result cannot prepare a vote transaction")]
    EmptyCanonicalResult,
    #[error("vote transaction hash mismatch: expected {expected}, got {actual}")]
    TransactionHashMismatch { expected: B256, actual: B256 },
    #[error("another result is already journaled for this job")]
    DifferentResultForJournaledJob,
    #[error("vote submission journal generation overflow")]
    JournalGenerationOverflow,
    #[error("vote submission journal is malformed")]
    InvalidJournal,
    #[error("vote submission journal checksum mismatch")]
    JournalChecksumMismatch,
    #[error("unsupported vote submission journal version {0}")]
    UnsupportedJournalVersion(u16),
    #[error("vote submission journal exceeds its fixed bound")]
    JournalTooLarge,
    #[error("vote submission journal has an ambiguous temporary file at {0}")]
    AmbiguousJournal(PathBuf),
    #[error("vote submission path is unsafe: {0}")]
    UnsafePath(PathBuf),
    #[error("vote submission I/O while trying to {operation} at {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl VoteSubmissionErrorV1 {
    #[must_use]
    pub const fn class(&self) -> VoteSubmissionFailureClassV1 {
        match self {
            Self::RetryableRpc(_) => VoteSubmissionFailureClassV1::Retryable,
            Self::Protocol(_)
            | Self::Preparer(_)
            | Self::UnrecoverableRpc(_)
            | Self::WrongRpcChain { .. }
            | Self::PreparerChangedResult
            | Self::InvalidPreparedTransaction(_)
            | Self::RawTransactionTooLarge
            | Self::EmptyCanonicalResult
            | Self::TransactionHashMismatch { .. }
            | Self::DifferentResultForJournaledJob
            | Self::JournalGenerationOverflow
            | Self::InvalidJournal
            | Self::JournalChecksumMismatch
            | Self::UnsupportedJournalVersion(_)
            | Self::JournalTooLarge
            | Self::AmbiguousJournal(_)
            | Self::UnsafePath(_)
            | Self::Io { .. } => VoteSubmissionFailureClassV1::Unrecoverable,
        }
    }
}

#[cfg(test)]
mod tests;
