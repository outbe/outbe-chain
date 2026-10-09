//! Equivocation evidence: two consensus messages from one signer for one
//! round.

use alloy_primitives::{Address, B256};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_validatorset::contract::ValidatorSet;

use crate::evidence::{EvidenceBlock, EvidenceCommittee};
use crate::runtime::{canonical_evidence_hash, registered_validator};
use crate::schema::SlashIndicator;

/// Signature check of one evidence block against the epoch committee.
type VerifyBlock = fn(&EvidenceBlock, &EvidenceCommittee) -> Result<()>;

/// Two evidence blocks from the same signer, with their raw bytes for the
/// order-independent dedup hash.
struct EvidencePair<'a> {
    first: EvidenceBlock,
    second: EvidenceBlock,
    raw: (&'a [u8], &'a [u8]),
}

impl<'a> EvidencePair<'a> {
    /// Parses both blocks and requires one signer.
    fn parse(block1: &'a [u8], block2: &'a [u8]) -> Result<Self> {
        let first = EvidenceBlock::parse(block1)?;
        let second = EvidenceBlock::parse(block2)?;
        if first.pubkey != second.pubkey {
            return Err(PrecompileError::Revert(
                "evidence blocks must have the same signer".into(),
            ));
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
    fn same_round(&self, mismatch: &str) -> Result<(u64, u64)> {
        let round = self.first.round()?;
        if round != self.second.round()? {
            return Err(PrecompileError::Revert(mismatch.into()));
        }
        Ok(round)
    }

    /// Requires different proposal bytes. `same` is the revert reason.
    fn require_distinct_proposals(&self, same: &str) -> Result<()> {
        if self.first.proposal_bytes == self.second.proposal_bytes {
            return Err(PrecompileError::Revert(same.into()));
        }
        Ok(())
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

/// One same-signer equivocation class: the signature check of each block and
/// whether the two proposals must differ.
struct EquivocationClass {
    require_distinct_proposals: bool,
    verify_first: VerifyBlock,
    verify_second: VerifyBlock,
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
        self.require_active_submitter(caller)?;
        let pair = EvidencePair::parse(block1, block2)?;
        pair.require_distinct_proposals("proposals must differ for double-proposal evidence")?;
        let round = pair.same_round("proposals must be for the same round")?;
        let evidence_hash = pair.evidence_hash();
        self.require_unprocessed_evidence(evidence_hash)?;

        // Verify both signatures under the committee that ran the evidence's epoch
        // (notarize namespace is committee-bound).
        let committee = self.committee_set_for_epoch(round.0)?;
        pair.first.verify_notarize_signature(&committee)?;
        pair.second.verify_notarize_signature(&committee)?;

        self.penalize_evidence_signer(&pair, evidence_hash, caller)
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
        self.require_active_submitter(caller)?;
        let pair = EvidencePair::parse(vote1, vote2)?;
        let round = pair.same_round("votes must be for the same round")?;
        let evidence_hash = pair.evidence_hash();
        self.require_unprocessed_evidence(evidence_hash)?;

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
            return Err(PrecompileError::Revert(
                "evidence must contain one valid notarize and one valid nullify signature".into(),
            ));
        }

        self.penalize_evidence_signer(&pair, evidence_hash, caller)
    }

    /// Shared verifier for the three commonware same-signer equivocation classes
    /// (`ConflictingNotarize`, `ConflictingFinalize`, `NullifyFinalize`). Each is
    /// two `EvidenceBlock`s from the SAME signer for the SAME round. The class
    /// verifies each block's signature against the appropriate Simplex
    /// sub-namespace. Same-vote-type classes set `require_distinct_proposals`
    /// (two notarizes / two finalizes must differ). The nullify+finalize
    /// class differs by construction. Dedup reuses the `evidence_processed`
    /// guard (slot 8) keyed by the order-independent `canonical_evidence_hash`.
    fn apply_equivocation_felony(
        &mut self,
        caller: Address,
        block1: &[u8],
        block2: &[u8],
        class: EquivocationClass,
    ) -> Result<()> {
        self.require_active_submitter(caller)?;
        let pair = EvidencePair::parse(block1, block2)?;
        let round = pair.same_round("votes must be for the same round")?;
        if class.require_distinct_proposals {
            pair.require_distinct_proposals("conflicting votes must be for different proposals")?;
        }
        let evidence_hash = pair.evidence_hash();
        self.require_unprocessed_evidence(evidence_hash)?;

        // Vote namespaces are committee-bound. Verify under the committee
        // that ran the evidence's epoch.
        let committee = self.committee_set_for_epoch(round.0)?;
        (class.verify_first)(&pair.first, &committee)?;
        (class.verify_second)(&pair.second, &committee)?;

        self.penalize_evidence_signer(&pair, evidence_hash, caller)
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
            EquivocationClass {
                require_distinct_proposals: true,
                verify_first: EvidenceBlock::verify_notarize_signature,
                verify_second: EvidenceBlock::verify_notarize_signature,
            },
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
            EquivocationClass {
                require_distinct_proposals: true,
                verify_first: EvidenceBlock::verify_finalize_signature,
                verify_second: EvidenceBlock::verify_finalize_signature,
            },
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
            EquivocationClass {
                require_distinct_proposals: false,
                verify_first: EvidenceBlock::verify_nullify_signature,
                verify_second: EvidenceBlock::verify_finalize_signature,
            },
        )
    }

    /// Rejects an evidence hash that was already processed.
    fn require_unprocessed_evidence(&self, evidence_hash: B256) -> Result<()> {
        if self.evidence_processed.read(&evidence_hash)? {
            return Err(PrecompileError::Revert("evidence already processed".into()));
        }
        Ok(())
    }

    /// Looks up the signer, marks the evidence as processed before any
    /// effect, then applies the evidence felony.
    fn penalize_evidence_signer(
        &mut self,
        pair: &EvidencePair<'_>,
        evidence_hash: B256,
        caller: Address,
    ) -> Result<()> {
        let vs = ValidatorSet::new(self.storage.clone());
        let validator_addr = registered_validator(&vs, pair.first.pubkey_hash())?;
        self.evidence_processed.write(&evidence_hash, true)?;
        self.apply_evidence_felony(validator_addr, caller)
    }
}
