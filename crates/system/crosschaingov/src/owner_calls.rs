//! The owner calls of the owner address: local calls and Interchain Account
//! dispatch, paid from the address balance. `hyperlanecontroller` makes its
//! writes through them, and so do the vote actions of this crate.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use outbe_hyperlanecontroller::errors::HyperlaneControllerError;
use outbe_hyperlanecontroller::precompile::IHyperlaneController;
use outbe_hyperlanecontroller::sol_ext::IInterchainAccountRouter;
use outbe_hyperlanecontroller::{HyperlaneControllerContract, OwnerCalls, RemoteCall};

/// Runs the owner calls of the owner address. Use it only in the frame of
/// that address: every call has the frame address as `msg.sender`.
pub struct Owner<'a> {
    controller: HyperlaneControllerContract<'a>,
}

impl<'a> Owner<'a> {
    pub fn new(storage: StorageHandle<'a>) -> Self {
        Self {
            controller: HyperlaneControllerContract::new(storage),
        }
    }

    /// Records a top-up. The payable route credits the value itself.
    pub fn fund(&mut self, from: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Err(HyperlaneControllerError::ZeroFund.into());
        }
        self.controller
            .emit(IHyperlaneController::Funded { from, amount })
    }

    /// Interchain Account of the owner on `domain`, as the ICA router derives it.
    pub fn remote_account(&self, domain: u32) -> Result<Address> {
        let controller = &self.controller;
        controller.require_initialized()?;
        controller.require_remote_domain(domain)?;
        let ret = controller.storage.staticcall(
            controller.ica_router.read()?,
            IInterchainAccountRouter::getRemoteInterchainAccountCall {
                _destination: domain,
                _owner: controller.address,
            }
            .abi_encode()
            .into(),
        )?;
        IInterchainAccountRouter::getRemoteInterchainAccountCall::abi_decode_returns(&ret).map_err(
            |_| {
                HyperlaneControllerError::UndecodableReturn(
                    "InterchainAccountRouter getRemoteInterchainAccount",
                )
                .into()
            },
        )
    }

    fn dispatch(&mut self, domain: u32, calls: &[RemoteCall]) -> Result<B256> {
        let router = self.controller.ica_router.read()?;
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
        let ret = self.controller.storage.call(
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
                HyperlaneControllerError::UndecodableReturn("InterchainAccountRouter callRemote")
            })?;
        self.controller
            .emit(IHyperlaneController::RemoteCallDispatched {
                domain,
                messageId: message_id,
                fee,
            })?;
        Ok(message_id)
    }

    fn quote(&self, router: Address, domain: u32) -> Result<U256> {
        let ret = self.controller.storage.staticcall(
            router,
            IInterchainAccountRouter::quoteGasPaymentCall {
                _destination: domain,
            }
            .abi_encode()
            .into(),
        )?;
        IInterchainAccountRouter::quoteGasPaymentCall::abi_decode_returns(&ret).map_err(|_| {
            HyperlaneControllerError::UndecodableReturn("InterchainAccountRouter quoteGasPayment")
                .into()
        })
    }

    fn require_balance(&self, required: U256) -> Result<()> {
        let available = self.controller.storage.balance(self.controller.address)?;
        if available < required {
            return Err(HyperlaneControllerError::InsufficientBalance {
                required,
                available,
            }
            .into());
        }
        Ok(())
    }
}

impl OwnerCalls for Owner<'_> {
    fn call_local(&mut self, to: Address, value: U256, data: Bytes) -> Result<Bytes> {
        if to == Address::ZERO {
            return Err(HyperlaneControllerError::InvalidAddress.into());
        }
        self.require_balance(value)?;
        let storage = self.controller.storage.clone();
        storage.with_checkpoint(|| {
            let ret = self.controller.storage.call(to, value, data)?;
            self.controller
                .emit(IHyperlaneController::LocalCallExecuted { target: to, value })?;
            Ok(ret)
        })
    }

    fn call_remote(&mut self, domain: u32, calls: &[RemoteCall]) -> Result<B256> {
        self.controller.require_initialized()?;
        if calls.is_empty() {
            return Err(HyperlaneControllerError::EmptyCalls.into());
        }
        self.controller.require_remote_domain(domain)?;
        let storage = self.controller.storage.clone();
        storage.with_checkpoint(|| self.dispatch(domain, calls))
    }
}
