//! The owner calls of `0x...EE14`.
//!
//! - `call_local(to, value, data)`: a call on Outbe from `0x...EE14`.
//! - `call_remote(domain, calls)`: one Interchain Account message to another
//!   chain. It reads the ICA router from the Hyperlane controller, quotes the
//!   fee, checks the balance and calls `callRemote`.
//!
//! Both run in the frame of `0x...EE14`, so `msg.sender` is `0x...EE14` on
//! Outbe and its Interchain Account on the remote chain. Another precompile
//! that calls these functions has a different `msg.sender` and fails the
//! owner check of the target.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_hyperlanecontroller::abi::ICrosschainGov;
use outbe_hyperlanecontroller::sol_ext::IInterchainAccountRouter;
use outbe_hyperlanecontroller::{HyperlaneControllerContract, Owner, RemoteCall};
use outbe_primitives::error::Result;

use crate::errors::CrosschainGovError;
use crate::schema::CrosschainGov;

impl Owner for CrosschainGov<'_> {
    /// Calls `to` on Outbe. A non-zero `value` comes from the precompile
    /// balance.
    fn call_local(&mut self, to: Address, value: U256, data: Bytes) -> Result<Bytes> {
        if to == Address::ZERO {
            return Err(CrosschainGovError::InvalidAddress.into());
        }
        self.require_balance(value)?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| {
            let ret = self.storage.call(to, value, data)?;
            self.emit(ICrosschainGov::LocalCallExecuted { target: to, value })?;
            Ok(ret)
        })
    }

    /// Sends `calls` to a remote `domain` in one Interchain Account message.
    /// The IGP quote comes from the precompile balance.
    fn call_remote(&mut self, domain: u32, calls: &[RemoteCall]) -> Result<B256> {
        let hyperlane = self.hyperlane();
        hyperlane.require_initialized()?;
        if calls.is_empty() {
            return Err(CrosschainGovError::EmptyCalls.into());
        }
        hyperlane.require_remote_domain(domain)?;
        let router = hyperlane.ica_router.read()?;
        let storage = self.storage.clone();
        storage.with_checkpoint(|| self.dispatch_remote(router, domain, calls))
    }
}

impl<'storage> CrosschainGov<'storage> {
    /// The Hyperlane controller state in the same account.
    pub(crate) fn hyperlane(&self) -> HyperlaneControllerContract<'storage> {
        HyperlaneControllerContract::new(self.storage.clone())
    }

    /// Records a top-up. The payable route credits the value itself.
    pub fn fund(&mut self, from: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Err(CrosschainGovError::ZeroFund.into());
        }
        self.emit(ICrosschainGov::Funded { from, amount })
    }

    /// Interchain Account of `0x...EE14` on `domain`, as the ICA router
    /// derives it.
    pub fn remote_account(&self, domain: u32) -> Result<Address> {
        let hyperlane = self.hyperlane();
        hyperlane.require_initialized()?;
        hyperlane.require_remote_domain(domain)?;
        let ret = self.storage.staticcall(
            hyperlane.ica_router.read()?,
            IInterchainAccountRouter::getRemoteInterchainAccountCall {
                _destination: domain,
                _owner: self.address,
            }
            .abi_encode()
            .into(),
        )?;
        IInterchainAccountRouter::getRemoteInterchainAccountCall::abi_decode_returns(&ret).map_err(
            |_| {
                CrosschainGovError::UndecodableReturn(
                    "InterchainAccountRouter getRemoteInterchainAccount",
                )
                .into()
            },
        )
    }

    fn dispatch_remote(
        &mut self,
        router: Address,
        domain: u32,
        calls: &[RemoteCall],
    ) -> Result<B256> {
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
                CrosschainGovError::UndecodableReturn("InterchainAccountRouter callRemote")
            })?;
        self.emit(ICrosschainGov::RemoteCallDispatched {
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
            CrosschainGovError::UndecodableReturn("InterchainAccountRouter quoteGasPayment").into()
        })
    }

    fn require_balance(&self, required: U256) -> Result<()> {
        let available = self.storage.balance(self.address)?;
        if available < required {
            return Err(CrosschainGovError::InsufficientBalance {
                required,
                available,
            }
            .into());
        }
        Ok(())
    }
}
