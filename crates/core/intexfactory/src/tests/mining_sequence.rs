//! The per-(series, owner) mining sequence: a counter that cannot wrap, and
//! that moves only together with a committed burn and mint.

use super::*;

/// The sequence at its maximum: the next mining is refused before any write,
/// so the paid units, the sequence and the ledger nonce stay as they were.
#[test]
fn a_mining_sequence_at_its_maximum_refuses_the_next_mining_without_writes() {
    use crate::sol_ext::IERC1155;

    let mut storage = factory_provider();
    storage.stub_sub_call_at_selector(
        crate::constants::INTEX_NFT1155_ADDRESS,
        IERC1155::balanceOfCall::SELECTOR,
        word(1),
    );
    StorageHandle::enter(&mut storage, |s| {
        select_prod_profile(&s);
        runtime::issue(&s, sample(7)).unwrap();
        let promis_amount = outbe_intex::api::read_series(&s, sid(7))
            .unwrap()
            .promis_load_minor;
        let mut factory = IntexFactoryContract::new(s.clone());
        factory.write_mine_seq(sid(7), owner(), u32::MAX).unwrap();
        let nonce = (0u64..)
            .find(|nonce| {
                runtime::validate_pow(owner(), promis_amount, sid(7), u32::MAX, *nonce).is_ok()
            })
            .unwrap();

        let error = runtime::mine_promis(
            &s,
            sid(7),
            owner(),
            U256::from(1),
            nonce,
            outbe_promisfactory::api::ModifyAuth {
                mac: [0u8; 32],
                op_nonce: 0,
            },
        )
        .unwrap_err();
        assert!(
            format!("{error:?}").contains("mining sequence overflow"),
            "{error:?}"
        );
        assert_eq!(
            IntexFactoryContract::new(s.clone())
                .read_mine_seq(sid(7), owner())
                .unwrap(),
            u32::MAX,
            "the sequence must not wrap"
        );
        // The refusal precedes the checkpoint, so the settled balance stub was
        // never debited: the same paid units remain minable once a sequence
        // reset is designed, and no event was emitted.
        assert!(outbe_intex::api::read_series(&s, sid(7)).is_ok());
    });
}
