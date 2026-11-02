//! The owner calls of the owner address: local calls and Interchain Account
//! dispatch, paid from the address balance. The only code that calls another
//! contract as the owner. It runs the calls of the vote actions and the
//! [`Calls`] that `hyperlanecontroller` returns.

use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_sol_types::SolCall;
use outbe_hyperlanecontroller::sol_ext::IInterchainAccountRouter;
use outbe_hyperlanecontroller::HyperlaneControllerContract;
use outbe_primitives::error::Result;
use outbe_primitives::storage::StorageHandle;

use crate::calls::{Calls, RemoteCall};
use crate::errors::CrosschainGovError;
use crate::precompile::ICrosschainGov;
use crate::schema::CrosschainGovContract;

/// Runs the owner calls of the owner address. Use it only in the frame of
/// that address: every call has the frame address as `msg.sender`.
pub struct Executor<'a> {
    gov: CrosschainGovContract<'a>,
    /// The Hyperlane registry, read only: the ICA router.
    hyperlane: HyperlaneControllerContract<'a>,
}

impl<'a> Executor<'a> {
    pub fn new(storage: StorageHandle<'a>) -> Self {
        Self {
            gov: CrosschainGovContract::new(storage.clone()),
            hyperlane: HyperlaneControllerContract::new(storage),
        }
    }

    /// Runs `calls`: the local calls first, then one message for each remote
    /// domain, all under one checkpoint.
    pub fn run(&mut self, calls: Calls) -> Result<()> {
        let storage = self.gov.storage.clone();
        storage.with_checkpoint(|| {
            for call in calls.local {
                self.call_local(call.to, call.value, call.data)?;
            }
            for (domain, remote) in &calls.remote {
                self.call_remote(*domain, remote)?;
            }
            Ok(())
        })
    }

    /// Calls `to` on this chain. The owner balance pays `value`.
    pub fn call_local(&mut self, to: Address, value: U256, data: Bytes) -> Result<Bytes> {
        if to == Address::ZERO {
            return Err(CrosschainGovError::InvalidAddress.into());
        }
        self.require_balance(value)?;
        let storage = self.gov.storage.clone();
        storage.with_checkpoint(|| {
            let ret = self.gov.storage.call(to, value, data)?;
            self.gov
                .emit(ICrosschainGov::LocalCallExecuted { target: to, value })?;
            Ok(ret)
        })
    }

    /// Runs `calls` on `domain` through the Interchain Account. The owner
    /// balance pays the dispatch fee.
    pub fn call_remote(&mut self, domain: u32, calls: &[RemoteCall]) -> Result<B256> {
        self.require_remote(domain)?;
        if calls.is_empty() {
            return Err(CrosschainGovError::EmptyCalls.into());
        }
        let storage = self.gov.storage.clone();
        storage.with_checkpoint(|| self.dispatch(domain, calls))
    }

    /// Records a top-up. The payable route credits the value itself.
    pub fn fund(&mut self, from: Address, amount: U256) -> Result<()> {
        if amount.is_zero() {
            return Err(CrosschainGovError::ZeroFund.into());
        }
        self.gov.emit(ICrosschainGov::Funded { from, amount })
    }

    /// Interchain Account of the owner on `domain`, as the ICA router derives it.
    pub fn remote_account(&self, domain: u32) -> Result<Address> {
        self.require_remote(domain)?;
        let ret = self.gov.storage.staticcall(
            self.hyperlane.ica_router.read()?,
            IInterchainAccountRouter::getRemoteInterchainAccountCall {
                _destination: domain,
                _owner: self.gov.address,
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

    fn dispatch(&mut self, domain: u32, calls: &[RemoteCall]) -> Result<B256> {
        let router = self.hyperlane.ica_router.read()?;
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
        let ret = self.gov.storage.call(
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
        self.gov.emit(ICrosschainGov::RemoteCallDispatched {
            domain,
            messageId: message_id,
            fee,
        })?;
        Ok(message_id)
    }

    fn quote(&self, router: Address, domain: u32) -> Result<U256> {
        let ret = self.gov.storage.staticcall(
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

    /// Requires an initialized Hyperlane registry and a remote `domain`.
    fn require_remote(&self, domain: u32) -> Result<()> {
        if !self.hyperlane.is_initialized()? {
            return Err(CrosschainGovError::NotInitialized.into());
        }
        if domain == 0 {
            return Err(CrosschainGovError::InvalidDomain.into());
        }
        if u64::from(domain) == self.gov.storage.chain_id()? {
            return Err(CrosschainGovError::LocalDomainNotAllowed { domain }.into());
        }
        Ok(())
    }

    fn require_balance(&self, required: U256) -> Result<()> {
        let available = self.gov.storage.balance(self.gov.address)?;
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
