use super::*;

#[test]
fn dispatch_set_authorized_settler_round_trip() {
    with_factory(|s| {
        let settler = address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");
        let data = IIntexFactory::setAuthorizedSettlerCall {
            seriesId: sid(7).into(),
            settler,
        }
        .abi_encode();
        // Caller (holder) is taken from msg.sender, not the calldata.
        precompile::dispatch(s.clone(), &data, holder(), U256::ZERO).unwrap();
        let f = IntexFactoryContract::new(s.clone());
        assert_eq!(
            f.read_authorized_settler(holder(), sid(7)).unwrap(),
            settler
        );
    });
}

#[test]
fn dispatch_rejects_value() {
    with_factory(|s| {
        let settler = address!("0xBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB");
        let data = IIntexFactory::setAuthorizedSettlerCall {
            seriesId: sid(7).into(),
            settler,
        }
        .abi_encode();
        assert!(precompile::dispatch(s.clone(), &data, holder(), U256::from(1)).is_err());
    });
}

#[test]
fn dispatch_mine_promis_routes_to_runtime() {
    with_factory(|s| {
        // Missing series -> the runtime error surfaces through dispatch.
        let data = IIntexFactory::minePromisCall {
            seriesId: sid(7).into(),
            amount: U256::from(1),
            nonce: U256::ZERO,
            mac: alloy_primitives::FixedBytes([0u8; 32]),
            opNonce: 0,
        }
        .abi_encode();
        assert!(precompile::dispatch(s.clone(), &data, holder(), U256::ZERO).is_err());
    });
}
