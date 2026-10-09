//! VRF evidence: an invalid threshold VRF proof in a Phase 1 transaction, and
//! equivocating or invalid VRF seed partials.

use alloy_primitives::{keccak256, Address, B256};
use outbe_consensus::proof::{
    invalid_vrf_evidence_hash_v2, verify_seed_partial_against_commitment,
    verify_seed_partial_attest_bytes, verify_v2_proof, CommitteeSnapshot, SeedPartialAttestation,
};
use outbe_primitives::consensus_metadata::CertifiedParentAccountingMetadata;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::protocol_schedule::OutbeProtocolSchedule;
use outbe_primitives::slashing_journal::{iso8601_now, record as journal_record, JournalRecord};
use outbe_primitives::system_tx::{recover_phase1_proposer, SystemTxInputV2};
use outbe_validatorset::contract::ValidatorSet;
use outbe_validatorset::state::{committee_snapshot_key, read_committee_snapshot};
use tracing::warn;

use crate::precompile::ISlashIndicator;
use crate::runtime::{classify_vrf_failure, registered_validator};
use crate::schema::SlashIndicator;
use crate::seed_partial_evidence::{InvalidSeedPartialEvidence, SeedPartialEquivocationEvidence};
use crate::vrf_evidence::InvalidVrfProofEvidence;

impl SlashIndicator<'_> {
    /// Submit evidence that the Phase 1 system transaction in a
    /// child block carried an invalid threshold VRF proof.
    ///
    /// `evidence` is the wire form of [`InvalidVrfProofEvidence`] (see
    /// `vrf_evidence.rs`). The runtime applies, in order:
    ///
    /// 1. **Submitter ACL**: `caller` must be a currently-`ACTIVE` validator
    ///    in `ValidatorSet`. Reading + verifying VRF/BLS proofs is heavy
    ///    cryptographic work. Gating the entry-point to active validators
    ///    keeps DoS exposure inside the staked set (a malicious validator
    ///    pays gas AND has slashable stake at risk). The gate does not permit
    ///    arbitrary EOAs to spam the chain.
    /// 2. Size cap (`invalid_vrf_evidence_max_bytes`).
    /// 3. Block-age cap (`invalid_vrf_evidence_max_age_blocks`).
    /// 4. Epoch-lag cap (`invalid_vrf_evidence_max_epoch_lag`). The runtime
    ///    reads the epoch from on-chain state via [`ValidatorSet::epoch_number`]
    ///    (BP-0 option C: epoch is consensus state, not a derived value).
    /// 5. Child and parent canonicity: claimed child and parent
    ///    hashes must match the canonical chain.
    /// 6. Dedup via
    ///    [`outbe_consensus::proof::invalid_vrf_evidence_hash_v2`] keyed by
    ///    `(child_block_hash, keccak256(phase1_tx_bytes))`. A replay of the same
    ///    evidence reverts with `"evidence already processed"`. This matches the
    ///    `submitDoubleProposalEvidence` / `submitConflictingVoteEvidence`
    ///    precedent.
    /// 7. Cryptographic proposer attribution (D-2): validate
    ///    `phase1_tx_bytes` as the canonical Phase 1 tx for the child block,
    ///    recover the signer, and decode metadata from its calldata. The
    ///    signed tx is the only source of truth for metadata/proof bytes.
    /// 8. Look up the committee snapshot for `metadata.finalized_epoch +
    ///    committee_set_hash` via the canonical
    ///    [`outbe_validatorset::state::committee_snapshot_key`] +
    ///    [`outbe_validatorset::state::read_committee_snapshot`] path. Reject
    ///    if the snapshot is absent OR if the recovered proposer is not in
    ///    the committee.
    /// 9. Re-run [`outbe_consensus::proof::verify_v2_proof`] against the
    ///    same metadata/snapshot/parent_hash that the child block used. Accept
    ///    only VRF-class failures from `V2VerifyError`. Any `Ok` or
    ///    non-VRF rejection reverts. The precompile is strictly for VRF
    ///    misbehavior, not for re-litigating BLS quorum or accounting
    ///    binding failures.
    /// 10. Mark dedup BEFORE applying effects, then call
    ///     `apply_evidence_felony` for jail + 5% slash +
    ///     10% submitter reward. This reuses the existing felony helper. The
    ///     other evidence types share the same economics.
    pub fn submit_invalid_vrf_evidence(
        &mut self,
        caller: Address,
        evidence_bytes: &[u8],
    ) -> Result<()> {
        self.submit_invalid_vrf_evidence_with_schedule(
            caller,
            evidence_bytes,
            &OutbeProtocolSchedule::default(),
        )
    }

    /// Test seam for [`Self::submit_invalid_vrf_evidence`].
    ///
    /// Production callers use the no-arg `submit_invalid_vrf_evidence`. It
    /// always passes [`OutbeProtocolSchedule::default`]. That is the canonical
    /// V2 schedule and the only schedule that the precompile dispatcher ever
    /// uses. This `_with_schedule` variant exists so integration tests can relax
    /// admissibility caps (max_age, max_epoch_lag, max_bytes). Then a test can
    /// stress a single axis without hitting the other caps.
    #[doc(hidden)]
    pub fn submit_invalid_vrf_evidence_with_schedule(
        &mut self,
        caller: Address,
        evidence_bytes: &[u8],
        schedule: &OutbeProtocolSchedule,
    ) -> Result<()> {
        // (1) Submitter ACL. Only currently-ACTIVE validators can submit.
        // Rationale: the verifier path is heavy cryptography (BLS + VRF +
        // ecrecover + storage reads). With the entry-point restricted to the
        // staked set, any griefer pays gas AND has slashable stake at risk.
        // Thus DoS becomes self-destructive rather than free.
        self.require_active_submitter(caller)?;

        // (2) Size cap. Bound the work the precompile body does on
        // attacker-controlled input.
        require_evidence_size(evidence_bytes, schedule)?;

        // (3) Decode the wire form.
        let ev = InvalidVrfProofEvidence::decode(evidence_bytes)?;

        // (4) Block-age and (5) epoch-lag admissibility.
        self.require_recent_vrf_evidence(&ev, schedule)?;

        // (6) Child + parent canonicity.
        self.require_canonical_vrf_blocks(&ev)?;

        // (7) Dedup key. A child block has exactly one Phase 1 system
        // transaction, so (child_hash, phase1_tx_hash) uniquely
        // identifies one invalid-VRF event.
        let phase1_tx_hash = keccak256(&ev.phase1_tx_bytes);
        let evidence_hash = invalid_vrf_evidence_hash_v2(ev.child_block_hash, phase1_tx_hash);
        if self.invalid_vrf_evidence_processed.read(&evidence_hash)? {
            return Err(PrecompileError::Revert("evidence already processed".into()));
        }

        // (8) Cryptographic proposer attribution.
        let (proposer, metadata) = self.attribute_phase1_proposer(&ev)?;

        // (9) + (10) Committee snapshot of the child's epoch, which must
        // contain the proposer.
        let snapshot = self.proposer_committee_snapshot(&metadata, proposer)?;

        // (11) Re-verify the proof. Only a VRF-class failure is slashable.
        let failure_class = vrf_failure_class(&metadata, &snapshot, ev.parent_block_hash)?;

        // (12) Mark dedup BEFORE applying effects so a panic / abort
        // between this point and the felony cannot enable replay.
        self.invalid_vrf_evidence_processed
            .write(&evidence_hash, true)?;

        // (13) Apply felony: jail + 5% slash + 10% submitter
        // reward. This uses the same helper that the other evidence types call.
        self.apply_evidence_felony(proposer, caller)?;

        // (14) Canonical event with re-derived failure class.
        self.emit(ISlashIndicator::InvalidVrfProofEvidenceApplied {
            proposer,
            submitter: caller,
            childBlockHash: ev.child_block_hash,
            failureCode: failure_class,
        })?;

        // (15) Journal + structured warn! for operator visibility.
        self.record_invalid_vrf_evidence(&ev, proposer, caller, failure_class);
        Ok(())
    }

    /// Rejects evidence whose child block is older than the age cap, or
    /// whose child epoch is older than the epoch-lag cap.
    fn require_recent_vrf_evidence(
        &self,
        ev: &InvalidVrfProofEvidence,
        schedule: &OutbeProtocolSchedule,
    ) -> Result<()> {
        let current_block = self.storage.block_number().unwrap_or(0);
        let max_acceptable_block = ev
            .child_block_number
            .saturating_add(schedule.invalid_vrf_evidence_max_age_blocks);
        if current_block > max_acceptable_block {
            return Err(PrecompileError::Revert(format!(
                "evidence stale: current_block {} > child_block {} + max_age {}",
                current_block, ev.child_block_number, schedule.invalid_vrf_evidence_max_age_blocks,
            )));
        }

        // Epoch-lag admissibility. Read the canonical on-chain epoch counter
        // from ValidatorSet. Option C: epoch is consensus state, which
        // update_epoch records at boundaries. We do NOT re-derive it from
        // block height.
        let vs = ValidatorSet::new(self.storage.clone());
        require_epoch_within_lag(
            vs.current_epoch_u64()?,
            "child_epoch",
            ev.child_epoch,
            schedule,
        )
    }

    /// Requires that the claimed child and parent blocks are canonical and
    /// adjacent.
    fn require_canonical_vrf_blocks(&self, ev: &InvalidVrfProofEvidence) -> Result<()> {
        self.require_canonical_block("child", ev.child_block_number, ev.child_block_hash)?;
        if ev.parent_block_number.saturating_add(1) != ev.child_block_number {
            return Err(PrecompileError::Revert(format!(
                "evidence parent/child number mismatch: parent={} child={}",
                ev.parent_block_number, ev.child_block_number,
            )));
        }
        self.require_canonical_block("parent", ev.parent_block_number, ev.parent_block_hash)
    }

    /// Requires that `hash` is the canonical hash at `number`. `role` names
    /// the block in the revert reason.
    fn require_canonical_block(&self, role: &str, number: u64, hash: B256) -> Result<()> {
        let canonical = self.storage.canonical_block_hash(number)?.ok_or_else(|| {
            PrecompileError::Revert(format!(
                "evidence {role} block {number} not in canonical-history window",
            ))
        })?;
        if canonical != hash {
            return Err(PrecompileError::Revert(format!(
                "evidence {role} hash {hash} is not canonical at block {number} (canonical: {canonical})",
            )));
        }
        Ok(())
    }

    /// The child block's proposer signs the Phase 1 tx. Its calldata is the
    /// single source of truth for metadata and proof bytes. The metadata must
    /// bind to the evidence parent block and child epoch.
    fn attribute_phase1_proposer(
        &self,
        ev: &InvalidVrfProofEvidence,
    ) -> Result<(Address, CertifiedParentAccountingMetadata)> {
        let chain_id = self.storage.chain_id()?;
        let (proposer, calldata) =
            recover_phase1_proposer(&ev.phase1_tx_bytes, chain_id, ev.child_block_number)
                .map_err(|err| PrecompileError::Revert(format!("phase1_tx invalid: {err}")))?;
        let metadata = match SystemTxInputV2::decode(calldata.as_ref()).map_err(|err| {
            PrecompileError::Revert(format!("phase1 calldata decode failed: {err}"))
        })? {
            SystemTxInputV2::CertifiedParentAccounting { metadata } => metadata,
            other => {
                return Err(PrecompileError::Revert(format!(
                    "phase1 calldata is not CertifiedParentAccounting: {:?}",
                    other.kind(),
                )));
            }
        };
        require_metadata_binding(ev, &metadata)?;
        Ok((proposer, metadata))
    }

    /// Loads the canonical committee snapshot for the child's epoch and
    /// requires the proposer in its committee.
    ///
    /// The membership check defends against a future bug that signs a
    /// Phase 1 tx with a key not bound to any active validator. Without this
    /// check, the felony helper would call jail_validator on a non-existent
    /// validator, and the slash path would silently no-op.
    fn proposer_committee_snapshot(
        &self,
        metadata: &CertifiedParentAccountingMetadata,
        proposer: Address,
    ) -> Result<CommitteeSnapshot> {
        let snapshot_key =
            committee_snapshot_key(metadata.finalized_epoch, metadata.committee_set_hash);
        let snapshot =
            read_committee_snapshot(self.storage.clone(), snapshot_key)?.ok_or_else(|| {
                PrecompileError::Revert(format!(
                    "no committee snapshot for finalized_epoch={} committee_set_hash={}",
                    metadata.finalized_epoch, metadata.committee_set_hash,
                ))
            })?;
        if !snapshot
            .committee
            .iter()
            .any(|entry| entry.address == proposer)
        {
            return Err(PrecompileError::Revert(format!(
                "phase1_tx proposer {proposer} not in committee for epoch {}",
                metadata.finalized_epoch,
            )));
        }
        Ok(snapshot)
    }

    /// Journal + structured warn! for an accepted invalid-VRF evidence.
    fn record_invalid_vrf_evidence(
        &self,
        ev: &InvalidVrfProofEvidence,
        proposer: Address,
        caller: Address,
        failure_class: u16,
    ) {
        let block_number = self.storage.block_number().unwrap_or(0);
        journal_record(JournalRecord::InvalidVrfProofEvidence {
            wall_clock: iso8601_now(),
            block_number,
            proposer: format!("{proposer:?}"),
            evidence_submitter: format!("{caller:?}"),
            child_block_hash: format!("{:?}", ev.child_block_hash),
            child_block_number: ev.child_block_number,
            child_epoch: ev.child_epoch,
            failure_class,
        });
        warn!(
            target: "outbe::slashing",
            event = "invalid_vrf_proof_evidence",
            %proposer,
            %caller,
            child_block_hash = %ev.child_block_hash,
            child_block_number = ev.child_block_number,
            child_epoch = ev.child_epoch,
            failure_class,
            block_number,
            "invalid-VRF evidence accepted - proposer self-incriminated by Phase 1 tx signature",
        );
    }

    /// Submit evidence that a validator equivocated on its VRF seed partial:
    /// two DIFFERENT identity-signed `bls_seed_partial`s for the same
    /// `(round, vrf_material_version)`. The evidence self-authenticates from the
    /// two MinPk identity signatures. No committee polynomial is needed. The
    /// method reuses the shared felony economics. An honest validator produces
    /// exactly one partial per round/version. It never identity-signs a second
    /// distinct one. Thus a valid pair cannot frame an honest node.
    pub fn submit_seed_partial_equivocation_evidence(
        &mut self,
        caller: Address,
        evidence_bytes: &[u8],
    ) -> Result<()> {
        self.submit_seed_partial_equivocation_evidence_with_schedule(
            caller,
            evidence_bytes,
            &OutbeProtocolSchedule::default(),
        )
    }

    /// Test seam for [`Self::submit_seed_partial_equivocation_evidence`] (lets
    /// integration tests relax the epoch-lag cap). Production uses the no-arg
    /// wrapper with the canonical schedule.
    #[doc(hidden)]
    pub fn submit_seed_partial_equivocation_evidence_with_schedule(
        &mut self,
        caller: Address,
        evidence_bytes: &[u8],
        schedule: &OutbeProtocolSchedule,
    ) -> Result<()> {
        // (1) Submitter ACL: ACTIVE validators only. Verification is BLS-heavy,
        // so gating to the staked set makes DoS self-destructive.
        self.require_active_submitter(caller)?;

        // (2) Decode the fixed-length wire form (length-checked inside).
        let ev = SeedPartialEquivocationEvidence::decode(evidence_bytes)?;

        // (3) Equivocation requires two DIFFERENT partials.
        if ev.partial_1 == ev.partial_2 {
            return Err(PrecompileError::Revert(
                "not equivocation: the two partials are identical".into(),
            ));
        }

        // (4) Both partials must carry a valid identity signature from the SAME
        // signer over the same (round, version). This is the soundness anchor:
        // it proves the accused signer itself produced both distinct partials.
        let ok1 = identity_signed(&ev, &ev.partial_1, &ev.identity_sig_1);
        let ok2 = identity_signed(&ev, &ev.partial_2, &ev.identity_sig_2);
        if !(ok1 && ok2) {
            return Err(PrecompileError::Revert(
                "both partials must carry a valid identity signature from the accused signer"
                    .into(),
            ));
        }

        // (5) Epoch-lag admissibility: bound how old the offense round can be
        // (reuses the shared evidence epoch-lag cap).
        // (6) Attribution: map the identity pubkey to a registered validator.
        let validator_addr =
            self.recent_partial_signer(ev.round_epoch, || ev.pubkey_hash(), schedule)?;

        // (7) Dedup BEFORE effects (order-independent in the two partials).
        let dedup = ev.dedup_hash();
        if self.seed_partial_equivocation_processed.read(&dedup)? {
            return Err(PrecompileError::Revert("evidence already processed".into()));
        }
        self.seed_partial_equivocation_processed
            .write(&dedup, true)?;

        // (8) Felony: jail + slash + reward submitter.
        self.apply_evidence_felony(validator_addr, caller)?;
        self.emit(ISlashIndicator::SeedPartialEquivocationApplied {
            validator: validator_addr,
            submitter: caller,
            roundEpoch: ev.round_epoch,
            roundView: ev.round_view,
            vrfVersion: ev.vrf_version,
        })?;
        Ok(())
    }

    /// Submit evidence that a validator emitted a single INVALID VRF seed
    /// partial: an identity-signed partial that fails verification against the
    /// committee's full public polynomial. Unlike equivocation, this needs the
    /// committee polynomial. The evidence carries the polynomial. This method
    /// checks it against the `vrf_public_polynomial_hash` committed in the
    /// committee snapshot. The executor derives that hash from the
    /// consensus-validated DKG boundary outcome. Thus a proposer cannot forge it
    /// to frame an honest validator. The method reuses the shared felony
    /// economics.
    pub fn submit_invalid_seed_partial_evidence(
        &mut self,
        caller: Address,
        evidence_bytes: &[u8],
    ) -> Result<()> {
        self.submit_invalid_seed_partial_evidence_with_schedule(
            caller,
            evidence_bytes,
            &OutbeProtocolSchedule::default(),
        )
    }

    /// Test seam for [`Self::submit_invalid_seed_partial_evidence`].
    #[doc(hidden)]
    pub fn submit_invalid_seed_partial_evidence_with_schedule(
        &mut self,
        caller: Address,
        evidence_bytes: &[u8],
        schedule: &OutbeProtocolSchedule,
    ) -> Result<()> {
        // (1) Submitter ACL: ACTIVE validators only (BLS-heavy verification).
        self.require_active_submitter(caller)?;

        // (2) Size cap. The polynomial commitment dominates. Reuse the VRF
        // evidence cap (the only other commitment-carrying evidence).
        require_evidence_size(evidence_bytes, schedule)?;

        // (3) Decode.
        let ev = InvalidSeedPartialEvidence::decode(evidence_bytes)?;

        // (4) Epoch-lag admissibility and (5) attribution.
        let validator_addr =
            self.recent_partial_signer(ev.round_epoch, || ev.pubkey_hash(), schedule)?;

        // (6) Load the committee snapshot for this round's epoch + committee.
        let snapshot_key = committee_snapshot_key(ev.round_epoch, ev.committee_set_hash);
        let snapshot =
            read_committee_snapshot(self.storage.clone(), snapshot_key)?.ok_or_else(|| {
                PrecompileError::Revert(
                    "no committee snapshot for (round_epoch, committee_set_hash)".into(),
                )
            })?;

        // (7)-(9) Signer index, material version and polynomial commitment
        // must match the snapshot.
        require_snapshot_commitment(&ev, &snapshot)?;

        // (10) Identity signature and (11) failed verification against the
        // committee polynomial.
        require_invalid_signed_partial(&ev)?;

        // (12) Dedup before effects.
        let dedup = ev.dedup_hash();
        if self.invalid_seed_partial_processed.read(&dedup)? {
            return Err(PrecompileError::Revert("evidence already processed".into()));
        }
        self.invalid_seed_partial_processed.write(&dedup, true)?;

        // (13) Felony.
        self.apply_evidence_felony(validator_addr, caller)?;
        self.emit(ISlashIndicator::InvalidSeedPartialApplied {
            validator: validator_addr,
            submitter: caller,
            roundEpoch: ev.round_epoch,
            roundView: ev.round_view,
            vrfVersion: ev.vrf_version,
        })?;
        Ok(())
    }

    /// Applies the epoch-lag cap to a seed-partial round, then maps the
    /// identity pubkey to a registered validator.
    fn recent_partial_signer(
        &self,
        round_epoch: u64,
        pubkey_hash: impl FnOnce() -> B256,
        schedule: &OutbeProtocolSchedule,
    ) -> Result<Address> {
        let vs = ValidatorSet::new(self.storage.clone());
        require_epoch_within_lag(
            vs.current_epoch_u64()?,
            "round_epoch",
            round_epoch,
            schedule,
        )?;
        registered_validator(&vs, pubkey_hash())
    }
}

/// Bounds the work the precompile body does on attacker-controlled input.
fn require_evidence_size(evidence_bytes: &[u8], schedule: &OutbeProtocolSchedule) -> Result<()> {
    if evidence_bytes.len() > schedule.invalid_vrf_evidence_max_bytes {
        return Err(PrecompileError::Revert(format!(
            "evidence too large: {} > {} bytes",
            evidence_bytes.len(),
            schedule.invalid_vrf_evidence_max_bytes,
        )));
    }
    Ok(())
}

/// Rejects evidence whose epoch is older than the epoch-lag cap. `label`
/// names the evidence epoch in the revert reason.
fn require_epoch_within_lag(
    current_epoch: u64,
    label: &str,
    evidence_epoch: u64,
    schedule: &OutbeProtocolSchedule,
) -> Result<()> {
    let max_lag = schedule.invalid_vrf_evidence_max_epoch_lag;
    if current_epoch > evidence_epoch.saturating_add(max_lag) {
        return Err(PrecompileError::Revert(format!(
            "evidence epoch-stale: current_epoch {current_epoch} > {label} {evidence_epoch} + max_lag {max_lag}",
        )));
    }
    Ok(())
}

/// The Phase 1 metadata must finalize the evidence parent block in the
/// evidence child epoch.
fn require_metadata_binding(
    ev: &InvalidVrfProofEvidence,
    metadata: &CertifiedParentAccountingMetadata,
) -> Result<()> {
    if metadata.finalized_block_number != ev.parent_block_number
        || metadata.finalized_block_hash != ev.parent_block_hash
    {
        return Err(PrecompileError::Revert(format!(
            "metadata parent binding mismatch: evidence=({}, {}), metadata=({}, {})",
            ev.parent_block_number,
            ev.parent_block_hash,
            metadata.finalized_block_number,
            metadata.finalized_block_hash,
        )));
    }
    if metadata.finalized_epoch != ev.child_epoch {
        return Err(PrecompileError::Revert(format!(
            "metadata epoch {} does not match evidence child_epoch {}",
            metadata.finalized_epoch, ev.child_epoch,
        )));
    }
    Ok(())
}

/// Re-verifies the proof. We expect verify_v2_proof to REJECT with a
/// VRF-class error. Anything else is non-slashable here.
fn vrf_failure_class(
    metadata: &CertifiedParentAccountingMetadata,
    snapshot: &CommitteeSnapshot,
    parent_block_hash: B256,
) -> Result<u16> {
    let Err(verify_err) = verify_v2_proof(
        metadata,
        snapshot,
        metadata.proof.as_ref(),
        parent_block_hash,
    ) else {
        return Err(PrecompileError::Revert(
            "evidence shows a VALID proof; nothing to slash".into(),
        ));
    };
    classify_vrf_failure(&verify_err).ok_or_else(|| {
        PrecompileError::Revert(format!(
            "verify_v2_proof rejected with non-VRF class ({verify_err}); not slashable here",
        ))
    })
}

/// True when `identity_sig` is the accused signer's identity signature over
/// `partial` in the evidence round and material version.
fn identity_signed(
    ev: &SeedPartialEquivocationEvidence,
    partial: &[u8; 48],
    identity_sig: &[u8; 96],
) -> bool {
    verify_seed_partial_attest_bytes(
        &ev.signer_pubkey,
        SeedPartialAttestation {
            round_epoch: ev.round_epoch,
            round_view: ev.round_view,
            vrf_material_version: ev.vrf_version,
            partial_bytes: partial,
        },
        identity_sig,
    )
}

/// Binds the evidence to the committee snapshot:
/// - the signer index selects the committee entry with the evidence pubkey,
///   so PK_i is derived at the right index;
/// - the material version is the snapshot's (the carried polynomial is the
///   snapshot's polynomial);
/// - the snapshot carries a polynomial hash, and it matches the carried
///   commitment.
fn require_snapshot_commitment(
    ev: &InvalidSeedPartialEvidence,
    snapshot: &CommitteeSnapshot,
) -> Result<()> {
    let idx = ev.signer_index as usize;
    let entry = snapshot
        .committee
        .get(idx)
        .ok_or_else(|| PrecompileError::Revert("signer index out of committee range".into()))?;
    if entry.consensus_pubkey != ev.signer_pubkey {
        return Err(PrecompileError::Revert(
            "signer pubkey does not match committee entry at index".into(),
        ));
    }
    if snapshot.vrf_material_version != ev.vrf_version {
        return Err(PrecompileError::Revert(
            "vrf material version does not match committee snapshot".into(),
        ));
    }
    if snapshot.vrf_public_polynomial_hash.is_zero() {
        return Err(PrecompileError::Revert(
            "committee snapshot has no polynomial commitment".into(),
        ));
    }
    if keccak256(&ev.commitment) != snapshot.vrf_public_polynomial_hash {
        return Err(PrecompileError::Revert(
            "commitment does not match committee snapshot polynomial hash".into(),
        ));
    }
    Ok(())
}

/// The accused signer identity-signed THIS partial (a relay cannot forge
/// it), and the partial FAILS verification against the committee
/// polynomial. A valid partial is not slashable. Malformed input rejects.
fn require_invalid_signed_partial(ev: &InvalidSeedPartialEvidence) -> Result<()> {
    if !verify_seed_partial_attest_bytes(
        &ev.signer_pubkey,
        SeedPartialAttestation {
            round_epoch: ev.round_epoch,
            round_view: ev.round_view,
            vrf_material_version: ev.vrf_version,
            partial_bytes: &ev.partial,
        },
        &ev.identity_sig,
    ) {
        return Err(PrecompileError::Revert(
            "partial is not identity-signed by the accused signer".into(),
        ));
    }
    match verify_seed_partial_against_commitment(
        &ev.commitment,
        ev.signer_index,
        ev.round_epoch,
        ev.round_view,
        &ev.partial,
    ) {
        None => Err(PrecompileError::Revert(
            "malformed commitment or partial".into(),
        )),
        Some(true) => Err(PrecompileError::Revert(
            "seed partial is valid; nothing to slash".into(),
        )),
        Some(false) => Ok(()),
    }
}
