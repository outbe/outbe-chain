//! Test fixtures for the Hyperlane controller and the CrosschainGov
//! precompile.
//!
//! Cross-contract interaction goes through `HashMapStorageProvider`'s
//! sub-call stubs. `stub_sub_call_at_selector` pins a target's reply per
//! selector. `enable_sub_call_stub()` makes every other sub-call succeed
//! with empty returndata.

use alloy_primitives::{address, Address, Bytes, B256, U256};
use alloy_sol_types::{SolCall, SolInterface, SolValue};
use outbe_primitives::addresses::CROSSCHAIN_GOV_ADDRESS;
use outbe_primitives::error::{PrecompileError, Result};
use outbe_primitives::storage::hashmap::HashMapStorageProvider;
use outbe_primitives::storage::StorageHandle;

use crate::abi::ICrosschainGov::{self, ICrosschainGovCalls};
use crate::schema::HyperlaneControllerContract;
use crate::sol_ext::{IInterchainAccountRouter, IOwnable, IStorageMultisigIsm};
use crate::{Owner, RemoteCall};

pub const CHAIN_ID: u64 = 54_322_345;
pub const LOCAL: u32 = CHAIN_ID as u32;
pub const SEPOLIA: u32 = 11_155_111;
pub const BSC: u32 = 97;

pub fn deployer() -> Address {
    address!("0x00000000000000000000000000000000000000d1")
}
pub fn stranger() -> Address {
    address!("0x00000000000000000000000000000000000000b0")
}
pub fn ica_router() -> Address {
    address!("0x0000000000000000000000000000000000000626")
}
pub fn validator_announce() -> Address {
    address!("0x0000000000000000000000000000000000000627")
}
pub fn local_ism() -> Address {
    address!("0x0000000000000000000000000000000000000151")
}
pub fn sepolia_ism() -> Address {
    address!("0x0000000000000000000000000000000000000152")
}
pub fn bsc_ism() -> Address {
    address!("0x0000000000000000000000000000000000000153")
}
pub fn v(n: u8) -> Address {
    Address::repeat_byte(n)
}

pub fn hook(n: u8) -> Address {
    let mut bytes = [0u8; 20];
    bytes[18] = 0x0a;
    bytes[19] = n;
    Address::from(bytes)
}

pub fn word_addr(value: Address) -> Bytes {
    Bytes::from(value.abi_encode())
}

pub fn provider() -> HashMapStorageProvider {
    HashMapStorageProvider::new(CHAIN_ID)
}

/// An [`Owner`] that records each owner call and does not run it.
#[derive(Debug, Default)]
pub struct RecordingOwner {
    pub local: Vec<(Address, U256, Bytes)>,
    pub remote: Vec<(u32, Vec<RemoteCall>)>,
}

impl Owner for RecordingOwner {
    fn call_local(&mut self, to: Address, value: U256, data: Bytes) -> Result<Bytes> {
        self.local.push((to, value, data));
        Ok(Bytes::new())
    }

    fn call_remote(&mut self, domain: u32, calls: &[RemoteCall]) -> Result<B256> {
        self.remote.push((domain, calls.to_vec()));
        Ok(B256::repeat_byte(0xaa))
    }
}

/// Decodes `data` and runs it with [`crate::dispatch()`] and `owner`.
pub fn dispatch_with(
    storage: StorageHandle<'_>,
    data: &[u8],
    caller: Address,
    owner: &mut impl Owner,
) -> Result<Bytes> {
    let call = ICrosschainGovCalls::abi_decode(data)
        .map_err(|error| PrecompileError::Revert(error.to_string()))?;
    crate::dispatch(storage, call, caller, owner)
}

/// Deployer still owns the local ISM, `0x…EE14` is its pending owner and
/// already owns the ICA router: the state right after `mise run
/// owner:transfer`. The router quotes a zero fee so `initialize` can
/// dispatch the remote `acceptOwnership()` calls without a balance.
pub fn stub_pre_initialize(p: &mut HashMapStorageProvider) {
    StorageHandle::enter(p, |storage| {
        HyperlaneControllerContract::new(storage)
            .bootstrap_admin
            .write(deployer())
            .unwrap();
    });
    p.stub_sub_call_at_selector(
        ica_router(),
        IInterchainAccountRouter::quoteGasPaymentCall::SELECTOR,
        Bytes::from(U256::ZERO.abi_encode()),
    );
    p.stub_sub_call_at_selector(
        ica_router(),
        IInterchainAccountRouter::callRemoteCall::SELECTOR,
        Bytes::from(B256::repeat_byte(0xaa).abi_encode()),
    );
    p.stub_sub_call_at_selector(
        local_ism(),
        IOwnable::ownerCall::SELECTOR,
        word_addr(deployer()),
    );
    p.stub_sub_call_at_selector(
        local_ism(),
        IStorageMultisigIsm::pendingOwnerCall::SELECTOR,
        word_addr(CROSSCHAIN_GOV_ADDRESS),
    );
    p.stub_sub_call_at_selector(
        ica_router(),
        IOwnable::ownerCall::SELECTOR,
        word_addr(CROSSCHAIN_GOV_ADDRESS),
    );
    p.enable_sub_call_stub();
}

/// `initialize` with the Outbe, Sepolia and BSC domains.
pub fn initialize_call() -> Bytes {
    ICrosschainGov::initializeCall {
        icaRouter: ica_router(),
        validatorAnnounce: validator_announce(),
        domains: vec![LOCAL, SEPOLIA, BSC],
        isms: vec![local_ism(), sepolia_ism(), bsc_ism()],
        hooks: vec![hook(1), hook(2), hook(3)],
    }
    .abi_encode()
    .into()
}

/// A provider after `initialize_call`.
pub fn initialized_provider() -> HashMapStorageProvider {
    let mut p = provider();
    stub_pre_initialize(&mut p);
    StorageHandle::enter(&mut p, |storage| {
        dispatch_with(
            storage,
            &initialize_call(),
            deployer(),
            &mut RecordingOwner::default(),
        )
        .unwrap();
    });
    p
}

/// The ICA router quotes `fee`, and the local ISM holds `validators` and
/// `threshold`.
pub fn stub_router_and_ism(
    p: &mut HashMapStorageProvider,
    fee: U256,
    (validators, threshold): (&[Address], u8),
) {
    p.stub_sub_call_at_selector(
        ica_router(),
        IInterchainAccountRouter::quoteGasPaymentCall::SELECTOR,
        Bytes::from(fee.abi_encode()),
    );
    p.stub_sub_call_at_selector(
        ica_router(),
        IInterchainAccountRouter::callRemoteCall::SELECTOR,
        Bytes::from(B256::repeat_byte(0xaa).abi_encode()),
    );
    p.stub_sub_call_at_selector(
        local_ism(),
        IStorageMultisigIsm::validatorsAndThresholdCall::SELECTOR,
        Bytes::from(
            IStorageMultisigIsm::validatorsAndThresholdCall::abi_encode_returns(
                &IStorageMultisigIsm::validatorsAndThresholdReturn {
                    _0: validators.to_vec(),
                    _1: threshold,
                },
            ),
        ),
    );
}

/// The first topic of every event at `0x…EE14`.
pub fn topics(p: &HashMapStorageProvider) -> Vec<B256> {
    p.get_events(CROSSCHAIN_GOV_ADDRESS)
        .iter()
        .map(|log| log.topics()[0])
        .collect()
}

/// The selector of a typed revert.
pub fn revert_selector(err: PrecompileError) -> [u8; 4] {
    match err {
        PrecompileError::RevertBytes(bytes) if bytes.len() >= 4 => {
            bytes[..4].try_into().expect("selector")
        }
        other => panic!("expected typed revert, got {other:?}"),
    }
}
