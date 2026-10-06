use super::*;

pub(super) fn owned(state: OwnedWire) -> UpgradeJournalStateV1 {
    match state {
        UpgradeJournalWire::CandidatePrepared(wire) => UpgradeJournalStateV1::CandidatePrepared {
            context: wire.context,
        },
        UpgradeJournalWire::KeyProvisioned(wire) => UpgradeJournalStateV1::KeyProvisioned {
            context: wire.context,
            sealed_root_hash: wire.sealed_root_hash,
        },
        UpgradeJournalWire::CandidateKeyReady(wire) => UpgradeJournalStateV1::CandidateKeyReady {
            security: wire.security(),
            context: wire.context,
        },
        UpgradeJournalWire::SubmissionPrepared(wire) => UpgradeJournalStateV1::SubmissionPrepared {
            security: wire.security(),
            context: wire.context,
            submission: wire.submission,
        },
        UpgradeJournalWire::Submitted(wire) => UpgradeJournalStateV1::Submitted {
            security: wire.security(),
            context: wire.context,
            submission: wire.submission,
            submitted_at_finalized_height: wire.submitted_at_finalized_height,
            transaction_hashes: wire.transaction_hashes,
        },
        UpgradeJournalWire::Finalized(wire) => UpgradeJournalStateV1::Finalized {
            security: wire.security(),
            context: wire.context,
            submission: wire.submission,
            finalized_height: wire.finalized_height,
            finalized_hash: wire.finalized_hash,
        },
        UpgradeJournalWire::Promoted(wire) => UpgradeJournalStateV1::Promoted {
            security: wire.security(),
            context: wire.context,
            submission: wire.submission,
            finalized_height: wire.finalized_height,
            finalized_hash: wire.finalized_hash,
        },
        UpgradeJournalWire::TerminalMissedCutoff(wire) => {
            UpgradeJournalStateV1::TerminalMissedCutoff {
                context: wire.context,
                finalized_height: wire.finalized_height,
                activation_height: wire.activation_height,
            }
        }
    }
}
