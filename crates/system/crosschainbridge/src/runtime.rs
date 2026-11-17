//! The owner calls: local calls and Interchain Account dispatch, paid from the
//! owner balance. The only code that calls another contract as the owner. It
//! runs the calls of the vote actions and the `callLocal` and `callRemote`
//! calls of the allowed-caller list. Use these methods only in the frame of the
//! owner address: every call has the frame address as `msg.sender`.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_hyperlanecontroller::sol_ext::IInterchainAccountRouter;
use outbe_hyperlanecontroller::HyperlaneControllerContract;
use outbe_primitives::error::Result;

use crate::constants::{ALLOWED_CALLERS, DEFAULT_REMOTE_GAS_LIMIT};
use crate::errors::CrosschainBridgeError;
use crate::precompile::ICrosschainBridge::{self, Call};
use crate::schema::CrosschainBridgeContract;

impl<'a> CrosschainBridgeContract<'a> {
    /// Requires that `caller` may make the owner call a contract through
    /// `callLocal` and `callRemote`.
    pub fn require_allowed_caller(caller: Address) -> Result<()> {
        if !ALLOWED_CALLERS.contains(&caller) {
            return Err(CrosschainBridgeError::CallerNotAllowed { caller }.into());
        }
        Ok(())
    }

    /// The Hyperlane registry, read only: the ICA router.
    fn hyperlane(&self) -> HyperlaneControllerContract<'a> {
        HyperlaneControllerContract::new(self.storage.clone())
    }

    /// Calls `to` on this chain. The owner balance pays `value`.
    pub fn call_local(&mut self, to: Address, value: U256, data: Bytes) -> Result<Bytes> {
        if to == Address::ZERO {
            return Err(CrosschainBridgeError::InvalidAddress.into());
        }
        self.require_balance(value)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let ret = self.storage.call(to, value, data)?;
            self.emit(ICrosschainBridge::LocalCallExecuted { target: to, value })?;
            Ok(ret)
        })
    }

    /// Runs `calls` on `domain` through the Interchain Account. The owner
    /// balance pays the dispatch fee for [`DEFAULT_REMOTE_GAS_LIMIT`].
    pub fn call_remote(&mut self, domain: u32, calls: &[Call]) -> Result<B256> {
        self.call_remote_with_gas_limit(domain, calls, U256::from(DEFAULT_REMOTE_GAS_LIMIT))
    }

    /// As [`Self::call_remote`], with the dispatch fee for `gas_limit`.
    pub fn call_remote_with_gas_limit(
        &mut self,
        domain: u32,
        calls: &[Call],
        gas_limit: U256,
    ) -> Result<B256> {
        self.require_remote(domain)?;
        if calls.is_empty() {
            return Err(CrosschainBridgeError::EmptyCalls.into());
        }
        if gas_limit.is_zero() {
            return Err(CrosschainBridgeError::InvalidGasLimit.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| self.dispatch(domain, calls, gas_limit))
    }

    /// Records a top-up. The payable route credits the value itself.
    pub fn fund(&mut self, from: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Err(CrosschainBridgeError::ZeroFund.into());
        }
        self.emit(ICrosschainBridge::Funded { from, amount })
    }

    /// Interchain Account of the owner on `domain`, as the ICA router derives it.
    pub fn remote_account(&self, domain: u32) -> Result<Address> {
        self.require_remote(domain)?;
        let ret = self.storage.staticcall(
            self.hyperlane().ica_router.read()?,
            IInterchainAccountRouter::getRemoteInterchainAccountCall {
                _destination: domain,
                _owner: self.address,
            }
            .abi_encode()
            .into(),
        )?;
        IInterchainAccountRouter::getRemoteInterchainAccountCall::abi_decode_returns(&ret).map_err(
            |_| {
                CrosschainBridgeError::UndecodableReturn(
                    "InterchainAccountRouter getRemoteInterchainAccount",
                )
                .into()
            },
        )
    }

    fn dispatch(&mut self, domain: u32, calls: &[Call], gas_limit: U256) -> Result<B256> {
        let router = self.hyperlane().ica_router.read()?;
        let fee = self.quote(router, domain, gas_limit)?;
        self.require_balance(fee)?;
        let calls = calls
            .iter()
            .map(|call| IInterchainAccountRouter::Call {
                to: call.to.into_word(),
                value: call.value,
                data: call.data.clone(),
            })
            .collect();
        let ret = self.storage.call(
            router,
            fee,
            IInterchainAccountRouter::callRemoteCall {
                _destination: domain,
                _calls: calls,
                _hookMetadata: self.hook_metadata(gas_limit),
            }
            .abi_encode()
            .into(),
        )?;
        let message_id = IInterchainAccountRouter::callRemoteCall::abi_decode_returns(&ret)
            .map_err(|_| {
                CrosschainBridgeError::UndecodableReturn("InterchainAccountRouter callRemote")
            })?;
        self.emit(ICrosschainBridge::RemoteCallDispatched {
            domain,
            messageId: message_id,
            fee,
            gasLimit: gas_limit,
        })?;
        Ok(message_id)
    }

    /// Hyperlane StandardHookMetadata with `gas_limit`.
    fn hook_metadata(&self, gas_limit: U256) -> Bytes {
        let mut metadata = vec![0, 1];
        metadata.extend_from_slice(&[0u8; 32]);
        metadata.extend_from_slice(&gas_limit.to_be_bytes::<32>());
        metadata.extend_from_slice(self.address.as_slice());
        metadata.into()
    }

    fn quote(&self, router: Address, domain: u32, gas_limit: U256) -> Result<U256> {
        let ret = self.storage.staticcall(
            router,
            IInterchainAccountRouter::quoteGasPaymentCall {
                _destination: domain,
                _gasLimit: gas_limit,
            }
            .abi_encode()
            .into(),
        )?;
        IInterchainAccountRouter::quoteGasPaymentCall::abi_decode_returns(&ret).map_err(|_| {
            CrosschainBridgeError::UndecodableReturn("InterchainAccountRouter quoteGasPayment")
                .into()
        })
    }

    /// Requires an initialized Hyperlane registry and a remote `domain`.
    fn require_remote(&self, domain: u32) -> Result<()> {
        if !self.hyperlane().is_initialized()? {
            return Err(CrosschainBridgeError::NotInitialized.into());
        }
        if domain == 0 {
            return Err(CrosschainBridgeError::InvalidDomain.into());
        }
        if u64::from(domain) == self.storage.chain_id()? {
            return Err(CrosschainBridgeError::LocalDomainNotAllowed { domain }.into());
        }
        Ok(())
    }

    fn require_balance(&self, required: U256) -> Result<()> {
        let available = self.storage.balance(self.address)?;
        if available < required {
            return Err(CrosschainBridgeError::InsufficientBalance {
                required,
                available,
            }
            .into());
        }
        Ok(())
    }
}
