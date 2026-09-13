use alloy_consensus::Transaction as AlloyTransaction;
use reth_ethereum::TransactionSigned;

use super::{
    SystemTxError, SystemTxInputV2, BOUNDARY_OUTCOME_SELECTOR,
    CERTIFIED_PARENT_ACCOUNTING_SELECTOR, CYCLE_TICK_SELECTOR, HOOK_EVENTS_SELECTOR,
    LATE_FINALIZE_CREDITS_SELECTOR, OCOMP_LIFECYCLE_BEGIN_SELECTOR,
    OCOMP_TERMINAL_REQUEST_SELECTOR, ORACLE_SLASH_WINDOW_SELECTOR, OUTBE_SYSTEM_TX_ADDRESS,
    REWARDS_GEM_DELIVERY_SELECTOR, TEE_BOOTSTRAP_SELECTOR,
};

/// Body-zone position of a system tx.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyZone {
    BeginBlock,
    EndBlock,
}

/// Consensus activation of the PoC OCOMP system-transaction lifecycle.
///
/// The production default is disabled. OCM-26 is the only task that may arm
/// the canonical devnet schedule; earlier tasks can exercise the exact fork
/// boundary by passing an explicit activation to layout validation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum OcompLifecycleActivation {
    #[default]
    Disabled,
    AtBlock(u64),
}

impl OcompLifecycleActivation {
    #[must_use]
    pub const fn at_block(height: u64) -> Self {
        Self::AtBlock(height)
    }

    #[must_use]
    pub const fn is_active_at(self, block_number: u64) -> bool {
        match self {
            Self::Disabled => false,
            Self::AtBlock(height) => block_number >= height,
        }
    }
}

/// begin_block system transaction kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SystemTxKind {
    CertifiedParentAccounting,
    /// mandatory begin-zone phase (blocks `>= 2`) that records verified
    /// late-finalize credits within the `K`-block inclusion window and settles
    /// matured per-block fee escrows. Ordered immediately after Phase 1 (CPA).
    LateFinalizeCredits,
    /// OCOMP begin-zone lifecycle slot. In the PoC this expires due jobs after
    /// the reserved no-op barrier and before ordinary user transactions.
    OcompLifecycleBegin,
    CycleTick,
    /// Mandatory Rewards-owned retryable delivery of one prepared UTC-day Gem batch.
    RewardsGemDelivery,
    BoundaryOutcome,
    /// Phase 3b: one-time TEE registry bootstrap (present only in the bootstrap
    /// block; reads the same-block `CommitteeSnapshotStore` written by Phase 3a).
    TeeBootstrap,
    OracleSlashWindow,
    /// Receipt container for whitelisted pre-exec hook events (`Vote`, `Update`, ...).
    HookEvents,
    /// Sole end-zone system transaction. The executor seals compressed
    /// entities before dispatching this terminal request slot.
    OcompTerminalRequest,
}

impl SystemTxKind {
    pub const fn selector(self) -> [u8; 4] {
        match self {
            Self::CertifiedParentAccounting => CERTIFIED_PARENT_ACCOUNTING_SELECTOR,
            Self::LateFinalizeCredits => LATE_FINALIZE_CREDITS_SELECTOR,
            Self::OcompLifecycleBegin => OCOMP_LIFECYCLE_BEGIN_SELECTOR,
            Self::CycleTick => CYCLE_TICK_SELECTOR,
            Self::RewardsGemDelivery => REWARDS_GEM_DELIVERY_SELECTOR,
            Self::BoundaryOutcome => BOUNDARY_OUTCOME_SELECTOR,
            Self::TeeBootstrap => TEE_BOOTSTRAP_SELECTOR,
            Self::OracleSlashWindow => ORACLE_SLASH_WINDOW_SELECTOR,
            Self::HookEvents => HOOK_EVENTS_SELECTOR,
            Self::OcompTerminalRequest => OCOMP_TERMINAL_REQUEST_SELECTOR,
        }
    }

    pub const fn body_zone(self) -> BodyZone {
        match self {
            Self::OcompTerminalRequest => BodyZone::EndBlock,
            Self::CertifiedParentAccounting
            | Self::LateFinalizeCredits
            | Self::OcompLifecycleBegin
            | Self::CycleTick
            | Self::RewardsGemDelivery
            | Self::BoundaryOutcome
            | Self::TeeBootstrap
            | Self::OracleSlashWindow
            | Self::HookEvents => BodyZone::BeginBlock,
        }
    }

    /// Whether a non-success EVM result (`Revert` / `Halt`) executing this
    /// begin-zone phase must fail the whole block instead of being recorded as a
    /// soft `status = 0` receipt and skipped. This classification applies only
    /// while the result fits the aggregate internal-work budget. An OOG consumes
    /// the full system-call gas limit; aggregate budget exhaustion always fails
    /// atomically before a receipt, including for a phase classified as soft.
    ///
    /// Consensus- and economic-critical phases are one-shot: their work cannot
    /// be retried by a later block, so a swallowed revert permanently loses it -
    /// stranded validator-fee escrow (`LateFinalizeCredits`), a dropped day of
    /// emission / terminal Metadosis (`CycleTick`), a skipped reshare / validator
    /// set activation (`BoundaryOutcome`), or unrecorded finalized-parent
    /// accounting (`CertifiedParentAccounting`). For these, a revert is a hard
    /// `BlockExecutionError`: the block is rejected on every validator
    /// deterministically (the revert is a function of committed chain state, the
    /// same for all proposers), honoring the "never silent stall / terminal
    /// failure is fatal" invariant rather than silently forfeiting real money or
    /// a protocol-state transition.
    ///
    /// `TeeBootstrap` is mandatory at block 1: a revert would commit a genesis
    /// committee that cannot execute confidential transactions, so it fails the
    /// block. `RewardsGemDelivery` is deliberately soft so its durable FIFO
    /// head retries in a later block; `OracleSlashWindow` and `HookEvents` also
    /// remain soft.
    pub const fn revert_fails_block(self) -> bool {
        match self {
            Self::CertifiedParentAccounting
            | Self::LateFinalizeCredits
            | Self::OcompLifecycleBegin
            | Self::CycleTick
            | Self::BoundaryOutcome
            | Self::TeeBootstrap
            | Self::OcompTerminalRequest => true,
            Self::RewardsGemDelivery | Self::OracleSlashWindow | Self::HookEvents => false,
        }
    }

    pub const fn begin_order(self) -> Option<u8> {
        match self {
            Self::CertifiedParentAccounting => Some(0),
            Self::LateFinalizeCredits => Some(1),
            Self::OcompLifecycleBegin => Some(2),
            Self::CycleTick => Some(3),
            Self::RewardsGemDelivery => Some(4),
            Self::BoundaryOutcome => Some(5),
            Self::TeeBootstrap => Some(6),
            Self::OracleSlashWindow => Some(7),
            Self::HookEvents => Some(8),
            Self::OcompTerminalRequest => None,
        }
    }

    pub const fn end_order(self) -> Option<u8> {
        match self {
            Self::OcompTerminalRequest => Some(0),
            Self::CertifiedParentAccounting
            | Self::LateFinalizeCredits
            | Self::OcompLifecycleBegin
            | Self::CycleTick
            | Self::RewardsGemDelivery
            | Self::BoundaryOutcome
            | Self::TeeBootstrap
            | Self::OracleSlashWindow
            | Self::HookEvents => None,
        }
    }

    pub(super) fn order_in(self, zone: BodyZone) -> Option<u8> {
        match zone {
            BodyZone::BeginBlock => self.begin_order(),
            BodyZone::EndBlock => self.end_order(),
        }
    }
}

pub fn system_tx_kind_from_selector(selector: [u8; 4]) -> Result<SystemTxKind, SystemTxError> {
    match selector {
        CERTIFIED_PARENT_ACCOUNTING_SELECTOR => Ok(SystemTxKind::CertifiedParentAccounting),
        LATE_FINALIZE_CREDITS_SELECTOR => Ok(SystemTxKind::LateFinalizeCredits),
        OCOMP_LIFECYCLE_BEGIN_SELECTOR => Ok(SystemTxKind::OcompLifecycleBegin),
        CYCLE_TICK_SELECTOR => Ok(SystemTxKind::CycleTick),
        REWARDS_GEM_DELIVERY_SELECTOR => Ok(SystemTxKind::RewardsGemDelivery),
        BOUNDARY_OUTCOME_SELECTOR => Ok(SystemTxKind::BoundaryOutcome),
        TEE_BOOTSTRAP_SELECTOR => Ok(SystemTxKind::TeeBootstrap),
        ORACLE_SLASH_WINDOW_SELECTOR => Ok(SystemTxKind::OracleSlashWindow),
        HOOK_EVENTS_SELECTOR => Ok(SystemTxKind::HookEvents),
        OCOMP_TERMINAL_REQUEST_SELECTOR => Ok(SystemTxKind::OcompTerminalRequest),
        other => Err(SystemTxError::UnknownSelector(other)),
    }
}

pub fn selector_from_input(input: &[u8]) -> Result<[u8; 4], SystemTxError> {
    let Some(bytes) = input.get(..4) else {
        return Err(SystemTxError::InputTooShort { len: input.len() });
    };
    bytes
        .try_into()
        .map_err(|_| SystemTxError::InputTooShort { len: input.len() })
}

pub fn is_reserved_system_tx<T>(tx: &T) -> bool
where
    T: AlloyTransaction + ?Sized,
{
    tx.to() == Some(OUTBE_SYSTEM_TX_ADDRESS)
}

pub fn decode_system_tx_kind(tx: &TransactionSigned) -> Result<SystemTxKind, SystemTxError> {
    let input = SystemTxInputV2::decode(tx.input().as_ref())?;
    Ok(input.kind())
}
