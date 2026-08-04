//! Typed validator lifecycle state and coupled-field decoding.
//!
//! The aggregate in this module deliberately keeps the independent storage
//! dimensions (stake, registry identity, networking, history, and lifecycle)
//! separate.  Lifecycle transitions consume their source state so callers
//! cannot accidentally keep using a stale substate after a transition.

use std::num::NonZeroU64;

use alloy_primitives::{Address, U256};
use outbe_primitives::consensus_p2p::{
    decode_versioned, encode_v1, P2pAddress, P2P_ADDRESS_VERSION_V1,
};
use outbe_primitives::error::{PrecompileError, Result};

/// Legacy persisted status bytes.
///
/// The typed state machine is authoritative for lifecycle decisions. These
/// constants remain public only because existing Rust and Solidity-facing code
/// still exchanges the persisted `uint8` representation.
use crate::runtime::status::{ACTIVE, EXITING, INACTIVE, JAILED, PENDING, REGISTERED, UNBONDING};

/// A validator together with its independently stored projections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatorState {
    address: Address,
    lifecycle: ValidatorLifecycle,
    stake: StakeProjection,
    registry_index: Option<NonZeroU64>,
    consensus_pubkey: Option<[u8; 48]>,
    p2p: Option<P2pInfo>,
    history: Option<ValidatorHistory>,
}

impl ValidatorState {
    /// Hydrates an address that has stake but no validator registry identity.
    pub(crate) fn hydrate_unregistered(address: Address, stake: StakeProjection) -> Result<Self> {
        let state = Self {
            address,
            lifecycle: ValidatorLifecycle::Unregistered,
            stake,
            registry_index: None,
            consensus_pubkey: None,
            p2p: None,
            history: None,
        };
        state.validate()?;
        Ok(state)
    }

    /// Hydrates a registered validator from already decoded, typed parts.
    /// Raw slot reads and zero-value validation stay in the runtime storage
    /// adapter instead of introducing a second flat validator representation.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn hydrate_registered(
        address: Address,
        registry_index: NonZeroU64,
        consensus_pubkey: [u8; 48],
        stake: StakeProjection,
        lifecycle: ValidatorLifecycle,
        p2p: Option<P2pInfo>,
        history: ValidatorHistory,
    ) -> Result<Self> {
        let state = Self {
            address,
            lifecycle,
            stake,
            registry_index: Some(registry_index),
            consensus_pubkey: Some(consensus_pubkey),
            p2p,
            history: Some(history),
        };
        state.validate()?;
        Ok(state)
    }

    pub const fn address(&self) -> Address {
        self.address
    }

    pub const fn lifecycle(&self) -> &ValidatorLifecycle {
        &self.lifecycle
    }

    pub const fn stake(&self) -> &StakeProjection {
        &self.stake
    }

    pub const fn registry_index(&self) -> Option<NonZeroU64> {
        self.registry_index
    }

    pub fn consensus_pubkey(&self) -> Option<&[u8; 48]> {
        self.consensus_pubkey.as_ref()
    }

    pub fn p2p(&self) -> Option<&P2pInfo> {
        self.p2p.as_ref()
    }

    pub fn history(&self) -> Option<&ValidatorHistory> {
        self.history.as_ref()
    }

    /// ABI-compatible effective share flag, including an explicit divergent
    /// lifecycle wrapper when the persisted flag outlives its normal phase.
    pub fn has_bls_share(&self) -> bool {
        self.lifecycle.has_share()
    }

    /// ABI-compatible effective readiness flag, including S-01/D-03 lifecycle.
    pub fn join_confirmed(&self) -> bool {
        self.lifecycle.join_confirmed()
    }

    pub(crate) fn stored_status(&self) -> u8 {
        self.lifecycle.stored_status().unwrap_or(REGISTERED)
    }

    pub(crate) fn stored_jailed_at(&self) -> u64 {
        self.lifecycle.stored_jailed_at()
    }

    pub const fn is_registered(&self) -> bool {
        self.registry_index.is_some()
    }

    pub(crate) fn register(
        mut self,
        registry_index: u64,
        consensus_pubkey: [u8; 48],
        joined_at_height: u64,
    ) -> Result<Self> {
        if !matches!(self.lifecycle, ValidatorLifecycle::Unregistered) {
            return Err(PrecompileError::Fatal(
                "validator registration transition requires Unregistered".into(),
            ));
        }
        self.registry_index = Some(NonZeroU64::new(registry_index).ok_or_else(|| {
            PrecompileError::Fatal("validator registry index must be non-zero".into())
        })?);
        self.consensus_pubkey = Some(consensus_pubkey);
        self.lifecycle = ValidatorLifecycle::Registered;
        self.p2p = None;
        self.history = Some(ValidatorHistory::new(joined_at_height, None, 0, 0, 0, 0));
        Ok(self)
    }

    /// Rebuilds the lifecycle-owned portion of an INACTIVE registry entry and
    /// clears stale readiness/share residue for the new admission attempt.
    pub(crate) fn reregister(
        mut self,
        consensus_pubkey: [u8; 48],
        joined_at_height: u64,
    ) -> Result<Self> {
        if !matches!(self.lifecycle.phase(), ValidatorLifecycle::Inactive) {
            return Err(PrecompileError::Fatal(
                "validator re-registration transition requires Inactive".into(),
            ));
        }
        self.consensus_pubkey = Some(consensus_pubkey);
        self.lifecycle = self
            .lifecycle
            .replace_phase(ValidatorLifecycle::Registered)
            .without_readiness_residue()
            .without_share_residue();
        self.stake = StakeProjection::new(self.stake.bonded, None);
        self.p2p = None;
        self.history = Some(ValidatorHistory::new(joined_at_height, None, 0, 0, 0, 0));
        Ok(self)
    }

    pub(crate) fn with_lifecycle(mut self, lifecycle: ValidatorLifecycle) -> Self {
        self.lifecycle = lifecycle;
        self
    }

    pub(crate) fn with_stake_projection(mut self, stake: StakeProjection) -> Self {
        self.stake = stake;
        self
    }

    pub(crate) fn with_history(mut self, history: ValidatorHistory) -> Self {
        self.history = Some(history);
        self
    }

    pub(crate) fn with_p2p(mut self, p2p: Option<P2pInfo>) -> Self {
        self.p2p = p2p;
        self
    }

    /// Checks registry coupling and the lifecycle's own nested invariants.
    pub(crate) fn validate(&self) -> Result<()> {
        self.lifecycle.validate(self.address)?;
        let unregistered = matches!(self.lifecycle.phase(), ValidatorLifecycle::Unregistered);
        let has_registry_bundle = self.registry_index.is_some()
            && self.consensus_pubkey.is_some()
            && self.history.is_some();

        if unregistered {
            if self.registry_index.is_some()
                || self.consensus_pubkey.is_some()
                || self.p2p.is_some()
                || self.history.is_some()
            {
                return Err(corrupt_state(
                    self.address,
                    "Unregistered lifecycle retains registry-owned data",
                ));
            }
        } else if !has_registry_bundle {
            return Err(corrupt_state(
                self.address,
                "registered lifecycle is missing index, consensus key, or history",
            ));
        }

        Ok(())
    }
}

/// Stake data mirrored into ValidatorSet storage.
///
/// This projection is kept outside `ValidatorLifecycle` because Staking can
/// accept stake for an address before that address is registered as a validator.
/// Staking remains the authoritative source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StakeProjection {
    bonded: U256,
    unbonding_end_hint: Option<u64>,
}

impl StakeProjection {
    pub const fn new(bonded: U256, unbonding_end_hint: Option<u64>) -> Self {
        Self {
            bonded,
            unbonding_end_hint,
        }
    }

    pub const fn bonded(&self) -> U256 {
        self.bonded
    }

    pub const fn unbonding_end_hint(&self) -> Option<u64> {
        self.unbonding_end_hint
    }
}

/// Versioned, validated consensus P2P information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum P2pInfo {
    V1(P2pAddress),
}

impl P2pInfo {
    pub(crate) fn decode_stored(
        validator: Address,
        version: u8,
        payload: &[u8],
    ) -> Result<Option<Self>> {
        if version == 0 && payload.is_empty() {
            return Ok(None);
        }
        let decoded = decode_versioned(version, payload).map_err(|error| {
            corrupt_state(validator, format!("invalid stored p2p address: {error}"))
        })?;
        match version {
            P2P_ADDRESS_VERSION_V1 => Ok(Some(Self::V1(decoded))),
            unknown => Err(corrupt_state(
                validator,
                format!("unknown stored p2p address version {unknown}"),
            )),
        }
    }

    pub const fn version(&self) -> u8 {
        match self {
            Self::V1(_) => P2P_ADDRESS_VERSION_V1,
        }
    }

    pub(crate) fn encode_stored(&self) -> Vec<u8> {
        match self {
            Self::V1(address) => encode_v1(address),
        }
    }

    pub const fn address(&self) -> &P2pAddress {
        match self {
            Self::V1(address) => address,
        }
    }
}

/// Historical counters and lifecycle heights retained for a registry entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValidatorHistory {
    joined_at_height: u64,
    last_deactivated_at_height: Option<u64>,
    slash_count: u64,
    missed_blocks: u64,
    missed_votes: u64,
    blocks_proposed: u64,
}

impl ValidatorHistory {
    pub const fn new(
        joined_at_height: u64,
        last_deactivated_at_height: Option<u64>,
        slash_count: u64,
        missed_blocks: u64,
        missed_votes: u64,
        blocks_proposed: u64,
    ) -> Self {
        Self {
            joined_at_height,
            last_deactivated_at_height,
            slash_count,
            missed_blocks,
            missed_votes,
            blocks_proposed,
        }
    }

    pub const fn joined_at_height(&self) -> u64 {
        self.joined_at_height
    }

    pub const fn last_deactivated_at_height(&self) -> Option<u64> {
        self.last_deactivated_at_height
    }

    pub const fn slash_count(&self) -> u64 {
        self.slash_count
    }

    pub const fn missed_blocks(&self) -> u64 {
        self.missed_blocks
    }

    pub const fn missed_votes(&self) -> u64 {
        self.missed_votes
    }

    pub const fn blocks_proposed(&self) -> u64 {
        self.blocks_proposed
    }

    pub const fn with_last_deactivated_at_height(
        mut self,
        last_deactivated_at_height: Option<u64>,
    ) -> Self {
        self.last_deactivated_at_height = last_deactivated_at_height;
        self
    }
}

/// Top-level validator registry lifecycle.
/// Complete validator lifecycle, including recognized persisted states where a
/// coupled field has survived outside the phase that normally owns it.
///
/// Discrepancy variants wrap the underlying phase instead of living in a
/// parallel collection on [`ValidatorState`]. They may be combined, but valid
/// aggregates use one canonical outer-to-inner order: jail height, share, then
/// readiness. Storage decoding and transition helpers always normalize to that
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidatorLifecycle {
    Unregistered,
    Registered,
    Pending(PendingState),
    Active(ActiveState),
    Exiting(ExitingState),
    Unbonding,
    Inactive,
    Jailed(JailedState),

    /// S-01/D-03: the persisted readiness flag survived outside PENDING.
    ReadinessOutsidePending(Box<ValidatorLifecycle>),

    /// A persisted share survived outside a phase that normally owns one.
    ShareOutsideCommitteeLifecycle(Box<ValidatorLifecycle>),

    /// D-06 or propagated residue: jail height survived outside JAILED.
    JailHeightOutsideJailed {
        lifecycle: Box<ValidatorLifecycle>,
        jailed_at: u64,
    },
}

impl ValidatorLifecycle {
    /// Validates a persisted status byte without hydrating any other field.
    pub(crate) fn validate_stored_status(status: u8) -> Result<()> {
        if status <= JAILED {
            Ok(())
        } else {
            Err(PrecompileError::Fatal(format!(
                "unknown validator status {status}"
            )))
        }
    }

    /// Decodes only the lifecycle-related storage fields for hot authorization
    /// and membership reads that do not need the fully hydrated aggregate.
    pub(crate) fn decode_stored(
        status: u8,
        has_bls_share: bool,
        join_confirmed: bool,
        jailed_at: u64,
    ) -> Result<Self> {
        Self::validate_stored_status(status)?;
        let mut lifecycle = decode_lifecycle(status, has_bls_share, join_confirmed, jailed_at)?;
        if join_confirmed && !matches!(lifecycle.phase(), Self::Pending(_)) {
            lifecycle = lifecycle.with_readiness_residue();
        }
        if has_bls_share
            && matches!(
                lifecycle.phase(),
                Self::Registered | Self::Unbonding | Self::Inactive
            )
        {
            lifecycle = lifecycle.with_share_residue();
        }
        if jailed_at != 0 && !matches!(lifecycle.phase(), Self::Jailed(_)) {
            lifecycle = lifecycle.with_jail_height_residue(jailed_at);
        }
        Ok(lifecycle)
    }

    /// Underlying persisted phase, stripping only explicit discrepancy wrappers.
    pub fn phase(&self) -> &Self {
        let mut current = self;
        loop {
            current = match current {
                Self::ReadinessOutsidePending(lifecycle)
                | Self::ShareOutsideCommitteeLifecycle(lifecycle) => lifecycle,
                Self::JailHeightOutsideJailed { lifecycle, .. } => lifecycle,
                phase => return phase,
            };
        }
    }

    /// Persisted status for registered lifecycle states. `Unregistered` is
    /// derived from the registry index and deliberately has no status value.
    pub fn stored_status(&self) -> Option<u8> {
        match self.phase() {
            Self::Unregistered => None,
            Self::Registered => Some(REGISTERED),
            Self::Pending(_) => Some(PENDING),
            Self::Active(_) => Some(ACTIVE),
            Self::Exiting(_) => Some(EXITING),
            Self::Unbonding => Some(UNBONDING),
            Self::Inactive => Some(INACTIVE),
            Self::Jailed(_) => Some(JAILED),
            Self::ReadinessOutsidePending(_)
            | Self::ShareOutsideCommitteeLifecycle(_)
            | Self::JailHeightOutsideJailed { .. } => unreachable!(),
        }
    }

    pub fn has_share(&self) -> bool {
        match self {
            Self::ShareOutsideCommitteeLifecycle(_) => true,
            Self::ReadinessOutsidePending(lifecycle)
            | Self::JailHeightOutsideJailed { lifecycle, .. } => lifecycle.has_share(),
            Self::Pending(state) => state.has_share(),
            Self::Active(state) => state.has_share(),
            Self::Exiting(state) => state.has_share(),
            Self::Jailed(state) => state.has_share(),
            Self::Unregistered | Self::Registered | Self::Unbonding | Self::Inactive => false,
        }
    }

    pub fn join_confirmed(&self) -> bool {
        match self {
            Self::ReadinessOutsidePending(_) => true,
            Self::ShareOutsideCommitteeLifecycle(lifecycle)
            | Self::JailHeightOutsideJailed { lifecycle, .. } => lifecycle.join_confirmed(),
            Self::Pending(state) => state.join_confirmed(),
            _ => false,
        }
    }

    pub fn stored_jailed_at(&self) -> u64 {
        match self {
            Self::JailHeightOutsideJailed { jailed_at, .. } => *jailed_at,
            Self::ReadinessOutsidePending(lifecycle)
            | Self::ShareOutsideCommitteeLifecycle(lifecycle) => lifecycle.stored_jailed_at(),
            Self::Jailed(state) => state.jailed_at(),
            _ => 0,
        }
    }

    pub fn is_active_status(&self) -> bool {
        matches!(self.phase(), Self::Active(_))
    }

    pub fn is_pending(&self) -> bool {
        matches!(self.phase(), Self::Pending(_))
    }

    pub fn is_registered_status(&self) -> bool {
        matches!(self.phase(), Self::Registered)
    }

    /// Whether the validator is cryptographically retained in the live
    /// committee and therefore remains accountable in the current epoch.
    pub fn is_current_consensus_participant(&self) -> bool {
        matches!(
            self.phase(),
            Self::Active(ActiveState::Participating)
                | Self::Exiting(ExitingState::RetainedInCurrentCommittee)
                | Self::Jailed(JailedState::RetainedInCurrentCommittee { .. })
        )
    }

    /// Whether the validator belongs in the next DKG reshare target.
    pub fn is_reshare_target(&self) -> bool {
        matches!(
            self.phase(),
            Self::Active(_)
                | Self::Pending(PendingState::Confirmed | PendingState::ConfirmedWithRetainedShare)
        )
    }

    /// Whether the validator is admitted as a syncing, non-voting secondary.
    pub fn is_secondary_admission(&self) -> bool {
        matches!(
            self.phase(),
            Self::Registered | Self::Pending(_) | Self::Jailed(_)
        )
    }

    pub(crate) fn replace_phase(self, phase: Self) -> Self {
        let (_, readiness, share, jailed_at) = self.into_parts();
        Self::from_parts(phase.into_phase(), readiness, share, jailed_at)
    }

    pub(crate) fn with_readiness_residue(self) -> Self {
        let (phase, _, share, jailed_at) = self.into_parts();
        Self::from_parts(phase, true, share, jailed_at)
    }

    pub(crate) fn without_readiness_residue(self) -> Self {
        let (phase, _, share, jailed_at) = self.into_parts();
        Self::from_parts(phase, false, share, jailed_at)
    }

    pub(crate) fn with_share_residue(self) -> Self {
        let (phase, readiness, _, jailed_at) = self.into_parts();
        Self::from_parts(phase, readiness, true, jailed_at)
    }

    pub(crate) fn without_share_residue(self) -> Self {
        let (phase, readiness, _, jailed_at) = self.into_parts();
        Self::from_parts(phase, readiness, false, jailed_at)
    }

    pub(crate) fn with_jail_height_residue(self, jailed_at: u64) -> Self {
        let (phase, readiness, share, _) = self.into_parts();
        Self::from_parts(phase, readiness, share, Some(jailed_at))
    }

    pub(crate) fn without_jail_height_residue(self) -> Self {
        let (phase, readiness, share, _) = self.into_parts();
        Self::from_parts(phase, readiness, share, None)
    }

    fn into_phase(self) -> Self {
        self.into_parts().0
    }

    fn into_parts(self) -> (Self, bool, bool, Option<u64>) {
        match self {
            Self::ReadinessOutsidePending(lifecycle) => {
                let (phase, _, share, jailed_at) = lifecycle.into_parts();
                (phase, true, share, jailed_at)
            }
            Self::ShareOutsideCommitteeLifecycle(lifecycle) => {
                let (phase, readiness, _, jailed_at) = lifecycle.into_parts();
                (phase, readiness, true, jailed_at)
            }
            Self::JailHeightOutsideJailed {
                lifecycle,
                jailed_at,
            } => {
                let (phase, readiness, share, _) = lifecycle.into_parts();
                (phase, readiness, share, Some(jailed_at))
            }
            phase => (phase, false, false, None),
        }
    }

    fn from_parts(phase: Self, readiness: bool, share: bool, jailed_at: Option<u64>) -> Self {
        let mut lifecycle = phase;
        if readiness {
            lifecycle = Self::ReadinessOutsidePending(Box::new(lifecycle));
        }
        if share {
            lifecycle = Self::ShareOutsideCommitteeLifecycle(Box::new(lifecycle));
        }
        if let Some(jailed_at) = jailed_at {
            lifecycle = Self::JailHeightOutsideJailed {
                lifecycle: Box::new(lifecycle),
                jailed_at,
            };
        }
        lifecycle
    }

    fn validate(&self, address: Address) -> Result<()> {
        let mut phase = self;
        let mut readiness = false;
        let mut share = false;
        let mut jail_height = None;
        let mut previous_wrapper_rank = None;
        loop {
            phase = match phase {
                Self::ReadinessOutsidePending(lifecycle) => {
                    let rank = 2;
                    if previous_wrapper_rank.is_some_and(|previous| rank <= previous) {
                        return Err(corrupt_state(
                            address,
                            "non-canonical lifecycle discrepancy nesting",
                        ));
                    }
                    previous_wrapper_rank = Some(rank);
                    if readiness {
                        return Err(corrupt_state(address, "duplicate readiness lifecycle"));
                    }
                    readiness = true;
                    lifecycle
                }
                Self::ShareOutsideCommitteeLifecycle(lifecycle) => {
                    let rank = 1;
                    if previous_wrapper_rank.is_some_and(|previous| rank <= previous) {
                        return Err(corrupt_state(
                            address,
                            "non-canonical lifecycle discrepancy nesting",
                        ));
                    }
                    previous_wrapper_rank = Some(rank);
                    if share {
                        return Err(corrupt_state(address, "duplicate share lifecycle"));
                    }
                    share = true;
                    lifecycle
                }
                Self::JailHeightOutsideJailed {
                    lifecycle,
                    jailed_at,
                } => {
                    let rank = 0;
                    if previous_wrapper_rank.is_some_and(|previous| rank <= previous) {
                        return Err(corrupt_state(
                            address,
                            "non-canonical lifecycle discrepancy nesting",
                        ));
                    }
                    previous_wrapper_rank = Some(rank);
                    if jail_height.is_some() || *jailed_at == 0 {
                        return Err(corrupt_state(address, "invalid jail-height lifecycle"));
                    }
                    jail_height = Some(*jailed_at);
                    lifecycle
                }
                value => value,
            };
            if !matches!(
                phase,
                Self::ReadinessOutsidePending(_)
                    | Self::ShareOutsideCommitteeLifecycle(_)
                    | Self::JailHeightOutsideJailed { .. }
            ) {
                break;
            }
        }

        if matches!(phase, Self::Unregistered) && (readiness || share || jail_height.is_some()) {
            return Err(corrupt_state(
                address,
                "Unregistered lifecycle contains persisted discrepancy state",
            ));
        }
        if readiness && matches!(phase, Self::Pending(_)) {
            return Err(corrupt_state(
                address,
                "pending readiness must be represented by PendingState",
            ));
        }
        if share
            && matches!(
                phase,
                Self::Pending(_) | Self::Active(_) | Self::Exiting(_) | Self::Jailed(_)
            )
        {
            return Err(corrupt_state(
                address,
                "committee share must be represented by the phase substate",
            ));
        }
        if jail_height.is_some() && matches!(phase, Self::Jailed(_)) {
            return Err(corrupt_state(
                address,
                "jailed height must be represented by JailedState",
            ));
        }
        Ok(())
    }
}

/// A staked validator awaiting first (or repaired) committee activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingState {
    AwaitingConfirmation,
    Confirmed,
    AwaitingConfirmationWithRetainedShare,
    ConfirmedWithRetainedShare,
}

impl PendingState {
    pub const fn confirm(self) -> Self {
        match self {
            Self::AwaitingConfirmation => Self::Confirmed,
            Self::AwaitingConfirmationWithRetainedShare => Self::ConfirmedWithRetainedShare,
            Self::Confirmed | Self::ConfirmedWithRetainedShare => self,
        }
    }

    pub const fn activate_at_boundary(self) -> ActiveState {
        let _ = self;
        ActiveState::Participating
    }

    pub const fn has_share(&self) -> bool {
        matches!(
            self,
            Self::AwaitingConfirmationWithRetainedShare | Self::ConfirmedWithRetainedShare
        )
    }

    pub const fn join_confirmed(&self) -> bool {
        matches!(self, Self::Confirmed | Self::ConfirmedWithRetainedShare)
    }
}

/// An ACTIVE validator's current committee-share state.
///
/// Transitions consume the source substate, so stale state cannot be reused
/// without an explicit clone:
///
/// ```compile_fail
/// use outbe_validatorset::ActiveState;
///
/// let active = ActiveState::Participating;
/// let _exiting = active.begin_exit();
/// let _stale_transition = active.begin_exit();
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveState {
    Participating,
    AwaitingShareRepair,
}

impl ActiveState {
    pub const fn included_at_boundary(self) -> Self {
        let _ = self;
        Self::Participating
    }

    pub const fn omitted_at_boundary(self) -> Self {
        let _ = self;
        Self::AwaitingShareRepair
    }

    pub const fn begin_exit(self) -> ExitingState {
        match self {
            Self::Participating => ExitingState::RetainedInCurrentCommittee,
            Self::AwaitingShareRepair => ExitingState::AlreadyExcluded,
        }
    }

    pub const fn jail(self, jailed_at: u64) -> JailedState {
        match self {
            Self::Participating => JailedState::RetainedInCurrentCommittee { jailed_at },
            Self::AwaitingShareRepair => JailedState::Excluded { jailed_at },
        }
    }

    pub const fn has_share(&self) -> bool {
        matches!(self, Self::Participating)
    }
}

/// A validator leaving the registry through the next committee boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExitingState {
    RetainedInCurrentCommittee,
    AlreadyExcluded,
}

impl ExitingState {
    pub const fn excluded_at_boundary(self) -> ValidatorLifecycle {
        let _ = self;
        ValidatorLifecycle::Unbonding
    }

    pub const fn has_share(&self) -> bool {
        matches!(self, Self::RetainedInCurrentCommittee)
    }
}

/// A jailed validator, either still retained by the live committee or excluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JailedState {
    RetainedInCurrentCommittee { jailed_at: u64 },
    Excluded { jailed_at: u64 },
}

impl JailedState {
    pub const fn excluded_at_boundary(self) -> Self {
        Self::Excluded {
            jailed_at: self.jailed_at(),
        }
    }

    pub const fn unjail(self) -> PendingState {
        match self {
            Self::RetainedInCurrentCommittee { .. } => {
                PendingState::AwaitingConfirmationWithRetainedShare
            }
            Self::Excluded { .. } => PendingState::AwaitingConfirmation,
        }
    }

    pub const fn leave_below_minimum(self) -> ExitingState {
        match self {
            Self::RetainedInCurrentCommittee { .. } => ExitingState::RetainedInCurrentCommittee,
            Self::Excluded { .. } => ExitingState::AlreadyExcluded,
        }
    }

    pub const fn has_share(&self) -> bool {
        matches!(self, Self::RetainedInCurrentCommittee { .. })
    }

    pub const fn jailed_at(&self) -> u64 {
        match self {
            Self::RetainedInCurrentCommittee { jailed_at } | Self::Excluded { jailed_at } => {
                *jailed_at
            }
        }
    }
}

fn decode_lifecycle(
    status: u8,
    has_bls_share: bool,
    join_confirmed: bool,
    jailed_at: u64,
) -> Result<ValidatorLifecycle> {
    let lifecycle = match status {
        REGISTERED => ValidatorLifecycle::Registered,
        PENDING => ValidatorLifecycle::Pending(match (join_confirmed, has_bls_share) {
            (false, false) => PendingState::AwaitingConfirmation,
            (true, false) => PendingState::Confirmed,
            (false, true) => PendingState::AwaitingConfirmationWithRetainedShare,
            (true, true) => PendingState::ConfirmedWithRetainedShare,
        }),
        ACTIVE => ValidatorLifecycle::Active(if has_bls_share {
            ActiveState::Participating
        } else {
            ActiveState::AwaitingShareRepair
        }),
        EXITING => ValidatorLifecycle::Exiting(if has_bls_share {
            ExitingState::RetainedInCurrentCommittee
        } else {
            ExitingState::AlreadyExcluded
        }),
        UNBONDING => ValidatorLifecycle::Unbonding,
        INACTIVE => ValidatorLifecycle::Inactive,
        JAILED => ValidatorLifecycle::Jailed(if has_bls_share {
            JailedState::RetainedInCurrentCommittee { jailed_at }
        } else {
            JailedState::Excluded { jailed_at }
        }),
        unknown => {
            return Err(PrecompileError::Fatal(format!(
                "unknown validator status {unknown}"
            )));
        }
    };
    Ok(lifecycle)
}

fn corrupt_state(address: Address, detail: impl std::fmt::Display) -> PrecompileError {
    PrecompileError::Fatal(format!("corrupt validator state for {address}: {detail}"))
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    use super::*;

    fn address() -> Address {
        Address::repeat_byte(0x11)
    }

    fn history() -> ValidatorHistory {
        ValidatorHistory::new(13, Some(21), 2, 3, 5, 8)
    }

    fn registered_state(lifecycle: ValidatorLifecycle) -> ValidatorState {
        ValidatorState::hydrate_registered(
            address(),
            NonZeroU64::new(3).unwrap(),
            [7; 48],
            StakeProjection::new(U256::from(1_500), Some(34)),
            lifecycle,
            None,
            history(),
        )
        .unwrap()
    }

    fn decoded_state(
        status: u8,
        has_bls_share: bool,
        join_confirmed: bool,
        jailed_at: u64,
    ) -> ValidatorState {
        registered_state(
            ValidatorLifecycle::decode_stored(status, has_bls_share, join_confirmed, jailed_at)
                .unwrap(),
        )
    }

    #[test]
    fn unregistered_address_can_have_pre_registration_stake_projection() {
        let state = ValidatorState::hydrate_unregistered(
            address(),
            StakeProjection::new(U256::from(900), Some(55)),
        )
        .unwrap();

        assert_eq!(state.lifecycle(), &ValidatorLifecycle::Unregistered);
        assert_eq!(state.stake().bonded(), U256::from(900));
        assert_eq!(state.stake().unbonding_end_hint(), Some(55));
        assert!(!state.is_registered());
        assert_eq!(state.consensus_pubkey(), None);
        assert_eq!(state.history(), None);
    }

    #[test]
    fn unknown_status_bytes_fail_closed() {
        for status in 7..=u8::MAX {
            assert!(matches!(
                ValidatorLifecycle::decode_stored(status, false, false, 0),
                Err(PrecompileError::Fatal(_))
            ));
        }
    }

    #[test]
    fn every_known_lifecycle_bundle_decodes_and_round_trips() {
        let cases = vec![
            (REGISTERED, false, false, 0, ValidatorLifecycle::Registered),
            (
                PENDING,
                false,
                false,
                0,
                ValidatorLifecycle::Pending(PendingState::AwaitingConfirmation),
            ),
            (
                PENDING,
                false,
                true,
                0,
                ValidatorLifecycle::Pending(PendingState::Confirmed),
            ),
            (
                PENDING,
                true,
                false,
                0,
                ValidatorLifecycle::Pending(PendingState::AwaitingConfirmationWithRetainedShare),
            ),
            (
                PENDING,
                true,
                true,
                0,
                ValidatorLifecycle::Pending(PendingState::ConfirmedWithRetainedShare),
            ),
            (
                ACTIVE,
                false,
                false,
                0,
                ValidatorLifecycle::Active(ActiveState::AwaitingShareRepair),
            ),
            (
                ACTIVE,
                true,
                false,
                0,
                ValidatorLifecycle::Active(ActiveState::Participating),
            ),
            (
                EXITING,
                false,
                false,
                0,
                ValidatorLifecycle::Exiting(ExitingState::AlreadyExcluded),
            ),
            (
                EXITING,
                true,
                false,
                0,
                ValidatorLifecycle::Exiting(ExitingState::RetainedInCurrentCommittee),
            ),
            (UNBONDING, false, false, 0, ValidatorLifecycle::Unbonding),
            (INACTIVE, false, false, 0, ValidatorLifecycle::Inactive),
            (
                JAILED,
                false,
                false,
                55,
                ValidatorLifecycle::Jailed(JailedState::Excluded { jailed_at: 55 }),
            ),
            (
                JAILED,
                true,
                false,
                55,
                ValidatorLifecycle::Jailed(JailedState::RetainedInCurrentCommittee {
                    jailed_at: 55,
                }),
            ),
        ];

        for (status, has_share, join_confirmed, jailed_at, expected) in cases {
            let state = decoded_state(status, has_share, join_confirmed, jailed_at);
            assert_eq!(state.lifecycle(), &expected);
            assert_eq!(state.stored_status(), status);
            assert_eq!(state.has_bls_share(), has_share);
            assert_eq!(state.join_confirmed(), join_confirmed);
            assert_eq!(state.stored_jailed_at(), jailed_at);
        }
    }

    #[test]
    fn lean_lifecycle_decoder_matches_typed_substates_and_fails_closed() {
        assert_eq!(
            ValidatorLifecycle::decode_stored(ACTIVE, false, true, 12).unwrap(),
            ValidatorLifecycle::JailHeightOutsideJailed {
                lifecycle: Box::new(ValidatorLifecycle::ReadinessOutsidePending(Box::new(
                    ValidatorLifecycle::Active(ActiveState::AwaitingShareRepair),
                ))),
                jailed_at: 12,
            }
        );
        assert_eq!(
            ValidatorLifecycle::decode_stored(PENDING, true, true, 0).unwrap(),
            ValidatorLifecycle::Pending(PendingState::ConfirmedWithRetainedShare)
        );
        assert!(matches!(
            ValidatorLifecycle::decode_stored(99, false, false, 0),
            Err(PrecompileError::Fatal(_))
        ));
    }

    #[test]
    fn pending_substates_decode_and_round_trip_every_flag_pair() {
        let cases = [
            (false, false, PendingState::AwaitingConfirmation),
            (true, false, PendingState::Confirmed),
            (
                false,
                true,
                PendingState::AwaitingConfirmationWithRetainedShare,
            ),
            (true, true, PendingState::ConfirmedWithRetainedShare),
        ];

        for (join_confirmed, has_share, expected) in cases {
            let state = decoded_state(PENDING, has_share, join_confirmed, 0);

            assert_eq!(state.lifecycle(), &ValidatorLifecycle::Pending(expected));
            assert_eq!(state.has_bls_share(), has_share);
            assert_eq!(state.join_confirmed(), join_confirmed);
        }
    }

    #[test]
    fn active_without_share_is_valid_repair_state() {
        let state = decoded_state(ACTIVE, false, false, 0);

        assert_eq!(
            state.lifecycle(),
            &ValidatorLifecycle::Active(ActiveState::AwaitingShareRepair)
        );
        assert!(!state.has_bls_share());
    }

    #[test]
    fn readiness_residue_outside_pending_is_preserved() {
        for status in [REGISTERED, ACTIVE] {
            let state = decoded_state(status, false, true, 0);

            assert!(matches!(
                state.lifecycle(),
                ValidatorLifecycle::ReadinessOutsidePending(_)
            ));
            assert!(state.join_confirmed());
        }
    }

    #[test]
    fn all_known_non_pending_residues_round_trip() {
        let state = decoded_state(UNBONDING, true, true, 99);

        assert_eq!(
            state.lifecycle(),
            &ValidatorLifecycle::JailHeightOutsideJailed {
                lifecycle: Box::new(ValidatorLifecycle::ShareOutsideCommitteeLifecycle(
                    Box::new(ValidatorLifecycle::ReadinessOutsidePending(Box::new(
                        ValidatorLifecycle::Unbonding,
                    ))),
                )),
                jailed_at: 99,
            }
        );
        assert!(state.has_bls_share());
        assert!(state.join_confirmed());
        assert_eq!(state.stored_jailed_at(), 99);
    }

    #[test]
    fn p2p_is_validated_and_round_trips() {
        let socket = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 30_400);
        let address_v1 = P2pAddress::Symmetric(socket);
        let encoded = encode_v1(&address_v1);

        let p2p = P2pInfo::decode_stored(address(), P2P_ADDRESS_VERSION_V1, &encoded)
            .unwrap()
            .unwrap();
        assert_eq!(p2p, P2pInfo::V1(address_v1));
        assert_eq!(p2p.encode_stored(), encoded);

        assert!(matches!(
            P2pInfo::decode_stored(address(), P2P_ADDRESS_VERSION_V1, &[255]),
            Err(PrecompileError::Fatal(_))
        ));

        assert!(matches!(
            P2pInfo::decode_stored(address(), 0, &[0]),
            Err(PrecompileError::Fatal(_))
        ));
    }

    #[test]
    fn transitions_preserve_share_sensitive_substates() {
        assert_eq!(
            PendingState::AwaitingConfirmationWithRetainedShare.confirm(),
            PendingState::ConfirmedWithRetainedShare
        );
        assert_eq!(
            PendingState::Confirmed.activate_at_boundary(),
            ActiveState::Participating
        );
        assert_eq!(
            ActiveState::Participating.omitted_at_boundary(),
            ActiveState::AwaitingShareRepair
        );
        assert_eq!(
            ActiveState::AwaitingShareRepair.included_at_boundary(),
            ActiveState::Participating
        );
        assert_eq!(
            ActiveState::Participating.begin_exit(),
            ExitingState::RetainedInCurrentCommittee
        );
        assert_eq!(
            ActiveState::AwaitingShareRepair.begin_exit(),
            ExitingState::AlreadyExcluded
        );
        assert_eq!(
            ExitingState::RetainedInCurrentCommittee.excluded_at_boundary(),
            ValidatorLifecycle::Unbonding
        );
    }

    #[test]
    fn every_share_sensitive_transition_branch_is_explicit() {
        assert_eq!(
            ActiveState::Participating.jail(10),
            JailedState::RetainedInCurrentCommittee { jailed_at: 10 }
        );
        assert_eq!(
            ActiveState::AwaitingShareRepair.jail(11),
            JailedState::Excluded { jailed_at: 11 }
        );

        let retained = JailedState::RetainedInCurrentCommittee { jailed_at: 12 };
        assert_eq!(
            retained.clone().excluded_at_boundary(),
            JailedState::Excluded { jailed_at: 12 }
        );
        assert_eq!(
            retained.unjail(),
            PendingState::AwaitingConfirmationWithRetainedShare
        );

        let excluded = JailedState::Excluded { jailed_at: 13 };
        assert_eq!(
            excluded.clone().excluded_at_boundary(),
            JailedState::Excluded { jailed_at: 13 }
        );
        assert_eq!(excluded.unjail(), PendingState::AwaitingConfirmation);

        assert_eq!(
            ExitingState::RetainedInCurrentCommittee.excluded_at_boundary(),
            ValidatorLifecycle::Unbonding
        );
        assert_eq!(
            ExitingState::AlreadyExcluded.excluded_at_boundary(),
            ValidatorLifecycle::Unbonding
        );
    }

    #[test]
    fn aggregate_validation_rejects_registry_lifecycle_mismatch() {
        let registered = registered_state(ValidatorLifecycle::Registered);
        let invalid = registered.with_lifecycle(ValidatorLifecycle::Unregistered);

        assert!(matches!(invalid.validate(), Err(PrecompileError::Fatal(_))));
        assert_eq!(ValidatorLifecycle::Unregistered.stored_status(), None);
    }

    #[test]
    fn lifecycle_predicates_encode_membership_policy() {
        assert!(ValidatorLifecycle::Active(ActiveState::AwaitingShareRepair).is_active_status());
        assert!(
            !ValidatorLifecycle::Active(ActiveState::AwaitingShareRepair)
                .is_current_consensus_participant()
        );
        assert!(ValidatorLifecycle::Active(ActiveState::Participating)
            .is_current_consensus_participant());
        assert!(
            ValidatorLifecycle::Exiting(ExitingState::RetainedInCurrentCommittee)
                .is_current_consensus_participant()
        );
        assert!(
            ValidatorLifecycle::Jailed(JailedState::RetainedInCurrentCommittee { jailed_at: 1 })
                .is_current_consensus_participant()
        );

        assert!(
            !ValidatorLifecycle::Pending(PendingState::AwaitingConfirmation).is_reshare_target()
        );
        assert!(ValidatorLifecycle::Pending(PendingState::Confirmed).is_reshare_target());
        assert!(ValidatorLifecycle::Active(ActiveState::AwaitingShareRepair).is_reshare_target());

        assert!(ValidatorLifecycle::Registered.is_registered_status());
        assert!(ValidatorLifecycle::Pending(PendingState::Confirmed).is_pending());
        assert!(ValidatorLifecycle::Registered.is_secondary_admission());
        assert!(
            ValidatorLifecycle::Pending(PendingState::AwaitingConfirmation)
                .is_secondary_admission()
        );
        assert!(
            ValidatorLifecycle::Jailed(JailedState::Excluded { jailed_at: 1 })
                .is_secondary_admission()
        );
        assert!(!ValidatorLifecycle::Active(ActiveState::Participating).is_secondary_admission());
    }

    #[test]
    fn early_unjail_retains_live_share_in_typed_pending_state() {
        let jailed = ActiveState::Participating.jail(77);
        assert_eq!(
            jailed,
            JailedState::RetainedInCurrentCommittee { jailed_at: 77 }
        );
        assert_eq!(
            jailed.unjail(),
            PendingState::AwaitingConfirmationWithRetainedShare
        );

        let state = decoded_state(PENDING, true, false, 0);
        assert_eq!(
            state.lifecycle(),
            &ValidatorLifecycle::Pending(PendingState::AwaitingConfirmationWithRetainedShare)
        );
        assert!(state.has_bls_share());
    }

    #[test]
    fn jailed_below_minimum_always_enters_exit_path() {
        assert_eq!(
            JailedState::RetainedInCurrentCommittee { jailed_at: 10 }.leave_below_minimum(),
            ExitingState::RetainedInCurrentCommittee
        );
        assert_eq!(
            JailedState::Excluded { jailed_at: 10 }.leave_below_minimum(),
            ExitingState::AlreadyExcluded
        );
    }

    #[test]
    fn aggregate_modifiers_keep_fields_private_and_composable() {
        let state = registered_state(ValidatorLifecycle::Registered);
        let updated = state
            .with_stake_projection(StakeProjection::new(U256::from(2_000), None))
            .with_history(ValidatorHistory::new(1, Some(2), 3, 4, 5, 6))
            .with_p2p(None)
            .with_lifecycle(ValidatorLifecycle::Pending(
                PendingState::AwaitingConfirmation,
            ));

        assert_eq!(updated.stake().bonded(), U256::from(2_000));
        assert_eq!(
            updated.lifecycle(),
            &ValidatorLifecycle::Pending(PendingState::AwaitingConfirmation)
        );
    }

    #[test]
    fn history_accessors_expose_every_stored_counter() {
        let history = history();

        assert_eq!(history.joined_at_height(), 13);
        assert_eq!(history.last_deactivated_at_height(), Some(21));
        assert_eq!(history.slash_count(), 2);
        assert_eq!(history.missed_blocks(), 3);
        assert_eq!(history.missed_votes(), 5);
        assert_eq!(history.blocks_proposed(), 8);
        assert_eq!(
            history
                .with_last_deactivated_at_height(Some(89))
                .last_deactivated_at_height(),
            Some(89)
        );
    }

    #[test]
    fn registration_constructs_identity_while_preserving_pre_registration_stake() {
        let state = ValidatorState::hydrate_unregistered(
            address(),
            StakeProjection::new(U256::from(700), Some(44)),
        )
        .unwrap();

        let registered = state.register(2, [9; 48], 88).unwrap();
        assert_eq!(registered.lifecycle(), &ValidatorLifecycle::Registered);
        assert_eq!(registered.registry_index().map(NonZeroU64::get), Some(2));
        assert_eq!(registered.consensus_pubkey(), Some(&[9; 48]));
        assert_eq!(registered.stake().bonded(), U256::from(700));
        assert_eq!(registered.stake().unbonding_end_hint(), Some(44));
        assert_eq!(registered.history().unwrap().joined_at_height(), 88);
    }

    #[test]
    fn reregistration_makes_d06_residue_explicit_without_repairing_it() {
        let lifecycle = ValidatorLifecycle::decode_stored(INACTIVE, true, true, 55).unwrap();
        let p2p = P2pInfo::V1(P2pAddress::Symmetric(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            30_401,
        )));
        let state = ValidatorState::hydrate_registered(
            address(),
            NonZeroU64::new(3).unwrap(),
            [7; 48],
            StakeProjection::new(U256::from(1_500), Some(77)),
            lifecycle,
            Some(p2p),
            history(),
        )
        .unwrap();

        let registered = state.reregister([8; 48], 99).unwrap();
        assert_eq!(
            registered.lifecycle(),
            &ValidatorLifecycle::JailHeightOutsideJailed {
                lifecycle: Box::new(ValidatorLifecycle::ReadinessOutsidePending(Box::new(
                    ValidatorLifecycle::Registered,
                ))),
                jailed_at: 55,
            }
        );
        assert!(!registered.has_bls_share());
        assert!(registered.join_confirmed());
        assert_eq!(registered.stored_jailed_at(), 55);
        assert_eq!(registered.stake().unbonding_end_hint(), None);
        assert_eq!(registered.p2p(), None);
        assert_eq!(registered.history().unwrap().slash_count(), 0);
    }

    #[test]
    fn discrepancy_wrappers_must_use_the_canonical_nesting_order() {
        let non_canonical = ValidatorLifecycle::ReadinessOutsidePending(Box::new(
            ValidatorLifecycle::ShareOutsideCommitteeLifecycle(Box::new(
                ValidatorLifecycle::Registered,
            )),
        ));
        let duplicate = ValidatorLifecycle::ReadinessOutsidePending(Box::new(
            ValidatorLifecycle::ReadinessOutsidePending(Box::new(ValidatorLifecycle::Registered)),
        ));

        assert!(ValidatorState::hydrate_registered(
            address(),
            NonZeroU64::new(3).unwrap(),
            [7; 48],
            StakeProjection::new(U256::from(1_500), None),
            non_canonical,
            None,
            history(),
        )
        .is_err());
        assert!(ValidatorState::hydrate_registered(
            address(),
            NonZeroU64::new(3).unwrap(),
            [7; 48],
            StakeProjection::new(U256::from(1_500), None),
            duplicate,
            None,
            history(),
        )
        .is_err());
    }
}
