//! Shared `quoteSettlement` Nod fixture for the executor witness tests.

use alloy_primitives::{Address, U256};
use outbe_nod::NodItemState;
use outbe_primitives::signer::OutbeEvmSigner;
use outbe_system_common::time::WorldwideDay;
use outbe_tee::resident::StateKeyHandle;

pub(crate) struct QuoteNodSeed {
    pub nod_item: NodItemState,
    pub nod_id: U256,
    pub user_signer: OutbeEvmSigner,
    pub user_sender: Address,
    pub settlement_asset: Address,
}

/// Build one unsettled Nod and seal it under the resident state key.
pub(crate) fn quote_nod_seed(nod_state_key: &StateKeyHandle, chain_id: u64) -> QuoteNodSeed {
    let worldwide_day = WorldwideDay::new(20_241_220);
    let entry_price_minor = U256::from(450_000_000u64);
    let issued_at = outbe_system_common::time::date_key_to_utc_timestamp(20_200_101).unwrap() + 1;
    let group_key = outbe_nod::test_support::group_key_at(
        &outbe_nod::config::NodParams::TESTNET,
        worldwide_day.value(),
        entry_price_minor,
        840,
    );
    let settlement_asset = Address::repeat_byte(0x71);
    let user_signer = OutbeEvmSigner::from_secret_bytes([2u8; 32]).unwrap();
    let user_sender = user_signer.address();
    let nod_id = outbe_nod::test_support::nod_id_of(user_sender, worldwide_day.value(), 840, 840);
    let mut nod_item = {
        let _enclave = outbe_nod::test_support::scope();
        outbe_nod::test_support::item(
            outbe_nod::test_support::NodItemFixture {
                is_settled: false,
                nod_id,
                owner: user_sender,
                gratis_load_minor: U256::from(1_000_000u64),
                emission_day: worldwide_day.value(),
                group_key,
                issuance_currency: 840,
                reference_currency: 840,
                issued_at,
            },
            entry_price_minor,
        )
    };
    nod_item.encrypted = outbe_nod::nod_encryption::encrypt_nod(
        nod_state_key,
        outbe_core_common::nod_encryption::NodTermsV1 {
            chain_id,
            nod_id,
            owner: user_sender,
            emission_day: worldwide_day.value(),
            entry_price_minor,
            issuance_currency: 840,
            reference_currency: 840,
        },
        U256::from(1_000_000u64),
        U256::from(1_000_000u64),
    )
    .expect("seal nod under the resident state key");
    outbe_nod::test_support::uninstall();
    QuoteNodSeed {
        nod_item,
        nod_id,
        user_signer,
        user_sender,
        settlement_asset,
    }
}
