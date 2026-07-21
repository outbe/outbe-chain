//! User-facing cross-chain WCOEN vault flow.
//!
//! Outbe locks WCOEN through the local token bridge and records pending operations.
//! The fixed BNB adapter owns the real 1:1 vault shares. Outbe receipt shares are
//! finalized only after an authenticated BNB acknowledgement; withdrawals are
//! finalized only after returned WCOEN has been credited to this precompile.

use alloy_primitives::{keccak256, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolValue};

use outbe_primitives::addresses::VAULT_PROVIDER_ADDRESS;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::api::IVaultProvider;
use crate::errors::VaultProviderError;
use crate::schema::VaultProviderContract;
use crate::sol_ext::{IERC7786Bridge, IERC7786TokenBridge, IERC20};

const SELF: Address = VAULT_PROVIDER_ADDRESS;

pub const DEPOSIT_REQUEST: u64 = 1;
pub const DEPOSIT_ACKNOWLEDGEMENT: u64 = 2;
pub const WITHDRAW_REQUEST: u64 = 3;
pub const WITHDRAW_RETURN: u64 = 4;

pub const OPERATION_DEPOSIT: u8 = 1;
pub const OPERATION_WITHDRAW: u8 = 2;
pub const STATUS_PENDING: u8 = 1;
pub const STATUS_COMPLETED: u8 = 2;

struct Configuration {
    asset: Address,
    token_bridge: Address,
    message_bridge: Address,
    destination_chain_id: U256,
    destination_domain: u32,
    remote_provider: Address,
}

fn ensure_owner(storage: &StorageHandle<'_>, sender: Address) -> Result<()> {
    if VaultProviderContract::new(storage.clone()).owner.read()? != sender {
        return Err(VaultProviderError::Unauthorized.into());
    }
    Ok(())
}

pub fn set_asset(
    storage: StorageHandle<'_>,
    sender: Address,
    asset: Address,
    token_bridge: Address,
    destination_chain_id: U256,
) -> Result<()> {
    ensure_owner(&storage, sender)?;
    ensure_no_pending_operations(&storage)?;
    if asset.is_zero() || token_bridge.is_zero() {
        return Err(VaultProviderError::ZeroAddress.into());
    }
    let local_chain_id = U256::from(storage.chain_id()?);
    if destination_chain_id.is_zero() || destination_chain_id == local_chain_id {
        return Err(VaultProviderError::InvalidDestinationChain.into());
    }
    u32::try_from(destination_chain_id)
        .map_err(|_| VaultProviderError::CrosschainDomainTooLarge)?;

    let mut contract = VaultProviderContract::new(storage);
    let old_asset = contract.crosschain_asset.read()?;
    contract.crosschain_asset.write(asset)?;
    contract.crosschain_token_bridge.write(token_bridge)?;
    contract
        .crosschain_destination_chain_id
        .write(destination_chain_id)?;
    contract.emit(IVaultProvider::CrosschainAssetUpdated {
        oldAsset: old_asset,
        newAsset: asset,
        tokenBridge: token_bridge,
        destinationChainId: destination_chain_id,
    })
}

pub fn quote_deposit(
    storage: &StorageHandle<'_>,
    user: Address,
    amount: U256,
    destination_gas_limit: U256,
    acknowledgement_gas_limit: U256,
) -> Result<(U256, B256)> {
    validate_amount(user, amount)?;
    let config = configuration(storage)?;
    let operation_id = preview_operation_id(storage, user, OPERATION_DEPOSIT, amount)?;
    let extra_data = deposit_data(operation_id, user, amount, acknowledgement_gas_limit);
    let fee = token_bridge_quote(storage, &config, amount, extra_data, destination_gas_limit)?;
    Ok((fee, operation_id))
}

pub fn deposit(
    storage: StorageHandle<'_>,
    user: Address,
    amount: U256,
    destination_gas_limit: U256,
    acknowledgement_gas_limit: U256,
    value: U256,
) -> Result<(B256, B256)> {
    validate_amount(user, amount)?;
    storage.with_checkpoint(|| {
        let config = configuration(&storage)?;
        let operation_id = next_operation_id(&storage, user, OPERATION_DEPOSIT, amount)?;
        let extra_data = deposit_data(operation_id, user, amount, acknowledgement_gas_limit);
        let required_fee = token_bridge_quote(
            &storage,
            &config,
            amount,
            extra_data.clone(),
            destination_gas_limit,
        )?;
        ensure_fee(value, required_fee)?;
        record_operation(&storage, operation_id, user, amount, OPERATION_DEPOSIT)?;

        erc20_transfer_from(&storage, config.asset, user, SELF, amount)?;
        erc20_approve(&storage, config.asset, config.token_bridge, amount)?;
        let ret = storage.call(
            config.token_bridge,
            value,
            IERC7786TokenBridge::sendAndCallCall {
                destinationDomain: config.destination_domain,
                to: config.remote_provider,
                amount,
                extraData: extra_data.into(),
                gasLimit: destination_gas_limit,
            }
            .abi_encode()
            .into(),
        )?;
        let send_id = IERC7786TokenBridge::sendAndCallCall::abi_decode_returns(&ret)
            .map_err(|_| VaultProviderError::UndecodableReturn("token bridge sendAndCall"))?;
        erc20_approve(&storage, config.asset, config.token_bridge, U256::ZERO)?;

        let mut contract = VaultProviderContract::new(storage.clone());
        contract.emit(IVaultProvider::CrosschainDepositSent {
            operationId: operation_id,
            user,
            assetsAmount: amount,
            destinationChainId: config.destination_chain_id,
            sendId: send_id,
        })?;
        Ok((operation_id, send_id))
    })
}

pub fn quote_withdraw(
    storage: &StorageHandle<'_>,
    user: Address,
    amount: U256,
    request_gas_limit: U256,
    return_gas_limit: U256,
) -> Result<(U256, B256)> {
    validate_amount(user, amount)?;
    ensure_shares(storage, user, amount)?;
    let config = configuration(storage)?;
    let operation_id = preview_operation_id(storage, user, OPERATION_WITHDRAW, amount)?;
    let payload = withdraw_request_data(operation_id, user, amount, return_gas_limit);
    let recipient = format_evm_v1(config.destination_chain_id, config.remote_provider);
    let fee = message_bridge_quote(
        storage,
        config.message_bridge,
        recipient,
        payload,
        gas_attributes(request_gas_limit),
    )?;
    Ok((fee, operation_id))
}

pub fn withdraw(
    storage: StorageHandle<'_>,
    user: Address,
    amount: U256,
    request_gas_limit: U256,
    return_gas_limit: U256,
    value: U256,
) -> Result<(B256, B256)> {
    validate_amount(user, amount)?;
    ensure_shares(&storage, user, amount)?;
    storage.with_checkpoint(|| {
        let config = configuration(&storage)?;
        let operation_id = next_operation_id(&storage, user, OPERATION_WITHDRAW, amount)?;
        let payload = withdraw_request_data(operation_id, user, amount, return_gas_limit);
        let recipient = format_evm_v1(config.destination_chain_id, config.remote_provider);
        let attributes = gas_attributes(request_gas_limit);
        let required_fee = message_bridge_quote(
            &storage,
            config.message_bridge,
            recipient.clone(),
            payload.clone(),
            attributes.clone(),
        )?;
        ensure_fee(value, required_fee)?;
        record_operation(&storage, operation_id, user, amount, OPERATION_WITHDRAW)?;

        let mut contract = VaultProviderContract::new(storage.clone());
        let user_shares = contract.crosschain_shares.read(&user)?;
        contract
            .crosschain_shares
            .write(&user, user_shares - amount)?;
        let total = contract.total_crosschain_shares.read()?;
        contract.total_crosschain_shares.write(total - amount)?;

        let ret = storage.call(
            config.message_bridge,
            value,
            IERC7786Bridge::sendMessageCall {
                recipient: recipient.into(),
                payload: payload.into(),
                attributes,
            }
            .abi_encode()
            .into(),
        )?;
        let send_id = IERC7786Bridge::sendMessageCall::abi_decode_returns(&ret)
            .map_err(|_| VaultProviderError::UndecodableReturn("ERC7786Bridge sendMessage"))?;
        contract.emit(IVaultProvider::CrosschainWithdrawalSent {
            operationId: operation_id,
            user,
            receiptShares: amount,
            destinationChainId: config.destination_chain_id,
            sendId: send_id,
        })?;
        Ok((operation_id, send_id))
    })
}

pub fn receive_deposit_acknowledgement(
    storage: StorageHandle<'_>,
    caller: Address,
    value: U256,
    sender: &Bytes,
    payload: &Bytes,
) -> Result<()> {
    outbe_primitives::dispatch::reject_value(&value)?;
    storage.with_checkpoint(|| {
        let config = configuration(&storage)?;
        authenticate_remote(&config, caller, config.message_bridge, sender)?;

        let (kind, operation_id, user, amount) =
            <(U256, B256, Address, U256)>::abi_decode_validate(payload)
                .map_err(|_| VaultProviderError::InvalidCrosschainCallback)?;
        if kind != U256::from(DEPOSIT_ACKNOWLEDGEMENT) {
            return Err(VaultProviderError::InvalidCrosschainCallback.into());
        }
        validate_pending_operation(&storage, operation_id, user, amount, OPERATION_DEPOSIT)?;

        let mut contract = VaultProviderContract::new(storage.clone());
        let current = contract.crosschain_shares.read(&user)?;
        contract.crosschain_shares.write(&user, current + amount)?;
        let total = contract.total_crosschain_shares.read()?;
        contract.total_crosschain_shares.write(total + amount)?;
        contract
            .operation_statuses
            .write(&operation_id, STATUS_COMPLETED)?;
        decrement_pending_operations(&contract)?;
        contract.emit(IVaultProvider::CrosschainDepositFinalized {
            operationId: operation_id,
            user,
            assetsAmount: amount,
            receiptShares: amount,
        })
    })
}

pub fn receive_withdrawal_return(
    storage: StorageHandle<'_>,
    caller: Address,
    source_domain: u32,
    from: &Bytes,
    amount: U256,
    extra_data: &Bytes,
) -> Result<()> {
    storage.with_checkpoint(|| {
        let config = configuration(&storage)?;
        if source_domain != config.destination_domain {
            return Err(VaultProviderError::InvalidCrosschainSender.into());
        }
        authenticate_remote(&config, caller, config.token_bridge, from)?;

        let (kind, operation_id, user, declared_amount) =
            <(U256, B256, Address, U256)>::abi_decode_validate(extra_data)
                .map_err(|_| VaultProviderError::InvalidCrosschainCallback)?;
        if kind != U256::from(WITHDRAW_RETURN) || declared_amount != amount {
            return Err(VaultProviderError::InvalidCrosschainCallback.into());
        }
        validate_pending_operation(&storage, operation_id, user, amount, OPERATION_WITHDRAW)?;

        erc20_transfer(&storage, config.asset, user, amount)?;
        let mut contract = VaultProviderContract::new(storage.clone());
        contract
            .operation_statuses
            .write(&operation_id, STATUS_COMPLETED)?;
        decrement_pending_operations(&contract)?;
        contract.emit(IVaultProvider::CrosschainWithdrawalFinalized {
            operationId: operation_id,
            user,
            receiptShares: amount,
            assetsAmount: amount,
        })
    })
}

fn configuration(storage: &StorageHandle<'_>) -> Result<Configuration> {
    let contract = VaultProviderContract::new(storage.clone());
    let asset = contract.crosschain_asset.read()?;
    if asset.is_zero() {
        return Err(VaultProviderError::CrosschainAssetNotConfigured.into());
    }
    let token_bridge = contract.crosschain_token_bridge.read()?;
    if token_bridge.is_zero() {
        return Err(VaultProviderError::CrosschainTokenBridgeNotConfigured.into());
    }
    let message_bridge = contract.crosschain_bridge.read()?;
    if message_bridge.is_zero() {
        return Err(VaultProviderError::CrosschainBridgeNotConfigured.into());
    }
    let destination_chain_id = contract.crosschain_destination_chain_id.read()?;
    if destination_chain_id.is_zero() {
        return Err(VaultProviderError::InvalidDestinationChain.into());
    }
    let destination_domain = u32::try_from(destination_chain_id)
        .map_err(|_| VaultProviderError::CrosschainDomainTooLarge)?;
    let remote_provider = contract
        .remote_vault_providers
        .read(&destination_chain_id)?;
    if remote_provider.is_zero() {
        return Err(
            VaultProviderError::RemoteVaultProviderNotConfigured(destination_chain_id).into(),
        );
    }
    Ok(Configuration {
        asset,
        token_bridge,
        message_bridge,
        destination_chain_id,
        destination_domain,
        remote_provider,
    })
}

fn authenticate_remote(
    config: &Configuration,
    caller: Address,
    expected_caller: Address,
    sender: &Bytes,
) -> Result<()> {
    let expected_sender = format_evm_v1(config.destination_chain_id, config.remote_provider);
    if caller != expected_caller || sender.as_ref() != expected_sender.as_slice() {
        return Err(VaultProviderError::InvalidCrosschainSender.into());
    }
    Ok(())
}

fn validate_amount(user: Address, amount: U256) -> Result<()> {
    if user.is_zero() {
        return Err(VaultProviderError::ZeroAddress.into());
    }
    if amount.is_zero() {
        return Err(VaultProviderError::InvalidCrosschainAmount.into());
    }
    Ok(())
}

fn ensure_shares(storage: &StorageHandle<'_>, user: Address, required: U256) -> Result<()> {
    let available = VaultProviderContract::new(storage.clone())
        .crosschain_shares
        .read(&user)?;
    if available < required {
        return Err(VaultProviderError::InsufficientCrosschainShares {
            available,
            required,
        }
        .into());
    }
    Ok(())
}

fn ensure_fee(provided: U256, required: U256) -> Result<()> {
    if provided != required {
        return Err(VaultProviderError::CrosschainFeeMismatch { provided, required }.into());
    }
    Ok(())
}

fn record_operation(
    storage: &StorageHandle<'_>,
    operation_id: B256,
    user: Address,
    amount: U256,
    kind: u8,
) -> Result<()> {
    let contract = VaultProviderContract::new(storage.clone());
    if contract.operation_statuses.read(&operation_id)? != 0 {
        return Err(VaultProviderError::CrosschainOperationAlreadyExists.into());
    }
    contract.operation_users.write(&operation_id, user)?;
    contract.operation_amounts.write(&operation_id, amount)?;
    contract.operation_kinds.write(&operation_id, kind)?;
    contract
        .operation_statuses
        .write(&operation_id, STATUS_PENDING)?;
    let pending = contract.pending_crosschain_operations.read()?;
    contract
        .pending_crosschain_operations
        .write(pending + U256::from(1))
}

fn ensure_no_pending_operations(storage: &StorageHandle<'_>) -> Result<()> {
    let pending = VaultProviderContract::new(storage.clone())
        .pending_crosschain_operations
        .read()?;
    if !pending.is_zero() {
        return Err(VaultProviderError::CrosschainOperationsPending(pending).into());
    }
    Ok(())
}

fn decrement_pending_operations(contract: &VaultProviderContract<'_>) -> Result<()> {
    let pending = contract.pending_crosschain_operations.read()?;
    if pending.is_zero() {
        return Err(VaultProviderError::InvalidCrosschainCallback.into());
    }
    contract
        .pending_crosschain_operations
        .write(pending - U256::from(1))
}

fn validate_pending_operation(
    storage: &StorageHandle<'_>,
    operation_id: B256,
    user: Address,
    amount: U256,
    kind: u8,
) -> Result<()> {
    let contract = VaultProviderContract::new(storage.clone());
    let status = contract.operation_statuses.read(&operation_id)?;
    if status == 0 {
        return Err(VaultProviderError::CrosschainOperationNotFound.into());
    }
    if status == STATUS_COMPLETED {
        return Err(VaultProviderError::CrosschainOperationAlreadyCompleted.into());
    }
    if status != STATUS_PENDING
        || contract.operation_users.read(&operation_id)? != user
        || contract.operation_amounts.read(&operation_id)? != amount
        || contract.operation_kinds.read(&operation_id)? != kind
    {
        return Err(VaultProviderError::InvalidCrosschainCallback.into());
    }
    Ok(())
}

fn next_operation_id(
    storage: &StorageHandle<'_>,
    user: Address,
    kind: u8,
    amount: U256,
) -> Result<B256> {
    let contract = VaultProviderContract::new(storage.clone());
    let nonce = contract.crosschain_operation_nonce.read()? + U256::from(1);
    contract.crosschain_operation_nonce.write(nonce)?;
    operation_id(storage, nonce, user, kind, amount)
}

fn preview_operation_id(
    storage: &StorageHandle<'_>,
    user: Address,
    kind: u8,
    amount: U256,
) -> Result<B256> {
    let nonce = VaultProviderContract::new(storage.clone())
        .crosschain_operation_nonce
        .read()?
        + U256::from(1);
    operation_id(storage, nonce, user, kind, amount)
}

fn operation_id(
    storage: &StorageHandle<'_>,
    nonce: U256,
    user: Address,
    kind: u8,
    amount: U256,
) -> Result<B256> {
    Ok(keccak256(
        (
            keccak256("OUTBE_CROSSCHAIN_VAULT_V1"),
            U256::from(storage.chain_id()?),
            SELF,
            nonce,
            user,
            U256::from(kind),
            amount,
        )
            .abi_encode(),
    ))
}

fn deposit_data(
    operation_id: B256,
    user: Address,
    amount: U256,
    acknowledgement_gas_limit: U256,
) -> Vec<u8> {
    (
        U256::from(DEPOSIT_REQUEST),
        operation_id,
        user,
        amount,
        acknowledgement_gas_limit,
    )
        .abi_encode()
}

fn withdraw_request_data(
    operation_id: B256,
    user: Address,
    amount: U256,
    return_gas_limit: U256,
) -> Vec<u8> {
    (
        U256::from(WITHDRAW_REQUEST),
        operation_id,
        user,
        amount,
        return_gas_limit,
    )
        .abi_encode()
}

fn message_bridge_quote(
    storage: &StorageHandle<'_>,
    bridge: Address,
    recipient: Vec<u8>,
    payload: Vec<u8>,
    attributes: Vec<Bytes>,
) -> Result<U256> {
    let ret = storage.staticcall(
        bridge,
        IERC7786Bridge::quoteCall {
            recipient: recipient.into(),
            payload: payload.into(),
            attributes,
        }
        .abi_encode()
        .into(),
    )?;
    IERC7786Bridge::quoteCall::abi_decode_returns(&ret)
        .map_err(|_| VaultProviderError::UndecodableReturn("ERC7786Bridge quote").into())
}

fn token_bridge_quote(
    storage: &StorageHandle<'_>,
    config: &Configuration,
    amount: U256,
    extra_data: Vec<u8>,
    gas_limit: U256,
) -> Result<U256> {
    let ret = storage.staticcall(
        config.token_bridge,
        IERC7786TokenBridge::quoteSendCall {
            destinationDomain: config.destination_domain,
            to: config.remote_provider,
            amount,
            extraData: extra_data.into(),
            gasLimit: gas_limit,
        }
        .abi_encode()
        .into(),
    )?;
    IERC7786TokenBridge::quoteSendCall::abi_decode_returns(&ret)
        .map_err(|_| VaultProviderError::UndecodableReturn("token bridge quoteSend").into())
}

fn erc20_approve(
    storage: &StorageHandle<'_>,
    token: Address,
    spender: Address,
    amount: U256,
) -> Result<()> {
    storage.call(
        token,
        U256::ZERO,
        IERC20::approveCall { spender, amount }.abi_encode().into(),
    )?;
    Ok(())
}

fn erc20_transfer_from(
    storage: &StorageHandle<'_>,
    token: Address,
    from: Address,
    to: Address,
    amount: U256,
) -> Result<()> {
    storage.call(
        token,
        U256::ZERO,
        IERC20::transferFromCall { from, to, amount }
            .abi_encode()
            .into(),
    )?;
    Ok(())
}

fn erc20_transfer(
    storage: &StorageHandle<'_>,
    token: Address,
    to: Address,
    amount: U256,
) -> Result<()> {
    storage.call(
        token,
        U256::ZERO,
        IERC20::transferCall { to, amount }.abi_encode().into(),
    )?;
    Ok(())
}

/// OpenZeppelin InteroperableAddress.formatEvmV1(chainId, address).
pub fn format_evm_v1(chain_id: U256, address: Address) -> Vec<u8> {
    let chain_word = chain_id.to_be_bytes::<32>();
    let first_nonzero = chain_word.iter().position(|byte| *byte != 0).unwrap_or(31);
    let chain_reference = &chain_word[first_nonzero..];

    let mut output = Vec::with_capacity(6 + chain_reference.len() + 20);
    output.extend_from_slice(&[0x00, 0x01, 0x00, 0x00]);
    output.push(chain_reference.len() as u8);
    output.extend_from_slice(chain_reference);
    output.push(20);
    output.extend_from_slice(address.as_slice());
    output
}

fn gas_attributes(gas_limit: U256) -> Vec<Bytes> {
    if gas_limit.is_zero() {
        return Vec::new();
    }
    let selector = keccak256("executionGasLimit(uint256)");
    let mut attribute = Vec::with_capacity(36);
    attribute.extend_from_slice(&selector[..4]);
    attribute.extend_from_slice(&gas_limit.to_be_bytes::<32>());
    vec![attribute.into()]
}
