use crate::world::rpc::*;

#[cfg(feature = "ocomp-integration")]
#[test]
fn encoded_reward_bearing_tribute_plaintext_preserves_waa_and_sra_beneficiaries() {
    let creator = Address::repeat_byte(0x11);
    let waa = Address::repeat_byte(0x22);
    let sra = Address::repeat_byte(0x33);
    let plaintext = encode_reward_bearing_tribute_plaintext(
        creator,
        B256::repeat_byte(0x44),
        "100",
        "0",
        B256::repeat_byte(0x55),
        &[waa],
        &[sra],
    )
    .expect("encode reward-bearing Tribute plaintext");
    let payload: serde_json::Value =
        serde_json::from_slice(&plaintext).expect("decode Tribute plaintext");

    assert_eq!(
        payload["wallet_addresses"],
        serde_json::json!([format!("{waa:#x}")])
    );
    assert_eq!(
        payload["sra_addresses"],
        serde_json::json!([format!("{sra:#x}")])
    );
}
