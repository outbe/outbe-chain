//! Append-only JSONL journal for SlashIndicator/ValidatorSet critical events.
//!
//! The journal is a process-local sidecar to the standard reth log. The runtime
//! writes one JSON record for each critical state-transition event to a single
//! file path (`<datadir>/slashing-journal.jsonl`). The application **never
//! rotates** the file. Operators may snapshot or trim the file manually. The
//! runtime never truncates it.
//!
//! ## Why
//!
//! Standard `reth.log` rotates when each file reaches the configured size
//! (~200 MB). At testnet finalization rates, this can mean that only a few
//! hours of runtime fit in the ~5-file rotation window. Slash and
//! validator-exit events are then routinely lost from on-host evidence before
//! anyone investigates. The journal closes that gap with a single tiny file
//! that survives indefinitely. The file holds one JSON line per event, with
//! ~10 events/day typical.
//!
//! ## Best-effort semantics
//!
//! The journal is **best-effort** observability. [`record`] serializes the
//! record and pushes it into a bounded in-memory queue. It does no file I/O on
//! the caller's thread. A dedicated writer thread appends the queued lines and
//! flushes once each time the queue becomes empty. When the queue is full, the
//! journal drops the oldest record and logs a warning. When a write fails
//! (disk full, permission error, file unwritable), the writer thread emits a
//! `tracing::warn!` and drops the write. A record queued just before the
//! process exits can be lost. The journal never blocks the consensus /
//! state-transition path that produced it. Determinism is unaffected because
//! the journal is off-chain. Its content is not identical on every node: each
//! record carries the local wall-clock time, and only a node that called
//! [`init`] writes records. The absence of the journal does not change the
//! on-chain state.
//!
//! ## Initialization
//!
//! The node calls [`init`] once at startup with the data directory. If
//! nothing calls [`init`] (e.g. tests), [`record`] silently no-ops.

use crate::journal_writer::Journal;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Filename of the journal inside the configured data directory.
pub const JOURNAL_FILENAME: &str = "slashing-journal.jsonl";

/// One record in the slashing/validator-set journal.
///
/// `tag = "event"` lets `serde_json` emit a flat object with the variant
/// name in the `event` field, which is convenient for ad-hoc grep / `jq`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum JournalRecord {
    /// `slash_proposer` recorded a miss for `validator`. The per-epoch counter
    /// reaches `count`. The record includes threshold reference values for
    /// context.
    ProposerMiss {
        wall_clock: String,
        block_number: u64,
        validator: String,
        count: u64,
        felony_threshold: u64,
        misdemeanor_threshold: u64,
    },

    /// `slash_proposer` reached `count % felony_threshold == 0`. Thus the
    /// event repeats at every multiple of the threshold. The runtime jails the
    /// validator (JAILED) and slashes `slash_percent` of its stake.
    ProposerFelony {
        wall_clock: String,
        block_number: u64,
        validator: String,
        miss_count: u64,
        felony_threshold: u64,
        felony_count: u64,
        slash_percent: u64,
    },

    /// Misdemeanor (informational, no slash).
    ProposerMisdemeanor {
        wall_clock: String,
        block_number: u64,
        validator: String,
        miss_count: u64,
        misdemeanor_threshold: u64,
    },

    /// `slash_voter` recorded a miss.
    VoterMiss {
        wall_clock: String,
        block_number: u64,
        validator: String,
        count: u64,
        misdemeanor_threshold: u64,
    },

    /// Voter misdemeanor.
    VoterMisdemeanor {
        wall_clock: String,
        block_number: u64,
        validator: String,
        miss_count: u64,
        misdemeanor_threshold: u64,
    },

    /// `slash_voter` reached `count % felony_threshold == 0`. The runtime
    /// jails the validator (JAILED) and slashes `slash_percent` of its stake.
    VoterFelony {
        wall_clock: String,
        block_number: u64,
        validator: String,
        miss_count: u64,
        felony_threshold: u64,
        felony_count: u64,
        slash_percent: u64,
    },

    /// Felony triggered by external evidence submission.
    EvidenceFelony {
        wall_clock: String,
        block_number: u64,
        validator: String,
        evidence_submitter: String,
        felony_count: u64,
        slash_percent: u64,
        slashed_amount: String,
    },

    /// `submitInvalidVrfProofEvidence` accepted evidence and
    /// applied the felony to the child block's Phase 1 tx signer. This record
    /// is emitted alongside the standard `EvidenceFelony` record. It gives
    /// operators the VRF-specific failure class and child-block context
    /// without reverse-engineering the call site.
    InvalidVrfProofEvidence {
        wall_clock: String,
        block_number: u64,
        proposer: String,
        evidence_submitter: String,
        child_block_hash: String,
        child_block_number: u64,
        child_epoch: u64,
        failure_class: u16,
    },

    /// Validator self-deactivated (or owner deactivated): ACTIVE->EXITING.
    ValidatorDeactivated {
        wall_clock: String,
        block_number: u64,
        validator: String,
        caller: String,
        self_initiated: bool,
    },

    /// Validator force-exited from a status path: ACTIVE->EXITING (or
    /// EXITING re-emit, or no-op for UNBONDING/INACTIVE).
    ValidatorForcedExit {
        wall_clock: String,
        block_number: u64,
        validator: String,
        status_before: String,
        status_after: String,
    },

    /// New validator registered (first time).
    ValidatorRegistered {
        wall_clock: String,
        block_number: u64,
        validator: String,
        index: u64,
    },

    /// Existing validator re-registered after an INACTIVE period.
    ValidatorReregistered {
        wall_clock: String,
        block_number: u64,
        validator: String,
        index: u64,
    },

    /// DKG reshare activated. The new active set is committed.
    ResharedSetActivated {
        wall_clock: String,
        block_number: u64,
        active_count: u32,
        transitioned_to_unbonding: u64,
        pending_set_change: bool,
        active_set_hash: String,
    },

    /// EXITING validator transitioned to UNBONDING during reshare.
    ValidatorUnbonding {
        wall_clock: String,
        block_number: u64,
        validator: String,
    },

    /// `reset_epoch_counters` cleared per-epoch miss counters.
    EpochCountersReset {
        wall_clock: String,
        block_number: u64,
        validator_count: usize,
    },
}

static JOURNAL: OnceLock<Journal> = OnceLock::new();

/// Initialize the journal. Call it once at node startup, before any
/// state-transition path runs. Subsequent calls are no-ops.
///
/// This function creates `datadir` if it is missing. It opens the journal
/// file in append mode, so existing content stays across node restarts.
pub fn init(datadir: &Path) -> std::io::Result<()> {
    if JOURNAL.get().is_some() {
        return Ok(());
    }

    let path = journal_path(datadir);
    let _ = JOURNAL.set(Journal::open(&path, "slashing")?);
    tracing::info!(
        target: "outbe::slashing::journal",
        path = %path.display(),
        "slashing journal initialized",
    );
    Ok(())
}

/// Returns the journal path under `datadir`.
pub fn journal_path(datadir: &Path) -> PathBuf {
    datadir.join(JOURNAL_FILENAME)
}

/// Queue `record` for the journal writer thread. If nothing called [`init`],
/// this is a no-op (test-friendly). This function does no file I/O. The
/// writer thread logs write errors at WARN and swallows them. It never blocks
/// the caller's state-transition path.
pub fn record(record: JournalRecord) {
    let Some(journal) = JOURNAL.get() else {
        return;
    };

    match serde_json::to_string(&record) {
        Ok(line) => journal.submit(line),
        Err(e) => {
            tracing::warn!(
                target: "outbe::slashing::journal",
                error = %e,
                "failed to serialize journal record",
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::iso8601_now;
    use std::io::Read;

    #[test]
    fn record_when_uninit_is_noop() {
        // No init -> record is a no-op (does not panic).
        record(JournalRecord::EpochCountersReset {
            wall_clock: "2026-01-01T00:00:00.000Z".into(),
            block_number: 0,
            validator_count: 0,
        });
    }

    #[test]
    fn record_serializes_to_jsonl_with_event_tag() {
        let rec = JournalRecord::ProposerFelony {
            wall_clock: "2026-05-10T12:34:56.789Z".into(),
            block_number: 12_345,
            validator: "0x2cf6fbd0".into(),
            miss_count: 150,
            felony_threshold: 150,
            felony_count: 1,
            slash_percent: 5,
        };
        let json = serde_json::to_string(&rec).expect("serializable");
        assert!(json.contains("\"event\":\"proposer_felony\""));
        assert!(json.contains("\"felony_count\":1"));
        assert!(json.contains("\"validator\":\"0x2cf6fbd0\""));
    }

    #[test]
    fn init_then_record_writes_lines_and_survives_reopen() {
        const CHILD_DIR_ENV: &str = "OUTBE_SLASHING_JOURNAL_CHILD_DIR";
        const CHILD_BLOCK_ENV: &str = "OUTBE_SLASHING_JOURNAL_CHILD_BLOCK";

        if let Some(dir) = std::env::var_os(CHILD_DIR_ENV) {
            let block_number: u64 = std::env::var(CHILD_BLOCK_ENV)
                .expect("child block env")
                .parse()
                .expect("valid block number");
            let dir = std::path::PathBuf::from(dir);
            init(&dir).expect("init slashing journal in child");
            record(JournalRecord::ValidatorUnbonding {
                wall_clock: iso8601_now(),
                block_number,
                validator: "0x2cf6fbd0".into(),
            });
            JOURNAL.get().expect("initialized journal").wait_idle();
            return;
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let nested = dir.path().join("nested");
        let path = journal_path(&nested);
        assert_eq!(path, nested.join("slashing-journal.jsonl"));

        for block_number in ["19200", "19201"] {
            crate::journal_writer::append_record_in_reopened_child(
                "slashing_journal::tests::init_then_record_writes_lines_and_survives_reopen",
                CHILD_DIR_ENV,
                CHILD_BLOCK_ENV,
                &nested,
                block_number,
            );
        }

        let mut content = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "expected old + new JSONL records");
        assert!(lines[0].contains("\"event\":\"validator_unbonding\""));
        assert!(lines[0].contains("\"block_number\":19200"));
        assert!(lines[1].contains("\"event\":\"validator_unbonding\""));
        assert!(lines[1].contains("\"block_number\":19201"));
    }
}
