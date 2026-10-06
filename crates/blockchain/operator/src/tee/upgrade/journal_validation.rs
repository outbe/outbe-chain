use super::*;

impl PreparedUpgradeSubmissionV1 {
    pub(super) fn validate(&self) -> Result<()> {
        if !self.has_commitments() || !self.has_bounded_variants() {
            eyre::bail!("upgrade submission is incomplete or exceeds its variant cap");
        }
        let first = &self.relay_variants[0];
        if first.relay != self.relay || first.calldata_hash != self.calldata_hash {
            eyre::bail!("upgrade submission relay binding mismatch");
        }
        for variant in &self.relay_variants {
            if !variant_identity_matches(variant, first, self.relay)
                || !variant_commitments_match(variant, self.calldata_hash)
            {
                eyre::bail!("upgrade submission contains a competing relay variant");
            }
        }
        Ok(())
    }

    fn has_commitments(&self) -> bool {
        !self.intent_hash.is_zero()
            && !self.evidence_hash.is_zero()
            && !self.calldata_hash.is_zero()
    }
    fn has_bounded_variants(&self) -> bool {
        !self.relay.is_zero()
            && !self.relay_variants.is_empty()
            && self.relay_variants.len() <= MAX_RELAY_VARIANTS
    }
}
fn variant_identity_matches(
    variant: &RawRelayTransactionV1,
    first: &RawRelayTransactionV1,
    relay: Address,
) -> bool {
    variant.relay == relay
        && variant.chain_id == first.chain_id
        && variant.account_nonce == first.account_nonce
        && variant.gas_limit == first.gas_limit
}
fn variant_commitments_match(variant: &RawRelayTransactionV1, calldata_hash: B256) -> bool {
    variant.calldata_hash == calldata_hash
        && keccak256(&variant.raw_transaction) == variant.transaction_hash
}

impl UpgradeJournalStateV1 {
    pub(super) fn validate(&self) -> Result<()> {
        self.context().validate()?;
        match self {
            Self::CandidatePrepared { .. } => Ok(()),
            Self::KeyProvisioned {
                sealed_root_hash, ..
            } => validate_root(*sealed_root_hash),
            Self::CandidateKeyReady { security, .. } => validate_key_ready(
                security.sealed_root_hash,
                security.resident_offer_public,
                security.proof_hash,
            ),
            Self::SubmissionPrepared { .. }
            | Self::Submitted { .. }
            | Self::Finalized { .. }
            | Self::Promoted { .. } => validate_submission_checkpoint(self),
            Self::TerminalMissedCutoff {
                finalized_height,
                activation_height,
                ..
            } => {
                if *activation_height == 0 || finalized_height < activation_height {
                    eyre::bail!("terminal cutoff checkpoint precedes policy activation");
                }
                Ok(())
            }
        }
    }
}

fn validate_submission_checkpoint(state: &UpgradeJournalStateV1) -> Result<()> {
    let (root, offer, proof, submission) = security_material(state)
        .ok_or_else(|| eyre::eyre!("submission checkpoint has no security material"))?;
    validate_key_ready(root, offer, proof)?;
    let submission =
        submission.ok_or_else(|| eyre::eyre!("submission checkpoint has no submission"))?;
    submission.validate()?;
    match state {
        UpgradeJournalStateV1::Submitted {
            transaction_hashes, ..
        } => {
            if transaction_hashes.is_empty()
                || transaction_hashes.len() > submission.relay_variants.len()
                || transaction_hashes
                    .iter()
                    .enumerate()
                    .any(|(index, hash)| submission.relay_variants[index].transaction_hash != *hash)
            {
                eyre::bail!("submitted upgrade transaction list is non-canonical");
            }
        }
        UpgradeJournalStateV1::Finalized { finalized_hash, .. }
        | UpgradeJournalStateV1::Promoted { finalized_hash, .. }
            if finalized_hash.is_zero() =>
        {
            eyre::bail!("finalized upgrade checkpoint has a zero block hash");
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn validate_checkpoint_transition(
    current: &UpgradeJournalStateV1,
    next: &UpgradeJournalStateV1,
) -> Result<()> {
    use UpgradeJournalStateV1::{
        CandidateKeyReady, CandidatePrepared, Finalized, KeyProvisioned, Promoted,
        SubmissionPrepared, Submitted, TerminalMissedCutoff,
    };
    if validate_expired_recovery(current, next)? {
        return Ok(());
    }
    let allowed = matches!(
        (current, next),
        (CandidatePrepared { .. }, KeyProvisioned { .. })
            | (CandidatePrepared { .. }, TerminalMissedCutoff { .. })
            | (KeyProvisioned { .. }, CandidateKeyReady { .. })
            | (CandidateKeyReady { .. }, SubmissionPrepared { .. })
            | (SubmissionPrepared { .. }, Submitted { .. })
            | (Submitted { .. }, Submitted { .. })
            | (Submitted { .. }, Finalized { .. })
            | (Finalized { .. }, Promoted { .. })
            | (Promoted { .. }, Promoted { .. })
            | (TerminalMissedCutoff { .. }, TerminalMissedCutoff { .. })
            | (KeyProvisioned { .. }, TerminalMissedCutoff { .. })
            | (CandidateKeyReady { .. }, TerminalMissedCutoff { .. })
            | (SubmissionPrepared { .. }, TerminalMissedCutoff { .. })
            | (Submitted { .. }, TerminalMissedCutoff { .. })
    );
    if !allowed {
        eyre::bail!(
            "invalid upgrade checkpoint transition {} -> {}",
            current.label(),
            next.label()
        );
    }
    if let (Some(current), Some(next)) = (security_material(current), security_material(next)) {
        if !security_material_preserved(current, next) {
            eyre::bail!("upgrade security material changed across checkpoints");
        }
    }
    Ok(())
}

fn validate_expired_recovery(
    current: &UpgradeJournalStateV1,
    next: &UpgradeJournalStateV1,
) -> Result<bool> {
    use UpgradeJournalStateV1::{CandidateKeyReady, KeyProvisioned, SubmissionPrepared, Submitted};
    if let (
        CandidateKeyReady {
            security: before, ..
        }
        | SubmissionPrepared {
            security: before, ..
        }
        | Submitted {
            security: before, ..
        },
        KeyProvisioned {
            sealed_root_hash: after,
            ..
        },
    ) = (current, next)
    {
        if before.sealed_root_hash == *after {
            return Ok(true);
        }
        eyre::bail!("expired submission recovery changed the sealed root");
    }
    Ok(false)
}
type SecurityMaterial<'a> = (B256, B256, B256, Option<&'a PreparedUpgradeSubmissionV1>);
fn security_material_preserved(current: SecurityMaterial<'_>, next: SecurityMaterial<'_>) -> bool {
    current.0 == next.0
        && hash_preserved(current.1, next.1)
        && hash_preserved(current.2, next.2)
        && current
            .3
            .is_none_or(|submission| Some(submission) == next.3)
}
fn hash_preserved(current: B256, next: B256) -> bool {
    current.is_zero() || current == next
}

pub(super) fn security_material(state: &UpgradeJournalStateV1) -> Option<SecurityMaterial<'_>> {
    match state {
        UpgradeJournalStateV1::CandidatePrepared { .. }
        | UpgradeJournalStateV1::TerminalMissedCutoff { .. } => None,
        UpgradeJournalStateV1::KeyProvisioned {
            sealed_root_hash, ..
        } => Some((*sealed_root_hash, B256::ZERO, B256::ZERO, None)),
        UpgradeJournalStateV1::CandidateKeyReady { security, .. } => Some((
            security.sealed_root_hash,
            security.resident_offer_public,
            security.proof_hash,
            None,
        )),
        UpgradeJournalStateV1::SubmissionPrepared {
            security,
            submission,
            ..
        }
        | UpgradeJournalStateV1::Submitted {
            security,
            submission,
            ..
        }
        | UpgradeJournalStateV1::Finalized {
            security,
            submission,
            ..
        }
        | UpgradeJournalStateV1::Promoted {
            security,
            submission,
            ..
        } => Some((
            security.sealed_root_hash,
            security.resident_offer_public,
            security.proof_hash,
            Some(submission),
        )),
    }
}

fn validate_root(sealed_root_hash: B256) -> Result<()> {
    if sealed_root_hash.is_zero() {
        eyre::bail!("upgrade checkpoint has a zero sealed-root hash");
    }
    Ok(())
}

pub(super) fn validate_key_ready(
    sealed_root_hash: B256,
    resident_offer_public: B256,
    proof_hash: B256,
) -> Result<()> {
    validate_root(sealed_root_hash)?;
    if resident_offer_public.is_zero() || proof_hash.is_zero() {
        eyre::bail!("key-ready checkpoint has a zero offer key or proof hash");
    }
    Ok(())
}
