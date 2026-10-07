use alloy_primitives::{B256, U256};
use outbe_primitives::time::WorldwideDay;
use thiserror::Error;

/// Consensus-visible phase of one Metadosis day.
///
/// There is intentionally no `Running` variant: execution progress belongs to
/// the validator-local supervisor, not to consensus.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DayPhase {
    Ready,
    OffchainPending,
    Terminal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobFsmTransitionKind {
    Defer,
    Request,
    OpenVoting,
    Expire,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobFsmTransitionRule {
    pub kind: JobFsmTransitionKind,
    pub from: DayPhase,
    pub to: DayPhase,
}

const JOB_FSM_TRANSITION_RULES: [JobFsmTransitionRule; 4] = [
    JobFsmTransitionRule {
        kind: JobFsmTransitionKind::Defer,
        from: DayPhase::Ready,
        to: DayPhase::Ready,
    },
    JobFsmTransitionRule {
        kind: JobFsmTransitionKind::Request,
        from: DayPhase::Ready,
        to: DayPhase::OffchainPending,
    },
    JobFsmTransitionRule {
        kind: JobFsmTransitionKind::OpenVoting,
        from: DayPhase::OffchainPending,
        to: DayPhase::OffchainPending,
    },
    JobFsmTransitionRule {
        kind: JobFsmTransitionKind::Expire,
        from: DayPhase::OffchainPending,
        to: DayPhase::Terminal,
    },
];

/// A request that never receives a certified-parent finality binding cannot
/// retain one of the bounded live-job slots forever.
pub const OCOMP_AWAITING_FINALITY_DEADLINE_BLOCKS: u64 = 64;

/// Frozen request/expiry transition table consumed by the model and storage
/// adapters. Future activation transitions append under their own DAG tasks.
#[must_use]
pub const fn transition_rules() -> &'static [JobFsmTransitionRule] {
    &JOB_FSM_TRANSITION_RULES
}

/// A fresh request applies the limit effect. An existing authoritative receipt is corruption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestEffectMode {
    Fresh { effect_nonce: u64 },
}

/// Commands accepted by the request/expiry slice of the production FSM.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobFsmCommand {
    Defer {
        at_height: u64,
        next_check_height: u64,
    },
    Request {
        at_height: u64,
        deadline_height: u64,
        intent_id: B256,
        lysis_limit_minor: U256,
        request_limit_receipt_hash: B256,
    },
    OpenVoting {
        at_height: u64,
        deadline_height: u64,
    },
    Expire {
        at_height: u64,
        at_time: u64,
    },
}

impl JobFsmCommand {
    const fn transition_kind(self) -> JobFsmTransitionKind {
        match self {
            Self::Defer { .. } => JobFsmTransitionKind::Defer,
            Self::Request { .. } => JobFsmTransitionKind::Request,
            Self::OpenVoting { .. } => JobFsmTransitionKind::OpenVoting,
            Self::Expire { .. } => JobFsmTransitionKind::Expire,
        }
    }
}

/// Stable read projection used by tests now and public views in later tasks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobFsmProjection {
    pub worldwide_day: WorldwideDay,
    pub phase: DayPhase,
    pub pending_nonce: u64,
    pub next_check_height: Option<u64>,
    pub live_intent_id: Option<B256>,
    pub deadline_height: Option<u64>,
    pub terminal_records: u16,
    pub retained_lysis_limit_minor: Option<U256>,
}

/// Immutable evidence retained while an expiry transition is committed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalAttempt {
    pub intent_id: B256,
    pub pending_nonce: u64,
    pub terminal_height: u64,
    pub terminal_time: u64,
    pub retained_lysis_limit_minor: U256,
}

/// Canonical persistence projection of the immutable request-phase effect.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetainedRequestEffectSnapshot {
    pub effect_nonce: u64,
    pub lysis_limit_minor: U256,
    pub receipt_hash: B256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadyAttemptSnapshot {
    pub pending_nonce: u64,
    pub next_check_height: u64,
    pub retained_effect: Option<RetainedRequestEffectSnapshot>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LiveAttemptSnapshot {
    pub intent_id: B256,
    pub pending_nonce: u64,
    pub requested_height: u64,
    pub deadline_height: Option<u64>,
    pub retained_effect: RetainedRequestEffectSnapshot,
}

/// Typed storage boundary. Loading persisted fields always passes through
/// [`JobFsmState::restore`] before the state can drive a transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobFsmSnapshot {
    pub worldwide_day: WorldwideDay,
    pub ready: Option<ReadyAttemptSnapshot>,
    pub live: Option<LiveAttemptSnapshot>,
    pub terminal: Vec<TerminalAttempt>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RetainedRequestEffect {
    effect_nonce: u64,
    lysis_limit_minor: U256,
    receipt_hash: B256,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReadyAttempt {
    pending_nonce: u64,
    next_check_height: u64,
    retained_effect: Option<RetainedRequestEffect>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LiveAttempt {
    intent_id: B256,
    pending_nonce: u64,
    requested_height: u64,
    deadline_height: Option<u64>,
    retained_effect: RetainedRequestEffect,
}

/// Complete bounded state needed to decide request and exclusive expiry
/// transitions without scanning unrelated WorldwideDays.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobFsmState {
    worldwide_day: WorldwideDay,
    attempt: Attempt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Attempt {
    Ready(ReadyAttempt),
    Pending(LiveAttempt),
    Terminal(TerminalAttempt),
}

struct PendingRequest {
    at_height: u64,
    deadline_height: u64,
    intent_id: B256,
    lysis_limit_minor: U256,
    receipt_hash: B256,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum JobFsmError {
    #[error("OCOMP FSM must have exactly one READY or OFFCHAIN_PENDING state")]
    InvalidPhaseCardinality,
    #[error("initial OCOMP pending nonce must be zero")]
    InvalidInitialNonce,
    #[error("OCOMP request is not due until height {due_height}")]
    RequestNotDue { due_height: u64 },
    #[error(
        "OCOMP deferred READY height {next_check_height} must follow processing height {at_height}"
    )]
    InvalidDeferredHeight {
        at_height: u64,
        next_check_height: u64,
    },
    #[error("OCOMP request requires READY state")]
    RequestRequiresReady,
    #[error("OCOMP expiry requires OFFCHAIN_PENDING state")]
    ExpiryRequiresPending,
    #[error("OCOMP voting open requires OFFCHAIN_PENDING state")]
    OpenVotingRequiresPending,
    #[error("OCOMP voting cannot open before height {open_height}")]
    VotingOpenTooEarly { open_height: u64 },
    #[error("OCOMP expiry requires a live attempt deadline")]
    ExpiryRequiresDeadline,
    #[error("OCOMP intent id is the reserved zero hash")]
    ZeroIntentId,
    #[error("OCOMP request limit receipt hash is the reserved zero hash")]
    ZeroRequestLimitReceiptHash,
    #[error("OCOMP deadline {deadline_height} must follow request height {request_height}")]
    InvalidDeadline {
        request_height: u64,
        deadline_height: u64,
    },
    #[error("OCOMP deadline {deadline_height} has not been reached at height {at_height}")]
    DeadlineNotReached {
        at_height: u64,
        deadline_height: u64,
    },
    #[error("OCOMP height overflow")]
    HeightOverflow,
    #[error("OCOMP terminal evidence is inconsistent")]
    InvalidTerminalEvidence,
    #[error("OCOMP request effect is inconsistent")]
    InvalidRequestEffect,
    #[error("OCOMP FSM transition table disagrees with the applied transition")]
    InvalidTransitionRule,
}

impl JobFsmState {
    /// Constructs the only fresh lifecycle state.
    #[must_use]
    pub fn initial_ready(worldwide_day: WorldwideDay, first_check_height: u64) -> Self {
        Self {
            worldwide_day,
            attempt: Attempt::Ready(ReadyAttempt {
                pending_nonce: 0,
                next_check_height: first_check_height,
                retained_effect: None,
            }),
        }
    }

    /// Restores persisted state, checking cardinality before its evidence.
    pub fn restore(snapshot: JobFsmSnapshot) -> Result<Self, JobFsmError> {
        let attempt = match (snapshot.ready, snapshot.live, snapshot.terminal.as_slice()) {
            (Some(ready), None, []) => Attempt::Ready(ReadyAttempt {
                pending_nonce: ready.pending_nonce,
                next_check_height: ready.next_check_height,
                retained_effect: ready.retained_effect.map(RetainedRequestEffect::from),
            }),
            (None, Some(live), []) => Attempt::Pending(LiveAttempt {
                intent_id: live.intent_id,
                pending_nonce: live.pending_nonce,
                requested_height: live.requested_height,
                deadline_height: live.deadline_height,
                retained_effect: RetainedRequestEffect::from(live.retained_effect),
            }),
            (None, None, [terminal]) => Attempt::Terminal(*terminal),
            _ => return Err(JobFsmError::InvalidPhaseCardinality),
        };
        let state = Self {
            worldwide_day: snapshot.worldwide_day,
            attempt,
        };
        state.validate()?;
        Ok(state)
    }

    #[must_use]
    pub fn snapshot(&self) -> JobFsmSnapshot {
        let mut snapshot = JobFsmSnapshot {
            worldwide_day: self.worldwide_day,
            ready: None,
            live: None,
            terminal: Vec::new(),
        };
        match self.attempt {
            Attempt::Ready(ready) => {
                snapshot.ready = Some(ReadyAttemptSnapshot {
                    pending_nonce: ready.pending_nonce,
                    next_check_height: ready.next_check_height,
                    retained_effect: ready
                        .retained_effect
                        .map(RetainedRequestEffectSnapshot::from),
                })
            }
            Attempt::Pending(live) => {
                snapshot.live = Some(LiveAttemptSnapshot {
                    intent_id: live.intent_id,
                    pending_nonce: live.pending_nonce,
                    requested_height: live.requested_height,
                    deadline_height: live.deadline_height,
                    retained_effect: RetainedRequestEffectSnapshot::from(live.retained_effect),
                })
            }
            Attempt::Terminal(terminal) => snapshot.terminal.push(terminal),
        }
        snapshot
    }

    pub fn request_effect_mode(&self) -> Result<RequestEffectMode, JobFsmError> {
        let ready = self.ready()?;
        if ready.retained_effect.is_some() || ready.pending_nonce != 0 {
            return Err(JobFsmError::InvalidRequestEffect);
        }
        Ok(RequestEffectMode::Fresh {
            effect_nonce: ready.pending_nonce,
        })
    }

    /// A rejected command never mutates the receiver.
    pub fn apply(&mut self, command: JobFsmCommand) -> Result<JobFsmProjection, JobFsmError> {
        let mut candidate = self.clone();
        candidate.apply_inner(command)?;
        candidate.validate()?;
        *self = candidate;
        Ok(self.projection())
    }

    fn apply_inner(&mut self, command: JobFsmCommand) -> Result<(), JobFsmError> {
        let rule = self.transition_rule(command.transition_kind())?;
        self.apply_command(command)?;
        if self.phase() != rule.to {
            return Err(JobFsmError::InvalidTransitionRule);
        }
        Ok(())
    }

    fn transition_rule(
        &self,
        kind: JobFsmTransitionKind,
    ) -> Result<JobFsmTransitionRule, JobFsmError> {
        let rule = transition_rules()
            .iter()
            .copied()
            .find(|rule| rule.kind == kind)
            .ok_or(JobFsmError::InvalidTransitionRule)?;
        if self.phase() != rule.from {
            return Err(match kind {
                JobFsmTransitionKind::Expire => JobFsmError::ExpiryRequiresPending,
                JobFsmTransitionKind::OpenVoting => JobFsmError::OpenVotingRequiresPending,
                JobFsmTransitionKind::Defer | JobFsmTransitionKind::Request => {
                    JobFsmError::RequestRequiresReady
                }
            });
        }
        Ok(rule)
    }

    fn apply_command(&mut self, command: JobFsmCommand) -> Result<(), JobFsmError> {
        match command {
            JobFsmCommand::Defer {
                at_height,
                next_check_height,
            } => self.defer(at_height, next_check_height),
            JobFsmCommand::Request {
                at_height,
                deadline_height,
                intent_id,
                lysis_limit_minor,
                request_limit_receipt_hash,
            } => self.request(PendingRequest {
                at_height,
                deadline_height,
                intent_id,
                lysis_limit_minor,
                receipt_hash: request_limit_receipt_hash,
            }),
            JobFsmCommand::OpenVoting {
                at_height,
                deadline_height,
            } => self.open_voting(at_height, deadline_height),
            JobFsmCommand::Expire { at_height, at_time } => self.expire(at_height, at_time),
        }
    }

    fn ready(&self) -> Result<ReadyAttempt, JobFsmError> {
        match self.attempt {
            Attempt::Ready(ready) => Ok(ready),
            _ => Err(JobFsmError::RequestRequiresReady),
        }
    }

    fn pending(&self, error: JobFsmError) -> Result<LiveAttempt, JobFsmError> {
        match self.attempt {
            Attempt::Pending(live) => Ok(live),
            _ => Err(error),
        }
    }

    fn defer(&mut self, at_height: u64, next_check_height: u64) -> Result<(), JobFsmError> {
        let mut ready = self.ready()?;
        ready.ensure_due(at_height)?;
        if next_check_height <= at_height {
            return Err(JobFsmError::InvalidDeferredHeight {
                at_height,
                next_check_height,
            });
        }
        ready.next_check_height = next_check_height;
        self.attempt = Attempt::Ready(ready);
        Ok(())
    }

    fn request(&mut self, input: PendingRequest) -> Result<(), JobFsmError> {
        let ready = self.ready()?;
        ready.ensure_due(input.at_height)?;
        if input.intent_id.is_zero() {
            return Err(JobFsmError::ZeroIntentId);
        }
        if input.receipt_hash.is_zero() {
            return Err(JobFsmError::ZeroRequestLimitReceiptHash);
        }
        ensure_deadline(input.at_height, input.deadline_height)?;
        if ready.retained_effect.is_some() {
            return Err(JobFsmError::InvalidRequestEffect);
        }
        self.attempt = Attempt::Pending(LiveAttempt {
            intent_id: input.intent_id,
            pending_nonce: ready.pending_nonce,
            requested_height: input.at_height,
            deadline_height: Some(input.deadline_height),
            retained_effect: RetainedRequestEffect {
                effect_nonce: ready.pending_nonce,
                lysis_limit_minor: input.lysis_limit_minor,
                receipt_hash: input.receipt_hash,
            },
        });
        Ok(())
    }

    fn open_voting(&mut self, at_height: u64, deadline_height: u64) -> Result<(), JobFsmError> {
        let mut live = self.pending(JobFsmError::OpenVotingRequiresPending)?;
        if at_height <= live.requested_height {
            return Err(JobFsmError::VotingOpenTooEarly {
                open_height: live
                    .requested_height
                    .checked_add(1)
                    .ok_or(JobFsmError::HeightOverflow)?,
            });
        }
        ensure_deadline(at_height, deadline_height)?;
        live.deadline_height = Some(deadline_height);
        self.attempt = Attempt::Pending(live);
        Ok(())
    }

    fn expire(&mut self, at_height: u64, at_time: u64) -> Result<(), JobFsmError> {
        let live = self.pending(JobFsmError::ExpiryRequiresPending)?;
        let deadline_height = live
            .deadline_height
            .ok_or(JobFsmError::ExpiryRequiresDeadline)?;
        if at_height < deadline_height {
            return Err(JobFsmError::DeadlineNotReached {
                at_height,
                deadline_height,
            });
        }
        self.attempt = Attempt::Terminal(TerminalAttempt {
            intent_id: live.intent_id,
            pending_nonce: live.pending_nonce,
            terminal_height: at_height,
            terminal_time: at_time,
            retained_lysis_limit_minor: live.retained_effect.lysis_limit_minor,
        });
        Ok(())
    }

    fn phase(&self) -> DayPhase {
        match self.attempt {
            Attempt::Ready(_) => DayPhase::Ready,
            Attempt::Pending(_) => DayPhase::OffchainPending,
            Attempt::Terminal(_) => DayPhase::Terminal,
        }
    }

    pub fn validate(&self) -> Result<(), JobFsmError> {
        match self.attempt {
            Attempt::Ready(ready) => ready.validate(),
            Attempt::Pending(live) => live.validate(),
            Attempt::Terminal(terminal) => {
                if terminal.pending_nonce != 0 || terminal.intent_id.is_zero() {
                    return Err(JobFsmError::InvalidTerminalEvidence);
                }
                Ok(())
            }
        }
    }

    #[must_use]
    pub fn projection(&self) -> JobFsmProjection {
        let mut projection = JobFsmProjection {
            worldwide_day: self.worldwide_day,
            phase: self.phase(),
            pending_nonce: 0,
            next_check_height: None,
            live_intent_id: None,
            deadline_height: None,
            terminal_records: 0,
            retained_lysis_limit_minor: None,
        };
        match self.attempt {
            Attempt::Ready(ready) => {
                projection.pending_nonce = ready.pending_nonce;
                projection.next_check_height = Some(ready.next_check_height);
                projection.retained_lysis_limit_minor =
                    ready.retained_effect.map(|effect| effect.lysis_limit_minor);
            }
            Attempt::Pending(live) => {
                projection.pending_nonce = live.pending_nonce;
                projection.live_intent_id = Some(live.intent_id);
                projection.deadline_height = live.deadline_height;
                projection.retained_lysis_limit_minor =
                    Some(live.retained_effect.lysis_limit_minor);
            }
            Attempt::Terminal(terminal) => {
                projection.pending_nonce = terminal.pending_nonce;
                projection.terminal_records = 1;
                projection.retained_lysis_limit_minor = Some(terminal.retained_lysis_limit_minor);
            }
        }
        projection
    }

    #[must_use]
    pub fn terminal_attempts(&self) -> &[TerminalAttempt] {
        match &self.attempt {
            Attempt::Terminal(terminal) => std::slice::from_ref(terminal),
            _ => &[],
        }
    }
}

fn ensure_deadline(request_height: u64, deadline_height: u64) -> Result<(), JobFsmError> {
    if deadline_height <= request_height {
        return Err(JobFsmError::InvalidDeadline {
            request_height,
            deadline_height,
        });
    }
    Ok(())
}

fn validate_initial_effect(
    pending_nonce: u64,
    effect: Option<RetainedRequestEffect>,
) -> Result<(), JobFsmError> {
    if pending_nonce != 0 {
        return Err(JobFsmError::InvalidInitialNonce);
    }
    if effect.is_some_and(|effect| effect.effect_nonce != 0 || effect.effect_nonce > pending_nonce)
    {
        return Err(JobFsmError::InvalidRequestEffect);
    }
    Ok(())
}

impl ReadyAttempt {
    fn ensure_due(self, at_height: u64) -> Result<(), JobFsmError> {
        if at_height < self.next_check_height {
            return Err(JobFsmError::RequestNotDue {
                due_height: self.next_check_height,
            });
        }
        Ok(())
    }
    fn validate(self) -> Result<(), JobFsmError> {
        if self.retained_effect.is_some() != (self.pending_nonce != 0) {
            return Err(JobFsmError::InvalidRequestEffect);
        }
        validate_initial_effect(self.pending_nonce, self.retained_effect)
    }
}

impl LiveAttempt {
    fn validate(self) -> Result<(), JobFsmError> {
        let invalid_deadline = self
            .deadline_height
            .is_some_and(|deadline| deadline <= self.requested_height);
        if self.intent_id.is_zero()
            || invalid_deadline
            || self.retained_effect.receipt_hash.is_zero()
        {
            return Err(JobFsmError::InvalidRequestEffect);
        }
        validate_initial_effect(self.pending_nonce, Some(self.retained_effect))
    }
}

impl From<RetainedRequestEffectSnapshot> for RetainedRequestEffect {
    fn from(snapshot: RetainedRequestEffectSnapshot) -> Self {
        Self {
            effect_nonce: snapshot.effect_nonce,
            lysis_limit_minor: snapshot.lysis_limit_minor,
            receipt_hash: snapshot.receipt_hash,
        }
    }
}

impl From<RetainedRequestEffect> for RetainedRequestEffectSnapshot {
    fn from(effect: RetainedRequestEffect) -> Self {
        Self {
            effect_nonce: effect.effect_nonce,
            lysis_limit_minor: effect.lysis_limit_minor,
            receipt_hash: effect.receipt_hash,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_is_terminal_and_does_not_create_a_successor_attempt() {
        let mut state = JobFsmState::initial_ready(WorldwideDay::new(20_260_726), 10);
        state
            .apply(JobFsmCommand::Request {
                at_height: 10,
                deadline_height: 74,
                intent_id: B256::repeat_byte(0x11),
                lysis_limit_minor: U256::from(900),
                request_limit_receipt_hash: B256::repeat_byte(0x22),
            })
            .unwrap();

        let projection = state
            .apply(JobFsmCommand::Expire {
                at_height: 74,
                at_time: 1_800,
            })
            .unwrap();

        assert_eq!(projection.phase, DayPhase::Terminal);
        assert_eq!(projection.pending_nonce, 0);
        assert_eq!(projection.next_check_height, None);
        assert_eq!(projection.live_intent_id, None);
        assert_eq!(projection.retained_lysis_limit_minor, Some(U256::from(900)));
        assert_eq!(projection.terminal_records, 1);
        assert_eq!(
            state.terminal_attempts(),
            &[TerminalAttempt {
                intent_id: B256::repeat_byte(0x11),
                pending_nonce: 0,
                terminal_height: 74,
                terminal_time: 1_800,
                retained_lysis_limit_minor: U256::from(900),
            }]
        );
    }
}
