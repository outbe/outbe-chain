use super::*;

pub(super) fn borrowed(state: &UpgradeJournalStateV1) -> BorrowedWire<'_> {
    match state {
        UpgradeJournalStateV1::CandidatePrepared { context } => {
            UpgradeJournalWire::CandidatePrepared(PreparedWire { context })
        }
        UpgradeJournalStateV1::KeyProvisioned {
            context,
            sealed_root_hash,
        } => UpgradeJournalWire::KeyProvisioned(ProvisionedWire {
            context,
            sealed_root_hash: *sealed_root_hash,
        }),
        UpgradeJournalStateV1::CandidateKeyReady { context, security } => {
            UpgradeJournalWire::CandidateKeyReady(ready(context, security))
        }
        UpgradeJournalStateV1::SubmissionPrepared {
            context,
            security,
            submission,
        } => {
            UpgradeJournalWire::SubmissionPrepared(self::submission(context, security, submission))
        }
        UpgradeJournalStateV1::Submitted {
            context,
            security,
            submission,
            submitted_at_finalized_height,
            transaction_hashes,
        } => UpgradeJournalWire::Submitted(submitted(
            self::submission(context, security, submission),
            *submitted_at_finalized_height,
            transaction_hashes,
        )),
        UpgradeJournalStateV1::Finalized {
            context,
            security,
            submission,
            finalized_height,
            finalized_hash,
        } => UpgradeJournalWire::Finalized(completed(
            self::submission(context, security, submission),
            *finalized_height,
            *finalized_hash,
        )),
        UpgradeJournalStateV1::Promoted {
            context,
            security,
            submission,
            finalized_height,
            finalized_hash,
        } => UpgradeJournalWire::Promoted(completed(
            self::submission(context, security, submission),
            *finalized_height,
            *finalized_hash,
        )),
        UpgradeJournalStateV1::TerminalMissedCutoff {
            context,
            finalized_height,
            activation_height,
        } => UpgradeJournalWire::TerminalMissedCutoff(TerminalWire {
            context,
            finalized_height: *finalized_height,
            activation_height: *activation_height,
        }),
    }
}

fn ready<'a>(
    context: &'a UpgradeContextV1,
    security: &UpgradeSecurityMaterialV1,
) -> ReadyWire<&'a UpgradeContextV1> {
    ReadyWire {
        context,
        sealed_root_hash: security.sealed_root_hash,
        resident_offer_public: security.resident_offer_public,
        proof_hash: security.proof_hash,
    }
}

fn submission<'a>(
    context: &'a UpgradeContextV1,
    security: &UpgradeSecurityMaterialV1,
    submission: &'a PreparedUpgradeSubmissionV1,
) -> SubmissionWire<&'a UpgradeContextV1, &'a PreparedUpgradeSubmissionV1> {
    SubmissionWire {
        context,
        sealed_root_hash: security.sealed_root_hash,
        resident_offer_public: security.resident_offer_public,
        proof_hash: security.proof_hash,
        submission,
    }
}

fn submitted<'a>(
    prepared: SubmissionWire<&'a UpgradeContextV1, &'a PreparedUpgradeSubmissionV1>,
    submitted_at_finalized_height: u64,
    transaction_hashes: &'a Vec<B256>,
) -> SubmittedWire<&'a UpgradeContextV1, &'a PreparedUpgradeSubmissionV1, &'a Vec<B256>> {
    SubmittedWire {
        context: prepared.context,
        sealed_root_hash: prepared.sealed_root_hash,
        resident_offer_public: prepared.resident_offer_public,
        proof_hash: prepared.proof_hash,
        submission: prepared.submission,
        submitted_at_finalized_height,
        transaction_hashes,
    }
}

fn completed<'a>(
    prepared: SubmissionWire<&'a UpgradeContextV1, &'a PreparedUpgradeSubmissionV1>,
    finalized_height: u64,
    finalized_hash: B256,
) -> CompletedWire<&'a UpgradeContextV1, &'a PreparedUpgradeSubmissionV1> {
    CompletedWire {
        context: prepared.context,
        sealed_root_hash: prepared.sealed_root_hash,
        resident_offer_public: prepared.resident_offer_public,
        proof_hash: prepared.proof_hash,
        submission: prepared.submission,
        finalized_height,
        finalized_hash,
    }
}
