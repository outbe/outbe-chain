use super::*;

use alloy_primitives::Bytes;
use alloy_sol_types::{sol, SolCall, SolEvent, SolValue};
use outbe_core_common::addresses::{TARGET_ROUTER_ADDRESS, WCOEN_ADDRESS, WCOEN_BRIDGE_ADDRESS};
use outbe_promislimit::{BurnSource, PromisLimitContract};
use outbe_system_common::units::UNITS_PER_PROTOCOL_MINOR;

use crate::errors::IntexFactoryError;

sol! {
    function transfer(address dst, uint256 wad) external returns (bool);
    function transferFrom(address src, address dst, uint256 wad) external returns (bool);
}

const DAY: u32 = 7;
const SOURCE: u32 = 10;

/// `InteroperableAddress.formatEvmV1(chain, account)`.
fn interop(chain: u32, account: Address) -> Vec<u8> {
    let reference: Vec<u8> = chain
        .to_be_bytes()
        .into_iter()
        .skip_while(|byte| *byte == 0)
        .collect();
    let mut out = vec![0x00, 0x01, 0x00, 0x00, reference.len() as u8];
    out.extend_from_slice(&reference);
    out.push(20);
    out.extend_from_slice(account.as_slice());
    out
}

/// A factory whose day `DAY` started on `chains`, with WCOEN stubbed to accept every call.
fn proceeds_provider(chains: &[u32]) -> HashMapStorageProvider {
    let mut storage = factory_provider();
    storage.stub_sub_call_at(WCOEN_ADDRESS, Bytes::from(true.abi_encode()));
    StorageHandle::enter(&mut storage, |s| {
        let list = IntexFactoryContract::new(s.clone())
            .day_targets
            .list(&EmissionDate::new(DAY));
        for &chain in chains {
            list.push(chain).unwrap();
        }
    });
    storage
}

/// The native COEN that the WCOEN transfer to address 0 leaves there.
fn unwrap_at_zero(s: &StorageHandle<'_>, amount: U256) {
    s.increase_balance(Address::ZERO, amount).unwrap();
}

fn crosschain(amount: U256, source: u32, from: Address) -> (Vec<u8>, Vec<u8>, U256, u32) {
    (
        interop(source, from),
        (DAY, 3u32).abi_encode_params(),
        amount,
        source,
    )
}

fn receive(
    s: &StorageHandle<'_>,
    caller: Address,
    amount: U256,
    source: u32,
    from: Address,
) -> outbe_primitives::error::Result<()> {
    let (from, extra_data, amount, src_chain_id) = crosschain(amount, source, from);
    runtime::receive_crosschain_proceeds(
        s,
        caller,
        runtime::CrosschainProceeds {
            src_chain_id,
            from: &from,
            amount,
            extra_data: &extra_data,
        },
    )
}

fn pool(s: &StorageHandle<'_>) -> U256 {
    PromisLimitContract::new(s.clone()).promis_limit().unwrap()
}

fn burned_events(
    storage: &HashMapStorageProvider,
) -> Vec<IIntexFactory::IntexAuctionProceedsBurned> {
    storage
        .get_events(INTEX_FACTORY_ADDRESS)
        .iter()
        .filter_map(|log| IIntexFactory::IntexAuctionProceedsBurned::decode_log_data(log).ok())
        .collect()
}

fn wcoen_calls(storage: &HashMapStorageProvider) -> Vec<Bytes> {
    storage
        .sub_call_inputs()
        .iter()
        .filter(|input| input.target == WCOEN_ADDRESS)
        .map(|input| input.calldata.clone())
        .collect()
}

fn revert_of(error: IntexFactoryError) -> String {
    outbe_primitives::error::PrecompileError::from(error).to_string()
}

#[test]
fn crosschain_proceeds_burn_the_wcoen_under_the_proceeds_code() {
    let amount = U256::from(3u64) * UNITS_PER_PROTOCOL_MINOR;
    let mut storage = proceeds_provider(&[SOURCE]);
    StorageHandle::enter(&mut storage, |s| {
        unwrap_at_zero(&s, amount);
        let before = pool(&s);
        receive(
            &s,
            WCOEN_BRIDGE_ADDRESS,
            amount,
            SOURCE,
            TARGET_ROUTER_ADDRESS,
        )
        .unwrap();
        assert_eq!(pool(&s), before + U256::from(3u64));
        assert_eq!(s.balance(Address::ZERO).unwrap(), U256::ZERO);
    });

    assert_eq!(
        wcoen_calls(&storage),
        vec![Bytes::from(
            transferCall {
                dst: Address::ZERO,
                wad: amount
            }
            .abi_encode()
        )]
    );
    assert_eq!(
        burned_events(&storage),
        vec![IIntexFactory::IntexAuctionProceedsBurned {
            emissionDay: DAY,
            srcChainId: SOURCE,
            pageIndex: 3,
            amountMinor: amount,
        }]
    );
}

#[test]
fn coen_below_one_minor_stays_on_address_zero() {
    let rest = U256::from(7u64);
    let amount = UNITS_PER_PROTOCOL_MINOR + rest;
    let mut storage = proceeds_provider(&[SOURCE]);
    StorageHandle::enter(&mut storage, |s| {
        unwrap_at_zero(&s, amount);
        let before = pool(&s);
        receive(
            &s,
            WCOEN_BRIDGE_ADDRESS,
            amount,
            SOURCE,
            TARGET_ROUTER_ADDRESS,
        )
        .unwrap();
        assert_eq!(pool(&s), before + U256::ONE);
        assert_eq!(s.balance(Address::ZERO).unwrap(), rest);
        assert_eq!(
            PromisLimitContract::new(s.clone())
                .burn_rest(BurnSource::IntexAuctionProceeds)
                .unwrap(),
            U256::ZERO
        );
    });
}

#[test]
fn crosschain_proceeds_come_only_from_the_wcoen_bridge() {
    with_factory(|s| {
        let err = receive(&s, owner(), U256::ONE, SOURCE, TARGET_ROUTER_ADDRESS).unwrap_err();
        assert_eq!(
            err.to_string(),
            revert_of(IntexFactoryError::UnauthorizedProceedsCaller(owner()))
        );
    });
}

#[test]
fn crosschain_proceeds_come_only_from_a_chain_of_the_day() {
    let mut storage = proceeds_provider(&[SOURCE]);
    StorageHandle::enter(&mut storage, |s| {
        let err = receive(
            &s,
            WCOEN_BRIDGE_ADDRESS,
            U256::ONE,
            20,
            TARGET_ROUTER_ADDRESS,
        )
        .unwrap_err();
        assert_eq!(
            err.to_string(),
            revert_of(IntexFactoryError::UnexpectedProceedsSource(20))
        );
    });
    assert!(burned_events(&storage).is_empty());
}

#[test]
fn crosschain_proceeds_come_only_from_target_router() {
    let mut storage = proceeds_provider(&[SOURCE]);
    StorageHandle::enter(&mut storage, |s| {
        let err = receive(&s, WCOEN_BRIDGE_ADDRESS, U256::ONE, SOURCE, owner()).unwrap_err();
        assert_eq!(
            err.to_string(),
            revert_of(IntexFactoryError::UnauthorizedProceedsSender(
                interop(SOURCE, owner()).into()
            ))
        );
    });
}

#[test]
fn local_proceeds_take_the_approved_wcoen_and_burn_it() {
    let amount = U256::from(2u64) * UNITS_PER_PROTOCOL_MINOR;
    let local = CHAIN_ID as u32;
    let mut storage = proceeds_provider(&[local]);
    StorageHandle::enter(&mut storage, |s| {
        unwrap_at_zero(&s, amount);
        let before = pool(&s);
        runtime::receive_local_proceeds(&s, TARGET_ROUTER_ADDRESS, DAY, 1, amount).unwrap();
        assert_eq!(pool(&s), before + U256::from(2u64));
    });
    assert_eq!(
        wcoen_calls(&storage)[0],
        Bytes::from(
            transferFromCall {
                src: TARGET_ROUTER_ADDRESS,
                dst: INTEX_FACTORY_ADDRESS,
                wad: amount
            }
            .abi_encode()
        )
    );
    assert_eq!(burned_events(&storage)[0].srcChainId, local);
}

#[test]
fn local_proceeds_come_only_from_target_router() {
    let mut storage = proceeds_provider(&[CHAIN_ID as u32]);
    StorageHandle::enter(&mut storage, |s| {
        let err = runtime::receive_local_proceeds(&s, owner(), DAY, 0, U256::ONE).unwrap_err();
        assert_eq!(
            err.to_string(),
            revert_of(IntexFactoryError::UnauthorizedProceedsCaller(owner()))
        );
    });
}

#[test]
fn zero_proceeds_are_refused() {
    let mut storage = proceeds_provider(&[SOURCE]);
    StorageHandle::enter(&mut storage, |s| {
        let err = receive(
            &s,
            WCOEN_BRIDGE_ADDRESS,
            U256::ZERO,
            SOURCE,
            TARGET_ROUTER_ADDRESS,
        )
        .unwrap_err();
        assert_eq!(err.to_string(), revert_of(IntexFactoryError::ZeroAmount));
    });
}

#[test]
fn the_token_hook_answers_its_selector() {
    use crate::precompile::dispatch;

    let amount = UNITS_PER_PROTOCOL_MINOR;
    let (from, extra_data, _, _) = crosschain(amount, SOURCE, TARGET_ROUTER_ADDRESS);
    let data = IIntexFactory::onCrosschainTokensReceivedCall {
        srcChainId: SOURCE,
        from: from.into(),
        amount,
        extraData: extra_data.into(),
    }
    .abi_encode();
    let mut storage = proceeds_provider(&[SOURCE]);
    StorageHandle::enter(&mut storage, |s| {
        unwrap_at_zero(&s, amount);
        let returned = dispatch(s.clone(), &data, WCOEN_BRIDGE_ADDRESS, U256::ZERO).unwrap();
        assert_eq!(
            IIntexFactory::onCrosschainTokensReceivedCall::abi_decode_returns(&returned).unwrap(),
            IIntexFactory::onCrosschainTokensReceivedCall::SELECTOR
        );
    });
}

/// No selector of IntexFactory takes value, so a funded call is refused before the runtime.
#[test]
fn every_selector_refuses_native_value() {
    use crate::precompile::dispatch;

    let calls = [IIntexFactory::settleIntexCall {
        intexId: U256::ZERO,
        owner: Address::ZERO,
        units: U256::ZERO,
        asset: Address::ZERO,
        snapshotId: U256::ZERO,
        amountCapMinor: U256::ZERO,
    }
    .abi_encode()];

    let mut provider = HashMapStorageProvider::new(1);
    StorageHandle::enter(&mut provider, |storage| {
        for data in &calls {
            let funded = dispatch(storage.clone(), data, Address::ZERO, U256::from(1u64));
            assert!(
                matches!(
                    funded,
                    Err(outbe_primitives::error::PrecompileError::Revert(ref message))
                        if message == "non-payable function called with value"
                ),
                "a funded call must be refused, got {funded:?}"
            );
        }
    });
}
