//! Independent Python JSON/Keccak known answers; never calculated by the tested builder.
use super::fixtures::*;
use alloy_primitives::{hex, keccak256, B256, U256};
use outbe_enclave::{nod_mine as nod, tribute_day as day, tribute_v2 as tribute};

#[test]
fn public_inputs_hash_vectors() {
    assert_eq!(
        hex::encode(tribute::encrypted_offer_inputs_hash(
            1,
            &parse::<Vec<_>>(OFFERS)
        )),
        "02bcf9db85fbf18c980c852413fb3646591eaeb19bda47f4d482d4eb87a142da"
    );
    assert_eq!(
        hex::encode(tribute::tribute_read_inputs_hash(&[parse(TRIBUTE)]).unwrap()),
        "6b7a72b47596a37a2b280407811cf4eba4c7e7fb50202ec1839374824b4919c2"
    );
    assert_eq!(
        hex::encode(day::day_operation_inputs_hash(&parse(DAY_REQUEST)).unwrap()),
        "f5d4f96ffedb06f87dcb05ced6100c577a2a483508cb36f6b9671592675ca93c"
    );
    assert_eq!(
        hex::encode(day::day_read_inputs_hash(&parse(DAY)).unwrap()),
        "d45b2948fc8a8956c0c157510ec7257e75988d916ee59c33c215026764770339"
    );
    assert_eq!(
        hex::encode(day::lysis_fractions_inputs_hash(&parse(LYSIS_REQUEST)).unwrap()),
        "c2f759acfd63bb0a47ab9910ffb24bdf72bcf5e024198897767e51da450ba256"
    );
    assert_eq!(
        hex::encode(day::day_fractions_read_inputs_hash(&parse(FRACTIONS)).unwrap()),
        "dc5a607fa41238046b3f7aa47e2ba92c5e6e3184719199b3943199c815dfbabb"
    );
    assert_eq!(
        hex::encode(nod::inputs_hash(&parse(MINE_REQUEST)).unwrap()),
        "5b2a54224b14481d7dd879f3b7a37547dcfae32f9190da9bd114d0b6f7ba82f2"
    );
    assert_eq!(
        hex::encode(nod::nod_read_hash(&parse(NOD)).unwrap()),
        "ac7fe9f255dcda9ad3ce660a537058a7714eba59ef4447e3da4ea07f0a50af25"
    );
    assert_eq!(
        hex::encode(nod::nod_group_load_inputs_hash(&parse(LOAD_REQUEST)).unwrap()),
        "629681c2ce3d153ca94f850dd3e820e1f2a051eda3aa2f1ffe5bdee95e2cc499"
    );
    assert_eq!(
        hex::encode(nod::nod_group_load_read_hash(&parse(LOAD)).unwrap()),
        "3024c13b2ff8b344bdb01060d776d753c983fd4dbf3886edd72463755d320e55"
    );
}

#[test]
fn signed_result_preimage_vectors() {
    let inputs = B256::repeat_byte(0x11);
    assert_eq!(
        hex::encode(keccak256(
            tribute::encrypted_offer_attestation_preimage(inputs, &parse::<Vec<_>>(OFFER_RESULT))
                .unwrap()
        )),
        "3ea07cc7a55d2edad6dbe151f9aafa11e0d747d53e3de6f74bf53cc981f731d4"
    );
    assert_eq!(
        hex::encode(keccak256(
            tribute::tribute_read_attestation_preimage(inputs, &parse::<Vec<_>>(AMOUNTS)).unwrap()
        )),
        "b910fa6c3c823139f11bfb34b2286be8fc34185915ee472459609d456db103b3"
    );
    assert_eq!(
        hex::encode(keccak256(
            day::day_operation_attestation_preimage(inputs, &parse(DAY)).unwrap()
        )),
        "6701f957fffc13a8950075244b16c6716f2074e386ba405c0b03979b5fd39e50"
    );
    assert_eq!(
        hex::encode(keccak256(
            day::lysis_fractions_attestation_preimage(inputs, &parse(LYSIS_RESULT)).unwrap()
        )),
        "622f45e4dabf6b252646cf70bd1ef3dcf288690f672a39728f01e1c796adb306"
    );
    assert_eq!(
        hex::encode(keccak256(
            day::day_fractions_read_attestation_preimage(
                inputs,
                &std::collections::BTreeMap::from([(3, 17), (7, 19)])
            )
            .unwrap()
        )),
        "373b9f97f36e28c4a36ea85164a0b8b5d26f400e086e6ac6f0d126c7b83165a4"
    );
    assert_eq!(
        hex::encode(keccak256(
            nod::nod_group_load_attestation_preimage(inputs, &parse(LOAD)).unwrap()
        )),
        "91df1d4253a46b88e14e87bf7bc77333ff2c3dae42a2b50f7e1030355ad21f5c"
    );
    assert_eq!(
        hex::encode(keccak256(day::day_read_attestation_preimage(
            inputs,
            U256::from(17)
        ))),
        "bb245e683dded9cd18614e433a684270492b529d7368a0572c58895fd99bac38"
    );
    assert_eq!(
        hex::encode(keccak256(nod::nod_read_preimage(inputs, U256::from(17)))),
        "9460df2fedef56a57d91681c662d511e12537af23d09db9f44d31e5a3ea7ba9d"
    );
    assert_eq!(
        hex::encode(keccak256(nod::nod_group_load_read_preimage(
            inputs,
            U256::from(17)
        ))),
        "f6911971f7f3a279f95d3fc0545722bf14c68c4c8614c693b825f2ace34b2cce"
    );
    assert_eq!(
        hex::encode(keccak256(
            nod::result_preimage(&parse(MINE_RESULT)).unwrap()
        )),
        "52b8387221eb04c32aa367f8aeee2ac62868682b694b60c267d52cad90067fda"
    );
}
