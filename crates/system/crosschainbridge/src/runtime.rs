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

use crate::constants::ALLOWED_CALLERS;
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
    /// balance pays the dispatch fee.
    pub fn call_remote(&mut self, domain: u32, calls: &[Call]) -> Result<B256> {
        self.require_remote(domain)?;
        if calls.is_empty() {
            return Err(CrosschainBridgeError::EmptyCalls.into());
        }
        let storage = self.storage.clone();
        storage.with_checkpoint(|| self.dispatch(domain, calls))
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

    fn dispatch(&mut self, domain: u32, calls: &[Call]) -> Result<B256> {
        let router = self.hyperlane().ica_router.read()?;
        let fee = self.quote(router, domain)?;
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
        })?;
        Ok(message_id)
    }

    fn quote(&self, router: Address, domain: u32) -> Result<U256> {
        let ret = self.storage.staticcall(
            router,
            IInterchainAccountRouter::quoteGasPaymentCall {
                _destination: domain,
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
