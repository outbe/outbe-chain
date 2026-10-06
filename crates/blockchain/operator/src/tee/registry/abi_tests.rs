use super::*;
use alloy_sol_types::SolValue;

// Independent V1 JSON and ABI words. Neither expectation uses the mapper.
const LEGACY_RENEWAL_JSON: &str = concat!(
    r#"{"nodeIdHash":"0x1111111111111111111111111111111111111111111111111111111111111111""#,
    r#","enclaveId":"0x1212121212121212121212121212121212121212121212121212121212121212""#,
    r#","bindingId":"0x1313131313131313131313131313131313131313131313131313131313131313""#,
    r#","intentHash":"0x1414141414141414141414141414141414141414141414141414141414141414""#,
    r#","evidenceHash":"0x1515151515151515151515151515151515151515151515151515151515151515""#,
    r#","policyHash":"0x1616161616161616161616161616161616161616161616161616161616161616""#,
    r#","bindingVersion":18446744073709551615"#,
    r#","registrationVersion":72623859790382856"#,
    r#","renewalNonce":1230066625199609624"#,
    r#","transitionNonce":2387509390608836392"#,
    r#","leaseStartedAt":3544952156018063160"#,
    r#","validUntil":4702394921427289928"#,
    r#","collateralValidUntil":5859837686836516696"#,
    r#","recipientX25519":"0x1717171717171717171717171717171717171717171717171717171717171717""#,
    r#","attestationEd25519":"0x1818181818181818181818181818181818181818181818181818181818181818""#,
    r#","noiseResponderX25519":"0x1919191919191919191919191919191919191919191919191919191919191919""#,
    r#","mrenclave":"0x1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a""#,
    r#","mrsigner":"0x1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b1b""#,
    r#","isvProdId":24930"#,
    r#","isvSvn":29042"#,
    r#","platformTcbStatus":129"#,
    r#","verdictHash":"0x1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c1c""#,
    r#","nodeHostAuthorizationHash":"0x1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d"}"#,
);

fn legacy_binding_bytes() -> Vec<u8> {
    let integer = |value: u64| U256::from(value).to_be_bytes::<32>();
    [
        integer(1),
        [0x11; 32],
        [0x12; 32],
        [0x13; 32],
        [0x14; 32],
        [0x15; 32],
        [0x16; 32],
        integer(0xffffffffffffffff),
        integer(0x102030405060708),
        integer(0x1112131415161718),
        integer(0x2122232425262728),
        integer(0x3132333435363738),
        integer(0x4142434445464748),
        integer(0x5152535455565758),
        [0x17; 32],
        [0x18; 32],
        [0x19; 32],
        [0x1a; 32],
        [0x1b; 32],
        integer(0x6162),
        integer(0x7172),
        integer(0x81),
        [0x1c; 32],
        [0x1d; 32],
    ]
    .concat()
}

#[test]
fn legacy_binding_abi_preserves_all_fields_and_exact_renewal_json() {
    let view = NodeEnclaveBindingV1View::abi_decode(&legacy_binding_bytes()).unwrap();
    let record = RenewalBindingV1::try_from(view).unwrap();
    assert_eq!(serde_json::to_string(&record).unwrap(), LEGACY_RENEWAL_JSON);
}

#[test]
fn present_renewal_binding_encodes_the_exact_v1_abi_words() {
    let record: RenewalBindingV1 = serde_json::from_str(LEGACY_RENEWAL_JSON).unwrap();
    let view: NodeEnclaveBindingV1View = (&record).into();
    assert_eq!(view.abi_encode(), legacy_binding_bytes());
}

#[test]
fn absent_registry_binding_rejects_even_when_other_fields_are_populated() {
    let mut view = NodeEnclaveBindingV1View::abi_decode(&legacy_binding_bytes()).unwrap();
    view.exists = false;
    assert_eq!(
        RenewalBindingV1::try_from(view).unwrap_err().to_string(),
        "finalized Registry has no enclave binding for this node",
    );
}

#[test]
fn renewal_json_still_requires_exact_v1_fields_and_integer_widths() {
    let original: serde_json::Value = serde_json::from_str(LEGACY_RENEWAL_JSON).unwrap();
    let mut unknown = original.clone();
    unknown["unexpected"] = serde_json::json!(1);
    assert!(serde_json::from_value::<RenewalBindingV1>(unknown).is_err());
    let mut missing = original.clone();
    missing.as_object_mut().unwrap().remove("enclaveId");
    assert!(serde_json::from_value::<RenewalBindingV1>(missing).is_err());
    let mut wide_product = original;
    wide_product["isvProdId"] = serde_json::json!(u32::from(u16::MAX) + 1);
    assert!(serde_json::from_value::<RenewalBindingV1>(wide_product).is_err());
}
