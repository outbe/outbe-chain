use std::num::NonZeroU64;

use alloy_primitives::{Address, U256};
use outbe_primitives::consensus_p2p::{decode_versioned, encode_v1, P2P_ADDRESS_VERSION_V1};
use outbe_primitives::error::{PrecompileError, Result};

use crate::runtime::status::{ACTIVE, EXITING, INACTIVE, JAILED, PENDING, REGISTERED, UNBONDING};

use super::state::{
    Active, ConsensusPubkey, Exiting, Inactive, Jail, JailRetained, Joining, P2pInfo,
    StakeProjection, Unbonding, ValidatorHistory, ValidatorLifecycle, ValidatorState,
    WaitingForReadiness, WaitingForStake,
};

macro_rules! lifecycle_ref {
    ($lifecycle:expr, $field:ident) => {
        match $lifecycle {
            ValidatorLifecycle::Absent => None,
            ValidatorLifecycle::WaitingForStake(state) => Some(&state.$field),
            ValidatorLifecycle::WaitingForReadiness(state) => Some(&state.$field),
            ValidatorLifecycle::Joining(state) => Some(&state.$field),
            ValidatorLifecycle::Active(state) => Some(&state.$field),
            ValidatorLifecycle::JailRetained(state) => Some(&state.$field),
            ValidatorLifecycle::Jail(state) => Some(&state.$field),
            ValidatorLifecycle::Exiting(state) => Some(&state.$field),
            ValidatorLifecycle::Unbonding(state) => Some(&state.$field),
            ValidatorLifecycle::Inactive(state) => Some(&state.$field),
        }
    };
}

impl P2pInfo {
    /// Decodes the two persisted P2P fields as one atomic value.
    pub(crate) fn decode_stored(validator: Address, version: u8, payload: &[u8]) -> Result<Self> {
        if version == 0 && payload.is_empty() {
            return Ok(Self::Unset);
        }
        if version == 0 || payload.is_empty() {
            return Err(corrupt_state(
                validator,
                "P2P version and payload must be both set or both empty",
            ));
        }
        if version != P2P_ADDRESS_VERSION_V1 {
            return Ok(Self::Opaque {
                version,
                payload: payload.to_vec(),
            });
        }
        let address = decode_versioned(version, payload).map_err(|error| {
            corrupt_state(
                validator,
                format_args!("invalid persisted P2P address: {error}"),
            )
        })?;
        Ok(Self::V1(address))
    }

    /// Encodes both persisted P2P fields as one atomic value.
    pub(crate) fn encode_stored(&self) -> (u8, Vec<u8>) {
        match self {
            Self::Unset => (0, Vec::new()),
            Self::V1(address) => (P2P_ADDRESS_VERSION_V1, encode_v1(address)),
            Self::Opaque { version, payload } => (*version, payload.clone()),
        }
    }
}

impl ValidatorState {
    pub const fn absent(address: Address) -> Self {
        Self {
            address,
            lifecycle: ValidatorLifecycle::Absent,
        }
    }

    /// The sole raw-storage-to-type-state adapter.
    ///
    /// Raw status tags remain ABI-compatible, while every coupled-field
    /// combination outside the canonical state machine fails closed. Stake
    /// threshold checks remain at the Staking transition seam because Staking,
    /// not ValidatorSet, owns the authoritative minimum and bonded ledger.
    pub(crate) fn decode_stored(address: Address, fields: StoredValidatorFields) -> Result<Self> {
        validate_hint(address, fields.stake)?;
        let p2p = P2pInfo::decode_stored(address, fields.p2p_version, &fields.p2p_payload)?;

        let Some(registry_index) = NonZeroU64::new(fields.registry_index) else {
            // A decoded `Unset` P2P value is exactly the empty raw pair, so
            // equality with the absent columns covers every owned field.
            if fields != StoredValidatorFields::absent() {
                return Err(corrupt_state(
                    address,
                    "absent validator retains registry, stake, or lifecycle data",
                ));
            }
            return Ok(Self::absent(address));
        };

        let lifecycle = fields.decode_registered(address, registry_index, p2p)?;
        let state = Self { address, lifecycle };
        state.validate()?;
        Ok(state)
    }

    pub const fn address(&self) -> Address {
        self.address
    }

    pub const fn lifecycle(&self) -> &ValidatorLifecycle {
        &self.lifecycle
    }

    pub fn into_lifecycle(self) -> ValidatorLifecycle {
        self.lifecycle
    }

    pub(crate) fn from_lifecycle(address: Address, lifecycle: ValidatorLifecycle) -> Result<Self> {
        let state = Self { address, lifecycle };
        state.validate()?;
        Ok(state)
    }

    pub(crate) fn with_lifecycle(self, lifecycle: ValidatorLifecycle) -> Result<Self> {
        Self::from_lifecycle(self.address, lifecycle)
    }

    pub const fn is_registered(&self) -> bool {
        !matches!(self.lifecycle, ValidatorLifecycle::Absent)
    }

    pub fn registry_index(&self) -> Option<NonZeroU64> {
        self.lifecycle.registry_index()
    }

    pub fn consensus_pubkey(&self) -> Option<&ConsensusPubkey> {
        self.lifecycle.consensus_pubkey()
    }

    pub fn p2p(&self) -> Option<&P2pInfo> {
        self.lifecycle.p2p()
    }

    /// Compatibility accessor for the ValidatorSet stake mirror. `None` is an
    /// encoded zero for `Absent` and `Inactive`.
    pub fn stake(&self) -> Option<&StakeProjection> {
        self.lifecycle.stake()
    }

    pub fn bonded_stake(&self) -> U256 {
        self.stake().map_or(U256::ZERO, StakeProjection::bonded)
    }

    pub fn unbonding_end_hint(&self) -> Option<u64> {
        self.stake().and_then(StakeProjection::unbonding_end_hint)
    }

    pub fn history(&self) -> Option<&ValidatorHistory> {
        self.lifecycle.history()
    }

    pub fn has_bls_share(&self) -> bool {
        self.lifecycle.has_bls_share()
    }

    pub fn join_confirmed(&self) -> bool {
        self.lifecycle.join_confirmed()
    }

    pub fn stored_status(&self) -> Option<u8> {
        self.lifecycle.stored_status()
    }

    pub fn stored_jailed_at(&self) -> u64 {
        self.lifecycle.stored_jailed_at()
    }

    pub(crate) fn validate(&self) -> Result<()> {
        self.lifecycle.validate(self.address)
    }
}

/// The raw persisted columns of one validator, before type-state decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredValidatorFields {
    pub(crate) registry_index: u64,
    pub(crate) consensus_pubkey: ConsensusPubkey,
    pub(crate) stake: StakeProjection,
    pub(crate) stored_status: u8,
    pub(crate) p2p_version: u8,
    pub(crate) p2p_payload: Vec<u8>,
    pub(crate) history: ValidatorHistory,
    pub(crate) has_bls_share: bool,
    pub(crate) join_confirmed: bool,
    pub(crate) jailed_at: u64,
}

impl StoredValidatorFields {
    /// The columns of an address that owns no registry entry.
    fn absent() -> Self {
        Self {
            registry_index: 0,
            consensus_pubkey: [0; 48],
            stake: StakeProjection::zero(),
            stored_status: REGISTERED,
            p2p_version: 0,
            p2p_payload: Vec::new(),
            history: ValidatorHistory::fresh(0),
            has_bls_share: false,
            join_confirmed: false,
            jailed_at: 0,
        }
    }

    /// Decodes the columns of a registered validator into its lifecycle.
    fn decode_registered(
        &self,
        address: Address,
        registry_index: NonZeroU64,
        p2p: P2pInfo,
    ) -> Result<ValidatorLifecycle> {
        ValidatorLifecycle::validate_stored_status(self.stored_status)?;
        if self.consensus_pubkey == [0; 48] {
            return Err(corrupt_state(
                address,
                "registered validator is missing its consensus public key",
            ));
        }
        if self.stored_status != JAILED && self.jailed_at != 0 {
            return Err(corrupt_state(
                address,
                "jail height is present outside a jailed lifecycle",
            ));
        }
        let payload = RegisteredPayload {
            registry_index,
            consensus_pubkey: self.consensus_pubkey,
            p2p,
            stake: self.stake,
            history: self.history,
        };
        match self.coupled_shape() {
            Some(shape) => payload.into_lifecycle(address, shape, self.jailed_at),
            None => Err(corrupt_state(
                address,
                format_args!(
                    "non-canonical coupled fields: status={}, share={}, readiness={}, jailed_at={}",
                    self.stored_status, self.has_bls_share, self.join_confirmed, self.jailed_at
                ),
            )),
        }
    }

    /// The lifecycle that the status byte, the BLS-share flag and the
    /// readiness flag select, or `None` for a non-canonical combination.
    fn coupled_shape(&self) -> Option<CoupledShape> {
        let shape = match (self.stored_status, self.has_bls_share, self.join_confirmed) {
            (REGISTERED, false, false) => CoupledShape::WaitingForStake,
            (PENDING, false, false) => CoupledShape::WaitingForReadiness,
            (PENDING, false, true) => CoupledShape::Joining,
            (ACTIVE, true, false) => CoupledShape::Active,
            (EXITING, true, false) => CoupledShape::Exiting,
            (UNBONDING, false, false) => CoupledShape::Unbonding,
            (INACTIVE, false, false) => CoupledShape::Inactive,
            (JAILED, true, false) => CoupledShape::JailRetained,
            (JAILED, false, false) => CoupledShape::Jail,
            _ => return None,
        };
        Some(shape)
    }
}

/// The canonical lifecycle selected by the coupled raw fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoupledShape {
    WaitingForStake,
    WaitingForReadiness,
    Joining,
    Active,
    Exiting,
    Unbonding,
    Inactive,
    JailRetained,
    Jail,
}

/// The decoded fields that every registered lifecycle payload carries.
struct RegisteredPayload {
    registry_index: NonZeroU64,
    consensus_pubkey: ConsensusPubkey,
    p2p: P2pInfo,
    stake: StakeProjection,
    history: ValidatorHistory,
}

macro_rules! registered_payload {
    ($payload:expr, $name:ident) => {
        $name {
            registry_index: $payload.registry_index,
            consensus_pubkey: $payload.consensus_pubkey,
            p2p: $payload.p2p,
            stake: $payload.stake,
            history: $payload.history,
        }
    };
    ($payload:expr, $name:ident, $jailed_at:expr) => {
        $name {
            registry_index: $payload.registry_index,
            consensus_pubkey: $payload.consensus_pubkey,
            p2p: $payload.p2p,
            stake: $payload.stake,
            history: $payload.history,
            jailed_at: $jailed_at,
        }
    };
}

impl RegisteredPayload {
    fn into_lifecycle(
        self,
        address: Address,
        shape: CoupledShape,
        jailed_at: u64,
    ) -> Result<ValidatorLifecycle> {
        Ok(match shape {
            CoupledShape::WaitingForStake => {
                ValidatorLifecycle::WaitingForStake(registered_payload!(self, WaitingForStake))
            }
            CoupledShape::WaitingForReadiness => ValidatorLifecycle::WaitingForReadiness(
                registered_payload!(self, WaitingForReadiness),
            ),
            CoupledShape::Joining => {
                ValidatorLifecycle::Joining(registered_payload!(self, Joining))
            }
            CoupledShape::Active => ValidatorLifecycle::Active(registered_payload!(self, Active)),
            CoupledShape::Exiting => {
                ValidatorLifecycle::Exiting(registered_payload!(self, Exiting))
            }
            CoupledShape::Unbonding => {
                ValidatorLifecycle::Unbonding(registered_payload!(self, Unbonding))
            }
            CoupledShape::Inactive => ValidatorLifecycle::Inactive(self.into_inactive(address)?),
            CoupledShape::JailRetained => {
                ValidatorLifecycle::JailRetained(registered_payload!(self, JailRetained, jailed_at))
            }
            CoupledShape::Jail => {
                ValidatorLifecycle::Jail(registered_payload!(self, Jail, jailed_at))
            }
        })
    }

    fn into_inactive(self, address: Address) -> Result<Inactive> {
        if self.stake != StakeProjection::zero() {
            return Err(corrupt_state(
                address,
                "inactive validator retains bonded stake or an unbonding hint",
            ));
        }
        Ok(Inactive {
            registry_index: self.registry_index,
            consensus_pubkey: self.consensus_pubkey,
            p2p: self.p2p,
            history: self.history,
        })
    }
}

impl ValidatorLifecycle {
    pub(crate) fn validate_stored_status(status: u8) -> Result<()> {
        if status <= JAILED {
            Ok(())
        } else {
            Err(PrecompileError::Fatal(format!(
                "unknown validator status {status}"
            )))
        }
    }

    pub fn stored_status(&self) -> Option<u8> {
        match self {
            Self::Absent => None,
            Self::WaitingForStake(_) => Some(REGISTERED),
            Self::WaitingForReadiness(_) | Self::Joining(_) => Some(PENDING),
            Self::Active(_) => Some(ACTIVE),
            Self::Exiting(_) => Some(EXITING),
            Self::Unbonding(_) => Some(UNBONDING),
            Self::Inactive(_) => Some(INACTIVE),
            Self::JailRetained(_) | Self::Jail(_) => Some(JAILED),
        }
    }

    pub const fn has_bls_share(&self) -> bool {
        matches!(
            self,
            Self::Active(_) | Self::JailRetained(_) | Self::Exiting(_)
        )
    }

    pub const fn join_confirmed(&self) -> bool {
        matches!(self, Self::Joining(_))
    }

    pub const fn stored_jailed_at(&self) -> u64 {
        match self {
            Self::JailRetained(state) => state.jailed_at,
            Self::Jail(state) => state.jailed_at,
            _ => 0,
        }
    }

    pub const fn is_active_status(&self) -> bool {
        matches!(self, Self::Active(_))
    }

    pub const fn is_pending(&self) -> bool {
        matches!(self, Self::WaitingForReadiness(_) | Self::Joining(_))
    }

    pub const fn is_registered_status(&self) -> bool {
        matches!(self, Self::WaitingForStake(_))
    }

    pub const fn is_current_consensus_participant(&self) -> bool {
        matches!(
            self,
            Self::Active(_) | Self::Exiting(_) | Self::JailRetained(_)
        )
    }

    pub const fn is_reshare_target(&self) -> bool {
        matches!(self, Self::Active(_) | Self::Joining(_))
    }

    pub const fn is_secondary_admission(&self) -> bool {
        matches!(
            self,
            Self::WaitingForStake(_)
                | Self::WaitingForReadiness(_)
                | Self::Joining(_)
                | Self::JailRetained(_)
                | Self::Jail(_)
        )
    }

    pub fn registry_index(&self) -> Option<NonZeroU64> {
        lifecycle_ref!(self, registry_index).copied()
    }

    pub fn consensus_pubkey(&self) -> Option<&ConsensusPubkey> {
        lifecycle_ref!(self, consensus_pubkey)
    }

    pub fn p2p(&self) -> Option<&P2pInfo> {
        lifecycle_ref!(self, p2p)
    }

    pub fn stake(&self) -> Option<&StakeProjection> {
        match self {
            Self::WaitingForStake(state) => Some(&state.stake),
            Self::WaitingForReadiness(state) => Some(&state.stake),
            Self::Joining(state) => Some(&state.stake),
            Self::Active(state) => Some(&state.stake),
            Self::JailRetained(state) => Some(&state.stake),
            Self::Jail(state) => Some(&state.stake),
            Self::Exiting(state) => Some(&state.stake),
            Self::Unbonding(state) => Some(&state.stake),
            Self::Absent | Self::Inactive(_) => None,
        }
    }

    pub fn history(&self) -> Option<&ValidatorHistory> {
        lifecycle_ref!(self, history)
    }

    pub(crate) fn validate(&self, address: Address) -> Result<()> {
        if let Some(key) = self.consensus_pubkey() {
            if *key == [0; 48] {
                return Err(corrupt_state(
                    address,
                    "registered validator has a zero key",
                ));
            }
        }
        if let Some(stake) = self.stake() {
            validate_hint(address, *stake)?;
        }
        if self
            .history()
            .is_some_and(|history| history.last_deactivated_at_height == Some(0))
        {
            return Err(corrupt_state(
                address,
                "deactivation height uses a non-canonical zero sentinel",
            ));
        }
        match self.deactivation_violation() {
            Some(violation) => Err(corrupt_state(address, violation)),
            None => Ok(()),
        }
    }

    /// The broken rule, when the deactivation height or jail height of this
    /// lifecycle contradicts its variant.
    fn deactivation_violation(&self) -> Option<&'static str> {
        match self {
            Self::Active(state) if state.history.last_deactivated_at_height.is_some() => {
                Some("active validator retains a deactivation height")
            }
            Self::Exiting(state) if state.history.last_deactivated_at_height.is_none() => {
                Some("exiting validator is missing its deactivation height")
            }
            Self::Unbonding(state) if state.history.last_deactivated_at_height.is_none() => {
                Some("unbonding validator is missing its deactivation height")
            }
            Self::Inactive(state) if state.history.last_deactivated_at_height.is_none() => {
                Some("inactive validator is missing its deactivation height")
            }
            Self::JailRetained(state) if !jail_height_matches(state.jailed_at, &state.history) => {
                Some("retained jail height and history deactivation height disagree")
            }
            Self::Jail(state) if !jail_height_matches(state.jailed_at, &state.history) => {
                Some("jail height and history deactivation height disagree")
            }
            _ => None,
        }
    }
}

/// True when a jail height is set and equals the history deactivation height.
fn jail_height_matches(jailed_at: u64, history: &ValidatorHistory) -> bool {
    jailed_at != 0 && history.last_deactivated_at_height == Some(jailed_at)
}

fn validate_hint(address: Address, stake: StakeProjection) -> Result<()> {
    if stake.unbonding_end_hint == Some(0) {
        return Err(corrupt_state(
            address,
            "unbonding-end hint uses a non-canonical zero sentinel",
        ));
    }
    Ok(())
}

fn corrupt_state(address: Address, detail: impl std::fmt::Display) -> PrecompileError {
    PrecompileError::Fatal(format!("corrupt validator state for {address}: {detail}"))
}
