//! Neutral zero-fee transaction views and policy errors.

use core::fmt;

use alloy_consensus::Transaction;
use alloy_eips::eip7702::SignedAuthorization;
use alloy_primitives::{Address, B256, U256};

use crate::error::PrecompileError;
use crate::transaction_call::TransactionCallFields;

/// Maximum gas limit for the one-time self-authorized EIP-7702 bootstrap.
pub const FREE_TX_BOOTSTRAP_GAS_LIMIT: u64 = 100_000;

/// A minimal transaction view for zero-fee hooks.
#[derive(Clone, Copy)]
pub struct ZeroFeeTransaction<'a> {
    /// Recovered signer of the signed EVM transaction.
    pub signer: Address,
    /// Call fields of the signed EVM transaction.
    pub call: TransactionCallFields<'a>,
}

impl fmt::Debug for ZeroFeeTransaction<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("ZeroFeeTransaction");
        out.field("signer", &self.signer);
        self.call.debug_fields(&mut out);
        out.finish()
    }
}

impl<'a> ZeroFeeTransaction<'a> {
    /// Adapt an immutable transaction. The caller supplies its recovered signer.
    pub fn from_transaction<T: Transaction + ?Sized>(tx: &'a T, signer: Address) -> Self {
        Self {
            signer,
            call: TransactionCallFields::from_transaction(tx),
        }
    }
}

/// Stable identifier for a registered zero-fee hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZeroFeeHookId {
    /// `Oracle.submitVote(ExchangeRateTuple[])`.
    OracleSubmitVote,
    /// `HyperlaneController.submitCheckpoint(uint32,bytes32,uint32,bytes32,bytes)`.
    HyperlaneSubmitCheckpoint,
    /// Own-lease `TeeRegistry.renewEnclave`.
    RenewEnclave,
}

/// A transaction that matched a hook's stateless zero-fee envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZeroFeeCandidate {
    /// Matching hook.
    pub hook: ZeroFeeHookId,
    /// Recovered transaction signer.
    pub signer: Address,
    /// Canonical node hash carried by an own-lease renewal. Other hooks leave this zero.
    pub node_id_hash: B256,
}

impl ZeroFeeCandidate {
    /// Creates a zero-fee candidate for a matching hook.
    pub const fn new(hook: ZeroFeeHookId, signer: Address) -> Self {
        Self {
            hook,
            signer,
            node_id_hash: B256::ZERO,
        }
    }
}

/// Stateful authorization outcome for a fee waiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZeroFeeAuthorization {
    /// Hook that authorized the fee waiver.
    pub hook: ZeroFeeHookId,
    /// Hook-specific account represented by the signer.
    pub subject: Address,
}

/// Typed precompile storage cause retained by [`ZeroFeePolicyError::Storage`].
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct ZeroFeeStorageError(#[source] pub std::sync::Arc<PrecompileError>);

impl PartialEq for ZeroFeeStorageError {
    fn eq(&self, other: &Self) -> bool {
        self.0.to_string() == other.0.to_string()
    }
}

impl Eq for ZeroFeeStorageError {}

impl From<PrecompileError> for ZeroFeeStorageError {
    fn from(value: PrecompileError) -> Self {
        Self(std::sync::Arc::new(value))
    }
}

/// Zero-fee policy rejection reason.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ZeroFeePolicyError {
    /// The zero-fee envelope targets an unknown hook.
    #[error("unknown zero-fee hook: {0:?}")]
    UnknownHook(ZeroFeeHookId),
    /// The transaction targets a zero-fee method but carries native value.
    #[error("zero-fee transaction must not transfer native value")]
    NonZeroValue,
    /// The transaction calldata exceeds the policy limit.
    #[error("zero-fee calldata size {size} exceeds limit {limit}")]
    CalldataTooLarge {
        /// Actual calldata size.
        size: usize,
        /// Maximum accepted calldata size.
        limit: usize,
    },
    /// The transaction gas limit exceeds the policy limit.
    #[error("zero-fee gas limit {gas_limit} exceeds limit {limit}")]
    GasLimitTooHigh {
        /// Actual gas limit.
        gas_limit: u64,
        /// Maximum accepted gas limit.
        limit: u64,
    },
    /// The transaction's fee cap is below the public txpool protocol minimum.
    #[error("zero-fee max_fee_per_gas {max_fee_per_gas} is below protocol minimum {minimum}")]
    FeeCapTooLow {
        /// Transaction max fee cap.
        max_fee_per_gas: u128,
        /// Minimum accepted fee cap.
        minimum: u128,
    },
    /// The transaction selector matched, but ABI decoding failed.
    #[error("zero-fee oracle vote calldata is malformed: {0}")]
    MalformedCalldata(&'static str),
    /// The signer is not authorized to use this zero-fee policy.
    #[error("zero-fee signer is not authorized for this transaction hook")]
    UnauthorizedSigner,
    /// The represented validator already submitted an oracle vote this period.
    #[error("zero-fee oracle vote already exists for validator")]
    AlreadyVoted,
    /// Stateful precompile storage read failed.
    #[error("zero-fee policy storage read failed: {0}")]
    Storage(#[from] ZeroFeeStorageError),
    /// The signer has already burned today's free-tx quota.
    #[error("free-tx daily quota exhausted: used {used} of {limit}")]
    FreeTxDailyExhausted {
        /// Effective count already recorded for today.
        used: u32,
        /// Daily limit.
        limit: u32,
    },
    /// Contract creation is not allowed through the free-tx path.
    #[error("free-tx must not be a contract creation")]
    FreeTxDailyContractCreationForbidden,
    /// The sponsored tx envelope carries non-zero native value.
    #[error("free-tx must not transfer native value")]
    FreeTxDailyValueNotZero,
    /// The sponsored tx envelope exceeds its `(target, selector)` gas ceiling.
    #[error("free-tx gas limit {gas_limit} exceeds free-tx limit {limit}")]
    FreeTxDailyGasLimitExceeded {
        /// Actual gas limit on the tx.
        gas_limit: u64,
        /// Selected sponsored-path maximum, default or operation-specific.
        limit: u64,
    },
    /// The sponsored tx envelope exceeds the free-tx calldata cap.
    #[error("free-tx calldata size {size} exceeds free-tx limit {limit}")]
    FreeTxDailyCalldataTooLarge {
        /// Actual calldata size.
        size: usize,
        /// Sponsored-path maximum.
        limit: usize,
    },
    /// The transaction target is not in the protocol-defined whitelist.
    #[error("free-tx target {to:?} is not a whitelisted precompile address")]
    FreeTxDailyTargetNotWhitelisted {
        /// Attempted call target.
        to: Address,
    },
}

impl From<PrecompileError> for ZeroFeePolicyError {
    fn from(value: PrecompileError) -> Self {
        Self::Storage(ZeroFeeStorageError::from(value))
    }
}

impl ZeroFeePolicyError {
    /// Stable numeric code emitted when a policy rejection becomes a failed receipt.
    ///
    /// Codes occupy the `100..=199` band. Code 111 is retired and must not be reused.
    pub const fn code(&self) -> u16 {
        match self {
            Self::UnknownHook(_) => 101,
            Self::NonZeroValue => 102,
            Self::CalldataTooLarge { .. } => 103,
            Self::GasLimitTooHigh { .. } => 104,
            Self::FeeCapTooLow { .. } => 105,
            Self::MalformedCalldata(_) => 106,
            Self::UnauthorizedSigner => 107,
            Self::AlreadyVoted => 108,
            Self::Storage(_) => 109,
            Self::FreeTxDailyExhausted { .. } => 110,
            Self::FreeTxDailyContractCreationForbidden => 112,
            Self::FreeTxDailyValueNotZero => 113,
            Self::FreeTxDailyGasLimitExceeded { .. } => 114,
            Self::FreeTxDailyCalldataTooLarge { .. } => 115,
            Self::FreeTxDailyTargetNotWhitelisted { .. } => 116,
        }
    }
}

/// Execution-layer independent view of a possible first ZeroFee delegation.
#[derive(Clone, Copy)]
pub struct BootstrapTransactionView<'a> {
    /// Recovered signer of the outer EIP-7702 transaction.
    pub signer: Address,
    /// Chain ID carried by the outer transaction.
    pub tx_chain_id: Option<u64>,
    /// Chain ID of the block being validated.
    pub network_chain_id: u64,
    /// Outer transaction nonce before REVM increments the sender.
    pub nonce: u64,
    /// Call fields of the outer transaction.
    pub call: TransactionCallFields<'a>,
    /// Whether the EIP-2930 access list is empty.
    pub access_list_empty: bool,
    /// Signed EIP-7702 authorization list.
    pub authorization_list: &'a [SignedAuthorization],
}

impl fmt::Debug for BootstrapTransactionView<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = f.debug_struct("BootstrapTransactionView");
        out.field("signer", &self.signer)
            .field("tx_chain_id", &self.tx_chain_id)
            .field("network_chain_id", &self.network_chain_id)
            .field("nonce", &self.nonce);
        self.call.debug_fields(&mut out);
        out.field("access_list_empty", &self.access_list_empty)
            .field("authorization_list", &self.authorization_list)
            .finish()
    }
}

impl<'a> BootstrapTransactionView<'a> {
    /// Borrow the bootstrap envelope only when an authorization list exists.
    pub fn from_transaction<T: Transaction + ?Sized>(
        tx: &'a T,
        signer: Address,
        network_chain_id: u64,
    ) -> Option<Self> {
        let authorization_list = tx.authorization_list()?;
        let call = TransactionCallFields::from_transaction(tx);
        Some(Self {
            signer,
            tx_chain_id: tx.chain_id(),
            network_chain_id,
            nonce: tx.nonce(),
            call,
            access_list_empty: tx.access_list().is_some_and(|list| list.is_empty()),
            authorization_list,
        })
    }
}

/// A transaction whose signed shape exactly matches the bootstrap protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapCandidate {
    /// Self-authorizing account.
    pub signer: Address,
    /// Expected pre-state account nonce.
    pub nonce: u64,
}

/// Minimal pre-state account view required to authorize a bootstrap waiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapAccountView {
    /// Native balance before transaction execution.
    pub balance: U256,
    /// Account nonce before transaction execution.
    pub nonce: u64,
    /// Whether the account has no deployed or delegated code.
    pub code_empty: bool,
}

/// Opaque sponsorship grant bound to one signer and one UTC day.
///
/// Fields stay private. The zero-fee runtime builds a grant with
/// [`SponsorshipAuthorization::from_parts`] and reads it through the accessors.
#[derive(Debug)]
pub struct SponsorshipAuthorization {
    signer: Address,
    current_day: u32,
    next_count: u32,
}

impl SponsorshipAuthorization {
    /// Build a grant. Callers outside the zero-fee runtime must not use this.
    #[doc(hidden)]
    pub const fn from_parts(signer: Address, current_day: u32, next_count: u32) -> Self {
        Self {
            signer,
            current_day,
            next_count,
        }
    }

    /// Signer this grant authorizes.
    pub const fn signer(&self) -> Address {
        self.signer
    }

    /// UTC day this grant authorizes.
    pub const fn current_day(&self) -> u32 {
        self.current_day
    }

    /// Counter value predicted by authorization, before transaction inclusion.
    pub const fn next_count(&self) -> u32 {
        self.next_count
    }
}
