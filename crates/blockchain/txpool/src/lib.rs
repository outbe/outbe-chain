//! Outbe transaction pool builder.
//!
//! The pool keeps standard Reth validation and adds deterministic ZeroFee
//! guards for whitelisted validator transactions.

pub mod maintain;

mod ocomp_admission;

use alloy_eips::{eip7840::BlobParams, merge::EPOCH_SLOTS};
use alloy_primitives::{Address, B256, KECCAK256_EMPTY, U256};
use outbe_ocomp_protocol::system_carrier::{
    classify_ocomp_system_carrier, OcompSystemCarrierCandidate, OcompSystemCarrierError,
    OcompSystemCarrierView,
};
use outbe_primitives::{
    addresses::OUTBE_SYSTEM_TX_ADDRESS,
    storage::{readonly::ReadOnlyStorageProvider, StorageHandle},
    system_tx::OcompLifecycleActivation,
    OutbePrimitives,
};
use outbe_zerofee::{BootstrapTransactionView, ZeroFeeHookId, ZeroFeeTransaction};
use reth_chainspec::{ChainSpecProvider, EthChainSpec, EthereumHardforks};
use reth_evm::ConfigureEvm;
use reth_node_builder::{
    components::{PoolBuilder, TxPoolBuilder},
    node::{FullNodeTypes, NodeTypes},
    BuilderContext,
};
use reth_storage_api::{BlockNumReader, StateProviderFactory};
use reth_transaction_pool::{
    blobstore::DiskFileBlobStore,
    error::{InvalidPoolTransactionError, PoolTransactionError},
    validate::ValidTransaction,
    EthPoolTransaction, EthTransactionValidator, Pool, PoolTransaction, Priority,
    TransactionOrdering, TransactionOrigin, TransactionValidationOutcome,
    TransactionValidationTaskExecutor, TransactionValidator,
};
use std::{any::Any, fmt, marker::PhantomData, time::SystemTime};

fn is_reserved_system_tx<T>(tx: &T) -> bool
where
    T: alloy_consensus::Transaction + ?Sized,
{
    tx.to() == Some(OUTBE_SYSTEM_TX_ADDRESS)
}

fn zero_fee_transaction<'a, T>(tx: &'a T, signer: Address) -> ZeroFeeTransaction<'a>
where
    T: alloy_consensus::Transaction + ?Sized,
{
    ZeroFeeTransaction {
        signer,
        to: tx.to(),
        value: tx.value(),
        input: tx.input().as_ref(),
        gas_limit: tx.gas_limit(),
        max_fee_per_gas: tx.max_fee_per_gas(),
        max_priority_fee_per_gas: tx.max_priority_fee_per_gas(),
    }
}

fn bootstrap_transaction<'a, T>(
    tx: &'a T,
    signer: Address,
    network_chain_id: u64,
) -> Option<BootstrapTransactionView<'a>>
where
    T: alloy_consensus::Transaction + ?Sized,
{
    let authorization_list = tx.authorization_list()?;
    Some(BootstrapTransactionView {
        signer,
        tx_chain_id: tx.chain_id(),
        network_chain_id,
        nonce: tx.nonce(),
        to: tx.to(),
        value: tx.value(),
        input: tx.input().as_ref(),
        gas_limit: tx.gas_limit(),
        max_fee_per_gas: tx.max_fee_per_gas(),
        max_priority_fee_per_gas: tx.max_priority_fee_per_gas(),
        access_list_empty: tx.access_list().is_some_and(|list| list.is_empty()),
        authorization_list,
    })
}

fn classify_ocomp_carrier<T>(
    tx: &T,
) -> Result<Option<OcompSystemCarrierCandidate>, OcompSystemCarrierError>
where
    T: alloy_consensus::Transaction + ?Sized,
{
    classify_ocomp_system_carrier(
        OcompSystemCarrierView {
            is_eip1559: tx.ty() == alloy_consensus::TxType::Eip1559 as u8,
            to: tx.to(),
            value: tx.value(),
            input: tx.input().as_ref(),
            gas_limit: tx.gas_limit(),
            max_fee_per_gas: tx.max_fee_per_gas(),
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas(),
        },
        &outbe_ocomp_protocol::profile::poc_schema_limits(),
    )
}

/// Returns a reserved priority class for zero-fee hooks that must outrank the
/// normal tip market. Keep this match exhaustive so every new hook makes an
/// explicit ordering decision.
fn zero_fee_priority_class(hook: ZeroFeeHookId) -> Option<u8> {
    match hook {
        ZeroFeeHookId::OracleSubmitVote => Some(1),
        ZeroFeeHookId::IntexFactoryPayContributorBatch => Some(1),
        ZeroFeeHookId::HyperlaneSubmitCheckpoint => Some(1),
    }
}

/// Outbe pool type with guarded ZeroFee validation.
pub type OutbeTransactionPool<Client, S, Evm, T = reth_transaction_pool::EthPooledTransaction> =
    Pool<
        TransactionValidationTaskExecutor<OutbeTransactionValidator<Client, T, Evm>>,
        OutbeTransactionOrdering<T>,
        S,
    >;

/// Orders hook-approved ZeroFee transactions ahead of the normal tip market.
#[derive(Debug)]
pub struct OutbeTransactionOrdering<T>(PhantomData<T>);

impl<T> Clone for OutbeTransactionOrdering<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T> Copy for OutbeTransactionOrdering<T> {}

impl<T> Default for OutbeTransactionOrdering<T> {
    fn default() -> Self {
        Self(PhantomData)
    }
}

impl<T> TransactionOrdering for OutbeTransactionOrdering<T>
where
    T: PoolTransaction + 'static,
{
    type PriorityValue = (u8, u128);
    type Transaction = T;

    fn priority(
        &self,
        transaction: &Self::Transaction,
        base_fee: u64,
    ) -> Priority<Self::PriorityValue> {
        let zero_fee_tx = zero_fee_transaction(transaction, transaction.sender());
        let normal_priority = || {
            transaction
                .effective_tip_per_gas(base_fee)
                .map(|tip| (0, tip))
                .into()
        };

        match classify_ocomp_carrier(transaction) {
            Ok(Some(_)) => return Priority::Value((2, 0)),
            Err(_) => return Priority::None,
            Ok(None) => {}
        }

        match outbe_zerofee::registry().classify(&zero_fee_tx) {
            Ok(Some(candidate)) => zero_fee_priority_class(candidate.hook)
                .map(|priority_class| Priority::Value((priority_class, 0)))
                .unwrap_or_else(normal_priority),
            Ok(None) => normal_priority(),
            Err(_) => Priority::None,
        }
    }
}

/// Builds the transaction pool used by Outbe nodes.
#[derive(Debug, Clone, Copy, Default)]
pub struct OutbePoolBuilder {
    ocomp_lifecycle_activation: OcompLifecycleActivation,
}

impl OutbePoolBuilder {
    #[must_use]
    pub const fn with_ocomp_lifecycle_activation(
        mut self,
        activation: OcompLifecycleActivation,
    ) -> Self {
        self.ocomp_lifecycle_activation = activation;
        self
    }
}

impl<Types, Node, Evm> PoolBuilder<Node, Evm> for OutbePoolBuilder
where
    Types: NodeTypes<ChainSpec: EthChainSpec + EthereumHardforks, Primitives = OutbePrimitives>,
    Node: FullNodeTypes<Types = Types>,
    Evm: ConfigureEvm<Primitives = OutbePrimitives> + Clone + 'static,
{
    type Pool = OutbeTransactionPool<Node::Provider, DiskFileBlobStore, Evm>;

    async fn build_pool(
        self,
        ctx: &BuilderContext<Node>,
        evm_config: Evm,
    ) -> eyre::Result<Self::Pool> {
        let pool_config = ctx.pool_config();

        let blobs_disabled = ctx.config().txpool.disable_blobs_support
            || ctx.config().txpool.blobpool_max_count == 0;

        let blob_cache_size = if let Some(blob_cache_size) = pool_config.blob_cache_size {
            Some(blob_cache_size)
        } else {
            let current_timestamp = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)?
                .as_secs();
            let blob_params = ctx
                .chain_spec()
                .blob_params_at_timestamp(current_timestamp)
                .unwrap_or_else(BlobParams::cancun);

            Some((blob_params.target_blob_count * EPOCH_SLOTS * 2) as u32)
        };

        let blob_store =
            reth_node_builder::components::create_blob_store_with_cache(ctx, blob_cache_size)?;

        let validator =
            TransactionValidationTaskExecutor::eth_builder(ctx.provider().clone(), evm_config)
                .set_eip4844(!blobs_disabled)
                .kzg_settings(ctx.kzg_settings()?)
                .with_max_tx_input_bytes(ctx.config().txpool.max_tx_input_bytes)
                .with_local_transactions_config(pool_config.local_transactions_config.clone())
                .set_tx_fee_cap(ctx.config().rpc.rpc_tx_fee_cap)
                .with_max_tx_gas_limit(ctx.config().txpool.max_tx_gas_limit)
                .with_minimum_priority_fee(ctx.config().txpool.minimum_priority_fee)
                .with_additional_tasks(ctx.config().txpool.additional_validation_tasks)
                .disable_balance_check()
                .build_with_tasks(ctx.task_executor().clone(), blob_store.clone())
                .map(|inner| {
                    OutbeTransactionValidator::new(inner, self.ocomp_lifecycle_activation)
                });

        if validator.validator().inner().eip4844() {
            let kzg_settings = validator.validator().inner().kzg_settings().clone();
            ctx.task_executor().spawn_blocking_task(async move {
                let _ = kzg_settings.get();
                tracing::debug!(target: "reth::cli", "Initialized KZG settings");
            });
        }

        let transaction_pool = TxPoolBuilder::new(ctx)
            .with_validator(validator)
            .build_with_ordering_and_spawn_maintenance_task(
                OutbeTransactionOrdering::default(),
                blob_store,
                pool_config,
            )?;

        tracing::info!(target: "reth::cli", "Outbe transaction pool initialized");
        tracing::debug!(target: "reth::cli", "Spawned txpool maintenance task");

        Ok(transaction_pool)
    }
}

#[derive(Debug)]
struct ValidOutcomeParts<T: reth_transaction_pool::PoolTransaction> {
    balance: U256,
    state_nonce: u64,
    bytecode_hash: Option<B256>,
    transaction: ValidTransaction<T>,
    propagate: bool,
    authorities: Option<Vec<Address>>,
}

#[derive(Debug)]
enum ValidOutcomeSplit<T: reth_transaction_pool::PoolTransaction> {
    Valid(ValidOutcomeParts<T>),
    Other(TransactionValidationOutcome<T>),
}

fn take_valid_outcome<T>(outcome: TransactionValidationOutcome<T>) -> ValidOutcomeSplit<T>
where
    T: reth_transaction_pool::PoolTransaction,
{
    let TransactionValidationOutcome::Valid {
        balance,
        state_nonce,
        bytecode_hash,
        transaction,
        propagate,
        authorities,
    } = outcome
    else {
        return ValidOutcomeSplit::Other(outcome);
    };

    ValidOutcomeSplit::Valid(ValidOutcomeParts {
        balance,
        state_nonce,
        bytecode_hash,
        transaction,
        propagate,
        authorities,
    })
}

fn valid_outcome<T>(parts: ValidOutcomeParts<T>) -> TransactionValidationOutcome<T>
where
    T: reth_transaction_pool::PoolTransaction,
{
    TransactionValidationOutcome::Valid {
        balance: parts.balance,
        state_nonce: parts.state_nonce,
        bytecode_hash: parts.bytecode_hash,
        transaction: parts.transaction,
        propagate: parts.propagate,
        authorities: parts.authorities,
    }
}

#[derive(Debug)]
enum ReservedSystemTxPolicy<T: reth_transaction_pool::PoolTransaction> {
    Continue(ValidOutcomeParts<T>),
    Reject(TransactionValidationOutcome<T>),
}

fn reject_reserved_system_tx_outcome<T>(parts: ValidOutcomeParts<T>) -> ReservedSystemTxPolicy<T>
where
    T: EthPoolTransaction + alloy_consensus::Transaction,
{
    if is_reserved_system_tx(parts.transaction.transaction()) {
        return ReservedSystemTxPolicy::Reject(TransactionValidationOutcome::Invalid(
            parts.transaction.into_transaction(),
            InvalidPoolTransactionError::other(OutbeReservedSystemTxPoolError),
        ));
    }
    ReservedSystemTxPolicy::Continue(parts)
}

/// Transaction validator that keeps reth's Ethereum checks and adds Outbe policy.
pub struct OutbeTransactionValidator<Client, Tx, Evm> {
    inner: EthTransactionValidator<Client, Tx, Evm>,
    ocomp_lifecycle_activation: OcompLifecycleActivation,
}

impl<Client, Tx, Evm> OutbeTransactionValidator<Client, Tx, Evm> {
    fn new(
        inner: EthTransactionValidator<Client, Tx, Evm>,
        ocomp_lifecycle_activation: OcompLifecycleActivation,
    ) -> Self {
        Self {
            inner,
            ocomp_lifecycle_activation,
        }
    }

    fn inner(&self) -> &EthTransactionValidator<Client, Tx, Evm> {
        &self.inner
    }
}

impl<Client, Tx, Evm> fmt::Debug for OutbeTransactionValidator<Client, Tx, Evm>
where
    EthTransactionValidator<Client, Tx, Evm>: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutbeTransactionValidator")
            .field("inner", &self.inner)
            .field(
                "ocomp_lifecycle_activation",
                &self.ocomp_lifecycle_activation,
            )
            .finish()
    }
}

impl<Client, Tx, Evm> TransactionValidator for OutbeTransactionValidator<Client, Tx, Evm>
where
    EthTransactionValidator<Client, Tx, Evm>: TransactionValidator<Transaction = Tx>,
    Client: StateProviderFactory
        + BlockNumReader
        + ChainSpecProvider<ChainSpec: EthChainSpec + EthereumHardforks>,
    Tx: EthPoolTransaction + alloy_consensus::Transaction,
    Evm: ConfigureEvm,
{
    type Transaction = Tx;
    type Block = <EthTransactionValidator<Client, Tx, Evm> as TransactionValidator>::Block;

    async fn validate_transaction(
        &self,
        origin: TransactionOrigin,
        transaction: Self::Transaction,
    ) -> TransactionValidationOutcome<Self::Transaction> {
        match classify_ocomp_carrier(&transaction) {
            Ok(Some(candidate)) => {
                return self.validate_ocomp_system_carrier(origin, transaction, candidate)
            }
            Err(error) => {
                return TransactionValidationOutcome::Invalid(
                    transaction,
                    InvalidPoolTransactionError::other(
                        ocomp_admission::OutbeOcompSystemCarrierPoolError(error.to_string()),
                    ),
                )
            }
            Ok(None) => {}
        }
        let outcome = self.inner.validate_transaction(origin, transaction).await;
        self.apply_outbe_policy(outcome)
    }

    fn on_new_head_block(&self, new_tip_block: &reth_primitives_traits::SealedBlock<Self::Block>) {
        self.inner.on_new_head_block(new_tip_block);
    }
}

impl<Client, Tx, Evm> OutbeTransactionValidator<Client, Tx, Evm>
where
    Client: StateProviderFactory
        + BlockNumReader
        + ChainSpecProvider<ChainSpec: EthChainSpec + EthereumHardforks>,
    Tx: EthPoolTransaction + alloy_consensus::Transaction,
    Evm: ConfigureEvm,
{
    fn validate_ocomp_system_carrier(
        &self,
        origin: TransactionOrigin,
        transaction: Tx,
        candidate: OcompSystemCarrierCandidate,
    ) -> TransactionValidationOutcome<Tx> {
        ocomp_admission::validate(
            &self.inner,
            self.ocomp_lifecycle_activation,
            origin,
            transaction,
            candidate,
        )
    }

    fn apply_outbe_policy(
        &self,
        outcome: TransactionValidationOutcome<Tx>,
    ) -> TransactionValidationOutcome<Tx> {
        let mut parts = match take_valid_outcome(outcome) {
            ValidOutcomeSplit::Valid(parts) => parts,
            ValidOutcomeSplit::Other(outcome) => return outcome,
        };

        parts = match reject_reserved_system_tx_outcome(parts) {
            ReservedSystemTxPolicy::Continue(parts) => parts,
            ReservedSystemTxPolicy::Reject(outcome) => return outcome,
        };

        let tx = parts.transaction.transaction();
        let signer = tx.sender();
        let zero_fee_tx = zero_fee_transaction(tx, signer);
        let normal_fee_outcome = |parts: ValidOutcomeParts<Tx>| {
            let cost = *parts.transaction.transaction().cost();
            if cost > parts.balance {
                let balance = parts.balance;
                TransactionValidationOutcome::Invalid(
                    parts.transaction.into_transaction(),
                    InvalidPoolTransactionError::Overdraft { cost, balance },
                )
            } else {
                valid_outcome(parts)
            }
        };
        let classification = outbe_zerofee::registry().classify(&zero_fee_tx);
        match classification {
            Ok(Some(candidate)) => match self.validate_zero_fee_state(candidate) {
                Ok(()) => {
                    parts.balance = U256::MAX;
                    valid_outcome(parts)
                }
                Err(err) => TransactionValidationOutcome::Invalid(
                    parts.transaction.into_transaction(),
                    InvalidPoolTransactionError::other(OutbeZeroFeePoolError(err.to_string())),
                ),
            },
            Ok(None) => {
                let bootstrap_candidate = bootstrap_transaction(
                    parts.transaction.transaction(),
                    signer,
                    self.inner.chain_id(),
                )
                .and_then(|view| outbe_zerofee::classify_bootstrap(&view));
                if let Some(candidate) = bootstrap_candidate {
                    match self.validate_bootstrap_state(candidate) {
                        Ok(true) => {
                            parts.balance = U256::MAX;
                            return valid_outcome(parts);
                        }
                        Ok(false) => return normal_fee_outcome(parts),
                        Err(err) => {
                            return TransactionValidationOutcome::Invalid(
                                parts.transaction.into_transaction(),
                                InvalidPoolTransactionError::other(OutbeZeroFeePoolError(
                                    err.to_string(),
                                )),
                            )
                        }
                    }
                }

                match self.try_eip7702_sponsorship(signer, &zero_fee_tx) {
                    Ok(SponsorshipOutcome::Accepted) => {
                        parts.balance = U256::MAX;
                        valid_outcome(parts)
                    }
                    Ok(SponsorshipOutcome::NotSponsored) => normal_fee_outcome(parts),
                    Err(err) => TransactionValidationOutcome::Invalid(
                        parts.transaction.into_transaction(),
                        InvalidPoolTransactionError::other(OutbeZeroFeePoolError(err.to_string())),
                    ),
                }
            }
            Err(err) => TransactionValidationOutcome::Invalid(
                parts.transaction.into_transaction(),
                InvalidPoolTransactionError::other(OutbeZeroFeePoolError(err.to_string())),
            ),
        }
    }

    /// Probe whether `signer` has an EIP-7702 delegation to
    /// [`outbe_zerofee::ZEROFEE_ADDRESS`]; if so, run the same
    /// `classify_sponsorship` + `authorize_sponsorship` checks the
    /// executor will perform at block time.
    ///
    /// Returns [`SponsorshipOutcome::NotSponsored`] when no delegation
    /// is present (the tx then falls back to the standard
    /// cost-vs-balance Overdraft gate). Returns `Err(_)` for an
    /// authenticated sponsorship attempt that fails any of the policy
    /// rules - the pool rejects with the policy reason in the
    /// `InvalidPoolTransactionError::other` payload so the caller sees
    /// the same error code the executor would produce at block time.
    ///
    /// The latest-block view used here is necessarily stale relative to
    /// the block currently building; an admitted sponsored tx whose
    /// quota was already burned by an earlier in-block tx will be
    /// rejected at execution time via a `status=0` receipt (same
    /// pattern as the oracle `AlreadyVoted` flow).
    fn try_eip7702_sponsorship(
        &self,
        signer: Address,
        zero_fee_tx: &ZeroFeeTransaction<'_>,
    ) -> Result<SponsorshipOutcome, OutbeZeroFeePoolError> {
        let state = self
            .inner
            .client()
            .latest()
            .map_err(|e| OutbeZeroFeePoolError(e.to_string()))?;

        // Resolve the signer's account + (optional) delegation bytecode
        // from the latest committed state, then hand the already-fetched
        // values to the pure decision core. Splitting the I/O from the
        // policy keeps the composition (delegation match -> classify ->
        // precheck, with NO quota check) deterministically unit-testable
        // without a provider mock - see `sponsorship_decision` tests.
        let Some(account) = state
            .basic_account(&signer)
            .map_err(|e| OutbeZeroFeePoolError(e.to_string()))?
        else {
            return Ok(SponsorshipOutcome::NotSponsored);
        };

        let delegation_bytecode = match account.bytecode_hash {
            Some(hash) => state
                .bytecode_by_hash(&hash)
                .map_err(|e| OutbeZeroFeePoolError(e.to_string()))?,
            None => None,
        };
        let delegated_to = delegation_bytecode
            .as_ref()
            .and_then(|bc| bc.eip7702_address());

        sponsorship_decision(signer, delegated_to, zero_fee_tx)
    }

    fn validate_zero_fee_state(
        &self,
        candidate: outbe_zerofee::ZeroFeeCandidate,
    ) -> Result<(), OutbeZeroFeePoolError> {
        let state = self
            .inner
            .client()
            .latest()
            .map_err(|e| OutbeZeroFeePoolError(e.to_string()))?;

        let reader = ocomp_admission::RethStateReader::new(&state);
        let mut provider = ReadOnlyStorageProvider::new(reader);
        let storage = StorageHandle::new(&mut provider);

        outbe_zerofee::registry()
            .authorize_fee_waiver(storage, candidate)
            .map(|_| ())
            .map_err(|e| OutbeZeroFeePoolError(e.to_string()))
    }

    fn validate_bootstrap_state(
        &self,
        candidate: outbe_zerofee::BootstrapCandidate,
    ) -> Result<bool, OutbeZeroFeePoolError> {
        let state = self
            .inner
            .client()
            .latest()
            .map_err(|e| OutbeZeroFeePoolError(e.to_string()))?;
        let Some(account) = state
            .basic_account(&candidate.signer)
            .map_err(|e| OutbeZeroFeePoolError(e.to_string()))?
        else {
            return Ok(false);
        };

        Ok(outbe_zerofee::authorize_bootstrap(
            candidate,
            outbe_zerofee::BootstrapAccountView {
                balance: account.balance,
                nonce: account.nonce,
                code_empty: account
                    .bytecode_hash
                    .is_none_or(|code_hash| code_hash == KECCAK256_EMPTY),
            },
        ))
    }
}

/// Outcome of the EIP-7702 sponsorship probe in `apply_outbe_policy`.
#[derive(Debug, PartialEq, Eq)]
enum SponsorshipOutcome {
    /// Signer is delegated to ZEROFEE_ADDRESS and passes all policy checks.
    Accepted,
    /// Signer is not delegated to ZEROFEE_ADDRESS; fall back to normal
    /// cost-vs-balance gating.
    NotSponsored,
}

/// Pure decision core for EIP-7702 sponsorship pool admission, factored
/// out of [`OutbeTransactionValidator::try_eip7702_sponsorship`] so the
/// composition is testable without a provider mock.
///
/// Inputs are the values the caller already fetched from the latest
/// committed state: the signer, its native `balance`, and the address
/// its account code delegates to (`None` if it is not an EIP-7702
/// delegation). The decision:
///   - `delegated_to != Some(ZEROFEE_ADDRESS)` -> `NotSponsored` (normal
///     fee path; never an error).
///   - delegated but the envelope does not match `classify_sponsorship`
///     (most importantly `priority_fee > 0` - "I am paying") ->
///     `NotSponsored`. The tx is a normal paid transaction that merely
///     originates from a delegated account; it must go through the
///     standard cost-vs-balance gating, NOT be rejected. This keeps
///     EIP-7702 delegation additive and lets a signer pay once their
///     daily free quota is exhausted.
///   - delegated AND envelope matches -> run `precheck_sponsorship`
///     (self-sponsorship); its policy error is
///     returned so the pool rejects with the matching code.
///
/// Quota is deliberately NOT checked here: the executor is authoritative
/// and quota-exhausted txs must land in the block as soft-failures
/// (code 110), so the pool admits them.
fn sponsorship_decision(
    signer: Address,
    delegated_to: Option<Address>,
    zero_fee_tx: &ZeroFeeTransaction<'_>,
) -> Result<SponsorshipOutcome, OutbeZeroFeePoolError> {
    if delegated_to != Some(outbe_zerofee::ZEROFEE_ADDRESS) {
        return Ok(SponsorshipOutcome::NotSponsored);
    }

    // Envelope mismatch (e.g. priority_fee > 0) means the signer is not
    // opting into sponsorship - fall through to the normal fee path
    // rather than rejecting the tx.
    if outbe_zerofee::classify_sponsorship(zero_fee_tx).is_err() {
        return Ok(SponsorshipOutcome::NotSponsored);
    }

    outbe_zerofee::precheck_sponsorship(signer)
        .map_err(|e| OutbeZeroFeePoolError(e.to_string()))?;

    Ok(SponsorshipOutcome::Accepted)
}

#[derive(Debug, thiserror::Error)]
#[error("reserved system transaction address is not accepted from users")]
struct OutbeReservedSystemTxPoolError;

impl PoolTransactionError for OutbeReservedSystemTxPoolError {
    fn is_bad_transaction(&self) -> bool {
        true
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[derive(Debug, thiserror::Error)]
#[error("zero-fee policy rejected transaction: {0}")]
struct OutbeZeroFeePoolError(String);

impl PoolTransactionError for OutbeZeroFeePoolError {
    fn is_bad_transaction(&self) -> bool {
        false
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests;
