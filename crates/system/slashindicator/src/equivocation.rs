//! Equivocation evidence permits one felony per (accused, epoch) across all
//! five entry points. Slot 8 stores both canonical pair hashes and domain-separated
//! epoch-felony keys. Active, JailRetained, Jail, Exiting and Unbonding remain
//! slashable; pending and inactive validators cannot consume evidence markers.

use alloy_primitives::{keccak256, Address, B256};
use outbe_primitives::equivocation_evidence::EquivocationEvidenceKind;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::ValidatorLifecycle;

use crate::errors::SlashIndicatorError;
use crate::evidence::{EvidenceBlock, EvidenceCommittee};
use crate::runtime::{canonical_evidence_hash, registered_validator_typed, SlashRuntimeError};
use crate::schema::SlashIndicator;

/// A verified target carries no mutable authority and performs no writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedSlashTarget {
    pub accused: Address,
    pub epoch: u64,
    pub evidence_hash: B256,
}

/// Preflight failures retain domain errors and provider causes.
#[derive(Debug, thiserror::Error)]
pub enum EquivocationPreflightError {
    #[error("submitter not active: {0}")]
    SubmitterNotActive(#[source] SlashIndicatorError),
    #[error("evidence already processed: {0}")]
    EvidenceAlreadyProcessed(#[source] SlashIndicatorError),
    #[error("validator already penalized: {0}")]
    ValidatorAlreadyPenalized(#[source] SlashIndicatorError),
    #[error("evidence rejected: {0}")]
    Rejected(#[source] SlashIndicatorError),
    #[error("precompile: {0}")]
    Precompile(#[source] PrecompileError),
}

impl From<EquivocationPreflightError> for PrecompileError {
    fn from(error: EquivocationPreflightError) -> Self {
        match error {
            EquivocationPreflightError::SubmitterNotActive(error)
            | EquivocationPreflightError::EvidenceAlreadyProcessed(error)
            | EquivocationPreflightError::ValidatorAlreadyPenalized(error)
            | EquivocationPreflightError::Rejected(error) => error.into(),
            EquivocationPreflightError::Precompile(error) => error,
        }
    }
}

/// Domain separation from validated pair preimages (at least 290 bytes).
pub(crate) fn equivocation_epoch_felony_key(validator: Address, epoch: u64) -> B256 {
    let mut bytes = [0u8; 62];
    bytes[..34].copy_from_slice(b"outbe:equivocation-epoch-felony:v1");
    bytes[34..54].copy_from_slice(validator.as_slice());
    bytes[54..].copy_from_slice(&epoch.to_be_bytes());
    keccak256(bytes)
}

/// Signature check of one evidence block against the epoch committee.
type VerifyBlock =
    fn(&EvidenceBlock, &EvidenceCommittee) -> core::result::Result<(), SlashIndicatorError>;

/// Two evidence blocks from the same signer, with their raw bytes for the
/// order-independent dedup hash.
struct EvidencePair<'a> {
    first: EvidenceBlock,
    second: EvidenceBlock,
    raw: (&'a [u8], &'a [u8]),
}

impl<'a> EvidencePair<'a> {
    /// Parses both blocks and requires one signer.
    fn parse(
        block1: &'a [u8],
        block2: &'a [u8],
    ) -> core::result::Result<Self, SlashIndicatorError> {
        let first = EvidenceBlock::parse(block1)?;
        let second = EvidenceBlock::parse(block2)?;
        if first.pubkey != second.pubkey {
            return Err(SlashIndicatorError::EvidenceSignerMismatch);
        }
        Ok(Self {
            first,
            second,
            raw: (block1, block2),
        })
    }

    /// Order-independent dedup hash of the two raw blocks. Callers compute it
    /// after the class checks and before the `evidence_processed` read.
    fn evidence_hash(&self) -> B256 {
        canonical_evidence_hash(self.raw.0, self.raw.1)
    }

    /// Requires one round (epoch + view) and returns it. `mismatch` is the
    /// revert reason.
    fn same_round(
        &self,
        mismatch: SlashIndicatorError,
    ) -> core::result::Result<(u64, u64), SlashIndicatorError> {
        let round = self.first.round()?;
        if round != self.second.round()? {
            return Err(mismatch);
        }
        Ok(round)
    }

    fn require_distinct_proposals(
        &self,
        same: SlashIndicatorError,
    ) -> core::result::Result<(), SlashIndicatorError> {
        if self.first.proposal_bytes == self.second.proposal_bytes {
            return Err(same);
        }
        Ok(())
    }

    fn validate_kind(
        &self,
        kind: EquivocationEvidenceKind,
    ) -> core::result::Result<(u64, u64), SlashIndicatorError> {
        let round = self.same_round(SlashIndicatorError::VotesMustBeSameRound)?;
        if kind != EquivocationEvidenceKind::NullifyFinalize {
            self.require_distinct_proposals(
                SlashIndicatorError::ConflictingVotesMustBeDifferentProposals,
            )?;
        }
        Ok(round)
    }

    /// True when `first` verifies with `verify_first` and `second` verifies
    /// with `verify_second`.
    fn verifies_as(
        &self,
        committee: &EvidenceCommittee,
        verify_first: VerifyBlock,
        verify_second: VerifyBlock,
    ) -> bool {
        verify_first(&self.first, committee).is_ok()
            && verify_second(&self.second, committee).is_ok()
    }
}

/// Validate wire structure without storage access or cryptographic work.
pub fn validate_equivocation_wire_pair(
    kind: EquivocationEvidenceKind,
    block1: &[u8],
    block2: &[u8],
) -> core::result::Result<(), SlashIndicatorError> {
    EvidencePair::parse(block1, block2)?
        .validate_kind(kind)
        .map(|_| ())
}

/// Read-only preflight; storage markers precede committee access and BLS.
pub fn preflight_equivocation_evidence(
    storage: StorageHandle<'_>,
    caller: Address,
    kind: EquivocationEvidenceKind,
    block1: &[u8],
    block2: &[u8],
) -> core::result::Result<VerifiedSlashTarget, EquivocationPreflightError> {
    let si = SlashIndicator::new(storage);
    si.preflight_submitter(caller)?;
    let pair = EvidencePair::parse(block1, block2).map_err(EquivocationPreflightError::Rejected)?;
    let round = pair
        .validate_kind(kind)
        .map_err(EquivocationPreflightError::Rejected)?;
    let target = si.preflight_target(&pair, round.0)?;
    let committee = si
        .committee_set_for_epoch_typed(round.0)
        .map_err(EquivocationPreflightError::from)?;
    let (first, second): (VerifyBlock, VerifyBlock) = match kind {
        EquivocationEvidenceKind::ConflictingNotarize => (
            EvidenceBlock::verify_notarize_signature,
            EvidenceBlock::verify_notarize_signature,
        ),
        EquivocationEvidenceKind::ConflictingFinalize => (
            EvidenceBlock::verify_finalize_signature,
            EvidenceBlock::verify_finalize_signature,
        ),
        EquivocationEvidenceKind::NullifyFinalize => (
            EvidenceBlock::verify_nullify_signature,
            EvidenceBlock::verify_finalize_signature,
        ),
    };
    first(&pair.first, &committee).map_err(EquivocationPreflightError::Rejected)?;
    second(&pair.second, &committee).map_err(EquivocationPreflightError::Rejected)?;
    Ok(target)
}

impl From<SlashRuntimeError> for EquivocationPreflightError {
    fn from(error: SlashRuntimeError) -> Self {
        match error {
            SlashRuntimeError::Domain(error) => Self::Rejected(error),
            SlashRuntimeError::Precompile(error) => Self::Precompile(error),
        }
    }
}

impl SlashIndicator<'_> {
    /// Submits double-proposal evidence.
    ///
    /// Each evidence block is encoded as:
    ///   `pubkey[48] || signature[96] || proposal_encoded[variable]`
    ///
    /// Verification:
    /// 1. Both blocks must have the same BLS MinPk signer public key.
    /// 2. Both proposals must be for the same round (epoch + view).
    /// 3. The proposal bytes must differ (two different proposals for the same round).
    /// 4. Both BLS signatures must be valid (signed over the Simplex notarize payload).
    /// 5. The signer must be a registered validator.
    ///
    /// On success: the runtime jails and slashes the validator (felony). The
    /// evidence submitter receives a reward (evidence_reward_percent of slashed amount).
    pub fn submit_double_proposal_evidence(
        &mut self,
        caller: Address,
        block1: &[u8],
        block2: &[u8],
    ) -> Result<()> {
        self.preflight_submitter(caller)?;
        let pair = EvidencePair::parse(block1, block2)?;
        pair.require_distinct_proposals(SlashIndicatorError::ProposalsMustDifferForDoubleProposal)?;
        let round = pair.same_round(SlashIndicatorError::ProposalsMustBeSameRound)?;
        let target = self.preflight_target(&pair, round.0)?;

        // Verify both signatures under the committee that ran the evidence's epoch
        // (notarize namespace is committee-bound).
        let committee = self.committee_set_for_epoch(round.0)?;
        pair.first.verify_notarize_signature(&committee)?;
        pair.second.verify_notarize_signature(&committee)?;

        self.penalize_evidence_signer(target, caller)
    }

    /// Submits conflicting vote evidence (notarize + nullify in the same view).
    ///
    /// Each evidence block is encoded as:
    ///   `pubkey[48] || signature[96] || payload_bytes[variable]`
    ///
    /// One vote must be a valid notarize signature and the other a valid nullify
    /// signature for the same round (epoch + view) by the same signer. This proves
    /// the validator voted both to accept and skip the same view.
    ///
    /// On success: the runtime jails and slashes the validator (felony). The
    /// evidence submitter receives a reward.
    pub fn submit_conflicting_vote_evidence(
        &mut self,
        caller: Address,
        vote1: &[u8],
        vote2: &[u8],
    ) -> Result<()> {
        self.preflight_submitter(caller)?;
        let pair = EvidencePair::parse(vote1, vote2)?;
        let round = pair.same_round(SlashIndicatorError::VotesMustBeSameRound)?;
        let target = self.preflight_target(&pair, round.0)?;

        // Verify conflicting vote types: one must be notarize, the other nullify.
        // Try ev1=notarize + ev2=nullify first, then the reverse. Both
        // namespaces are committee-bound, so verify under the epoch's committee.
        let committee = self.committee_set_for_epoch(round.0)?;
        let notarize_then_nullify = pair.verifies_as(
            &committee,
            EvidenceBlock::verify_notarize_signature,
            EvidenceBlock::verify_nullify_signature,
        );
        if !notarize_then_nullify
            && !pair.verifies_as(
                &committee,
                EvidenceBlock::verify_nullify_signature,
                EvidenceBlock::verify_notarize_signature,
            )
        {
            return Err(SlashIndicatorError::ConflictingVoteSignaturesInvalid.into());
        }

        self.penalize_evidence_signer(target, caller)
    }

    /// Verify once, then apply the common epoch-scoped felony.
    fn apply_equivocation_felony(
        &mut self,
        caller: Address,
        block1: &[u8],
        block2: &[u8],
        kind: EquivocationEvidenceKind,
    ) -> Result<()> {
        let target =
            preflight_equivocation_evidence(self.storage.clone(), caller, kind, block1, block2)?;
        self.penalize_evidence_signer(target, caller)
    }

    /// `ConflictingNotarize`: the same signer notarized two DIFFERENT proposals
    /// in one view.
    pub fn submit_conflicting_notarize_evidence(
        &mut self,
        caller: Address,
        block1: &[u8],
        block2: &[u8],
    ) -> Result<()> {
        self.apply_equivocation_felony(
            caller,
            block1,
            block2,
            EquivocationEvidenceKind::ConflictingNotarize,
        )
    }

    /// `ConflictingFinalize`: the same signer finalized two DIFFERENT proposals
    /// in one view.
    pub fn submit_conflicting_finalize_evidence(
        &mut self,
        caller: Address,
        block1: &[u8],
        block2: &[u8],
    ) -> Result<()> {
        self.apply_equivocation_felony(
            caller,
            block1,
            block2,
            EquivocationEvidenceKind::ConflictingFinalize,
        )
    }

    /// `NullifyFinalize`: the same signer both nullified (voted to skip) and
    /// finalized the same view. `nullify_block` is the nullify vote,
    /// `finalize_block` the finalize vote.
    pub fn submit_nullify_finalize_evidence(
        &mut self,
        caller: Address,
        nullify_block: &[u8],
        finalize_block: &[u8],
    ) -> Result<()> {
        self.apply_equivocation_felony(
            caller,
            nullify_block,
            finalize_block,
            EquivocationEvidenceKind::NullifyFinalize,
        )
    }

    fn preflight_submitter(
        &self,
        caller: Address,
    ) -> core::result::Result<(), EquivocationPreflightError> {
        self.require_active_submitter_typed(caller)
            .map_err(|error| match error {
                SlashRuntimeError::Domain(
                    error @ SlashIndicatorError::SubmitterNotActiveValidator { .. },
                ) => EquivocationPreflightError::SubmitterNotActive(error),
                other => other.into(),
            })
    }

    /// Exact-pair replay precedes lifecycle and epoch-felony checks.
    fn preflight_target(
        &self,
        pair: &EvidencePair<'_>,
        epoch: u64,
    ) -> core::result::Result<VerifiedSlashTarget, EquivocationPreflightError> {
        use EquivocationPreflightError::{Precompile, Rejected, ValidatorAlreadyPenalized};
        let evidence_hash = pair.evidence_hash();
        self.require_unprocessed_evidence(evidence_hash)?;
        let vs = ValidatorSet::new(self.storage.clone());
        let accused = registered_validator_typed(&vs, pair.first.pubkey_hash())
            .map_err(EquivocationPreflightError::from)?;
        let lifecycle = vs.validator_lifecycle(accused).map_err(Precompile)?;
        if !matches!(
            lifecycle,
            ValidatorLifecycle::Active(_)
                | ValidatorLifecycle::JailRetained(_)
                | ValidatorLifecycle::Jail(_)
                | ValidatorLifecycle::Exiting(_)
                | ValidatorLifecycle::Unbonding(_)
        ) {
            return Err(Rejected(SlashIndicatorError::ValidatorNotSlashable {
                validator: accused,
                status: lifecycle.stored_status().unwrap_or_default(),
            }));
        }
        if self
            .evidence_processed
            .read(&equivocation_epoch_felony_key(accused, epoch))
            .map_err(Precompile)?
        {
            return Err(ValidatorAlreadyPenalized(
                SlashIndicatorError::ValidatorAlreadyPenalized {
                    validator: accused,
                    epoch,
                },
            ));
        }
        Ok(VerifiedSlashTarget {
            accused,
            epoch,
            evidence_hash,
        })
    }

    fn require_unprocessed_evidence(
        &self,
        evidence_hash: B256,
    ) -> core::result::Result<(), EquivocationPreflightError> {
        if self
            .evidence_processed
            .read(&evidence_hash)
            .map_err(EquivocationPreflightError::Precompile)?
        {
            return Err(EquivocationPreflightError::EvidenceAlreadyProcessed(
                SlashIndicatorError::EvidenceAlreadyProcessed,
            ));
        }
        Ok(())
    }

    /// Commit both markers before applying the already verified felony.
    fn penalize_evidence_signer(
        &mut self,
        target: VerifiedSlashTarget,
        caller: Address,
    ) -> Result<()> {
        let guard = self.storage.checkpoint_guard()?;
        self.evidence_processed.write(&target.evidence_hash, true)?;
        self.evidence_processed.write(
            &equivocation_epoch_felony_key(target.accused, target.epoch),
            true,
        )?;
        self.apply_evidence_felony(target.accused, caller)?;
        guard.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::precompile::ISlashIndicator;
    use crate::test_signing;
    use alloy_primitives::U256;
    use alloy_sol_types::SolError;
    use outbe_primitives::storage::hashmap::HashMapStorageProvider;
    use outbe_validatorset::test_support::StorageOverrides;
    use EquivocationPreflightError::{
        EvidenceAlreadyProcessed, Rejected, SubmitterNotActive, ValidatorAlreadyPenalized,
    };

    #[test]
    fn wire_validation_keeps_typed_structure_errors() {
        let kind = EquivocationEvidenceKind::ConflictingNotarize;
        assert!(matches!(
            validate_equivocation_wire_pair(kind, &[0; 144], &[0; 145]),
            Err(SlashIndicatorError::EvidenceBlockTooShort)
        ));
        let block = |payload: &[u8]| [vec![1; 144], payload.to_vec()].concat();
        for (payload, expected) in [
            (
                vec![0x80, 0, 1],
                ISlashIndicator::NonCanonicalVarint::SELECTOR,
            ),
            (vec![0x80], ISlashIndicator::UnexpectedEndOfVarint::SELECTOR),
            (
                [vec![0xff; 9], vec![2]].concat(),
                ISlashIndicator::VarintOverflow::SELECTOR,
            ),
        ] {
            let error: PrecompileError =
                validate_equivocation_wire_pair(kind, &block(&payload), &block(&payload))
                    .unwrap_err()
                    .into();
            assert!(matches!(error, PrecompileError::RevertBytes(bytes) if bytes[..4] == expected));
        }
    }

    #[test]
    fn provider_error_conversion_preserves_typed_source() {
        use std::error::Error as _;
        for error in [
            PrecompileError::Storage("storage sentinel".into()),
            PrecompileError::Fatal("fatal sentinel".into()),
            PrecompileError::FatalWithCause(Box::new(std::io::Error::other("typed sentinel"))),
        ] {
            let wrapped = EquivocationPreflightError::from(SlashRuntimeError::Precompile(error));
            assert!(wrapped.source().unwrap().is::<PrecompileError>());
            let restored = PrecompileError::from(wrapped);
            match restored {
                PrecompileError::Storage(message) => assert_eq!(message, "storage sentinel"),
                PrecompileError::Fatal(message) => assert_eq!(message, "fatal sentinel"),
                PrecompileError::FatalWithCause(cause) => assert!(cause.is::<std::io::Error>()),
                other => panic!("lost provider error: {other:?}"),
            }
        }
    }

    #[test]
    fn typed_preflight_orders_acl_structure_pair_and_epoch_before_bls() {
        let mut provider = HashMapStorageProvider::new(1);
        provider.set_block_number(1);
        provider.enter(|storage| {
            let caller = Address::repeat_byte(0x11);
            let accused = Address::repeat_byte(0x22);
            let owner = Address::repeat_byte(0x33);
            let (sk, pk) = test_signing::keypair(77).unwrap();
            let mut vs = ValidatorSet::new(storage.clone());
            vs.test_configure_registry(owner).unwrap();
            let mut epoch = vs.epoch_snapshot().unwrap();
            epoch.number = U256::from(1);
            vs.test_set_epoch_snapshot(epoch).unwrap();
            vs.test_seed_authoritative_min_stake(U256::from(1)).unwrap();
            vs.test_register_active_validator(caller, &[0x77; 48], U256::from(1))
                .unwrap();
            vs.test_register_active_validator(accused, &pk.to_bytes(), U256::from(100))
                .unwrap();
            let kind = EquivocationEvidenceKind::ConflictingNotarize;
            let check = |caller, a: &[u8], b: &[u8]| {
                preflight_equivocation_evidence(storage.clone(), caller, kind, a, b)
            };
            assert!(matches!(
                check(Address::ZERO, &[], &[]),
                Err(SubmitterNotActive(_))
            ));
            assert!(matches!(
                check(caller, &[], &[]),
                Err(Rejected(SlashIndicatorError::EvidenceBlockTooShort))
            ));
            let committee = test_signing::committee();
            let snapshot = outbe_validatorset::state::CommitteeSnapshot {
                committee: outbe_simplex::test_harness::committee_entries(committee.iter()),
                vrf_material_version: 1,
                vrf_group_public_key_bytes: vec![0x11; 96],
                vrf_public_polynomial_hash: B256::ZERO,
            };
            for member in &snapshot.committee {
                vs.test_register_active_validator(
                    member.address,
                    &member.consensus_pubkey,
                    U256::from(1),
                )
                .unwrap();
            }
            vs.test_seed_committee_ring(owner, &snapshot).unwrap();
            let ns = outbe_primitives::proof::notarize_namespace(&committee);
            let valid_block = |digest| {
                test_signing::signed_evidence(
                    &sk,
                    &pk,
                    &ns,
                    &test_signing::proposal(1, 5, 4, [digest; 32]),
                    test_signing::POP_DST,
                )
            };
            let valid_a = valid_block(1);
            let valid_b = valid_block(2);
            let target = check(caller, &valid_a, &valid_b).unwrap();
            assert_eq!(
                target,
                VerifiedSlashTarget {
                    accused,
                    epoch: 1,
                    evidence_hash: canonical_evidence_hash(&valid_a, &valid_b),
                }
            );
            let si = SlashIndicator::new(storage.clone());
            assert!(!si.is_evidence_processed(target.evidence_hash).unwrap());
            assert!(!si
                .is_evidence_processed(equivocation_epoch_felony_key(accused, 1))
                .unwrap());
            assert_eq!(si.get_felony_count(accused).unwrap(), 0);
            assert!(matches!(
                vs.validator_lifecycle(accused).unwrap(),
                ValidatorLifecycle::Active(_)
            ));
            let mut invalid_a = valid_a;
            invalid_a[48..144].fill(0);
            assert!(matches!(
                check(caller, &invalid_a, &valid_b),
                Err(Rejected(SlashIndicatorError::InvalidBlsSignature))
            ));
            // Epoch 99 is intentionally absent: both markers must win over its lookup.
            let a = test_signing::signed_evidence(
                &sk,
                &pk,
                &ns,
                &test_signing::proposal(99, 5, 4, [1; 32]),
                test_signing::POP_DST,
            );
            let b = test_signing::signed_evidence(
                &sk,
                &pk,
                &ns,
                &test_signing::proposal(99, 5, 4, [2; 32]),
                test_signing::POP_DST,
            );
            assert!(matches!(
                check(caller, &a, &b),
                Err(Rejected(
                    SlashIndicatorError::MissingCommitteeSnapshotForEpoch { epoch: 99 }
                ))
            ));
            let si = SlashIndicator::new(storage.clone());
            assert!(!si
                .is_evidence_processed(canonical_evidence_hash(&a, &b))
                .unwrap());
            assert!(!si
                .is_evidence_processed(equivocation_epoch_felony_key(accused, 99))
                .unwrap());
            si.evidence_processed
                .write(&equivocation_epoch_felony_key(accused, 99), true)
                .unwrap();
            assert!(matches!(
                check(caller, &a, &b),
                Err(ValidatorAlreadyPenalized(
                    SlashIndicatorError::ValidatorAlreadyPenalized { validator, epoch: 99 }
                )) if validator == accused
            ));
            si.evidence_processed
                .write(&canonical_evidence_hash(&a, &b), true)
                .unwrap();
            assert!(matches!(
                check(caller, &b, &a),
                Err(EvidenceAlreadyProcessed(_))
            ));
        });
    }
}
