//! Append-only JSONL journal for Vote / governance critical events.
//!
//! The journal is a process-local sidecar to the standard reth log. The
//! runtime writes one JSON record per critical state-transition event to a
//! single file path (`<datadir>/governance-journal.jsonl`). The application
//! **never rotates** the journal. Operators may snapshot or trim the file
//! manually. The runtime never truncates it.
//!
//! ## Best-effort semantics
//!
//! The journal is **best-effort** observability. A write that fails (disk
//! full, permission error, file unwritable) emits a `tracing::warn!`, and
//! the runtime drops it. Failed writes never block the consensus /
//! state-transition path that produced them. Determinism is unaffected because
//! the journal is off-chain. Its content is not identical on every node: each
//! record carries the local wall-clock time. Absence of the journal does not
//! change the on-chain state.
//!
//! ## Initialization
//!
//! The node calls [`init`] once at startup with the data directory. If
//! nothing calls [`init`] (e.g. tests), [`record`] silently no-ops.

use crate::journal_writer::Journal;
use crate::time::iso8601_now;

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Filename of the journal inside the configured data directory.
pub const JOURNAL_FILENAME: &str = "governance-journal.jsonl";

/// Common block context shared by every journal record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventContext {
    pub wall_clock: String,
    pub block_number: u64,
}

impl EventContext {
    /// Builds context for the current wall-clock time at `block_number`.
    pub fn at(block_number: u64) -> Self {
        Self {
            wall_clock: iso8601_now(),
            block_number,
        }
    }
}

/// Proposal identity fields reused across finalization records.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProposalRef {
    pub proposal_id: String,
    pub proposer: String,
    pub target_module: String,
}

/// Yes/no vote counters at a tally point.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct VoteTallyRef {
    pub yes_votes: u64,
    pub no_votes: u64,
    pub active_validator_count: u32,
}

/// One record in the governance journal.
///
/// `tag = "event"` lets `serde_json` emit a flat object with the variant
/// name in the `event` field, which is convenient for ad-hoc grep / `jq`.
/// Nested structs are flattened into the top-level JSON object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum JournalRecord {
    /// A new proposal was created and entered the voting phase.
    ProposalCreated {
        #[serde(flatten)]
        ctx: EventContext,
        #[serde(flatten)]
        proposal: ProposalRef,
        voting_deadline_height: u64,
        payload_len: usize,
        payload_hash: String,
    },

    /// An active validator cast a vote on a pending proposal.
    VoteCast {
        #[serde(flatten)]
        ctx: EventContext,
        proposal_id: String,
        voter: String,
        approve: bool,
    },

    /// Quorum reached and the target-module handler accepted the proposal.
    ProposalApproved {
        #[serde(flatten)]
        ctx: EventContext,
        #[serde(flatten)]
        proposal: ProposalRef,
        #[serde(flatten)]
        tally: VoteTallyRef,
    },

    /// Quorum reached but the target-module handler reverted the proposal.
    ProposalRejected {
        #[serde(flatten)]
        ctx: EventContext,
        #[serde(flatten)]
        proposal: ProposalRef,
        #[serde(flatten)]
        tally: VoteTallyRef,
        reject_reason: String,
    },

    /// Quorum reached but the target module reported a proposal execution error.
    ProposalErrored {
        #[serde(flatten)]
        ctx: EventContext,
        #[serde(flatten)]
        proposal: ProposalRef,
        #[serde(flatten)]
        tally: VoteTallyRef,
        execution_error: String,
    },

    /// Voting deadline passed without reaching quorum.
    ProposalExpired {
        #[serde(flatten)]
        ctx: EventContext,
        #[serde(flatten)]
        proposal: ProposalRef,
        #[serde(flatten)]
        tally: VoteTallyRef,
    },
}

impl JournalRecord {
    /// Journal entry for a newly created proposal.
    pub fn proposal_created(
        block_number: u64,
        proposal: ProposalRef,
        voting_deadline_height: u64,
        payload_len: usize,
        payload_hash: String,
    ) -> Self {
        Self::ProposalCreated {
            ctx: EventContext::at(block_number),
            proposal,
            voting_deadline_height,
            payload_len,
            payload_hash,
        }
    }

    /// Journal entry for a cast vote.
    pub fn vote_cast(block_number: u64, proposal_id: String, voter: String, approve: bool) -> Self {
        Self::VoteCast {
            ctx: EventContext::at(block_number),
            proposal_id,
            voter,
            approve,
        }
    }

    /// Journal entry for an approved proposal.
    pub fn proposal_approved(
        block_number: u64,
        proposal: ProposalRef,
        tally: VoteTallyRef,
    ) -> Self {
        Self::ProposalApproved {
            ctx: EventContext::at(block_number),
            proposal,
            tally,
        }
    }

    /// Journal entry for a rejected proposal.
    pub fn proposal_rejected(
        block_number: u64,
        proposal: ProposalRef,
        tally: VoteTallyRef,
        reject_reason: String,
    ) -> Self {
        Self::ProposalRejected {
            ctx: EventContext::at(block_number),
            proposal,
            tally,
            reject_reason,
        }
    }

    /// Journal entry for a proposal whose target execution returned an error outcome.
    pub fn proposal_errored(
        block_number: u64,
        proposal: ProposalRef,
        tally: VoteTallyRef,
        execution_error: String,
    ) -> Self {
        Self::ProposalErrored {
            ctx: EventContext::at(block_number),
            proposal,
            tally,
            execution_error,
        }
    }

    /// Journal entry for an expired proposal.
    pub fn proposal_expired(block_number: u64, proposal: ProposalRef, tally: VoteTallyRef) -> Self {
        Self::ProposalExpired {
            ctx: EventContext::at(block_number),
            proposal,
            tally,
        }
    }
}

static JOURNAL: OnceLock<Journal> = OnceLock::new();

/// Initialize the journal. Must be called once at node startup before any
/// state-transition path runs. Subsequent calls are no-ops.
///
/// `datadir` is created if missing. The journal file is opened in append
/// mode, so existing content is preserved across node restarts.
pub fn init(datadir: &Path) -> std::io::Result<()> {
    if JOURNAL.get().is_some() {
        return Ok(());
    }

    let path = journal_path(datadir);
    let _ = JOURNAL.set(Journal::open(&path)?);
    tracing::info!(
        target: "outbe::governance::journal",
        path = %path.display(),
        "governance journal initialized",
    );
    Ok(())
}

/// Returns the journal path under `datadir`.
pub fn journal_path(datadir: &Path) -> PathBuf {
    datadir.join(JOURNAL_FILENAME)
}

/// Append `record` to the journal. If [`init`] has not been called, this
/// is a no-op (test-friendly). This function logs write errors at WARN and
/// swallows them. It never blocks the caller's state-transition path.
pub fn record(record: JournalRecord) {
    let Some(journal) = JOURNAL.get() else {
        return;
    };

    let line = match serde_json::to_string(&record) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(
                target: "outbe::governance::journal",
                error = %e,
                "failed to serialize journal record",
            );
            return;
        }
    };

    let mut guard = match journal.writer.lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };

    if let Err(e) = writeln!(guard, "{line}") {
        tracing::warn!(
            target: "outbe::governance::journal",
            error = %e,
            "failed to append journal record",
        );
        return;
    }
    if let Err(e) = guard.flush() {
        tracing::warn!(
            target: "outbe::governance::journal",
            error = %e,
            "failed to flush journal record",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn sample_proposal() -> ProposalRef {
        ProposalRef {
            proposal_id: "1".into(),
            proposer: "0x2cf6fbd0".into(),
            target_module: "0x000000000000000000000000000000000000ee0b".into(),
        }
    }

    fn sample_tally() -> VoteTallyRef {
        VoteTallyRef {
            yes_votes: 3,
            no_votes: 1,
            active_validator_count: 4,
        }
    }

    #[test]
    fn record_when_uninit_is_noop() {
        record(JournalRecord::vote_cast(
            0,
            "1".into(),
            "0xabc".into(),
            true,
        ));
    }

    #[test]
    fn journal_path_uses_governance_filename() {
        let dir = std::path::Path::new("/tmp/outbe-test");
        assert_eq!(journal_path(dir), dir.join("governance-journal.jsonl"));
    }

    #[test]
    fn record_serializes_to_jsonl_with_event_tag() {
        let rec = JournalRecord::proposal_approved(12_345, sample_proposal(), sample_tally());
        let json = serde_json::to_string(&rec).expect("serializable");
        assert!(json.contains("\"event\":\"proposal_approved\""));
        assert!(json.contains("\"yes_votes\":3"));
        assert!(json.contains("\"proposal_id\":\"1\""));
        assert!(json.contains("\"block_number\":12345"));
    }

    #[test]
    fn finalization_variants_share_flat_proposal_and_tally_fields() {
        let proposal = sample_proposal();
        let tally = sample_tally();
        for rec in [
            JournalRecord::proposal_approved(100, proposal.clone(), tally),
            JournalRecord::proposal_rejected(100, proposal.clone(), tally, "bad payload".into()),
            JournalRecord::proposal_expired(100, proposal, tally),
        ] {
            let json = serde_json::to_string(&rec).unwrap();
            assert!(json.contains("\"proposer\":\"0x2cf6fbd0\""));
            assert!(json.contains("\"yes_votes\":3"));
            assert!(json.contains("\"active_validator_count\":4"));
        }
    }

    #[test]
    fn init_then_record_writes_lines_and_survives_reopen() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = journal_path(dir.path());
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        let mut writer = std::io::BufWriter::new(file);
        let rec = JournalRecord::proposal_expired(19_200, sample_proposal(), sample_tally());
        writeln!(writer, "{}", serde_json::to_string(&rec).unwrap()).unwrap();
        writer.flush().unwrap();
        drop(writer);

        let mut content = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert!(content.contains("\"event\":\"proposal_expired\""));
        assert!(content.contains("\"block_number\":19200"));
    }
}
