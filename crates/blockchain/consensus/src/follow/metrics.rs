//! Follower and upstream-bootstrap telemetry.

use super::upstream::UpstreamTipError;

/// Closed telemetry labels for UpstreamPollErrorReason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpstreamPollErrorReason {
    Http429,
    HttpStatus,
    Transport,
    InvalidResponse,
}

impl UpstreamPollErrorReason {
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Http429 => "http_429",
            Self::HttpStatus => "http_status",
            Self::Transport => "transport",
            Self::InvalidResponse => "invalid_response",
        }
    }
}

/// Closed telemetry labels for FollowerRoundResult.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowerRoundResult {
    Idle,
    Pulled,
    CeilingClamped,
    TipUnreachable,
}

impl FollowerRoundResult {
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Pulled => "pulled",
            Self::CeilingClamped => "ceiling_clamped",
            Self::TipUnreachable => "tip_unreachable",
        }
    }
}

/// Closed telemetry labels for FollowerResolverKind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowerResolverKind {
    Block,
    Finalized,
}

impl FollowerResolverKind {
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Finalized => "finalized",
        }
    }
}

/// Closed telemetry labels for FollowerResolverFailureReason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowerResolverFailureReason {
    Timeout,
    PermitSaturated,
    MissingOrError,
    HashMismatch,
    Oversized,
    UnannotatedBlock,
    ProofUnauthenticated,
    DeliveryRejected,
}

impl FollowerResolverFailureReason {
    pub const fn as_label(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::PermitSaturated => "permit_saturated",
            Self::MissingOrError => "missing_or_error",
            Self::HashMismatch => "hash_mismatch",
            Self::Oversized => "oversized",
            Self::UnannotatedBlock => "unannotated_block",
            Self::ProofUnauthenticated => "proof_unauthenticated",
            Self::DeliveryRejected => "delivery_rejected",
        }
    }
}

impl From<&UpstreamTipError> for UpstreamPollErrorReason {
    fn from(error: &UpstreamTipError) -> Self {
        match error {
            UpstreamTipError::HttpRateLimited { .. } => Self::Http429,
            UpstreamTipError::HttpStatus { .. } => Self::HttpStatus,
            UpstreamTipError::Transport { .. } => Self::Transport,
            UpstreamTipError::InvalidResponse { .. } => Self::InvalidResponse,
        }
    }
}

/// Clears freshness when the driver starts and when its run future is dropped.
pub(super) struct TipKnownGuard(metrics::Gauge);

impl TipKnownGuard {
    pub(super) fn new() -> Self {
        let gauge = metrics::gauge!("outbe_follower_upstream_tip_known");
        gauge.set(0.0);
        Self(gauge)
    }
}

impl Drop for TipKnownGuard {
    fn drop(&mut self) {
        self.0.set(0.0);
    }
}

pub fn record_upstream_tip_ok(height: u64, now_unix_secs: f64) {
    metrics::gauge!("outbe_follower_upstream_tip_height").set(height as f64);
    metrics::gauge!("outbe_follower_upstream_tip_known").set(1.0);
    metrics::gauge!("outbe_follower_upstream_last_ok_timestamp_seconds").set(now_unix_secs);
}

pub fn record_upstream_tip_err(err: &UpstreamTipError) {
    metrics::gauge!("outbe_follower_upstream_tip_known").set(0.0);
    metrics::counter!("outbe_follower_upstream_poll_errors_total", "reason" => UpstreamPollErrorReason::from(err).as_label()).increment(1);
}

pub fn record_follower_heights(processed: Option<u64>, ceiling: Option<u64>) {
    metrics::gauge!("outbe_follower_processed_height").set(processed.unwrap_or_default() as f64);
    metrics::gauge!("outbe_follower_supported_ceiling_height")
        .set(ceiling.unwrap_or_default() as f64);
}

pub fn record_follower_round(result: FollowerRoundResult) {
    metrics::counter!("outbe_follower_rounds_total", "result" => result.as_label()).increment(1);
}

pub fn record_resolver_failure(kind: FollowerResolverKind, reason: FollowerResolverFailureReason) {
    metrics::counter!("outbe_follower_resolver_failures_total", "kind" => kind.as_label(), "reason" => reason.as_label()).increment(1);
}
