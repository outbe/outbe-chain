use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolInterface;
use outbe_primitives::dispatch::{dispatch_call, view};
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::StorageHandle;
use outbe_system_common::addresses::{STABLECOIN_ADDRESS_PREFIX, STABLECOIN_MARKER_CODE};
use outbe_system_common::stablecoin::stablecoin_address;

pub use crate::abi::IStablecoinFactory;
use crate::errors::{AbiDecodeError, StablecoinFactoryError};
use crate::schema::StablecoinFactoryContract;

/// Selectors on this precompile that accept native value. The route table binds
/// this to the address's `ValuePolicy` at compile time, so a selector added here
/// without flipping the route fails the build.
pub const PAYABLE_SELECTORS: &[[u8; 4]] = &[];

pub fn dispatch(
    storage: StorageHandle<'_>,
    data: &[u8],
    _caller: Address,
    value: U256,
) -> Result<Bytes> {
    if !value.is_zero() {
        return Err(StablecoinFactoryError::UnexpectedValue { value }.into());
    }
    dispatch_call(
        data,
        |input| -> std::result::Result<_, AbiDecodeError> {
            let call = IStablecoinFactory::IStablecoinFactoryCalls::abi_decode(input)?;
            if call.abi_encode().as_slice() != input {
                return Err(AbiDecodeError::NonCanonicalCalldata);
            }
            Ok(call)
        },
        |call| {
            use IStablecoinFactory::IStablecoinFactoryCalls::*;

            let factory = StablecoinFactoryContract::new(storage);
            match call {
                tokenCount(call) => view(call, |_| factory.token_count()),
                listTokens(call) => view(call, |call| factory.list_tokens(call.offset, call.limit)),
                tokenById(call) => view(call, |call| factory.token_by_id(call.tokenId)),
                tokenByTicker(call) => view(call, |call| factory.token_by_ticker(&call.ticker)),
                tokenIdOf(call) => view(call, |call| factory.token_id_of(call.token)),
                isStablecoin(call) => view(call, |call| factory.is_stablecoin(call.token)),
                predictTokenAddress(call) => view(call, |call| {
                    factory
                        .predict_token_address(call.issuer, &call.ticker)
                        .map(
                            |(token_id, token)| IStablecoinFactory::predictTokenAddressReturn {
                                tokenId: token_id,
                                token,
                            },
                        )
                }),
            }
        },
    )
}

/// Errors that occur when a route dispatches a stablecoin-class call.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrecompileRouteError {
    /// The callee is in the stablecoin prefix and is not a Factory token.
    #[error("unregistered stablecoin address")]
    UnregisteredStablecoinAddress,
    /// A Factory token has no path that accepts native value.
    #[error("stablecoin token cannot receive native value")]
    StablecoinCannotReceiveNativeValue,
    /// The Factory registry could not be read during a plain native transfer.
    #[error("stablecoin registry unavailable")]
    StablecoinRegistryUnavailable,
    /// The Factory token id does not derive the callee.
    #[error("stablecoin Factory reverse id does not derive callee {token}")]
    StablecoinReverseIdMismatch {
        /// Callee that failed the reverse-id check.
        token: Address,
    },
    /// The account bytecode is not the stablecoin marker.
    #[error("stablecoin {token} marker code mismatch")]
    StablecoinMarkerCodeMismatch {
        /// Callee whose code is not the marker.
        token: Address,
    },
    /// The token account has no V1 schema.
    #[error("stablecoin {token} schema is not initialized for V1")]
    StablecoinSchemaNotInitialized {
        /// Callee whose schema read reverted.
        token: Address,
    },
    /// The token identity does not match the Factory registration.
    #[error("stablecoin {token} identity does not match Factory registration")]
    StablecoinIdentityMismatch {
        /// Callee whose identity does not match.
        token: Address,
    },
}

impl From<PrecompileRouteError> for PrecompileError {
    fn from(error: PrecompileRouteError) -> Self {
        // Every route failure is a user revert. The variant text is the reason.
        Self::Revert(error.to_string())
    }
}

/// Dispatches a call whose callee falls in the stablecoin address class.
pub fn stablecoin_class_dispatch(
    storage: StorageHandle<'_>,
    token: Address,
    data: &[u8],
    caller: Address,
    value: U256,
) -> Result<Bytes> {
    let plain_native_transfer = data.is_empty() && !value.is_zero();

    // A plain native send must not be able to abort the whole transaction. A
    // registry error is fatal to a token call, but here it degrades to a revert.
    // Thus the frame simply rolls back and the value returns to the sender.
    let registered = read_stablecoin_registration(&storage, token, plain_native_transfer)?;

    let Some(factory_token_id) = registered else {
        // Reserving the address class must not make native value unspendable at
        // an address the Factory never issued. An externally owned account
        // whose address happens to fall in the prefix must still receive plain
        // transfers. This uses only revm's ordinary CALL balance semantics. It
        // invokes no token ABI and falls through to no account bytecode.
        if plain_native_transfer {
            return Ok(Bytes::new());
        }
        return Err(PrecompileRouteError::UnregisteredStablecoinAddress.into());
    };

    // A Factory-derived token address has no key and no selector that moves
    // native balance, so accepting a plain transfer there would strand the value
    // permanently. Refuse it instead, and let the frame's checkpoint return the
    // funds to the sender.
    if plain_native_transfer {
        return Err(PrecompileRouteError::StablecoinCannotReceiveNativeValue.into());
    }

    authenticate_registered_stablecoin(&storage, token, factory_token_id)?;

    outbe_stablecoin::precompile::dispatch(storage, token, data, caller, value)
}

/// Reads the Factory token id for `token`.
///
/// A registry failure during a plain native transfer becomes
/// [`PrecompileRouteError::StablecoinRegistryUnavailable`]. Any other failure
/// is returned unchanged.
pub fn read_stablecoin_registration(
    storage: &StorageHandle<'_>,
    token: Address,
    plain_native_transfer: bool,
) -> Result<Option<B256>> {
    match crate::StablecoinFactoryApi::token_id_of(storage.clone(), token) {
        Ok(registered) => Ok(registered),
        Err(_) if plain_native_transfer => {
            Err(PrecompileRouteError::StablecoinRegistryUnavailable.into())
        }
        Err(error) => Err(error),
    }
}

/// Checks that `token` is the Factory registration named by `factory_token_id`.
pub fn authenticate_registered_stablecoin(
    storage: &StorageHandle<'_>,
    token: Address,
    factory_token_id: B256,
) -> Result<()> {
    let expected_address = stablecoin_address(factory_token_id, STABLECOIN_ADDRESS_PREFIX);
    if expected_address != token {
        return Err(PrecompileRouteError::StablecoinReverseIdMismatch { token }.into());
    }

    storage.with_account_info(token, |info| {
        let marker = info
            .code
            .as_ref()
            .map(revm::state::Bytecode::original_bytes)
            .unwrap_or_default();
        if marker.as_ref() != STABLECOIN_MARKER_CODE {
            return Err(PrecompileRouteError::StablecoinMarkerCodeMismatch { token }.into());
        }
        Ok(())
    })?;

    if let Err(error) = outbe_stablecoin::api::schema_version(storage.clone(), token) {
        if crate::vote_target::is_revert_error(&error) {
            return Err(PrecompileRouteError::StablecoinSchemaNotInitialized { token }.into());
        }
        return Err(error);
    }

    let identity = outbe_stablecoin::api::identity(storage.clone(), token)?;
    if identity.token_id != factory_token_id {
        return Err(PrecompileRouteError::StablecoinIdentityMismatch { token }.into());
    }

    Ok(())
}
