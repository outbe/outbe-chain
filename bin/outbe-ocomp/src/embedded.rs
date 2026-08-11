//! Local OCOMP job state shared by the embedded Validator and FullNode policies.
//!
//! This is deliberately not a scheduler or replay ledger. Canonical block
//! notifications reconstruct it, while the existing runner and retention/CAS
//! modules own computation and artifacts.

use std::collections::BTreeMap;

use alloy_primitives::B256;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbeddedOcompModeV1 {
    Validator,
    FullNode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbeddedJobStateV1 {
    Computing,
    WaitAtDeadline,
    LocalReady,
    Verified,
    ClosedNoQuorum,
    FatalMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmbeddedJobActionV1 {
    SubmitVote {
        job_id: B256,
        result_digest: B256,
    },
    AwaitCanonical {
        job_id: B256,
    },
    AwaitLocalResult {
        job_id: B256,
    },
    HoldProgress {
        job_id: B256,
        through_height: u64,
    },
    ReleaseProgress {
        job_id: B256,
    },
    FatalMismatch {
        job_id: B256,
        local_result_digest: B256,
        canonical_result_digest: B256,
    },
    CloseNoQuorum {
        job_id: B256,
    },
    ProtocolOwned,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EmbeddedJobV1 {
    deadline_height: u64,
    local_result_digest: Option<B256>,
    canonical_result_digest: Option<B256>,
    state: EmbeddedJobStateV1,
}

#[derive(Debug)]
pub struct EmbeddedOcompJobsV1 {
    mode: EmbeddedOcompModeV1,
    jobs: BTreeMap<B256, EmbeddedJobV1>,
}

impl EmbeddedOcompJobsV1 {
    #[must_use]
    pub const fn new(mode: EmbeddedOcompModeV1) -> Self {
        Self {
            mode,
            jobs: BTreeMap::new(),
        }
    }

    pub fn observe_job(
        &mut self,
        job_id: B256,
        deadline_height: u64,
    ) -> Result<(), EmbeddedOcompStateErrorV1> {
        if job_id.is_zero() || deadline_height == 0 {
            return Err(EmbeddedOcompStateErrorV1::InvalidJob);
        }
        match self.jobs.get(&job_id) {
            Some(existing) if existing.deadline_height == deadline_height => Ok(()),
            Some(_) => Err(EmbeddedOcompStateErrorV1::ConflictingReplay { job_id }),
            None => {
                self.jobs.insert(
                    job_id,
                    EmbeddedJobV1 {
                        deadline_height,
                        local_result_digest: None,
                        canonical_result_digest: None,
                        state: EmbeddedJobStateV1::Computing,
                    },
                );
                Ok(())
            }
        }
    }

    pub fn record_local_result(
        &mut self,
        job_id: B256,
        result_digest: B256,
    ) -> Result<EmbeddedJobActionV1, EmbeddedOcompStateErrorV1> {
        if result_digest.is_zero() {
            return Err(EmbeddedOcompStateErrorV1::InvalidResult);
        }
        let job = self
            .jobs
            .get_mut(&job_id)
            .ok_or(EmbeddedOcompStateErrorV1::UnknownJob { job_id })?;
        if job
            .local_result_digest
            .is_some_and(|existing| existing != result_digest)
        {
            return Err(EmbeddedOcompStateErrorV1::ConflictingLocalResult { job_id });
        }
        job.local_result_digest = Some(result_digest);
        job.state = EmbeddedJobStateV1::LocalReady;
        Ok(match self.mode {
            EmbeddedOcompModeV1::Validator if job.canonical_result_digest.is_some() => {
                job.state = EmbeddedJobStateV1::Verified;
                EmbeddedJobActionV1::ProtocolOwned
            }
            EmbeddedOcompModeV1::Validator => match job.canonical_result_digest {
                None => EmbeddedJobActionV1::SubmitVote {
                    job_id,
                    result_digest,
                },
                Some(canonical) if canonical == result_digest => {
                    job.state = EmbeddedJobStateV1::Verified;
                    EmbeddedJobActionV1::ReleaseProgress { job_id }
                }
                Some(canonical) => {
                    job.state = EmbeddedJobStateV1::FatalMismatch;
                    EmbeddedJobActionV1::FatalMismatch {
                        job_id,
                        local_result_digest: result_digest,
                        canonical_result_digest: canonical,
                    }
                }
            },
            EmbeddedOcompModeV1::FullNode => match job.canonical_result_digest {
                None => EmbeddedJobActionV1::AwaitCanonical { job_id },
                Some(canonical) if canonical == result_digest => {
                    job.state = EmbeddedJobStateV1::Verified;
                    EmbeddedJobActionV1::ReleaseProgress { job_id }
                }
                Some(canonical) => {
                    job.state = EmbeddedJobStateV1::FatalMismatch;
                    EmbeddedJobActionV1::FatalMismatch {
                        job_id,
                        local_result_digest: result_digest,
                        canonical_result_digest: canonical,
                    }
                }
            },
        })
    }

    pub fn observe_deadline(
        &mut self,
        job_id: B256,
    ) -> Result<EmbeddedJobActionV1, EmbeddedOcompStateErrorV1> {
        let job = self
            .jobs
            .get_mut(&job_id)
            .ok_or(EmbeddedOcompStateErrorV1::UnknownJob { job_id })?;
        if self.mode == EmbeddedOcompModeV1::Validator {
            return Ok(EmbeddedJobActionV1::ProtocolOwned);
        }
        if job.local_result_digest.is_some() {
            return Ok(EmbeddedJobActionV1::AwaitCanonical { job_id });
        }
        job.state = EmbeddedJobStateV1::WaitAtDeadline;
        Ok(EmbeddedJobActionV1::HoldProgress {
            job_id,
            through_height: job.deadline_height,
        })
    }

    pub fn record_canonical_result(
        &mut self,
        job_id: B256,
        result_digest: B256,
    ) -> Result<EmbeddedJobActionV1, EmbeddedOcompStateErrorV1> {
        if result_digest.is_zero() {
            return Err(EmbeddedOcompStateErrorV1::InvalidResult);
        }
        let job = self
            .jobs
            .get_mut(&job_id)
            .ok_or(EmbeddedOcompStateErrorV1::UnknownJob { job_id })?;
        if job
            .canonical_result_digest
            .is_some_and(|existing| existing != result_digest)
        {
            return Err(EmbeddedOcompStateErrorV1::ConflictingCanonicalResult { job_id });
        }
        job.canonical_result_digest = Some(result_digest);
        if self.mode == EmbeddedOcompModeV1::Validator {
            job.state = EmbeddedJobStateV1::Verified;
            return Ok(EmbeddedJobActionV1::ProtocolOwned);
        }
        let Some(local) = job.local_result_digest else {
            return Ok(EmbeddedJobActionV1::AwaitLocalResult { job_id });
        };
        if local == result_digest {
            job.state = EmbeddedJobStateV1::Verified;
            Ok(EmbeddedJobActionV1::ReleaseProgress { job_id })
        } else {
            job.state = EmbeddedJobStateV1::FatalMismatch;
            Ok(EmbeddedJobActionV1::FatalMismatch {
                job_id,
                local_result_digest: local,
                canonical_result_digest: result_digest,
            })
        }
    }

    pub fn record_no_quorum(
        &mut self,
        job_id: B256,
    ) -> Result<EmbeddedJobActionV1, EmbeddedOcompStateErrorV1> {
        let job = self
            .jobs
            .get_mut(&job_id)
            .ok_or(EmbeddedOcompStateErrorV1::UnknownJob { job_id })?;
        match job.state {
            EmbeddedJobStateV1::Computing
            | EmbeddedJobStateV1::WaitAtDeadline
            | EmbeddedJobStateV1::LocalReady => {
                job.state = EmbeddedJobStateV1::ClosedNoQuorum;
                Ok(EmbeddedJobActionV1::CloseNoQuorum { job_id })
            }
            EmbeddedJobStateV1::ClosedNoQuorum => Ok(EmbeddedJobActionV1::CloseNoQuorum { job_id }),
            EmbeddedJobStateV1::Verified | EmbeddedJobStateV1::FatalMismatch => {
                Err(EmbeddedOcompStateErrorV1::TerminalJob { job_id })
            }
        }
    }

    #[must_use]
    pub fn state(&self, job_id: B256) -> Option<EmbeddedJobStateV1> {
        self.jobs.get(&job_id).map(|job| job.state)
    }

    #[must_use]
    pub fn live_job_count(&self) -> usize {
        self.jobs
            .values()
            .filter(|job| {
                matches!(
                    job.state,
                    EmbeddedJobStateV1::Computing
                        | EmbeddedJobStateV1::WaitAtDeadline
                        | EmbeddedJobStateV1::LocalReady
                )
            })
            .count()
    }

    /// Highest finalized height a FullNode may expose to its execution gate.
    ///
    /// A job becomes a barrier at its deadline and remains one until the local
    /// result is verified against canonical authority or the chain closes it
    /// without quorum. This intentionally includes `LocalReady`: having a local
    /// answer is not enough until the finalized quorum answer is known.
    #[must_use]
    pub fn progress_limit(&self, finalized_height: u64) -> u64 {
        if self.mode == EmbeddedOcompModeV1::Validator {
            return finalized_height;
        }
        self.jobs
            .values()
            .filter(|job| {
                job.deadline_height <= finalized_height
                    && matches!(
                        job.state,
                        EmbeddedJobStateV1::Computing
                            | EmbeddedJobStateV1::WaitAtDeadline
                            | EmbeddedJobStateV1::LocalReady
                    )
            })
            .fold(finalized_height, |limit, job| {
                limit.min(job.deadline_height.saturating_sub(1))
            })
    }
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum EmbeddedOcompStateErrorV1 {
    #[error("embedded OCOMP job identity or deadline is invalid")]
    InvalidJob,
    #[error("embedded OCOMP local result digest is invalid")]
    InvalidResult,
    #[error("embedded OCOMP replay conflicts for job {job_id}")]
    ConflictingReplay { job_id: B256 },
    #[error("embedded OCOMP local result conflicts for job {job_id}")]
    ConflictingLocalResult { job_id: B256 },
    #[error("embedded OCOMP canonical result conflicts for job {job_id}")]
    ConflictingCanonicalResult { job_id: B256 },
    #[error("embedded OCOMP job {job_id} is unknown")]
    UnknownJob { job_id: B256 },
    #[error("embedded OCOMP job {job_id} is already terminal")]
    TerminalJob { job_id: B256 },
}
