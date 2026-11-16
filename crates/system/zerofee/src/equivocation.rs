//! Zero-fee vote-equivocation evidence is capped at 1_500_000 gas and 16_384
//! calldata bytes. Only canonical ABI encodings (including offsets, padding and
//! exact length) are admitted; the decoded call must re-encode byte-for-byte.
//!
//! Authorization runs fresh read-only SlashIndicator preflight on every attempt;
//! there is no evidence-hash-to-bool authorization cache. Active submitter checks,
//! wire validation, replay markers and accused lifecycle precede the retained
//! committee lookup and two BLS verifications. In the mempool only staked Active
//! submitters can reach that expensive work. The per-block soft-failure cap bounds
//! block re-execution work, not repeated authorization attempts in the mempool.

use alloy_primitives::Bytes;
use alloy_sol_types::{SolCall, SolInterface};
use outbe_primitives::{equivocation_evidence::EquivocationEvidenceKind, storage::StorageHandle};
use outbe_slashindicator::{
    precompile::ISlashIndicator, preflight_equivocation_evidence, validate_equivocation_wire_pair,
    EquivocationPreflightError, VerifiedSlashTarget,
};
use outbe_system_common::addresses::SLASH_INDICATOR_ADDRESS;

use crate::envelope::ZeroFeeEnvelope;
use crate::hooks::{
    ZeroFeeAuthorization, ZeroFeeCandidate, ZeroFeeHook, ZeroFeeHookTag, ZeroFeePolicyError,
    ZeroFeeTransaction, SLASH_EQUIVOCATION_HOOK,
};

/// Maximum gas limit of a zero-fee vote-equivocation submission.
pub const MAX_ZERO_FEE_SLASH_EQUIVOCATION_GAS_LIMIT: u64 = 1_500_000;
const MAX_CALLDATA_BYTES: usize = 16_384;
const MALFORMED: &str = "equivocation evidence calldata or wire pair is not canonical";

/// Protocol fee waiver for the three vote-equivocation evidence selectors.
pub struct SlashEquivocationHook;

fn envelope() -> ZeroFeeEnvelope {
    ZeroFeeEnvelope {
        target: SLASH_INDICATOR_ADDRESS,
        min_max_fee_per_gas: alloy_eips::eip1559::MIN_PROTOCOL_BASE_FEE.into(),
        max_calldata_bytes: MAX_CALLDATA_BYTES,
        max_gas_limit: MAX_ZERO_FEE_SLASH_EQUIVOCATION_GAS_LIMIT,
        malformed_reason: MALFORMED,
    }
}

fn decode(input: &[u8]) -> Result<(EquivocationEvidenceKind, Bytes, Bytes), ZeroFeePolicyError> {
    use ISlashIndicator::ISlashIndicatorCalls as Calls;
    let malformed = || ZeroFeePolicyError::MalformedCalldata(MALFORMED);
    let call = Calls::abi_decode_validate(input).map_err(|_| malformed())?;
    if call.abi_encode() != input {
        return Err(malformed());
    }
    match call {
        Calls::submitConflictingNotarizeEvidence(c) => Ok((
            EquivocationEvidenceKind::ConflictingNotarize,
            c.block1,
            c.block2,
        )),
        Calls::submitConflictingFinalizeEvidence(c) => Ok((
            EquivocationEvidenceKind::ConflictingFinalize,
            c.block1,
            c.block2,
        )),
        Calls::submitNullifyFinalizeEvidence(c) => Ok((
            EquivocationEvidenceKind::NullifyFinalize,
            c.nullifyBlock,
            c.finalizeBlock,
        )),
        _ => Err(malformed()),
    }
}

pub(crate) fn map_preflight_error(error: EquivocationPreflightError) -> ZeroFeePolicyError {
    match error {
        EquivocationPreflightError::SubmitterNotActive(_) => ZeroFeePolicyError::UnauthorizedSigner,
        EquivocationPreflightError::EvidenceAlreadyProcessed(_) => {
            ZeroFeePolicyError::HookConflict("equivocation evidence already processed")
        }
        EquivocationPreflightError::ValidatorAlreadyPenalized(_) => {
            ZeroFeePolicyError::HookConflict("validator already penalized for equivocation epoch")
        }
        EquivocationPreflightError::Rejected(_) => {
            ZeroFeePolicyError::HookConflict("equivocation evidence rejected")
        }
        EquivocationPreflightError::Precompile(error) => ZeroFeePolicyError::from(error),
    }
}

impl ZeroFeeHook for SlashEquivocationHook {
    fn id(&self) -> ZeroFeeHookTag {
        SLASH_EQUIVOCATION_HOOK
    }

    fn classify(
        &self,
        tx: &ZeroFeeTransaction<'_>,
    ) -> Result<Option<ZeroFeeCandidate>, ZeroFeePolicyError> {
        use ISlashIndicator::*;
        let bounds = envelope();
        let candidate = if tx
            .call
            .input
            .starts_with(&submitConflictingNotarizeEvidenceCall::SELECTOR)
        {
            bounds.classify::<submitConflictingNotarizeEvidenceCall>(self.id(), tx)?
        } else if tx
            .call
            .input
            .starts_with(&submitConflictingFinalizeEvidenceCall::SELECTOR)
        {
            bounds.classify::<submitConflictingFinalizeEvidenceCall>(self.id(), tx)?
        } else if tx
            .call
            .input
            .starts_with(&submitNullifyFinalizeEvidenceCall::SELECTOR)
        {
            bounds.classify::<submitNullifyFinalizeEvidenceCall>(self.id(), tx)?
        } else {
            None
        };
        let Some(_) = candidate else {
            return Ok(None);
        };
        let (kind, first, second) = decode(tx.call.input)?;
        validate_equivocation_wire_pair(kind, &first, &second)
            .map_err(|_| ZeroFeePolicyError::MalformedCalldata(MALFORMED))?;
        Ok(Some(ZeroFeeCandidate::new(
            SLASH_EQUIVOCATION_HOOK,
            tx.signer,
        )))
    }

    fn authorize_fee_waiver(
        &self,
        storage: StorageHandle<'_>,
        tx: &ZeroFeeTransaction<'_>,
        candidate: ZeroFeeCandidate,
    ) -> Result<ZeroFeeAuthorization, ZeroFeePolicyError> {
        let (kind, first, second) = decode(tx.call.input)?;
        let _: VerifiedSlashTarget =
            preflight_equivocation_evidence(storage, tx.signer, kind, &first, &second)
                .map_err(map_preflight_error)?;
        Ok(ZeroFeeAuthorization {
            hook: SLASH_EQUIVOCATION_HOOK,
            subject: candidate.signer,
        })
    }
}
